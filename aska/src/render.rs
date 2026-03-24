/// Rendering utilities for Aska shell output.
///
/// Platform-agnostic: all output flows through the `Sink` trait, allowing the
/// same formatting code to drive a UART, a framebuffer text layer, or a test
/// capture buffer.

/// Output sink — implemented by the calling environment.
pub trait Sink {
    fn write_str(&mut self, s: &str);

    fn write_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        self.write_str(s);
    }

    fn newline(&mut self) {
        self.write_str("\r\n");
    }
}

/// ANSI escape sequences for terminal colour.
pub mod color {
    pub const RESET:  &str = "\x1b[0m";
    pub const BOLD:   &str = "\x1b[1m";
    pub const RED:    &str = "\x1b[31m";
    pub const GREEN:  &str = "\x1b[32m";
    pub const YELLOW: &str = "\x1b[33m";
    pub const BLUE:   &str = "\x1b[34m";
    pub const CYAN:   &str = "\x1b[36m";
    pub const DIM:    &str = "\x1b[90m";
    pub const WHITE:  &str = "\x1b[97m";
}

/// Print a bold section header followed by a newline.
pub fn header(sink: &mut impl Sink, title: &str) {
    sink.write_str(color::BOLD);
    sink.write_str(title);
    sink.write_str(color::RESET);
    sink.newline();
}

/// Print a key-value line, padding the key to `key_width` characters.
pub fn kv(sink: &mut impl Sink, key: &str, value: &str, key_width: usize) {
    sink.write_str("  ");
    sink.write_str(key);
    let pad = key_width.saturating_sub(key.len());
    for _ in 0..pad {
        sink.write_str(" ");
    }
    sink.write_str(value);
    sink.newline();
}

/// Print a success message in green.
pub fn ok(sink: &mut impl Sink, msg: &str) {
    sink.write_str(color::GREEN);
    sink.write_str(msg);
    sink.write_str(color::RESET);
    sink.newline();
}

/// Print an error message in red, prefixed with "Error: ".
pub fn err(sink: &mut impl Sink, msg: &str) {
    sink.write_str(color::RED);
    sink.write_str("Error: ");
    sink.write_str(msg);
    sink.write_str(color::RESET);
    sink.newline();
}

/// Print a dim informational line.
pub fn info(sink: &mut impl Sink, msg: &str) {
    sink.write_str(color::DIM);
    sink.write_str(msg);
    sink.write_str(color::RESET);
    sink.newline();
}

/// Print a cyan notice (AI/memory output).
pub fn notice(sink: &mut impl Sink, msg: &str) {
    sink.write_str(color::CYAN);
    sink.write_str(msg);
    sink.write_str(color::RESET);
    sink.newline();
}

/// Print a horizontal separator line (40 dashes).
pub fn separator(sink: &mut impl Sink) {
    sink.write_str("----------------------------------------");
    sink.newline();
}

/// Write a decimal u32 to the sink.
pub fn write_dec(sink: &mut impl Sink, n: u32) {
    if n == 0 {
        sink.write_str("0");
        return;
    }
    let mut buf = [0u8; 10];
    let mut i = 10usize;
    let mut v = n;
    while v > 0 {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
    // SAFETY: buf[i..10] are all ASCII digits
    if let Ok(s) = core::str::from_utf8(&buf[i..]) {
        sink.write_str(s);
    }
}

/// Write a u64 as lowercase hex (no leading zeros, minimum one digit).
pub fn write_hex(sink: &mut impl Sink, n: u64) {
    if n == 0 {
        sink.write_str("0");
        return;
    }
    let mut buf = [0u8; 16];
    let mut i = 16usize;
    let mut v = n;
    while v > 0 {
        i -= 1;
        let nibble = (v & 0xf) as u8;
        buf[i] = if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 };
        v >>= 4;
    }
    if let Ok(s) = core::str::from_utf8(&buf[i..]) {
        sink.write_str(s);
    }
}

/// Write a human-readable file size (B / KB / MB) to the sink.
pub fn write_size(sink: &mut impl Sink, bytes: u32) {
    if bytes >= 1024 * 1024 {
        write_dec(sink, bytes / (1024 * 1024));
        sink.write_str(" MB");
    } else if bytes >= 1024 {
        write_dec(sink, bytes / 1024);
        sink.write_str(" KB");
    } else {
        write_dec(sink, bytes);
        sink.write_str(" B");
    }
}
