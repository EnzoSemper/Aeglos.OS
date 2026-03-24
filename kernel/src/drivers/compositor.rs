//! Aurora Compositor — IPC-based surface manager.
//!
//! Each EL0 process creates one or more surfaces via SYS_SURF_CREATE.
//! The compositor maps the surface pixel buffer into the process's address
//! space so the process can draw directly (no IPC per pixel).
//! When the process calls SYS_SURF_FLUSH, the compositor composites all
//! dirty surfaces onto the physical framebuffer in z-order.
//!
//! Phase-6.1 improvements:
//!   • Dirty-flag tracking — composite() is a no-op when nothing changed.
//!   • Pre-clipped blit_surface — per-pixel x/y bounds checks eliminated.
//!   • resize(id, w, h) — reallocates the pixel buffer in-place.
//!   • raise(id) / lower(id) — z-order control at runtime.
//!   • mark_dirty(id) / dirty_all() — explicit damage notification.

use crate::memory::{alloc_pages, release_pages};
use crate::memory::vmm::phys_to_virt;
use crate::drivers::framebuffer;

pub const MAX_SURFACES: usize = 16;
const PIXEL_BYTES: usize = 4; // XRGB8888

#[derive(Clone)]
pub struct Surface {
    pub used:      bool,
    pub owner:     usize,    // TID of owning process (0 = kernel)
    pub x:         i32,
    pub y:         i32,
    pub w:         u32,
    pub h:         u32,
    pub z:         u8,       // z-order: 0=bottom, 255=top
    pub buf_pa:    usize,    // physical address of pixel buffer
    pub buf_pages: usize,    // number of pages allocated
    pub dirty:     bool,     // surface has been modified since last composite
}

impl Surface {
    const fn empty() -> Self {
        Surface {
            used: false, owner: 0, x: 0, y: 0, w: 0, h: 0,
            z: 0, buf_pa: 0, buf_pages: 0, dirty: false,
        }
    }
}

static mut SURFACES:    [Surface; MAX_SURFACES] = {
    const E: Surface = Surface::empty();
    [E; MAX_SURFACES]
};
/// Set when any surface is dirty; composite() clears it on flush.
static mut ANY_DIRTY: bool = false;

/// Create a surface. Returns surface_id (index) or usize::MAX on error.
pub fn create(owner_tid: usize, w: u32, h: u32, z: u8) -> usize {
    unsafe {
        let slot = match SURFACES.iter().position(|s| !s.used) {
            Some(i) => i,
            None => return usize::MAX,
        };
        let pixel_count = (w as usize) * (h as usize);
        let byte_count  = pixel_count * PIXEL_BYTES;
        let pages = (byte_count + 4095) / 4096;
        let pa = match alloc_pages(pages) {
            Some(p) => p,
            None => return usize::MAX,
        };
        let va = phys_to_virt(pa) as *mut u8;
        core::ptr::write_bytes(va, 0, pages * 4096);

        SURFACES[slot] = Surface {
            used: true, owner: owner_tid, x: 0, y: 0,
            w, h, z, buf_pa: pa, buf_pages: pages, dirty: true,
        };
        ANY_DIRTY = true;
        slot
    }
}

/// Destroy a surface and free its buffer.
pub fn destroy(id: usize) {
    unsafe {
        if id >= MAX_SURFACES || !SURFACES[id].used { return; }
        let pages = SURFACES[id].buf_pages;
        let pa    = SURFACES[id].buf_pa;
        release_pages(pa, pages);
        SURFACES[id] = Surface::empty();
        ANY_DIRTY = true; // need to redraw without this surface
    }
}

/// Return the physical address of the surface pixel buffer.
pub fn buf_pa(id: usize) -> usize {
    unsafe {
        if id >= MAX_SURFACES || !SURFACES[id].used { return 0; }
        SURFACES[id].buf_pa
    }
}

/// Move a surface to screen position (x, y).
pub fn set_pos(id: usize, x: i32, y: i32) {
    unsafe {
        if id >= MAX_SURFACES || !SURFACES[id].used { return; }
        SURFACES[id].x = x;
        SURFACES[id].y = y;
        SURFACES[id].dirty = true;
        ANY_DIRTY = true;
    }
}

/// Mark a surface (or a sub-region) as dirty so composite() will re-blit it.
/// Call this after drawing into the surface buffer.
pub fn mark_dirty(id: usize) {
    unsafe {
        if id >= MAX_SURFACES || !SURFACES[id].used { return; }
        SURFACES[id].dirty = true;
        ANY_DIRTY = true;
    }
}

