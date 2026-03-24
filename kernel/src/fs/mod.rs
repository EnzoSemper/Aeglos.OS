/// Aeglos VFS — virtual filesystem layer.
///
/// Presents a single path-based fd API that routes to the correct backend
/// based on path prefix:
///
///   /proc/…   → ProcFS  — synthetic kernel-info nodes (read-only)
///   /mem/…    → MemFS   — semantic key/value store (STORE / RETRIEVE)
///   everything else → FAT32 on the VirtIO block device
///
/// All backends share a single VFS fd table (VFS_MAX_FD entries).  Each
/// entry carries the backend discriminant so read/write/close can dispatch
/// correctly without the caller knowing which backend is involved.

pub mod fat32;

// ── VFS fd table ─────────────────────────────────────────────────────────────

/// Maximum open files across all backends.
const VFS_MAX_FD: usize = 16;

/// Per-open-file state.
enum VfsFd {
    /// FAT32 file — wraps fat32's own fd index.
    Fat32(usize),
    /// Synthetic proc/mem file — content pre-generated into a 512-byte inline
    /// buffer at open time; reads advance `pos`.
    Inline { buf: [u8; 512], len: usize, pos: usize, writable: bool },
    /// Kernel pipe end — backed by `PIPES[id]` ring buffer.
    Pipe { id: usize, write_end: bool },
}

// ── Kernel pipe pool (item 1.5) ───────────────────────────────────────────────

const PIPE_CAP:  usize = 4096;
const MAX_PIPES: usize = 8;

struct PipeBuf {
    data:       [u8; PIPE_CAP],
    read_pos:   usize,
    write_pos:  usize,
    count:      usize,
    open_read:  bool,
    open_write: bool,
}

impl PipeBuf {
    const fn new() -> Self {
        PipeBuf {
            data: [0; PIPE_CAP],
            read_pos: 0, write_pos: 0, count: 0,
            open_read: false, open_write: false,
        }
    }
    fn read_bytes(&mut self, out: &mut [u8]) -> usize {
        let n = out.len().min(self.count);
        for i in 0..n {
            out[i] = self.data[self.read_pos];
            self.read_pos = (self.read_pos + 1) % PIPE_CAP;
        }
        self.count -= n;
        n
    }
    fn write_bytes(&mut self, src: &[u8]) -> usize {
        let space = PIPE_CAP - self.count;
        let n = src.len().min(space);
        for i in 0..n {
            self.data[self.write_pos] = src[i];
            self.write_pos = (self.write_pos + 1) % PIPE_CAP;
        }
        self.count += n;
        n
    }
}

static mut PIPES: [PipeBuf; MAX_PIPES] = {
    const E: PipeBuf = PipeBuf::new();
    [E; MAX_PIPES]
};

// ── VFS fd table ─────────────────────────────────────────────────────────────

// SAFETY: single-threaded kernel; accessed only under irq_save (via syscall).
static mut VFS_FDS: [Option<VfsFd>; VFS_MAX_FD] = {
    const NONE: Option<VfsFd> = None;
    [NONE; VFS_MAX_FD]
};

/// Allocate a free VFS fd slot.  Returns the index or `None`.
unsafe fn vfs_alloc(fd: VfsFd) -> Option<usize> {
    for i in 0..VFS_MAX_FD {
        if VFS_FDS[i].is_none() {
            VFS_FDS[i] = Some(fd);
            return Some(i);
        }
    }
    None
}

// ── Path routing ─────────────────────────────────────────────────────────────

#[derive(PartialEq)]
enum Backend { Fat32, Proc, Mem }

fn route(path: &str) -> Backend {
    if path.starts_with("/proc") { Backend::Proc }
    else if path.starts_with("/mem") { Backend::Mem }
    else { Backend::Fat32 }
}

// ── ProcFS ───────────────────────────────────────────────────────────────────

/// Write decimal `n` into `buf` starting at `pos`. Returns new pos.
fn write_dec(buf: &mut [u8], mut pos: usize, mut n: u64) -> usize {
    if n == 0 { if pos < buf.len() { buf[pos] = b'0'; pos += 1; } return pos; }
    let start = pos;
    while n > 0 && pos < buf.len() {
        buf[pos] = b'0' + (n % 10) as u8;
        pos += 1;
        n /= 10;
    }
    // Reverse the digits we just wrote
    let mut l = start; let mut r = pos - 1;
    while l < r { buf.swap(l, r); l += 1; r -= 1; }
    pos
}

