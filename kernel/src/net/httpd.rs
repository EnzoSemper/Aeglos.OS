/// Aeglos HTTP/1.1 server — REST interface for AI inference and system status.
///
/// Runs as a kernel task (EL1) on port 80.  Accepts one connection at a time.
///
/// Routes:
///   GET  /             → HTML status page (OS info, IP, uptime)
///   GET  /health       → 200 OK  {"status":"ok"}
///   GET  /stats        → 200 OK  JSON system statistics
///   POST /infer        → Forward body as prompt to Numenor, return response
///   GET  /mem?q=QUERY  → Semantic memory search, return JSON results
///   *                  → 404 Not Found

extern crate alloc;
use alloc::vec::Vec;
use core::convert::TryInto;

use super::tcp::{
    tcp_listen, tcp_accept, tcp_write, tcp_read,
    tcp_wait_readable, tcp_close, tcp_state, TcpState, ConnId,
};
use crate::process::scheduler;
use crate::ipc::Message;
use numenor::ipc::{AiMessage, AI_OP_INFER, AI_OP_INFER_STREAM, AI_OP_TOKEN, AI_OP_STREAM_END};
use super::ws::{ws_accept, ws_send_text, ws_send_close, ws_recv_frame,
                find_header_value, WS_OPCODE_TEXT, WS_OPCODE_CLOSE};

// ── Constants ─────────────────────────────────────────────────────────────────

const HTTP_PORT:       u16   = 80;
const RECV_BUF_SIZE:   usize = 4096;
const BODY_BUF_SIZE:   usize = 512;
const RESP_BUF_SIZE:   usize = 8192;
const AI_TIMEOUT_TICK: usize = 6000; // 60 s at 100 Hz

// ── Kernel task entry point ───────────────────────────────────────────────────

/// Entry point for the HTTP server kernel task.
/// Spawned by `kernel_main` once the network stack is up.
pub fn httpd_task() -> ! {
    let uart = crate::drivers::uart::Uart::new();

    // Wait for DHCP to assign an IP (up to 10 s)
    let freq = crate::arch::aarch64::timer::physical_timer_freq();
    let deadline = crate::arch::aarch64::timer::physical_timer_count() + freq * 10;
    loop {
        let ip = super::get_ip();
        if ip[0] != 0 { break; }
        if crate::arch::aarch64::timer::physical_timer_count() >= deadline { break; }
        scheduler::yield_cpu();
    }

    let listener = loop {
        match tcp_listen(HTTP_PORT) {
            Some(id) => break id,
            None => { scheduler::yield_cpu(); }
        }
    };

    let ip = super::get_ip();
    uart.puts("[httpd] Listening on http://");
    for (i, &b) in ip.iter().enumerate() {
        uart.put_dec(b as usize);
        if i < 3 { uart.puts("."); }
    }
    uart.puts(":80/\r\n");

    loop {
        // Accept one connection (100 ms poll timeout so we can yield between attempts)
        if let Some(conn) = tcp_accept(listener, 100) {
            handle_connection(conn);
        } else {
            scheduler::yield_cpu();
        }
    }
}

// ── Connection handler ────────────────────────────────────────────────────────

