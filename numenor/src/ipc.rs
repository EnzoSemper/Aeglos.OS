/// IPC interface for Numenor — communicates with the Aeglos kernel
/// to service AI syscalls from other components.

use core::convert::TryInto;

pub const AI_OP_LOAD:         u64 = 1;
pub const AI_OP_INFER:        u64 = 2;
pub const AI_OP_UNLOAD:       u64 = 3;
/// Streaming inference request.  Same args as AI_OP_INFER.
/// Numenor will send one AI_OP_TOKEN message per token piece,
/// followed by AI_OP_STREAM_END when generation is complete.
pub const AI_OP_INFER_STREAM: u64 = 10;
/// Partial token delivered during streaming inference.
/// data[0..8] = AI_OP_TOKEN, data[8] = byte count (max 23), data[9..32] = bytes.
pub const AI_OP_TOKEN:        u64 = 11;
/// End-of-stream signal sent after last token.
pub const AI_OP_STREAM_END:   u64 = 12;
/// Clear conversation history (start a new session).
pub const AI_OP_RESET_HISTORY: u64 = 20;
/// Query the current embedding dimension from the loaded model.
/// Reply: data[0..8] = AI_OP_RELOAD_EMB, data[8..16] = actual_dim (u64).
pub const AI_OP_RELOAD_EMB: u64 = 30;

/// Message layout for AI calls:
/// - data[0..8]: Operation ID (u64)
/// - data[8..16]: Argument 1 (u64) - e.g. pointer to prompt
/// - data[16..24]: Argument 2 (u64) - e.g. length of prompt
/// - data[24..32]: Reserved
pub struct AiMessage {
    pub op: u64,
    pub arg1: u64,
    pub arg2: u64,
}

impl AiMessage {
    pub fn from_bytes(data: &[u8; 32]) -> Self {
        let op = u64::from_le_bytes(data[0..8].try_into().unwrap());
        let arg1 = u64::from_le_bytes(data[8..16].try_into().unwrap());
        let arg2 = u64::from_le_bytes(data[16..24].try_into().unwrap());
        Self { op, arg1, arg2 }
    }

    pub fn to_bytes(&self) -> [u8; 32] {
        let mut data = [0u8; 32];
        data[0..8].copy_from_slice(&self.op.to_le_bytes());
        data[8..16].copy_from_slice(&self.arg1.to_le_bytes());
        data[16..24].copy_from_slice(&self.arg2.to_le_bytes());
        data
    }
}
