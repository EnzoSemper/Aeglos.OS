/// Virtual memory manager — TTBR1 kernel/user VA split.
///
/// The kernel is linked at VMA = PA + KERNEL_VA_OFFSET so that EL1 code always
/// runs via TTBR1 (high VA).  TTBR0 is per-process (user ELF/stack only).
///
/// `init()` builds two L0 tables that share all L1/L2 subtables:
///   TTBR0 — identity map (low VA = PA): MMIO + kernel RAM
///   TTBR1 — high-VA map  (VA = PA + KERNEL_VA_OFFSET): same PAs
///
/// Because AArch64 page table entries store physical output addresses, the
/// same L1/L2 physical tables are valid for both TTBR0 and TTBR1.

use crate::memory;

// ── Kernel virtual-to-physical offset ────────────────────────────────────────

/// Offset added to a PA to obtain the kernel virtual address.
/// Bit 63 set → hardware selects TTBR1 for all kernel VAs.
pub const KERNEL_VA_OFFSET: usize = 0xFFFF_0000_0000_0000;

/// Convert a physical address to the corresponding kernel virtual address.
#[inline(always)]
pub fn phys_to_virt(pa: usize) -> usize {
    pa + KERNEL_VA_OFFSET
}

/// Convert a kernel virtual address back to its physical address.
#[inline(always)]
pub fn virt_to_phys(va: usize) -> usize {
    va - KERNEL_VA_OFFSET
}

/// Convert any kernel pointer (either high VA or raw PA) to a physical address
/// suitable for DMA descriptors or hardware registers.
///
/// High VAs (bit 63 set) → subtract KERNEL_VA_OFFSET to get PA.
/// Low VAs  (< KERNEL_VA_OFFSET) → already a PA, return unchanged.
#[inline(always)]
pub fn to_dma_addr(ptr: usize) -> usize {
    if ptr >= KERNEL_VA_OFFSET { ptr - KERNEL_VA_OFFSET } else { ptr }
}

// ── AArch64 page table entry bits ────────────────────────────────────────────

const PTE_VALID: u64        = 1 << 0;
const PTE_TABLE: u64        = 1 << 1;  // Table descriptor (L0–L2)
const PTE_AF: u64           = 1 << 10; // Access flag
const PTE_SH_INNER: u64     = 3 << 8;  // Inner shareable
const PTE_AP_RW: u64        = 0 << 6;  // AP[2:1]=00: EL1 RW, EL0 no access
const PTE_AP_USER_RW: u64   = 1 << 6;  // AP[2:1]=01: EL0+EL1 RW
const PTE_UXN: u64          = 1 << 54; // Unprivileged execute-never
const PTE_PXN: u64          = 1 << 53; // Privileged execute-never

const ATTR_NORMAL: u64   = 0 << 2;  // MAIR index 0: Normal cacheable
const ATTR_DEVICE: u64   = 1 << 2;  // MAIR index 1: Device-nGnRnE

// Block descriptor: valid but bit 1 clear = block (not table) entry.
const BLOCK_DESC: u64    = PTE_VALID;

const ENTRIES_PER_TABLE: usize = 512;
const L2_BLOCK_SIZE: usize = 2 * 1024 * 1024; // 2 MiB

// ── Page table type ───────────────────────────────────────────────────────────

/// A single 4-KiB-aligned page table (512 × 8-byte entries).
#[repr(C, align(4096))]
struct PageTable {
    entries: [u64; ENTRIES_PER_TABLE],
}

impl PageTable {
    const fn zero() -> Self {
        Self { entries: [0; ENTRIES_PER_TABLE] }
    }
}

// ── Static L0 tables ─────────────────────────────────────────────────────────

/// TTBR0 L0 — kernel identity table (low VA = PA, EL1-only).
static mut L0: PageTable = PageTable::zero();

/// TTBR1 L0 — high-VA kernel table (VA = PA + KERNEL_VA_OFFSET, EL1-only).
static mut L0_TTBR1: PageTable = PageTable::zero();

