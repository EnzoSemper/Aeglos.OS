#![no_std]
#![no_main]

use core::arch::global_asm;
use core::panic::PanicInfo;
use aska::{
    intent::{parse, split_first_word, Intent, MemOp, CapOp},
    shell::ShellState,
    render::{self, Sink},
};

pub mod auth;
pub mod fonts;
pub mod ui;
pub mod desktop;
pub mod location;
pub mod icons;

// ── Syscall numbers ────────────────────────────────────────────────────────────
const SYS_SEND:         usize = 1;
const SYS_RECV:         usize = 2;
const SYS_YIELD:        usize = 3;
const SYS_EXIT:         usize = 4;
const SYS_LOG:          usize = 8;
const SYS_EXEC:         usize = 13;
const SYS_OPEN:         usize = 16;
const SYS_READ_FD:      usize = 17;
const SYS_WRITE_FD:     usize = 18;
const SYS_CLOSE:        usize = 19;
const SYS_READDIR:      usize = 20;
const SYS_WAIT:         usize = 21;
const SYS_KREAD:        usize = 25;
const SYS_FB_INFO:      usize = 26;
const SYS_TRY_RECV:     usize = 30; // non-blocking recv — never Blocks the task
const SYS_GET_RTC:      usize = 31; // () -> Unix epoch seconds from PL031 RTC
const SYS_GET_STATS:    usize = 32; // (out_ptr) -> 0, writes [free_mb, total_mb, cpu_pct, task_cnt] u32x4
const SYS_GET_IP:       usize = 33;
const SYS_PING:         usize = 34; // (ip, timeout_ms) -> rtt or -1
const SYS_HTTP_GET:     usize = 35; // (url_ptr, url_len, buf_ptr, buf_len) -> bytes or -err
const SYS_HTTPS_GET:    usize = 36; // same, TLS
const SYS_HTTPS_POST:   usize = 50; // (url_ptr, url_len, body_ptr, body_len, buf_ptr, buf_len) -> bytes or -err

const AI_OP_RESET_HISTORY: u64 = 20;
const SYS_DNS_RESOLVE:  usize = 37; // (name_ptr, name_len, out_ip4_ptr) -> 0 or -1
const SYS_CREATE:       usize = 38; // (path_ptr, path_len, flags) → fd or -1
const SYS_SURF_CREATE:  usize = 40; // (width, height, z_order) → surface_id or -1
const SYS_SURF_DESTROY: usize = 41; // (surface_id, 0, 0) → 0
const SYS_SURF_MAP:     usize = 42; // (surface_id, 0, 0) → user VA of pixel buffer
const SYS_SURF_FLUSH:   usize = 43; // (surface_id, 0, 0) → 0
const SYS_SURF_MOVE:    usize = 44; // (surface_id, x, y) → 0
const SYS_FB_MAP:       usize = 27;
const SYS_FB_FLUSH:     usize = 28;
const SYS_INPUT_POLL:   usize = 29;
const SYS_CONSOLE_GETS: usize = 99;
const SYS_WASM_EXEC:    usize = 51; // (path_ptr, path_len) → exit_code or -err
const SYS_CAP_GRANT:    usize = 52; // (tid, cap_bits) → 0 or -1
const SYS_CAP_REVOKE:   usize = 53; // (tid, cap_bits) → 0 or -1
const SYS_CAP_QUERY:    usize = 54; // (tid) → caps bitmask as usize
const SYS_LIST_TASKS:   usize = 55; // (out_ptr, max) → count; writes TaskRecord × n
// TCP socket syscalls
const SYS_TCP_CONNECT:       usize = 60; // (ip_u32, port, timeout_ms) → conn_id ≥0 or -1
const SYS_TCP_LISTEN:        usize = 61; // (port) → listener_id ≥0 or -1
const SYS_TCP_ACCEPT:        usize = 62; // (listener_id, timeout_ms) → conn_id ≥0 or -1
const SYS_TCP_WRITE:         usize = 63; // (conn_id, buf_ptr, len) → bytes or -1
const SYS_TCP_READ:          usize = 64; // (conn_id, buf_ptr, len) → bytes or -1
const SYS_TCP_WAIT_READABLE: usize = 65; // (conn_id, timeout_ms) → 0 ok / -1
const SYS_TCP_CLOSE:         usize = 66; // (conn_id) → 0
const SYS_SURF_RESIZE: usize = 67; // (surface_id, new_w, new_h) → 0 ok / -1
const SYS_SURF_RAISE:  usize = 68; // (surface_id) → 0
const SYS_SURF_LOWER:  usize = 69; // (surface_id) → 0
const SYS_SURF_DIRTY:  usize = 70; // (surface_id) → 0
const SYS_SPEAK:       usize = 71; // (text_ptr, text_len, 0) → 0 ok / -1 (TTS via HDA)
const SYS_PIPE:        usize = 45;  // (out_fds: *mut [u32;2]) → 0 ok / -1
const SYS_GETC:        usize = 100; // () → raw char (no echo, blocks)

// ── Semantic service TID and opcodes ──────────────────────────────────────────
const SEM_TID:       usize = 2;
const OP_STORE:      u64   = 100;
const OP_RETRIEVE:   u64   = 101;
const OP_SEARCH_TAG: u64   = 103;
const OP_QUERY:      u64   = 104;

// ── Capability bits (must match kernel/src/syscall/mod.rs) ────────────────────
const CAP_SEND: u64 = 1 << 0;
const CAP_RECV: u64 = 1 << 1;
const CAP_AI:   u64 = 1 << 3;
const CAP_LOG:  u64 = 1 << 4;
const CAP_MEM:  u64 = 1 << 5;
const CAP_NET:  u64 = 1 << 6;
const CAP_USER_DEFAULT: u64 = CAP_SEND | CAP_RECV | CAP_AI | CAP_LOG | CAP_MEM | CAP_NET;

// ── AArch64 CRT0 ──────────────────────────────────────────────────────────────
// Clear BSS, call ash_main; loop on SYS_EXIT if ash_main ever returns.
global_asm!(
    ".section .text.ash_entry, \"ax\"",
    ".global ash_entry",
    "ash_entry:",
    // Zero BSS
    "    adrp x0, __bss_start",
    "    add  x0, x0, :lo12:__bss_start",
    "    adrp x1, __bss_end",
    "    add  x1, x1, :lo12:__bss_end",
    "0:  cmp  x0, x1",
    "    b.ge 1f",
    "    str  xzr, [x0], #8",
    "    b    0b",
    // Call the Rust shell
    "1:  bl   ash_main",
    // Fallthrough to SYS_EXIT (ash_main is noreturn)
    "2:  mov  x8, #4",
    "    svc  #0",
    "    b    2b",
);

// ── Raw syscall wrappers ───────────────────────────────────────────────────────

#[inline(always)]
unsafe fn syscall_0(n: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "svc #0",
        in("x8") n,
        lateout("x0") ret,
        clobber_abi("system"),
    );
    ret
}

#[inline(always)]
unsafe fn syscall_1(n: usize, a0: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "svc #0",
        in("x8") n,
        in("x0") a0,
        lateout("x0") ret,
        clobber_abi("system"),
    );
    ret
}

#[inline(always)]
unsafe fn syscall_2(n: usize, a0: usize, a1: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "svc #0",
        in("x8") n,
        in("x0") a0,
        in("x1") a1,
        lateout("x0") ret,
        clobber_abi("system"),
    );
    ret
}

#[inline(always)]
unsafe fn syscall_3(n: usize, a0: usize, a1: usize, a2: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "svc #0",
        in("x8") n,
        in("x0") a0,
        in("x1") a1,
        in("x2") a2,
        lateout("x0") ret,
        clobber_abi("system"),
    );
    ret
}

#[inline(always)]
unsafe fn syscall_4(n: usize, a0: usize, a1: usize, a2: usize, a3: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "svc #0",
        in("x8") n,
        in("x0") a0,
        in("x1") a1,
        in("x2") a2,
        in("x3") a3,
        lateout("x0") ret,
        clobber_abi("system"),
    );
    ret
}

#[inline(always)]
unsafe fn syscall_6(n: usize, a0: usize, a1: usize, a2: usize, a3: usize,
                    a4: usize, a5: usize) -> usize {
    let ret: usize;
    core::arch::asm!(
        "svc #0",
        in("x8") n,
        in("x0") a0,
        in("x1") a1,
        in("x2") a2,
        in("x3") a3,
        in("x4") a4,
        in("x5") a5,
        lateout("x0") ret,
        clobber_abi("system"),
    );
    ret
}

// ── File syscall helpers ───────────────────────────────────────────────────────

unsafe fn file_create(path: &str) -> isize {
    syscall_2(SYS_CREATE, path.as_ptr() as usize, path.len()) as isize
}

unsafe fn file_write(fd: usize, buf: &[u8]) -> isize {
    syscall_3(SYS_WRITE_FD, fd, buf.as_ptr() as usize, buf.len()) as isize
}

unsafe fn file_close(fd: usize) {
    syscall_1(SYS_CLOSE, fd);
}

// ── UART output ───────────────────────────────────────────────────────────────

// ── Capture mode — redirects print() output to a buffer for shell pipes ───────

// ── Background job table ───────────────────────────────────────────────────────
const MAX_JOBS: usize = 8;
static mut JOB_TIDS: [usize; MAX_JOBS] = [0; MAX_JOBS];
static mut JOB_COUNT: usize = 0;

fn job_add(tid: usize) {
    unsafe {
        if JOB_COUNT < MAX_JOBS {
            JOB_TIDS[JOB_COUNT] = tid;
            JOB_COUNT += 1;
        }
    }
}

fn cmd_jobs() {
    unsafe {
        if JOB_COUNT == 0 {
            println("No background jobs.");
            return;
        }
        for i in 0..JOB_COUNT {
            print("["); render::write_dec(&mut UartSink, i as u32);
            print("] TID "); render::write_dec(&mut UartSink, JOB_TIDS[i] as u32);
            println("  running");
        }
    }
}

fn cmd_wait_job(tid: usize) {
    if tid == 0 {
        println("Usage: wait <tid>");
        return;
    }
    let code = unsafe { syscall_1(SYS_WAIT, tid) as isize };
    print("TID "); render::write_dec(&mut UartSink, tid as u32);
    print(" exited with code "); render::write_dec(&mut UartSink, code as u32);
    println("");
    // Remove from job table
    unsafe {
        let mut i = 0;
        while i < JOB_COUNT {
            if JOB_TIDS[i] == tid {
                JOB_TIDS[i] = JOB_TIDS[JOB_COUNT - 1];
                JOB_COUNT -= 1;
            } else {
                i += 1;
            }
        }
    }
}

fn cmd_speak(text: &str) {
    if text.is_empty() {
        println("Usage: speak <text>");
        return;
    }
    let ret = unsafe { syscall_2(SYS_SPEAK, text.as_ptr() as usize, text.len()) as isize };
    if ret < 0 {
        println("[speak] HDA TTS unavailable.");
    }
    // No need to print anything on success — audio is the output
}

fn cmd_bg_exec(cmd: &str) {
    // Strip trailing '&' if present and trim
    let cmd = cmd.trim_end_matches('&').trim();
    if cmd.is_empty() {
        println("Usage: <cmd> &");
        return;
    }
    // Only `exec <path>` is supported as a background command
    let (verb, rest) = aska::intent::split_first_word(cmd);
    let path = match verb {
        "exec" | "execw" => rest.trim(),
        _ => cmd,  // treat whole thing as a path
    };
    if path.is_empty() {
        println("bg: nothing to run");
        return;
    }
    let tid = unsafe {
        syscall_3(SYS_EXEC, path.as_ptr() as usize, path.len(), CAP_USER_DEFAULT as usize) as isize
    };
    if tid < 0 {
        print("bg: failed to launch '"); print(path); println("'");
        return;
    }
    job_add(tid as usize);
    print("["); render::write_dec(&mut UartSink, unsafe { JOB_COUNT - 1 } as u32);
    print("] TID "); render::write_dec(&mut UartSink, tid as u32);
    println("  (background)");
}

