//! Persistent kernel configuration — stored as `key=value\n` lines in
//! `/etc/config` on the FAT32 partition.
//!
//! The file is read at boot (`init()`) into an in-memory table (up to
//! CONFIG_MAX_ENTRIES pairs).  Changes written by `set()` are immediately
//! flushed back to disk.
//!
//! The format is intentionally minimal: one `key=value` per line, no
//! sections, no comments.  Empty lines and lines without `=` are ignored.

use crate::fs::fat32;

const CONFIG_PATH: &str = "/config";
const CONFIG_MAX_ENTRIES: usize = 32;
const KEY_MAX:            usize = 32;
const VAL_MAX:            usize = 128;

// ── In-memory config table ────────────────────────────────────────────────────

struct Entry {
    key:   [u8; KEY_MAX],
    klen:  usize,
    val:   [u8; VAL_MAX],
    vlen:  usize,
}

impl Entry {
    const fn empty() -> Self {
        Entry { key: [0; KEY_MAX], klen: 0, val: [0; VAL_MAX], vlen: 0 }
    }

    fn key_str(&self) -> &str {
        core::str::from_utf8(&self.key[..self.klen]).unwrap_or("")
    }
    fn val_str(&self) -> &str {
        core::str::from_utf8(&self.val[..self.vlen]).unwrap_or("")
    }
}

static mut TABLE:  [Entry; CONFIG_MAX_ENTRIES] = {
    const E: Entry = Entry::empty();
    [E; CONFIG_MAX_ENTRIES]
};
static mut N_ENTRIES: usize = 0;
static mut LOADED:    bool  = false;

// ── Parser ────────────────────────────────────────────────────────────────────

fn parse_config(data: &[u8]) {
    unsafe {
        N_ENTRIES = 0;
        let mut i = 0;
        while i < data.len() && N_ENTRIES < CONFIG_MAX_ENTRIES {
            // Find end of line
            let line_start = i;
            while i < data.len() && data[i] != b'\n' { i += 1; }
            let line = &data[line_start..i];
            if i < data.len() { i += 1; } // skip \n

            // Skip empty lines
            if line.is_empty() { continue; }
            // Trim trailing \r
            let line = if line.last() == Some(&b'\r') { &line[..line.len()-1] } else { line };
            if line.is_empty() { continue; }

            // Find '='
            let eq = match line.iter().position(|&b| b == b'=') {
                Some(p) => p,
                None    => continue,
            };

            let key_b = &line[..eq];
            let val_b = &line[eq+1..];

            let klen = key_b.len().min(KEY_MAX);
            let vlen = val_b.len().min(VAL_MAX);
            let e = &mut TABLE[N_ENTRIES];
            e.klen = klen;
            e.vlen = vlen;
            e.key[..klen].copy_from_slice(&key_b[..klen]);
            e.val[..vlen].copy_from_slice(&val_b[..vlen]);
            N_ENTRIES += 1;
        }
    }
}

// ── Serialiser ────────────────────────────────────────────────────────────────

/// Serialise the in-memory table into `buf`.  Returns bytes written.
fn serialise(buf: &mut [u8]) -> usize {
    let mut pos = 0usize;
    macro_rules! push {
        ($s:expr) => {{
            let b: &[u8] = $s;
            let n = b.len().min(buf.len().saturating_sub(pos));
            buf[pos..pos+n].copy_from_slice(&b[..n]);
            pos += n;
        }};
    }
    unsafe {
        for i in 0..N_ENTRIES {
            let e = &TABLE[i];
            if e.klen == 0 { continue; }
            push!(&e.key[..e.klen]);
            push!(b"=");
            push!(&e.val[..e.vlen]);
            push!(b"\n");
        }
    }
    pos
}

// ── FAT32 persistence helpers ─────────────────────────────────────────────────