/// Shared user L0 — legacy shared user table built by create_user_table().
static mut USER_L0: PageTable = PageTable::zero();

/// TTBR0 value (PA of L0) installed at init time.
static mut TTBR0_ADDR: usize = 0;
/// TTBR1 value (PA of L0_TTBR1) installed at init time.
static mut TTBR1_ADDR: usize = 0;
/// Shared user TTBR0 value (PA of USER_L0).
static mut USER_TTBR0_ADDR: usize = 0;

// ── Helper: allocate a zeroed page for a page table ──────────────────────────

fn alloc_table() -> usize {
    memory::alloc_page().expect("vmm: out of memory for page tables")
}

/// Dereference a page-table PA as a mutable reference.
///
/// # Safety
/// `pa` must be a valid physical address returned by `alloc_table()` or a
/// static table whose PA was derived via `virt_to_phys()`.
#[inline(always)]
unsafe fn tbl(pa: usize) -> *mut PageTable {
    phys_to_virt(pa) as *mut PageTable
}

// ── VMM init ─────────────────────────────────────────────────────────────────

/// Build the runtime kernel page tables and return (ttbr0_pa, ttbr1_pa).
///
/// * TTBR0 maps PA 0x0800_0000–0x0C00_0000 (MMIO, EL1-only, device) and
///   PA 0x4000_0000–0x2_4000_0000 (RAM, EL1-only, normal, PXN=0).
/// * TTBR1 maps the exact same PA ranges at VA = PA + KERNEL_VA_OFFSET,
///   sharing all L1/L2 sub-tables with TTBR0.
///
/// Must be called after `memory::init()` (page allocator).
pub fn init() -> (usize, usize) {
    unsafe {
        // Derive PAs of the static L0 tables (they are BSS at high VA now).
        let ttbr0_pa = virt_to_phys(&raw const L0 as usize);
        let ttbr1_pa = virt_to_phys(&raw const L0_TTBR1 as usize);

        // Allocate a shared L1 table (both L0s point to the same L1).
        let l1_pa = alloc_table();
        let l1    = &mut *tbl(l1_pa);

        // Both L0 tables → shared L1.
        L0.entries[0]       = (l1_pa as u64) | PTE_VALID | PTE_TABLE;
        L0_TTBR1.entries[0] = (l1_pa as u64) | PTE_VALID | PTE_TABLE;

        // ── MMIO: 0x0800_0000–0x0C00_0000 (EL1-only, device, 2 MiB blocks) ──
        let l2_mmio_pa = alloc_table();
        let l2_mmio    = &mut *tbl(l2_mmio_pa);
        l1.entries[0]  = (l2_mmio_pa as u64) | PTE_VALID | PTE_TABLE;

        let device_flags = BLOCK_DESC | PTE_AF | PTE_SH_INNER | PTE_AP_RW
                         | ATTR_DEVICE | PTE_UXN | PTE_PXN;
        let mmio_base_idx = 0x0800_0000_usize >> 21; // 64
        for i in 0..32 {
            let pa = ((mmio_base_idx + i) << 21) as u64;
            l2_mmio.entries[mmio_base_idx + i] = pa | device_flags;
        }

        // ── RAM: 0x4000_0000–0x4_4000_0000 (EL1-only, normal, 2 MiB blocks) ──
        // PXN=0: EL1 must execute kernel code; UXN=1: EL0 must not.
        // Upper bound covers 16 GiB; actual pages below ram_end are marked free.
        let kernel_flags = BLOCK_DESC | PTE_AF | PTE_SH_INNER | PTE_AP_RW
                         | ATTR_NORMAL | PTE_UXN; // PXN=0

        let mut addr: usize = 0x4000_0000;
        let end:  usize     = 0x4_4000_0000;
        while addr < end {
            let l1_idx  = (addr >> 30) & 0x1FF;
            let l2_pa   = alloc_table();
            let l2      = &mut *tbl(l2_pa);
            l1.entries[l1_idx] = (l2_pa as u64) | PTE_VALID | PTE_TABLE;

            for i in 0..ENTRIES_PER_TABLE {
                if addr >= end { break; }
                l2.entries[i] = (addr as u64) | kernel_flags;
                addr += L2_BLOCK_SIZE;
            }
        }

        TTBR0_ADDR = ttbr0_pa;
        TTBR1_ADDR = ttbr1_pa;
        (ttbr0_pa, ttbr1_pa)
    }
}

