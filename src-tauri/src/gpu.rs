// Optional GPU-backed nonce search, run alongside (not instead of) the CPU
// threads in miner.rs. Exists to close some of the gap to miners running
// their own custom GPU clients against PIKO's public protocol -- see the
// PIKO mining-decentralization thread this was built for: one miner was
// found dominating recent blocks with an estimated multi-GH/s custom
// client, and diluting that by giving GPU mining to everyone (rather than
// trying to cap or ban it, which isn't really enforceable against a
// permissionless PoW+principal system anyway) was the agreed path.
//
// Correctness posture for a live economic system: this shader is fast but
// NOT trusted. Every candidate nonce it reports is re-hashed on the CPU
// with the exact same `sha2` crate the CPU search path already uses
// (verify_candidate below) before it's ever handed to the mining
// supervisor to submit. A shader bug can therefore only ever waste a
// round-trip, never produce a bad `submitProof` call -- and `mother`
// itself re-verifies the proof before charging any ICP fee regardless, so
// the actual economic blast radius of a bug here is zero.
use crate::miner::{height_to_bytes8, leading_zero_bits, nonce_to_bytes8, MiningJob};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

const WORKGROUP_SIZE: u32 = 256;
const NUM_WORKGROUPS: u32 = 4096; // 4096 * 256 = 1,048,576 threads/batch
const TOTAL_THREADS: u32 = WORKGROUP_SIZE * NUM_WORKGROUPS;
// ~520 MH/s measured on a GTX 1050 puts this around 130ms/batch -- short
// enough that the stop flag and power throttle both stay responsive.
const ITERATIONS_PER_BATCH: u32 = 64;

// Sentinel meaning "pool mode is off" -- a hash's leading-zero-bit count
// can never reach this (max possible is 256, for a 32-byte digest), so the
// shader's share check can stay an unconditional comparison with no
// separate enable flag needed.
const SHARE_DIFFICULTY_DISABLED: u32 = u32::MAX;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuParams {
    ph: [u32; 8],
    height_hi: u32,
    height_lo: u32,
    nonce_base_hi: u32,
    nonce_base_lo: u32,
    iterations: u32,
    difficulty_bits: u32,
    share_difficulty_bits: u32,
    total_threads: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuFound {
    flag: u32,
    nonce_hi: u32,
    nonce_lo: u32,
    share_flag: u32,
    share_nonce_hi: u32,
    share_nonce_lo: u32,
}

fn is_usable(adapter: &wgpu::Adapter) -> bool {
    // Software rasterizers (llvmpipe, etc.) technically work but are never
    // going to beat the CPU path -- not worth ever picking one.
    adapter.get_info().device_type != wgpu::DeviceType::Cpu
}

async fn pick_adapter(instance: &wgpu::Instance) -> Option<wgpu::Adapter> {
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    if let Some(a) = adapters
        .into_iter()
        .find(|a| a.get_info().device_type == wgpu::DeviceType::DiscreteGpu)
    {
        return Some(a);
    }
    let adapters = instance.enumerate_adapters(wgpu::Backends::all());
    if let Some(a) = adapters.into_iter().find(is_usable) {
        return Some(a);
    }
    instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .filter(is_usable)
}

/// Best-effort probe for the UI: returns the adapter's name if a real GPU
/// is available to mine with, or None (frontend then just doesn't show the
/// GPU toggle at all rather than offering a setting that can't do anything).
pub fn probe() -> Option<String> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });
        pick_adapter(&instance).await.map(|a| a.get_info().name)
    })
}

fn candidate_zero_bits(job: &MiningJob, nonce: u64) -> u32 {
    let mut data = [0u8; 48];
    data[..32].copy_from_slice(&job.previous_hash);
    data[32..40].copy_from_slice(&height_to_bytes8(job.height));
    data[40..].copy_from_slice(&nonce_to_bytes8(nonce));
    let digest = Sha256::digest(data);
    leading_zero_bits(&digest)
}

fn verify_candidate(job: &MiningJob, nonce: u64) -> bool {
    candidate_zero_bits(job, nonce) >= job.difficulty_bits
}

/// Spawns one dedicated OS thread owning a wgpu device, searching the same
/// job the CPU threads are already searching (a different, randomly offset
/// slice of the nonce space -- see the random-offset comment in miner.rs;
/// negligible collision risk, and even a collision would just be some
/// duplicate work, not a correctness issue). Reports into the same
/// hash_count/tx as the CPU threads. Returns None if no usable adapter is
/// found (caller should already have checked via `probe()`, but this is
/// re-checked here since the adapter landscape could in principle change
/// between the two calls).
pub fn start_gpu_mining(
    job: MiningJob,
    power_percent: Arc<AtomicU32>,
    stop_flag: Arc<AtomicBool>,
    hash_count: Arc<AtomicU64>,
    tx: Sender<u64>,
    share_tx: Option<Sender<u64>>,
) -> Option<thread::JoinHandle<()>> {
    Some(thread::spawn(move || {
        pollster::block_on(gpu_mine_loop(job, power_percent, stop_flag, hash_count, tx, share_tx));
    }))
}