/// Append a literal `s` into `buf[pos..]`. Returns new pos.
fn write_str(buf: &mut [u8], pos: usize, s: &[u8]) -> usize {
    let n = s.len().min(buf.len().saturating_sub(pos));
    buf[pos..pos + n].copy_from_slice(&s[..n]);
    pos + n
}

/// Format a dotted-decimal IPv4 from packed big-endian u32.
fn write_ip(buf: &mut [u8], pos: usize, ip: [u8; 4]) -> usize {
    let mut p = pos;
    for (i, &b) in ip.iter().enumerate() {
        p = write_dec(buf, p, b as u64);
        if i < 3 { p = write_str(buf, p, b"."); }
    }
    p
}

/// Open a ProcFS node.  Returns a filled VfsFd::Inline or None.
fn proc_open(path: &str) -> Option<VfsFd> {
    let node = path.trim_start_matches('/');
    // Normalise: "proc/foo" and "/proc/foo" → "proc/foo"
    let node = node.strip_prefix("proc").unwrap_or(node);
    let node = node.trim_start_matches('/');

    let mut buf = [0u8; 512];
    let mut p   = 0usize;

    match node {
        // ── /proc/version ──────────────────────────────────────────────────
        "version" => {
            p = write_str(&mut buf, p, b"Aeglos OS 1.0 (AArch64 bare-metal)\n");
        }

        // ── /proc/uptime ───────────────────────────────────────────────────
        "uptime" => {
            let ticks = crate::process::scheduler::total_ticks() as u64;
            let secs  = ticks / 100; // 100 Hz timer
            p = write_dec(&mut buf, p, secs);
            p = write_str(&mut buf, p, b" seconds\n");
        }

        // ── /proc/meminfo ──────────────────────────────────────────────────
        "meminfo" => {
            let free_pages  = crate::memory::free_pages();
            let total_pages = crate::memory::total_pages();
            let free_mb     = (free_pages  * crate::memory::PAGE_SIZE) / (1024 * 1024);
            let total_mb    = (total_pages * crate::memory::PAGE_SIZE) / (1024 * 1024);
            p = write_str(&mut buf, p, b"MemTotal:  ");
            p = write_dec(&mut buf, p, total_mb as u64);
            p = write_str(&mut buf, p, b" MB\nMemFree:   ");
            p = write_dec(&mut buf, p, free_mb as u64);
            p = write_str(&mut buf, p, b" MB\nMemUsed:   ");
            p = write_dec(&mut buf, p, (total_mb - free_mb) as u64);
            p = write_str(&mut buf, p, b" MB\n");
        }

        // ── /proc/net/ip ───────────────────────────────────────────────────
        "net/ip" | "net" => {
            let ip = crate::net::get_ip();
            p = write_ip(&mut buf, p, ip);
            p = write_str(&mut buf, p, b"\n");
        }

        // ── /proc/net/mac ──────────────────────────────────────────────────
        "net/mac" => {
            let mac = crate::net::get_mac();
            for (i, &b) in mac.iter().enumerate() {
                // hex byte
                let hi = b >> 4;
                let lo = b & 0xF;
                if p < buf.len() { buf[p] = if hi < 10 { b'0' + hi } else { b'a' + hi - 10 }; p += 1; }
                if p < buf.len() { buf[p] = if lo < 10 { b'0' + lo } else { b'a' + lo - 10 }; p += 1; }
                if i < 5 && p < buf.len() { buf[p] = b':'; p += 1; }
            }
            p = write_str(&mut buf, p, b"\n");
        }

        // ── /proc/cpuinfo ─────────────────────────────────────────────────
        "cpuinfo" => {
            let ncpus = crate::smp::MAX_CPUS;
            p = write_str(&mut buf, p, b"Architecture: AArch64\nCPUs:         ");
            p = write_dec(&mut buf, p, ncpus as u64);
            p = write_str(&mut buf, p, b"\nModel:        QEMU virt (ARMv8.2-A)\n");
        }

        // ── /proc/tasks ───────────────────────────────────────────────────
        "tasks" => {
            use crate::process::scheduler::TaskRecord;
            let mut recs = [TaskRecord { tid: 0, caps: 0, name: [0; 16], _pad: 0 }; 16];
            let n = crate::process::scheduler::list_tasks(&mut recs);
            p = write_str(&mut buf, p, b"TID  NAME             CAPS\n");
            for i in 0..n {
                let r = &recs[i];
                // TID
                p = write_dec(&mut buf, p, r.tid);
                p = write_str(&mut buf, p, b"    ");
                // Name (up to 15 chars, padded)
                let nlen = r.name.iter().position(|&b| b == 0).unwrap_or(16);
                let name_bytes = &r.name[..nlen];
                p = write_str(&mut buf, p, name_bytes);
                // pad to 17 chars
                let pad = 17usize.saturating_sub(nlen);
                for _ in 0..pad { if p < buf.len() { buf[p] = b' '; p += 1; } }
                // caps hex
                p = write_str(&mut buf, p, b"0x");
                let caps = r.caps;
                for shift in (0..64u32).rev().step_by(4) {
                    let nibble = ((caps >> shift) & 0xF) as u8;
                    if p < buf.len() {
                        buf[p] = if nibble < 10 { b'0' + nibble } else { b'a' + nibble - 10 };
                        p += 1;
                    }
                }
                p = write_str(&mut buf, p, b"\n");
                if p + 40 > buf.len() { break; } // guard against overflow
            }
        }

        // ── /proc/stats ───────────────────────────────────────────────────
        "stats" => {
            let idle  = crate::process::scheduler::idle_ticks() as u64;
            let total = crate::process::scheduler::total_ticks() as u64;
            let cpu_pct = if total > 0 { 100 - (idle * 100 / total) } else { 0 };
            let task_cnt = crate::process::scheduler::task_count();
            p = write_str(&mut buf, p, b"cpu_pct:   ");
            p = write_dec(&mut buf, p, cpu_pct);
            p = write_str(&mut buf, p, b"%\ntask_cnt:  ");
            p = write_dec(&mut buf, p, task_cnt as u64);
            p = write_str(&mut buf, p, b"\n");
        }

        _ => return None, // Unknown proc node
    }

    Some(VfsFd::Inline { buf, len: p, pos: 0, writable: false })
}

