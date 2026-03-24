use crate::fonts;
use crate::icons;

pub const WIDTH: usize = 1280;
pub const HEIGHT: usize = 720;
static mut FB: *mut u32 = core::ptr::null_mut();
static mut PITCH: usize = 0;
static mut BLUR_BUF: [u32; WIDTH * HEIGHT] = [0u32; WIDTH * HEIGHT];

pub fn init(fb_ptr: *mut u32, pitch: u32) {
    unsafe {
        FB = fb_ptr;
        PITCH = pitch as usize / 4; // words per row
    }
}

pub fn rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
}

pub fn put_pixel(x: usize, y: usize, color: u32) {
    if x >= WIDTH || y >= HEIGHT { return; }
    unsafe {
        if !FB.is_null() {
            // B8G8R8A8_UNORM: alpha in high byte must be 0xFF (opaque)
            FB.add(y * PITCH + x).write_volatile(color | 0xFF000000);
        }
    }
}

pub fn get_pixel(x: usize, y: usize) -> u32 {
    if x >= WIDTH || y >= HEIGHT { return 0; }
    unsafe {
        if !FB.is_null() {
            FB.add(y * PITCH + x).read_volatile()
        } else {
            0
        }
    }
}

pub fn blend(bg: u32, fg: u32, alpha: u8) -> u32 {
    if alpha == 0 { return bg; }
    if alpha == 255 { return fg; }
    let a = alpha as u32;
    let inv_a = 255 - a;

    let br = (bg >> 16) & 0xFF;
    let bg_g = (bg >> 8) & 0xFF;
    let bb = bg & 0xFF;

    let fr = (fg >> 16) & 0xFF;
    let fg_g = (fg >> 8) & 0xFF;
    let fb = fg & 0xFF;

    let r = ((fr * a) + (br * inv_a)) / 255;
    let g = ((fg_g * a) + (bg_g * inv_a)) / 255;
    let b = ((fb * a) + (bb * inv_a)) / 255;

    rgb(r as u8, g as u8, b as u8)
}

pub fn fill_rect(x: usize, y: usize, w: usize, h: usize, color: u32) {
    let right = (x + w).min(WIDTH);
    let bottom = (y + h).min(HEIGHT);
    for row in y..bottom {
        for col in x..right {
            put_pixel(col, row, color);
        }
    }
}

pub fn draw_rect(x: usize, y: usize, w: usize, h: usize, color: u32, thickness: usize) {
    fill_rect(x, y, w, thickness, color);
    fill_rect(x, y + h - thickness, w, thickness, color);
    fill_rect(x, y, thickness, h, color);
    fill_rect(x + w - thickness, y, thickness, h, color);
}

pub fn draw_char_share_tech(x: usize, y: usize, ch: u8, fg: u32) -> usize {
    if ch < 32 || ch > 126 { return 0; }
    let idx = (ch - 32) as usize;
    let glyph = &fonts::SHARE_TECH_MONO_16_GLYPHS[idx];
    
    let mut bmp_idx = glyph.bmp_idx;
    for row in 0..glyph.h {
        for col in 0..glyph.w {
            let alpha = fonts::SHARE_TECH_MONO_16_BITMAP[bmp_idx];
            bmp_idx += 1;
            if alpha > 0 {
                let px = (x as isize + col as isize + glyph.ox as isize) as usize;
                let py = (y as isize + row as isize + glyph.oy as isize) as usize;
                if px < WIDTH && py < HEIGHT {
                    if alpha == 255 {
                        put_pixel(px, py, fg);
                    } else {
                        let bg = get_pixel(px, py);
                        put_pixel(px, py, blend(bg, fg, alpha));
                    }
                }
            }
        }
    }
    glyph.adv as usize
}

pub fn draw_string_share_tech(x: usize, y: usize, s: &str, fg: u32) {
    let mut cx = x;
    for byte in s.bytes() {
        cx += draw_char_share_tech(cx, y, byte, fg);
    }
}