fn handle_connection(id: ConnId) {
    // Read the full HTTP request (headers + body) with a 5 s timeout
    let mut raw = [0u8; RECV_BUF_SIZE];
    let mut total = 0usize;

    // Read until we see the end of headers (\r\n\r\n) or buffer fills
    loop {
        if !tcp_wait_readable(id, 5000) { tcp_close(id); return; }
        let n = tcp_read(id, &mut raw[total..]);
        if n == 0 { break; }
        total += n;

        // Check if we have end-of-headers
        if has_end_of_headers(&raw[..total]) { break; }
        if total >= raw.len() { break; }
    }

    if total == 0 { tcp_close(id); return; }

    // Parse request line
    let (method, path, _version) = match parse_request_line(&raw[..total]) {
        Some(t) => t,
        None    => { send_400(id); tcp_close(id); return; }
    };

    // Parse Content-Length if present
    let content_length = parse_content_length(&raw[..total]).unwrap_or(0);

    // Find header/body boundary
    let body_start = match find_body_start(&raw[..total]) {
        Some(i) => i,
        None    => total,
    };

    // Read remaining body bytes if needed
    let mut body_owned = [0u8; BODY_BUF_SIZE];
    let body_len;
    {
        let already = if body_start < total { total - body_start } else { 0 };
        let need = content_length.saturating_sub(already).min(BODY_BUF_SIZE - already);
        if already > 0 {
            let copy = already.min(BODY_BUF_SIZE);
            body_owned[..copy].copy_from_slice(&raw[body_start..body_start + copy]);
        }
        let mut got = already;
        if need > 0 {
            let deadline = crate::arch::aarch64::timer::physical_timer_count()
                           + crate::arch::aarch64::timer::physical_timer_freq() * 5;
            while got < already + need {
                if !tcp_wait_readable(id, 1000) { break; }
                let n = tcp_read(id, &mut body_owned[got..already + need]);
                if n == 0 { break; }
                got += n;
                if crate::arch::aarch64::timer::physical_timer_count() >= deadline { break; }
            }
        }
        body_len = got;
    }
    let body = &body_owned[..body_len];

    // Detect WebSocket upgrade request
    let is_ws_upgrade = find_header_value(&raw[..total], b"Upgrade")
        .map(|v| v.eq_ignore_ascii_case(b"websocket"))
        .unwrap_or(false);

    if is_ws_upgrade {
        let ws_key = find_header_value(&raw[..total], b"Sec-WebSocket-Key")
            .unwrap_or(b"");
        let path_base = path.splitn(2, |&b| b == b'?').next().unwrap_or(path);
        if path_base == b"/ws" {
            if ws_accept(id, ws_key) {
                handle_ws_session(id);
            }
        } else {
            send_400(id);
        }
        tcp_close(id);
        return;
    }

    // Dispatch normal HTTP
    let mut resp = [0u8; RESP_BUF_SIZE];
    let resp_len = route(method, path, body, &mut resp);

    // Send response
    let mut sent = 0;
    while sent < resp_len {
        let n = tcp_write(id, &resp[sent..resp_len]);
        if n == 0 { break; }
        sent += n;
    }

    tcp_close(id);
}

// ── WebSocket session ─────────────────────────────────────────────────────────

/// Handle a WebSocket session after the handshake.
/// Protocol: client sends JSON prompt → server streams tokens → sends {"done":true}.
fn handle_ws_session(id: ConnId) {
    let mut frame_buf = [0u8; 1024];

    loop {
        let frame = match ws_recv_frame(id, &mut frame_buf, 30_000) {
            Some(f) => f,
            None    => break,
        };

        if frame.opcode == WS_OPCODE_CLOSE { break; }
        if frame.opcode != WS_OPCODE_TEXT  { continue; }

        let text = &frame_buf[..frame.len];

        // Extract prompt from JSON: {"prompt":"..."} or bare text
        let prompt = extract_json_string(text, b"prompt").unwrap_or(text);

        // Send AI_OP_INFER_STREAM to Numenor (TID 1)
        let numenor_tid = 1usize;
        let msg = numenor::ipc::AiMessage {
            op:   AI_OP_INFER_STREAM,
            arg1: prompt.as_ptr() as u64,
            arg2: prompt.len()   as u64,
        };
        let send_msg = crate::ipc::Message { sender: 0, data: msg.to_bytes() };
        crate::process::scheduler::send_message(numenor_tid, send_msg);

        // Forward streamed tokens to WebSocket client
        let deadline = crate::arch::aarch64::timer::physical_timer_count()
            + crate::arch::aarch64::timer::physical_timer_freq() * 60;

        loop {
            if crate::arch::aarch64::timer::physical_timer_count() >= deadline { break; }
            let mut imsg = crate::ipc::Message { sender: 0, data: [0; 32] };
            match scheduler::try_recv_message() {
                Some(m) => imsg = m,
                None    => { crate::process::scheduler::yield_cpu(); continue; }
            }
            if imsg.sender != numenor_tid { continue; }
            let op = u64::from_le_bytes(imsg.data[0..8].try_into().unwrap_or([0;8]));
            if op == AI_OP_TOKEN {
                let tlen = (imsg.data[8] as usize).min(23);
                ws_send_text(id, &imsg.data[9..9 + tlen]);
            } else if op == AI_OP_STREAM_END {
                ws_send_text(id, b"{\"done\":true}");
                break;
            }
        }
    }

    ws_send_close(id);
}

