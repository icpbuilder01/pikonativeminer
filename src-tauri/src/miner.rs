// The actual hash search -- native equivalent of miner.worker.ts in the
// browser mining site, same byte layout, same nonce-striding/random-offset
// design (see that file's own comments in piko-icp for the full reasoning
// on why the random offset matters), but real multi-threaded native SHA-256
// via the `sha2` crate instead of one Web Worker per core in a browser.
// `sha2` auto-detects and uses hardware acceleration (SHA-NI on x86,
// the ARMv8 crypto extensions on ARM) at runtime with no special code
// needed here -- that hardware-acceleration gap is the whole reason this
// app exists alongside the browser miner, not a replacement for it.
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub struct MiningJob {
    pub previous_hash: [u8; 32],
    pub height: u64,
    pub difficulty_bits: u32,
    // Some(bits) when pool mode is on -- a lower, easier threshold that
    // gets reported (via a separate channel, see start_mining) without
    // stopping the search, so the pool can credit partial work between the
    // rare full-difficulty finds. None when mining solo.
    pub share_difficulty_bits: Option<u32>,
}

// Owns every search thread for a job -- CPU threads always, plus the GPU
// thread too when GPU mining is enabled (see gpu.rs). Both kinds share the
// same stop_flag/hash_count/tx (constructed once by the caller in lib.rs)
// so the supervisor only ever has one hashrate counter and one winner
// channel to watch, regardless of how many search backends are active.
pub struct MiningHandle {
    stop_flag: Arc<AtomicBool>,
    pub hash_count: Arc<AtomicU64>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl MiningHandle {
    pub fn new(stop_flag: Arc<AtomicBool>, hash_count: Arc<AtomicU64>) -> Self {
        MiningHandle {
            stop_flag,
            hash_count,
            threads: Vec::new(),
        }
    }

    pub fn add_threads(&mut self, threads: Vec<thread::JoinHandle<()>>) {
        self.threads.extend(threads);
    }

    pub fn stop(self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        for t in self.threads {
            let _ = t.join();
        }
    }
}

pub fn nonce_to_bytes8(n: u64) -> [u8; 8] {
    n.to_be_bytes()
}

pub fn height_to_bytes8(n: u64) -> [u8; 8] {
    n.to_be_bytes()
}

pub fn leading_zero_bits(hash: &[u8]) -> u32 {
    let mut count = 0u32;
    for byte in hash {
        if *byte == 0 {
            count += 8;
            continue;
        }
        count += byte.leading_zeros();
        break;
    }
    count
}

/// Starts `num_threads` search threads, each hashing a disjoint slice of
/// the nonce space (workerIndex + k*workerCount, exactly like the browser
/// worker) starting from a shared random offset for this job. `stop_flag`,
/// `hash_count` and `tx` are owned by the caller (lib.rs's supervisor) so a
/// GPU search (see gpu.rs) can be started against the very same job and
/// report into the same counter/channel -- returns just the thread handles,
/// not a full MiningHandle, so the caller can merge them with the GPU
/// thread's handle into one.
pub fn start_mining(
    job: MiningJob,
    num_threads: usize,
    // Shared with the caller so a live power-level change (Low/High/Max)
    // takes effect immediately on already-running threads, same idea as
    // the browser worker's own dutyCycle -- read fresh every loop
    // iteration, not captured once at spawn time. 0-100, 100 = full speed.
    power_percent: Arc<AtomicU32>,
    stop_flag: Arc<AtomicBool>,
    hash_count: Arc<AtomicU64>,
    tx: std::sync::mpsc::Sender<u64>,
    // Present only in pool mode -- see MiningJob::share_difficulty_bits.
    // A share found here doesn't stop the search; the supervisor forwards
    // it to pikopool instead of mother, on its own async task.
    share_tx: Option<std::sync::mpsc::Sender<u64>>,
) -> Vec<thread::JoinHandle<()>> {
    let mut header = [0u8; 40];
    header[..32].copy_from_slice(&job.previous_hash);
    header[32..].copy_from_slice(&height_to_bytes8(job.height));

    // Shared across all threads for this job so they partition the space
    // cleanly among themselves (like workerIndex/workerCount in the
    // browser), but freshly randomized per job -- see miner.worker.ts's
    // own nonceOffset comment for why this matters for genuine fairness
    // between independent sessions on the network.
    let mut offset_bytes = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut offset_bytes);
    // Masked well below u64::MAX so stride*batches never wraps during a
    // realistic search.
    let nonce_offset = u64::from_be_bytes(offset_bytes) & 0x00ff_ffff_ffff_ffff;

