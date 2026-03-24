//! WebSocket server support (RFC 6455).
//!
//! Provides:
//! - `ws_accept(id, key_b64)` — perform the opening handshake
//! - `ws_send_text(id, data)` — send an unmasked TEXT frame
//! - `ws_send_close(id)` — send a CLOSE frame
//! - `ws_recv_frame(id, buf)` — receive and unmask one client frame
//!
//! SHA-1 and Base64 are implemented inline (no external deps).

use super::tcp::{ConnId, tcp_write, tcp_read, tcp_wait_readable};

// ── RFC 6455 magic GUID ───────────────────────────────────────────────────────

const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// ── SHA-1 (FIPS 180-4) ───────────────────────────────────────────────────────

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0,
    ];

    let bit_len = (data.len() as u64) * 8;

    // Pad: append 0x80, zeros, 8-byte big-endian length (total ≡ 0 mod 64)
    let pad_len = {
        let rem = (data.len() + 9) % 64;
        if rem == 0 { 9 } else { 9 + (64 - rem) }
    };
    let total_len = data.len() + pad_len;
    let mut buf = [0u8; 256]; // enough for our key + guid (< 100 bytes)
    let tlen = total_len.min(buf.len());
    let copy_len = data.len().min(buf.len());
    buf[..copy_len].copy_from_slice(&data[..copy_len]);
    if data.len() < buf.len() { buf[data.len()] = 0x80; }
    if tlen >= 8 {
        buf[tlen - 8..tlen].copy_from_slice(&bit_len.to_be_bytes());
    }

    let mut i = 0;
    while i + 64 <= tlen {
        let mut w = [0u32; 80];
        for j in 0..16 {
            w[j] = u32::from_be_bytes([
                buf[i + j*4], buf[i + j*4+1], buf[i + j*4+2], buf[i + j*4+3],
            ]);
        }
        for j in 16..80 {
            w[j] = (w[j-3] ^ w[j-8] ^ w[j-14] ^ w[j-16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for j in 0..80 {
            let (f, k) = match j {
                0..=19  => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d,             0x6ED9EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _       => (b ^ c ^ d,             0xCA62C1D6u32),
            };
            let temp = a.rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(w[j]);
            e = d; d = c; c = b.rotate_left(30); b = a; a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        i += 64;
    }

    let mut out = [0u8; 20];
    for j in 0..5 { out[j*4..j*4+4].copy_from_slice(&h[j].to_be_bytes()); }
    out
}

// ── Base64 ───────────────────────────────────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8], out: &mut [u8]) -> usize {
    let mut o = 0;
    let mut i = 0;
    while i < data.len() {
        let b0 = data[i];
        let b1 = if i + 1 < data.len() { data[i + 1] } else { 0 };
        let b2 = if i + 2 < data.len() { data[i + 2] } else { 0 };
        if o + 4 > out.len() { break; }
        out[o]   = B64[(b0 >> 2) as usize];
        out[o+1] = B64[((b0 & 3) << 4 | b1 >> 4) as usize];
        out[o+2] = if i + 1 < data.len() { B64[((b1 & 0xF) << 2 | b2 >> 6) as usize] } else { b'=' };
        out[o+3] = if i + 2 < data.len() { B64[(b2 & 0x3F) as usize] } else { b'=' };
        o += 4;
        i += 3;
    }
    o
}

fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+'        => Some(62),
        b'/'        => Some(63),
        _           => None,
    }
}

/// Decode up to `out.len()` bytes from base64-encoded `input`.
/// Returns number of bytes written.
pub fn base64_decode(input: &[u8], out: &mut [u8]) -> usize {
    let mut o = 0;
    let mut i = 0;
    while i + 3 < input.len() && o + 2 < out.len() {
        let a = match b64_val(input[i])   { Some(v) => v, None => { i += 1; continue; } };
        let b = match b64_val(input[i+1]) { Some(v) => v, None => break };
        let c = if input[i+2] == b'=' { 0 } else { match b64_val(input[i+2]) { Some(v) => v, None => break } };
        let d = if input[i+3] == b'=' { 0 } else { match b64_val(input[i+3]) { Some(v) => v, None => break } };
        if o < out.len()     { out[o]   = (a << 2) | (b >> 4); o += 1; }
        if o < out.len() && input[i+2] != b'=' { out[o] = (b << 4) | (c >> 2); o += 1; }
        if o < out.len() && input[i+3] != b'=' { out[o] = (c << 6) | d; o += 1; }
        i += 4;
    }
    o
}

// ── Sec-WebSocket-Accept computation ─────────────────────────────────────────

/// Compute the `Sec-WebSocket-Accept` header value into `out`.
/// Returns the number of bytes written (base64-encoded SHA-1 of key+GUID).
pub fn compute_accept(key_b64: &[u8], out: &mut [u8]) -> usize {
    let mut concat = [0u8; 64];
    let klen = key_b64.len().min(60);
    concat[..klen].copy_from_slice(&key_b64[..klen]);
    let glen = WS_GUID.len().min(64 - klen);
    concat[klen..klen + glen].copy_from_slice(&WS_GUID[..glen]);
    let digest = sha1(&concat[..klen + glen]);
    base64_encode(&digest, out)
}

// ── Handshake ─────────────────────────────────────────────────────────────────