// ── Router ────────────────────────────────────────────────────────────────────

fn route(method: &[u8], path: &[u8], body: &[u8], out: &mut [u8; RESP_BUF_SIZE]) -> usize {
    // Strip query string from path for matching
    let path_base = path.splitn(2, |&b| b == b'?').next().unwrap_or(path);
    let query     = path.splitn(2, |&b| b == b'?').nth(1).unwrap_or(b"");

    match (method, path_base) {
        (b"GET",  b"/")       => page_index(out),
        (b"GET",  b"/health") => page_health(out),
        (b"GET",  b"/stats")  => page_stats(out),
        (b"POST", b"/infer")  => page_infer(body, out),
        (b"GET",  b"/mem")    => page_mem(query, out),
        _                     => page_404(out),
    }
}

// ── Route handlers ────────────────────────────────────────────────────────────

fn page_index(out: &mut [u8; RESP_BUF_SIZE]) -> usize {
    let ip = super::get_ip();
    let free_mb  = crate::memory::free_pages() * crate::memory::PAGE_SIZE / 1048576;
    let total_mb = crate::memory::total_pages() * crate::memory::PAGE_SIZE / 1048576;

    let mut body = [0u8; 2048];
    let mut b = 0usize;

    macro_rules! wb {
        ($s:expr) => { for &c in $s { if b < body.len() { body[b] = c; b += 1; } } };
        (str $s:expr) => { wb!($s.as_bytes()) };
        (u $v:expr) => {
            let s = num_to_str($v as u64);
            wb!(s.as_slice())
        };
    }

    wb!(str "<html><head><title>Aeglos OS</title></head><body>");
    wb!(str "<h1>Aeglos OS — AI-Native Kernel</h1><pre>");
    wb!(str "IP     : ");
    for (i, &b2) in ip.iter().enumerate() {
        wb!(u b2 as usize);
        if i < 3 { wb!(str "."); }
    }
    wb!(str "\nMemory : ");
    wb!(u free_mb);
    wb!(str " MB free / ");
    wb!(u total_mb);
    wb!(str " MB total");
    wb!(str "\nRoutes : GET /health  GET /stats  POST /infer  GET /mem?q=...");
    wb!(str "</pre></body></html>");

    build_response(200, b"text/html", &body[..b], out)
}

fn page_health(out: &mut [u8; RESP_BUF_SIZE]) -> usize {
    build_response(200, b"application/json", b"{\"status\":\"ok\"}", out)
}

fn page_stats(out: &mut [u8; RESP_BUF_SIZE]) -> usize {
    let free_mb  = crate::memory::free_pages() * crate::memory::PAGE_SIZE / 1048576;
    let total_mb = crate::memory::total_pages() * crate::memory::PAGE_SIZE / 1048576;
    let task_cnt = scheduler::task_count();

    let idle  = scheduler::idle_ticks();
    let total = scheduler::total_ticks();
    let cpu_pct = if total > 0 { 100 - (idle * 100 / total) } else { 0 };

    let mut body = [0u8; 256];
    let mut p = 0usize;
    macro_rules! w {
        ($s:expr) => { for &c in $s.as_bytes() { if p < body.len() { body[p] = c; p += 1; } } };
    }
    macro_rules! wu {
        ($v:expr) => { let s = num_to_str($v as u64); w!(s.as_str()); };
    }

    w!("{\"free_mb\":"); wu!(free_mb);
    w!(",\"total_mb\":"); wu!(total_mb);
    w!(",\"cpu_pct\":"); wu!(cpu_pct);
    w!(",\"task_cnt\":"); wu!(task_cnt);
    w!("}");

    build_response(200, b"application/json", &body[..p], out)
}