/// When true, `print()` writes to CAPTURE_BUF instead of UART.
static mut CAPTURING: bool = false;
static mut CAPTURE_BUF: [u8; 8192] = [0; 8192];
static mut CAPTURE_LEN: usize = 0;

/// Start capturing all print() output.
fn capture_start() {
    unsafe { CAPTURING = true; CAPTURE_LEN = 0; }
}

/// Stop capturing; returns a slice of the captured bytes.
fn capture_end() -> &'static [u8] {
    unsafe { CAPTURING = false; &CAPTURE_BUF[..CAPTURE_LEN] }
}

fn print(s: &str) {
    unsafe {
        if CAPTURING {
            let b = s.as_bytes();
            let space = CAPTURE_BUF.len() - CAPTURE_LEN;
            let n = b.len().min(space);
            CAPTURE_BUF[CAPTURE_LEN..CAPTURE_LEN + n].copy_from_slice(&b[..n]);
            CAPTURE_LEN += n;
        } else {
            syscall_2(SYS_LOG, s.as_ptr() as usize, s.len());
        }
    }
}

fn println(s: &str) {
    print(s);
    print("\r\n");
}

/// Zero-size UART sink — bridges `print()` into the aska::render::Sink API.
struct UartSink;

impl Sink for UartSink {
    fn write_str(&mut self, s: &str) { print(s); }
}

// ── IPC message (must match kernel ipc::Message layout) ───────────────────────
#[repr(C)]
struct Message {
    sender: usize,
    data:   [u8; 32],
}

// ── DirEntry (must match kernel/src/fs/mod.rs exactly) ────────────────────────
// Layout (repr(C)):
//   0   name[256]   — NUL-terminated filename
//  256  is_dir  u8
//  257  _pad[2] u8
//  260  size    u32
// Total: 264 bytes
const NAME_MAX: usize = 255;

#[repr(C)]
#[derive(Copy, Clone)]
struct DirEntry {
    name:   [u8; NAME_MAX + 1],
    is_dir: u8,
    _pad:   [u8; 2],
    size:   u32,
}

impl DirEntry {
    const fn zeroed() -> Self {
        Self { name: [0u8; NAME_MAX + 1], is_dir: 0, _pad: [0; 2], size: 0 }
    }

    fn name_str(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
        core::str::from_utf8(&self.name[..len]).unwrap_or("<?>")
    }
}

// ── UI Syscalls ───────────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InputEvent {
    pub type_: u16,
    pub code: u16,
    pub value: u32,
}

unsafe fn sys_fb_info() -> (u32, u32, u32) {
    let mut w = 0; let mut h = 0; let mut p = 0;
    syscall_3(SYS_FB_INFO, &mut w as *mut _ as usize, &mut h as *mut _ as usize, &mut p as *mut _ as usize);
    (w, h, p)
}

unsafe fn sys_fb_map() -> *mut u32 {
    syscall_0(SYS_FB_MAP) as *mut u32
}

unsafe fn sys_fb_flush() {
    syscall_0(SYS_FB_FLUSH);
}

// ── Compositor syscall wrappers ────────────────────────────────────────────────

#[allow(dead_code)]
unsafe fn surf_create(w: u32, h: u32, z: u8) -> isize {
    syscall_3(SYS_SURF_CREATE, w as usize, h as usize, z as usize) as isize
}

#[allow(dead_code)]
unsafe fn surf_destroy(id: usize) {
    syscall_1(SYS_SURF_DESTROY, id);
}

#[allow(dead_code)]
unsafe fn surf_map(id: usize) -> usize {
    syscall_1(SYS_SURF_MAP, id)
}

#[allow(dead_code)]
unsafe fn surf_flush(id: usize) {
    syscall_1(SYS_SURF_FLUSH, id);
}

#[allow(dead_code)]
unsafe fn surf_move(id: usize, x: i32, y: i32) {
    syscall_3(SYS_SURF_MOVE, id, x as usize, y as usize);
}

#[allow(dead_code)]
pub unsafe fn surf_resize(id: usize, w: u32, h: u32) -> bool {
    syscall_3(SYS_SURF_RESIZE, id, w as usize, h as usize) as isize >= 0
}

#[allow(dead_code)]
pub unsafe fn surf_raise(id: usize) {
    syscall_1(SYS_SURF_RAISE, id);
}

#[allow(dead_code)]
pub unsafe fn surf_lower(id: usize) {
    syscall_1(SYS_SURF_LOWER, id);
}

#[allow(dead_code)]
pub unsafe fn surf_mark_dirty(id: usize) {
    syscall_1(SYS_SURF_DIRTY, id);
}

/// Allocate a kernel pipe. Returns `Some((read_fd, write_fd))` or `None`.
#[allow(dead_code)]
pub fn sys_pipe() -> Option<(usize, usize)> {
    let mut fds = [0u32; 2];
    let ret = unsafe { syscall_1(SYS_PIPE, fds.as_mut_ptr() as usize) as isize };
    if ret == 0 { Some((fds[0] as usize, fds[1] as usize)) } else { None }
}

// ── Readline — arrow-key history + tab completion ─────────────────────────────

/// Read one raw char from UART (no echo, blocking).
#[inline(always)]
fn rl_getc() -> u8 {
    unsafe { syscall_0(SYS_GETC) as u8 }
}

/// ANSI cursor-back n columns (ESC [ n D).
fn rl_cursor_back(n: usize) {
    if n == 0 { return; }
    let mut s = [0u8; 12];
    s[0] = 0x1b; s[1] = b'[';
    let mut p = 2usize;
    let n = n.min(9999);
    if n >= 1000 { s[p] = b'0' + (n / 1000) as u8; p += 1; }
    if n >= 100  { s[p] = b'0' + ((n / 100) % 10) as u8; p += 1; }
    if n >= 10   { s[p] = b'0' + ((n / 10)  % 10) as u8; p += 1; }
    s[p] = b'0' + (n % 10) as u8; p += 1;
    s[p] = b'D'; p += 1;
    unsafe { syscall_2(SYS_LOG, s.as_ptr() as usize, p) };
}

/// ANSI cursor-forward n columns (ESC [ n C).
fn rl_cursor_fwd(n: usize) {
    if n == 0 { return; }
    let mut s = [0u8; 12];
    s[0] = 0x1b; s[1] = b'[';
    let mut p = 2usize;
    let n = n.min(9999);
    if n >= 1000 { s[p] = b'0' + (n / 1000) as u8; p += 1; }
    if n >= 100  { s[p] = b'0' + ((n / 100) % 10) as u8; p += 1; }
    if n >= 10   { s[p] = b'0' + ((n / 10)  % 10) as u8; p += 1; }
    s[p] = b'0' + (n % 10) as u8; p += 1;
    s[p] = b'C'; p += 1;
    unsafe { syscall_2(SYS_LOG, s.as_ptr() as usize, p) };
}

/// Erase the current terminal line and redraw the prompt + line content,
/// leaving the cursor at `cur` columns after the end of the prompt.
/// The prompt is always "aska > " (7 visible chars).
fn rl_redraw(line: &[u8], cur: usize) {
    print("\r\x1b[2K");
    print("\x1b[1;32maska\x1b[0m \x1b[90m>\x1b[0m ");
    unsafe { syscall_2(SYS_LOG, line.as_ptr() as usize, line.len()) };
    let back = line.len() - cur;
    rl_cursor_back(back);
}

/// Commands available for Tab completion.
const RL_COMMANDS: &[&[u8]] = &[
    b"help", b"?",
    b"ls", b"dir", b"cat", b"type", b"exec", b"execw", b"save",
    b"mem", b"memory",
    b"net", b"ifconfig", b"ping", b"dns", b"nslookup",
    b"fetch", b"curl", b"wget", b"download", b"post",
    b"nc", b"connect", b"telnet", b"listen",
    b"reset", b"install",
    b"wasm", b"wasmrun",
    b"cap", b"caps", b"capctl",
    b"speak", b"say", b"tts",
    b"jobs", b"wait",
    b"exit", b"quit", b"q",
];

/// Perform Tab completion on the first word in `line[..*len]`.
/// Completes in-place; updates `len` and `cur`.
fn rl_complete(line: &mut [u8; 256], len: &mut usize, cur: &mut usize) {
    // Only complete the command word (before the first space).
    let word_end = line[..*cur].iter().position(|&b| b == b' ').unwrap_or(*cur);
    let prefix   = &line[..word_end];

    let mut hits = [0usize; 32];
    let mut n    = 0usize;
    for (i, cmd) in RL_COMMANDS.iter().enumerate() {
        if cmd.len() >= prefix.len() && cmd[..prefix.len()] == *prefix {
            if n < 32 { hits[n] = i; n += 1; }
        }
    }

    if n == 0 {
        print("\x07"); // bell
    } else if n == 1 {
        let cmd = RL_COMMANDS[hits[0]];
        let tail_start = prefix.len();
        // Copy remainder of completion after the prefix
        for (i, &b) in cmd[tail_start..].iter().enumerate() {
            if *len + i < 255 { line[*len + i] = b; }
        }
        let added = cmd.len() - tail_start;
        *len += added;
        // Append a space after completion if not already present
        if *len < 255 { line[*len] = b' '; *len += 1; }
        *cur = *len;
        rl_redraw(&line[..*len], *cur);
    } else {
        // Show all matches on a new line, then redraw.
        print("\r\n");
        for i in 0..n {
            let cmd = RL_COMMANDS[hits[i]];
            unsafe { syscall_2(SYS_LOG, cmd.as_ptr() as usize, cmd.len()) };
            print("  ");
        }
        print("\r\n");
        rl_redraw(&line[..*len], *cur);
    }
}

