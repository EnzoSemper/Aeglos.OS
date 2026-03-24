//! Cryptographically Secure Pseudo-Random Number Generator.
//!
//! Entropy sources (in priority order):
//!   1. ARMv8.5-A `RNDR` instruction — hardware RNG, available on Apple Silicon
//!      and QEMU `-cpu max`.  Detected at runtime via `ID_AA64ISAR0_EL1[63:60]`.
//!   2. Timer-jitter accumulator — 1024 samples of `CNTPCT_EL0`, XOR-folded
//!      with a Fibonacci LFSR tap.  Adequate entropy on systems without RNDR.
//!
//! All sources are mixed into a 256-bit seed.  The seed initialises a
//! ChaCha20 stream cipher (IETF variant, 20 rounds) used as a DRBG.
//! After 2^64 output blocks (2^70 bytes) the 64-bit counter overflows;
//! in practice `reseed()` should be called periodically from the timer IRQ.
//!
//! SMP safety: a TAS spinlock guards the global DRBG state.  CSPRNG calls
//! are infrequent (TLS handshakes, key generation) so lock contention is
//! negligible.

use core::sync::atomic::{AtomicBool, Ordering};

// ── ChaCha20 constants ("expand 32-byte k") ──────────────────────────────────

const SIGMA: [u32; 4] = [
    0x6170_7865, 0x3320_646e, 0x7962_2d32, 0x6b20_6574,
];

// ── Global DRBG ───────────────────────────────────────────────────────────────

struct Drbg {
    state:     [u32; 16],
    keystream: [u8; 64],
    pos:       usize,      // next unconsumed byte in keystream
}

static mut DRBG: Drbg = Drbg {
    state:     [0u32; 16],
    keystream: [0u8; 64],
    pos:       64, // forces block generation on first use
};

static DRBG_READY: AtomicBool = AtomicBool::new(false);
static DRBG_LOCK:  AtomicBool = AtomicBool::new(false);

#[inline(always)]
fn lock() {
    while DRBG_LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
}

#[inline(always)]
fn unlock() {
    DRBG_LOCK.store(false, Ordering::Release);
}

// ── ChaCha20 block function ───────────────────────────────────────────────────

/// Quarter-round on four indices of a working array.
/// Uses explicit indexing to avoid multiple-mutable-borrow issues.
macro_rules! qr {
    ($x:expr, $a:expr, $b:expr, $c:expr, $d:expr) => {{
        $x[$a] = $x[$a].wrapping_add($x[$b]); $x[$d] ^= $x[$a]; $x[$d] = $x[$d].rotate_left(16);
        $x[$c] = $x[$c].wrapping_add($x[$d]); $x[$b] ^= $x[$c]; $x[$b] = $x[$b].rotate_left(12);
        $x[$a] = $x[$a].wrapping_add($x[$b]); $x[$d] ^= $x[$a]; $x[$d] = $x[$d].rotate_left(8);
        $x[$c] = $x[$c].wrapping_add($x[$d]); $x[$b] ^= $x[$c]; $x[$b] = $x[$b].rotate_left(7);
    }};
}

fn chacha20_block(state: &[u32; 16]) -> [u8; 64] {
    let mut x = *state;
    for _ in 0..10 {
        // Column rounds
        qr!(x,  0,  4,  8, 12);
        qr!(x,  1,  5,  9, 13);
        qr!(x,  2,  6, 10, 14);
        qr!(x,  3,  7, 11, 15);
        // Diagonal rounds
        qr!(x,  0,  5, 10, 15);
        qr!(x,  1,  6, 11, 12);
        qr!(x,  2,  7,  8, 13);
        qr!(x,  3,  4,  9, 14);
    }
    for i in 0..16 { x[i] = x[i].wrapping_add(state[i]); }
    let mut out = [0u8; 64];
    for i in 0..16 { out[i*4..i*4+4].copy_from_slice(&x[i].to_le_bytes()); }
    out
}

// ── Entropy collection ────────────────────────────────────────────────────────

