//! ELF64 loader for AArch64.
//!
//! Parses a statically-linked ELF64 binary and loads its PT_LOAD segments
//! directly into physical memory, then spawns the binary as a new EL0 task.
//!
//! ## Why it works without a separate page table per process
//!
//! The kernel uses an identity-mapped page table (`vmm::USER_L0`) that covers
//! all of RAM (0x4000_0000 – 0x1_4000_0000) with EL0 read/write/execute
//! permissions.  Under identity mapping VA == PA, so placing ELF segments at
//! their `p_vaddr` addresses is the same as placing them at their physical
//! addresses — no additional mapping work is needed.
//!
//! Per-process VA isolation (Item 12, PHASE2.md) will replace this shared
//! table with private per-task page trees.
//!
//! ## Linker constraints for user ELFs
//!
//! - Must be statically linked (`-static`)
//! - Machine: `AArch64` (e_machine = 0xB7)
//! - Link base: any RAM address (e.g. `0x4800_0000`)
//! - Entry must be within RAM (accessible from EL0 via USER_L0)

use super::scheduler;

// ── ELF constants ─────────────────────────────────────────────────────────────

const ELF_MAGIC:  [u8; 4] = [0x7F, b'E', b'L', b'F'];
const ELFCLASS64: u8       = 2;
const EM_AARCH64: u16      = 0xB7;
const PT_LOAD:    u32      = 1;

// ── Error type ────────────────────────────────────────────────────────────────

/// Errors produced by the ELF loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElfError {
    /// File too small to contain a valid ELF header.
    TooShort,
    /// First four bytes are not `\x7FELF`.
    BadMagic,
    /// EI_CLASS ≠ 2 (not a 64-bit ELF).
    Not64Bit,
    /// e_machine ≠ 0xB7 (not AArch64).
    WrongArch,
    /// A PT_LOAD segment references data outside the file or has invalid sizes.
    BadSegment,
}

impl ElfError {
    pub fn as_str(self) -> &'static str {
        match self {
            ElfError::TooShort   => "ELF: file too short",
            ElfError::BadMagic   => "ELF: bad magic",
            ElfError::Not64Bit   => "ELF: not 64-bit",
            ElfError::WrongArch  => "ELF: wrong architecture (need AArch64)",
            ElfError::BadSegment => "ELF: bad PT_LOAD segment",
        }
    }
}

// ── Byte-level field reads (avoids alignment concerns) ────────────────────────

