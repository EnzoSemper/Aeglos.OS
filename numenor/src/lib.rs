#![no_std]

// Numenor will use syscalls directly.

pub mod engine;
pub mod model;
pub mod ipc;

use engine::Engine;
use ipc::{AiMessage, AI_OP_INFER, AI_OP_INFER_STREAM, AI_OP_RESET_HISTORY, AI_OP_RELOAD_EMB};

// Match Kernel Message struct layout exactly
#[repr(C)]
struct IpcMessage {
    sender: usize,
    data: [u8; 32],
}

pub fn main() -> ! {
    let engine = Engine::new();
    
    loop {
        let mut msg = IpcMessage { sender: 0, data: [0; 32] };
        
        // SYS_RECV (2) takes one argument: pointer to Message struct
        let ret: isize;
        unsafe {
            core::arch::asm!(
                "mov x8, #2",
                "svc #0",
                in("x0") &mut msg as *mut _ as usize,
                lateout("x0") ret,
                out("x8") _,
                clobber_abi("system"),
            );
        }

        if ret == 0 {
            let ai_msg = AiMessage::from_bytes(&msg.data);
            match ai_msg.op {
                AI_OP_INFER => {
                    let ptr = ai_msg.arg1 as *const u8;
                    let len = ai_msg.arg2 as usize;
                    let response = if !ptr.is_null() && len > 0 {
                        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
                        engine.infer(slice)
                    } else {
                        b"Error: Invalid Input"
                    };

                    let reply = AiMessage {
                        op: AI_OP_INFER,
                        arg1: response.as_ptr() as u64,
                        arg2: response.len() as u64,
                    };

                    let reply_msg = IpcMessage {
                        sender: 0,
                        data: reply.to_bytes(),
                    };

                    unsafe {
                        core::arch::asm!(
                            "mov x8, #1",
                            "svc #0",
                            in("x0") msg.sender,
                            in("x1") &reply_msg as *const _ as usize,
                            lateout("x0") _,
                            out("x8") _,
                            clobber_abi("system"),
                        );
                    }
                }
                AI_OP_INFER_STREAM => {
                    // Streaming inference: tokens delivered as AI_OP_TOKEN IPC
                    // messages directly to the requester; AI_OP_STREAM_END is
                    // sent when generation completes.
                    let ptr = ai_msg.arg1 as *const u8;
                    let len = ai_msg.arg2 as usize;
                    if !ptr.is_null() && len > 0 {
                        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
                        engine.infer_streaming(slice, msg.sender);
                    } else {
                        // Invalid prompt — send immediate stream end
                        let mut end = [0u8; 32];
                        end[0..8].copy_from_slice(&ipc::AI_OP_STREAM_END.to_le_bytes());
                        let end_msg = IpcMessage { sender: 0, data: end };
                        unsafe {
                            core::arch::asm!(
                                "mov x8, #1",
                                "svc #0",
                                in("x0") msg.sender,
                                in("x1") &end_msg as *const _ as usize,
                                lateout("x0") _,
                                out("x8") _,
                                clobber_abi("system"),
                            );
                        }
                    }
                }
                AI_OP_RESET_HISTORY => {
                    unsafe { crate::engine::llm_history_clear_pub(); }
                }
                AI_OP_RELOAD_EMB => {
                    // Query the actual embedding dim from the loaded model and
                    // reply so the caller can adapt without rebuilding.
                    let dim = crate::engine::get_embedding_dim() as u64;
                    let reply = AiMessage {
                        op: AI_OP_RELOAD_EMB,
                        arg1: dim,
                        arg2: 0,
                    };
                    let reply_msg = IpcMessage {
                        sender: 0,
                        data: reply.to_bytes(),
                    };
                    unsafe {
                        core::arch::asm!(
                            "mov x8, #1",
                            "svc #0",
                            in("x0") msg.sender,
                            in("x1") &reply_msg as *const _ as usize,
                            lateout("x0") _,
                            out("x8") _,
                            clobber_abi("system"),
                        );
                    }
                }
                _ => {}
            }
        }
    }
}
