
#![no_std]

pub mod block_manager;
pub mod store;
pub mod index;
pub mod query;
pub mod hash;
pub mod metadata;
pub mod vector;

// Match Kernel Message struct layout exactly
#[repr(C)]
struct IpcMessage {
    sender: usize,
    data: [u8; 32],
}

static STORE: store::Store = store::Store::new();

/// IPC opcodes for the semantic service
const OP_STORE: u64 = 100;          // Store data (no metadata)
const OP_RETRIEVE: u64 = 101;       // Retrieve by hash
const OP_STORE_META: u64 = 102;     // Store data with metadata
const OP_SEARCH_TAG: u64 = 103;     // Search by tag
const OP_QUERY: u64 = 104;          // Keyword search in content
const OP_VECTOR_SEARCH: u64 = 105;  // Vector similarity search
const OP_STORE_VEC:     u64 = 106;  // Store data with embedding vector (no metadata)

fn send_reply(target: usize, data: [u8; 32]) {
    let reply = IpcMessage { sender: 0, data };
    unsafe {
        core::arch::asm!(
            "mov x8, #1",
            "svc #0",
            in("x0") target,
            in("x1") &reply as *const _ as usize,
            lateout("x0") _,
            out("x8") _,
            clobber_abi("system"),
        );
    }
}

fn recv_msg(msg: &mut IpcMessage) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "mov x8, #2",
            "svc #0",
            in("x0") msg as *mut _ as usize,
            lateout("x0") ret,
            out("x8") _,
            clobber_abi("system"),
        );
    }
    ret
}

fn sys_log(s: &[u8]) {
    unsafe {
        core::arch::asm!(
            "mov x8, #8",
            "svc #0",
            in("x0") s.as_ptr() as usize,
            in("x1") s.len(),
            lateout("x0") _,
            out("x8") _,
            clobber_abi("system"),
        );
    }
}

#[no_mangle]
pub fn main() -> ! {
    sys_log(b"[sem] starting\r\n");
    STORE.init();
    sys_log(b"[sem] init done, entering recv loop\r\n");

    loop {
        let mut msg = IpcMessage { sender: 0, data: [0; 32] };
        let ret = recv_msg(&mut msg);

        if ret != 0 {
            continue;
        }

        let op = u64::from_le_bytes(msg.data[0..8].try_into().unwrap_or([0; 8]));
        let sender = msg.sender;

        match op {
            OP_STORE => handle_store(sender, &msg.data),
            OP_RETRIEVE => handle_retrieve(sender, &msg.data),
            OP_STORE_META => handle_store_meta(sender, &msg.data),
            OP_SEARCH_TAG => handle_search_tag(sender, &msg.data),
            OP_QUERY         => handle_query(sender, &msg.data),
            OP_VECTOR_SEARCH => handle_vector_search(sender, &msg.data),
            OP_STORE_VEC     => handle_store_vec(sender, &msg.data),
            _ => {}
        }
    }
}

/// STORE (100): [Op:8][DataPtr:8][Len:8][HashBufPtr:8]
fn handle_store(sender: usize, data: &[u8; 32]) {
    let ptr_raw = read_u64(data, 8);
    let len     = read_u64(data, 16) as usize;
    let hash_raw = read_u64(data, 24);

    let status = if valid_ptr(ptr_raw, len as u64) && valid_ptr(hash_raw, 32) && len > 0 {
        let ptr      = ptr_raw as *const u8;
        let hash_buf = hash_raw as *mut u8;
        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
        match STORE.store_full(slice, None, None) {
            Ok(h) => {
                unsafe { core::ptr::copy_nonoverlapping(h.as_ptr(), hash_buf, 32); }
                0u64
            }
            Err(_) => 1u64,
        }
    } else {
        2u64
    };

    let mut reply = [0u8; 32];
    reply[0..8].copy_from_slice(&status.to_le_bytes());
    send_reply(sender, reply);
}

/// RETRIEVE (101): [Op:8][HashPtr:8][OutBufPtr:8][BufLen:8]
fn handle_retrieve(sender: usize, data: &[u8; 32]) {
    let hash_raw = read_u64(data, 8);
    let out_raw  = read_u64(data, 16);
    let out_len  = read_u64(data, 24) as usize;

    let (status, bytes_read) = if valid_ptr(hash_raw, 32) && valid_ptr(out_raw, out_len as u64) {
        let hash    = unsafe { &*(hash_raw as *const [u8; 32]) };
        let out_buf = unsafe { core::slice::from_raw_parts_mut(out_raw as *mut u8, out_len) };
        match STORE.retrieve(hash, out_buf) {
            Ok(sz) => (0u64, sz as u64),
            Err(_) => (1u64, 0u64),
        }
    } else {
        (2u64, 0u64)
    };

    let mut reply = [0u8; 32];
    reply[0..8].copy_from_slice(&status.to_le_bytes());
    reply[8..16].copy_from_slice(&bytes_read.to_le_bytes());
    send_reply(sender, reply);
}