/// Return the kernel TTBR0 PA (identity table for kernel tasks).
pub fn ttbr0() -> usize {
    unsafe { TTBR0_ADDR }
}

/// Return the kernel TTBR1 PA.
#[allow(dead_code)]
pub fn ttbr1() -> usize {
    unsafe { TTBR1_ADDR }
}

/// Return the shared user TTBR0 PA (legacy path).
pub fn user_ttbr0() -> usize {
    unsafe { USER_TTBR0_ADDR }
}

/// Get the currently active TTBR0 (identity table, same as ttbr0()).
pub fn current_ttbr0() -> usize {
    unsafe { TTBR0_ADDR }
}

// ── Shared user table (legacy path) ──────────────────────────────────────────

/// Build the shared user-accessible identity table.
///
/// After the TTBR1 split the kernel accesses MMIO via TTBR1, so user tables
/// need NOT contain MMIO mappings.  This table only maps RAM for EL0 access.
pub fn create_user_table() -> usize {
    unsafe {
        let l0_pa = virt_to_phys(&raw const USER_L0 as usize);

        let l1_pa = alloc_table();
        let l1    = &mut *tbl(l1_pa);
        USER_L0.entries[0] = (l1_pa as u64) | PTE_VALID | PTE_TABLE;

        // RAM: 0x4000_0000–0x4_4000_0000 (EL0+EL1 RW, EL0 executable).
        let mut addr: usize = 0x4000_0000;
        let end:  usize     = 0x4_4000_0000;
        while addr < end {
            let l1_idx = (addr >> 30) & 0x1FF;
            let l2_pa  = alloc_table();
            let l2     = &mut *tbl(l2_pa);
            l1.entries[l1_idx] = (l2_pa as u64) | PTE_VALID | PTE_TABLE;
            for i in 0..ENTRIES_PER_TABLE {
                if addr >= end { break; }
                let flags = BLOCK_DESC | PTE_AF | PTE_SH_INNER
                          | PTE_AP_USER_RW | ATTR_NORMAL;
                l2.entries[i] = (addr as u64) | flags;
                addr += L2_BLOCK_SIZE;
            }
        }

        USER_TTBR0_ADDR = l0_pa;
        l0_pa
    }
}

// ── Per-process page table ────────────────────────────────────────────────────

/// Describes one region to map in a per-process page table.
pub struct ProcSegment {
    /// Physical address of the region (= VA under identity map for user ELFs).
    pub pa:   usize,
    /// Size in bytes (ceiled to 4 KiB internally).
    pub size: usize,
    /// True → AP[2:1]=01 (EL0+EL1 read/write).
    pub el0:  bool,
    /// True → UXN cleared (EL0 may execute).
    pub exec: bool,
    /// True → MAIR index 1 (Device).  False → Normal cacheable.
    pub dev:  bool,
}

/// Create a per-process page table containing only the given user segments.
///
/// After the TTBR1 split, per-process TTBR0 tables contain NO kernel RAM and
/// NO MMIO mappings — EL1 exception handlers access those via TTBR1.  Only
/// the caller-supplied ELF segments and user stack are mapped.
///
/// Returns the L0 physical address — store in `Task::ttbr0`.
pub fn create_process_table(segs: &[ProcSegment]) -> usize {
    let root = alloc_table();

    for seg in segs {
        if seg.size == 0 { continue; }
        let attr  = if seg.dev  { ATTR_DEVICE    } else { ATTR_NORMAL };
        let ap    = if seg.el0  { PTE_AP_USER_RW } else { PTE_AP_RW };
        let uxn   = if seg.exec { 0              } else { PTE_UXN };
        // PXN=1: EL1 must never execute user pages.
        let flags = PTE_VALID | PTE_TABLE | PTE_AF | PTE_SH_INNER
                  | attr | ap | uxn | PTE_PXN;
        map_pages(root, seg.pa, seg.pa, seg.size, flags);
    }

    root
}

