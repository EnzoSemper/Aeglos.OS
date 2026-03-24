//! FAT32 filesystem driver.
//!
//! Backed by the VirtIO block device (`drivers::virtio`).
//!
//! ## Initialization
//!
//! Call `init(partition_lba)` with the starting LBA of the FAT32 volume
//! (0 for the raw block device, or the partition's first sector for a
//! partitioned disk).  Returns `true` if a valid FAT32 BPB was found.
//!
//! ## Path format
//!
//! Absolute paths with `/` separator, e.g. `/boot/aeglos.bin`.
//! Case-insensitive matching against 8.3 short names and LFN entries.
//!
//! ## Sector cache
//!
//! A 16-slot direct-mapped cache (indexed by `lba % 16`) avoids repeated
//! block-device reads for FAT traversal and directory scans.  Write-through
//! keeps disk and cache coherent without a flush step.
//!
//! ## Thread safety
//!
//! All state is `static mut`.  Callers must ensure exclusive access (i.e. call
//! from a single task or under a lock).  IRQs must be disabled around VirtIO
//! operations (see `virtio.rs` notes).

use super::DirEntry;
use crate::drivers::virtio;

// ── Constants ─────────────────────────────────────────────────────────────────

const SECTOR_SIZE: usize = 512;
const MAX_OPEN:    usize = 8;
const CACHE_SLOTS: usize = 16;

const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN:    u8 = 0x02;
const ATTR_SYSTEM:    u8 = 0x04;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE:   u8 = 0x20;
const ATTR_LONG_NAME: u8 = ATTR_READ_ONLY | ATTR_HIDDEN | ATTR_SYSTEM | ATTR_VOLUME_ID;

const FAT_EOC_MIN: u32 = 0x0FFFFFF8; // End-of-chain marker (min value)
const FAT_FREE:    u32 = 0x00000000; // Free cluster
const FAT_MASK:    u32 = 0x0FFFFFFF; // 28 significant bits

// ── FAT32 State ───────────────────────────────────────────────────────────────

static mut READY:            bool = false;
static mut PART_LBA:         u64  = 0;     // Partition start (absolute LBA)
static mut BYTES_PER_SEC:    u32  = 512;
static mut SECS_PER_CLUSTER: u32  = 1;
static mut FAT_START:        u64  = 0;     // Absolute LBA of FAT 0
static mut DATA_START:       u64  = 0;     // Absolute LBA of cluster 2
static mut ROOT_CLUSTER:     u32  = 2;
static mut TOTAL_CLUSTERS:   u32  = 0;
static mut FREE_HINT:        u32  = 3;     // Next free cluster hint

// ── Sector Cache ──────────────────────────────────────────────────────────────

/// Tag = absolute LBA; u64::MAX = empty slot.
static mut CTAG:  [u64; CACHE_SLOTS]               = [u64::MAX; CACHE_SLOTS];
static mut CDATA: [[u8; SECTOR_SIZE]; CACHE_SLOTS] = [[0; SECTOR_SIZE]; CACHE_SLOTS];

/// Return a reference to the cached sector data, loading from disk if needed.
unsafe fn csec(lba: u64) -> &'static [u8; SECTOR_SIZE] {
    let s = (lba as usize) % CACHE_SLOTS;
    if CTAG[s] != lba {
        virtio::read_block(lba, &mut CDATA[s]);
        CTAG[s] = lba;
    }
    &CDATA[s]
}

/// Write a sector through the cache (updates cache + disk).
unsafe fn wsec(lba: u64, data: &[u8]) {
    let s = (lba as usize) % CACHE_SLOTS;
    CDATA[s][..data.len().min(SECTOR_SIZE)].copy_from_slice(&data[..data.len().min(SECTOR_SIZE)]);
    CTAG[s] = lba;
    virtio::write_block(lba, &CDATA[s]);
}

/// Invalidate a cache slot (used after zeroing cluster sectors).
unsafe fn cinval(lba: u64) {
    let s = (lba as usize) % CACHE_SLOTS;
    if CTAG[s] == lba { CTAG[s] = u64::MAX; }
}

// ── Byte-level field readers ──────────────────────────────────────────────────

#[inline(always)] fn u8at(b: &[u8], o: usize) -> u8  { b[o] }
#[inline(always)] fn u16at(b: &[u8], o: usize) -> u16 { u16::from_le_bytes([b[o], b[o+1]]) }
#[inline(always)] fn u32at(b: &[u8], o: usize) -> u32 { u32::from_le_bytes([b[o], b[o+1], b[o+2], b[o+3]]) }
#[inline(always)] fn u16at_u(b: &[u8], o: usize) -> u16 { u16at(b, o) } // alias for clarity

// ── FAT access ────────────────────────────────────────────────────────────────

/// Read the FAT entry for `cluster`.
unsafe fn fat_get(cluster: u32) -> u32 {
    let byte_off  = cluster as u64 * 4;
    let sec_off   = byte_off / BYTES_PER_SEC as u64;
    let in_sec    = (byte_off % BYTES_PER_SEC as u64) as usize;
    let lba       = FAT_START + sec_off;
    u32at(csec(lba), in_sec) & FAT_MASK
}

