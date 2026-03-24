/// Vector embedding dimension.
/// 384 dimensions (e.g. mini-LM) * 4 bytes (f32) = 1536 bytes.
/// Fits exactly in 3 x 512-byte sectors.
pub const EMBEDDING_DIM: usize = 384;
pub const EMBEDDING_SIZE: usize = EMBEDDING_DIM * 4;

// sqrtf is exported #[no_mangle] by the kernel's memory/mod.rs (via libm).
extern "C" {
    fn sqrtf(x: f32) -> f32;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Embedding {
    pub vector: [f32; EMBEDDING_DIM],
}

impl Embedding {
    pub fn zero() -> Self {
        Self { vector: [0.0; EMBEDDING_DIM] }
    }

    /// Cosine similarity between two embeddings.  Returns a value in [-1, 1];
    /// higher is more similar.  Works correctly for both normalized and
    /// unnormalized vectors.
    pub fn cosine_similarity(&self, other: &Self) -> f32 {
        let mut dot    = 0.0f32;
        let mut norm_a = 0.0f32;
        let mut norm_b = 0.0f32;

        for i in 0..EMBEDDING_DIM {
            dot    += self.vector[i] * other.vector[i];
            norm_a += self.vector[i] * self.vector[i];
            norm_b += other.vector[i] * other.vector[i];
        }

        if norm_a == 0.0 || norm_b == 0.0 {
            return 0.0;
        }

        unsafe { dot / (sqrtf(norm_a) * sqrtf(norm_b)) }
    }

    /// Deserialize from a flat byte buffer (little-endian f32 array).
    pub fn from_bytes(src: &[u8]) -> Self {
        let mut emb = Self::zero();
        let floats = EMBEDDING_DIM.min(src.len() / 4);
        for i in 0..floats {
            let bytes = [src[i*4], src[i*4+1], src[i*4+2], src[i*4+3]];
            emb.vector[i] = f32::from_le_bytes(bytes);
        }
        emb
    }
}