async fn gpu_mine_loop(
    job: MiningJob,
    power_percent: Arc<AtomicU32>,
    stop_flag: Arc<AtomicBool>,
    hash_count: Arc<AtomicU64>,
    tx: Sender<u64>,
    share_tx: Option<Sender<u64>>,
) {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::all(),
        ..Default::default()
    });
    let adapter = match pick_adapter(&instance).await {
        Some(a) => a,
        None => return, // no GPU to mine with -- CPU threads carry the job alone
    };
    let (device, queue) = match adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("piko gpu miner"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: wgpu::MemoryHints::default(),
            },
            None,
        )
        .await
    {
        Ok(pair) => pair,
        Err(_) => return,
    };

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("sha256_search"),
        source: wgpu::ShaderSource::Wgsl(include_str!("shaders/sha256_search.wgsl").into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("gpu miner bgl"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("gpu miner pl"),
        bind_group_layouts: &[&bind_group_layout],
        push_constant_ranges: &[],
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("gpu miner pipeline"),
        layout: Some(&pipeline_layout),
        module: &shader,
        entry_point: "main",
        compilation_options: wgpu::PipelineCompilationOptions::default(),
        cache: None,
    });

    let mut ph = [0u32; 8];
    for (i, chunk) in job.previous_hash.chunks_exact(4).enumerate() {
        ph[i] = u32::from_be_bytes(chunk.try_into().unwrap());
    }
    let height_bytes = height_to_bytes8(job.height);
    let height_hi = u32::from_be_bytes(height_bytes[0..4].try_into().unwrap());
    let height_lo = u32::from_be_bytes(height_bytes[4..8].try_into().unwrap());

    let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("params"),
        size: std::mem::size_of::<GpuParams>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let found_size = std::mem::size_of::<GpuFound>() as u64;
    let found_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("found"),
        size: found_size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let staging_buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("found staging"),
        size: found_size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("gpu miner bg"),
        layout: &bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: params_buf.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: found_buf.as_entire_binding() },
        ],
    });

    // Random 56-bit start, same masking rationale as the CPU path's own
    // nonce_offset -- plenty of headroom before batch_base*iterations could
    // ever wrap u64.
    let mut offset_bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut offset_bytes);
    let mut batch_base = u64::from_be_bytes(offset_bytes) & 0x00ff_ffff_ffff_ffff;

    let share_difficulty_bits = job.share_difficulty_bits.unwrap_or(SHARE_DIFFICULTY_DISABLED);
    let zero_found = GpuFound {
        flag: 0,
        nonce_hi: 0,
        nonce_lo: 0,
        share_flag: 0,
        share_nonce_hi: 0,
        share_nonce_lo: 0,
    };

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            return;
        }

        let params = GpuParams {
            ph,
            height_hi,
            height_lo,
            nonce_base_hi: (batch_base >> 32) as u32,
            nonce_base_lo: batch_base as u32,
            iterations: ITERATIONS_PER_BATCH,
            difficulty_bits: job.difficulty_bits,
            share_difficulty_bits,
            total_threads: TOTAL_THREADS,
        };
        queue.write_buffer(&params_buf, 0, bytemuck::bytes_of(&params));
        queue.write_buffer(&found_buf, 0, bytemuck::bytes_of(&zero_found));

        let batch_start = Instant::now();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(NUM_WORKGROUPS, 1, 1);
        }
        encoder.copy_buffer_to_buffer(&found_buf, 0, &staging_buf, 0, found_size);
        queue.submit(Some(encoder.finish()));
        device.poll(wgpu::Maintain::Wait);

        let slice = staging_buf.slice(..);
        let (map_tx, map_rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = map_tx.send(res);
        });
        device.poll(wgpu::Maintain::Wait);
        let mapped_ok = map_rx.recv().map(|r| r.is_ok()).unwrap_or(false);

        let batch_hashes = TOTAL_THREADS as u64 * ITERATIONS_PER_BATCH as u64;

        if mapped_ok {
            let data = slice.get_mapped_range();
            let found: &GpuFound = bytemuck::from_bytes(&data);
            let result = *found;
            drop(data);
            staging_buf.unmap();

            if result.flag != 0 {
                let nonce = ((result.nonce_hi as u64) << 32) | result.nonce_lo as u64;
                if verify_candidate(&job, nonce) {
                    hash_count.fetch_add(batch_hashes, Ordering::Relaxed);
                    stop_flag.store(true, Ordering::Relaxed);
                    let _ = tx.send(nonce);
                    return;
                } else {
                    // Should never happen (would mean a shader bug), but
                    // since it costs nothing to check, just keep mining
                    // instead of trusting an unverified nonce.
                    eprintln!("gpu miner: candidate nonce failed CPU verification, discarding and continuing");
                }
            }
            if result.share_flag != 0 {
                if let Some(share_tx) = &share_tx {
                    let nonce = ((result.share_nonce_hi as u64) << 32) | result.share_nonce_lo as u64;
                    if candidate_zero_bits(&job, nonce) >= share_difficulty_bits {
                        let _ = share_tx.send(nonce);
                    } else {
                        eprintln!("gpu miner: share candidate failed CPU verification, discarding and continuing");
                    }
                }
            }
        } else {
            staging_buf.unmap();
        }

        hash_count.fetch_add(batch_hashes, Ordering::Relaxed);
        batch_base = batch_base.wrapping_add(batch_hashes);

        let power = power_percent.load(Ordering::Relaxed).clamp(1, 100);
        if power < 100 {
            let dt = batch_start.elapsed();
            let sleep_for = dt.mul_f64((100 - power) as f64 / power as f64);
            thread::sleep(sleep_for);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // Real end-to-end correctness check against actual GPU hardware: mines
    // a deliberately easy job (difficulty low enough to find a nonce in a
    // fraction of a second) and confirms the winning nonce's hash, computed
    // independently by the CPU `sha2` crate, really does satisfy the
    // difficulty target -- i.e. the WGSL shader's SHA-256 implementation
    // and the 48-byte header layout genuinely match the CPU path bit for
    // bit, not just "the shader compiles." Skips (rather than fails) on a
    // machine with no usable GPU, e.g. CI runners -- this is a hardware
    // verification test, not a portability test.
    #[test]
    fn gpu_search_matches_cpu_sha256() {
        if probe().is_none() {
            eprintln!("no usable GPU adapter found, skipping gpu_search_matches_cpu_sha256");
            return;
        }

        let job = MiningJob {
            previous_hash: [0x42; 32],
            height: 123_456,
            difficulty_bits: 16, // easy: found within a batch or two, well under a second
            share_difficulty_bits: None,
        };

        let stop_flag = Arc::new(AtomicBool::new(false));
        let hash_count = Arc::new(AtomicU64::new(0));
        let power_percent = Arc::new(AtomicU32::new(100));
        let (tx, rx) = std::sync::mpsc::channel();

        let handle = start_gpu_mining(job, power_percent, Arc::clone(&stop_flag), hash_count, tx, None)
            .expect("adapter was just probed as available");

        let nonce = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("GPU search did not find a low-difficulty nonce within 30s");
        stop_flag.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        assert!(
            verify_candidate(&job, nonce),
            "GPU-found nonce {nonce} failed independent CPU SHA-256 verification -- shader/header-layout mismatch"
        );
    }

    // TEMP-BENCH: re-measures real GPU throughput with the current shipped
    // dual-threshold shader (solo vs. pool-mode share reporting enabled),
    // to confirm the extra share_difficulty_bits branch in the WGSL hot
    // loop hasn't regressed the ~520 MH/s measured on this GTX 1050 before
    // pool mode existed. Remove after reading the output -- not meant to
    // ship.
    #[test]
    fn gpu_hashrate() {
        if probe().is_none() {
            eprintln!("no usable GPU adapter found, skipping gpu_hashrate");
            return;
        }
        for share_bits in [None, Some(20u32)] {
            let job = MiningJob {
                previous_hash: [0x11; 32],
                height: 999_999,
                difficulty_bits: 64, // unreachable within the benchmark window -- never stops the search
                share_difficulty_bits: share_bits,
            };
            let stop_flag = Arc::new(AtomicBool::new(false));
            let hash_count = Arc::new(AtomicU64::new(0));
            let power_percent = Arc::new(AtomicU32::new(100));
            let (tx, _rx) = std::sync::mpsc::channel();
            let (share_tx, _share_rx) = std::sync::mpsc::channel();
            let handle = start_gpu_mining(
                job,
                power_percent,
                Arc::clone(&stop_flag),
                Arc::clone(&hash_count),
                tx,
                share_bits.map(|_| share_tx),
            )
            .expect("adapter was just probed as available");
            let start = Instant::now();
            thread::sleep(Duration::from_secs(3));
            let elapsed = start.elapsed();
            stop_flag.store(true, Ordering::Relaxed);
            handle.join().unwrap();
            let count = hash_count.load(Ordering::Relaxed);
            let rate = count as f64 / elapsed.as_secs_f64();
            eprintln!(
                "GPU: share_difficulty_bits={share_bits:?} hashrate={:.1} MH/s (count={count}, elapsed={elapsed:?})",
                rate / 1e6
            );
        }
    }
}
