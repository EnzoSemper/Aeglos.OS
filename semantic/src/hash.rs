
/// Simple 32-byte hash (Placeholder for SHA-256)
/// Uses FNV-1a extended to 32 bytes (repeating/mixing).
pub fn hash_buf(buf: &[u8]) -> [u8; 32] {
    let mut hash: u64 = 0xcbf29ce484222325;
    let prime: u64 = 0x1099511628211;

    for byte in buf {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(prime);
    }

    // Expand 64-bit hash to 32 bytes by mixing
    let mut out = [0u8; 32];
    for i in 0..4 {
        let val = hash.wrapping_add(i as u64).wrapping_mul(prime);
        out[i*8..(i+1)*8].copy_from_slice(&val.to_le_bytes());
    }
    out
}
