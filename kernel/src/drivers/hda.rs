//! Intel High Definition Audio (HDA) driver.
//!
//! Supports the QEMU `intel-hda` + `hda-duplex` device pair.
//! Scans via the PCIe enumerator (vendor 0x8086, device 0x2668).
//! Generates PCM audio (square wave with simple envelope) for a
//! chiptune boot melody.
//!
//! # HDA architecture (simplified)
//! - CORB: ring buffer of 32-bit commands sent TO the codec
//! - RIRB: ring buffer of 64-bit responses FROM the codec
//! - Stream Descriptor: DMA engine for one audio stream
//! - BDL: Buffer Descriptor List — array of (phys_addr, len, ioc) entries

use crate::memory::vmm::{KERNEL_VA_OFFSET, phys_to_virt};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Timer tick at which we should stop the DMA stream (0 = not armed).
static STOP_AT_TICK: AtomicUsize = AtomicUsize::new(0);

// ── HDA MMIO register offsets ─────────────────────────────────────────────────
const GCAP:      usize = 0x00; // u16 Global Capabilities
const GCTL:      usize = 0x08; // u32 Global Control
const STATESTS:  usize = 0x0E; // u16 State Change Status (which codecs present)
const CORBLBASE: usize = 0x40; // u32 CORB Lower Base Addr
const CORBUBASE: usize = 0x44; // u32 CORB Upper Base Addr
const CORBWP:    usize = 0x48; // u16 CORB Write Pointer
const CORBRP:    usize = 0x4A; // u16 CORB Read Pointer (bit15=reset)
const CORBCTL:   usize = 0x4C; // u8  CORB Control (bit1=run)
const CORBSIZE:  usize = 0x4E; // u8  CORB Size (0=2,1=16,2=256 entries)
const RIRBLBASE: usize = 0x50; // u32 RIRB Lower Base Addr
const RIRBUBASE: usize = 0x54; // u32 RIRB Upper Base Addr
const RIRBWP:    usize = 0x58; // u16 RIRB Write Pointer (bit15=reset)
const RINTCNT:   usize = 0x5A; // u16 Response Interrupt Count
const RIRBCTL:   usize = 0x5C; // u8  RIRB Control (bit1=run)
const RIRBSIZE:  usize = 0x5E; // u8  RIRB Size

// Stream Descriptor field offsets (relative to stream descriptor base)
const SD_CTL:  usize = 0x00; // u32 Stream Control + Status
const SD_CBL:  usize = 0x08; // u32 Cyclic Buffer Length (bytes)
const SD_LVI:  usize = 0x0C; // u16 Last Valid Index (BDL entries - 1)
const SD_FMT:  usize = 0x12; // u16 Stream Format
const SD_BDPL: usize = 0x18; // u32 BDL Lower Base Addr
const SD_BDPU: usize = 0x1C; // u32 BDL Upper Base Addr

// Stream Control bits
const SD_CTL_SRST:       u32 = 1 << 0; // stream reset
const SD_CTL_RUN:        u32 = 1 << 1; // stream run
const SD_CTL_STRM_SHIFT: u32 = 20;     // stream number field

// Global Control
const GCTL_CRST: u32 = 1 << 0; // codec reset (1=normal, 0=reset)

// HDA stream format for 48000 Hz, 16-bit, stereo:
//   [15:14] = 00 (48 kHz base)
//   [13:11] = 000 (×1 multiplier)
//   [10:8]  = 000 (÷1 divisor)
//   [6:4]   = 001 (16-bit samples)
//   [3:0]   = 0001 (2 channels = stereo, value = channels - 1)
// 48 kHz is the standard CoreAudio/HDA default and avoids QEMU resampling.
const STREAM_FMT_48K_16_STEREO: u16 = (0 << 14) | (1 << 4) | 1;

// BDL entry: 16 bytes exactly (addr u64 + length u32 + ioc u32)
#[repr(C)]
struct BdlEntry {
    addr:   u64,
    length: u32,
    ioc:    u32,
}

const _: () = assert!(core::mem::size_of::<BdlEntry>() == 16);