/// STORE_META (102): [Op:8][DataPtr:8][Len:8][MetaPtr:8]
/// Hash is written to the first 32 bytes of the metadata's _padding area.
/// Caller provides a Metadata struct pointer. Hash returned in reply.
fn handle_store_meta(sender: usize, data: &[u8; 32]) {
    let ptr_raw  = read_u64(data, 8);
    let len      = read_u64(data, 16) as usize;
    let meta_raw = read_u64(data, 24);

    let (status, hash) = if valid_ptr(ptr_raw, len as u64) && len > 0 {
        let slice = unsafe { core::slice::from_raw_parts(ptr_raw as *const u8, len) };
        let meta = if valid_ptr(meta_raw, core::mem::size_of::<metadata::Metadata>() as u64) {
            Some(unsafe { &*(meta_raw as *const metadata::Metadata) })
        } else {
            None
        };
        match STORE.store_full(slice, meta, None) {
            Ok(h) => (0u64, h),
            Err(_) => (1u64, [0u8; 32]),
        }
    } else {
        (2u64, [0u8; 32])
    };

    // Reply: [Status:8][Hash0..23:24] (first 24 bytes of hash)
    let mut reply = [0u8; 32];
    reply[0..8].copy_from_slice(&status.to_le_bytes());
    reply[8..32].copy_from_slice(&hash[0..24]);
    send_reply(sender, reply);
}

/// SEARCH_TAG (103): [Op:8][TagPtr:8][TagLen:8][ResultBufPtr:8]
/// Searches metadata tags for a substring match.
/// Writes matching hashes (32 bytes each) to ResultBuf.
/// Reply: [Status:8][Count:8][0][0]
fn handle_search_tag(sender: usize, data: &[u8; 32]) {
    let tag_raw    = read_u64(data, 8);
    let tag_len    = read_u64(data, 16) as usize;
    let result_raw = read_u64(data, 24);

    let (status, count) = if valid_ptr(tag_raw, tag_len as u64) && tag_len > 0
                             && valid_ptr(result_raw, 256) {
        let tag        = unsafe { core::slice::from_raw_parts(tag_raw as *const u8, tag_len) };
        let result_buf = result_raw as *mut u8;
        let idx = index::Index::new(&STORE.bm);
        let (results, n) = idx.search_by_tag(tag);

        // Write hashes to result buffer (32 bytes each)
        for k in 0..n {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    results[k].hash.as_ptr(),
                    result_buf.add(k * 32),
                    32,
                );
            }
        }
        (0u64, n as u64)
    } else {
        (2u64, 0u64)
    };

    let mut reply = [0u8; 32];
    reply[0..8].copy_from_slice(&status.to_le_bytes());
    reply[8..16].copy_from_slice(&count.to_le_bytes());
    send_reply(sender, reply);
}

/// QUERY (104): [Op:8][KeywordPtr:8][KeywordLen:8][ResultBufPtr:8]
/// Searches content data for keyword matches.
/// Writes matching hashes (32 bytes each) to ResultBuf.
/// Reply: [Status:8][Count:8][0][0]
fn handle_query(sender: usize, data: &[u8; 32]) {
    let kw_raw     = read_u64(data, 8);
    let kw_len     = read_u64(data, 16) as usize;
    let result_raw = read_u64(data, 24);

    let (status, count) = if valid_ptr(kw_raw, kw_len as u64) && kw_len > 0
                             && valid_ptr(result_raw, 256) {
        let keyword    = unsafe { core::slice::from_raw_parts(kw_raw as *const u8, kw_len) };
        let result_buf = result_raw as *mut u8;
        let qe = query::QueryEngine::new(&STORE.bm);
        let (results, n) = qe.search_keyword(keyword);

        for k in 0..n {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    results[k].hash.as_ptr(),
                    result_buf.add(k * 32),
                    32,
                );
            }
        }
        (0u64, n as u64)
    } else {
        (2u64, 0u64)
    };

    let mut reply = [0u8; 32];
    reply[0..8].copy_from_slice(&status.to_le_bytes());
    reply[8..16].copy_from_slice(&count.to_le_bytes());
    send_reply(sender, reply);
}