pub fn measure_string_share_tech(s: &str) -> usize {
    let mut w = 0;
    for ch in s.bytes() {
        if ch >= 32 && ch <= 126 {
            let idx = (ch - 32) as usize;
            w += fonts::SHARE_TECH_MONO_16_GLYPHS[idx].adv as usize;
        }
    }
    w
}

pub fn draw_char_chakra(x: usize, y: usize, ch: u8, fg: u32) -> usize {
    if ch < 32 || ch > 126 { return 0; }
    let idx = (ch - 32) as usize;
    let glyph = &fonts::CHAKRA_PETCH_16_GLYPHS[idx];
    
    let mut bmp_idx = glyph.bmp_idx;
    for row in 0..glyph.h {
        for col in 0..glyph.w {
            let alpha = fonts::CHAKRA_PETCH_16_BITMAP[bmp_idx];
            bmp_idx += 1;
            if alpha > 0 {
                let px = (x as isize + col as isize + glyph.ox as isize) as usize;
                let py = (y as isize + row as isize + glyph.oy as isize) as usize;
                if px < WIDTH && py < HEIGHT {
                    if alpha == 255 {
                        put_pixel(px, py, fg);
                    } else {
                        let bg = get_pixel(px, py);
                        put_pixel(px, py, blend(bg, fg, alpha));
                    }
                }
            }
        }
    }
    glyph.adv as usize
}

pub fn draw_string_chakra(x: usize, y: usize, s: &str, fg: u32) {
    let mut cx = x;
    for byte in s.bytes() {
        cx += draw_char_chakra(cx, y, byte, fg);
    }
}

/// Draw a string, stopping before any character would start at or past `max_x`.
/// Characters that would exceed `max_x` are silently dropped (hard clip, no ellipsis).
pub fn draw_string_clipped(x: usize, y: usize, s: &str, fg: u32, max_x: usize) {
    let mut cx = x;
    for byte in s.bytes() {
        if byte < 32 || byte > 126 { continue; }
        let adv = fonts::SHARE_TECH_MONO_16_GLYPHS[(byte - 32) as usize].adv as usize;
        if cx + adv > max_x { break; }
        cx += draw_char_share_tech(cx, y, byte, fg);
    }
}

/// Glyph advance for a single byte (0 if non-printable).
#[inline(always)]
fn glyph_adv(b: u8) -> usize {
    if b < 32 || b > 126 { 0 }
    else { fonts::SHARE_TECH_MONO_16_GLYPHS[(b - 32) as usize].adv as usize }
}

/// Measure the total pixel height a string would occupy when word-wrapped.
/// `x_start` is the cursor x for the first word; subsequent lines start at `left_x`.
/// Returns height in pixels (always ≥ `line_h`).
pub fn measure_wrapped_height(x_start: usize, s: &str, left_x: usize, max_x: usize, line_h: usize) -> usize {
    if max_x <= left_x { return line_h; }
    let avail = max_x - left_x;
    let mut cx = x_start;
    let mut lines: usize = 1;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Collect one word (or one non-space chunk)
        let ws = i;
        while i < bytes.len() && bytes[i] != b' ' && bytes[i] != b'\n' { i += 1; }
        // Measure word width
        let mut ww = 0usize;
        for j in ws..i { ww += glyph_adv(bytes[j]); }
        // Wrap before word if it doesn't fit (and we're not at line start)
        if cx > left_x && ww > 0 && cx + ww > max_x { lines += 1; cx = left_x; }
        // Step through word chars (handles words wider than max_x)
        for j in ws..i {
            let a = glyph_adv(bytes[j]);
            if cx + a > max_x { lines += 1; cx = left_x; }
            cx += a;
        }
        // Handle delimiter
        if i < bytes.len() {
            if bytes[i] == b'\n' { lines += 1; cx = left_x; i += 1; }
            else { // space
                let sp = glyph_adv(b' ');
                if cx + sp > max_x { lines += 1; cx = left_x; } else { cx += sp; }
                i += 1;
            }
        }
        // If the whole word was wider than one line, the loop above already wrapped
        let _ = avail;
    }
    lines * line_h
}