/// Full readline: read one line from UART with history navigation and tab
/// completion.  Writes the result into `buf`; returns the number of bytes.
/// Caller should call `state.push_history(result)` after this returns.
fn readline(buf: &mut [u8], state: &aska::shell::ShellState) -> usize {
    let max = buf.len().min(255);
    let mut line     = [0u8; 256];
    let mut len      = 0usize;
    let mut cur      = 0usize;  // cursor column within line
    let mut hist_idx: Option<usize> = None; // None = editing new; Some(i) = history
    let mut saved    = [0u8; 256];
    let mut saved_len = 0usize;

    loop {
        let b = rl_getc();
        match b {
            // ── Enter ────────────────────────────────────────────────────────
            b'\r' | b'\n' => {
                print("\r\n");
                let n = len.min(max);
                buf[..n].copy_from_slice(&line[..n]);
                return n;
            }

            // ── Backspace / DEL ──────────────────────────────────────────────
            0x7F | 0x08 => {
                if cur > 0 {
                    for i in cur - 1..len - 1 { line[i] = line[i + 1]; }
                    len -= 1; cur -= 1;
                    rl_redraw(&line[..len], cur);
                }
            }

            // ── Escape sequences ─────────────────────────────────────────────
            0x1b => {
                let b2 = rl_getc();
                if b2 != b'[' { continue; }
                let b3 = rl_getc();
                match b3 {
                    b'A' => { // Up arrow — go back in history
                        let next = hist_idx.map_or(0, |i| i + 1);
                        if let Some(entry) = state.history_entry(next) {
                            if hist_idx.is_none() {
                                saved[..len].copy_from_slice(&line[..len]);
                                saved_len = len;
                            }
                            hist_idx = Some(next);
                            let n = entry.len().min(255);
                            line[..n].copy_from_slice(&entry[..n]);
                            len = n; cur = n;
                            rl_redraw(&line[..len], cur);
                        }
                    }
                    b'B' => { // Down arrow — go forward
                        match hist_idx {
                            None => {}
                            Some(0) => {
                                hist_idx = None;
                                line[..saved_len].copy_from_slice(&saved[..saved_len]);
                                len = saved_len; cur = saved_len;
                                rl_redraw(&line[..len], cur);
                            }
                            Some(i) => {
                                let prev = i - 1;
                                if let Some(entry) = state.history_entry(prev) {
                                    hist_idx = Some(prev);
                                    let n = entry.len().min(255);
                                    line[..n].copy_from_slice(&entry[..n]);
                                    len = n; cur = n;
                                    rl_redraw(&line[..len], cur);
                                }
                            }
                        }
                    }
                    b'C' => { // Right arrow
                        if cur < len { cur += 1; rl_cursor_fwd(1); }
                    }
                    b'D' => { // Left arrow
                        if cur > 0  { cur -= 1; rl_cursor_back(1); }
                    }
                    b'3' => { // DEL (ESC [ 3 ~)
                        let _ = rl_getc(); // consume '~'
                        if cur < len {
                            for i in cur..len - 1 { line[i] = line[i + 1]; }
                            len -= 1;
                            rl_redraw(&line[..len], cur);
                        }
                    }
                    b'H' => { // Home
                        rl_cursor_back(cur); cur = 0;
                    }
                    b'F' => { // End
                        rl_cursor_fwd(len - cur); cur = len;
                    }
                    _ => {}
                }
            }

            // ── Tab — completion ─────────────────────────────────────────────
            0x09 => {
                rl_complete(&mut line, &mut len, &mut cur);
            }

            // ── Control keys ─────────────────────────────────────────────────
            0x01 => { rl_cursor_back(cur); cur = 0; }        // Ctrl-A home
            0x05 => { rl_cursor_fwd(len - cur); cur = len; } // Ctrl-E end
            0x0B => { // Ctrl-K kill to end
                if cur < len { len = cur; rl_redraw(&line[..len], cur); }
            }
            0x15 => { // Ctrl-U kill to beginning
                if cur > 0 {
                    let tail = len - cur;
                    for i in 0..tail { line[i] = line[i + cur]; }
                    len = tail; cur = 0;
                    rl_redraw(&line[..len], cur);
                }
            }
            0x03 => { // Ctrl-C
                print("^C\r\n");
                return 0;
            }

            // ── Printable character ──────────────────────────────────────────
            b if b >= 0x20 && b < 0x7F => {
                if len < max {
                    if cur == len {
                        // Append at end — just echo.
                        line[len] = b; len += 1; cur += 1;
                        let ch = [b];
                        unsafe { syscall_2(SYS_LOG, ch.as_ptr() as usize, 1) };
                    } else {
                        // Insert in middle — shift right, redraw.
                        for i in (cur..len).rev() { line[i + 1] = line[i]; }
                        line[cur] = b; len += 1; cur += 1;
                        rl_redraw(&line[..len], cur);
                    }
                }
            }

            _ => {} // ignore everything else
        }
    }
}

unsafe fn sys_input_poll(events: &mut [InputEvent]) -> usize {
    syscall_2(SYS_INPUT_POLL, events.as_mut_ptr() as usize, events.len())
}

pub fn sys_ping(ip_packed: u32, timeout_ms: u32) -> isize {
    unsafe { syscall_2(SYS_PING, ip_packed as usize, timeout_ms as usize) as isize }
}

pub fn sys_http_get(url: &str, buf: &mut [u8]) -> isize {
    unsafe { syscall_4(SYS_HTTP_GET, url.as_ptr() as usize, url.len(), buf.as_mut_ptr() as usize, buf.len()) as isize }
}

pub fn sys_https_get(url: &str, buf: &mut [u8]) -> isize {
    unsafe { syscall_4(SYS_HTTPS_GET, url.as_ptr() as usize, url.len(), buf.as_mut_ptr() as usize, buf.len()) as isize }
}

pub fn sys_https_post(url: &str, body: &[u8], buf: &mut [u8]) -> isize {
    unsafe {
        syscall_6(SYS_HTTPS_POST,
            url.as_ptr() as usize, url.len(),
            body.as_ptr() as usize, body.len(),
            buf.as_mut_ptr() as usize, buf.len()) as isize
    }
}

pub fn sys_dns_resolve(name: &str, out_ip: &mut u32) -> isize {
    unsafe { syscall_3(SYS_DNS_RESOLVE, name.as_ptr() as usize, name.len(), out_ip as *mut u32 as usize) as isize }
}

// ── TCP socket helpers ────────────────────────────────────────────────────────
// IP is passed as a big-endian packed u32 (e.g. 10.0.2.15 = 0x0A00_020F).

pub fn sys_tcp_connect(ip: u32, port: u16, timeout_ms: u32) -> isize {
    unsafe { syscall_3(SYS_TCP_CONNECT, ip as usize, port as usize, timeout_ms as usize) as isize }
}

pub fn sys_tcp_listen(port: u16) -> isize {
    unsafe { syscall_1(SYS_TCP_LISTEN, port as usize) as isize }
}

pub fn sys_tcp_accept(listener: usize, timeout_ms: u32) -> isize {
    unsafe { syscall_2(SYS_TCP_ACCEPT, listener, timeout_ms as usize) as isize }
}

pub fn sys_tcp_write(conn: usize, data: &[u8]) -> isize {
    unsafe { syscall_3(SYS_TCP_WRITE, conn, data.as_ptr() as usize, data.len()) as isize }
}

pub fn sys_tcp_read(conn: usize, buf: &mut [u8]) -> isize {
    unsafe { syscall_3(SYS_TCP_READ, conn, buf.as_mut_ptr() as usize, buf.len()) as isize }
}

pub fn sys_tcp_wait_readable(conn: usize, timeout_ms: u32) -> isize {
    unsafe { syscall_2(SYS_TCP_WAIT_READABLE, conn, timeout_ms as usize) as isize }
}

pub fn sys_tcp_close(conn: usize) {
    unsafe { syscall_1(SYS_TCP_CLOSE, conn); }
}

// ── Syscall helpers ────────────────────────────────────────────────────────────

unsafe fn sys_yield() {
    syscall_0(SYS_YIELD);
}

unsafe fn sys_exit() -> ! {
    syscall_0(SYS_EXIT);
    loop {}
}

unsafe fn sys_send_msg(dest: usize, op: u64, data_ptr: *const u8, len: usize) -> isize {
    let mut msg = Message { sender: 0, data: [0; 32] };
    msg.data[0..8].copy_from_slice(&op.to_le_bytes());
    msg.data[8..16].copy_from_slice(&(data_ptr as u64).to_le_bytes());
    msg.data[16..24].copy_from_slice(&(len as u64).to_le_bytes());
    syscall_2(SYS_SEND, dest, &msg as *const _ as usize) as isize
}

// ── Semantic IPC helpers ───────────────────────────────────────────────────────

fn sem_send(data: [u8; 32]) {
    let msg = Message { sender: 0, data };
    unsafe { syscall_2(SYS_SEND, SEM_TID, &msg as *const _ as usize) };
}

fn sem_recv() -> [u8; 32] {
    let mut msg = Message { sender: 0, data: [0; 32] };
    loop {
        let ret = unsafe { syscall_1(SYS_RECV, &mut msg as *mut _ as usize) as isize };
        if ret == 0 && msg.sender == SEM_TID {
            return msg.data;
        }
        unsafe { sys_yield() };
    }
}

// ── Built-in: ls ──────────────────────────────────────────────────────────────

fn cmd_ls(path: &str) {
    let dir = if path.is_empty() { "/" } else { path };

    const EMPTY: DirEntry = DirEntry::zeroed();
    let mut entries = [EMPTY; 64];

    let count = unsafe {
        syscall_3(
            SYS_READDIR,
            dir.as_ptr() as usize,
            dir.len(),
            entries.as_mut_ptr() as usize,
        ) as isize
    };

    if count < 0 {
        print("ls: '"); print(dir); println("': not found");
        return;
    }
    if count == 0 { println("(empty)"); return; }

    let mut s = UartSink;
    for i in 0..count as usize {
        let e = &entries[i];
        let name = e.name_str();
        if e.is_dir != 0 {
            print(render::color::BLUE);
            print(name);
            print("/");
            print(render::color::RESET);
        } else {
            print(name);
        }
        // Right-align size at column 28
        let width = name.len() + if e.is_dir != 0 { 1 } else { 0 };
        let pad = if width < 28 { 28 - width } else { 2 };
        for _ in 0..pad { print(" "); }
        if e.is_dir != 0 {
            print(render::color::DIM);
            print("<dir>");
            print(render::color::RESET);
        } else {
            render::write_size(&mut s, e.size);
        }
        println("");
    }
}

// ── Built-in: cat ─────────────────────────────────────────────────────────────

fn cmd_cat(path: &str) {
    if path.is_empty() { println("Usage: cat <path>"); return; }

    let fd = unsafe {
        syscall_2(SYS_OPEN, path.as_ptr() as usize, path.len()) as isize
    };
    if fd < 0 {
        print("cat: '"); print(path); println("': not found");
        return;
    }

    let mut buf = [0u8; 512];
    let mut any = false;
    loop {
        let n = unsafe {
            syscall_3(SYS_READ_FD, fd as usize, buf.as_mut_ptr() as usize, buf.len()) as isize
        };
        if n <= 0 { break; }
        any = true;
        let slice = &buf[..n as usize];
        if let Ok(s) = core::str::from_utf8(slice) {
            print(s);
        } else {
            print("[binary: ");
            render::write_dec(&mut UartSink, n as u32);
            println(" bytes]");
            break;
        }
    }
    if any { println(""); }

    unsafe { syscall_1(SYS_CLOSE, fd as usize); }
}

// ── Built-in: exec / execw ────────────────────────────────────────────────────

fn cmd_exec(path: &str, wait: bool) {
    if path.is_empty() {
        println("Usage: exec <path>   (execw to wait for exit)");
        return;
    }

    let tid = unsafe {
        syscall_3(
            SYS_EXEC,
            path.as_ptr() as usize,
            path.len(),
            CAP_USER_DEFAULT as usize,
        ) as isize
    };
    if tid < 0 {
        print("exec: failed to launch '"); print(path); println("'");
        return;
    }

    print("[exec] launched TID ");
    render::write_dec(&mut UartSink, tid as u32);
    println("");

    if wait {
        let code = unsafe { syscall_1(SYS_WAIT, tid as usize) as isize };
        print("[exec] TID ");
        render::write_dec(&mut UartSink, tid as u32);
        print(" exited with code ");
        render::write_dec(&mut UartSink, code as u32);
        println("");
    }
}

// ── Built-in: install ─────────────────────────────────────────────────────────

fn cmd_install() {
    let path = "/installer";
    let tid = unsafe {
        syscall_3(
            SYS_EXEC,
            path.as_ptr() as usize,
            path.len(),
            !0, // CAP_ALL
        ) as isize
    };
    if tid < 0 {
        println("install: failed to launch '/installer'");
        return;
    }
    unsafe { syscall_1(SYS_WAIT, tid as usize) };
}

// ── Built-in: mem ─────────────────────────────────────────────────────────────

fn mem_store(text: &str) {
    if text.is_empty() { println("mem store: provide text to store"); return; }

    let mut hash = [0u8; 32];
    let mut req  = [0u8; 32];
    req[0..8].copy_from_slice(&OP_STORE.to_le_bytes());
    req[8..16].copy_from_slice(&(text.as_ptr() as u64).to_le_bytes());
    req[16..24].copy_from_slice(&(text.len() as u64).to_le_bytes());
    req[24..32].copy_from_slice(&(hash.as_mut_ptr() as u64).to_le_bytes());
    sem_send(req);
    let reply = sem_recv();

    let status = u64::from_le_bytes(reply[0..8].try_into().unwrap_or([0; 8]));
    if status == 0 {
        render::ok(&mut UartSink, "[Memory] Stored.");
    } else {
        render::err(&mut UartSink, "[Memory] Store failed.");
    }
}