/// Read the physical counter — available at EL1 without the virtual-counter
/// restriction that applies to EL0.
#[inline(always)]
fn read_counter() -> u64 {
    let t: u64;
    unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) t, options(nomem, nostack)); }
    t
}

/// Return true if the ARMv8.5-A RNDR instruction is available.
fn has_rndr() -> bool {
    let isar0: u64;
    unsafe {
        core::arch::asm!(
            "mrs {}, ID_AA64ISAR0_EL1",
            out(reg) isar0,
            options(nomem, nostack),
        );
    }
    (isar0 >> 60) & 0xF != 0
}

/// Read one 64-bit hardware random value via RNDR.
/// Returns `(value, ok)` — `ok` is false if the hardware RNG is not ready.
#[inline(always)]
fn read_rndr() -> (u64, bool) {
    let val: u64;
    let ok: u64;
    unsafe {
        // S3_3_C2_C4_0 = RNDR.  Sets PSTATE.C = 1 on success, 0 on failure.
        core::arch::asm!(
            "mrs {val}, S3_3_C2_C4_0",
            "cset {ok}, cs",
            val = out(reg) val,
            ok  = out(reg) ok,
            options(nomem, nostack),
        );
    }
    (val, ok != 0)
}

/// Accumulate entropy from timer jitter.
/// Samples `CNTPCT_EL0` 1024 times in a tight loop; the sub-nanosecond
/// variation in the low bits of successive reads provides real entropy.
/// The samples are folded through a Fibonacci LFSR tap so that each bit
/// of the accumulator depends on many samples.
fn timer_jitter_entropy() -> u64 {
    let mut acc: u64 = read_counter().wrapping_mul(0x9e37_79b9_7f4a_7c15);
    for _ in 0..1024 {
        let t = read_counter();
        // Mix: LFSR feedback (taps 64, 63, 61, 60)
        let feedback = ((acc >> 63) ^ (acc >> 62) ^ (acc >> 60) ^ (acc >> 59)) & 1;
        acc = (acc << 1) | feedback;
        acc ^= t;
    }
    acc
}

/// Collect a 256-bit seed from available entropy sources.
fn collect_seed() -> [u8; 32] {
    let mut words = [0u64; 4];

    if has_rndr() {
        // Hardware RNG path: 4 × 64-bit RNDR words.
        for w in words.iter_mut() {
            let mut v = 0u64;
            for _ in 0..8 {
                let (r, ok) = read_rndr();
                if ok { v ^= r; } else { v ^= read_counter(); }
            }
            *w = v;
        }
    } else {
        // Timer-jitter path: 4 independent accumulations.
        for w in words.iter_mut() {
            *w = timer_jitter_entropy();
        }
    }

    // Always mix in current counter + stack/PC addresses for extra uniqueness.
    let t = read_counter();
    let sp: u64;
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)); }
    words[0] ^= t.wrapping_mul(0x517c_c1b7_2722_0a95);
    words[1] ^= sp.wrapping_mul(0x6c62_272e_07bb_0142);
    words[2] ^= t.rotate_right(17) ^ sp.rotate_left(13);
    words[3] ^= t.wrapping_add(sp).wrapping_mul(0xbf58_476d_1ce4_e5b9);

    // Final Feistel-style mix pass
    words[0] ^= words[3].rotate_left(31);
    words[1] ^= words[0].rotate_left(17);
    words[2] ^= words[1].rotate_left(13);
    words[3] ^= words[2].rotate_left(29);

    let mut out = [0u8; 32];
    for i in 0..4 { out[i*8..i*8+8].copy_from_slice(&words[i].to_le_bytes()); }
    out
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Return true if the ARMv8.5-A `RNDR` instruction is available on this CPU.
/// Used for boot logging.
pub fn has_rndr_available() -> bool { has_rndr() }