/// Draw a string with word-wrapping between `left_x` and `max_x`, starting at `(x, y)`.
/// Stops drawing once `y >= max_y`. Returns the y coordinate of the line *after* the last.
pub fn draw_string_wrapped(x: usize, y: usize, s: &str, fg: u32,
                            left_x: usize, max_x: usize, line_h: usize, max_y: usize) -> usize {
    let mut cx = x;
    let mut cy = y;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if cy >= max_y { break; }
        let ws = i;
        while i < bytes.len() && bytes[i] != b' ' && bytes[i] != b'\n' { i += 1; }
        // Measure word
        let mut ww = 0usize;
        for j in ws..i { ww += glyph_adv(bytes[j]); }
        // Wrap before word?
        if cx > left_x && ww > 0 && cx + ww > max_x {
            cy += line_h; cx = left_x;
            if cy >= max_y { return cy; }
        }
        // Draw word chars
        for j in ws..i {
            let b = bytes[j];
            let a = glyph_adv(b);
            if a == 0 { continue; }
            if cx + a > max_x { cy += line_h; cx = left_x; if cy >= max_y { return cy; } }
            cx += draw_char_share_tech(cx, cy, b, fg);
        }
        // Delimiter
        if i < bytes.len() {
            if bytes[i] == b'\n' {
                cy += line_h; cx = left_x; i += 1;
            } else {
                let sp = glyph_adv(b' ');
                if cx + sp >= max_x { cy += line_h; cx = left_x; }
                else { cx += draw_char_share_tech(cx, cy, b' ', fg); }
                i += 1;
            }
        }
    }
    cy + line_h
}

// ── Advanced rendering primitives ─────────────────────────────────────────────

/// Returns true if pixel (px,py) is inside a rounded rectangle.
#[inline]
fn in_rrect(px: usize, py: usize, x: usize, y: usize, w: usize, h: usize, r: usize) -> bool {
    if px < x || py < y || px >= x + w || py >= y + h { return false; }
    let r = r.min(w / 2).min(h / 2);
    let right  = x + w;
    let bottom = y + h;
    let r2 = (r * r) as i64;
    let check = |cx: usize, cy: usize| -> bool {
        let dx = px as i64 - cx as i64;
        let dy = py as i64 - cy as i64;
        dx * dx + dy * dy <= r2
    };
    if px < x + r && py < y + r        { return check(x + r,     y + r);      }
    if px >= right - r && py < y + r   { return check(right - r,  y + r);      }
    if px < x + r && py >= bottom - r  { return check(x + r,     bottom - r);  }
    if px >= right - r && py >= bottom - r { return check(right - r, bottom - r); }
    true
}

/// Fill a rounded rectangle.
pub fn fill_rounded_rect(x: usize, y: usize, w: usize, h: usize, r: usize, color: u32) {
    let right  = (x + w).min(WIDTH);
    let bottom = (y + h).min(HEIGHT);
    for row in y..bottom {
        for col in x..right {
            if in_rrect(col, row, x, y, w, h, r) {
                put_pixel(col, row, color);
            }
        }
    }
}

/// Fill a rounded rectangle with alpha blending.
pub fn fill_rounded_rect_alpha(x: usize, y: usize, w: usize, h: usize, r: usize, color: u32, alpha: u8) {
    if alpha == 0 { return; }
    if alpha == 255 { fill_rounded_rect(x, y, w, h, r, color); return; }
    let right  = (x + w).min(WIDTH);
    let bottom = (y + h).min(HEIGHT);
    for row in y..bottom {
        for col in x..right {
            if in_rrect(col, row, x, y, w, h, r) {
                let bg = get_pixel(col, row);
                put_pixel(col, row, blend(bg, color, alpha));
            }
        }
    }
}

/// Draw the border of a rounded rectangle (stroke only, not fill).
pub fn stroke_rounded_rect(x: usize, y: usize, w: usize, h: usize, r: usize, color: u32, t: usize) {
    if w == 0 || h == 0 { return; }
    let right  = (x + w).min(WIDTH);
    let bottom = (y + h).min(HEIGHT);
    let ri = r.saturating_sub(t);
    for row in y..bottom {
        for col in x..right {
            let outer = in_rrect(col, row, x, y, w, h, r);
            let inner = w > t * 2 && h > t * 2
                && in_rrect(col, row, x + t, y + t, w - t * 2, h - t * 2, ri);
            if outer && !inner {
                put_pixel(col, row, color);
            }
        }
    }
}