fn mem_query(query: &str) {
    if query.is_empty() { println("mem query: provide search text"); return; }

    let mut hash = [0u8; 32];
    let mut req  = [0u8; 32];
    req[0..8].copy_from_slice(&OP_QUERY.to_le_bytes());
    req[8..16].copy_from_slice(&(query.as_ptr() as u64).to_le_bytes());
    req[16..24].copy_from_slice(&(query.len() as u64).to_le_bytes());
    req[24..32].copy_from_slice(&(hash.as_mut_ptr() as u64).to_le_bytes());
    sem_send(req);
    let reply = sem_recv();

    let status = u64::from_le_bytes(reply[0..8].try_into().unwrap_or([0; 8]));
    let count  = u64::from_le_bytes(reply[8..16].try_into().unwrap_or([0; 8]));

    if status != 0 || count == 0 {
        render::info(&mut UartSink, "[Memory] Nothing found.");
        return;
    }

    let mut out  = [0u8; 256];
    let mut req2 = [0u8; 32];
    req2[0..8].copy_from_slice(&OP_RETRIEVE.to_le_bytes());
    req2[8..16].copy_from_slice(&(hash.as_ptr() as u64).to_le_bytes());
    req2[16..24].copy_from_slice(&(out.as_mut_ptr() as u64).to_le_bytes());
    req2[24..32].copy_from_slice(&(out.len() as u64).to_le_bytes());
    sem_send(req2);
    let reply2 = sem_recv();

    let st2   = u64::from_le_bytes(reply2[0..8].try_into().unwrap_or([0; 8]));
    let bytes = u64::from_le_bytes(reply2[8..16].try_into().unwrap_or([0; 8])) as usize;
    if st2 == 0 && bytes > 0 {
        render::notice(&mut UartSink, core::str::from_utf8(&out[..bytes]).unwrap_or("<binary>"));
    } else {
        render::err(&mut UartSink, "[Memory] Retrieve failed.");
    }
}

fn mem_search(tag: &str) {
    if tag.is_empty() { println("mem search: provide a tag"); return; }

    let mut hash = [0u8; 32];
    let mut req  = [0u8; 32];
    req[0..8].copy_from_slice(&OP_SEARCH_TAG.to_le_bytes());
    req[8..16].copy_from_slice(&(tag.as_ptr() as u64).to_le_bytes());
    req[16..24].copy_from_slice(&(tag.len() as u64).to_le_bytes());
    req[24..32].copy_from_slice(&(hash.as_mut_ptr() as u64).to_le_bytes());
    sem_send(req);
    let reply = sem_recv();

    let status = u64::from_le_bytes(reply[0..8].try_into().unwrap_or([0; 8]));
    let count  = u64::from_le_bytes(reply[8..16].try_into().unwrap_or([0; 8]));

    if status != 0 || count == 0 {
        render::info(&mut UartSink, "[Memory] No entries with that tag.");
        return;
    }

    print("[Memory] Found ");
    render::write_dec(&mut UartSink, count as u32);
    println(if count == 1 { " entry." } else { " entries." });

    let mut out  = [0u8; 256];
    let mut req2 = [0u8; 32];
    req2[0..8].copy_from_slice(&OP_RETRIEVE.to_le_bytes());
    req2[8..16].copy_from_slice(&(hash.as_ptr() as u64).to_le_bytes());
    req2[16..24].copy_from_slice(&(out.as_mut_ptr() as u64).to_le_bytes());
    req2[24..32].copy_from_slice(&(out.len() as u64).to_le_bytes());
    sem_send(req2);
    let reply2 = sem_recv();

    let st2   = u64::from_le_bytes(reply2[0..8].try_into().unwrap_or([0; 8]));
    let bytes = u64::from_le_bytes(reply2[8..16].try_into().unwrap_or([0; 8])) as usize;
    if st2 == 0 && bytes > 0 {
        print(render::color::DIM);
        print("  -> ");
        print(render::color::RESET);
        println(core::str::from_utf8(&out[..bytes]).unwrap_or("<binary>"));
    }
}

// ── Tool-call execution (AI-initiated, embedded in LLM response) ──────────────

/// Execute a tool call of the form `OP:args` and write the result into `out`.
/// Returns the number of bytes written.
fn execute_tool_to_buf(cmd: &str, out: &mut [u8]) -> usize {
    // cmd is the interior of [[...]], e.g. "FETCH:https://example.com"
    let colon = cmd.find(':').unwrap_or(cmd.len());
    let op   = cmd[..colon].trim();
    let args = if colon < cmd.len() { cmd[colon + 1..].trim() } else { "" };

    match op {
        // ── Network ──────────────────────────────────────────────────────────
        "FETCH" | "GET" => {
            let ret = if args.starts_with("https://") {
                sys_https_get(args, out)
            } else {
                sys_http_get(args, out)
            };
            if ret < 0 {
                let e = b"error: fetch failed";
                let n = e.len().min(out.len());
                out[..n].copy_from_slice(&e[..n]); n
            } else { ret as usize }
        }
        "POST" => {
            let (url, body) = split_first_word(args);
            let ret = sys_https_post(url, body.as_bytes(), out);
            if ret < 0 {
                let e = b"error: post failed";
                let n = e.len().min(out.len());
                out[..n].copy_from_slice(&e[..n]); n
            } else { ret as usize }
        }
        "DNS" | "RESOLVE" => {
            let mut ip: u32 = 0;
            let ret = sys_dns_resolve(args, &mut ip);
            if ret < 0 || ip == 0 {
                copy_into(out, b"error: could not resolve")
            } else {
                let octets = [(ip >> 24) as u8, (ip >> 16) as u8, (ip >> 8) as u8, ip as u8];
                let mut p = 0usize;
                for (i, b) in octets.iter().enumerate() {
                    let d = fmt_dec(*b as u32);
                    let n = d.len().min(out.len() - p);
                    out[p..p+n].copy_from_slice(&d.as_bytes()[..n]); p += n;
                    if i < 3 && p < out.len() { out[p] = b'.'; p += 1; }
                }
                p
            }
        }
        "PING" => {
            let mut ip: u32 = 0;
            sys_dns_resolve(args, &mut ip);
            if ip == 0 { return copy_into(out, b"error: could not resolve"); }
            let rtt = unsafe {
                syscall_2(SYS_PING, ip as usize, 3000) as isize
            };
            if rtt < 0 {
                copy_into(out, b"timeout")
            } else {
                let msg = b"rtt ";
                let n = msg.len().min(out.len());
                out[..n].copy_from_slice(&msg[..n]);
                let mut p = n;
                let d = fmt_dec(rtt as u32);
                let dn = d.len().min(out.len() - p);
                out[p..p+dn].copy_from_slice(&d.as_bytes()[..dn]); p += dn;
                if p < out.len() { out[p] = b'm'; p += 1; }
                if p < out.len() { out[p] = b's'; p += 1; }
                p
            }
        }
        // ── Filesystem ───────────────────────────────────────────────────────
        "LS" | "LIST" => {
            // Read directory listing via SYS_OPEN+SYS_READ_FD on "/"
            let path = if args.is_empty() { "/" } else { args };
            let fd = unsafe { syscall_2(SYS_OPEN, path.as_ptr() as usize, path.len()) as isize };
            if fd < 0 { return copy_into(out, b"error: cannot open dir"); }
            let n = unsafe { syscall_3(SYS_READ_FD, fd as usize, out.as_mut_ptr() as usize, out.len()) as isize };
            unsafe { syscall_1(SYS_CLOSE, fd as usize); }
            if n < 0 { 0 } else { n as usize }
        }
        "CAT" | "READ" => {
            let fd = unsafe { syscall_2(SYS_OPEN, args.as_ptr() as usize, args.len()) as isize };
            if fd < 0 { return copy_into(out, b"error: file not found"); }
            let n = unsafe { syscall_3(SYS_READ_FD, fd as usize, out.as_mut_ptr() as usize, out.len()) as isize };
            unsafe { syscall_1(SYS_CLOSE, fd as usize); }
            if n < 0 { 0 } else { n as usize }
        }
        "SAVE" | "WRITE" => {
            let (path, content) = split_first_word(args);
            let fd = unsafe { file_create(path) };
            if fd < 0 { return copy_into(out, b"error: cannot create file"); }
            let n = unsafe { file_write(fd as usize, content.as_bytes()) };
            unsafe { file_close(fd as usize); }
            if n < 0 {
                copy_into(out, b"error: write failed")
            } else {
                copy_into(out, b"ok: file saved")
            }
        }
        // ── Memory ───────────────────────────────────────────────────────────
        "MEM_STORE" | "STORE" => {
            mem_store(args);
            copy_into(out, b"ok: stored in semantic memory")
        }
        "MEM_QUERY" | "QUERY" | "RECALL" => {
            // Query semantic memory and capture result
            let n = unsafe {
                let mut msg_data = [0u8; 32];
                msg_data[0..8].copy_from_slice(&100u64.to_le_bytes()); // QUERY op
                msg_data[8..8+args.len().min(24)].copy_from_slice(
                    &args.as_bytes()[..args.len().min(24)]);
                syscall_3(SYS_SEND, 2, msg_data.as_ptr() as usize, 32);
                let mut reply = [0u8; 32];
                syscall_2(SYS_RECV, reply.as_mut_ptr() as usize, 32);
                let ptr = u64::from_le_bytes(reply[8..16].try_into().unwrap_or([0;8])) as usize;
                let len = u64::from_le_bytes(reply[16..24].try_into().unwrap_or([0;8])) as usize;
                if ptr != 0 && len > 0 {
                    let copy_n = len.min(out.len());
                    syscall_3(SYS_KREAD, ptr, out.as_mut_ptr() as usize, copy_n);
                    copy_n
                } else { 0 }
            };
            if n == 0 { copy_into(out, b"no results") } else { n }
        }
        "MEM_SEARCH" | "SEARCH" | "TAG" => {
            mem_search(args);
            copy_into(out, b"(search results printed above)")
        }
        // ── System ───────────────────────────────────────────────────────────
        "STATS" => {
            let mut stats = [0u32; 4];
            unsafe { syscall_1(SYS_GET_STATS, stats.as_mut_ptr() as usize); }
            let mut p = 0usize;
            macro_rules! ws { ($s:expr) => { let n = $s.len().min(out.len()-p); out[p..p+n].copy_from_slice(&$s[..n]); p += n; }; }
            ws!(b"free_mb="); let d = fmt_dec(stats[0]); ws!(d.as_bytes());
            ws!(b" total_mb="); let d = fmt_dec(stats[1]); ws!(d.as_bytes());
            ws!(b" cpu_pct="); let d = fmt_dec(stats[2]); ws!(d.as_bytes());
            ws!(b" tasks="); let d = fmt_dec(stats[3]); ws!(d.as_bytes());
            p
        }
        _ => {
            let msg = b"error: unknown tool '";
            let n = msg.len().min(out.len()); out[..n].copy_from_slice(&msg[..n]);
            let mut p = n;
            let on = op.len().min(out.len() - p);
            out[p..p+on].copy_from_slice(&op.as_bytes()[..on]); p += on;
            if p < out.len() { out[p] = b'\''; p += 1; }
            p
        }
    }
}

/// Copy `src` into `dst`, return bytes written.
fn copy_into(dst: &mut [u8], src: &[u8]) -> usize {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
    n
}

/// Format a u32 as decimal digits into a stack-allocated string.
fn fmt_dec(mut n: u32) -> NumStr {
    let mut s = NumStr { buf: [0u8; 12], len: 0 };
    if n == 0 { s.buf[0] = b'0'; s.len = 1; return s; }
    let mut tmp = [0u8; 12];
    let mut i = 12;
    while n > 0 { i -= 1; tmp[i] = b'0' + (n % 10) as u8; n /= 10; }
    s.len = 12 - i;
    s.buf[..s.len].copy_from_slice(&tmp[i..]);
    s
}
struct NumStr { buf: [u8; 12], len: usize }
impl NumStr {
    fn as_bytes(&self) -> &[u8] { &self.buf[..self.len] }
    fn len(&self) -> usize { self.len }
}

