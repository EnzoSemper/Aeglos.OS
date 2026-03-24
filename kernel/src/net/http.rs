//! HTTP/1.1 client — GET over plain TCP. HTTPS stub (TLS pending).

use super::dns::dns_resolve;
use super::tcp::{
    tcp_connect, tcp_wait_established, tcp_write, tcp_read,
    tcp_wait_readable, tcp_close, tcp_readable, tcp_state, TcpState,
};

// ── Public types ──────────────────────────────────────────────────────────────

pub enum HttpResult {
    Ok(usize),          // bytes written to caller buffer
    DnsError,           // DNS resolution failed
    TcpError,           // TCP connect / send failed
    HttpError(u16),     // non-2xx status code
    Timeout,            // wait_readable or connect timed out
    BufferTooSmall,     // response body exceeds buf.len()
}

// ── Public functions ──────────────────────────────────────────────────────────

/// HTTP/1.1 GET over plain TCP. Writes the response body into `buf`.
pub fn http_get(host: &str, path: &str, port: u16, buf: &mut [u8]) -> HttpResult {
    // 1. Resolve hostname (fast-path for dotted IPs)
    let ip = match dns_resolve(host) {
        Some(ip) => ip,
        None     => return HttpResult::DnsError,
    };

    // 2. TCP connect — single attempt with 25s timeout.
    // SLIRP's first cold external connection can take 8-15s on macOS/HVF
    // (SLIRP must connect on the host side before sending SYN-ACK back).
    let id = match tcp_connect(ip, port) {
        Some(id) => id,
        None     => return HttpResult::TcpError,
    };

    if !tcp_wait_established(id, 25000) {
        tcp_close(id);
        return HttpResult::TcpError;
    }

    // 3. Send HTTP GET request
    let mut req = [0u8; 1024];
    let req_len = build_request(&mut req, host, path);
    if tcp_write(id, &req[..req_len]) == 0 {
        tcp_close(id);
        return HttpResult::TcpError;
    }

    // 4. Read and parse response
    let result = read_response(id, buf);
    tcp_close(id);
    result
}

/// HTTPS GET — TLS 1.3 client (TLS_AES_128_GCM_SHA256 + x25519).
/// Certificate verification is skipped at this stage.
pub fn https_get(host: &str, path: &str, buf: &mut [u8]) -> HttpResult {
    super::tls::tls_get(host, path, buf)
}

// ── Request builder ───────────────────────────────────────────────────────────

fn build_request(out: &mut [u8; 1024], host: &str, path: &str) -> usize {
    let mut p = 0usize;
    macro_rules! w {
        ($s:expr) => { for &b in $s.as_bytes() { if p < out.len() { out[p] = b; p += 1; } } };
    }
    w!("GET "); w!(path); w!(" HTTP/1.1\r\nHost: "); w!(host);
    w!("\r\nConnection: close\r\nUser-Agent: Aeglos/1.0\r\n\r\n");
    p
}

// ── Response reader ───────────────────────────────────────────────────────────