/// Write a FAT entry for `cluster`.
unsafe fn fat_put(cluster: u32, value: u32) {
    let byte_off = cluster as u64 * 4;
    let sec_off  = byte_off / BYTES_PER_SEC as u64;
    let in_sec   = (byte_off % BYTES_PER_SEC as u64) as usize;
    let lba      = FAT_START + sec_off;
    // Load sector into cache so we can patch it
    let _ = csec(lba);
    let s = lba as usize % CACHE_SLOTS;
    let val_bytes = (value & FAT_MASK).to_le_bytes();
    CDATA[s][in_sec..in_sec + 4].copy_from_slice(&val_bytes);
    CTAG[s] = lba;
    virtio::write_block(lba, &CDATA[s]);
}

/// Is this cluster value an end-of-chain marker?
#[inline(always)]
fn is_eoc(v: u32) -> bool { (v & FAT_MASK) >= FAT_EOC_MIN || (v & FAT_MASK) == 0 }

/// Convert a cluster number to its starting LBA.
unsafe fn cluster_lba(n: u32) -> u64 {
    DATA_START + (n as u64 - 2) * SECS_PER_CLUSTER as u64
}

/// Allocate one free cluster (marks as EOC in FAT). Returns 0 on failure.
unsafe fn alloc_cluster() -> u32 {
    let start = FREE_HINT;
    let end   = TOTAL_CLUSTERS + 2;
    // First pass: FREE_HINT → end
    for c in start..end {
        if fat_get(c) == FAT_FREE {
            fat_put(c, 0x0FFFFFFF); // mark EOC
            FREE_HINT = c + 1;
            // Zero cluster data
            for s in 0..SECS_PER_CLUSTER {
                let lba = cluster_lba(c) + s as u64;
                let zeros = [0u8; SECTOR_SIZE];
                virtio::write_block(lba, &zeros);
                cinval(lba);
            }
            return c;
        }
    }
    // Second pass: 3 → FREE_HINT (wrap)
    for c in 3..start {
        if fat_get(c) == FAT_FREE {
            fat_put(c, 0x0FFFFFFF);
            FREE_HINT = c + 1;
            for s in 0..SECS_PER_CLUSTER {
                let lba = cluster_lba(c) + s as u64;
                let zeros = [0u8; SECTOR_SIZE];
                virtio::write_block(lba, &zeros);
                cinval(lba);
            }
            return c;
        }
    }
    0 // full
}

// ── Open file table ───────────────────────────────────────────────────────────

struct OpenFile {
    used:            bool,
    writable:        bool,
    size:            u32,   // total file size (bytes)
    start_cluster:   u32,   // first cluster of file
    cur_cluster:     u32,   // cluster containing current offset
    cur_clus_idx:    u32,   // index of cur_cluster in the chain (0 = first)
    offset:          u32,   // absolute byte offset within file
    // Directory location — needed to update size on write/close
    dir_lba:         u64,   // LBA of sector containing the dir entry
    dir_entry_off:   usize, // byte offset within that sector
}

const EMPTY_FILE: OpenFile = OpenFile {
    used: false, writable: false, size: 0,
    start_cluster: 0, cur_cluster: 0, cur_clus_idx: 0, offset: 0,
    dir_lba: 0, dir_entry_off: 0,
};
static mut OPEN_FILES: [OpenFile; MAX_OPEN] = [EMPTY_FILE; MAX_OPEN];

// ── Public API — init ─────────────────────────────────────────────────────────