/// VECTOR_SEARCH (105): [Op:8][EmbPtr:8][Threshold_f32_bits:4+pad:4][ResultBufPtr:8]
///
/// `EmbPtr`     — pointer to a 1536-byte Embedding (384 × f32, little-endian).
/// `Threshold`  — minimum cosine similarity (f32 bits packed in bytes 16-19).
/// `ResultBufPtr` — pointer to result buffer.
///
/// Each result slot written to the buffer is 40 bytes:
///   [Hash:32][Similarity_f32_bits:4][Pad:4]
///
/// Reply: [Status:8][Count:8]
fn handle_vector_search(sender: usize, data: &[u8; 32]) {
    let emb_raw    = read_u64(data, 8);
    // threshold packed as f32 bits in bytes 16-19
    let thr_bits   = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
    let threshold  = f32::from_bits(thr_bits);
    let result_raw = read_u64(data, 24);

    const EMB_SIZE: usize = 384 * 4; // 1536 bytes
    const RESULT_SLOT: usize = 40;   // 32-byte hash + 4-byte f32 + 4-byte pad
    const MAX_RESULTS: usize = 8;

    let (status, count) = if valid_ptr(emb_raw, EMB_SIZE as u64)
                           && valid_ptr(result_raw, (MAX_RESULTS * RESULT_SLOT) as u64) {
        let emb_bytes = unsafe { core::slice::from_raw_parts(emb_raw as *const u8, EMB_SIZE) };
        let query     = vector::Embedding::from_bytes(emb_bytes);

        let idx = index::Index::new(&STORE.bm);
        let (results, n) = idx.search_by_vector(&query, threshold);

        let result_buf = result_raw as *mut u8;
        for k in 0..n {
            unsafe {
                // Write 32-byte hash
                core::ptr::copy_nonoverlapping(
                    results[k].hash.as_ptr(),
                    result_buf.add(k * RESULT_SLOT),
                    32,
                );
                // Write similarity as f32 bits (4 bytes)
                let sim_bits = results[k].similarity.to_bits().to_le_bytes();
                core::ptr::copy_nonoverlapping(
                    sim_bits.as_ptr(),
                    result_buf.add(k * RESULT_SLOT + 32),
                    4,
                );
                // 4 bytes padding already zero from caller
            }
        }
        (0u64, n as u64)
    } else {
        (2u64, 0u64)
    };

    let mut reply = [0u8; 32];
    reply[0..8].copy_from_slice(&status.to_le_bytes());
    reply[8..16].copy_from_slice(&count.to_le_bytes());
    send_reply(sender, reply);
}

/// STORE_VEC (106): [Op:8][DataPtr:8][DataLen:8][EmbPtr:8]
/// Store data together with a 1536-byte embedding (no metadata).
/// Reply: [Status:8][Hash0..23:24] (first 24 bytes of hash)
fn handle_store_vec(sender: usize, data: &[u8; 32]) {
    let data_raw = read_u64(data, 8);
    let data_len = read_u64(data, 16) as usize;
    let emb_raw  = read_u64(data, 24);

    const EMB_SIZE: usize = 384 * 4; // 1536 bytes

    let (status, hash) = if valid_ptr(data_raw, data_len as u64) && data_len > 0
                            && valid_ptr(emb_raw, EMB_SIZE as u64) {
        let slice     = unsafe { core::slice::from_raw_parts(data_raw as *const u8, data_len) };
        let emb_bytes = unsafe { core::slice::from_raw_parts(emb_raw as *const u8, EMB_SIZE) };
        let embedding = vector::Embedding::from_bytes(emb_bytes);
        match STORE.store_full(slice, None, Some(&embedding)) {
            Ok(h)  => (0u64, h),
            Err(_) => (1u64, [0u8; 32]),
        }
    } else {
        (2u64, [0u8; 32])
    };

    let mut reply = [0u8; 32];
    reply[0..8].copy_from_slice(&status.to_le_bytes());
    reply[8..32].copy_from_slice(&hash[0..24]);
    send_reply(sender, reply);
}

fn read_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset+8].try_into().unwrap_or([0; 8]))
}

/// Validate that a (ptr, len) range lies within addressable RAM.
/// Accepts both physical addresses and kernel high-VA (TTBR1) addresses.
/// Rejects null, MMIO addresses, and anything that would wrap or exceed RAM.
fn valid_ptr(ptr: u64, len: u64) -> bool {
    const RAM_BASE: u64 = 0x4000_0000;
    const RAM_END:  u64 = 0x1_4000_0000; // 4 GiB at 1 GiB base (physical)
    // Kernel virtual address range: PA + KERNEL_VA_OFFSET (bit 63 set)
    const KVA_OFFSET: u64 = 0xFFFF_0000_0000_0000;
    const KVA_BASE:   u64 = KVA_OFFSET + RAM_BASE;
    const KVA_END:    u64 = KVA_OFFSET + RAM_END;

    // Normalise: map KVA → PA so we can do a single range check.
    let (base, end_limit) = if ptr >= KVA_BASE {
        (ptr - KVA_OFFSET, KVA_END - KVA_OFFSET)
    } else {
        (ptr, RAM_END)
    };

    if base < RAM_BASE || base >= end_limit {
        return false;
    }
    match base.checked_add(len) {
        Some(end) => end <= end_limit,
        None => false,
    }
}
