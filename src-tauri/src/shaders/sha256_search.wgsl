// GPU nonce search for PIKO's proof-of-work: SHA-256(previousHash(32) ||
// height(8, BE) || nonce(8, BE)), exactly the same 48-byte layout as
// miner.rs's CPU search and the browser site's miner.worker.ts. Every
// candidate this shader finds is re-verified with the CPU `sha2` crate in
// gpu.rs before it's ever sent to `submitProof` -- this shader only needs
// to be fast, not the last line of defense, so a bug here can waste a
// round-trip at worst, never a bad chain submission (see gpu.rs's comment
// on that verification step for why).
//
// Individual scalar fields instead of `array<u32,8>` for prev_hash: WGSL
// uniform-buffer arrays get a mandatory 16-byte-per-element stride
// (std140-style), which would silently blow up the layout vs. the plain
// packed #[repr(C)] struct on the Rust side. Plain scalars avoid that traps
// entirely.
struct Params {
    ph0: u32, ph1: u32, ph2: u32, ph3: u32, ph4: u32, ph5: u32, ph6: u32, ph7: u32,
    height_hi: u32,
    height_lo: u32,
    nonce_base_hi: u32,
    nonce_base_lo: u32,
    iterations: u32,
    difficulty_bits: u32,
    total_threads: u32,
    _pad: u32,
};

struct Found {
    flag: atomic<u32>,
    nonce_hi: u32,
    nonce_lo: u32,
    _pad: u32,
};

@group(0) @binding(0) var<uniform> params: Params;
@group(0) @binding(1) var<storage, read_write> found: Found;

var<private> K: array<u32, 64> = array<u32, 64>(
    0x428a2f98u,0x71374491u,0xb5c0fbcfu,0xe9b5dba5u,0x3956c25bu,0x59f111f1u,0x923f82a4u,0xab1c5ed5u,
    0xd807aa98u,0x12835b01u,0x243185beu,0x550c7dc3u,0x72be5d74u,0x80deb1feu,0x9bdc06a7u,0xc19bf174u,
    0xe49b69c1u,0xefbe4786u,0x0fc19dc6u,0x240ca1ccu,0x2de92c6fu,0x4a7484aau,0x5cb0a9dcu,0x76f988dau,
    0x983e5152u,0xa831c66du,0xb00327c8u,0xbf597fc7u,0xc6e00bf3u,0xd5a79147u,0x06ca6351u,0x14292967u,
    0x27b70a85u,0x2e1b2138u,0x4d2c6dfcu,0x53380d13u,0x650a7354u,0x766a0abbu,0x81c2c92eu,0x92722c85u,
    0xa2bfe8a1u,0xa81a664bu,0xc24b8b70u,0xc76c51a3u,0xd192e819u,0xd6990624u,0xf40e3585u,0x106aa070u,
    0x19a4c116u,0x1e376c08u,0x2748774cu,0x34b0bcb5u,0x391c0cb3u,0x4ed8aa4au,0x5b9cca4fu,0x682e6ff3u,
    0x748f82eeu,0x78a5636fu,0x84c87814u,0x8cc70208u,0x90befffau,0xa4506cebu,0xbef9a3f7u,0xc67178f2u
);

fn rotr(x: u32, n: u32) -> u32 {
    return (x >> n) | (x << (32u - n));
}

