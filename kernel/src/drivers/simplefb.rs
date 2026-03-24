//! SimpleFB driver — real-hardware UEFI GOP framebuffer path.
//!
//! On real AArch64 hardware (and QEMU with UEFI), the UEFI bootloader queries
//! the Graphics Output Protocol for the linear framebuffer and writes the
//! parameters into a `BootInfo` struct at physical address 0x4007_F000 before
//! jumping to the kernel.
//!
//! This driver reads those parameters and provides the same pixel-write API as
//! the VirtIO GPU path, so the rest of the kernel (`framebuffer.rs`, the Aurora
//! compositor, ...) can use either backend transparently.

use crate::memory::vmm::KERNEL_VA_OFFSET;

// Physical address where the bootloader writes the BootInfo struct.
// Must be below KERNEL_LOAD_PA (0x4008_0000) so it is never overwritten.
const BOOT_INFO_PA: u64 = 0x4007_F000;

/// Magic value that identifies a valid BootInfo struct.
const BOOT_INFO_MAGIC: u32 = 0x00AE_6105; // "AEGLOS"

/// Pixel format constants (matching the bootloader).
pub const FMT_BGR: u32 = 1;
pub const FMT_RGB: u32 = 2;
pub const FMT_BITMASK: u32 = 3;

/// Information written by the UEFI bootloader before jumping to the kernel.
///
/// Layout is fixed; do NOT reorder fields.
#[repr(C)]
pub struct BootInfo {
    pub magic:     u32,   // 0x00AE_6105 when valid
    pub fb_base:   u64,   // Framebuffer physical address
    pub fb_width:  u32,   // Pixels per row
    pub fb_height: u32,   // Rows
    pub fb_stride: u32,   // Pixels per scanline (>= width)
    pub fb_format: u32,   // 1=BGR, 2=RGB, 3=Bitmask
}

// ── Module-level state ─────────────────────────────────────────────────────────

static mut FB_PA:     u64   = 0;
static mut FB_VA:     usize = 0;
static mut FB_WIDTH:  u32   = 0;
static mut FB_HEIGHT: u32   = 0;
static mut FB_STRIDE: u32   = 0;  // pixels per row (not bytes)
static mut FB_FORMAT: u32   = FMT_BGR;
static mut FB_READY:  bool  = false;

// ── Public API ─────────────────────────────────────────────────────────────────

/// Attempt to read the BootInfo struct placed by the UEFI bootloader.
///
/// Returns `true` if a valid struct was found and the driver was initialised.
pub fn probe() -> bool {
    // The MMU is active when this is called; the bootloader wrote to a physical
    // address below the kernel, so we access it through the TTBR1 kernel alias.
    let va = BOOT_INFO_PA as usize + KERNEL_VA_OFFSET;
    let info = unsafe { &*(va as *const BootInfo) };

    if info.magic != BOOT_INFO_MAGIC {
        return false;
    }
    if info.fb_base == 0 || info.fb_width == 0 || info.fb_height == 0 {
        return false;
    }

    unsafe {
        init(info.fb_base, info.fb_width, info.fb_height, info.fb_stride, info.fb_format);
    }
    true
}

/// Initialise the driver with explicit parameters (called from `probe()` or
/// directly when the framebuffer base is discovered via DTB).
///
/// # Safety
/// `pa` must point to a valid, writable linear framebuffer.
pub unsafe fn init(pa: u64, w: u32, h: u32, stride: u32, format: u32) {
    FB_PA     = pa;
    FB_WIDTH  = w;
    FB_HEIGHT = h;
    // stride=0 → same as width (packed scanlines)
    FB_STRIDE = if stride == 0 { w } else { stride };
    FB_FORMAT = format;
    // Map: VA = PA + kernel offset (identity-mapped in TTBR1 table)
    FB_VA     = pa as usize + KERNEL_VA_OFFSET;
    FB_READY  = true;
}

/// Returns `true` if the driver was successfully initialised.
#[inline]
pub fn ready() -> bool {
    unsafe { FB_READY }
}

/// Returns `(va, width, height, stride_pixels)`.
///
/// `va` is a kernel virtual address suitable for direct pixel writes from EL1.
#[inline]
pub fn get_info() -> (usize, u32, u32, u32) {
    unsafe { (FB_VA, FB_WIDTH, FB_HEIGHT, FB_STRIDE) }
}

/// Write a 32-bit pixel (XRGB / BGRX depending on `FB_FORMAT`) at (x, y).
///
/// Out-of-bounds coordinates are silently ignored.
#[inline]
pub fn put_pixel(x: u32, y: u32, color: u32) {
    unsafe {
        if x >= FB_WIDTH || y >= FB_HEIGHT || FB_VA == 0 {
            return;
        }
        let byte_offset = (y as usize * FB_STRIDE as usize + x as usize) * 4;
        core::ptr::write_volatile((FB_VA + byte_offset) as *mut u32, color);
    }
}

/// Fill a rectangle with a solid colour.
pub fn fill_rect(x: u32, y: u32, w: u32, h: u32, color: u32) {
    let x1 = x + w;
    let y1 = y + h;
    for row in y..y1.min(unsafe { FB_HEIGHT }) {
        for col in x..x1.min(unsafe { FB_WIDTH }) {
            put_pixel(col, row, color);
        }
    }
}