/// Legacy wrapper — executes tool and prints result to UART (used in GUI/desktop).
fn execute_tool(cmd: &str) {
    let mut buf = [0u8; 2048];
    let n = execute_tool_to_buf(cmd, &mut buf);
    if n > 0 {
        if let Ok(s) = core::str::from_utf8(&buf[..n]) { println(s); }
        else { println("[binary result]"); }
    }
}

// ── Built-in: net info ────────────────────────────────────────────────────────

fn cmd_net_info() {
    let ip_raw = unsafe { syscall_0(SYS_GET_IP) as u32 };
    if ip_raw == 0 {
        println("net: no IP address (network offline or DHCP pending)");
        return;
    }
    let mut s = UartSink;
    print("IP address : ");
    let octets = [(ip_raw >> 24) as u8, (ip_raw >> 16) as u8, (ip_raw >> 8) as u8, ip_raw as u8];
    for (i, b) in octets.iter().enumerate() {
        render::write_dec(&mut s, *b as u32);
        if i < 3 { print("."); }
    }
    println("");
}

// ── Built-in: dns ─────────────────────────────────────────────────────────────

fn cmd_dns(name: &str) {
    if name.is_empty() { println("Usage: dns <hostname>"); return; }
    let mut ip_packed: u32 = 0;
    let ret = sys_dns_resolve(name, &mut ip_packed);
    if ret < 0 {
        print("dns: could not resolve '"); print(name); println("'");
        return;
    }
    let mut s = UartSink;
    print(name); print(" -> ");
    let octets = [(ip_packed >> 24) as u8, (ip_packed >> 16) as u8, (ip_packed >> 8) as u8, ip_packed as u8];
    for (i, b) in octets.iter().enumerate() {
        render::write_dec(&mut s, *b as u32);
        if i < 3 { print("."); }
    }
    println("");
}

// ── Built-in: fetch ───────────────────────────────────────────────────────────

fn cmd_fetch(url: &str) {
    if url.is_empty() { println("Usage: fetch <http[s]://host/path>"); return; }
    let is_https = url.starts_with("https://");
    if !is_https && !url.starts_with("http://") {
        println("fetch: URL must begin with http:// or https://");
        return;
    }

    print("Fetching "); print(url); println(if is_https { " (TLS)..." } else { " ..." });

    // 8 KB response buffer on the stack
    let mut buf = [0u8; 8192];
    let ret = if is_https {
        sys_https_get(url, &mut buf)
    } else {
        sys_http_get(url, &mut buf)
    };

    if ret < 0 {
        let reason = match ret {
            -1 => "DNS resolution failed",
            -2 => "TCP / TLS connection failed",
            -3 => "HTTP error (non-2xx)",
            -4 => "Timeout",
            -5 => "Buffer too small",
            _  => "Unknown error",
        };
        print("fetch: error: "); println(reason);
        return;
    }

    let bytes = ret as usize;
    print("["); render::write_dec(&mut UartSink, bytes as u32); println(" bytes]");

    // Print body (first 4 KB to avoid flooding terminal)
    let show = bytes.min(4096);
    if let Ok(s) = core::str::from_utf8(&buf[..show]) {
        print(s);
        if show < bytes {
            print("\r\n[... truncated — ");
            render::write_dec(&mut UartSink, (bytes - show) as u32);
            println(" bytes omitted]");
        }
    } else {
        print("[binary: "); render::write_dec(&mut UartSink, bytes as u32); println(" bytes]");
    }
}

// ── Built-in: post ────────────────────────────────────────────────────────────

fn cmd_post(url: &str, body: &str) {
    if url.is_empty() {
        println("Usage: post <https://host/path> <body>");
        return;
    }
    if !url.starts_with("https://") {
        println("post: only https:// URLs are supported");
        return;
    }

    print("POST "); print(url); println(" (TLS)...");

    let mut buf = [0u8; 8192];
    let ret = sys_https_post(url, body.as_bytes(), &mut buf);

    if ret < 0 {
        let reason = match ret {
            -1 => "DNS resolution failed",
            -2 => "TCP / TLS connection failed",
            -3 => "HTTP error (non-2xx)",
            -4 => "Timeout",
            -5 => "Buffer too small",
            _  => "Unknown error",
        };
        print("post: error: "); println(reason);
        return;
    }

    let bytes = ret as usize;
    print("["); render::write_dec(&mut UartSink, bytes as u32); println(" bytes]");
    let show = bytes.min(4096);
    if let Ok(s) = core::str::from_utf8(&buf[..show]) {
        print(s);
        if show < bytes {
            print("\r\n[... truncated — ");
            render::write_dec(&mut UartSink, (bytes - show) as u32);
            println(" bytes omitted]");
        }
    } else {
        print("[binary: "); render::write_dec(&mut UartSink, bytes as u32); println(" bytes]");
    }
}

// ── Built-in: save ────────────────────────────────────────────────────────────

fn cmd_save(path: &str, content: &str) {
    if path.is_empty() {
        println("Usage: save <path> <content>");
        return;
    }
    let fd = unsafe { file_create(path) };
    if fd < 0 {
        print("save: could not create '"); print(path); println("'");
        return;
    }
    let written = unsafe { file_write(fd as usize, content.as_bytes()) };
    unsafe { file_close(fd as usize); }
    if written >= 0 {
        print("Saved "); render::write_dec(&mut UartSink, written as u32);
        print(" bytes to '"); print(path); println("'");
    } else {
        print("save: write failed for '"); print(path); println("'");
    }
}

// ── Built-in: download ────────────────────────────────────────────────────────

fn cmd_download(url: &str, filepath: &str) {
    if url.is_empty() || filepath.is_empty() {
        println("Usage: download <http[s]://url> <filepath>");
        return;
    }
    let is_https = url.starts_with("https://");
    if !is_https && !url.starts_with("http://") {
        println("download: URL must begin with http:// or https://");
        return;
    }

    print("Fetching "); print(url); println(if is_https { " (TLS)..." } else { " ..." });

    let mut buf = [0u8; 8192];
    let ret = if is_https {
        sys_https_get(url, &mut buf)
    } else {
        sys_http_get(url, &mut buf)
    };
    if ret < 0 {
        let reason = match ret {
            -1 => "DNS resolution failed",
            -2 => "TCP / TLS connection failed",
            -3 => "HTTP error (non-2xx)",
            -4 => "Timeout",
            -5 => "Buffer too small",
            _  => "Unknown error",
        };
        print("download: fetch error: "); println(reason);
        return;
    }

    let bytes = ret as usize;
    let fd = unsafe { file_create(filepath) };
    if fd < 0 {
        print("download: could not create '"); print(filepath); println("'");
        return;
    }
    let written = unsafe { file_write(fd as usize, &buf[..bytes]) };
    unsafe { file_close(fd as usize); }
    if written >= 0 {
        print("Saved "); render::write_dec(&mut UartSink, written as u32);
        print(" bytes to '"); print(filepath); println("'");
    } else {
        print("download: write failed for '"); print(filepath); println("'");
    }
}

// ── Built-in: ping ────────────────────────────────────────────────────────────

fn cmd_ping(target: &str) {
    if target.is_empty() {
        println("Usage: ping <host|ip>");
        return;
    }

    // Parse dotted-decimal IPv4; fall back to DNS.
    let mut ip = [0u8; 4];
    let mut valid_ip = true;
    let mut idx = 0;
    let mut curr = 0u32;
    let mut in_num = false;
    for b in target.bytes() {
        if b == b'.' {
            if !in_num || idx >= 4 { valid_ip = false; break; }
            ip[idx] = curr as u8; idx += 1; curr = 0; in_num = false;
        } else if b >= b'0' && b <= b'9' {
            curr = curr * 10 + (b - b'0') as u32;
            if curr > 255 { valid_ip = false; break; }
            in_num = true;
        } else {
            valid_ip = false; break;
        }
    }
    if valid_ip && in_num && idx == 3 { ip[3] = curr as u8; } else { valid_ip = false; }

    let mut ip_packed: u32;
    if valid_ip {
        ip_packed = ((ip[0] as u32) << 24) | ((ip[1] as u32) << 16)
                  | ((ip[2] as u32) <<  8) |  (ip[3] as u32);
    } else {
        ip_packed = 0;
        if sys_dns_resolve(target, &mut ip_packed) < 0 {
            println("ping: unknown host");
            return;
        }
    }

    print("PING "); print(target); println(" 32 bytes of data.");
    let mut timeouts = 0u32;
    for seq in 1u32..=4 {
        let rtt = sys_ping(ip_packed, 1000);
        if rtt < 0 {
            print("Request timeout for icmp_seq ");
            render::write_dec(&mut UartSink, seq);
            println("");
            timeouts += 1;
        } else {
            print("40 bytes from "); print(target);
            print(": icmp_seq="); render::write_dec(&mut UartSink, seq);
            print(" ttl=64 time="); render::write_dec(&mut UartSink, rtt as u32);
            println(" ms");
        }
    }
    print("--- "); print(target); println(" ping statistics ---");
    print("4 packets transmitted, ");
    render::write_dec(&mut UartSink, 4 - timeouts);
    println(" packets received");
}

// ── Built-in: nc — raw TCP connect ────────────────────────────────────────────

/// Parse a dotted-decimal IPv4 string into a packed big-endian u32.
/// Returns None if the string is not valid IPv4.
fn parse_ipv4(s: &str) -> Option<u32> {
    let mut ip = [0u32; 4];
    let mut idx = 0;
    let mut curr = 0u32;
    let mut in_num = false;
    for b in s.bytes() {
        match b {
            b'.' => {
                if !in_num || idx >= 3 { return None; }
                ip[idx] = curr; idx += 1; curr = 0; in_num = false;
            }
            b'0'..=b'9' => {
                curr = curr * 10 + (b - b'0') as u32;
                if curr > 255 { return None; }
                in_num = true;
            }
            _ => return None,
        }
    }
    if !in_num || idx != 3 { return None; }
    ip[3] = curr;
    Some((ip[0] << 24) | (ip[1] << 16) | (ip[2] << 8) | ip[3])
}

/// Resolve hostname or dotted-decimal IP to a packed u32.
fn resolve_host(host: &str) -> Option<u32> {
    if let Some(packed) = parse_ipv4(host) {
        return Some(packed);
    }
    let mut packed = 0u32;
    if sys_dns_resolve(host, &mut packed) >= 0 { Some(packed) } else { None }
}

/// `nc <host> <port> [message]`
///
/// Connect to `host:port`, optionally send `message`, then receive and print
/// up to 4 KB of the response.  Closes the connection when done.
fn cmd_nc(host: &str, port: u16, msg: &str) {
    print("nc: connecting to "); print(host);
    print(":"); render::write_dec(&mut UartSink, port as u32);
    println("...");

    let ip = match resolve_host(host) {
        Some(ip) => ip,
        None     => { println("nc: could not resolve host"); return; }
    };

    let conn = sys_tcp_connect(ip, port, 5000);
    if conn < 0 {
        println("nc: connection refused / timeout");
        return;
    }
    let conn = conn as usize;
    println("nc: connected");

    // Send optional message
    if !msg.is_empty() {
        let sent = sys_tcp_write(conn, msg.as_bytes());
        if sent < 0 { println("nc: write error"); sys_tcp_close(conn); return; }
        // Append CRLF if the message doesn't end with newline
        if !msg.ends_with('\n') {
            sys_tcp_write(conn, b"\r\n");
        }
    }

    // Receive response (up to 4 KB, 5 s timeout)
    let mut buf = [0u8; 4096];
    let mut total = 0usize;
    loop {
        if sys_tcp_wait_readable(conn, 5000) < 0 { break; }
        let n = sys_tcp_read(conn, &mut buf[total..]);
        if n <= 0 { break; }
        total += n as usize;
        if total >= buf.len() { break; }
    }

    if total > 0 {
        // Print as text (replace non-printable except \n\r\t with '.')
        for &b in &buf[..total] {
            match b {
                b'\r' => {}
                b'\n' => println(""),
                0x09  => print("\t"),
                0x20..=0x7E => {
                    let s = core::str::from_utf8(core::slice::from_ref(&b)).unwrap_or(".");
                    print(s);
                }
                _ => print("."),
            }
        }
        println("");
    } else {
        println("nc: no data received");
    }

    sys_tcp_close(conn);
    println("nc: connection closed");
}

