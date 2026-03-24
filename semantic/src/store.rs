
use crate::block_manager::BlockManager;
use crate::hash::hash_buf;
#[allow(unused_imports)]
use crate::metadata::Metadata;
#[allow(unused_imports)]
use crate::vector::Embedding;

// Disk Layout:
// Block 0: Superblock
// Block 1..100: Index (Hash Table)
// Block 101..: Data Region

const INDEX_START_SECTOR: u64 = 1;
const INDEX_SECTORS: u64 = 100;
#[allow(dead_code)]
const DATA_START_SECTOR: u64 = 101;

const ENTRIES_PER_SECTOR: usize = 8;
const ENTRY_SIZE: usize = 64;

// Entry: 32 bytes hash + 8 bytes sector + 8 bytes length
// + 8 metadata_sector + 8 vector_sector = 64 bytes exactly.
// 8 entries per 512-byte sector.
// An entry is "in use" when sector != 0 (sector 0 is the superblock,
// never a valid data sector).
#[derive(Clone, Copy)]
#[repr(C)]
struct IndexEntry {
    hash: [u8; 32],
    sector: u64,
    len: u64,
    metadata_sector: u64, // 0 if none
    vector_sector: u64,   // 0 if none
}

pub struct Store {
    pub bm: BlockManager,
}

impl Store {
    pub const fn new() -> Self {
        Self {
            bm: BlockManager::new(),
        }
    }

    pub fn init(&self) {
        self.bm.init();
    }

    /// Store content blob. Returns Hash.
    pub fn store(&self, data: &[u8]) -> Result<[u8; 32], &'static str> {
        self.store_full(data, None, None)
    }

    pub fn store_full(&self, data: &[u8], metadata: Option<&Metadata>, vector: Option<&Embedding>) -> Result<[u8; 32], &'static str> {
        let hash = hash_buf(data);

        // 1. Check if exists
        if let Some(_) = self.lookup_index(&hash) {
            return Ok(hash);
        }

        // 2. Allocate blocks
        if data.len() > 512 {
            return Err("Data too large for single block");
        }

        // Allocate Data Sector
        let sector = self.bm.allocate().ok_or("Disk full")?;

        // Allocate Metadata Sector if provided
        let meta_sector = if let Some(m) = metadata {
            let s = self.bm.allocate().ok_or("Disk full (meta)")?;
            let mut buf = [0u8; 512];
            let m_bytes = unsafe { core::slice::from_raw_parts(m as *const _ as *const u8, core::mem::size_of::<Metadata>()) };
            buf[..m_bytes.len()].copy_from_slice(m_bytes);
            self.bm.write_block(s, &buf);
            s
        } else {
            0
        };

        // Allocate Vector Sector(s) if provided
        // Embedding is 1536 bytes = 3 sectors.
        let vec_sector = if let Some(v) = vector {
            let s1 = self.bm.allocate().ok_or("Disk full (vec)")?;
            let s2 = self.bm.allocate().ok_or("Disk full (vec)")?;
            let s3 = self.bm.allocate().ok_or("Disk full (vec)")?;

            let v_bytes = unsafe { core::slice::from_raw_parts(v as *const _ as *const u8, core::mem::size_of::<Embedding>()) };
            let mut buf = [0u8; 512];
            buf.copy_from_slice(&v_bytes[0..512]);
            self.bm.write_block(s1, &buf);
            buf.copy_from_slice(&v_bytes[512..1024]);
            self.bm.write_block(s2, &buf);
            buf.copy_from_slice(&v_bytes[1024..1536]);
            self.bm.write_block(s3, &buf);

            s1
        } else {
            0
        };

        // Write Data
        let mut buf = [0u8; 512];
        buf[..data.len()].copy_from_slice(data);
        self.bm.write_block(sector, &buf);

        // Write Index
        self.write_index(&hash, sector, data.len() as u64, meta_sector, vec_sector)?;

        Ok(hash)
    }

    pub fn retrieve(&self, hash: &[u8; 32], out: &mut [u8]) -> Result<usize, &'static str> {
        if let Some((sector, len)) = self.lookup_index(hash) {
            if out.len() < len as usize {
                return Err("Buffer too small");
            }
            let mut buf = [0u8; 512];
            self.bm.read_block(sector, &mut buf);
            out[..len as usize].copy_from_slice(&buf[..len as usize]);
            Ok(len as usize)
        } else {
            Err("Not found")
        }
    }

    // Linear scan of index sectors.
    // Uses explicit byte-offset arithmetic to avoid optimizer issues
    // with pointer-walk loops on #[repr(C)] structs.
    fn lookup_index(&self, hash: &[u8; 32]) -> Option<(u64, u64)> {
        let mut buf = [0u8; 512];
        let mut i: u64 = 0;
        while i < INDEX_SECTORS {
            self.bm.read_block(INDEX_START_SECTOR + i, &mut buf);
            let mut j: usize = 0;
            while j < ENTRIES_PER_SECTOR {
                let off = j * ENTRY_SIZE;
                // Read sector field (at offset 32 within entry) to check if in use
                let sector_bytes: [u8; 8] = [
                    buf[off + 32], buf[off + 33], buf[off + 34], buf[off + 35],
                    buf[off + 36], buf[off + 37], buf[off + 38], buf[off + 39],
                ];
                let sector = u64::from_le_bytes(sector_bytes);
                if sector != 0 {
                    // Entry is in use — compare hash (first 32 bytes of entry)
                    if &buf[off..off + 32] == hash {
                        let len_bytes: [u8; 8] = [
                            buf[off + 40], buf[off + 41], buf[off + 42], buf[off + 43],
                            buf[off + 44], buf[off + 45], buf[off + 46], buf[off + 47],
                        ];
                        let len = u64::from_le_bytes(len_bytes);
                        return Some((sector, len));
                    }
                }
                j += 1;
            }
            i += 1;
        }
        None
    }

    fn write_index(&self, hash: &[u8; 32], sector: u64, len: u64, meta_sec: u64, vec_sec: u64) -> Result<(), &'static str> {
        let mut buf = [0u8; 512];
        let mut i: u64 = 0;
        while i < INDEX_SECTORS {
            let idx_sector = INDEX_START_SECTOR + i;
            self.bm.read_block(idx_sector, &mut buf);

            let mut j: usize = 0;
            while j < ENTRIES_PER_SECTOR {
                let off = j * ENTRY_SIZE;
                // Check if slot is free (sector field == 0)
                let sector_bytes: [u8; 8] = [
                    buf[off + 32], buf[off + 33], buf[off + 34], buf[off + 35],
                    buf[off + 36], buf[off + 37], buf[off + 38], buf[off + 39],
                ];
                let existing_sector = u64::from_le_bytes(sector_bytes);
                if existing_sector == 0 {
                    // Free slot — write entry
                    buf[off..off + 32].copy_from_slice(hash);
                    buf[off + 32..off + 40].copy_from_slice(&sector.to_le_bytes());
                    buf[off + 40..off + 48].copy_from_slice(&len.to_le_bytes());
                    buf[off + 48..off + 56].copy_from_slice(&meta_sec.to_le_bytes());
                    buf[off + 56..off + 64].copy_from_slice(&vec_sec.to_le_bytes());
                    self.bm.write_block(idx_sector, &buf);
                    return Ok(());
                }
                j += 1;
            }
            i += 1;
        }
        Err("Index full")
    }
}