/// Initialise the DRBG.  Must be called after the system timer is running.
/// Safe to call multiple times — subsequent calls reseed the DRBG.
pub fn init() {
    let seed = collect_seed();
    let t    = read_counter();
    let sp: u64;
    unsafe { core::arch::asm!("mov {}, sp", out(reg) sp, options(nomem, nostack)); }

    lock();
    unsafe {
        // "expand 32-byte k" constants
        DRBG.state[0]  = SIGMA[0];
        DRBG.state[1]  = SIGMA[1];
        DRBG.state[2]  = SIGMA[2];
        DRBG.state[3]  = SIGMA[3];
        // 256-bit key from seed
        for i in 0..8 {
            DRBG.state[4 + i] = u32::from_le_bytes(seed[i*4..i*4+4].try_into().unwrap_or([0;4]));
        }
        // 64-bit counter starts at 0
        DRBG.state[12] = 0;
        DRBG.state[13] = 0;
        // 64-bit nonce from timer + SP
        DRBG.state[14] = t as u32 ^ (sp >> 16) as u32;
        DRBG.state[15] = (t >> 32) as u32 ^ sp as u32;
        // Force immediate block generation
        DRBG.pos = 64;
    }
    unlock();

    DRBG_READY.store(true, Ordering::Release);
}

/// Mix additional entropy into the DRBG key (forward-secrecy reseed).
/// Safe to call from the timer IRQ.
pub fn reseed() {
    let extra = collect_seed();
    lock();
    unsafe {
        for i in 0..8 {
            let x = u32::from_le_bytes(extra[i*4..i*4+4].try_into().unwrap_or([0;4]));
            DRBG.state[4 + i] ^= x;
        }
        // Advance nonce
        DRBG.state[14] = DRBG.state[14].wrapping_add(1);
        DRBG.pos = 64; // discard current block
    }
    unlock();
}

/// Fill `buf` with cryptographically random bytes.
/// Falls back to timer-based output if called before `init()`.
pub fn fill_bytes(buf: &mut [u8]) {
    if !DRBG_READY.load(Ordering::Acquire) {
        // Pre-init fallback: use timer + address mixing.
        let mut t = read_counter().wrapping_mul(0x9e37_79b9_7f4a_7c15);
        for (i, b) in buf.iter_mut().enumerate() {
            t ^= (i as u64).wrapping_mul(0x6c62_272e_07bb_0142);
            t  = t.rotate_left(13).wrapping_add(0x517c_c1b7_2722_0a95);
            *b = (t >> 56) as u8;
        }
        return;
    }

    lock();
    unsafe {
        for b in buf.iter_mut() {
            if DRBG.pos >= 64 {
                let state_copy = DRBG.state;
                DRBG.keystream = chacha20_block(&state_copy);
                // Increment 64-bit little-endian counter
                DRBG.state[12] = DRBG.state[12].wrapping_add(1);
                if DRBG.state[12] == 0 {
                    DRBG.state[13] = DRBG.state[13].wrapping_add(1);
                }
                DRBG.pos = 0;
            }
            *b = DRBG.keystream[DRBG.pos];
            DRBG.pos += 1;
        }
    }
    unlock();
}

/// Return 32 random bytes (convenience wrapper).
pub fn random_bytes_32() -> [u8; 32] {
    let mut buf = [0u8; 32];
    fill_bytes(&mut buf);
    buf
}

// ── `rand_core` integration ───────────────────────────────────────────────────

/// Zero-size handle to the global ChaCha20-DRBG.
/// Implements `RngCore + CryptoRng` for use with `x25519_dalek`,
/// `aes-gcm`, and any other crate that accepts a generic RNG.
pub struct Csprng;

impl rand_core::RngCore for Csprng {
    fn next_u32(&mut self) -> u32 {
        rand_core::impls::next_u32_via_fill(self)
    }
    fn next_u64(&mut self) -> u64 {
        rand_core::impls::next_u64_via_fill(self)
    }
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        fill_bytes(dest);
    }
    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core::Error> {
        fill_bytes(dest);
        Ok(())
    }
}

/// `Csprng` is a CSPRNG — mark it as cryptographically suitable.
impl rand_core::CryptoRng for Csprng {}