/// `listen <port>` — accept one incoming TCP connection, read and print data.
fn cmd_tcp_listen(port: u16) {
    print("listen: binding port "); render::write_dec(&mut UartSink, port as u32); println("...");

    let listener = sys_tcp_listen(port);
    if listener < 0 {
        println("listen: could not bind port");
        return;
    }
    println("listen: waiting for connection (30 s timeout)...");

    let conn = sys_tcp_accept(listener as usize, 30_000);
    if conn < 0 {
        println("listen: timeout — no connection");
        return;
    }
    let conn = conn as usize;
    println("listen: connection accepted");

    // Read up to 4 KB
    let mut buf = [0u8; 4096];
    let mut total = 0usize;
    loop {
        if sys_tcp_wait_readable(conn, 10_000) < 0 { break; }
        let n = sys_tcp_read(conn, &mut buf[total..]);
        if n <= 0 { break; }
        total += n as usize;
        if total >= buf.len() { break; }
    }

    if total > 0 {
        for &b in &buf[..total] {
            match b {
                b'\r' => {}
                b'\n' => println(""),
                0x20..=0x7E => {
                    let s = core::str::from_utf8(core::slice::from_ref(&b)).unwrap_or(".");
                    print(s);
                }
                _ => print("."),
            }
        }
        println("");
    }

    sys_tcp_close(conn);
    println("listen: connection closed");
}

// ── Built-in: cap — capability management ─────────────────────────────────────

/// Layout must match kernel's scheduler::TaskRecord (32 bytes).
#[repr(C)]
struct TaskRecord {
    tid:  u64,
    caps: u64,
    name: [u8; 16],
    _pad: u64,
}

/// Parse a capability name or hex/decimal bitmask.
/// Recognised names: send, recv, blk, ai, log, mem, all.
/// Returns the bitmask or None on parse failure.
fn parse_cap(s: &str) -> Option<u64> {
    match s {
        "send"  => Some(1 << 0),
        "recv"  => Some(1 << 1),
        "blk"   => Some(1 << 2),
        "ai"    => Some(1 << 3),
        "log"   => Some(1 << 4),
        "mem"   => Some(1 << 5),
        "all"   => Some(!0u64),
        "none"  => Some(0),
        _ => {
            // Try hex (0x…) or decimal
            if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                u64::from_str_radix(hex, 16).ok()
            } else {
                s.parse::<u64>().ok()
            }
        }
    }
}

/// Print a human-readable capability set like `[send recv ai log mem]`.
fn print_caps(caps: u64) {
    print("[");
    if caps == !0u64 { print("ALL"); }
    else {
        const NAMES: &[(&str, u64)] = &[
            ("send", 1 << 0), ("recv", 1 << 1), ("blk", 1 << 2),
            ("ai",   1 << 3), ("log",  1 << 4), ("mem", 1 << 5),
        ];
        let mut first = true;
        for &(name, bit) in NAMES {
            if caps & bit != 0 {
                if !first { print(" "); }
                print(name);
                first = false;
            }
        }
        // Show any unknown high bits as hex
        let known: u64 = (1 << 6) - 1;
        let extra = caps & !known;
        if extra != 0 {
            if !first { print(" "); }
            print("0x");
            render::write_hex(&mut UartSink, extra);
        }
        if caps == 0 { print("none"); }
    }
    print("]");
}

fn cmd_cap_list() {
    const MAX: usize = 16;
    let mut records: [TaskRecord; MAX] = core::array::from_fn(|_| TaskRecord { tid: 0, caps: 0, name: [0; 16], _pad: 0 });
    let count = unsafe {
        syscall_2(SYS_LIST_TASKS, records.as_mut_ptr() as usize, MAX)
    };
    if count == 0 {
        println("cap: no tasks found (need CAP_LOG?)");
        return;
    }
    let mut s = UartSink;
    render::header(&mut s, "TID  Name             Capabilities");
    for i in 0..count {
        let r = &records[i];
        let name_len = r.name.iter().position(|&b| b == 0).unwrap_or(16);
        let name_str = core::str::from_utf8(&r.name[..name_len]).unwrap_or("?");

        // TID column
        render::write_dec(&mut UartSink, r.tid as u32);
        let tid_w = if r.tid >= 10 { 2 } else { 1 };
        for _ in 0..(5usize.saturating_sub(tid_w)) { print(" "); }

        // Name column (16 chars wide)
        print(name_str);
        for _ in 0..(17usize.saturating_sub(name_str.len())) { print(" "); }

        // Caps
        print_caps(r.caps);
        println("");
    }
}

fn cmd_cap_show(tid_str: &str) {
    if tid_str.is_empty() { println("cap show: provide a TID"); return; }
    let tid = match tid_str.parse::<usize>() {
        Ok(n) => n,
        Err(_) => { print("cap show: invalid TID: "); println(tid_str); return; }
    };
    let ret = unsafe { syscall_1(SYS_CAP_QUERY, tid) as isize };
    if ret < 0 {
        print("cap show: TID "); render::write_dec(&mut UartSink, tid as u32); println(" not found");
    } else {
        print("TID "); render::write_dec(&mut UartSink, tid as u32); print(": ");
        print_caps(ret as u64);
        println("");
    }
}

fn cmd_cap_grant(tid_str: &str, cap_str: &str) {
    if tid_str.is_empty() || cap_str.is_empty() {
        println("Usage: cap grant <tid> <cap>   (caps: send recv blk ai log mem all)");
        return;
    }
    let tid = match tid_str.parse::<usize>() {
        Ok(n) => n,
        Err(_) => { print("cap grant: invalid TID: "); println(tid_str); return; }
    };
    let bits = match parse_cap(cap_str) {
        Some(b) => b,
        None => { print("cap grant: unknown capability: "); println(cap_str); return; }
    };
    let ret = unsafe { syscall_2(SYS_CAP_GRANT, tid, bits as usize) as isize };
    if ret == 0 {
        print("Granted "); print(cap_str); print(" to TID "); render::write_dec(&mut UartSink, tid as u32);
        // Show new caps
        let new_ret = unsafe { syscall_1(SYS_CAP_QUERY, tid) as isize };
        if new_ret >= 0 { print("  -> "); print_caps(new_ret as u64); }
        println("");
    } else {
        print("cap grant: TID "); render::write_dec(&mut UartSink, tid as u32); println(" not found");
    }
}

fn cmd_cap_revoke(tid_str: &str, cap_str: &str) {
    if tid_str.is_empty() || cap_str.is_empty() {
        println("Usage: cap revoke <tid> <cap>  (caps: send recv blk ai log mem all)");
        return;
    }
    let tid = match tid_str.parse::<usize>() {
        Ok(n) => n,
        Err(_) => { print("cap revoke: invalid TID: "); println(tid_str); return; }
    };
    let bits = match parse_cap(cap_str) {
        Some(b) => b,
        None => { print("cap revoke: unknown capability: "); println(cap_str); return; }
    };
    let ret = unsafe { syscall_2(SYS_CAP_REVOKE, tid, bits as usize) as isize };
    if ret == 0 {
        print("Revoked "); print(cap_str); print(" from TID "); render::write_dec(&mut UartSink, tid as u32);
        let new_ret = unsafe { syscall_1(SYS_CAP_QUERY, tid) as isize };
        if new_ret >= 0 { print("  -> "); print_caps(new_ret as u64); }
        println("");
    } else {
        print("cap revoke: TID "); render::write_dec(&mut UartSink, tid as u32); println(" not found");
    }
}

fn print_cap_help() {
    let mut s = UartSink;
    render::header(&mut s, "Capability management:");
    render::kv(&mut s, "cap list",              "Show all tasks and their caps",  24);
    render::kv(&mut s, "cap show <tid>",         "Show caps for a specific task",  24);
    render::kv(&mut s, "cap grant <tid> <cap>",  "Grant a capability to a task",   24);
    render::kv(&mut s, "cap revoke <tid> <cap>", "Revoke a capability from a task",24);
    s.newline();
    render::kv(&mut s, "cap names:",    "send  recv  blk  ai  log  mem  all",    14);
}

// ── Built-in: wasm ────────────────────────────────────────────────────────────

fn cmd_wasm(path: &str) {
    if path.is_empty() {
        println("Usage: wasm <path.wasm>");
        return;
    }

    print("wasm: loading '"); print(path); println("' ...");

    let ret = unsafe {
        syscall_2(SYS_WASM_EXEC, path.as_ptr() as usize, path.len()) as isize
    };

    match ret {
        0 => println("wasm: exited OK (0)"),
        n if n > 0 => {
            print("wasm: exited with code ");
            render::write_dec(&mut UartSink, n as u32);
            println("");
        }
        -1 => println("wasm: error — invalid path"),
        -2 => { print("wasm: error — file not found: '"); print(path); println("'"); }
        -3 => println("wasm: error — invalid WASM (validation failed)"),
        -4 => println("wasm: error — parse failed"),
        -5 => println("wasm: error — trap during execution"),
        n  => {
            print("wasm: error (");
            render::write_dec(&mut UartSink, (-n) as u32);
            println(")");
        }
    }
}

// ── Pipe consumer commands ────────────────────────────────────────────────────

/// Print lines from `text` that contain `pattern` (case-sensitive).
fn cmd_grep(pattern: &str, text: &str) {
    if pattern.is_empty() { print(text); return; }
    let pat = pattern.as_bytes();
    for line in text.split('\n') {
        let lb = line.as_bytes();
        // Search for pat as a substring of lb
        if lb.len() >= pat.len() {
            let mut found = false;
            'outer: for start in 0..=(lb.len() - pat.len()) {
                for (i, &b) in pat.iter().enumerate() {
                    if lb[start + i] != b { continue 'outer; }
                }
                found = true;
                break;
            }
            if found { print(line); print("\r\n"); }
        }
    }
}

/// Print the first `n` lines of `text`.
fn cmd_head(n: usize, text: &str) {
    let mut count = 0usize;
    for line in text.split('\n') {
        if count >= n { break; }
        print(line); print("\r\n");
        count += 1;
    }
}

/// Count lines, words, and bytes in `text`.
fn cmd_wc(text: &str) {
    let bytes = text.len();
    let lines = text.split('\n').count();
    let words = text.split(|c: char| c.is_ascii_whitespace()).filter(|s| !s.is_empty()).count();
    // Print: "  <lines>  <words>  <bytes>"
    let mut num_buf = [0u8; 32];
    let mut out_buf = [0u8; 64];
    let mut p = 0usize;

    fn write_num(buf: &mut [u8], pos: usize, mut n: usize) -> usize {
        if n == 0 { buf[pos] = b'0'; return pos + 1; }
        let start = pos;
        let mut p = pos;
        while n > 0 && p < buf.len() { buf[p] = b'0' + (n % 10) as u8; p += 1; n /= 10; }
        let mut l = start; let mut r = p - 1;
        while l < r { buf.swap(l, r); l += 1; r -= 1; }
        p
    }

    out_buf[p] = b' '; p += 1;
    p = write_num(&mut out_buf, p, lines);
    out_buf[p] = b' '; p += 1;
    p = write_num(&mut out_buf, p, words);
    out_buf[p] = b' '; p += 1;
    p = write_num(&mut out_buf, p, bytes);
    out_buf[p] = b'\r'; p += 1;
    out_buf[p] = b'\n'; p += 1;
    drop(num_buf);
    unsafe { syscall_2(SYS_LOG, out_buf.as_ptr() as usize, p) };
}

// ── Shell pipeline ────────────────────────────────────────────────────────────