// ── Page mapping helpers ──────────────────────────────────────────────────────

/// Map a contiguous range of 4 KiB pages into an existing page table.
pub fn map_pages(root: usize, pa: usize, va: usize, size: usize, flags: u64) {
    let pa_base = pa & !0xFFF;
    let va_base = va & !0xFFF;
    let pages   = (size + (pa - pa_base) + 0xFFF) / 4096;
    for i in 0..pages {
        let page_pa = pa_base + i * 4096;
        let page_va = va_base + i * 4096;
        map_one_page(root, page_pa, page_va, flags);
    }
}

/// Install a single 4 KiB page descriptor (walks / allocates L0→L1→L2→L3).
fn map_one_page(root: usize, pa: usize, va: usize, flags: u64) {
    unsafe {
        let l0_ptr = tbl(root);
        let l0_idx = (va >> 39) & 0x1FF;
        let l1_idx = (va >> 30) & 0x1FF;
        let l2_idx = (va >> 21) & 0x1FF;
        let l3_idx = (va >> 12) & 0x1FF;

        let l1 = get_or_alloc_subtable(&mut (*l0_ptr).entries[l0_idx]);
        let l2 = get_or_alloc_subtable(&mut (*l1).entries[l1_idx]);
        let l3 = get_or_alloc_subtable(&mut (*l2).entries[l2_idx]);

        // Merge UXN: keep execute-permission if either mapping allows it.
        let existing = (*l3).entries[l3_idx];
        let merged_flags = if existing & PTE_VALID != 0 && existing & PTE_AP_USER_RW != 0 {
            let uxn = (existing & PTE_UXN) & (flags & PTE_UXN);
            (flags & !PTE_UXN) | uxn
        } else {
            flags
        };

        (*l3).entries[l3_idx] = (pa as u64 & 0x0000_FFFF_FFFF_F000) | merged_flags;
    }
}

/// Install a single 2 MiB block descriptor at L2 (walks / allocates L0→L1→L2).
fn map_block(root: usize, pa: usize, va: usize, flags: u64) {
    unsafe {
        let l0_ptr = tbl(root);
        let l0_idx = (va >> 39) & 0x1FF;
        let l1_idx = (va >> 30) & 0x1FF;
        let l2_idx = (va >> 21) & 0x1FF;

        let l1 = get_or_alloc_subtable(&mut (*l0_ptr).entries[l0_idx]);
        let l2 = get_or_alloc_subtable(&mut (*l1).entries[l1_idx]);

        (*l2).entries[l2_idx] = (pa as u64 & 0x0000_FFFF_FFE0_0000) | flags;
    }
}

/// Return the next-level table, allocating and splitting blocks as needed.
///
/// If `entry` is a block descriptor, split it into 512 × 4 KiB pages before
/// returning the new sub-table (required when a user page lands inside an
/// existing kernel 2 MiB block).
///
/// # Safety
/// `entry` must be an L0/L1/L2 slot (not an L3 slot).
unsafe fn get_or_alloc_subtable(entry: &mut u64) -> *mut PageTable {
    if *entry & PTE_VALID == 0 {
        let t = alloc_table();
        *entry = (t as u64) | PTE_VALID | PTE_TABLE;
        tbl(t)
    } else if *entry & PTE_TABLE == 0 {
        // Block descriptor — split into 512 × 4 KiB page descriptors.
        let block_pa   = (*entry & 0x0000_FFFF_FFE0_0000) as usize;
        let page_flags = (*entry & !0x0000_FFFF_FFE0_0000u64) | PTE_TABLE;
        let t          = alloc_table();
        let new_tbl    = &mut *tbl(t);
        for i in 0..ENTRIES_PER_TABLE {
            let page_pa = (block_pa + i * 4096) as u64;
            new_tbl.entries[i] = (page_pa & 0x0000_FFFF_FFFF_F000) | page_flags;
        }
        *entry = (t as u64) | PTE_VALID | PTE_TABLE;
        tbl(t)
    } else {
        let addr = (*entry & 0x0000_FFFF_FFFF_F000) as usize;
        tbl(addr)
    }
}

