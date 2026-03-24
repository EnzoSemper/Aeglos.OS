
use core::sync::atomic::{AtomicUsize, Ordering};

pub const BLOCK_SIZE: usize = 512;
pub const TOTAL_BLOCKS: usize = 20480; // 10MB / 512 bytes

// Physical offset on drive.img where the semantic store begins.
//
// drive.img layout:
//   [  0 MB .. 512 MB) — FAT32 (rebuilt on every build, sectors 0..1_048_576)
//   [512 MB ..  ~5.5GB) — Qwen3-8B model, ~10.5M sectors
//   [  7 GB ..  7GB+10MB) — Semantic store (this code, sectors below are logical)
//
// Placing the store at 7 GB keeps it clear of FAT32 and the model while
// remaining within the 8 GB drive.img.  The physical sector is:
//   7 * 1024 * 1024 * 1024 / 512 = 14_680_064
const SEMANTIC_BASE_SECTOR: u64 = 14_680_064;

// Superblock lives at logical sector 0 (physical SEMANTIC_BASE_SECTOR + 0).
// Layout (all little-endian):
//   [0..8]   magic   = SUPERBLOCK_MAGIC
//   [8..12]  version = 1u32
//   [12..20] next_free_sector (u64)   ← logical, relative to SEMANTIC_BASE_SECTOR
//   [20..512] zeros
const SUPERBLOCK_SECTOR: u64 = 0;
const SUPERBLOCK_MAGIC: &[u8; 8] = b"AEGLOS\x01\x00";

pub struct BlockManager {
    next_free: AtomicUsize,
}

impl BlockManager {
    pub const fn new() -> Self {
        Self {
            next_free: AtomicUsize::new(101), // Start after index region (0..100)
        }
    }

    /// Called once at startup. Reads the superblock and restores next_free,
    /// or writes a fresh superblock if the disk is unformatted.
    pub fn init(&self) {
        let mut buf = [0u8; 512];
        self.read_block(SUPERBLOCK_SECTOR, &mut buf);

        if &buf[0..8] == SUPERBLOCK_MAGIC {
            // Valid superblock — restore allocator state
            let next = u64::from_le_bytes([
                buf[12], buf[13], buf[14], buf[15],
                buf[16], buf[17], buf[18], buf[19],
            ]);
            // Sanity: must be in the data region and within bounds
            let next = if next >= 101 && (next as usize) < TOTAL_BLOCKS {
                next as usize
            } else {
                101
            };
            self.next_free.store(next, Ordering::Relaxed);
        } else {
            // Unformatted disk — write initial superblock
            self.flush_superblock();
        }
    }

    /// Allocate one block. Returns its sector number, or None if disk is full.
    /// Persists next_free to the superblock after each allocation.
    pub fn allocate(&self) -> Option<u64> {
        let block = self.next_free.fetch_add(1, Ordering::Relaxed);
        if block >= TOTAL_BLOCKS {
            return None;
        }
        self.flush_superblock();
        Some(block as u64)
    }

    /// Read a block from disk using syscall.
    /// `sector` is a logical sector number relative to SEMANTIC_BASE_SECTOR.
    pub fn read_block(&self, sector: u64, buf: &mut [u8; 512]) {
        let phys = (SEMANTIC_BASE_SECTOR + sector) as usize;
        unsafe {
            sys_blk_read(phys, buf.as_mut_ptr() as usize);
        }
    }

    /// Write a block to disk using syscall.
    /// `sector` is a logical sector number relative to SEMANTIC_BASE_SECTOR.
    pub fn write_block(&self, sector: u64, buf: &[u8; 512]) {
        let phys = (SEMANTIC_BASE_SECTOR + sector) as usize;
        unsafe {
            sys_blk_write(phys, buf.as_ptr() as usize);
        }
    }

    /// Write current allocator state to sector 0.
    fn flush_superblock(&self) {
        let mut buf = [0u8; 512];
        buf[0..8].copy_from_slice(SUPERBLOCK_MAGIC);
        buf[8..12].copy_from_slice(&1u32.to_le_bytes()); // version
        let next = self.next_free.load(Ordering::Relaxed) as u64;
        buf[12..20].copy_from_slice(&next.to_le_bytes());
        self.write_block(SUPERBLOCK_SECTOR, &buf);
    }
}


unsafe fn sys_blk_read(sector: usize, buf_ptr: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "mov x8, #6", // SYS_BLK_READ
        "svc #0",
        in("x0") sector,
        in("x1") buf_ptr,
        in("x2") 512usize, // kernel expects (sector, buf_ptr, len)
        lateout("x0") ret,
        out("x8") _,
        clobber_abi("system"),
    );
    ret
}

unsafe fn sys_blk_write(sector: usize, buf_ptr: usize) -> isize {
    let ret: isize;
    core::arch::asm!(
        "mov x8, #7", // SYS_BLK_WRITE
        "svc #0",
        in("x0") sector,
        in("x1") buf_ptr,
        lateout("x0") ret,
        out("x8") _,
        clobber_abi("system"),
    );
    ret
}
