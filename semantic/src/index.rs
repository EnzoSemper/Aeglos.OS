/// Metadata index — search stored content by tags, content type, or vector similarity.
///
/// Scans the on-disk index entries, reads their associated metadata / vector
/// sectors, and filters by the requested criteria.  Returns matching hashes.

use crate::block_manager::BlockManager;
use crate::vector::{Embedding, EMBEDDING_DIM};

const INDEX_START_SECTOR: u64 = 1;
const INDEX_SECTORS: u64 = 100;
const ENTRIES_PER_SECTOR: usize = 8;
const ENTRY_SIZE: usize = 64;

/// Maximum results returned by a single search.
const MAX_RESULTS: usize = 8;

/// Maximum results returned by a vector similarity search.
const MAX_VEC_RESULTS: usize = 8;

pub struct Index<'a> {
    bm: &'a BlockManager,
}

/// A search result: hash + data sector + data length.
#[derive(Clone, Copy)]
pub struct SearchResult {
    pub hash: [u8; 32],
    pub sector: u64,
    pub len: u64,
}

impl<'a> Index<'a> {
    pub fn new(bm: &'a BlockManager) -> Self {
        Self { bm }
    }

    /// Search for entries whose metadata contains the given tag substring.
    /// Returns up to MAX_RESULTS matches.
    pub fn search_by_tag(&self, tag: &[u8]) -> ([SearchResult; MAX_RESULTS], usize) {
        let mut results = [SearchResult { hash: [0; 32], sector: 0, len: 0 }; MAX_RESULTS];
        let mut count = 0;

        let mut idx_buf = [0u8; 512];
        let mut meta_buf = [0u8; 512];
        let mut i: u64 = 0;

        while i < INDEX_SECTORS && count < MAX_RESULTS {
            self.bm.read_block(INDEX_START_SECTOR + i, &mut idx_buf);

            let mut j: usize = 0;
            while j < ENTRIES_PER_SECTOR && count < MAX_RESULTS {
                let off = j * ENTRY_SIZE;

                // Read sector field (offset 32) to check if in use
                let sector = read_u64(&idx_buf, off + 32);
                if sector == 0 {
                    j += 1;
                    continue;
                }

                // Read metadata_sector (offset 48)
                let meta_sector = read_u64(&idx_buf, off + 48);
                if meta_sector == 0 {
                    j += 1;
                    continue;
                }

                // Read metadata from disk
                self.bm.read_block(meta_sector, &mut meta_buf);

                // Tags are at offset 52 in Metadata (created:8 + accessed:8 + content_type:4 + parent:32 + flags:4 = 56)
                // Wait: created(8) + accessed(8) + content_type(4) + parent(32) + flags(4) = 56
                // tags start at byte 56, length 128
                let tags_start = 56;
                let tags_end = tags_start + 128;
                let tags_slice = &meta_buf[tags_start..tags_end];

                // Find null terminator
                let tags_len = tags_slice.iter().position(|&b| b == 0).unwrap_or(128);
                let tags = &tags_slice[..tags_len];

                // Substring search for the tag
                if contains(tags, tag) {
                    let mut hash = [0u8; 32];
                    hash.copy_from_slice(&idx_buf[off..off + 32]);
                    let len = read_u64(&idx_buf, off + 40);
                    results[count] = SearchResult { hash, sector, len };
                    count += 1;
                }

                j += 1;
            }
            i += 1;
        }

        (results, count)
    }

    /// Search for entries by content_type value.
    pub fn search_by_type(&self, content_type: u32) -> ([SearchResult; MAX_RESULTS], usize) {
        let mut results = [SearchResult { hash: [0; 32], sector: 0, len: 0 }; MAX_RESULTS];
        let mut count = 0;

        let mut idx_buf = [0u8; 512];
        let mut meta_buf = [0u8; 512];
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

                let meta_sector = read_u64(&idx_buf, off + 48);
                if meta_sector == 0 {
                    j += 1;
                    continue;
                }

                self.bm.read_block(meta_sector, &mut meta_buf);

                // content_type is at offset 16 in Metadata (created:8 + accessed:8 = 16)
                let ct = read_u32(&meta_buf, 16);
                if ct == content_type {
                    let mut hash = [0u8; 32];
                    hash.copy_from_slice(&idx_buf[off..off + 32]);
                    let len = read_u64(&idx_buf, off + 40);
                    results[count] = SearchResult { hash, sector, len };
                    count += 1;
                }

                j += 1;
            }
            i += 1;
        }

        (results, count)
    }
}