/// Fill a rectangle with an alpha-blended color (for overlays/glassmorphism).
pub fn fill_rect_alpha(x: usize, y: usize, w: usize, h: usize, color: u32, alpha: u8) {
    if alpha == 0 { return; }
    if alpha == 255 { fill_rect(x, y, w, h, color); return; }
    let right  = (x + w).min(WIDTH);
    let bottom = (y + h).min(HEIGHT);
    for row in y..bottom {
        for col in x..right {
            let bg = get_pixel(col, row);
            put_pixel(col, row, blend(bg, color, alpha));
        }
    }
}

/// Vertical linear gradient fill.
pub fn fill_gradient_v(x: usize, y: usize, w: usize, h: usize, top: u32, bot: u32) {
    let right = (x + w).min(WIDTH);
    let bot_y = (y + h).min(HEIGHT);
    for row in y..bot_y {
        let t = if h > 1 { ((row - y) * 255 / (h - 1)) as u8 } else { 0 };
        let color = blend(top, bot, t);
        for col in x..right {
            put_pixel(col, row, color);
        }
    }
}

/// Horizontal linear gradient fill.
pub fn fill_gradient_h(x: usize, y: usize, w: usize, h: usize, left: u32, right_c: u32) {
    let right  = (x + w).min(WIDTH);
    let bottom = (y + h).min(HEIGHT);
    for row in y..bottom {
        for col in x..right {
            let t = if w > 1 { ((col - x) * 255 / (w - 1)) as u8 } else { 0 };
            put_pixel(col, row, blend(left, right_c, t));
        }
    }
}

/// Drop shadow — layered semi-transparent rectangles offset down-right.
pub fn draw_shadow(x: usize, y: usize, w: usize, h: usize, offset: usize, layers: usize) {
    for i in 0..layers {
        let alpha = (100u32 * (layers - i) as u32 / layers as u32) as u8;
        let ox = x.saturating_add(offset).saturating_add(i);
        let oy = y.saturating_add(offset).saturating_add(i);
        let ow = w.saturating_sub(i * 2);
        let oh = h.saturating_sub(i);
        fill_rect_alpha(ox, oy, ow, oh, 0x000000, alpha);
    }
}

/// Inward glow border — draws a colored halo around the inside edge of a rect.
pub fn draw_inner_glow(x: usize, y: usize, w: usize, h: usize, color: u32, spread: usize) {
    for i in 0..spread {
        let alpha = (180u32 * (spread - i) as u32 / spread as u32) as u8;
        let ox = x + i;
        let oy = y + i;
        let ow = w.saturating_sub(i * 2);
        let oh = h.saturating_sub(i * 2);
        if ow == 0 || oh == 0 { break; }
        let right  = (ox + ow).min(WIDTH);
        let bottom = (oy + oh).min(HEIGHT);
        // Top/bottom rows
        for col in ox..right {
            if oy < HEIGHT {
                let bg = get_pixel(col, oy); put_pixel(col, oy, blend(bg, color, alpha));
            }
            if bottom > 0 && bottom - 1 < HEIGHT {
                let bg = get_pixel(col, bottom - 1); put_pixel(col, bottom - 1, blend(bg, color, alpha));
            }
        }
        // Left/right columns (skip corners already done)
        for row in oy + 1..bottom.saturating_sub(1) {
            if ox < WIDTH {
                let bg = get_pixel(ox, row); put_pixel(ox, row, blend(bg, color, alpha));
            }
            if right > 0 && right - 1 < WIDTH {
                let bg = get_pixel(right - 1, row); put_pixel(right - 1, row, blend(bg, color, alpha));
            }
        }
    }
}