#[inline(always)]
fn u16le(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

#[inline(always)]
fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[inline(always)]
fn u64le(b: &[u8], off: usize) -> u64 {
    // Panics if slice is too short — callers validate before calling.
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}

// ── ELF file-header field offsets (ELF64, LE) ─────────────────────────────────
//   0x00  e_ident[16]
//   0x10  e_type      u16
//   0x12  e_machine   u16
//   0x14  e_version   u32
//   0x18  e_entry     u64
//   0x20  e_phoff     u64
//   0x28  e_shoff     u64
//   0x30  e_flags     u32
//   0x34  e_ehsize    u16
//   0x36  e_phentsize u16
//   0x38  e_phnum     u16
//   …

// ── ELF program-header field offsets (Phdr64) ─────────────────────────────────
//   0x00  p_type   u32
//   0x04  p_flags  u32
//   0x08  p_offset u64
//   0x10  p_vaddr  u64
//   0x18  p_paddr  u64
//   0x20  p_filesz u64
//   0x28  p_memsz  u64
//   0x30  p_align  u64
//   size = 56 bytes

// ── Public API ────────────────────────────────────────────────────────────────

/// Parse and load an ELF64 binary into physical memory.
///
/// All `PT_LOAD` segments are written to their `p_vaddr` address (== physical
/// address under identity mapping).  BSS regions (`p_memsz > p_filesz`) are
/// zeroed.
///
/// Returns the entry point address (`e_entry`) on success.
pub fn load_elf(bytes: &[u8]) -> Result<usize, ElfError> {
    // 64-byte ELF header is the minimum
    if bytes.len() < 64 {
        return Err(ElfError::TooShort);
    }

    // Validate magic and class
    if &bytes[0..4] != ELF_MAGIC {
        return Err(ElfError::BadMagic);
    }
    if bytes[4] != ELFCLASS64 {
        return Err(ElfError::Not64Bit);
    }
    if u16le(bytes, 0x12) != EM_AARCH64 {
        return Err(ElfError::WrongArch);
    }

    let e_entry     = u64le(bytes, 0x18) as usize;
    let e_phoff     = u64le(bytes, 0x20) as usize;
    let e_phentsize = u16le(bytes, 0x36) as usize;
    let e_phnum     = u16le(bytes, 0x38) as usize;

    // Walk program headers
    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;

        // Need at least 40 bytes for p_type … p_memsz
        if ph + 40 > bytes.len() {
            return Err(ElfError::TooShort);
        }

        let p_type   = u32le(bytes, ph);
        if p_type != PT_LOAD {
            continue;
        }

        let p_offset = u64le(bytes, ph + 0x08) as usize;
        let p_vaddr  = u64le(bytes, ph + 0x10) as usize;
        let p_filesz = u64le(bytes, ph + 0x20) as usize;
        let p_memsz  = u64le(bytes, ph + 0x28) as usize;

        if p_memsz == 0 {
            continue;
        }

        // Validate file-side bounds
        if p_filesz > 0 {
            if p_offset + p_filesz > bytes.len() {
                return Err(ElfError::BadSegment);
            }
        }

        // Copy to physical memory (via high-VA alias)
        let dst_va = crate::memory::vmm::phys_to_virt(p_vaddr);
        let dst = dst_va as *mut u8;
        unsafe {
            if p_filesz > 0 {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(p_offset),
                    dst,
                    p_filesz,
                );
            }
            // Zero BSS (p_memsz - p_filesz bytes after the file data)
            if p_memsz > p_filesz {
                core::ptr::write_bytes(dst.add(p_filesz), 0, p_memsz - p_filesz);
            }
        }
    }

    Ok(e_entry)
}

// ── ASLR ─────────────────────────────────────────────────────────────────────

/// ET_DYN (position-independent) ELF type.
const ET_DYN: u16 = 3;

/// EL0-accessible RAM where ASLR slides can be placed.
/// We pick from 0x4800_0000 – 0x8000_0000 (about 896 MB of randomisation
/// space), in 64 KB-aligned (16-page) steps → 14336 possible base addresses.
const ASLR_BASE:    usize = 0x4800_0000;
const ASLR_LIMIT:   usize = 0x8000_0000;
const ASLR_GRANULE: usize = 0x10000; // 64 KB

/// Generate a random ASLR slide for a PIE ELF whose lowest segment vaddr is
/// `min_vaddr`.  Returns a page-aligned offset that keeps the binary inside
/// EL0 RAM and avoids placing it below `ASLR_BASE`.
fn aslr_slide(min_vaddr: usize) -> usize {
    let mut rand_bytes = [0u8; 4];
    crate::csprng::fill_bytes(&mut rand_bytes);
    let r = u32::from_le_bytes(rand_bytes) as usize;

    // How many granule slots fit in [ASLR_BASE, ASLR_LIMIT)?
    let slots = (ASLR_LIMIT - ASLR_BASE) / ASLR_GRANULE;
    let slot  = r % slots;
    let base  = ASLR_BASE + slot * ASLR_GRANULE;

    // Slide = desired_base - min_vaddr (may wrap if min_vaddr > base, but
    // the resulting load address is what matters — we keep it in bounds).
    base.wrapping_sub(min_vaddr)
}