/// List a ProcFS directory. Returns the number of entries written.
fn proc_readdir(path: &str, out: &mut [DirEntry]) -> usize {
    let node = path.trim_start_matches('/');
    let node = node.strip_prefix("proc").unwrap_or(node);
    let node = node.trim_start_matches('/');

    let proc_root = [
        ("version",  false),
        ("uptime",   false),
        ("meminfo",  false),
        ("cpuinfo",  false),
        ("tasks",    false),
        ("stats",    false),
        ("net",      true),   // sub-directory
    ];
    let proc_net = [
        ("ip",  false),
        ("mac", false),
    ];

    let entries: &[(&str, bool)] = match node {
        "" => &proc_root,
        "net" => &proc_net,
        _ => return 0,
    };

    let mut count = 0;
    for &(name, is_dir) in entries {
        if count >= out.len() { break; }
        let e = &mut out[count];
        let n = name.len().min(NAME_MAX);
        e.name[..n].copy_from_slice(&name.as_bytes()[..n]);
        e.name[n] = 0;
        e.is_dir = if is_dir { 1 } else { 0 };
        e._pad   = [0; 2];
        e.size   = 0;
        count += 1;
    }
    count
}

// ── MemFS ────────────────────────────────────────────────────────────────────

/// Open a semantic memory key as a read-only file.
/// Path format: `/mem/<key>` — key may contain path separators.
fn mem_open(path: &str) -> Option<VfsFd> {
    let key = path.trim_start_matches('/');
    let key = key.strip_prefix("mem/").unwrap_or(key.strip_prefix("mem").unwrap_or(""));
    if key.is_empty() { return None; }

    // Send RETRIEVE (101) to the Semantic service (TID 2) and wait for reply.
    use crate::ipc::Message;
    use crate::process::scheduler;

    let my_tid = scheduler::current_tid();
    let mut req = Message { sender: my_tid, data: [0u8; 32] };
    // op=101 (OP_RETRIEVE), arg1=key_ptr, arg2=key_len
    req.data[0..8].copy_from_slice(&101u64.to_le_bytes());
    req.data[8..16].copy_from_slice(&(key.as_ptr() as u64).to_le_bytes());
    req.data[16..24].copy_from_slice(&(key.len() as u64).to_le_bytes());

    if scheduler::send_message(2, req).is_err() { return None; }

    // Block-poll for reply (semantic service is on CPU 1).
    let freq = crate::arch::aarch64::timer::physical_timer_freq();
    let end  = crate::arch::aarch64::timer::physical_timer_count() + freq; // 1 s timeout

    loop {
        if let Some(msg) = scheduler::try_recv_message() {
            let mut buf = [0u8; 512];
            // Semantic RETRIEVE reply: data[0..8]=op, data[8..16]=len, data[16..]=payload
            let reply_op = u64::from_le_bytes(msg.data[0..8].try_into().unwrap_or([0;8]));
            if reply_op == 101 {
                let len = u64::from_le_bytes(msg.data[8..16].try_into().unwrap_or([0;8])) as usize;
                let len = len.min(16).min(512); // inline payload fits in 16 bytes
                buf[..len].copy_from_slice(&msg.data[16..16 + len]);
                return Some(VfsFd::Inline { buf, len, pos: 0, writable: false });
            }
        }
        if crate::arch::aarch64::timer::physical_timer_count() >= end { break; }
        unsafe { core::arch::asm!("yield"); }
    }
    None
}

