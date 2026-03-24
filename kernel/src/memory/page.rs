/// Bitmap-based physical page allocator.
///
/// Manages 4 KiB pages from the end of the kernel image up to the top of
/// physical RAM (discovered at boot from the DTB). The bitmap is sized for
/// the maximum supported RAM (8 GiB at 0x4000_0000–0x2_4000_0000); only
/// the pages below `ram_end` are marked free.
///
/// Call `init(ram_end)` once after BSS is zeroed, passing the end address
/// obtained from `dtb::parse_memory()`.

pub const PAGE_SIZE: usize = 4096;

/// Absolute ceiling for bitmap sizing: 16 GiB of RAM starting at 0x4000_0000.
const MAX_RAM_END: usize = 0x4_4000_0000;

/// Maximum pages the bitmap can track (16384 MiB / 4 KiB = 4 194 304).
/// Bitmap occupies 524 288 bytes (128 pages) in BSS — always reserved.
const MAX_PAGES: usize = (MAX_RAM_END - 0x4000_0000) / PAGE_SIZE;
const BITMAP_BYTES: usize = (MAX_PAGES + 7) / 8;

/// Static bitmap — one bit per physical page. 1 = free, 0 = used/reserved.
/// Placed in BSS so it's zeroed at boot (all pages start as used/reserved).
static mut BITMAP: [u8; BITMAP_BYTES] = [0; BITMAP_BYTES];

/// Base address of the first allocatable page.
static mut PAGE_BASE: usize = 0;
/// Total number of allocatable pages.
static mut NUM_PAGES: usize = 0;
/// Runtime RAM ceiling (set by `init`), used for bounds checks in `free_page`.
static mut RAM_TOP: usize = 0;

/// Align `addr` up to the next page boundary.
const fn page_align_up(addr: usize) -> usize {
    (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

/// Initialize the page allocator.
///
/// `ram_end` is the first byte *above* RAM, discovered from the DTB (or the
/// compile-time default).  Call once after BSS is zeroed.  Marks all pages
/// from `__kernel_end` (page-aligned up) to `ram_end` as free.
pub fn init(ram_end: usize) {
    extern "C" {
        static __kernel_end: u8;
    }

    // Clamp to the maximum the bitmap can represent.
    let ram_end = ram_end.min(MAX_RAM_END);

    let kernel_end = unsafe { &__kernel_end as *const u8 as usize };
    let base = page_align_up(kernel_end);

    // BITMAP lives inside the kernel BSS, below __kernel_end — no extra space needed.

    let num_pages = (ram_end - base) / PAGE_SIZE;

    unsafe {
        PAGE_BASE = base;
        NUM_PAGES = num_pages;
        RAM_TOP   = ram_end;
        let uart = crate::drivers::uart::Uart::new();
        uart.puts("[page] BASE=");
        uart.put_hex(base);
        uart.puts(" NUM=");
        uart.put_dec(num_pages);
        uart.puts("\r\n");

        // Mark all allocatable pages as free (bit = 1)
        let uart = crate::drivers::uart::Uart::new();
        uart.puts("[page] init loop start\r\n");
        for i in 0..num_pages {
            let byte = i / 8;
            let bit = i % 8;
            BITMAP[byte] |= 1 << bit;
        }
        uart.puts("[page] init loop done\r\n");
    }
}

/// Allocate contiguous physical pages.
pub fn alloc_pages(count: usize) -> Option<usize> {
    if count == 0 { return None; }
    unsafe {
        let mut consecutive = 0;
        let mut start_idx = 0;
        for i in 0..NUM_PAGES {
            let byte = i / 8;
            let bit = i % 8;
            if BITMAP[byte] & (1 << bit) != 0 {
                // Page is free
                if consecutive == 0 { start_idx = i; }
                consecutive += 1;
                if consecutive == count {
                    // Found contiguous block. Mark as used.
                    for k in 0..count {
                        let idx = start_idx + k;
                        let b = idx / 8;
                        let bi = idx % 8;
                        BITMAP[b] &= !(1 << bi);
                    }
                    let addr = PAGE_BASE + start_idx * PAGE_SIZE;
                    // Zero via high VA (kernel runs via TTBR1 after boot).
                    core::ptr::write_bytes(
                        crate::memory::vmm::phys_to_virt(addr) as *mut u8,
                        0,
                        count * PAGE_SIZE,
                    );
                    return Some(addr);
                }
            } else {
                consecutive = 0;
            }
        }
        None
    }
}

/// Allocate a single physical page. Returns the physical address or None.
pub fn alloc_page() -> Option<usize> {
    alloc_pages(1)
}

/// Free a previously allocated page.
pub fn free_page(addr: usize) {
    unsafe {
        if addr < PAGE_BASE || addr >= RAM_TOP {
            return;
        }
        if (addr & (PAGE_SIZE - 1)) != 0 {
            return; // Not page-aligned
        }
        let index = (addr - PAGE_BASE) / PAGE_SIZE;
        if index < NUM_PAGES {
            let byte = index / 8;
            let bit = index % 8;
            BITMAP[byte] |= 1 << bit;
        }
    }
}

/// Free a contiguous block of pages previously returned by `alloc_pages`.
pub fn release_pages(base: usize, count: usize) {
    for i in 0..count {
        free_page(base + i * PAGE_SIZE);
    }
}

/// Count of free pages.
pub fn free_pages() -> usize {
    let mut count = 0;
    unsafe {
        for i in 0..NUM_PAGES {
            let byte = i / 8;
            let bit = i % 8;
            if BITMAP[byte] & (1 << bit) != 0 {
                count += 1;
            }
        }
    }
    count
}

/// Count of used (allocated) pages.
pub fn used_pages() -> usize {
    unsafe { NUM_PAGES - free_pages() }
}

/// Total number of managed pages.
pub fn total_pages() -> usize {
    unsafe { NUM_PAGES }
}