/// Filled circle.
pub fn fill_circle(cx: usize, cy: usize, r: usize, color: u32) {
    let r2 = (r * r) as i64;
    let ri = r as i64;
    for dy in -ri..=ri {
        for dx in -ri..=ri {
            if dx * dx + dy * dy <= r2 {
                let px = cx as i64 + dx;
                let py = cy as i64 + dy;
                if px >= 0 && py >= 0 && (px as usize) < WIDTH && (py as usize) < HEIGHT {
                    put_pixel(px as usize, py as usize, color);
                }
            }
        }
    }
}

/// Horizontal separator with fade-out at edges.
pub fn draw_separator_h(x: usize, y: usize, w: usize, color: u32) {
    let fade = (w / 8).max(1);
    for i in 0..w {
        let col = x + i;
        if col >= WIDTH || y >= HEIGHT { continue; }
        let alpha: u8 = if i < fade       { (i * 255 / fade) as u8 }
                        else if i >= w - fade { ((w - 1 - i) * 255 / fade) as u8 }
                        else               { 255 };
        if alpha == 255 { put_pixel(col, y, color); }
        else { let bg = get_pixel(col, y); put_pixel(col, y, blend(bg, color, alpha)); }
    }
}

/// Progress bar (pct: 0–100).
pub fn draw_progress(x: usize, y: usize, w: usize, h: usize, pct: u8, fill: u32, bg: u32) {
    fill_rect(x, y, w, h, bg);
    let fill_w = (w as u32 * pct as u32 / 100) as usize;
    if fill_w > 0 { fill_rect(x, y, fill_w, h, fill); }
}

/// Draw a ShareTech Mono character at 2× scale.
pub fn draw_char_share_tech_2x(x: usize, y: usize, ch: u8, fg: u32) -> usize {
    if ch < 32 || ch > 126 { return 0; }
    let idx = (ch - 32) as usize;
    let glyph = &fonts::SHARE_TECH_MONO_16_GLYPHS[idx];
    let mut bmp_idx = glyph.bmp_idx;
    for row in 0..glyph.h {
        for col in 0..glyph.w {
            let alpha = fonts::SHARE_TECH_MONO_16_BITMAP[bmp_idx];
            bmp_idx += 1;
            if alpha > 0 {
                let bx = x as isize + (col as isize + glyph.ox as isize) * 2;
                let by = y as isize + (row as isize + glyph.oy as isize) * 2;
                if bx < 0 || by < 0 { continue; }
                let (px, py) = (bx as usize, by as usize);
                for sy in 0..2usize {
                    for sx in 0..2usize {
                        let ppx = px + sx; let ppy = py + sy;
                        if ppx < WIDTH && ppy < HEIGHT {
                            if alpha == 255 { put_pixel(ppx, ppy, fg); }
                            else { let bg = get_pixel(ppx, ppy); put_pixel(ppx, ppy, blend(bg, fg, alpha)); }
                        }
                    }
                }
            }
        }
    }
    glyph.adv as usize * 2
}

/// Draw a string at 2× ShareTech scale.
pub fn draw_string_share_tech_2x(x: usize, y: usize, s: &str, fg: u32) {
    let mut cx = x;
    for byte in s.bytes() { cx += draw_char_share_tech_2x(cx, y, byte, fg); }
}

/// Measure a 2× ShareTech string width.
pub fn measure_string_share_tech_2x(s: &str) -> usize {
    measure_string_share_tech(s) * 2
}

/// Draw a Chakra Petch character at 2× scale.
pub fn draw_char_chakra_2x(x: usize, y: usize, ch: u8, fg: u32) -> usize {
    if ch < 32 || ch > 126 { return 0; }
    let idx = (ch - 32) as usize;
    let glyph = &fonts::CHAKRA_PETCH_16_GLYPHS[idx];
    let mut bmp_idx = glyph.bmp_idx;
    for row in 0..glyph.h {
        for col in 0..glyph.w {
            let alpha = fonts::CHAKRA_PETCH_16_BITMAP[bmp_idx];
            bmp_idx += 1;
            if alpha > 0 {
                let bx = x as isize + (col as isize + glyph.ox as isize) * 2;
                let by = y as isize + (row as isize + glyph.oy as isize) * 2;
                if bx < 0 || by < 0 { continue; }
                let (px, py) = (bx as usize, by as usize);
                for sy in 0..2usize {
                    for sx in 0..2usize {
                        let ppx = px + sx; let ppy = py + sy;
                        if ppx < WIDTH && ppy < HEIGHT {
                            if alpha == 255 { put_pixel(ppx, ppy, fg); }
                            else { let bg = get_pixel(ppx, ppy); put_pixel(ppx, ppy, blend(bg, fg, alpha)); }
                        }
                    }
                }
            }
        }
    }
    glyph.adv as usize * 2
}