/// Force all surfaces dirty (e.g. after a z-order change).
fn dirty_all() {
    unsafe {
        for s in SURFACES.iter_mut() {
            if s.used { s.dirty = true; }
        }
        ANY_DIRTY = true;
    }
}

/// Resize a surface's pixel buffer.  Returns true on success.
/// The new buffer is zeroed; existing pixel content is discarded.
pub fn resize(id: usize, new_w: u32, new_h: u32) -> bool {
    unsafe {
        if id >= MAX_SURFACES || !SURFACES[id].used { return false; }
        let old_pages = SURFACES[id].buf_pages;
        let old_pa    = SURFACES[id].buf_pa;

        let new_pixels = (new_w as usize) * (new_h as usize);
        let new_bytes  = new_pixels * PIXEL_BYTES;
        let new_pages  = (new_bytes + 4095) / 4096;

        let new_pa = match alloc_pages(new_pages) {
            Some(p) => p,
            None => return false,
        };
        let va = phys_to_virt(new_pa) as *mut u8;
        core::ptr::write_bytes(va, 0, new_pages * 4096);

        release_pages(old_pa, old_pages);
        SURFACES[id].buf_pa    = new_pa;
        SURFACES[id].buf_pages = new_pages;
        SURFACES[id].w         = new_w;
        SURFACES[id].h         = new_h;
        SURFACES[id].dirty     = true;
        ANY_DIRTY = true;
        true
    }
}

/// Increase the z-order of a surface by 1, clamped to 254.
/// Triggers a full composite refresh.
pub fn raise(id: usize) {
    unsafe {
        if id >= MAX_SURFACES || !SURFACES[id].used { return; }
        if SURFACES[id].z < 254 { SURFACES[id].z += 1; }
        dirty_all();
    }
}

/// Decrease the z-order of a surface by 1, clamped to 0.
/// Triggers a full composite refresh.
pub fn lower(id: usize) {
    unsafe {
        if id >= MAX_SURFACES || !SURFACES[id].used { return; }
        if SURFACES[id].z > 0 { SURFACES[id].z -= 1; }
        dirty_all();
    }
}

/// Composite all surfaces onto the physical framebuffer in z-order.
///
/// Early-returns if no surface has been marked dirty since the last call.
/// Called after any SYS_SURF_FLUSH to update the display.
pub fn composite() {
    unsafe {
        if !ANY_DIRTY { return; }

        // Build sorted list of active surfaces (ascending z).
        let mut order: [usize; MAX_SURFACES] = [usize::MAX; MAX_SURFACES];
        let mut count = 0usize;
        for (i, s) in SURFACES.iter().enumerate() {
            if s.used { order[count] = i; count += 1; }
        }
        // Insertion sort by z (surfaces are few, ≤16)
        for i in 1..count {
            let mut j = i;
            while j > 0 && SURFACES[order[j]].z < SURFACES[order[j - 1]].z {
                order.swap(j, j - 1);
                j -= 1;
            }
        }
        for k in 0..count {
            let i = order[k];
            blit_surface(&SURFACES[i]);
            SURFACES[i].dirty = false;
        }
        ANY_DIRTY = false;
    }
    framebuffer::flush();
}

/// Blit one surface onto the framebuffer with pre-clipped bounds.
///
/// Pre-computing clip ranges means the inner loop body contains zero
/// conditional branches (only the alpha skip, which CPUs predict well).
fn blit_surface(s: &Surface) {
    let (fb_w, fb_h) = framebuffer::dimensions();
    let src_va = unsafe { phys_to_virt(s.buf_pa) as *const u32 };

    // Compute source row/col range that actually falls within the framebuffer.
    let row_start: usize = if s.y < 0 { (-s.y) as usize } else { 0 };
    let row_end:   usize = {
        let limit = (fb_h as i32).saturating_sub(s.y) as usize;
        limit.min(s.h as usize)
    };
    let col_start: usize = if s.x < 0 { (-s.x) as usize } else { 0 };
    let col_end:   usize = {
        let limit = (fb_w as i32).saturating_sub(s.x) as usize;
        limit.min(s.w as usize)
    };

    if row_start >= row_end || col_start >= col_end { return; }

    let stride = s.w as usize;
    for row in row_start..row_end {
        let dst_y = (s.y + row as i32) as usize;
        let src_row = row * stride;
        for col in col_start..col_end {
            let pixel = unsafe {
                core::ptr::read_volatile(src_va.add(src_row + col))
            };
            if pixel & 0xFF00_0000 == 0 { continue; }
            framebuffer::put_pixel((s.x + col as i32) as usize, dst_y, pixel);
        }
    }
}
