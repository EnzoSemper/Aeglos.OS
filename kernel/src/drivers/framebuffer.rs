//! Framebuffer driver — VirtIO GPU (QEMU) or UEFI GOP SimpleFB (real hardware).
//!
//! At init time we try SimpleFB first (populated by the UEFI bootloader), then
//! fall back to the VirtIO GPU driver for QEMU sessions.
//!
//! The rest of the kernel (boot logo, Aurora compositor, ...) always calls
//! *this* module and never talks to virtio_gpu / simplefb directly.

use super::font;
use super::virtio_gpu;
use super::simplefb;

static mut WIDTH: usize = 0;
static mut HEIGHT: usize = 0;
static mut BPP: usize = 4; // XRGB8888: 4 bytes per pixel
static mut STRIDE: usize = 0; // Bytes per row

/// Which backend is active.
#[derive(Copy, Clone, PartialEq)]
enum Backend { None, SimpleFb, VirtioGpu }
static mut BACKEND: Backend = Backend::None;

/// Framebuffer state — initialized once at boot.
static mut FB_ADDR: usize = 0;

/// Pack an RGB color as XRGB8888 pixel value.
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

/// Initialise the framebuffer.
///
/// Tries UEFI GOP / SimpleFB first (populated by the bootloader); if not
/// present falls back to the VirtIO GPU driver (QEMU sessions).
///
/// Returns `true` if any framebuffer is available.
pub fn init() -> bool {
    // ── Path 1: UEFI GOP / SimpleFB (real hardware + UEFI QEMU) ──────────────
    if simplefb::probe() {
        let (va, w, h, stride_px) = simplefb::get_info();
        unsafe {
            WIDTH   = w as usize;
            HEIGHT  = h as usize;
            // stride is in *pixels*; multiply by BPP for byte stride
            STRIDE  = stride_px as usize * BPP;
            FB_ADDR = va;
            BACKEND = Backend::SimpleFb;
        }
        fill_rect(0, 0, unsafe { WIDTH }, unsafe { HEIGHT }, rgb(16, 16, 24));
        // No explicit flush needed — SimpleFB is a linear framebuffer;
        // writes are visible immediately.
        return true;
    }

    // ── Path 2: VirtIO GPU (QEMU with virtio-gpu-device) ─────────────────────
    let (fb_ptr, w, h, pitch) = virtio_gpu::get_framebuffer();
    if fb_ptr.is_null() {
        return false;
    }

    unsafe {
        WIDTH   = w as usize;
        HEIGHT  = h as usize;
        STRIDE  = pitch as usize;
        FB_ADDR = fb_ptr as usize;
        BACKEND = Backend::VirtioGpu;
    }

    fill_rect(0, 0, unsafe { WIDTH }, unsafe { HEIGHT }, rgb(16, 16, 24));
    flush();
    true
}

/// Flush the framebuffer to screen.
///
/// For SimpleFB this is a no-op (writes are already visible).
/// For VirtIO GPU we issue a `RESOURCE_FLUSH` command to the host.
pub fn flush() {
    unsafe {
        if BACKEND == Backend::VirtioGpu {
            virtio_gpu::flush();
        }
        // SimpleFB: linear framebuffer — nothing to flush.
    }
}

/// Set a single pixel.
#[inline]
pub fn put_pixel(x: usize, y: usize, color: u32) {
    unsafe {
        if x >= WIDTH || y >= HEIGHT {
            return;
        }
        let offset = y * STRIDE + x * BPP;
        core::ptr::write_volatile((FB_ADDR + offset) as *mut u32, color);
    }
}

/// Fill a rectangle with a solid color.
pub fn fill_rect(x: usize, y: usize, w: usize, h: usize, color: u32) {
    unsafe {
        for row in y..(y + h).min(HEIGHT) {
            for col in x..(x + w).min(WIDTH) {
                let offset = row * STRIDE + col * BPP;
                core::ptr::write_volatile((FB_ADDR + offset) as *mut u32, color);
            }
        }
    }
}

/// Draw a single character at pixel position (x, y) with foreground color.
pub fn draw_char(x: usize, y: usize, ch: u8, fg: u32) {
    let glyph = font::glyph(ch);
    for row in 0..font::GLYPH_HEIGHT {
        let bits = glyph[row];
        for col in 0..font::GLYPH_WIDTH {
            if bits & (0x80 >> col) != 0 {
                put_pixel(x + col, y + row, fg);
            }
        }
    }
}

/// Draw a character with both foreground and background colors.
pub fn draw_char_bg(x: usize, y: usize, ch: u8, fg: u32, bg: u32) {
    let glyph = font::glyph(ch);
    for row in 0..font::GLYPH_HEIGHT {
        let bits = glyph[row];
        for col in 0..font::GLYPH_WIDTH {
            let color = if bits & (0x80 >> col) != 0 { fg } else { bg };
            put_pixel(x + col, y + row, color);
        }
    }
}

/// Draw a string at pixel position (x, y).
pub fn draw_string(x: usize, y: usize, s: &str, fg: u32) {
    let mut cx = x;
    for byte in s.bytes() {
        if byte == b'\n' {
            continue; // Basic newline handling would need tracking cy
        }
        draw_char(cx, y, byte, fg);
        cx += font::GLYPH_WIDTH;
    }
}

/// Draw a string at a given scale (each pixel becomes scale x scale).
pub fn draw_string_scaled(x: usize, y: usize, s: &str, fg: u32, scale: usize) {
    let mut cx = x;
    for byte in s.bytes() {
        draw_char_scaled(cx, y, byte, fg, scale);
        cx += font::GLYPH_WIDTH * scale;
    }
}

/// Draw a single character at the given scale factor.
fn draw_char_scaled(x: usize, y: usize, ch: u8, fg: u32, scale: usize) {
    let glyph = font::glyph(ch);
    for row in 0..font::GLYPH_HEIGHT {
        let bits = glyph[row];
        for col in 0..font::GLYPH_WIDTH {
            if bits & (0x80 >> col) != 0 {
                fill_rect(x + col * scale, y + row * scale, scale, scale, fg);
            }
        }
    }
}

/// Get framebuffer dimensions.
pub fn dimensions() -> (usize, usize) {
    unsafe { (WIDTH, HEIGHT) }
}