/// Draw a string in Chakra Petch at 2× scale.
pub fn draw_string_chakra_2x(x: usize, y: usize, s: &str, fg: u32) {
    let mut cx = x;
    for byte in s.bytes() { cx += draw_char_chakra_2x(cx, y, byte, fg); }
}

/// Measure a 2× Chakra Petch string width.
pub fn measure_string_chakra_2x(s: &str) -> usize {
    let mut w = 0;
    for ch in s.bytes() {
        if ch >= 32 && ch <= 126 {
            w += fonts::CHAKRA_PETCH_16_GLYPHS[(ch - 32) as usize].adv as usize * 2;
        }
    }
    w
}

/// Draw a ShareTech Mono character at an arbitrary integer scale (1–8×). (6.3)
pub fn draw_char_share_tech_scaled(x: usize, y: usize, ch: u8, scale: usize, fg: u32) -> usize {
    if ch < 32 || ch > 126 { return 0; }
    let s = scale.max(1).min(8);
    let idx = (ch - 32) as usize;
    let glyph = &fonts::SHARE_TECH_MONO_16_GLYPHS[idx];
    let mut bmp_idx = glyph.bmp_idx;
    for row in 0..glyph.h {
        for col in 0..glyph.w {
            let alpha = fonts::SHARE_TECH_MONO_16_BITMAP[bmp_idx];
            bmp_idx += 1;
            if alpha > 0 {
                let bx = x as isize + (col as isize + glyph.ox as isize) * s as isize;
                let by = y as isize + (row as isize + glyph.oy as isize) * s as isize;
                if bx < 0 || by < 0 { continue; }
                let (px, py) = (bx as usize, by as usize);
                for sy in 0..s {
                    for sx in 0..s {
                        let ppx = px + sx; let ppy = py + sy;
                        if ppx < WIDTH && ppy < HEIGHT {
                            if alpha == 255 { put_pixel(ppx, ppy, fg); }
                            else { let bg = get_pixel(ppx, ppy); put_pixel(ppx, ppy, blend(bg, fg, alpha)); }
                        }
                    }
                }
            }
        }
    }
    glyph.adv as usize * s
}

/// Draw a ShareTech Mono string at an arbitrary integer scale. (6.3)
pub fn draw_string_share_tech_scaled(x: usize, y: usize, s: &str, scale: usize, fg: u32) {
    let mut cx = x;
    for byte in s.bytes() { cx += draw_char_share_tech_scaled(cx, y, byte, scale, fg); }
}

/// Measure a ShareTech Mono string width at the given scale. (6.3)
pub fn measure_string_share_tech_scaled(s: &str, scale: usize) -> usize {
    measure_string_share_tech(s) * scale.max(1)
}

/// Blit a pre-rasterised RGBA icon (alpha<<24|r<<16|g<<8|b) onto the framebuffer.
/// Pixels with alpha == 0 are skipped (transparent). All others are alpha-composited.
pub fn blit_icon(x: usize, y: usize, pixels: &[u32], w: usize, h: usize) {
    for row in 0..h {
        for col in 0..w {
            let px = x + col;
            let py = y + row;
            if px >= WIDTH || py >= HEIGHT { continue; }
            let p = pixels[row * w + col];
            let a = ((p >> 24) & 0xFF) as u8;
            if a == 0 { continue; }
            if a == 255 {
                // Opaque — write directly (strip alpha, keep RGB)
                put_pixel(px, py, p & 0x00FF_FFFF);
            } else {
                let bg = get_pixel(px, py);
                put_pixel(px, py, blend(bg, p & 0x00FF_FFFF, a));
            }
        }
    }
}