// ── Unmap and free pages ──────────────────────────────────────────────────────

/// Unmap a range of 4 KiB pages from a process table, TLB-flush each VA,
/// and free the physical pages back to the allocator.
pub fn unmap_pages(root: usize, va: usize, size: usize) {
    let va_base = va & !0xFFF;
    let pages   = (size + (va - va_base) + 0xFFF) / 4096;
    unsafe {
        let l0_ptr = tbl(root);
        for i in 0..pages {
            let page_va = va_base + i * 4096;
            let l0_idx  = (page_va >> 39) & 0x1FF;
            let l1_idx  = (page_va >> 30) & 0x1FF;
            let l2_idx  = (page_va >> 21) & 0x1FF;
            let l3_idx  = (page_va >> 12) & 0x1FF;

            let l0e = (*l0_ptr).entries[l0_idx];
            if l0e & PTE_VALID == 0 { continue; }
            let l1 = tbl((l0e & 0x0000_FFFF_FFFF_F000) as usize);

            let l1e = (*l1).entries[l1_idx];
            if l1e & PTE_VALID == 0 { continue; }
            let l2 = tbl((l1e & 0x0000_FFFF_FFFF_F000) as usize);

            let l2e = (*l2).entries[l2_idx];
            if l2e & PTE_VALID == 0 { continue; }
            if l2e & PTE_TABLE == 0 { continue; } // Skip 2 MiB block entries
            let l3 = tbl((l2e & 0x0000_FFFF_FFFF_F000) as usize);

            let l3e = (*l3).entries[l3_idx];
            if l3e & PTE_VALID == 0 { continue; }

            (*l3).entries[l3_idx] = 0;
            let pa = (l3e & 0x0000_FFFF_FFFF_F000) as usize;
            memory::free_page(pa);

            core::arch::asm!(
                "tlbi vaae1, {0}",
                in(reg) (page_va as u64) >> 12,
                options(nostack)
            );
        }
        core::arch::asm!("dsb sy", options(nostack));
        core::arch::asm!("isb",    options(nostack));
    }
}

/// Map a range of pages as EL0 read/write, no-execute into a process table.
pub fn map_user_rw(root: usize, pa: usize, size: usize) {
    let flags = PTE_VALID | PTE_TABLE | PTE_AF | PTE_SH_INNER
              | PTE_AP_USER_RW | ATTR_NORMAL | PTE_UXN | PTE_PXN;
    map_pages(root, pa, pa, size, flags);
}

/// Map a device (MMIO) physical region into the kernel TTBR1 page table.
///
/// After this call, the region is accessible at VA = PA + KERNEL_VA_OFFSET
/// from EL1 code.  Performs DSB+ISB+TLBI to flush any stale translations.
pub fn map_kernel_mmio(pa: usize, size: usize) {
    let root = unsafe { TTBR1_ADDR };
    if root == 0 || size == 0 { return; }
    let flags = PTE_VALID | PTE_TABLE | PTE_AF | PTE_SH_INNER
              | PTE_AP_RW | ATTR_DEVICE | PTE_UXN | PTE_PXN;
    let va_base = phys_to_virt(pa) & !0xFFF;
    map_pages(root, pa, va_base, size, flags);
    // CRITICAL: tlbi vmalle1 hangs under QEMU HVF — use per-VA tlbi vaae1 instead.
    let pages = (size + 4095) / 4096;
    unsafe {
        core::arch::asm!("dsb sy", options(nostack));
        for i in 0..pages {
            let va = (va_base + i * 4096) as u64;
            core::arch::asm!(
                "tlbi vaae1, {0}",
                in(reg) va >> 12,
                options(nostack)
            );
        }
        core::arch::asm!("dsb sy", options(nostack));
        core::arch::asm!("isb",    options(nostack));
    }
}