fn read_response(id: usize, buf: &mut [u8]) -> HttpResult {
    // Accumulate headers in a 2 KB stack buffer.
    let mut hdr_buf = [0u8; 2048];
    let mut hdr_len = 0usize;
    let mut hdr_end = 0usize;   // byte offset past the trailing \r\n\r\n

    // ── Phase 1: receive header ───────────────────────────────────────────────
    'hdr: loop {
        if !tcp_wait_readable(id, 10_000) {
            let st = tcp_state(id);
            if st == TcpState::CloseWait || st == TcpState::Closed
               || st == TcpState::Free   || st == TcpState::TimeWait {
                break 'hdr; // partial header; fall through to parse what we have
            }
            return HttpResult::Timeout;
        }
        let n = tcp_read(id, &mut hdr_buf[hdr_len..]);
        if n == 0 { continue; }
        let search_from = hdr_len.saturating_sub(3);
        hdr_len += n;
        // Search for the blank line that ends headers.
        for i in search_from..hdr_len.saturating_sub(3) {
            if &hdr_buf[i..i + 4] == b"\r\n\r\n" {
                hdr_end = i + 4;
                break 'hdr;
            }
        }
        if hdr_len >= hdr_buf.len() {
            return HttpResult::HttpError(0); // header too large
        }
    }

    if hdr_end == 0 { return HttpResult::TcpError; }

    // ── Parse status line ─────────────────────────────────────────────────────
    let status = parse_status(&hdr_buf[..hdr_end]);
    if status == 0 { return HttpResult::TcpError; }
    if !(200..300).contains(&status) { return HttpResult::HttpError(status); }

    // ── Parse Content-Length ──────────────────────────────────────────────────
    let content_length = parse_content_length(&hdr_buf[..hdr_end]);

    // ── Phase 2: body ─────────────────────────────────────────────────────────
    // Bytes that arrived together with the header (after hdr_end).
    let pre = &hdr_buf[hdr_end..hdr_len];
    let mut written = 0usize;
    if !pre.is_empty() {
        if pre.len() > buf.len() { return HttpResult::BufferTooSmall; }
        buf[..pre.len()].copy_from_slice(pre);
        written = pre.len();
        if content_length.map(|cl| written >= cl).unwrap_or(false) {
            return HttpResult::Ok(written);
        }
    }

    // Stream remaining body bytes.
    loop {
        let avail = tcp_readable(id);
        if avail > 0 {
            if written >= buf.len() { return HttpResult::BufferTooSmall; }
            let n = tcp_read(id, &mut buf[written..]);
            written += n;
            if content_length.map(|cl| written >= cl).unwrap_or(false) {
                break;
            }
            continue;
        }

        // Nothing buffered — check connection liveness.
        let st = tcp_state(id);
        match st {
            TcpState::Closed | TcpState::Free | TcpState::TimeWait => break,
            // Peer closed its send direction; no more data will arrive.
            TcpState::CloseWait | TcpState::FinWait2 => break,
            _ => {
                if !tcp_wait_readable(id, 8_000) { break; }
            }
        }
    }

    HttpResult::Ok(written)
}

// ── Header helpers ────────────────────────────────────────────────────────────

/// Parse the three-digit HTTP status code from the status line.
fn parse_status(hdr: &[u8]) -> u16 {
    // "HTTP/1.x NNN ..."
    if hdr.len() < 12 { return 0; }
    if &hdr[..5] != b"HTTP/" { return 0; }
    let mut i = 5usize;
    // Skip version string
    while i < hdr.len() && hdr[i] != b' ' { i += 1; }
    i += 1; // skip space
    let mut code = 0u16;
    for _ in 0..3 {
        if i >= hdr.len() { return 0; }
        let b = hdr[i];
        if b < b'0' || b > b'9' { return 0; }
        code = code * 10 + (b - b'0') as u16;
        i += 1;
    }
    code
}

/// Case-insensitive search for "content-length:" in the header block.
fn parse_content_length(hdr: &[u8]) -> Option<usize> {
    const NEEDLE: &[u8] = b"content-length:";
    let mut i = 0usize;
    'lines: loop {
        if i >= hdr.len() { return None; }
        // Try to match NEEDLE at position i (case-insensitive)
        if i + NEEDLE.len() <= hdr.len() {
            let mut hit = true;
            for (j, &nb) in NEEDLE.iter().enumerate() {
                if hdr[i + j].to_ascii_lowercase() != nb { hit = false; break; }
            }
            if hit {
                let mut vi = i + NEEDLE.len();
                while vi < hdr.len() && (hdr[vi] == b' ' || hdr[vi] == b'\t') { vi += 1; }
                let mut val = 0usize;
                while vi < hdr.len() && hdr[vi] >= b'0' && hdr[vi] <= b'9' {
                    val = val * 10 + (hdr[vi] - b'0') as usize;
                    vi += 1;
                }
                return Some(val);
            }
        }
        // Advance to the next line.
        while i < hdr.len() && hdr[i] != b'\n' { i += 1; }
        if i >= hdr.len() { return None; }
        i += 1; // skip '\n'
        continue 'lines;
    }
}

// ── URL parser ────────────────────────────────────────────────────────────────

/// Split a URL of the form `http[s]://host[:port]/path` into its components.
/// Returns `(scheme_is_https, host, port, path)` or `None` on parse failure.
pub fn parse_url(url: &str) -> Option<(bool, &str, u16, &str)> {
    let (https, rest) = if url.starts_with("https://") {
        (true, &url[8..])
    } else if url.starts_with("http://") {
        (false, &url[7..])
    } else {
        return None;
    };

    // Split on first '/'
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None      => (rest, "/"),
    };

    // Split host from port
    let (host, port) = match authority.rfind(':') {
        Some(idx) => {
            let h = &authority[..idx];
            let p: u16 = authority[idx + 1..].parse().ok()?;
            (h, p)
        }
        None => (authority, if https { 443 } else { 80 }),
    };

    Some((https, host, port, path))
}