    let difficulty_bits = job.difficulty_bits;
    let share_difficulty_bits = job.share_difficulty_bits;
    let mut threads = Vec::with_capacity(num_threads);

    for worker_index in 0..num_threads {
        let stop_flag = Arc::clone(&stop_flag);
        let hash_count = Arc::clone(&hash_count);
        let power_percent = Arc::clone(&power_percent);
        let tx = tx.clone();
        let share_tx = share_tx.clone();
        let header = header;
        let stride = num_threads as u64;
        let start_nonce = nonce_offset.wrapping_add(worker_index as u64);

        threads.push(thread::spawn(move || {
            let mut nonce = start_nonce;
            let mut local_attempts: u64 = 0;
            const REPORT_BATCH: u64 = 2000; // amortize the shared atomic's contention cost
            // Duty-cycle throttling for Low/High power, same window-based
            // approach as the browser worker: hash flat-out for
            // WINDOW * (power/100), then genuinely sleep (a real idle OS
            // thread, not just spinning slower) for the rest of the
            // window, so Low power actually lets the CPU cool off.
            const WINDOW: Duration = Duration::from_millis(200);
            let mut window_start = Instant::now();

            loop {
                if stop_flag.load(Ordering::Relaxed) {
                    break;
                }

                let mut data = [0u8; 48];
                data[..40].copy_from_slice(&header);
                data[40..].copy_from_slice(&nonce_to_bytes8(nonce));

                let digest = Sha256::digest(data);
                local_attempts += 1;
                let zero_bits = leading_zero_bits(&digest);

                if zero_bits >= difficulty_bits {
                    hash_count.fetch_add(local_attempts, Ordering::Relaxed);
                    stop_flag.store(true, Ordering::Relaxed);
                    let _ = tx.send(nonce);
                    return;
                } else if let Some(share_bits) = share_difficulty_bits {
                    if zero_bits >= share_bits {
                        if let Some(share_tx) = &share_tx {
                            let _ = share_tx.send(nonce);
                        }
                    }
                }

                nonce = nonce.wrapping_add(stride);

                let power = power_percent.load(Ordering::Relaxed).clamp(1, 100);
                if power < 100 {
                    let active_for = WINDOW.mul_f64(power as f64 / 100.0);
                    if window_start.elapsed() >= active_for {
                        hash_count.fetch_add(local_attempts, Ordering::Relaxed);
                        local_attempts = 0;
                        let sleep_for = WINDOW.mul_f64(1.0 - power as f64 / 100.0);
                        thread::sleep(sleep_for);
                        window_start = Instant::now();
                        continue;
                    }
                }

                if local_attempts >= REPORT_BATCH {
                    hash_count.fetch_add(local_attempts, Ordering::Relaxed);
                    local_attempts = 0;
                }
            }
            hash_count.fetch_add(local_attempts, Ordering::Relaxed);
        }));
    }

    threads
}

// TEMP-BENCH: re-measures real CPU throughput with the current shipped
// dual-threshold code (solo vs. pool-mode share checking enabled), to
// confirm the pool-mode branch added to the hot loop hasn't regressed the
// numbers benchmarked before pool mode existed. Remove after reading the
// output -- not meant to ship.
#[cfg(test)]
mod temp_bench {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn cpu_hashrate() {
        let num_threads = num_cpus::get();
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
            let threads = start_mining(
                job,
                num_threads,
                power_percent,
                Arc::clone(&stop_flag),
                Arc::clone(&hash_count),
                tx,
                share_bits.map(|_| share_tx),
            );
            let start = Instant::now();
            thread::sleep(Duration::from_secs(3));
            let elapsed = start.elapsed();
            stop_flag.store(true, Ordering::Relaxed);
            for t in threads {
                let _ = t.join();
            }
            let count = hash_count.load(Ordering::Relaxed);
            let rate = count as f64 / elapsed.as_secs_f64();
            eprintln!(
                "CPU: threads={num_threads} share_difficulty_bits={share_bits:?} hashrate={:.1} MH/s (count={count}, elapsed={elapsed:?})",
                rate / 1e6
            );
        }
    }
}