fn page_infer(prompt: &[u8], out: &mut [u8; RESP_BUF_SIZE]) -> usize {
    if prompt.is_empty() {
        return build_response(400, b"text/plain", b"Empty prompt", out);
    }
    if prompt.len() > BODY_BUF_SIZE {
        return build_response(413, b"text/plain", b"Prompt too large", out);
    }

    // Build AiMessage: op=INFER, arg1=prompt_ptr (kernel VA), arg2=prompt_len
    let ai_msg = AiMessage {
        op:   AI_OP_INFER,
        arg1: prompt.as_ptr() as u64,
        arg2: prompt.len() as u64,
    };
    let req = Message {
        sender: scheduler::current_tid(),
        data:   ai_msg.to_bytes(),
    };

    const NUMENOR_TID: usize = 1;
    if scheduler::send_message(NUMENOR_TID, req).is_err() {
        return build_response(503, b"text/plain", b"Numenor unavailable", out);
    }

    // Spin-yield until Numenor replies (up to AI_TIMEOUT_TICK ticks)
    let deadline = crate::arch::aarch64::timer::tick_count() + AI_TIMEOUT_TICK;
    let reply = loop {
        if let Some(msg) = scheduler::recv_message() {
            if msg.sender == NUMENOR_TID { break Some(msg); }
        }
        if crate::arch::aarch64::timer::tick_count() >= deadline { break None; }
        scheduler::yield_cpu();
    };

    let reply = match reply {
        Some(m) => m,
        None    => return build_response(504, b"text/plain", b"Numenor timeout", out),
    };

    // Extract response pointer and length from reply
    let ai_reply = AiMessage::from_bytes(&reply.data);
    let resp_ptr = ai_reply.arg1 as *const u8;
    let resp_len = ai_reply.arg2 as usize;

    let response = if resp_ptr.is_null() || resp_len == 0 {
        b"" as &[u8]
    } else {
        unsafe { core::slice::from_raw_parts(resp_ptr, resp_len.min(4096)) }
    };

    build_response(200, b"text/plain", response, out)
}

fn page_mem(query: &[u8], out: &mut [u8; RESP_BUF_SIZE]) -> usize {
    // Extract q= parameter from query string
    let q = query.split(|&b| b == b'&')
        .find_map(|kv| {
            let mut parts = kv.splitn(2, |&b| b == b'=');
            let key = parts.next()?;
            let val = parts.next()?;
            if key == b"q" { Some(val) } else { None }
        })
        .unwrap_or(b"");

    if q.is_empty() {
        return build_response(400, b"application/json",
                              b"{\"error\":\"missing q= parameter\"}", out);
    }

    // Forward to semantic service (TID=2, op=QUERY=104)
    const SEMANTIC_TID: usize = 2;
    let mut data = [0u8; 32];
    data[0] = 104; // QUERY op
    let copy = q.len().min(28);
    data[4..4 + copy].copy_from_slice(&q[..copy]);

    let req = Message { sender: scheduler::current_tid(), data };
    if scheduler::send_message(SEMANTIC_TID, req).is_err() {
        return build_response(503, b"application/json",
                              b"{\"error\":\"semantic service unavailable\"}", out);
    }

    let deadline = crate::arch::aarch64::timer::tick_count() + 500; // 5 s
    let reply = loop {
        if let Some(msg) = scheduler::recv_message() {
            if msg.sender == SEMANTIC_TID { break Some(msg); }
        }
        if crate::arch::aarch64::timer::tick_count() >= deadline { break None; }
        scheduler::yield_cpu();
    };

    let reply = match reply {
        Some(m) => m,
        None    => return build_response(504, b"application/json",
                                         b"{\"error\":\"semantic timeout\"}", out),
    };

    // reply.data contains the result text (up to 28 bytes) in data[4..]
    let result_len = reply.data[1] as usize;
    let result = &reply.data[4..4 + result_len.min(28)];

    let mut body = [0u8; 128];
    let mut p = 0usize;
    body[p..p+13].copy_from_slice(b"{\"result\":\""); p += 11;
    let copy = result.len().min(body.len() - p - 4);
    body[p..p + copy].copy_from_slice(&result[..copy]); p += copy;
    body[p] = b'"'; p += 1;
    body[p] = b'}'; p += 1;

    build_response(200, b"application/json", &body[..p], out)
}

fn page_404(out: &mut [u8; RESP_BUF_SIZE]) -> usize {
    build_response(404, b"text/plain", b"Not Found", out)
}

fn send_400(id: ConnId) {
    let mut buf = [0u8; RESP_BUF_SIZE];
    let n = build_response(400, b"text/plain", b"Bad Request", &mut buf);
    tcp_write(id, &buf[..n]);
}

// ── HTTP primitives ───────────────────────────────────────────────────────────

