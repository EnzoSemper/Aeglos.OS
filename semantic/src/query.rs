/// Natural language query engine — keyword search across stored content.
///
/// Scans all stored entries, reads their data sectors, and checks for
/// keyword matches. Returns matching hashes with relevance info.

use crate::block_manager::BlockManager;

const INDEX_START_SECTOR: u64 = 1;
const INDEX_SECTORS: u64 = 100;
const ENTRIES_PER_SECTOR: usize = 8;
const ENTRY_SIZE: usize = 64;

const MAX_RESULTS: usize = 8;

pub struct QueryEngine<'a> {
    bm: &'a BlockManager,
}

/// A query result: hash + data sector + data length.
#[derive(Clone, Copy)]
pub struct QueryResult {
    pub hash: [u8; 32],
    pub sector: u64,
    pub len: u64,
}

impl<'a> QueryEngine<'a> {
    pub fn new(bm: &'a BlockManager) -> Self {
        Self { bm }
    }

    /// Search for entries whose data content contains the keyword (case-sensitive).
    pub fn search_keyword(&self, keyword: &[u8]) -> ([QueryResult; MAX_RESULTS], usize) {
        let mut results = [QueryResult { hash: [0; 32], sector: 0, len: 0 }; MAX_RESULTS];
        let mut count = 0;

        if keyword.is_empty() {
            return (results, 0);
        }

        let mut idx_buf = [0u8; 512];
        let mut data_buf = [0u8; 512];
        let mut i: u64 = 0;

        while i < INDEX_SECTORS && count < MAX_RESULTS {
            self.bm.read_block(INDEX_START_SECTOR + i, &mut idx_buf);

            let mut j: usize = 0;
            while j < ENTRIES_PER_SECTOR && count < MAX_RESULTS {
                let off = j * ENTRY_SIZE;

                let sector = read_u64(&idx_buf, off + 32);
                if sector == 0 {
                    j += 1;
                    continue;
                }

                let len = read_u64(&idx_buf, off + 40);
                if len == 0 || len > 512 {
                    j += 1;
                    continue;
                }

                // Read the actual data
                self.bm.read_block(sector, &mut data_buf);
                let data = &data_buf[..len as usize];

                // Substring search
                if contains(data, keyword) {
                    let mut hash = [0u8; 32];
                    hash.copy_from_slice(&idx_buf[off..off + 32]);
                    results[count] = QueryResult { hash, sector, len };
                    count += 1;
                }

                j += 1;
            }
            i += 1;
        }

        (results, count)
    }
}

fn read_u64(buf: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        buf[offset], buf[offset+1], buf[offset+2], buf[offset+3],
        buf[offset+4], buf[offset+5], buf[offset+6], buf[offset+7],
    ])
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return needle.is_empty();
    }
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        if &haystack[i..i + needle.len()] == needle {
            return true;
        }
        i += 1;
    }
    false
}