// ── Module state ───────────────────────────────────────────────────────────────
static mut HDA_MMIO:  usize = 0; // kernel VA of HDA MMIO BAR
static mut SD_OFF:    usize = 0; // stream descriptor offset from MMIO base
static mut CORB_PA:   usize = 0;
static mut RIRB_PA:   usize = 0;
static mut BDL_PA:    usize = 0;
static mut CORB_WP:   u16   = 0;
static mut RIRB_LAST: u16   = 0;
static mut READY:     bool  = false;

// ── MMIO helpers ──────────────────────────────────────────────────────────────

#[inline] unsafe fn rd8(off: usize)  -> u8  { core::ptr::read_volatile((HDA_MMIO + off) as *const u8)  }
#[inline] unsafe fn rd16(off: usize) -> u16 { core::ptr::read_volatile((HDA_MMIO + off) as *const u16) }
#[inline] unsafe fn rd32(off: usize) -> u32 { core::ptr::read_volatile((HDA_MMIO + off) as *const u32) }
#[inline] unsafe fn wr8(off: usize, v: u8)  { core::ptr::write_volatile((HDA_MMIO + off) as *mut u8,  v) }
#[inline] unsafe fn wr16(off: usize, v: u16){ core::ptr::write_volatile((HDA_MMIO + off) as *mut u16, v) }
#[inline] unsafe fn wr32(off: usize, v: u32){ core::ptr::write_volatile((HDA_MMIO + off) as *mut u32, v) }

#[inline] unsafe fn sd_rd32(off: usize) -> u32  { rd32(SD_OFF + off) }
#[inline] unsafe fn sd_wr16(off: usize, v: u16) { wr16(SD_OFF + off, v) }
#[inline] unsafe fn sd_wr32(off: usize, v: u32) { wr32(SD_OFF + off, v) }

fn spin_us(micros: u64) {
    let freq: u64;
    unsafe { core::arch::asm!("mrs {}, cntfrq_el0", out(reg) freq, options(nomem, nostack)) };
    let ticks = freq * micros / 1_000_000;
    let start: u64;
    unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) start, options(nomem, nostack)) };
    loop {
        let now: u64;
        unsafe { core::arch::asm!("mrs {}, cntpct_el0", out(reg) now, options(nomem, nostack)) };
        if now.wrapping_sub(start) >= ticks { break; }
    }
}

// ── CORB / RIRB ───────────────────────────────────────────────────────────────

/// Send one verb to the codec via CORB and return the RIRB response.
unsafe fn corb_send(codec: u8, node: u8, verb: u32, payload: u8) -> u64 {
    // 12-bit verb + 8-bit payload form
    let cmd: u32 = ((codec as u32) << 28)
                 | ((node  as u32) << 20)
                 | ((verb & 0xFFF) << 8)
                 | (payload as u32);
    let corb_va = phys_to_virt(CORB_PA) as *mut u32;
    let wp = CORB_WP.wrapping_add(1) & 0xFF;
    core::ptr::write_volatile(corb_va.add(wp as usize), cmd);
    CORB_WP = wp;
    wr16(CORBWP, wp);

    // Wait for RIRB write pointer to advance
    for _ in 0..10_000_u32 {
        spin_us(10);
        let rp = rd16(RIRBWP) & 0xFF;
        if rp != RIRB_LAST {
            RIRB_LAST = rp;
            let rirb_va = phys_to_virt(RIRB_PA) as *const u64;
            return core::ptr::read_volatile(rirb_va.add(rp as usize));
        }
    }
    0 // timeout
}

/// Send a 4-bit verb with 16-bit payload (e.g. SET_AMPLIFIER_GAIN_MUTE).
unsafe fn corb_send16(codec: u8, node: u8, verb4: u8, payload: u16) -> u64 {
    // 4-bit verb + 16-bit payload form: bits [19:16] = verb4, [15:0] = payload
    let cmd: u32 = ((codec as u32) << 28)
                 | ((node  as u32) << 20)
                 | ((verb4 as u32 & 0xF) << 16)
                 | (payload as u32);
    let corb_va = phys_to_virt(CORB_PA) as *mut u32;
    let wp = CORB_WP.wrapping_add(1) & 0xFF;
    core::ptr::write_volatile(corb_va.add(wp as usize), cmd);
    CORB_WP = wp;
    wr16(CORBWP, wp);

    for _ in 0..10_000_u32 {
        spin_us(10);
        let rp = rd16(RIRBWP) & 0xFF;
        if rp != RIRB_LAST {
            RIRB_LAST = rp;
            let rirb_va = phys_to_virt(RIRB_PA) as *const u64;
            return core::ptr::read_volatile(rirb_va.add(rp as usize));
        }
    }
    0
}