/// Perform the WebSocket opening handshake.
/// `key_b64` is the raw value of the `Sec-WebSocket-Key` header (base64, 24 bytes).
/// Returns true on success.
pub fn ws_accept(id: ConnId, key_b64: &[u8]) -> bool {
    let mut accept_val = [0u8; 32];
    let alen = compute_accept(key_b64, &mut accept_val);

    let mut resp = [0u8; 256];
    let mut p = 0usize;
    macro_rules! w { ($s:expr) => { for &b in $s { if p < resp.len() { resp[p] = b; p += 1; } } }; }
    w!(b"HTTP/1.1 101 Switching Protocols\r\n");
    w!(b"Upgrade: websocket\r\n");
    w!(b"Connection: Upgrade\r\n");
    w!(b"Sec-WebSocket-Accept: ");
    w!(&accept_val[..alen]);
    w!(b"\r\n\r\n");

    tcp_write(id, &resp[..p]) > 0
}

// ── Frame encoding/decoding ───────────────────────────────────────────────────

pub const WS_OPCODE_TEXT:  u8 = 0x1;
pub const WS_OPCODE_CLOSE: u8 = 0x8;
pub const WS_OPCODE_PING:  u8 = 0x9;
pub const WS_OPCODE_PONG:  u8 = 0xA;

/// Send an unmasked server WebSocket frame.
/// `opcode` is one of the WS_OPCODE_* constants.
pub fn ws_send_frame(id: ConnId, opcode: u8, payload: &[u8]) {
    let plen = payload.len();
    let mut hdr = [0u8; 10];
    let hdr_len;
    hdr[0] = 0x80 | (opcode & 0x0F); // FIN=1
    if plen <= 125 {
        hdr[1] = plen as u8;
        hdr_len = 2;
    } else if plen <= 65535 {
        hdr[1] = 126;
        hdr[2] = (plen >> 8) as u8;
        hdr[3] = plen as u8;
        hdr_len = 4;
    } else {
        hdr[1] = 127;
        hdr[2] = 0; hdr[3] = 0; hdr[4] = 0; hdr[5] = 0;
        hdr[6] = (plen >> 24) as u8;
        hdr[7] = (plen >> 16) as u8;
        hdr[8] = (plen >> 8)  as u8;
        hdr[9] =  plen as u8;
        hdr_len = 10;
    }
    tcp_write(id, &hdr[..hdr_len]);
    if !payload.is_empty() {
        tcp_write(id, payload);
    }
}

#[inline]
pub fn ws_send_text(id: ConnId, data: &[u8]) {
    ws_send_frame(id, WS_OPCODE_TEXT, data);
}

#[inline]
pub fn ws_send_close(id: ConnId) {
    ws_send_frame(id, WS_OPCODE_CLOSE, &[]);
}

pub struct WsFrame {
    pub opcode: u8,
    pub len:    usize,
}

/// Receive one WebSocket frame from the client.
/// Unmasks in place if masked.  Returns `None` on error/close.
pub fn ws_recv_frame(id: ConnId, buf: &mut [u8], timeout_ms: u32) -> Option<WsFrame> {
    // Read at least 2 header bytes
    let mut hdr = [0u8; 14];
    if !tcp_wait_readable(id, timeout_ms) { return None; }
    let n = tcp_read(id, &mut hdr[..2]);
    if n < 2 { return None; }

    let opcode = hdr[0] & 0x0F;
    let masked  = (hdr[1] & 0x80) != 0;
    let len7    = (hdr[1] & 0x7F) as usize;

    let payload_len = if len7 <= 125 {
        len7
    } else if len7 == 126 {
        if !tcp_wait_readable(id, 1000) { return None; }
        let n = tcp_read(id, &mut hdr[2..4]);
        if n < 2 { return None; }
        ((hdr[2] as usize) << 8) | (hdr[3] as usize)
    } else {
        // 8-byte extended length — cap at buf.len() for safety
        if !tcp_wait_readable(id, 1000) { return None; }
        let _ = tcp_read(id, &mut hdr[2..10]);
        // Only read lower 32 bits (frames > 4 GB unsupported on bare metal)
        ((hdr[6] as usize) << 24) | ((hdr[7] as usize) << 16)
            | ((hdr[8] as usize) << 8) | (hdr[9] as usize)
    };

    let mut mask = [0u8; 4];
    if masked {
        if !tcp_wait_readable(id, 1000) { return None; }
        let n = tcp_read(id, &mut mask);
        if n < 4 { return None; }
    }

    let to_read = payload_len.min(buf.len());
    let mut got = 0;
    while got < to_read {
        if !tcp_wait_readable(id, 2000) { break; }
        let n = tcp_read(id, &mut buf[got..to_read]);
        if n == 0 { break; }
        got += n;
    }

    if masked {
        for i in 0..got { buf[i] ^= mask[i % 4]; }
    }

    Some(WsFrame { opcode, len: got })
}

// ── Header helpers (shared with httpd) ───────────────────────────────────────

/// Search for a header value in a raw HTTP request.
/// Returns a slice of the value bytes (trimmed), or None.
pub fn find_header_value<'a>(raw: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    let mut i = 0;
    while i < raw.len() {
        // Find line end
        let end = raw[i..].iter().position(|&b| b == b'\n').map(|p| i + p).unwrap_or(raw.len());
        let line = &raw[i..end];
        if line.starts_with(b"\r") || line.is_empty() { break; }
        if line.len() > name.len() + 1 {
            let (lname, rest) = line.split_at(name.len());
            if lname.eq_ignore_ascii_case(name) && rest.starts_with(b":") {
                let val = rest[1..].trim_ascii_start();
                let val = if val.ends_with(b"\r") { &val[..val.len()-1] } else { val };
                return Some(val);
            }
        }
        i = end + 1;
    }
    None
}