/// Run a single stage (no pipe) of a pipeline. Returns true if the command
/// was a recognised pipe-source (and it ran).  `pipe_in` is Some(data) when
/// this stage has pipe input from the previous stage.
fn run_stage(cmd: &str, pipe_in: Option<&[u8]>) {
    let cmd = cmd.trim();
    if cmd.is_empty() { return; }

    // For pipe-consumer commands, use pipe_in if available.
    let pipe_str: &str = pipe_in.map_or("", |b| core::str::from_utf8(b).unwrap_or(""));

    match parse(cmd) {
        Intent::Grep(pat)  => cmd_grep(pat, pipe_str),
        Intent::Head(n)    => cmd_head(n, pipe_str),
        Intent::Wc         => cmd_wc(pipe_str),
        Intent::ListDir(p) => cmd_ls(p),
        Intent::CatFile(p) => cmd_cat(p),
        Intent::NetInfo    => cmd_net_info(),
        // Everything else: if we have pipe input and no specific handler,
        // just print the pipe input (passthrough).
        _ if pipe_in.is_some() => {
            unsafe { syscall_2(SYS_LOG, pipe_str.as_ptr() as usize, pipe_str.len()) };
        }
        _ => {
            // Non-pipeline command in middle of pipeline — ignore silently.
        }
    }
}

/// Execute `input` as a pipeline: split on `|`, run each stage with capture,
/// feed output of each stage as input to the next.
fn run_pipeline(input: &str) {
    // Split on `|` (max 8 stages)
    let mut stages = [""; 8];
    let mut stage_count = 0usize;
    let bytes = input.as_bytes();
    let mut start = 0usize;
    for i in 0..bytes.len() {
        if bytes[i] == b'|' && stage_count < 7 {
            stages[stage_count] = &input[start..i];
            stage_count += 1;
            start = i + 1;
        }
    }
    stages[stage_count] = &input[start..];
    stage_count += 1;

    if stage_count == 1 {
        // No actual pipe — shouldn't reach here, but handle safely.
        run_stage(stages[0], None);
        return;
    }

    // Run each stage; capture all but the last.
    // pipe_buf holds captured output between stages.
    let mut pipe_buf = [0u8; 8192];
    let mut pipe_len = 0usize;
    let mut has_pipe = false;

    for s in 0..stage_count {
        let is_last = s == stage_count - 1;
        let pipe_in = if has_pipe { Some(&pipe_buf[..pipe_len]) } else { None };

        if !is_last {
            capture_start();
            run_stage(stages[s], pipe_in);
            let captured = capture_end();
            let n = captured.len().min(pipe_buf.len());
            pipe_buf[..n].copy_from_slice(&captured[..n]);
            pipe_len = n;
            has_pipe = true;
        } else {
            run_stage(stages[s], pipe_in);
        }
    }
}

// ── Environment variables (4.7) ───────────────────────────────────────────────

const ENV_MAX:    usize = 32;
const ENV_KEY_LEN: usize = 32;
const ENV_VAL_LEN: usize = 256;

static mut ENV_KEYS:  [[u8; ENV_KEY_LEN]; ENV_MAX] = [[0; ENV_KEY_LEN]; ENV_MAX];
static mut ENV_VALS:  [[u8; ENV_VAL_LEN]; ENV_MAX] = [[0; ENV_VAL_LEN]; ENV_MAX];
static mut ENV_KLENS: [usize; ENV_MAX]              = [0; ENV_MAX];
static mut ENV_VLENS: [usize; ENV_MAX]              = [0; ENV_MAX];
static mut ENV_COUNT: usize                         = 0;

fn env_init() {
    env_set_raw(b"USER",  b"root");
    env_set_raw(b"HOME",  b"/");
    env_set_raw(b"SHELL", b"/ash");
    env_set_raw(b"OS",    b"Aeglos");
    env_set_raw(b"ARCH",  b"aarch64");
}

fn env_set_raw(key: &[u8], val: &[u8]) {
    unsafe {
        // Find existing slot or free slot.
        let mut slot = ENV_MAX;
        for i in 0..ENV_COUNT {
            if ENV_KLENS[i] == key.len() && ENV_KEYS[i][..key.len()] == *key {
                slot = i; break;
            }
        }
        if slot == ENV_MAX {
            if ENV_COUNT >= ENV_MAX { return; }
            slot = ENV_COUNT;
            ENV_COUNT += 1;
        }
        let kn = key.len().min(ENV_KEY_LEN - 1);
        let vn = val.len().min(ENV_VAL_LEN - 1);
        ENV_KEYS[slot][..kn].copy_from_slice(&key[..kn]);
        ENV_KLENS[slot] = kn;
        ENV_VALS[slot][..vn].copy_from_slice(&val[..vn]);
        ENV_VLENS[slot] = vn;
    }
}

fn env_unset_raw(key: &[u8]) {
    unsafe {
        for i in 0..ENV_COUNT {
            if ENV_KLENS[i] == key.len() && ENV_KEYS[i][..key.len()] == *key {
                // Swap with last and shrink.
                let last = ENV_COUNT - 1;
                if i != last {
                    ENV_KEYS[i]  = ENV_KEYS[last];
                    ENV_VALS[i]  = ENV_VALS[last];
                    ENV_KLENS[i] = ENV_KLENS[last];
                    ENV_VLENS[i] = ENV_VLENS[last];
                }
                ENV_COUNT -= 1;
                return;
            }
        }
    }
}

fn env_get_raw(key: &[u8]) -> Option<&'static [u8]> {
    unsafe {
        for i in 0..ENV_COUNT {
            if ENV_KLENS[i] == key.len() && ENV_KEYS[i][..key.len()] == *key {
                return Some(&ENV_VALS[i][..ENV_VLENS[i]]);
            }
        }
        None
    }
}

/// Expand `$VAR` and `${VAR}` references in `input`, writing into `out`.
/// Returns the number of bytes written.
fn env_expand<'a>(input: &str, out: &'a mut [u8]) -> &'a str {
    let src = input.as_bytes();
    let mut si = 0usize;
    let mut di = 0usize;

    while si < src.len() && di < out.len() {
        if src[si] == b'$' {
            si += 1;
            // Optional braces: ${VAR}
            let braced = si < src.len() && src[si] == b'{';
            if braced { si += 1; }
            // Collect var name (alphanumeric + underscore)
            let name_start = si;
            while si < src.len() {
                let b = src[si];
                if b.is_ascii_alphanumeric() || b == b'_' { si += 1; } else { break; }
            }
            if braced && si < src.len() && src[si] == b'}' { si += 1; }
            let name = &src[name_start..si.min(src.len())];
            // Look up and copy value
            if let Some(val) = env_get_raw(name) {
                let n = val.len().min(out.len() - di);
                out[di..di + n].copy_from_slice(&val[..n]);
                di += n;
            }
        } else {
            out[di] = src[si];
            di += 1;
            si += 1;
        }
    }

    core::str::from_utf8(&out[..di]).unwrap_or("")
}

fn cmd_env_list() {
    unsafe {
        for i in 0..ENV_COUNT {
            let k = &ENV_KEYS[i][..ENV_KLENS[i]];
            let v = &ENV_VALS[i][..ENV_VLENS[i]];
            syscall_2(SYS_LOG, k.as_ptr() as usize, k.len());
            print("=");
            syscall_2(SYS_LOG, v.as_ptr() as usize, v.len());
            print("\r\n");
        }
    }
}

/// Parse and execute `export KEY=VALUE` or `export KEY VALUE`.
fn cmd_env_export(args: &str) {
    let args = args.trim();
    // Support both "KEY=VALUE" and "KEY VALUE"
    if let Some(eq) = args.find('=') {
        let key = args[..eq].trim();
        let val = args[eq + 1..].trim();
        env_set_raw(key.as_bytes(), val.as_bytes());
    } else {
        let (key, val) = aska::intent::split_first_word(args);
        env_set_raw(key.trim().as_bytes(), val.trim().as_bytes());
    }
}

fn cmd_env_unset(key: &str) {
    env_unset_raw(key.trim().as_bytes());
}

// ── Shell help ─────────────────────────────────────────────────────────────────

fn print_help() {
    let mut s = UartSink;
    render::header(&mut s, "Filesystem:");
    render::kv(&mut s, "ls [path]",           "List directory (default: /)",  24);
    render::kv(&mut s, "cat <path>",           "Print file contents",          24);
    render::kv(&mut s, "exec <path>",          "Launch ELF (background)",      24);
    render::kv(&mut s, "execw <path>",         "Launch ELF and wait for exit", 24);
    render::kv(&mut s, "save <path> <text>",   "Write text to file",           24);
    s.newline();
    render::header(&mut s, "Semantic memory:");
    render::kv(&mut s, "mem store <text>",     "Store to long-term memory",    24);
    render::kv(&mut s, "mem query <text>",     "Query memory by keyword",      24);
    render::kv(&mut s, "mem search <tag>",     "Search memory by tag",         24);
    s.newline();
    render::header(&mut s, "Network:");
    render::kv(&mut s, "net",                  "Show IP address",              24);
    render::kv(&mut s, "ping <host|ip>",       "ICMP ping (4 packets)",        24);
    render::kv(&mut s, "dns <hostname>",       "DNS A-record lookup",          24);
    render::kv(&mut s, "fetch <http://...>",   "HTTP GET (print body)",        24);
    render::kv(&mut s, "download <url> <p>",   "HTTP GET and save to file",    24);
    s.newline();
    render::header(&mut s, "System:");
    render::kv(&mut s, "wasm <path.wasm>",     "Run WASM app from filesystem", 24);
    render::kv(&mut s, "install",              "Run Aeglos Installer",         24);
    render::kv(&mut s, "cap list",             "List tasks and capabilities",  24);
    render::kv(&mut s, "cap show <tid>",       "Show caps for task",           24);
    render::kv(&mut s, "cap grant <tid> <c>",  "Grant capability to task",     24);
    render::kv(&mut s, "cap revoke <tid> <c>", "Revoke capability from task",  24);
    render::kv(&mut s, "help / ?",             "Show this message",            24);
    render::kv(&mut s, "exit / quit / q",      "Exit Aska",                    24);
    s.newline();
    render::header(&mut s, "AI:");
    render::kv(&mut s, "<anything else>",      "Sent to Numenor for inference", 24);
}

// ── Shell banner ──────────────────────────────────────────────────────────────

fn print_banner() {
    println("\x1b[1;36m");
    println("  +---------------------------------+");
    println("  |   Aska  -  AI Shell  v0.2  EL0 |");
    println("  |   Aeglos OS  /  AArch64         |");
    println("  +---------------------------------+\x1b[0m");
}

