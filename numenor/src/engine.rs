/// Numenor inference engine.
/// Executes local LLM inference (GGUF/GGML format).

pub struct Engine;


#[link(name = "numenor_cpp", kind = "static")]
extern "C" {
    fn call_global_ctors();
    fn llm_init();
    fn llm_infer(prompt: *const u8, prompt_len: usize, output: *mut u8, output_len: usize) -> usize;
    fn llm_fast_infer(prompt: *const u8, prompt_len: usize, output: *mut u8, output_len: usize) -> usize;
    fn llm_embedding(text: *const u8, text_len: i32, out: *mut f32, out_dim: i32) -> i32;
    fn llm_get_embedding_dim() -> i32;
    fn llm_history_clear();
    fn llm_infer_streaming(
        prompt: *const u8, prompt_len: usize,
        callback: extern "C" fn(*const u8, i32, *mut ()),
        userdata: *mut (),
    ) -> usize;
    fn llm_fast_infer_streaming(
        prompt: *const u8, prompt_len: usize,
        callback: extern "C" fn(*const u8, i32, *mut ()),
        userdata: *mut (),
    ) -> usize;
}

extern "C" {
    fn shim_console_puts(s: *const u8);
}

/// Public wrapper to clear conversation history from lib.rs.
pub fn llm_history_clear_pub() {
    unsafe { llm_history_clear(); }
}

/// Returns the embedding dimension of the currently-loaded model.
/// Queries the C++ engine at runtime — no hardcoded dimension.
/// Returns 384 as a safe fallback if the model is not yet loaded.
pub fn get_embedding_dim() -> usize {
    let d = unsafe { llm_get_embedding_dim() };
    if d > 0 { d as usize } else { 384 }
}

impl Engine {
    pub fn new() -> Self {
        unsafe {
            call_global_ctors();
            llm_init();
        }
        Self
    }



    /// Run inference on a prompt.
    pub fn infer(&self, prompt: &[u8]) -> &'static [u8] {
        static mut OUTPUT_BUF:  [u8; 1024] = [0; 1024];
        static mut RAG_BUF:     [u8; 256]  = [0; 256];
        static mut COMBINED_BUF:[u8; 2048] = [0; 2048];

        let prompt_str = core::str::from_utf8(prompt).unwrap_or("");