/// A vector similarity search result.
#[derive(Clone, Copy)]
pub struct VecResult {
    pub hash:       [u8; 32],
    pub sector:     u64,
    pub len:        u64,
    pub similarity: f32,
}

impl<'a> Index<'a> {
    /// Find the stored entries most similar to `query`.
    ///
    /// Scans all index entries that have a `vec_sector`, loads the 3-sector
    /// (1536-byte) embedding from disk, computes cosine similarity against
    /// `query`, and collects entries with similarity ≥ `threshold`.
    ///
    /// Returns results sorted descending by similarity (insertion-sort into
    /// the fixed-size result array, no heap allocation required).
    pub fn search_by_vector(
        &self,
        query:     &Embedding,
        threshold: f32,
    ) -> ([VecResult; MAX_VEC_RESULTS], usize) {
        let empty = VecResult { hash: [0; 32], sector: 0, len: 0, similarity: 0.0 };
        let mut results = [empty; MAX_VEC_RESULTS];
        let mut count   = 0usize;

        // Scratch buffer for loading a 3-sector (1536-byte) embedding.
        let mut sec_buf = [0u8; 512];
        let mut emb_bytes = [0u8; EMBEDDING_DIM * 4];

        let mut idx_buf = [0u8; 512];
        let mut i: u64 = 0;

        while i < INDEX_SECTORS {
            self.bm.read_block(INDEX_START_SECTOR + i, &mut idx_buf);

            let mut j: usize = 0;
            while j < ENTRIES_PER_SECTOR {
                let off = j * ENTRY_SIZE;

                let data_sector = read_u64(&idx_buf, off + 32);
                if data_sector == 0 { j += 1; continue; }

                // vec_sector is at entry offset 56 (= 32 hash + 8 sector + 8 len + 8 meta)
                let vec_sector = read_u64(&idx_buf, off + 56);
                if vec_sector == 0 { j += 1; continue; }

                // Load embedding: 3 consecutive sectors → 1536 bytes
                self.bm.read_block(vec_sector,     &mut sec_buf);
                emb_bytes[0..512].copy_from_slice(&sec_buf);
                self.bm.read_block(vec_sector + 1, &mut sec_buf);
                emb_bytes[512..1024].copy_from_slice(&sec_buf);
                self.bm.read_block(vec_sector + 2, &mut sec_buf);
                emb_bytes[1024..1536].copy_from_slice(&sec_buf);

                let stored = Embedding::from_bytes(&emb_bytes);
                let sim    = query.cosine_similarity(&stored);

                if sim >= threshold {
                    let mut hash = [0u8; 32];
                    hash.copy_from_slice(&idx_buf[off..off + 32]);
                    let len = read_u64(&idx_buf, off + 40);

                    let candidate = VecResult { hash, sector: data_sector, len, similarity: sim };

                    // Insertion-sort into results (descending similarity).
                    if count < MAX_VEC_RESULTS {
                        results[count] = candidate;
                        count += 1;
                        // Bubble up
                        let mut k = count - 1;
                        while k > 0 && results[k].similarity > results[k - 1].similarity {
                            results.swap(k, k - 1);
                            k -= 1;
                        }
                    } else if sim > results[MAX_VEC_RESULTS - 1].similarity {
                        // Replace the weakest result
                        results[MAX_VEC_RESULTS - 1] = candidate;
                        let mut k = MAX_VEC_RESULTS - 1;
                        while k > 0 && results[k].similarity > results[k - 1].similarity {
                            results.swap(k, k - 1);
                            k -= 1;
                        }
                    }
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

fn read_u32(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset], buf[offset+1], buf[offset+2], buf[offset+3],
    ])
}

/// Simple substring search: does `haystack` contain `needle`?
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