// One SHA-256 compression of a single 64-byte block. Circular 16-word
// message schedule (not the full w[64]) -- cuts per-thread register
// pressure 4x, which is what turned out to actually cap throughput on a
// GTX 1050 (measured ~100 MH/s with w[64] vs ~520 MH/s with this).
fn sha256_block(state_in: array<u32, 8>, w_in: array<u32, 16>) -> array<u32, 8> {
    var w = w_in;

    var a = state_in[0]; var b = state_in[1]; var c = state_in[2]; var d = state_in[3];
    var e = state_in[4]; var f = state_in[5]; var g = state_in[6]; var h = state_in[7];

    for (var t: u32 = 0u; t < 64u; t = t + 1u) {
        let idx = t % 16u;
        if (t >= 16u) {
            let w15 = w[(t + 1u) % 16u];
            let w2 = w[(t + 14u) % 16u];
            let w7 = w[(t + 9u) % 16u];
            let s0 = rotr(w15, 7u) ^ rotr(w15, 18u) ^ (w15 >> 3u);
            let s1 = rotr(w2, 17u) ^ rotr(w2, 19u) ^ (w2 >> 10u);
            w[idx] = w[idx] + s0 + w7 + s1;
        }
        let S1 = rotr(e, 6u) ^ rotr(e, 11u) ^ rotr(e, 25u);
        let ch = (e & f) ^ ((~e) & g);
        let temp1 = h + S1 + ch + K[t] + w[idx];
        let S0 = rotr(a, 2u) ^ rotr(a, 13u) ^ rotr(a, 22u);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let temp2 = S0 + maj;
        h = g; g = f; f = e; e = d + temp1;
        d = c; c = b; b = a; a = temp1 + temp2;
    }

    var out: array<u32, 8>;
    out[0] = state_in[0] + a; out[1] = state_in[1] + b; out[2] = state_in[2] + c; out[3] = state_in[3] + d;
    out[4] = state_in[4] + e; out[5] = state_in[5] + f; out[6] = state_in[6] + g; out[7] = state_in[7] + h;
    return out;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let lane = gid.x;
    let iv = array<u32, 8>(
        0x6a09e667u, 0xbb67ae85u, 0x3c6ef372u, 0xa54ff53au,
        0x510e527fu, 0x9b05688cu, 0x1f83d9abu, 0x5be0cd19u
    );

    var i: u32 = 0u;
    // Runtime-bounded on purpose (see gpu-bench notes): a compile-time
    // bounded outer loop here made naga's SPIR-V backend fully unroll it
    // alongside the 64-round compression loop, which blew up shader
    // compile time on some adapters.
    loop {
        if (i >= params.iterations) {
            break;
        }

        // nonce = nonce_base + lane + i*total_threads, as a u64 split into
        // two u32 words (hi/lo), with explicit carry propagation -- WGSL
        // has no u64 integer type.
        var lo = params.nonce_base_lo + lane;
        var hi = params.nonce_base_hi;
        if (lo < params.nonce_base_lo) { hi = hi + 1u; }
        let add = i * params.total_threads;
        let new_lo = lo + add;
        if (new_lo < lo) { hi = hi + 1u; }
        lo = new_lo;

        var w: array<u32, 16>;
        w[0] = params.ph0; w[1] = params.ph1; w[2] = params.ph2; w[3] = params.ph3;
        w[4] = params.ph4; w[5] = params.ph5; w[6] = params.ph6; w[7] = params.ph7;
        w[8] = params.height_hi; w[9] = params.height_lo;
        w[10] = hi; w[11] = lo;
        // Standard SHA-256 padding for a 48-byte message: 0x80 marker byte,
        // zero padding, then the 64-bit big-endian bit length (384).
        w[12] = 0x80000000u;
        w[13] = 0u;
        w[14] = 0u;
        w[15] = 384u;

        var state = sha256_block(iv, w);

        var total: u32 = 0u;
        var done = false;
        for (var wi: u32 = 0u; wi < 8u; wi = wi + 1u) {
            if (!done) {
                let word = state[wi];
                if (word == 0u) {
                    total = total + 32u;
                } else {
                    total = total + countLeadingZeros(word);
                    done = true;
                }
            }
        }

        if (total >= params.difficulty_bits) {
            let prev = atomicExchange(&found.flag, 1u);
            if (prev == 0u) {
                found.nonce_hi = hi;
                found.nonce_lo = lo;
            }
        }

        i = i + 1u;
    }
}