        unsafe {
            let buf: &mut [u8] = &mut COMBINED_BUF;
            let mut cursor = 0usize;
            let mut has_rag_context = false;

            // ── Optional RAG context via vector similarity search ─────────────
            if !prompt_str.is_empty() {
                let found = query_semantic(prompt_str, &mut RAG_BUF);
                if found > 0 {
                    has_rag_context = true;
                    shim_console_puts(b"[Numenor] RAG Context Injected.\r\n\0".as_ptr());
                    buf_append(buf, &mut cursor, b"[Context: ");
                    // Copy context, stripping [[...]] prompt-injection patterns.
                    let rag = &RAG_BUF[..found];
                    let mut ri = 0usize;
                    while ri < rag.len() && cursor < buf.len() - 1 {
                        if ri + 1 < rag.len() && rag[ri] == b'[' && rag[ri + 1] == b'[' {
                            ri += 2;
                            while ri < rag.len() {
                                if ri + 1 < rag.len() && rag[ri] == b']' && rag[ri + 1] == b']' {
                                    ri += 2; break;
                                }
                                ri += 1;
                            }
                        } else {
                            buf[cursor] = rag[ri]; cursor += 1; ri += 1;
                        }
                    }
                    buf_append(buf, &mut cursor, b"]\n");
                }
            }

            // ── User prompt (date/time is injected by llm_engine.cpp sys prompt)
            buf_append(buf, &mut cursor, prompt);

            // ── Route to fast or main model based on query complexity ─────────
            // Fast model (Qwen3-0.6B): short/simple queries, lower latency.
            // Main model (Qwen3-8B):   long prompts, code, reasoning, RAG context.
            let use_fast = !has_rag_context && is_simple_query(prompt);

            // ── AI-informed scheduling: claim more CPU during inference ────────
            sys_set_priority(NUMENOR_TID, 3);

            let len = if use_fast {
                shim_console_puts(b"[Numenor] Fast model.\r\n\0".as_ptr());
                llm_fast_infer(
                    COMBINED_BUF.as_ptr(),
                    cursor,
                    OUTPUT_BUF.as_mut_ptr(),
                    OUTPUT_BUF.len(),
                )
            } else {
                llm_infer(
                    COMBINED_BUF.as_ptr(),
                    cursor,
                    OUTPUT_BUF.as_mut_ptr(),
                    OUTPUT_BUF.len(),
                )
            };

            // ── AI-informed scheduling: yield CPU based on what we just did ───
            let post_priority: usize = if len > 64 { 1 } else { 2 };
            sys_set_priority(NUMENOR_TID, post_priority);

            if len > 0 {
                store_qa_in_semantic(prompt, &OUTPUT_BUF[..len]);
            }
            &OUTPUT_BUF[..len]
        }
    }

    /// Streaming inference — sends one `AI_OP_TOKEN` IPC message per token
    /// to `reply_to`, then sends `AI_OP_STREAM_END`.
    pub fn infer_streaming(&self, prompt: &[u8], reply_to: usize) {
        static mut RAG_BUF:      [u8; 256]  = [0; 256];
        static mut COMBINED_BUF: [u8; 2048] = [0; 2048];

        let prompt_str = core::str::from_utf8(prompt).unwrap_or("");

        unsafe {
            let buf: &mut [u8] = &mut COMBINED_BUF;
            let mut cursor = 0usize;
            let mut has_rag_context = false;

            if !prompt_str.is_empty() {
                let found = query_semantic(prompt_str, &mut RAG_BUF);
                if found > 0 {
                    has_rag_context = true;
                    buf_append(buf, &mut cursor, b"[Context: ");
                    buf_append(buf, &mut cursor, &RAG_BUF[..found]);
                    buf_append(buf, &mut cursor, b"]\n");
                }
            }
            buf_append(buf, &mut cursor, prompt);

            let use_fast = !has_rag_context && is_simple_query(prompt);

            STREAM_TARGET_TID = reply_to;
            sys_set_priority(NUMENOR_TID, 3);

            if use_fast {
                llm_fast_infer_streaming(
                    COMBINED_BUF.as_ptr(), cursor,
                    token_callback,
                    core::ptr::null_mut(),
                );
            } else {
                llm_infer_streaming(
                    COMBINED_BUF.as_ptr(), cursor,
                    token_callback,
                    core::ptr::null_mut(),
                );
            }

            sys_set_priority(NUMENOR_TID, 2);
        }

        // Send end-of-stream marker
        let mut end = [0u8; 32];
        end[0..8].copy_from_slice(&crate::ipc::AI_OP_STREAM_END.to_le_bytes());
        unsafe { sys_send_ipc(reply_to, end); }
    }
}

// ── Streaming callback state ──────────────────────────────────────────────────

static mut STREAM_TARGET_TID: usize = 0;

/// C-callable token callback.  Packs up to 23 bytes of token text into an
/// inline IPC message and delivers it to `STREAM_TARGET_TID`.
extern "C" fn token_callback(piece: *const u8, len: i32, _userdata: *mut ()) {
    if len <= 0 || piece.is_null() { return; }
    let len = (len as usize).min(23);
    let bytes = unsafe { core::slice::from_raw_parts(piece, len) };

    let mut data = [0u8; 32];
    data[0..8].copy_from_slice(&crate::ipc::AI_OP_TOKEN.to_le_bytes());
    data[8] = len as u8;
    data[9..9 + len].copy_from_slice(bytes);

    unsafe { sys_send_ipc(STREAM_TARGET_TID, data); }
}