// ── Melody data ───────────────────────────────────────────────────────────────
//
// "Aeglos Fanfare" — simple, memorable boot jingle (~7 seconds).
// Pairs of (frequency_mHz, duration_ms). frequency_mHz=0 means silence.
// Pure C major (C-E-G-A only) — no dissonant intervals.
// One-shot: timer stops the DMA stream after MELODY_TOTAL_MS elapses.

const MELODY: &[(u32, u32)] = &[
    // ── Phrase 1: rising arpeggio fanfare (1.3 s) ─────────
    (392000,  120), (0, 30),  // G4  short upbeat
    (523250,  150), (0, 30),  // C5
    (659250,  150), (0, 30),  // E5
    (783990,  150), (0, 30),  // G5
    (1046500, 480), (0, 80),  // C6  peak, held

    // ── Phrase 2: gentle descent to rest (1.7 s) ──────────
    (880000,  200), (0, 30),  // A5
    (783990,  200), (0, 30),  // G5
    (659250,  300), (0, 50),  // E5  slight pause
    (523250,  580), (0, 100), // C5  breathe

    // ── Phrase 3: short echo / development (1.6 s) ────────
    (659250,  150), (0, 20),  // E5
    (523250,  150), (0, 20),  // C5
    (659250,  150), (0, 20),  // E5
    (783990,  280), (0, 40),  // G5  answer
    (659250,  150), (0, 20),  // E5
    (523250,  150), (0, 20),  // C5
    (392000,  350), (0, 80),  // G4  fall back

    // ── Phrase 4: grand resolution (2.1 s) ────────────────
    (523250,  180), (0, 30),  // C5
    (659250,  180), (0, 30),  // E5
    (783990,  180), (0, 30),  // G5
    (1046500, 280), (0, 50),  // C6  climax
    (783990,  200), (0, 30),  // G5
    (659250,  200), (0, 30),  // E5
    (523250,  700), (0, 0),   // C5  final hold — THE END
];

/// Total melody duration in milliseconds (used for one-shot stop).
const MELODY_TOTAL_MS: usize = {
    let mut total = 0usize;
    let mut i = 0;
    while i < MELODY.len() {
        total += MELODY[i].1 as usize;
        i += 1;
    }
    total
};

const SAMPLE_RATE: u32 = 48000;

// ── PCM generation ────────────────────────────────────────────────────────────

/// PCM buffer in BSS — 3 MB covers ~8 seconds of 44100 Hz stereo 16-bit.
/// The melody is ~4.5 seconds (~1.6 MB), well within this limit.
static mut PCM_BUF: [u8; 3 * 1024 * 1024] = [0u8; 3 * 1024 * 1024];