/// Blit the built-in folder icon.
pub fn blit_folder_icon(x: usize, y: usize) {
    blit_icon(x, y, &icons::FOLDER_ICON, icons::FOLDER_ICON_W, icons::FOLDER_ICON_H);
}

/// Blur a rectangular region of the framebuffer in-place.
/// Uses separable box blur (horizontal pass then vertical pass).
/// `radius` controls blur spread; 4-10 gives good shadow softness.
pub fn blur_region(rx: usize, ry: usize, rw: usize, rh: usize, radius: usize) {
    if rw == 0 || rh == 0 || radius == 0 { return; }
    let right  = (rx + rw).min(WIDTH);
    let bottom = (ry + rh).min(HEIGHT);

    // ── Horizontal pass: FB → BLUR_BUF ────────────────────────────────────
    unsafe {
        for y in ry..bottom {
            // Copy source row from FB to BLUR_BUF
            for x in rx..right {
                BLUR_BUF[y * WIDTH + x] = get_pixel(x, y);
            }
            // Write blurred row to FB using BLUR_BUF as source
            for x in rx..right {
                let lo = x.saturating_sub(radius).max(rx);
                let hi = (x + radius + 1).min(right);
                let cnt = (hi - lo) as u32;
                let mut sr = 0u32; let mut sg = 0u32; let mut sb = 0u32;
                for kx in lo..hi {
                    let p = BLUR_BUF[y * WIDTH + kx];
                    sr += (p >> 16) & 0xFF;
                    sg += (p >>  8) & 0xFF;
                    sb +=  p        & 0xFF;
                }
                put_pixel(x, y, ((sr / cnt) << 16) | ((sg / cnt) << 8) | (sb / cnt));
            }
        }

        // ── Vertical pass: FB (post-H-blur) → BLUR_BUF → FB ───────────────
        // Copy horizontally-blurred pixels into BLUR_BUF
        for y in ry..bottom {
            for x in rx..right {
                BLUR_BUF[y * WIDTH + x] = get_pixel(x, y);
            }
        }
        // Write vertically-blurred result to FB
        for x in rx..right {
            for y in ry..bottom {
                let lo = y.saturating_sub(radius).max(ry);
                let hi = (y + radius + 1).min(bottom);
                let cnt = (hi - lo) as u32;
                let mut sr = 0u32; let mut sg = 0u32; let mut sb = 0u32;
                for ky in lo..hi {
                    let p = BLUR_BUF[ky * WIDTH + x];
                    sr += (p >> 16) & 0xFF;
                    sg += (p >>  8) & 0xFF;
                    sb +=  p        & 0xFF;
                }
                put_pixel(x, y, ((sr / cnt) << 16) | ((sg / cnt) << 8) | (sb / cnt));
            }
        }
    }
}

/// Draw a soft drop-shadow behind a rectangle and blur it.
/// Call this BEFORE drawing the window it belongs to.
pub fn draw_blur_shadow(wx: usize, wy: usize, ww: usize, wh: usize, spread: usize, blur_r: usize) {
    // Draw a solid dark rect offset down-right, then blur it
    let sx = wx.saturating_sub(spread / 2).saturating_add(4);
    let sy = wy.saturating_sub(spread / 2).saturating_add(7);
    let sw = ww + spread;
    let sh = wh + spread / 2;
    // Draw filled shadow rect (dark, semi-transparent layers for soft edge)
    for i in 0..4 {
        let alpha = 60u8 + (i as u8) * 20;
        let ex = sx + i * 2;
        let ey = sy + i * 2;
        let ew = sw.saturating_sub(i * 4);
        let eh = sh.saturating_sub(i * 2);
        fill_rect_alpha(ex, ey, ew, eh, 0x000000, alpha);
    }
    // Now blur the shadow region
    let bx = sx.saturating_sub(blur_r);
    let by = sy.saturating_sub(blur_r);
    let bw = sw + blur_r * 2;
    let bh = sh + blur_r;
    blur_region(bx, by, bw, bh, blur_r);
}