// ── Buffer helper ─────────────────────────────────────────────────────────────

/// Bounds-checked append into a flat byte buffer.
fn buf_append(dst: &mut [u8], cursor: &mut usize, src: &[u8]) {
    let n = src.len().min(dst.len().saturating_sub(*cursor));
    if n > 0 {
        dst[*cursor..*cursor + n].copy_from_slice(&src[..n]);
        *cursor += n;
    }
}

/// Route simple, short queries to the fast model tier.
/// Returns true  → use fast model (Qwen3-0.6B, lower latency).
/// Returns false → use main model (Qwen3-8B, higher quality).
///
/// Heuristics (applied to the raw user prompt, before RAG injection):
///   • Prompt longer than 100 bytes → complex (main model).
///   • Contains a "complex intent" keyword → complex.
///   • Otherwise → simple (fast model).
fn is_simple_query(prompt: &[u8]) -> bool {
    if prompt.len() > 100 {
        return false;
    }
    // Keywords that indicate the user wants deep reasoning / code / explanation.
    // Only lowercase is checked — most terminal input is lowercase.
    const COMPLEX_KW: &[&[u8]] = &[
        b"code", b"write", b"impl", b"creat", b"expla", b"analyz",
        b"compar", b"differ", b"generat", b"debug", b"fix", b"refactor",
        b"reason", b"summar", b"translat", b"convert",
        b"how does", b"why does", b"what is the",
    ];
    for kw in COMPLEX_KW {
        if bytes_contains(prompt, kw) {
            return false;
        }
    }
    true
}

/// Case-sensitive byte substring search (haystack, needle).
fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() { return true; }
    if needle.len() > haystack.len() { return false; }
    haystack.windows(needle.len()).any(|w| w == needle)
}

// IPC Helpers and RAG Logic

#[repr(C)]
struct IpcMessage {
    sender: usize,
    data: [u8; 32],
}

const OP_STORE: u64 = 100;
const OP_RETRIEVE: u64 = 101;
const OP_VECTOR_SEARCH: u64 = 105;
const OP_STORE_VEC: u64 = 106;
const SEMANTIC_TID: usize = 2;
const NUMENOR_TID:  usize = 1; // Our own TID — always spawned first

/// Maximum embedding dimension supported in the static buffer.
/// Large enough for Qwen3-8B (4096-dim) and similar models.
/// Embeddings are clipped to SEMANTIC_EMBEDDING_DIM before sending to the
/// semantic store, keeping the on-disk format stable regardless of model swap.
const EMBEDDING_BUF_MAX: usize = 4096;
/// On-disk embedding dimension used by the semantic store (fixed format).
const SEMANTIC_EMBEDDING_DIM: usize = 384;
const VECTOR_RESULT_SLOTS: usize = 8;
const VECTOR_RESULT_SLOT_SIZE: usize = 40; // 32-byte hash + 4-byte f32 sim + 4-byte pad
const VECTOR_RESULT_BUF_SIZE: usize = VECTOR_RESULT_SLOTS * VECTOR_RESULT_SLOT_SIZE; // 320 bytes
// Minimum cosine similarity to accept a vector search result (f32 bits)
const SIMILARITY_THRESHOLD_BITS: u32 = 0x3F00_0000; // 0.5f32