// ── VFS syscall implementations ───────────────────────────────────────────────

/// Maximum length of a filename returned by readdir (NUL-terminated UTF-8).
pub const NAME_MAX: usize = 255;

/// Directory entry — C-repr so it can be passed through the syscall boundary
/// to user-mode buffers directly.
#[repr(C)]
pub struct DirEntry {
    /// NUL-terminated UTF-8 filename (up to 255 chars + NUL).
    pub name:   [u8; NAME_MAX + 1],
    /// 1 if this entry is a subdirectory, 0 for a regular file.
    pub is_dir: u8,
    /// Unused padding (keeps struct size a multiple of 4).
    pub _pad:   [u8; 2],
    /// File size in bytes (0 for directories).
    pub size:   u32,
}

impl DirEntry {
    pub const fn zeroed() -> Self {
        Self { name: [0; NAME_MAX + 1], is_dir: 0, _pad: [0; 2], size: 0 }
    }

    /// Return the filename as a &str (stops at first NUL).
    pub fn name_str(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_MAX);
        core::str::from_utf8(&self.name[..len]).unwrap_or("<?>")
    }
}

/// Open a file by absolute path. Returns a VFS fd index (≥ 0) or -1.
pub fn sys_open(path_ptr: *const u8, path_len: usize) -> isize {
    if path_ptr.is_null() || path_len == 0 { return -1; }
    let bytes = unsafe { core::slice::from_raw_parts(path_ptr, path_len) };
    let path  = core::str::from_utf8(bytes).unwrap_or("");
    if path.is_empty() { return -1; }

    unsafe {
        match route(path) {
            Backend::Proc => {
                match proc_open(path) {
                    Some(fd) => vfs_alloc(fd).map(|i| i as isize).unwrap_or(-1),
                    None     => -1,
                }
            }
            Backend::Mem => {
                match mem_open(path) {
                    Some(fd) => vfs_alloc(fd).map(|i| i as isize).unwrap_or(-1),
                    None     => -1,
                }
            }
            Backend::Fat32 => {
                match fat32::open(path) {
                    Some(fat_fd) => vfs_alloc(VfsFd::Fat32(fat_fd)).map(|i| i as isize).unwrap_or(-1),
                    None         => -1,
                }
            }
        }
    }
}

/// Read up to `len` bytes from `fd` into `buf_ptr`. Returns bytes read or -1.
pub fn sys_read_fd(fd: usize, buf_ptr: *mut u8, len: usize) -> isize {
    if buf_ptr.is_null() || len == 0 || fd >= VFS_MAX_FD { return -1; }
    let buf = unsafe { core::slice::from_raw_parts_mut(buf_ptr, len) };
    unsafe {
        match VFS_FDS[fd].as_mut() {
            Some(VfsFd::Fat32(fat_fd)) => fat32::read(*fat_fd, buf),
            Some(VfsFd::Inline { buf: ibuf, len: ilen, pos, .. }) => {
                let avail = ilen.saturating_sub(*pos);
                let n = avail.min(len);
                if n == 0 { return 0; }
                buf[..n].copy_from_slice(&ibuf[*pos..*pos + n]);
                *pos += n;
                n as isize
            }
            Some(VfsFd::Pipe { id, write_end: false }) => {
                PIPES[*id].read_bytes(buf) as isize
            }
            Some(VfsFd::Pipe { write_end: true, .. }) => -1, // can't read write-end
            None => -1,
        }
    }
}

/// Write `len` bytes from `buf_ptr` into `fd`. Returns bytes written or -1.
pub fn sys_write_fd(fd: usize, buf_ptr: *const u8, len: usize) -> isize {
    if buf_ptr.is_null() || len == 0 || fd >= VFS_MAX_FD { return -1; }
    let buf = unsafe { core::slice::from_raw_parts(buf_ptr, len) };
    unsafe {
        match VFS_FDS[fd].as_mut() {
            Some(VfsFd::Fat32(fat_fd)) => fat32::write(*fat_fd, buf),
            Some(VfsFd::Inline { writable, .. }) => {
                if *writable { len as isize } else { -1 }
            }
            Some(VfsFd::Pipe { id, write_end: true }) => {
                PIPES[*id].write_bytes(buf) as isize
            }
            Some(VfsFd::Pipe { write_end: false, .. }) => -1, // can't write read-end
            None => -1,
        }
    }
}