/// Fill a radial gradient (inner colour at centre, outer at edge).
pub fn fill_gradient_radial(cx: usize, cy: usize, r: usize, inner: u32, outer: u32) {
    if r == 0 { return; }
    let r2 = (r * r) as u64;
    let x0 = cx.saturating_sub(r);
    let y0 = cy.saturating_sub(r);
    let x1 = (cx + r + 1).min(WIDTH);
    let y1 = (cy + r + 1).min(HEIGHT);
    for py in y0..y1 {
        for px in x0..x1 {
            let dx = px as i64 - cx as i64;
            let dy = py as i64 - cy as i64;
            let d2 = (dx * dx + dy * dy) as u64;
            if d2 > r2 { continue; }
            // t: 0 at centre, 255 at edge
            let t = ((d2 * 255 / r2) as u32).min(255) as u8;
            let col = blend(inner, outer, t);
            let bg = get_pixel(px, py);
            put_pixel(px, py, blend(bg, col, 180));
        }
    }
}

/// Apply a vignette: darkens edges, leaving the centre bright.
/// `strength` 0–255 controls how dark the corners get.
pub fn vignette(strength: u8) {
    if strength == 0 { return; }
    let cx = WIDTH  / 2;
    let cy = HEIGHT / 2;
    let max_d2 = (cx * cx + cy * cy) as u64;
    for py in 0..HEIGHT {
        for px in 0..WIDTH {
            let dx = px as i64 - cx as i64;
            let dy = py as i64 - cy as i64;
            let d2 = (dx * dx + dy * dy) as u64;
            // alpha: 0 at centre, strength at corner
            let t = (d2 * strength as u64 / max_d2).min(255) as u8;
            if t > 4 {
                let bg = get_pixel(px, py);
                put_pixel(px, py, blend(bg, 0x000000, t));
            }
        }
    }
}

/// Draw subtle horizontal scanlines over a region (CRT effect).
/// `step` = rows between lines; `alpha` = 0–255 darkness.
pub fn scanlines(y_start: usize, y_end: usize, step: usize, alpha: u8) {
    if alpha == 0 || step == 0 { return; }
    let mut y = y_start;
    while y < y_end.min(HEIGHT) {
        for x in 0..WIDTH {
            let bg = get_pixel(x, y);
            put_pixel(x, y, blend(bg, 0x000000, alpha));
        }
        y += step;
    }
}

/// Add deterministic per-pixel noise grain to a region.
/// `amount` controls grain intensity (4–12 is subtle but visible).
pub fn noise_grain(x: usize, y: usize, w: usize, h: usize, amount: u8, seed: u32) {
    if amount == 0 { return; }
    let right  = (x + w).min(WIDTH);
    let bottom = (y + h).min(HEIGHT);
    for py in y..bottom {
        for px in x..right {
            // Cheap deterministic hash
            let hv = (px as u32)
                .wrapping_mul(2246822519)
                .wrapping_add((py as u32).wrapping_mul(3266489917))
                .wrapping_add(seed)
                .wrapping_mul(2246822519);
            let noise = (hv >> 24) as u8 % amount;
            if noise == 0 { continue; }
            let p = get_pixel(px, py);
            let r = (((p >> 16) & 0xFF) as u8).saturating_add(noise);
            let g = (((p >>  8) & 0xFF) as u8).saturating_add(noise);
            let b = ((p & 0xFF) as u8).saturating_add(noise);
            put_pixel(px, py, ((r as u32) << 16) | ((g as u32) << 8) | b as u32);
        }
    }
}

/// Fill a rounded rect with a frosted-glass effect:
/// blurs whatever is behind it, then overlays a dark semi-transparent tint + subtle grain.
pub fn frosted_rounded_rect(x: usize, y: usize, w: usize, h: usize, r: usize,
                             tint: u32, tint_alpha: u8) {
    // 1. Blur the background behind this rect
    blur_region(x, y, w, h, 6);
    // 2. Dark tint overlay
    fill_rounded_rect_alpha(x, y, w, h, r, tint, tint_alpha);
    // 3. Subtle grain
    noise_grain(x, y, w, h, 6, (x as u32).wrapping_add(y as u32));
}