/// Query semantic memory using vector similarity search.
/// Embeds `query_text`, finds the nearest stored entry (cosine ≥ 0.5),
/// retrieves its content into `out_buf`.  Returns bytes written.
unsafe fn query_semantic(query_text: &str, out_buf: &mut [u8]) -> usize {
    // Static buffers — guaranteed to be in BSS (low physical addresses, within valid_ptr range).
    // EMB_BUF sized for the largest supported model; only SEMANTIC_EMBEDDING_DIM floats
    // are forwarded to the semantic store to preserve the on-disk format.
    static mut EMB_BUF: [f32; EMBEDDING_BUF_MAX]  = [0f32; EMBEDDING_BUF_MAX];
    static mut RESULT_BUF: [u8; VECTOR_RESULT_BUF_SIZE] = [0u8; VECTOR_RESULT_BUF_SIZE];

    // 1. Generate embedding for the query text using the model's actual dim.
    let model_dim = llm_get_embedding_dim();
    let request_dim = if model_dim > 0 { model_dim } else { SEMANTIC_EMBEDDING_DIM as i32 };
    let n = llm_embedding(
        query_text.as_ptr(),
        query_text.len() as i32,
        EMB_BUF.as_mut_ptr(),
        request_dim,
    );
    if n <= 0 {
        return 0;
    }

    // 2. Send VECTOR_SEARCH (105):
    //    [Op:8][EmbPtr:8][Threshold_f32_bits:4 + pad:4][ResultBufPtr:8]
    // Always use SEMANTIC_EMBEDDING_DIM floats for the vector store (on-disk format).
    // If the model has more dims, we use only the first SEMANTIC_EMBEDDING_DIM.
    let mut msg_data = [0u8; 32];
    msg_data[0..8].copy_from_slice(&OP_VECTOR_SEARCH.to_le_bytes());
    msg_data[8..16].copy_from_slice(&(EMB_BUF.as_ptr() as u64).to_le_bytes());
    msg_data[16..20].copy_from_slice(&SIMILARITY_THRESHOLD_BITS.to_le_bytes()); // f32 bits
    // bytes 20-23 remain zero (pad)
    msg_data[24..32].copy_from_slice(&(RESULT_BUF.as_mut_ptr() as u64).to_le_bytes());

    sys_send_ipc(SEMANTIC_TID, msg_data);

    // 3. Wait for reply [Status:8][Count:8]
    let mut reply = IpcMessage { sender: 0, data: [0; 32] };
    sys_recv_ipc(&mut reply);

    let status = u64::from_le_bytes(reply.data[0..8].try_into().unwrap());
    let count  = u64::from_le_bytes(reply.data[8..16].try_into().unwrap());

    if status != 0 || count == 0 {
        return 0;
    }

    // 4. Retrieve best match — hash is at RESULT_BUF[0..32].
    let mut retrieve_data = [0u8; 32];
    retrieve_data[0..8].copy_from_slice(&OP_RETRIEVE.to_le_bytes());
    retrieve_data[8..16].copy_from_slice(&(RESULT_BUF.as_ptr() as u64).to_le_bytes()); // HashPtr
    retrieve_data[16..24].copy_from_slice(&(out_buf.as_mut_ptr() as u64).to_le_bytes()); // OutPtr
    retrieve_data[24..32].copy_from_slice(&(out_buf.len() as u64).to_le_bytes());        // OutLen

    sys_send_ipc(SEMANTIC_TID, retrieve_data);
    sys_recv_ipc(&mut reply);

    let status_ret = u64::from_le_bytes(reply.data[0..8].try_into().unwrap());
    let bytes_read = u64::from_le_bytes(reply.data[8..16].try_into().unwrap());

    if status_ret == 0 { bytes_read as usize } else { 0 }
}

/// Inform the kernel scheduler of our desired priority level.
/// SYS_SET_PRIORITY = 50; args: x0=tid, x1=priority.
unsafe fn sys_set_priority(tid: usize, priority: usize) {
    core::arch::asm!(
        "mov x8, #50",
        "svc #0",
        in("x0") tid,
        in("x1") priority,
        lateout("x0") _,
        out("x8") _,
        clobber_abi("system"),
    );
}

unsafe fn sys_send_ipc(target: usize, data: [u8; 32]) {
    let msg = IpcMessage { sender: 0, data };
    core::arch::asm!(
        "mov x8, #1",
        "svc #0",
        in("x0") target,
        in("x1") &msg as *const _ as usize,
        lateout("x0") _,
        out("x8") _,
        clobber_abi("system"),
    );
}