/// Close a file descriptor. Returns 0 or -1.
pub fn sys_close(fd: usize) -> isize {
    if fd >= VFS_MAX_FD { return -1; }
    unsafe {
        match VFS_FDS[fd].take() {
            Some(VfsFd::Fat32(fat_fd))            => { fat32::close(fat_fd); 0 }
            Some(VfsFd::Inline { .. })             => 0,
            Some(VfsFd::Pipe { id, write_end }) => {
                if write_end { PIPES[id].open_write = false; }
                else         { PIPES[id].open_read  = false; }
                0
            }
            None => -1,
        }
    }
}

/// Create or truncate a file for writing. Returns fd or -1.
pub fn sys_create(path_ptr: *const u8, path_len: usize) -> isize {
    if path_ptr.is_null() || path_len == 0 { return -1; }
    let bytes = unsafe { core::slice::from_raw_parts(path_ptr, path_len) };
    let path  = core::str::from_utf8(bytes).unwrap_or("");
    if path.is_empty() { return -1; }

    unsafe {
        // Only FAT32 and MemFS support writes; proc is read-only.
        match route(path) {
            Backend::Proc => -1,
            Backend::Mem => {
                // /mem/<key> write: return a writable inline buffer; on close
                // the data is flushed to semantic store.  For simplicity, we
                // return a writable inline fd here (content discarded on close —
                // caller must use MEM_STORE IPC for persistent storage).
                let fd = VfsFd::Inline { buf: [0u8; 512], len: 0, pos: 0, writable: true };
                vfs_alloc(fd).map(|i| i as isize).unwrap_or(-1)
            }
            Backend::Fat32 => {
                match fat32::open_write(path) {
                    Some(fat_fd) => vfs_alloc(VfsFd::Fat32(fat_fd)).map(|i| i as isize).unwrap_or(-1),
                    None         => -1,
                }
            }
        }
    }
}

/// List directory at `path` into `out`. Returns entry count or -1 on error.
pub fn sys_readdir(
    path_ptr: *const u8,
    path_len: usize,
    out_ptr:  *mut DirEntry,
    max:      usize,
) -> isize {
    if path_ptr.is_null() || out_ptr.is_null() || max == 0 { return -1; }
    let bytes = unsafe { core::slice::from_raw_parts(path_ptr, path_len) };
    let path  = core::str::from_utf8(bytes).unwrap_or("");
    let out   = unsafe { core::slice::from_raw_parts_mut(out_ptr, max) };

    match route(path) {
        Backend::Proc => proc_readdir(path, out) as isize,
        Backend::Mem  => 0, // no directory listing for MemFS yet
        Backend::Fat32 => fat32::readdir(path, out) as isize,
    }
}

/// Allocate a kernel pipe.  Writes two u32 fd indices to `out_ptr`:
///   out_ptr[0] = read-end fd
///   out_ptr[1] = write-end fd
/// Returns 0 on success, -1 on error.
pub fn sys_pipe(out_ptr: usize) -> isize {
    if out_ptr == 0 { return -1; }
    unsafe {
        let id = match PIPES.iter().position(|p| !p.open_read && !p.open_write) {
            Some(i) => i,
            None => return -1,
        };
        PIPES[id].open_read  = true;
        PIPES[id].open_write = true;
        PIPES[id].read_pos   = 0;
        PIPES[id].write_pos  = 0;
        PIPES[id].count      = 0;

        let rfd = match vfs_alloc(VfsFd::Pipe { id, write_end: false }) {
            Some(f) => f,
            None => {
                PIPES[id].open_read = false; PIPES[id].open_write = false;
                return -1;
            }
        };
        let wfd = match vfs_alloc(VfsFd::Pipe { id, write_end: true }) {
            Some(f) => f,
            None => {
                VFS_FDS[rfd] = None;
                PIPES[id].open_read = false; PIPES[id].open_write = false;
                return -1;
            }
        };

        let out = out_ptr as *mut u32;
        core::ptr::write(out,       rfd as u32);
        core::ptr::write(out.add(1), wfd as u32);
        0
    }
}
