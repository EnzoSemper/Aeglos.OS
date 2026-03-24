/// AArch64 Cache Maintenance Operations
///
/// Provides functions to clean and invalidate data cache lines.

use core::arch::asm;

/// Clean and Invalidate Data Cache by Virtual Address to Point of Coherence (CIVAC).
///
/// Ensures that writes in the cache are visible to main memory (for DMA),
/// and that reads from the cache will fetch fresh data from main memory.
pub unsafe fn clean_inval_dcache_range(start: usize, end: usize) {
    let ctr: u64;
    asm!("mrs {}, ctr_el0", out(reg) ctr);
    
    // DminLine is bits [19:16], value is Log2(words). Words = 4 bytes.
    // So value 4 means 2^4 words = 16 words = 64 bytes.
    let dminline = (ctr >> 16) & 0xF;
    let line_size = 4 << dminline;

    let mut addr = start & !(line_size - 1);
    while addr < end {
        asm!("dc civac, {}", in(reg) addr);
        addr += line_size;
    }
    asm!("dsb sy");
}