fn flush_to_disk() {
    let mut buf = [0u8; 4096];
    let len = serialise(&mut buf);

    // Atomic write: write to /config.t, then rename → /config
    let tmp = "/config.t";
    if let Some(fd) = fat32::open_write(tmp) {
        fat32::write(fd, &buf[..len]);
        fat32::close(fd);
        fat32::rename(tmp, CONFIG_PATH);
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Load the config from FAT32 (call once at boot, after virtio_blk is ready).
/// Safe to call multiple times — subsequent calls reload from disk.
pub fn init() {
    static mut FILE_BUF: [u8; 4096] = [0u8; 4096];
    unsafe {
        if let Some(fd) = fat32::open(CONFIG_PATH) {
            let mut total = 0usize;
            loop {
                let n = fat32::read(fd, &mut FILE_BUF[total..]);
                if n <= 0 { break; }
                total += n as usize;
                if total >= FILE_BUF.len() { break; }
            }
            fat32::close(fd);
            if total > 0 {
                parse_config(&FILE_BUF[..total]);
            } else {
                N_ENTRIES = 0;
            }
        } else {
            N_ENTRIES = 0;
        }
        LOADED = true;
    }
}

/// Look up a config key.  Returns a static-lifetime str slice pointing into
/// the in-memory table (valid until the next `set()` or `init()`).
pub fn get(key: &str) -> Option<&'static str> {
    unsafe {
        for i in 0..N_ENTRIES {
            if TABLE[i].key_str() == key {
                // SAFETY: We return a reference to the static TABLE.
                let e: &'static Entry = &*(&TABLE[i] as *const Entry);
                return Some(e.val_str());
            }
        }
    }
    None
}

/// Set or update a config key and flush to disk.
/// Returns `false` if the table is full or a key/val is too long.
pub fn set(key: &str, val: &str) -> bool {
    if key.len() > KEY_MAX || val.len() > VAL_MAX { return false; }
    unsafe {
        // Update existing entry
        for i in 0..N_ENTRIES {
            if TABLE[i].key_str() == key {
                let vlen = val.len().min(VAL_MAX);
                TABLE[i].val[..vlen].copy_from_slice(&val.as_bytes()[..vlen]);
                TABLE[i].vlen = vlen;
                flush_to_disk();
                return true;
            }
        }
        // New entry
        if N_ENTRIES >= CONFIG_MAX_ENTRIES { return false; }
        let e = &mut TABLE[N_ENTRIES];
        let klen = key.len().min(KEY_MAX);
        let vlen = val.len().min(VAL_MAX);
        e.key[..klen].copy_from_slice(&key.as_bytes()[..klen]);
        e.klen = klen;
        e.val[..vlen].copy_from_slice(&val.as_bytes()[..vlen]);
        e.vlen = vlen;
        N_ENTRIES += 1;
        flush_to_disk();
        true
    }
}

/// Remove a key from the config and flush to disk.
pub fn remove(key: &str) -> bool {
    unsafe {
        for i in 0..N_ENTRIES {
            if TABLE[i].key_str() == key {
                // Shift entries down
                for j in i..N_ENTRIES-1 {
                    TABLE[j].key   = TABLE[j+1].key;
                    TABLE[j].klen  = TABLE[j+1].klen;
                    TABLE[j].val   = TABLE[j+1].val;
                    TABLE[j].vlen  = TABLE[j+1].vlen;
                }
                TABLE[N_ENTRIES-1] = Entry::empty();
                N_ENTRIES -= 1;
                flush_to_disk();
                return true;
            }
        }
    }
    false
}

/// Iterate all entries, calling `f(key, val)` for each.
pub fn for_each<F: FnMut(&str, &str)>(mut f: F) {
    unsafe {
        for i in 0..N_ENTRIES {
            f(TABLE[i].key_str(), TABLE[i].val_str());
        }
    }
}

/// Syscall: SYS_CONFIG_GET (74) — copy the value for `key` into `out_buf`.
/// Returns bytes written, or -1 if not found.
pub fn sys_config_get(key_ptr: usize, key_len: usize, out_ptr: usize, out_len: usize) -> isize {
    if key_ptr == 0 || key_len == 0 { return -1; }
    let key_bytes = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_len) };
    let key = core::str::from_utf8(key_bytes).unwrap_or("");
    match get(key) {
        Some(v) => {
            if out_ptr != 0 && out_len > 0 {
                let n = v.len().min(out_len);
                unsafe { core::ptr::copy_nonoverlapping(v.as_ptr(), out_ptr as *mut u8, n); }
                n as isize
            } else {
                v.len() as isize // return needed length
            }
        }
        None => -1,
    }
}

/// Syscall: SYS_CONFIG_SET (75) — set `key` = `val`.
/// Returns 0 on success, -1 on failure.
pub fn sys_config_set(key_ptr: usize, key_len: usize, val_ptr: usize, val_len: usize) -> isize {
    if key_ptr == 0 || key_len == 0 { return -1; }
    let key_bytes = unsafe { core::slice::from_raw_parts(key_ptr as *const u8, key_len) };
    let val_bytes = if val_ptr != 0 && val_len > 0 {
        unsafe { core::slice::from_raw_parts(val_ptr as *const u8, val_len) }
    } else { b"" };
    let key = core::str::from_utf8(key_bytes).unwrap_or("");
    let val = core::str::from_utf8(val_bytes).unwrap_or("");
    if set(key, val) { 0 } else { -1 }
}