/// Load an ELF64 binary and spawn it as a new EL0 (user) task.
///
/// Parses PT_LOAD segments, copies them to their p_vaddr (== PA under
/// identity mapping), builds a per-process page table, and spawns the task.
///
/// **ASLR**: ET_DYN (PIE) binaries receive a random load-base slide.
/// ET_EXEC binaries are loaded at their link-time vaddrs (no relocation).
///
/// `bytes` — raw ELF file contents in memory.
/// `caps`  — capability bitmask granted to the new task.
pub fn spawn_elf(name: &str, bytes: &[u8], caps: u64) -> Result<usize, &'static str> {
    use crate::memory::vmm::ProcSegment;

    if bytes.len() < 64              { return Err(ElfError::TooShort.as_str()); }
    if &bytes[0..4] != ELF_MAGIC     { return Err(ElfError::BadMagic.as_str()); }
    if bytes[4] != ELFCLASS64        { return Err(ElfError::Not64Bit.as_str()); }
    if u16le(bytes, 0x12) != EM_AARCH64 { return Err(ElfError::WrongArch.as_str()); }

    let e_type      = u16le(bytes, 0x10);
    let e_entry_raw = u64le(bytes, 0x18) as usize;
    let e_phoff     = u64le(bytes, 0x20) as usize;
    let e_phentsize = u16le(bytes, 0x36) as usize;
    let e_phnum     = u16le(bytes, 0x38) as usize;

    // Compute ASLR slide for PIE (ET_DYN) binaries.
    // For ET_EXEC the slide is 0 — the linker chose fixed vaddrs.
    let aslr_offset = if e_type == ET_DYN {
        // Find the lowest p_vaddr to anchor the slide correctly.
        let mut min_va = usize::MAX;
        for i in 0..e_phnum {
            let ph = e_phoff + i * e_phentsize;
            if ph + 48 > bytes.len() { continue; }
            if u32le(bytes, ph) != PT_LOAD { continue; }
            let va = u64le(bytes, ph + 0x10) as usize;
            if va < min_va { min_va = va; }
        }
        if min_va == usize::MAX { 0 } else { aslr_slide(min_va) }
    } else {
        0
    };

    let e_entry = e_entry_raw.wrapping_add(aslr_offset);

    // ELF program-header p_flags bits
    const PF_X: u32 = 1 << 0;
    const PF_W: u32 = 1 << 1;

    // Collect PT_LOAD segments (max 16)
    let mut segs: [ProcSegment; 16] = core::array::from_fn(|_| ProcSegment {
        pa: 0, size: 0, el0: false, exec: false, dev: false,
    });
    let mut n_segs = 0usize;

    for i in 0..e_phnum {
        let ph = e_phoff + i * e_phentsize;
        if ph + 48 > bytes.len() { return Err(ElfError::TooShort.as_str()); }

        let p_type   = u32le(bytes, ph);
        if p_type != PT_LOAD { continue; }

        let p_flags  = u32le(bytes, ph + 0x04);
        let p_offset = u64le(bytes, ph + 0x08) as usize;
        let p_vaddr  = u64le(bytes, ph + 0x10) as usize;
        let p_filesz = u64le(bytes, ph + 0x20) as usize;
        let p_memsz  = u64le(bytes, ph + 0x28) as usize;

        if p_memsz == 0 { continue; }
        if p_filesz > 0 && p_offset + p_filesz > bytes.len() {
            return Err(ElfError::BadSegment.as_str());
        }

        // Apply ASLR slide to the segment's virtual address.
        let load_va = p_vaddr.wrapping_add(aslr_offset);

        // Copy segment to physical memory using kernel high-VA alias
        let dst_va = crate::memory::vmm::phys_to_virt(load_va);
        let dst = dst_va as *mut u8;
        unsafe {
            if p_filesz > 0 {
                core::ptr::copy_nonoverlapping(
                    bytes.as_ptr().add(p_offset), dst, p_filesz,
                );
            }
            if p_memsz > p_filesz {
                core::ptr::write_bytes(dst.add(p_filesz), 0, p_memsz - p_filesz);
            }
        }

        // Record for per-process page table (use slid address as PA).
        if n_segs < 16 {
            segs[n_segs] = ProcSegment {
                pa:   load_va,
                size: p_memsz,
                el0:  true,
                exec: p_flags & PF_X != 0,
                dev:  false,
            };
            n_segs += 1;
        }
    }

    scheduler::spawn_user_entry_with_segs(name, e_entry, caps, &segs[..n_segs])
}