/// Generate all melody samples into the static PCM_BUF.
/// Returns (PA, byte_count) or (0, 0) on failure.
unsafe fn generate_pcm() -> (usize, usize) {
    // Count total samples needed
    let mut total_bytes: usize = 0;
    for &(_, dur_ms) in MELODY {
        total_bytes += (SAMPLE_RATE as u64 * dur_ms as u64 / 1000) as usize * 4;
    }
    let capacity = PCM_BUF.len();
    let total_bytes = total_bytes.min(capacity);

    let buf = &mut PCM_BUF[..total_bytes];
    let mut byte_pos: usize = 0;

    for &(freq_mhz, dur_ms) in MELODY {
        let samples = (SAMPLE_RATE as u64 * dur_ms as u64 / 1000) as usize;
        if freq_mhz == 0 {
            // Silence
            for _ in 0..samples {
                if byte_pos + 4 > buf.len() { break; }
                buf[byte_pos]     = 0;
                buf[byte_pos + 1] = 0;
                buf[byte_pos + 2] = 0;
                buf[byte_pos + 3] = 0;
                byte_pos += 4;
            }
        } else {
            // Triangle wave at freq_mhz millihertz — smooth 16-bit era tone.
            // Triangle wave rises linearly from -amp to +amp over the first half-
            // period, then falls from +amp to -amp over the second half.
            // Much softer than square wave; no harsh high-frequency content.
            let period = (SAMPLE_RATE as u64 * 1000 / freq_mhz as u64) as usize;
            let period = if period == 0 { 1 } else { period };
            let amp: i32 = 9000; // slightly louder than square (triangle has lower RMS)

            // 8 ms fade-in / fade-out envelope for smooth note transitions
            let fade_samples = (SAMPLE_RATE * 8 / 1000) as usize;

            for i in 0..samples {
                if byte_pos + 4 > buf.len() { break; }

                let gain: i32 = if i < fade_samples {
                    amp * i as i32 / fade_samples as i32
                } else if samples > fade_samples && i >= samples - fade_samples {
                    amp * (samples - i) as i32 / fade_samples as i32
                } else {
                    amp
                };

                // Triangle: phase in [0, period), output in [-1, +1] mapped to [-gain, +gain]
                let phase = i % period;
                let triangle: i32 = if phase < period / 2 {
                    // Rising: -gain → +gain
                    gain - 2 * gain * phase as i32 / period as i32
                } else {
                    // Falling: +gain → -gain
                    -gain + 2 * gain * (phase - period / 2) as i32 / period as i32
                };
                let sample = triangle.clamp(-32767, 32767) as i16;
                let [lo, hi] = sample.to_le_bytes();

                buf[byte_pos]     = lo;
                buf[byte_pos + 1] = hi; // L
                buf[byte_pos + 2] = lo;
                buf[byte_pos + 3] = hi; // R
                byte_pos += 4;
            }
        }
    }

    // PCM_BUF is in BSS — VA = PA + KERNEL_VA_OFFSET
    let va = PCM_BUF.as_ptr() as usize;
    let pa = va - KERNEL_VA_OFFSET;

    // Flush PCM data from CPU cache to DRAM so HDA DMA sees the real samples.
    // AArch64: DC CVAC (clean to Point of Coherency) per cache line.
    let cache_line = 64usize;
    let end = va + byte_pos;
    let mut p = va & !(cache_line - 1);
    while p < end {
        core::arch::asm!("dc cvac, {}", in(reg) p, options(nostack));
        p += cache_line;
    }
    core::arch::asm!("dsb sy", options(nostack));

    (pa, byte_pos)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the HDA controller at `bar0_pa` and begin DMA playback of the
/// boot melody.  Returns immediately — audio plays asynchronously.
pub fn init(bar0_pa: usize) -> bool {
    let uart = crate::drivers::uart::Uart::new();
    unsafe { _init(bar0_pa, &uart) }
}

unsafe fn _init(bar0_pa: usize, uart: &crate::drivers::uart::Uart) -> bool {
    HDA_MMIO = phys_to_virt(bar0_pa);
    uart.puts("[hda]  Initializing Intel HDA...\r\n");

    // ── 1. Global reset ───────────────────────────────────────────────────────
    wr32(GCTL, 0);         // assert CRST (reset)
    spin_us(100);
    wr32(GCTL, GCTL_CRST); // release reset

    // Wait for controller to report ready (CRST=1)
    let mut ok = false;
    for _ in 0..1000_u32 {
        spin_us(100);
        if rd32(GCTL) & GCTL_CRST != 0 { ok = true; break; }
    }
    if !ok {
        uart.puts("[hda]  GCTL reset timeout\r\n");
        return false;
    }
    spin_us(2000); // codec needs ~521 µs after reset to enumerate

    // ── 2. Read GCAP to find stream offsets ───────────────────────────────────
    let gcap  = rd16(GCAP);
    let n_oss = ((gcap >> 12) & 0xF) as usize; // output streams
    let n_iss = ((gcap >>  8) & 0xF) as usize; // input streams
    if n_oss == 0 {
        uart.puts("[hda]  No output streams\r\n");
        return false;
    }
    // First output stream descriptor is at 0x80 + (n_iss * 0x20)
    SD_OFF = 0x80 + n_iss * 0x20;
    uart.puts("[hda]  n_iss=");
    uart.put_dec(n_iss);
    uart.puts(" n_oss=");
    uart.put_dec(n_oss);
    uart.puts(" sd_off=0x");
    uart.put_hex(SD_OFF);
    uart.puts("\r\n");

    // ── 3. Set up CORB ────────────────────────────────────────────────────────
    let corb_pa = match crate::memory::alloc_page() {
        Some(p) => p,
        None    => { uart.puts("[hda]  OOM CORB\r\n"); return false; }
    };
    core::ptr::write_bytes(phys_to_virt(corb_pa) as *mut u8, 0, 4096);
    CORB_PA = corb_pa;

    wr32(CORBLBASE, (corb_pa & 0xFFFF_FFFF) as u32);
    wr32(CORBUBASE, (corb_pa >> 32) as u32);
    wr8(CORBSIZE, 0x02);    // 256 entries
    // Reset CORBRP: set bit15, then clear
    wr16(CORBRP, 0x8000);
    spin_us(10);
    wr16(CORBRP, 0x0000);
    wr16(CORBWP, 0);
    CORB_WP = 0;
    wr8(CORBCTL, 0x02);     // DMA run

    // ── 4. Set up RIRB ────────────────────────────────────────────────────────
    let rirb_pa = match crate::memory::alloc_page() {
        Some(p) => p,
        None    => { uart.puts("[hda]  OOM RIRB\r\n"); return false; }
    };
    core::ptr::write_bytes(phys_to_virt(rirb_pa) as *mut u8, 0, 4096);
    RIRB_PA = rirb_pa;

    wr32(RIRBLBASE, (rirb_pa & 0xFFFF_FFFF) as u32);
    wr32(RIRBUBASE, (rirb_pa >> 32) as u32);
    wr8(RIRBSIZE, 0x02);    // 256 entries
    wr16(RIRBWP, 0x8000);   // reset write pointer
    spin_us(10);
    RIRB_LAST = 0;
    wr16(RINTCNT, 0xFE);    // respond after every entry
    wr8(RIRBCTL, 0x02);     // DMA run

    // ── 5. Detect codecs ──────────────────────────────────────────────────────
    spin_us(2000);
    let statests = rd16(STATESTS);
    if statests == 0 {
        uart.puts("[hda]  No codecs found\r\n");
        return false;
    }
    uart.puts("[hda]  Codec mask: 0x");
    uart.put_hex(statests as usize);
    uart.puts("\r\n");

    // ── 6. Configure codec ────────────────────────────────────────────────────
    // QEMU hda-duplex: codec 0, AFG=NID1, DAC=NID2, Pin=NID3
    let codec: u8 = 0;

    // Power on AFG (node 1): verb 0x705 = SET_POWER_STATE, payload D0=0x00
    corb_send(codec, 1, 0x705, 0x00);
    spin_us(1000);

    // Set DAC (node 2) stream tag=1, channel=0: verb 0x706 = SET_STREAM_CHANNEL
    corb_send(codec, 2, 0x706, 0x10); // stream tag 1 (bits[7:4]), channel 0 (bits[3:0])

    // Set DAC format: verb 0x200 = SET_CONVERTER_FORMAT (upper 4 bits of verb encode high byte)
    // Full 20-bit verb for converter format: 0x200 | high_byte, payload = low_byte
    let fmt = STREAM_FMT_48K_16_STEREO;
    corb_send(codec, 2, 0x200 | ((fmt >> 8) as u32), (fmt & 0xFF) as u8);

    // Unmute DAC output amplifier (node 2): 4-bit verb 0x3 = SET_AMPLIFIER_GAIN_MUTE
    // payload: bit15=out, bit13=L, bit12=R, bit7=mute, bits[6:0]=gain
    // 0xB07F: out=1, L=1, R=1, mute=0, gain=0x7F (max — clamped to num_steps by HDA spec).
    // QEMU hda-duplex amp has offset=0x1F meaning gain=0 → -54 dB (inaudible).
    // Setting gain=0x7F is clamped to the maximum step, which maps to 0 dB (unity).
    corb_send16(codec, 2, 0x3, 0xB07Fu16);

    // Enable pin widget output — try both NID 3 and NID 4 to cover QEMU versions.
    // verb 0x707 = SET_PIN_WIDGET_CONTROL, bit6=output enable
    corb_send(codec, 3, 0x707, 0x40);
    corb_send16(codec, 3, 0x3, 0xB07Fu16);
    corb_send(codec, 4, 0x707, 0x40);
    corb_send16(codec, 4, 0x3, 0xB07Fu16);

    uart.puts("[hda]  Codec configured\r\n");

    // ── 7. Generate PCM data ──────────────────────────────────────────────────
    let (pcm_pa, pcm_bytes) = generate_pcm();
    if pcm_pa == 0 || pcm_bytes == 0 {
        uart.puts("[hda]  PCM generation failed\r\n");
        return false;
    }
    uart.puts("[hda]  PCM: ");
    uart.put_dec(pcm_bytes / 1024);
    uart.puts(" KB\r\n");

    // ── 8. Set up BDL ────────────────────────────────────────────────────────
    let bdl_pa = match crate::memory::alloc_page() {
        Some(p) => p,
        None    => { uart.puts("[hda]  OOM BDL\r\n"); return false; }
    };
    BDL_PA = bdl_pa;
    let bdl_va = phys_to_virt(bdl_pa) as *mut BdlEntry;
    // Single BDL entry covering the entire PCM buffer
    core::ptr::write_volatile(bdl_va, BdlEntry {
        addr:   pcm_pa as u64,
        length: pcm_bytes as u32,
        ioc:    1,
    });

    // ── 9. Configure output stream descriptor ─────────────────────────────────
    // Assert stream reset
    sd_wr32(SD_CTL, SD_CTL_SRST);
    spin_us(100);
    // Wait for SRST to read back 1
    for _ in 0..1000_u32 { spin_us(10); if sd_rd32(SD_CTL) & SD_CTL_SRST != 0 { break; } }
    // Clear stream reset
    sd_wr32(SD_CTL, 0);
    for _ in 0..1000_u32 { spin_us(10); if sd_rd32(SD_CTL) & SD_CTL_SRST == 0 { break; } }

    // Stream tag = 1 in bits [23:20]
    sd_wr32(SD_CTL, 1 << SD_CTL_STRM_SHIFT);
    sd_wr32(SD_CBL, pcm_bytes as u32);           // cyclic buffer length
    sd_wr16(SD_LVI, 0);                           // last valid BDL index = 0 (1 entry)
    sd_wr16(SD_FMT, STREAM_FMT_48K_16_STEREO);
    // BDL base address
    wr32(SD_OFF + SD_BDPL, (bdl_pa & 0xFFFF_FFFF) as u32);
    wr32(SD_OFF + SD_BDPU, (bdl_pa >> 32) as u32);

    // ── 10. Run! ──────────────────────────────────────────────────────────────
    let ctl = sd_rd32(SD_CTL);
    sd_wr32(SD_CTL, ctl | SD_CTL_RUN);

    READY = true;
    uart.puts("[hda]  Stream running — chiptune playing!\r\n");

    // Arm the one-shot stop: timer fires at 100 Hz, so ticks = ms / 10.
    // Add 20 ticks of margin so the final note fully decays.
    let start_tick = crate::arch::aarch64::timer::ticks();
    let stop_tick  = start_tick + MELODY_TOTAL_MS / 10 + 20;
    STOP_AT_TICK.store(stop_tick, Ordering::Relaxed);

    true
}

pub fn is_ready() -> bool {
    unsafe { READY }
}

// ── Text-to-Speech ─────────────────────────────────────────────────────────────

/// Letter/character → (F1_milliHz, F2_milliHz, duration_ms).
/// Vowels use two-formant synthesis. Consonants use a single tone burst.
/// Zero frequency = silence.
fn phoneme_for(ch: u8) -> (u32, u32, u32) {
    let c = if ch >= b'a' && ch <= b'z' { ch - 32 } else { ch };
    match c {
        b'A'            => (800_000,  1200_000, 110),
        b'E'            => (400_000,  2200_000, 100),
        b'I'            => (300_000,  2800_000,  90),
        b'O'            => (500_000,   900_000, 110),
        b'U'            => (400_000,   800_000, 100),
        b'B' | b'P'     => (150_000,         0,  40),
        b'M' | b'N'     => (220_000,         0,  55),
        b'D' | b'T'     => (450_000,         0,  40),
        b'L'            => (280_000,         0,  55),
        b'R'            => (200_000,         0,  60),
        b'S' | b'Z'     => (4000_000,        0,  45),
        b'F' | b'V'     => (2500_000,        0,  45),
        b'K' | b'G'     => (380_000,         0,  40),
        b'W'            => (260_000,   800_000,  70),
        b'Y' | b'J'     => (300_000,  2000_000,  60),
        b'H'            => (3000_000,        0,  30),
        b'Q' | b'C'     => (380_000,   900_000,  50),
        b'X'            => (500_000,  2000_000,  50),
        b' ' | b'\t'    => (0, 0,  70),
        b',' | b';'     => (0, 0, 130),
        b'.' | b'!' | b'?' => (0, 0, 220),
        b'0'..=b'9'     => (400_000 + (ch - b'0') as u32 * 40_000, 0, 80),
        _               => (0, 0,  25),
    }
}

/// Write `samples` frames of a triangle wave at `freq_mhz` millihertz into
/// `buf` starting at `byte_pos`.  Returns new byte_pos.
fn write_triangle(buf: &mut [u8], mut byte_pos: usize,
                  freq_mhz: u32, amp: i32, samples: usize) -> usize {
    let period = (SAMPLE_RATE as u64 * 1000 / freq_mhz as u64).max(1) as usize;
    let fade = (SAMPLE_RATE * 6 / 1000) as usize;
    for i in 0..samples {
        if byte_pos + 4 > buf.len() { break; }
        let gain = if i < fade { amp * i as i32 / fade as i32 }
                   else if samples > fade && i >= samples - fade {
                       amp * (samples - i) as i32 / fade as i32 }
                   else { amp };
        let phase = i % period;
        let tri: i32 = if phase < period / 2 {
            gain - 2 * gain * phase as i32 / period as i32
        } else {
            -gain + 2 * gain * (phase - period / 2) as i32 / period as i32
        };
        let s = tri.clamp(-32767, 32767) as i16;
        let [lo, hi] = s.to_le_bytes();
        buf[byte_pos] = lo; buf[byte_pos+1] = hi;
        buf[byte_pos+2] = lo; buf[byte_pos+3] = hi;
        byte_pos += 4;
    }
    byte_pos
}

/// Synthesise PCM for `text` into `PCM_BUF`.  Returns (PA, byte_count).
unsafe fn generate_tts_pcm(text: &[u8]) -> (usize, usize) {
    let buf = &mut PCM_BUF;
    let mut pos = 0usize;

    for &ch in text {
        let (f1, f2, dur_ms) = phoneme_for(ch);
        let samples = (SAMPLE_RATE as u64 * dur_ms as u64 / 1000) as usize;

        if f1 == 0 || samples == 0 {
            // Silence
            let end = (pos + samples * 4).min(buf.len());
            buf[pos..end].fill(0);
            pos = end;
            continue;
        }

        if f2 == 0 {
            // Single-formant consonant
            pos = write_triangle(buf, pos, f1, 7000, samples);
        } else {
            // Two-formant vowel: mix both at half amplitude
            let period1 = (SAMPLE_RATE as u64 * 1000 / f1 as u64).max(1) as usize;
            let period2 = (SAMPLE_RATE as u64 * 1000 / f2 as u64).max(1) as usize;
            let fade = (SAMPLE_RATE * 6 / 1000) as usize;
            let amp = 4500i32;
            for i in 0..samples {
                if pos + 4 > buf.len() { break; }
                let gain = if i < fade { amp * i as i32 / fade as i32 }
                           else if samples > fade && i >= samples - fade {
                               amp * (samples - i) as i32 / fade as i32 }
                           else { amp };
                let ph1 = i % period1;
                let ph2 = i % period2;
                let tri1 = if ph1 < period1/2 {
                    gain - 2*gain*ph1 as i32/period1 as i32
                } else { -gain + 2*gain*(ph1-period1/2) as i32/period1 as i32 };
                let tri2 = if ph2 < period2/2 {
                    gain - 2*gain*ph2 as i32/period2 as i32
                } else { -gain + 2*gain*(ph2-period2/2) as i32/period2 as i32 };
                let s = (tri1 + tri2).clamp(-32767, 32767) as i16;
                let [lo, hi] = s.to_le_bytes();
                buf[pos] = lo; buf[pos+1] = hi;
                buf[pos+2] = lo; buf[pos+3] = hi;
                pos += 4;
            }
        }
    }

    // Cache-flush so HDA DMA sees the new samples.
    let va = PCM_BUF.as_ptr() as usize;
    let pa = va - KERNEL_VA_OFFSET;
    let cache_line = 64usize;
    let end_va = va + pos;
    let mut p = va & !(cache_line - 1);
    while p < end_va {
        core::arch::asm!("dc cvac, {}", in(reg) p, options(nostack));
        p += cache_line;
    }
    core::arch::asm!("dsb sy", options(nostack));

    (pa, pos)
}

/// Synthesise and play `text` through the HDA output stream.
/// Returns `false` if HDA was never initialised.
pub fn speak(text: &[u8]) -> bool {
    unsafe {
        if !READY || HDA_MMIO == 0 || BDL_PA == 0 { return false; }

        // Stop current stream
        let ctl = rd32(SD_OFF + SD_CTL);
        wr32(SD_OFF + SD_CTL, ctl & !(SD_CTL_RUN));
        spin_us(500);

        // Generate new PCM
        let (pcm_pa, pcm_bytes) = generate_tts_pcm(text);
        if pcm_bytes == 0 { return false; }

        // Reset stream descriptor
        sd_wr32(SD_CTL, SD_CTL_SRST);
        spin_us(100);
        for _ in 0..1000_u32 { spin_us(10); if sd_rd32(SD_CTL) & SD_CTL_SRST != 0 { break; } }
        sd_wr32(SD_CTL, 0);
        for _ in 0..1000_u32 { spin_us(10); if sd_rd32(SD_CTL) & SD_CTL_SRST == 0 { break; } }

        // Update BDL entry with new length (same buffer PA, new byte count)
        let bdl_va = phys_to_virt(BDL_PA) as *mut BdlEntry;
        core::ptr::write_volatile(bdl_va, BdlEntry {
            addr:   pcm_pa as u64,
            length: pcm_bytes as u32,
            ioc:    1,
        });

        // Reconfigure stream
        sd_wr32(SD_CTL, 1 << SD_CTL_STRM_SHIFT);
        sd_wr32(SD_CBL, pcm_bytes as u32);
        sd_wr16(SD_LVI, 0);
        sd_wr16(SD_FMT, STREAM_FMT_48K_16_STEREO);
        wr32(SD_OFF + SD_BDPL, (BDL_PA & 0xFFFF_FFFF) as u32);
        wr32(SD_OFF + SD_BDPU, (BDL_PA >> 32) as u32);

        // Start stream
        let c2 = sd_rd32(SD_CTL);
        sd_wr32(SD_CTL, c2 | SD_CTL_RUN);

        // Arm auto-stop timer
        let dur_ms = pcm_bytes / 4 * 1000 / SAMPLE_RATE as usize;
        let start = crate::arch::aarch64::timer::ticks();
        STOP_AT_TICK.store(start + dur_ms / 10 + 10, Ordering::Relaxed);

        true
    }
}

/// Stop DMA playback immediately.
pub fn stop() {
    unsafe {
        if HDA_MMIO != 0 {
            let ctl = rd32(SD_OFF + SD_CTL);
            wr32(SD_OFF + SD_CTL, ctl & !SD_CTL_RUN);
        }
    }
    STOP_AT_TICK.store(0, Ordering::Relaxed); // disarm tick-based stop too
}

/// Called from the 100 Hz timer IRQ. Stops DMA playback once the melody ends.
pub fn tick() {
    let stop = STOP_AT_TICK.load(Ordering::Relaxed);
    if stop == 0 {
        return;
    }
    let now = crate::arch::aarch64::timer::ticks();
    if now >= stop {
        // Disarm so we don't execute this repeatedly.
        STOP_AT_TICK.store(0, Ordering::Relaxed);
        unsafe {
            if HDA_MMIO != 0 {
                let ctl = rd32(SD_OFF + SD_CTL);
                wr32(SD_OFF + SD_CTL, ctl & !SD_CTL_RUN);
            }
        }
    }
}