/// Detect and initialize FAT32 on the block device.
///
/// `partition_lba` is the LBA of the volume's boot sector (0 for a raw
/// FAT32 volume, or the partition start for a partitioned disk).
///
/// Returns `true` if a valid FAT32 BPB was found and parsed.
pub fn init(partition_lba: u64) -> bool {
    unsafe {
        let uart = crate::drivers::uart::Uart::new();

        let sec = csec(partition_lba);

        // Boot sector signature
        if sec[510] != 0x55 || sec[511] != 0xAA {
            uart.puts("[fat32] No boot signature at LBA ");
            uart.put_hex(partition_lba as usize);
            uart.puts("\r\n");
            return false;
        }

        let bps = u16at(sec, 11) as u32;
        if bps != 512 {
            uart.puts("[fat32] Unsupported sector size\r\n");
            return false;
        }

        let spc       = u8at(sec, 13) as u32;
        let reserved  = u16at(sec, 14) as u32;
        let num_fats  = u8at(sec, 16) as u32;
        let fat_sz16  = u16at(sec, 22) as u32;
        let tot16     = u16at(sec, 19) as u32;

        // Distinguish FAT32 from FAT12/16: FAT32 has fat_sz16==0 and tot16==0
        if fat_sz16 != 0 || tot16 != 0 {
            uart.puts("[fat32] Not FAT32 (fat_sz16 or tot16 non-zero)\r\n");
            return false;
        }

        let fat_sz32  = u32at(sec, 36);
        let root_clus = u32at(sec, 44);
        let tot32     = u32at(sec, 32);

        // Validate "FAT32   " label at offset 82
        let fstype = &sec[82..90];
        if &fstype[..5] != b"FAT32" {
            uart.puts("[fat32] No FAT32 type label\r\n");
            return false;
        }

        let fat_start_sec = partition_lba + reserved as u64;
        let data_start_sec = fat_start_sec + num_fats as u64 * fat_sz32 as u64;
        let total_clusters = (tot32 - (data_start_sec - partition_lba) as u32) / spc;

        READY            = true;
        PART_LBA         = partition_lba;
        BYTES_PER_SEC    = bps;
        SECS_PER_CLUSTER = spc;
        FAT_START        = fat_start_sec;
        DATA_START       = data_start_sec;
        ROOT_CLUSTER     = root_clus;
        TOTAL_CLUSTERS   = total_clusters;
        FREE_HINT        = 3;

        uart.puts("[fat32] Ready  root=");
        uart.put_hex(root_clus as usize);
        uart.puts(" clusters=");
        uart.put_dec(total_clusters as usize);
        uart.puts(" spc=");
        uart.put_dec(spc as usize);
        uart.puts("\r\n");

        true
    }
}

// ── Directory parsing ─────────────────────────────────────────────────────────

/// Construct the short 8.3 display name from a directory entry's 11-byte
/// DIR_Name field into `out`. Returns the length written.
///
/// DIR_Name format: 8 bytes name (space-padded), 3 bytes ext (space-padded).
/// We produce "NAME.EXT" (or "NAME" if no ext) with trailing spaces removed.
fn short_name(raw: &[u8; 11], out: &mut [u8; 256]) -> usize {
    let mut n = 0usize;
    // Name (bytes 0–7)
    let mut name_len = 8;
    while name_len > 0 && raw[name_len - 1] == b' ' { name_len -= 1; }
    for i in 0..name_len {
        out[n] = raw[i].to_ascii_lowercase();
        n += 1;
    }
    // Extension (bytes 8–10)
    let mut ext_len = 3;
    while ext_len > 0 && raw[8 + ext_len - 1] == b' ' { ext_len -= 1; }
    if ext_len > 0 {
        out[n] = b'.';
        n += 1;
        for i in 0..ext_len {
            out[n] = raw[8 + i].to_ascii_lowercase();
            n += 1;
        }
    }
    out[n] = 0;
    n
}

/// Extract up to 13 UTF-16LE chars from an LFN entry into `buf[off..]`.
/// Returns the number of chars written (ASCII only; high bytes discarded).
fn lfn_chars(ent: &[u8; 32], buf: &mut [u8; 256], off: usize) -> usize {
    // LFN character positions within a 32-byte LFN entry (byte offsets):
    //   Chars 1–5:  bytes  1,2  3,4  5,6  7,8  9,10
    //   Chars 6–11: bytes 14,15 16,17 18,19 20,21 22,23 24,25
    //   Chars 12–13:bytes 28,29 30,31
    const POS: &[usize] = &[1,3,5,7,9, 14,16,18,20,22,24, 28,30];
    let mut n = 0usize;
    for &p in POS {
        let lo = ent[p];
        let hi = ent[p + 1];
        if lo == 0xFF && hi == 0xFF { break; } // end marker
        if lo == 0 && hi == 0 { break; }        // NUL terminator
        let pos = off + n;
        if pos < 255 {
            // Represent as ASCII if hi==0 and lo is printable, else '?'
            buf[pos] = if hi == 0 && lo.is_ascii() { lo } else { b'?' };
            n += 1;
        }
    }
    n
}