unsafe fn sys_recv_ipc(msg: &mut IpcMessage) {
    loop {
        let ret: isize;
        core::arch::asm!(
            "mov x8, #2",
            "svc #0",
            in("x0") msg as *mut _ as usize,
            lateout("x0") ret,
            out("x8") _,
            clobber_abi("system"),
        );
        // If success (0), verify sender?
        // For simplicity, we assume next message is the reply.
        // In real system, we check msg.sender == SEMANTIC_TID
        if ret == 0 {
            // Check if it's from Semantic (TID 2)?
            // if msg.sender == SEMANTIC_TID { break; }
            break; 
        }
        // Yield if empty?
        core::arch::asm!("mov x8, #3", "svc #0");
    }
}

/// Store a Q&A pair with its embedding vector in the semantic memory service.
/// Uses OP_STORE_VEC (106) so future queries can find it via cosine similarity.
unsafe fn store_qa_in_semantic(query: &[u8], response: &[u8]) {
    static mut STORE_BUF: [u8; 512]               = [0u8; 512];
    static mut STORE_EMB:  [f32; EMBEDDING_BUF_MAX] = [0f32; EMBEDDING_BUF_MAX];

    // Build "Q: ... \n A: ..." text blob.
    let prefix_q = b"Q: ";
    let mid_a    = b"\nA: ";
    let needed   = prefix_q.len() + query.len() + mid_a.len() + response.len();
    if needed > STORE_BUF.len() { return; }

    let mut c = 0usize;
    core::ptr::copy_nonoverlapping(prefix_q.as_ptr(), STORE_BUF.as_mut_ptr().add(c), prefix_q.len());
    c += prefix_q.len();
    core::ptr::copy_nonoverlapping(query.as_ptr(),    STORE_BUF.as_mut_ptr().add(c), query.len());
    c += query.len();
    core::ptr::copy_nonoverlapping(mid_a.as_ptr(),    STORE_BUF.as_mut_ptr().add(c), mid_a.len());
    c += mid_a.len();
    core::ptr::copy_nonoverlapping(response.as_ptr(), STORE_BUF.as_mut_ptr().add(c), response.len());
    c += response.len();

    // Generate embedding for the question text (query only — keeps the embedding
    // semantically focused on what was asked, not the answer phrasing).
    // Clip to SEMANTIC_EMBEDDING_DIM for on-disk format compatibility.
    let n = llm_embedding(
        query.as_ptr(),
        query.len() as i32,
        STORE_EMB.as_mut_ptr(),
        SEMANTIC_EMBEDDING_DIM as i32,
    );

    let mut reply = IpcMessage { sender: 0, data: [0; 32] };

    if n > 0 {
        // STORE_VEC (106): [Op:8][DataPtr:8][DataLen:8][EmbPtr:8]
        let mut msg_data = [0u8; 32];
        msg_data[0..8].copy_from_slice(&OP_STORE_VEC.to_le_bytes());
        msg_data[8..16].copy_from_slice(&(STORE_BUF.as_ptr() as u64).to_le_bytes());
        msg_data[16..24].copy_from_slice(&(c as u64).to_le_bytes());
        msg_data[24..32].copy_from_slice(&(STORE_EMB.as_ptr() as u64).to_le_bytes());
        sys_send_ipc(SEMANTIC_TID, msg_data);
    } else {
        // Embedding failed — fall back to plain store (no vector index).
        let mut msg_data = [0u8; 32];
        msg_data[0..8].copy_from_slice(&OP_STORE.to_le_bytes());
        msg_data[8..16].copy_from_slice(&(STORE_BUF.as_ptr() as u64).to_le_bytes());
        msg_data[16..24].copy_from_slice(&(c as u64).to_le_bytes());
        sys_send_ipc(SEMANTIC_TID, msg_data);
    }

    // Drain the reply so the semantic service is unblocked for next request.
    sys_recv_ipc(&mut reply);
}