// ── Shell main ────────────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn ash_main() -> ! {
    print_banner();
    println("Type \x1b[1mhelp\x1b[0m for commands, or just chat with the AI.");
    println("");

    // Test the new UI syscalls
    unsafe {
        let (w, h, p) = sys_fb_info();
        if w > 0 && h > 0 {
            let fb = sys_fb_map();
            if !fb.is_null() && (fb as usize) != !0usize {
                print("[ash] mapped "); render::write_dec(&mut UartSink, w);
                print("x"); render::write_dec(&mut UartSink, h);
                println(" framebuffer! Starting Aurora Desktop Compositor...");
                
                ui::init(fb, p);
                
                let mut dtop = desktop::Desktop::new();
                let mut evt_buf = [InputEvent { type_: 0, code: 0, value: 0 }; 32];
                let mut mx = 640;
                let mut my = 360;
                let mut md = false;

                loop {
                    let n = sys_input_poll(&mut evt_buf);

                    for i in 0..n {
                        let ev = &evt_buf[i];
                        if ev.type_ == 3 {
                            if ev.code == 0 { mx = ((ev.value as u64 * w as u64) / 32768) as usize; }
                            if ev.code == 1 { my = ((ev.value as u64 * h as u64) / 32768) as usize; }
                        } else if ev.type_ == 1 {
                            if ev.code == 272 || ev.code == 330 {
                                md = ev.value == 1;
                            } else {
                                // Keyboard event — route to login screen or desktop
                                if !dtop.logged_in {
                                    dtop.handle_login_input(ev.code, ev.value == 1);
                                } else {
                                    dtop.handle_key(ev.code, ev.value == 1);
                                }
                            }
                        }
                    }

                    if dtop.logged_in {
                        dtop.handle_mouse(mx, my, md);
                    }
                    
                    // Update wall-clock epoch from RTC (~once per frame)
                    let rtc_epoch = unsafe { syscall_0(SYS_GET_RTC) };
                    if rtc_epoch > 0 { dtop.epoch = rtc_epoch as u32; }

                    // Update system stats (CPU%, MEM)
                    unsafe { syscall_1(SYS_GET_STATS, dtop.stats.as_mut_ptr() as usize); }

                    // Fetch system IP
                    let ip_raw = unsafe { syscall_0(SYS_GET_IP) as u32 };
                    let mut ip_str = [0u8; 16];
                    let mut ip_idx = 0;
                    macro_rules! pu8 {
                        ($b:expr) => {
                            if $b >= 100 { ip_str[ip_idx] = b'0' + ($b / 100); ip_idx += 1; }
                            if $b >= 10 { ip_str[ip_idx] = b'0' + (($b / 10) % 10); ip_idx += 1; }
                            ip_str[ip_idx] = b'0' + ($b % 10); ip_idx += 1;
                        }
                    }
                    pu8!((ip_raw >> 24) as u8); ip_str[ip_idx] = b'.'; ip_idx += 1;
                    pu8!((ip_raw >> 16) as u8); ip_str[ip_idx] = b'.'; ip_idx += 1;
                    pu8!((ip_raw >> 8) as u8); ip_str[ip_idx] = b'.'; ip_idx += 1;
                    pu8!(ip_raw as u8);
                    dtop.sys_ip = ip_str;
                    dtop.sys_ip_len = ip_idx;

                    if let Some(msg) = dtop.sys_send_msg.take() {
                        // AI_OP_INFER_STREAM = 10: tokens arrive one by one
                        unsafe {
                            sys_send_msg(1, 10, msg.as_ptr(), msg.len());
                        }
                    }

                    // Drain streamed tokens (non-blocking, called each frame).
                    // AI_OP_TOKEN (11): append inline token bytes to llm_buffer.
                    // AI_OP_STREAM_END (12): push complete response to chat history.
                    if dtop.llm_response_active {
                        let mut msg = Message { sender: 0, data: [0u8; 32] };
                        let ret = unsafe { syscall_1(SYS_TRY_RECV, &mut msg as *mut _ as usize) as isize };
                        if ret == 0 && msg.sender == 1 {
                            let op = u64::from_le_bytes(
                                msg.data[0..8].try_into().unwrap_or([0; 8]));
                            if op == 11 {
                                // AI_OP_TOKEN — append to streaming buffer
                                let tlen = (msg.data[8] as usize).min(23);
                                let avail = dtop.llm_buffer.len().saturating_sub(dtop.llm_len);
                                let copy  = tlen.min(avail);
                                let start = dtop.llm_len;
                                dtop.llm_buffer[start..start + copy]
                                    .copy_from_slice(&msg.data[9..9 + copy]);
                                dtop.llm_len += copy;
                                // Redraw with partial response visible each frame
                                dtop.on_llm_partial();
                            } else if op == 12 {
                                // AI_OP_STREAM_END — finalize
                                dtop.on_llm_response();
                                dtop.llm_response_active = false;
                            }
                        }
                    }

                    dtop.tick_fs();
                    dtop.tick_login();
                    if !dtop.logged_in {
                        dtop.draw_login();
                    } else {
                        dtop.draw();
                    }
                    sys_fb_flush();

                    // Optional yield if idle to not burn CPU
                    sys_yield();
                }
            }
        }
    }

    let mut state   = aska::shell::ShellState::new();
    let mut cmd_buf = [0u8; 256];
    env_init();

    loop {
        // Print prompt — readline will redraw it on escape sequences,
        // so the prompt is printed once here and rl_redraw echoes it too.
        print("\x1b[1;32maska\x1b[0m \x1b[90m>\x1b[0m ");

        let len = readline(&mut cmd_buf, &state);
        if len == 0 {
            continue; // Ctrl-C or empty
        }

        let raw   = &cmd_buf[..len];
        let raw_str = core::str::from_utf8(raw).unwrap_or("").trim();
        if raw_str.is_empty() { continue; }

        state.push_history(raw);

        // Expand $VAR references before parsing/dispatching.
        let mut expand_buf = [0u8; 512];
        let input = env_expand(raw_str, &mut expand_buf);
        let input = input.trim();
        if input.is_empty() { continue; }

        // Background job: if input ends with '&', launch without waiting.
        if input.ends_with('&') {
            cmd_bg_exec(input);
            continue;
        }

        // Pipeline: if input contains '|', route through the pipeline engine.
        if input.contains('|') {
            run_pipeline(input);
            continue;
        }

        match parse(input) {
            Intent::Empty => {}

            Intent::Help => print_help(),

            Intent::Exit => {
                println("Goodbye.");
                unsafe { sys_exit() };
            }

            // ── Filesystem ───────────────────────────────────────────────────
            Intent::ListDir(path) => cmd_ls(path),
            Intent::CatFile(path) => cmd_cat(path),
            Intent::Exec(path, wait) => cmd_exec(path, wait),
            Intent::Save(path, content) => cmd_save(path, content),

            // ── Semantic memory ───────────────────────────────────────────────
            Intent::Mem(MemOp::Store(text))  => mem_store(text),
            Intent::Mem(MemOp::Query(query)) => mem_query(query),
            Intent::Mem(MemOp::Search(tag))  => mem_search(tag),
            Intent::Mem(MemOp::Help) => {
                let mut s = UartSink;
                render::header(&mut s, "Semantic memory:");
                render::kv(&mut s, "mem store <text>", "Store to long-term memory", 20);
                render::kv(&mut s, "mem query <text>", "Query memory by keyword",   20);
                render::kv(&mut s, "mem search <tag>", "Search memory by tag",      20);
            }

            // ── Network ───────────────────────────────────────────────────────
            Intent::NetInfo              => cmd_net_info(),
            Intent::Ping(target)         => cmd_ping(target),
            Intent::Dns(name)            => cmd_dns(name),
            Intent::Fetch(url)           => cmd_fetch(url),
            Intent::Download(url, path)  => cmd_download(url, path),
            Intent::Post(url, body)      => cmd_post(url, body),
            Intent::Nc(ip, port, msg)    => cmd_nc(ip, port, msg),
            Intent::TcpListen(port)      => cmd_tcp_listen(port),

            // ── System ────────────────────────────────────────────────────────
            Intent::ResetHistory   => {
                // Send AI_OP_RESET_HISTORY (20) to Numenor — no reply expected
                unsafe { sys_send_msg(1, AI_OP_RESET_HISTORY, core::ptr::null(), 0) };
                println("[Aska] Conversation history cleared.");
            }
            Intent::Install        => cmd_install(),
            Intent::WasmRun(path)  => cmd_wasm(path),

            // ── Capabilities ──────────────────────────────────────────────────
            Intent::Cap(CapOp::List)              => cmd_cap_list(),
            Intent::Cap(CapOp::Show(tid))         => cmd_cap_show(tid),
            Intent::Cap(CapOp::Grant(tid, cap))   => cmd_cap_grant(tid, cap),
            Intent::Cap(CapOp::Revoke(tid, cap))  => cmd_cap_revoke(tid, cap),
            Intent::Cap(CapOp::Help)              => print_cap_help(),

            // ── Environment variables ─────────────────────────────────────────
            Intent::EnvList => cmd_env_list(),
            Intent::EnvSet(key, val) => {
                env_set_raw(key.as_bytes(), val.as_bytes());
            }
            Intent::EnvUnset(key) => cmd_env_unset(key),

            // ── TTS ───────────────────────────────────────────────────────────
            Intent::Speak(text) => cmd_speak(text),

            // ── Background jobs ───────────────────────────────────────────────
            Intent::Jobs             => cmd_jobs(),
            Intent::WaitJob(tid)     => cmd_wait_job(tid),
            Intent::BgExec(cmd)      => cmd_bg_exec(cmd),

            // ── Pipe consumers (used after | but also available standalone) ────
            Intent::Grep(_) | Intent::Head(_) | Intent::Wc => {
                println("[aska] Use with a pipe: e.g. ls | grep foo");
            }

            // ── AI fallthrough (streaming + tool-call loop) ───────────────────
            Intent::AiQuery(q) => {
                // Tool-call feedback loop: up to 3 rounds.
                // Round 0: original query → model may emit [[TOOL:args]]
                // Round N: [TOOL RESULT: ...] → model continues / emits more tools
                let mut prompt_buf  = [0u8; 512];
                let mut prompt_len  = q.len().min(512);
                prompt_buf[..prompt_len].copy_from_slice(&q.as_bytes()[..prompt_len]);

                for _round in 0..3 {
                    unsafe { sys_send_msg(1, 10, prompt_buf.as_ptr(), prompt_len) };

                    // Stream tokens, accumulate for tool scanning
                    let mut resp_buf = [0u8; 1024];
                    let mut resp_len = 0usize;

                    unsafe {
                        loop {
                            let mut msg = Message { sender: 0, data: [0; 32] };
                            let ret = syscall_1(SYS_RECV, &mut msg as *mut _ as usize) as isize;
                            if ret != 0 || msg.sender != 1 { sys_yield(); continue; }

                            let op = u64::from_le_bytes(
                                msg.data[0..8].try_into().unwrap_or([0; 8]));

                            if op == 11 {
                                let tlen = (msg.data[8] as usize).min(23);
                                if tlen > 0 {
                                    let piece = &msg.data[9..9 + tlen];
                                    if let Ok(s) = core::str::from_utf8(piece) {
                                        print(s);
                                    }
                                    let copy = tlen.min(resp_buf.len() - resp_len);
                                    resp_buf[resp_len..resp_len + copy]
                                        .copy_from_slice(&piece[..copy]);
                                    resp_len += copy;
                                }
                            } else if op == 12 {
                                break; // AI_OP_STREAM_END
                            }
                        }
                    }
                    println(""); // newline after streamed tokens

                    // Scan for tool calls: [[OP:args]]
                    let resp = core::str::from_utf8(&resp_buf[..resp_len]).unwrap_or("");
                    let tool_call = if let Some(s) = resp.find("[[") {
                        if let Some(rel_end) = resp[s..].find("]]") {
                            Some(&resp[s + 2..s + rel_end])
                        } else { None }
                    } else { None };

                    match tool_call {
                        None => break, // no tool call — done
                        Some(tc) => {
                            // Show tool invocation
                            print(render::color::YELLOW);
                            print("[tool] ");
                            print(tc);
                            print(render::color::RESET);
                            print(" → ");

                            let mut result_buf = [0u8; 2048];
                            let rlen = execute_tool_to_buf(tc, &mut result_buf);
                            let result_str = core::str::from_utf8(&result_buf[..rlen])
                                .unwrap_or("[binary]");

                            // Print result summary (first 200 chars)
                            let show = result_str.len().min(200);
                            println(&result_str[..show]);
                            if result_str.len() > 200 { println("..."); }

                            // Build continuation prompt for next round
                            // "[TOOL RESULT for [[tc]]]: <result>\nPlease continue."
                            let mut nb = [0u8; 512];
                            let mut np = 0usize;
                            macro_rules! wa { ($s:expr) => {
                                let n = $s.len().min(nb.len() - np);
                                nb[np..np+n].copy_from_slice(&$s[..n]); np += n;
                            }; }
                            wa!(b"[TOOL RESULT for [[");
                            wa!(tc.as_bytes());
                            wa!(b"]]]: ");
                            wa!(result_buf[..rlen.min(300)].as_ref());
                            wa!(b"\nPlease continue your response.");
                            prompt_len = np;
                            prompt_buf[..np].copy_from_slice(&nb[..np]);
                        }
                    }
                }
            }
        }
    }
}

// ── Panic handler ─────────────────────────────────────────────────────────────

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    unsafe { sys_exit() }
}
