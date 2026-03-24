//! WASM linear memory — a contiguous, bounds-checked byte array.
//!
//! One WASM page = 64 KiB.  Backed by physical pages from the kernel
//! allocator.  The full allocation (`min_pages` up to a ceiling) is done
//! up-front so that `memory.grow` never needs to re-allocate.

use crate::memory;

/// WASM page size: 64 KiB.
pub const WASM_PAGE_SIZE: usize = 65536;

/// Hard ceiling on WASM linear memory (4 MiB = 64 pages).
pub const WASM_MAX_PAGES: usize = 64;

/// Kernel pages per WASM page (64 KiB / 4 KiB = 16).
const KERNEL_PAGES_PER_WASM_PAGE: usize = WASM_PAGE_SIZE / memory::PAGE_SIZE;

pub struct LinearMemory {
    /// Physical base address of the allocated region.
    pub base: usize,
    /// Current size in WASM pages (may grow up to `cap_pages`).
    pub pages: usize,
    /// Total allocated WASM pages (pre-allocated ceiling).
    cap_pages: usize,
}

impl LinearMemory {
    /// Allocate `min_pages` WASM pages (capped at `WASM_MAX_PAGES`).
    ///
    /// We always pre-allocate the full `min_pages` region at once so
    /// `grow()` just adjusts the visible size without touching the allocator.
    pub fn new(min_pages: usize) -> Option<Self> {
        let cap = min_pages.max(1).min(WASM_MAX_PAGES);
        let kernel_page_count = cap * KERNEL_PAGES_PER_WASM_PAGE;
        let base = memory::alloc_pages(kernel_page_count)?;
        // alloc_pages already zeros; just make sure.
        unsafe { core::ptr::write_bytes(base as *mut u8, 0, cap * WASM_PAGE_SIZE); }
        Some(LinearMemory { base, pages: min_pages.min(cap), cap_pages: cap })
    }

    /// Current size in WASM pages.
    #[inline] pub fn size(&self) -> u32 { self.pages as u32 }

    /// Grow by `delta` WASM pages.  Returns old size, or -1 on failure.
    pub fn grow(&mut self, delta: u32) -> i32 {
        let old = self.pages;
        let new = old + delta as usize;
        if new > self.cap_pages { return -1; }
        self.pages = new;
        old as i32
    }

    // ── Bounds-checked accessors ─────────────────────────────────────────────

    #[inline]
    fn check(&self, addr: u32, size: usize) -> bool {
        (addr as usize).saturating_add(size) <= self.pages * WASM_PAGE_SIZE
    }

    pub fn load_u8(&self, addr: u32) -> Option<u8> {
        if !self.check(addr, 1) { return None; }
        unsafe { Some(*(self.base as *const u8).add(addr as usize)) }
    }
    pub fn load_u16(&self, addr: u32) -> Option<u16> {
        if !self.check(addr, 2) { return None; }
        unsafe {
            let p = (self.base + addr as usize) as *const u8;
            Some(u16::from_le_bytes([*p, *p.add(1)]))
        }
    }
    pub fn load_u32(&self, addr: u32) -> Option<u32> {
        if !self.check(addr, 4) { return None; }
        unsafe {
            let p = (self.base + addr as usize) as *const u8;
            Some(u32::from_le_bytes([*p, *p.add(1), *p.add(2), *p.add(3)]))
        }
    }
    pub fn load_u64(&self, addr: u32) -> Option<u64> {
        if !self.check(addr, 8) { return None; }
        unsafe {
            let p = (self.base + addr as usize) as *const u8;
            Some(u64::from_le_bytes([
                *p, *p.add(1), *p.add(2), *p.add(3),
                *p.add(4), *p.add(5), *p.add(6), *p.add(7),
            ]))
        }
    }

    pub fn store_u8(&mut self, addr: u32, v: u8) -> bool {
        if !self.check(addr, 1) { return false; }
        unsafe { *(self.base as *mut u8).add(addr as usize) = v; }
        true
    }
    pub fn store_u16(&mut self, addr: u32, v: u16) -> bool {
        if !self.check(addr, 2) { return false; }
        unsafe {
            let p = (self.base + addr as usize) as *mut u8;
            let b = v.to_le_bytes();
            *p = b[0]; *p.add(1) = b[1];
        }
        true
    }
    pub fn store_u32(&mut self, addr: u32, v: u32) -> bool {
        if !self.check(addr, 4) { return false; }
        unsafe {
            let p = (self.base + addr as usize) as *mut u8;
            let b = v.to_le_bytes();
            *p = b[0]; *p.add(1) = b[1]; *p.add(2) = b[2]; *p.add(3) = b[3];
        }
        true
    }
    pub fn store_u64(&mut self, addr: u32, v: u64) -> bool {
        if !self.check(addr, 8) { return false; }
        unsafe {
            let p = (self.base + addr as usize) as *mut u8;
            let b = v.to_le_bytes();
            for i in 0..8 { *p.add(i) = b[i]; }
        }
        true
    }

    /// Copy a slice of bytes from linear memory (for host imports like `aeglos_log`).
    pub fn read_bytes(&self, addr: u32, len: u32) -> Option<&[u8]> {
        if !self.check(addr, len as usize) { return None; }
        unsafe {
            Some(core::slice::from_raw_parts(
                (self.base + addr as usize) as *const u8,
                len as usize,
            ))
        }
    }

    /// Write a slice of bytes into linear memory.
    pub fn write_bytes(&mut self, addr: u32, data: &[u8]) -> bool {
        if !self.check(addr, data.len()) { return false; }
        unsafe {
            let dst = (self.base + addr as usize) as *mut u8;
            core::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
        }
        true
    }
}