/// Case-insensitive ASCII comparison.
fn name_eq(a: &[u8], b: &str) -> bool {
    let b = b.as_bytes();
    if a.len() != b.len() { return false; }
    a.iter().zip(b.iter()).all(|(&x, &y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

/// Scan the directory rooted at `dir_cluster`, calling `cb` for each valid entry.
///
/// The callback receives:
/// - `name`: the display name (short or LFN), null-terminated
/// - `name_len`: number of chars (without NUL)
/// - `attr`: DIR_Attr byte
/// - `cluster`: first cluster of the entry
/// - `size`: file size in bytes
/// - `dir_lba`: LBA of the sector containing this directory entry
/// - `dir_entry_off`: byte offset of the 8.3 entry within that sector
///
/// Return `true` from `cb` to stop iteration early.
unsafe fn scan_dir<F>(dir_cluster: u32, mut cb: F)
where
    F: FnMut(
        &[u8; 256], usize, // name, name_len
        u8, u32, u32,      // attr, cluster, size
        u64, usize,        // dir_lba, dir_entry_off
    ) -> bool,
{
    // LFN accumulator: we collect LFN entries (which appear in reverse order
    // before the 8.3 entry) into `lfn_buf`, then emit them when we see the
    // matching 8.3 entry.
    let mut lfn_buf    = [0u8; 256];
    let mut lfn_len    = 0usize;
    let mut lfn_active = false;

    let cluster_bytes = SECS_PER_CLUSTER as usize * SECTOR_SIZE;
    let mut cluster   = dir_cluster;

    'outer: loop {
        if cluster < 2 || is_eoc(cluster) { break; }

        let base_lba = cluster_lba(cluster);

        for sec_idx in 0..SECS_PER_CLUSTER as u64 {
            let lba = base_lba + sec_idx;
            let sec = csec(lba);

            for entry_off in (0..SECTOR_SIZE).step_by(32) {
                let e = &sec[entry_off..entry_off + 32];
                let first = e[0];

                if first == 0x00 { break 'outer; } // No more entries ever
                if first == 0xE5 {                  // Deleted entry
                    lfn_active = false;
                    lfn_len    = 0;
                    continue;
                }

                let attr = e[11];

                if attr == ATTR_LONG_NAME {
                    // LFN entry — accumulate name chars.
                    let seq  = e[0] & 0x1F; // sequence number (1-based)
                    let last = (e[0] & 0x40) != 0;
                    if last {
                        // First LFN entry we see (last in sequence = first chars)
                        lfn_len    = 0;
                        lfn_active = true;
                        for b in lfn_buf.iter_mut() { *b = 0; }
                    }
                    if lfn_active && seq >= 1 {
                        // Chars occupy positions (seq-1)*13 … seq*13-1 in the name.
                        let base_pos = (seq as usize - 1) * 13;
                        let added    = lfn_chars(
                            e.try_into().unwrap_or(&[0u8; 32]),
                            &mut lfn_buf,
                            base_pos,
                        );
                        let end = base_pos + added;
                        if end > lfn_len { lfn_len = end; }
                    }
                    continue;
                }

                // Skip volume labels and dot/dot-dot entries
                if attr & ATTR_VOLUME_ID != 0 { lfn_active = false; continue; }
                if first == b'.' { lfn_active = false; continue; }

                // Regular 8.3 entry
                let clus_hi = u16at(e, 20) as u32;
                let clus_lo = u16at(e, 26) as u32;
                let entry_cluster = (clus_hi << 16) | clus_lo;
                let file_size     = u32at(e, 28);

                let mut name_buf = [0u8; 256];
                let name_len;

                if lfn_active && lfn_len > 0 {
                    // Use accumulated LFN name
                    name_buf[..lfn_len].copy_from_slice(&lfn_buf[..lfn_len]);
                    name_buf[lfn_len] = 0;
                    name_len = lfn_len;
                } else {
                    // Fall back to 8.3 short name
                    let raw: &[u8; 11] = e[..11].try_into().unwrap();
                    name_len = short_name(raw, &mut name_buf);
                }

                lfn_active = false;
                lfn_len    = 0;

                if cb(&name_buf, name_len, attr, entry_cluster, file_size, lba, entry_off) {
                    return;
                }
            }
        }

        cluster = fat_get(cluster);
    }
}

// ── Path navigation ───────────────────────────────────────────────────────────

/// Split `path` into components, skip empty parts.
fn path_parts(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

/// Walk a path and return `(cluster, size, is_dir, dir_lba, dir_entry_off)`.
/// `dir_lba` / `dir_entry_off` point to the 8.3 directory entry (for writes).
unsafe fn resolve(path: &str) -> Option<(u32, u32, bool, u64, usize)> {
    let mut dir_cluster = ROOT_CLUSTER;
    let mut parts       = path_parts(path).peekable();

    // Empty path or "/" → root directory
    if parts.peek().is_none() {
        return Some((ROOT_CLUSTER, 0, true, 0, 0));
    }

    let mut result: Option<(u32, u32, bool, u64, usize)> = None;

    'parts: for component in path_parts(path) {
        result = None;
        scan_dir(dir_cluster, |name, name_len, attr, cluster, size, lba, off| {
            let nm = &name[..name_len];
            if name_eq(nm, component) {
                let is_dir = attr & ATTR_DIRECTORY != 0;
                result = Some((cluster, size, is_dir, lba, off));
                return true; // stop
            }
            false
        });
        match result {
            Some((c, _, true, _, _))  => { dir_cluster = c; }
            Some(_) => { /* regular file — must be last component */ }
            None    => return None,
        }
    }

    result
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Open a file by absolute path. Returns a file-descriptor index or `None`.
pub fn open(path: &str) -> Option<usize> {
    unsafe {
        if !READY { return None; }

        let (cluster, size, is_dir, dir_lba, dir_off) = resolve(path)?;
        if is_dir { return None; } // can't open a directory as a file

        // Find a free slot in the open file table
        let fd = OPEN_FILES.iter().position(|f| !f.used)?;

        OPEN_FILES[fd] = OpenFile {
            used:          true,
            writable:      false,
            size,
            start_cluster: cluster,
            cur_cluster:   cluster,
            cur_clus_idx:  0,
            offset:        0,
            dir_lba,
            dir_entry_off: dir_off,
        };
        Some(fd)
    }
}

/// Open a file for writing (creates it if it doesn't exist, truncates if it does).
pub fn open_write(path: &str) -> Option<usize> {
    unsafe {
        if !READY { return None; }

        // Try to resolve; if found, truncate; if not found, create.
        if let Some((cluster, _size, is_dir, dir_lba, dir_off)) = resolve(path) {
            if is_dir { return None; }
            // Truncate: free all clusters in chain, keep start cluster
            let mut c = fat_get(cluster);
            while !is_eoc(c) {
                let next = fat_get(c);
                fat_put(c, FAT_FREE);
                c = next;
            }
            fat_put(cluster, 0x0FFFFFFF);

            // Reset size in directory entry
            let sec = csec(dir_lba);
            let mut buf = [0u8; SECTOR_SIZE];
            buf.copy_from_slice(sec);
            let off = dir_off;
            buf[off + 28] = 0; buf[off + 29] = 0; buf[off + 30] = 0; buf[off + 31] = 0;
            wsec(dir_lba, &buf);

            let fd = OPEN_FILES.iter().position(|f| !f.used)?;
            OPEN_FILES[fd] = OpenFile {
                used: true, writable: true, size: 0,
                start_cluster: cluster, cur_cluster: cluster, cur_clus_idx: 0,
                offset: 0, dir_lba, dir_entry_off: dir_off,
            };
            return Some(fd);
        }

        // File not found — create it in its parent directory.
        create(path)
    }
}

/// Create a new file at `path` (parent directory must exist). Returns fd or `None`.
pub fn create(path: &str) -> Option<usize> {
    unsafe {
        if !READY { return None; }

        // Separate parent dir path from filename
        let (parent, fname) = match path.rfind('/') {
            Some(i) => (&path[..i.max(1)], &path[i + 1..]),
            None    => ("/", path),
        };

        let fname = fname.trim();
        if fname.is_empty() || fname.len() > 11 { return None; } // 8.3 only for create

        // Resolve parent directory
        let parent_cluster = if parent == "/" || parent.is_empty() {
            ROOT_CLUSTER
        } else {
            let (c, _, is_dir, _, _) = resolve(parent)?;
            if !is_dir { return None; }
            c
        };

        // Allocate a cluster for the new file
        let new_cluster = alloc_cluster();
        if new_cluster == 0 { return None; }

        // Find an empty (0x00 or 0xE5) directory entry slot in parent
        let mut found_dir_lba = u64::MAX;
        let mut found_entry_off = 0usize;

        let mut cluster = parent_cluster;
        'search: loop {
            if cluster < 2 || is_eoc(cluster) { break; }
            let base = cluster_lba(cluster);
            for si in 0..SECS_PER_CLUSTER as u64 {
                let lba = base + si;
                let sec = csec(lba);
                for off in (0..SECTOR_SIZE).step_by(32) {
                    let first = sec[off];
                    if first == 0x00 || first == 0xE5 {
                        found_dir_lba   = lba;
                        found_entry_off = off;
                        break 'search;
                    }
                }
            }
            cluster = fat_get(cluster);
        }

        // If no space in existing clusters, extend directory
        if found_dir_lba == u64::MAX {
            let new_dir_cluster = alloc_cluster();
            if new_dir_cluster == 0 { fat_put(new_cluster, FAT_FREE); return None; }
            // Link new cluster into directory chain
            let mut c = parent_cluster;
            while !is_eoc(fat_get(c)) { c = fat_get(c); }
            fat_put(c, new_dir_cluster);
            found_dir_lba   = cluster_lba(new_dir_cluster);
            found_entry_off = 0;
        }

        // Build the 8.3 directory entry
        let mut entry = [0u8; 32];
        // Name: uppercase, space-padded to 11 bytes
        let (name_part, ext_part) = match fname.rfind('.') {
            Some(i) => (&fname[..i], &fname[i+1..]),
            None    => (fname, ""),
        };
        for (i, &b) in name_part.as_bytes().iter().enumerate().take(8) {
            entry[i] = b.to_ascii_uppercase();
        }
        for i in name_part.len()..8 { entry[i] = b' '; }
        for (i, &b) in ext_part.as_bytes().iter().enumerate().take(3) {
            entry[8 + i] = b.to_ascii_uppercase();
        }
        for i in ext_part.len()..3 { entry[8 + i] = b' '; }
        entry[11] = ATTR_ARCHIVE;
        // First cluster
        entry[20] = (new_cluster >> 24) as u8;
        entry[21] = (new_cluster >> 16) as u8;
        entry[26] = (new_cluster >> 8)  as u8;
        entry[27] = (new_cluster)       as u8;
        // Size = 0 initially
        entry[28..32].copy_from_slice(&0u32.to_le_bytes());

        // Write directory entry into the sector
        let sec = csec(found_dir_lba);
        let mut buf = [0u8; SECTOR_SIZE];
        buf.copy_from_slice(sec);
        buf[found_entry_off..found_entry_off + 32].copy_from_slice(&entry);
        wsec(found_dir_lba, &buf);

        // Allocate fd
        let fd = OPEN_FILES.iter().position(|f| !f.used)?;
        OPEN_FILES[fd] = OpenFile {
            used: true, writable: true, size: 0,
            start_cluster: new_cluster, cur_cluster: new_cluster,
            cur_clus_idx: 0, offset: 0,
            dir_lba: found_dir_lba, dir_entry_off: found_entry_off,
        };
        Some(fd)
    }
}

/// Read up to `buf.len()` bytes from `fd` at the current position.
/// Returns number of bytes read, or -1 on error.
pub fn read(fd: usize, buf: &mut [u8]) -> isize {
    unsafe {
        if fd >= MAX_OPEN || !OPEN_FILES[fd].used { return -1; }
        let f = &mut OPEN_FILES[fd];

        let remaining = f.size.saturating_sub(f.offset);
        if remaining == 0 { return 0; }

        let want      = buf.len().min(remaining as usize);
        let cluster_bytes = SECS_PER_CLUSTER as usize * SECTOR_SIZE;
        let mut done  = 0usize;

        while done < want {
            if f.cur_cluster < 2 || is_eoc(f.cur_cluster) { break; }

            let offset_in_cluster = f.offset as usize % cluster_bytes;
            let offset_in_sector  = offset_in_cluster % SECTOR_SIZE;
            let sector_in_cluster = offset_in_cluster / SECTOR_SIZE;

            let lba  = cluster_lba(f.cur_cluster) + sector_in_cluster as u64;
            let sec  = csec(lba);
            let avail_in_sector = SECTOR_SIZE - offset_in_sector;
            let chunk = (want - done).min(avail_in_sector);

            buf[done..done + chunk].copy_from_slice(&sec[offset_in_sector..offset_in_sector + chunk]);
            done      += chunk;
            f.offset  += chunk as u32;

            // Advance cluster if we've crossed a cluster boundary
            let new_offset_in_cluster = f.offset as usize % cluster_bytes;
            if new_offset_in_cluster == 0 && done < want {
                f.cur_cluster  = fat_get(f.cur_cluster);
                f.cur_clus_idx += 1;
            }
        }

        done as isize
    }
}

/// Write `buf` to `fd` at the current position, extending the file as needed.
/// Returns bytes written or -1.
pub fn write(fd: usize, buf: &[u8]) -> isize {
    unsafe {
        if fd >= MAX_OPEN || !OPEN_FILES[fd].used || !OPEN_FILES[fd].writable {
            return -1;
        }
        let cluster_bytes = SECS_PER_CLUSTER as usize * SECTOR_SIZE;
        let mut done      = 0usize;

        while done < buf.len() {
            let f = &mut OPEN_FILES[fd];

            // If current cluster is end-of-chain, allocate a new one
            if is_eoc(f.cur_cluster) || f.cur_cluster < 2 {
                let new_c = alloc_cluster();
                if new_c == 0 { break; }
                if f.start_cluster < 2 {
                    f.start_cluster = new_c;
                    f.cur_cluster   = new_c;
                } else {
                    // Find tail of chain and link
                    let mut tail = f.start_cluster;
                    while !is_eoc(fat_get(tail)) { tail = fat_get(tail); }
                    fat_put(tail, new_c);
                    f.cur_cluster = new_c;
                }
            }

            let offset_in_cluster = f.offset as usize % cluster_bytes;
            let sector_in_cluster = offset_in_cluster / SECTOR_SIZE;
            let offset_in_sector  = offset_in_cluster % SECTOR_SIZE;
            let lba               = cluster_lba(f.cur_cluster) + sector_in_cluster as u64;

            let avail  = SECTOR_SIZE - offset_in_sector;
            let chunk  = (buf.len() - done).min(avail);

            // Read-modify-write the sector
            let _ = csec(lba);
            let s = lba as usize % CACHE_SLOTS;
            CDATA[s][offset_in_sector..offset_in_sector + chunk]
                .copy_from_slice(&buf[done..done + chunk]);
            CTAG[s] = lba;
            virtio::write_block(lba, &CDATA[s]);

            done      += chunk;
            let f      = &mut OPEN_FILES[fd];
            f.offset  += chunk as u32;
            if f.offset > f.size { f.size = f.offset; }

            // Advance cluster if needed
            let new_off_in_clus = f.offset as usize % cluster_bytes;
            if new_off_in_clus == 0 {
                let next = fat_get(f.cur_cluster);
                if !is_eoc(next) {
                    f.cur_cluster  = next;
                    f.cur_clus_idx += 1;
                }
                // else: will alloc next iteration
            }
        }

        // Update file size in directory entry
        let f = &mut OPEN_FILES[fd];
        if f.dir_lba != 0 {
            let _ = csec(f.dir_lba);
            let s   = f.dir_lba as usize % CACHE_SLOTS;
            let off = f.dir_entry_off;
            let sz  = f.size.to_le_bytes();
            CDATA[s][off + 28..off + 32].copy_from_slice(&sz);
            virtio::write_block(f.dir_lba, &CDATA[s]);
        }

        done as isize
    }
}

/// Close an open file descriptor (flush pending size update).
pub fn close(fd: usize) {
    unsafe {
        if fd >= MAX_OPEN || !OPEN_FILES[fd].used { return; }
        // Final size flush (redundant if write() already flushed, but safe)
        let f = &OPEN_FILES[fd];
        if f.writable && f.dir_lba != 0 {
            let _ = csec(f.dir_lba);
            let s   = f.dir_lba as usize % CACHE_SLOTS;
            let off = f.dir_entry_off;
            let sz  = f.size.to_le_bytes();
            CDATA[s][off + 28..off + 32].copy_from_slice(&sz);
            virtio::write_block(f.dir_lba, &CDATA[s]);
        }
        OPEN_FILES[fd] = EMPTY_FILE;
    }
}

/// Seek `fd` to absolute byte offset. Returns 0 on success, -1 on error.
/// Rewinds cluster chain from start when seeking backwards.
pub fn seek(fd: usize, offset: u32) -> isize {
    unsafe {
        if fd >= MAX_OPEN || !OPEN_FILES[fd].used { return -1; }
        let f = &mut OPEN_FILES[fd];
        if offset > f.size { return -1; }

        let cluster_bytes = SECS_PER_CLUSTER as u32 * SECTOR_SIZE as u32;
        let target_clus_idx = offset / cluster_bytes;

        // If seeking forward from current position, continue from cur_cluster.
        // If seeking backward, restart from start_cluster.
        let (mut c, mut idx) = if target_clus_idx >= f.cur_clus_idx {
            (f.cur_cluster, f.cur_clus_idx)
        } else {
            (f.start_cluster, 0)
        };

        while idx < target_clus_idx && !is_eoc(c) {
            c   = fat_get(c);
            idx += 1;
        }

        f.cur_cluster  = c;
        f.cur_clus_idx = idx;
        f.offset       = offset;
        0
    }
}

/// List directory at `path` into `out` (max `out.len()` entries).
/// Returns the number of entries written.
pub fn readdir(path: &str, out: &mut [DirEntry]) -> usize {
    unsafe {
        if !READY { return 0; }

        let dir_cluster = if path == "/" || path.is_empty() {
            ROOT_CLUSTER
        } else {
            match resolve(path) {
                Some((c, _, true, _, _)) => c,
                _ => return 0,
            }
        };

        let mut count = 0usize;
        scan_dir(dir_cluster, |name, name_len, attr, cluster, size, _lba, _off| {
            if count >= out.len() { return true; }
            let e = &mut out[count];
            let n = name_len.min(super::NAME_MAX);
            e.name[..n].copy_from_slice(&name[..n]);
            e.name[n] = 0;
            e.is_dir  = if attr & ATTR_DIRECTORY != 0 { 1 } else { 0 };
            e._pad    = [0; 2];
            e.size    = size;
            let _ = cluster; // used by future VFS clone
            count    += 1;
            false
        });
        count
    }
}

/// Delete a file at `path` — frees its cluster chain and marks its directory
/// entry as deleted (0xE5 in byte 0 of the name field).
/// Returns `true` on success.
pub fn unlink(path: &str) -> bool {
    unsafe {
        if !READY { return false; }
        let (cluster, _size, is_dir, dir_lba, dir_off) = match resolve(path) {
            Some(v) => v,
            None    => return false,
        };
        if is_dir { return false; } // refuse to delete directories

        // Free cluster chain
        let mut c = cluster;
        while !is_eoc(c) && c >= 2 {
            let next = fat_get(c);
            fat_put(c, FAT_FREE);
            c = next;
        }

        // Mark directory entry deleted (0xE5 in byte 0)
        let mut sec = *csec(dir_lba);
        sec[dir_off] = 0xE5;
        wsec(dir_lba, &sec);
        true
    }
}

/// Rename (move) a file from `src` to `dst` within the **same directory**.
///
/// The rename is nearly-atomic from a FAT32 perspective:
///   1. Read the source directory entry (get cluster ptr + size).
///   2. Delete `dst` if it already exists (free its clusters, mark deleted).
///   3. Build a new 8.3 directory entry for `dst` pointing to the src clusters.
///   4. Write the new entry to an empty slot in the parent directory.
///   5. Mark the source entry deleted.
///
/// Cross-directory renames are not supported (returns `false`).
/// Returns `true` on success.
pub fn rename(src: &str, dst: &str) -> bool {
    unsafe {
        if !READY { return false; }

        // Resolve source
        let (src_cluster, src_size, src_is_dir, src_dir_lba, src_dir_off) =
            match resolve(src) { Some(v) => v, None => return false };
        if src_is_dir { return false; }

        // Both paths must share the same parent directory.
        let parent_src = match src.rfind('/') {
            Some(i) => &src[..i.max(1)],
            None    => "/",
        };
        let parent_dst = match dst.rfind('/') {
            Some(i) => &dst[..i.max(1)],
            None    => "/",
        };
        if !name_eq(parent_src.as_bytes(), parent_dst) { return false; }

        // Get destination filename
        let dst_fname = match dst.rfind('/') {
            Some(i) => dst[i+1..].trim(),
            None    => dst.trim(),
        };
        if dst_fname.is_empty() || dst_fname.len() > 11 { return false; }

        // Delete destination if it exists
        if let Some((dst_cluster, _sz, false, dst_dir_lba, dst_dir_off)) = resolve(dst) {
            // Free dst cluster chain
            let mut c = dst_cluster;
            while !is_eoc(c) && c >= 2 {
                let next = fat_get(c);
                fat_put(c, FAT_FREE);
                c = next;
            }
            // Mark dst dir entry deleted
            let mut sec = *csec(dst_dir_lba);
            sec[dst_dir_off] = 0xE5;
            wsec(dst_dir_lba, &sec);
        }

        // Find empty slot in parent directory for the new entry
        let parent_cluster = if parent_src == "/" || parent_src.is_empty() {
            ROOT_CLUSTER
        } else {
            match resolve(parent_src) {
                Some((c, _, true, _, _)) => c,
                _ => return false,
            }
        };

        let mut new_dir_lba   = u64::MAX;
        let mut new_entry_off = 0usize;

        let mut cluster = parent_cluster;
        'find: loop {
            if cluster < 2 || is_eoc(cluster) { break; }
            let base = cluster_lba(cluster);
            for si in 0..SECS_PER_CLUSTER as u64 {
                let lba = base + si;
                let sec = csec(lba);
                for off in (0..SECTOR_SIZE).step_by(32) {
                    let first = sec[off];
                    if first == 0x00 || first == 0xE5 {
                        new_dir_lba   = lba;
                        new_entry_off = off;
                        break 'find;
                    }
                }
            }
            cluster = fat_get(cluster);
        }
        if new_dir_lba == u64::MAX { return false; }

        // Build the new 8.3 directory entry with dst name + src cluster chain
        let mut entry = [0u8; 32];
        let (name_part, ext_part) = match dst_fname.rfind('.') {
            Some(i) => (&dst_fname[..i], &dst_fname[i+1..]),
            None    => (dst_fname, ""),
        };
        for (i, &b) in name_part.as_bytes().iter().enumerate().take(8) {
            entry[i] = b.to_ascii_uppercase();
        }
        for i in name_part.len()..8 { entry[i] = b' '; }
        for (i, &b) in ext_part.as_bytes().iter().enumerate().take(3) {
            entry[8+i] = b.to_ascii_uppercase();
        }
        for i in ext_part.len()..3 { entry[8+i] = b' '; }
        entry[11] = ATTR_ARCHIVE;
        // First cluster (high word in bytes 20-21, low word in bytes 26-27)
        entry[20] = ((src_cluster >> 16) & 0xFF) as u8;
        entry[21] = ((src_cluster >> 24) & 0xFF) as u8;
        entry[26] = ( src_cluster        & 0xFF) as u8;
        entry[27] = ((src_cluster >> 8)  & 0xFF) as u8;
        // File size
        entry[28] = ( src_size        & 0xFF) as u8;
        entry[29] = ((src_size >>  8)  & 0xFF) as u8;
        entry[30] = ((src_size >> 16)  & 0xFF) as u8;
        entry[31] = ((src_size >> 24)  & 0xFF) as u8;

        // Write new entry
        let mut new_sec = *csec(new_dir_lba);
        new_sec[new_entry_off..new_entry_off+32].copy_from_slice(&entry);
        wsec(new_dir_lba, &new_sec);

        // Mark source entry deleted
        let mut src_sec = *csec(src_dir_lba);
        src_sec[src_dir_off] = 0xE5;
        wsec(src_dir_lba, &src_sec);

        true
    }
}

/// Read entire file at `path` into a freshly-allocated heap buffer.
/// Returns `(ptr, len)` or `(null, 0)` on failure. Caller must free.
pub fn read_file_alloc(path: &str) -> (*mut u8, usize) {
    unsafe {
        if !READY { return (core::ptr::null_mut(), 0); }
        let fd = match open(path) {
            Some(f) => f,
            None    => return (core::ptr::null_mut(), 0),
        };
        let size = OPEN_FILES[fd].size as usize;
        if size == 0 { close(fd); return (core::ptr::null_mut(), 0); }

        let buf = crate::memory::heap::c_malloc(size);
        if buf.is_null() { close(fd); return (core::ptr::null_mut(), 0); }
        let slice = core::slice::from_raw_parts_mut(buf, size);
        let n = read(fd, slice);
        close(fd);
        if n < 0 { crate::memory::heap::c_free(buf); return (core::ptr::null_mut(), 0); }
        (buf, n as usize)
    }
}