fn build_response(status: u16, content_type: &[u8], body: &[u8], out: &mut [u8; RESP_BUF_SIZE]) -> usize {
    let mut p = 0usize;

    macro_rules! w {
        ($s:expr) => { for &c in $s { if p < out.len() { out[p] = c; p += 1; } } };
    }

    let status_str: &[u8] = match status {
        200 => b"200 OK",
        400 => b"400 Bad Request",
        404 => b"404 Not Found",
        413 => b"413 Payload Too Large",
        503 => b"503 Service Unavailable",
        504 => b"504 Gateway Timeout",
        _   => b"200 OK",
    };

    w!(b"HTTP/1.1 "); w!(status_str); w!(b"\r\n");
    w!(b"Content-Type: "); w!(content_type); w!(b"\r\n");
    w!(b"Content-Length: ");
    let cls = num_to_str(body.len() as u64);
    w!(cls.as_bytes());
    w!(b"\r\nConnection: close\r\nServer: Aeglos/1.0\r\n");
    w!(b"Access-Control-Allow-Origin: *\r\n\r\n");
    w!(body);

    p
}

// ── Request parsing helpers ───────────────────────────────────────────────────

fn parse_request_line(raw: &[u8]) -> Option<(&[u8], &[u8], &[u8])> {
    let line_end = raw.iter().position(|&b| b == b'\r')?;
    let line = &raw[..line_end];
    let mut parts = line.splitn(3, |&b| b == b' ');
    let method  = parts.next()?;
    let path    = parts.next()?;
    let version = parts.next().unwrap_or(b"HTTP/1.0");
    Some((method, path, version))
}

fn parse_content_length(raw: &[u8]) -> Option<usize> {
    // Find "Content-Length:" header (case-sensitive per HTTP/1.1)
    let needle = b"Content-Length: ";
    let pos = raw.windows(needle.len()).position(|w| w == needle)?;
    let rest = &raw[pos + needle.len()..];
    let end  = rest.iter().position(|&b| b == b'\r').unwrap_or(rest.len());
    let num  = &rest[..end];
    let mut val = 0usize;
    for &b in num {
        if b < b'0' || b > b'9' { break; }
        val = val * 10 + (b - b'0') as usize;
    }
    Some(val)
}

fn find_body_start(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

fn has_end_of_headers(raw: &[u8]) -> bool {
    raw.windows(4).any(|w| w == b"\r\n\r\n")
}

// ── Number formatting (no_std) ────────────────────────────────────────────────

fn num_to_str(mut n: u64) -> NumStr {
    let mut s = NumStr { buf: [0u8; 20], len: 0 };
    if n == 0 {
        s.buf[0] = b'0'; s.len = 1; return s;
    }
    let mut i = 20usize;
    while n > 0 {
        i -= 1;
        s.buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    // Shift to front
    let digits = 20 - i;
    for j in 0..digits { s.buf[j] = s.buf[i + j]; }
    s.len = digits;
    s
}

struct NumStr {
    buf: [u8; 20],
    len: usize,
}

impl NumStr {
    fn as_bytes(&self) -> &[u8] { &self.buf[..self.len] }
    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.len]).unwrap_or("0")
    }
    fn as_slice(&self) -> &[u8] { self.as_bytes() }
}

// ── JSON helper ───────────────────────────────────────────────────────────────

/// Extract a string value for `key` from a flat JSON object.
/// Handles `{"key":"value"}` only (no nesting, no escapes).
/// Returns a slice of the raw bytes of the value (without quotes).
fn extract_json_string<'a>(json: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    // Find `"key":"`
    let mut i = 0;
    while i < json.len() {
        if json[i] == b'"' {
            i += 1;
            // Check key match
            if json[i..].starts_with(key) {
                let after = i + key.len();
                if after + 2 < json.len() && json[after] == b'"' {
                    // key": found; skip past `":`
                    let mut j = after + 1;
                    while j < json.len() && json[j] != b'"' { j += 1; }
                    if j >= json.len() { return None; }
                    j += 1; // skip opening quote
                    let start = j;
                    while j < json.len() && json[j] != b'"' { j += 1; }
                    return Some(&json[start..j]);
                }
            }
            // Skip past the rest of this string literal
            while i < json.len() && json[i] != b'"' { i += 1; }
        }
        i += 1;
    }
    None
}
