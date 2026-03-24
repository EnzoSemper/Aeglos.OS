//! Minimal TLS 1.3 client — TLS_AES_128_GCM_SHA256 + x25519.
//!
//! Certificate verification is SKIPPED (no CA bundle). This is acceptable
//! for the current OS stage; a pinned-cert or system-CA bundle can be added later.
//!
//! References: RFC 8446 (TLS 1.3)

extern crate alloc;
use alloc::vec::Vec;

use aes_gcm::{Aes128Gcm, KeyInit, aead::{Aead, Payload}};
use sha2::{Sha256, Digest};
use hmac::{Hmac, Mac};
use hkdf::Hkdf;
use x25519_dalek::{EphemeralSecret, PublicKey};

use super::tcp::{
    tcp_connect, tcp_wait_established, tcp_write, tcp_read,
    tcp_wait_readable, tcp_close, tcp_readable, tcp_state, TcpState,
};
use super::dns::dns_resolve;

// Re-export HttpResult for https_get
pub use super::http::HttpResult;

// ── TLS record types ──────────────────────────────────────────────────────────
const RT_CHANGE_CIPHER_SPEC: u8 = 20;
const RT_ALERT:              u8 = 21;
const RT_HANDSHAKE:          u8 = 22;
const RT_APPLICATION_DATA:   u8 = 23;

// ── Handshake message types ───────────────────────────────────────────────────
const HT_CLIENT_HELLO:       u8 = 1;
const HT_SERVER_HELLO:       u8 = 2;
const HT_ENCRYPTED_EXTS:     u8 = 8;
const HT_CERTIFICATE:        u8 = 11;
const HT_CERT_VERIFY:        u8 = 15;
const HT_FINISHED:           u8 = 20;

// ── Extension types ───────────────────────────────────────────────────────────
const EXT_SERVER_NAME:        u16 = 0x0000;
const EXT_SUPPORTED_GROUPS:   u16 = 0x000A;
const EXT_SIG_ALGS:           u16 = 0x000D;
const EXT_SUPPORTED_VERSIONS: u16 = 0x002B;
const EXT_KEY_SHARE:          u16 = 0x0033;

const TLS_VERSION_12: u16 = 0x0303;
const TLS_VERSION_13: u16 = 0x0304;
const CIPHER_AES128_GCM_SHA256: u16 = 0x1301;
const GROUP_X25519: u16 = 0x001D;

// ── State ─────────────────────────────────────────────────────────────────────

struct TlsConn {
    id:              usize,     // TCP connection id
    transcript:      Sha256,    // running handshake hash
    server_hs_key:   [u8; 16],  // AES-128 key for server → client handshake
    server_hs_iv:    [u8; 12],  // IV base
    client_hs_key:   [u8; 16],
    client_hs_iv:    [u8; 12],
    server_app_key:  [u8; 16],
    server_app_iv:   [u8; 12],
    client_app_key:  [u8; 16],
    client_app_iv:   [u8; 12],
    server_seq:      u64,
    client_seq:      u64,
    app_server_seq:  u64,
    app_client_seq:  u64,
}

// ── Public entry point ────────────────────────────────────────────────────────

/// TLS 1.3 HTTPS GET. Writes response body into `buf`. Returns bytes written or error.
/// Perform a TLS 1.3 handshake over TCP and return an established `TlsConn`.
fn tls_handshake(host: &str, port: u16) -> Option<TlsConn> {
    let ip = dns_resolve(host)?;
    let id = tcp_connect(ip, port)?;
    if !tcp_wait_established(id, 25_000) {
        tcp_close(id);
        return None;
    }

    let secret = make_ephemeral_secret();
    let public = PublicKey::from(&secret);

    let client_random = make_random_32();
    let ch_body = build_client_hello(host, &client_random, public.as_bytes());

    let mut transcript = Sha256::new();
    transcript.update(&ch_body);

    let record = tls_record(RT_HANDSHAKE, &ch_body);
    if tcp_write(id, &record) == 0 {
        tcp_close(id);
        return None;
    }

    let mut rx_buf = [0u8; 8192];
    let n = read_record(id, &mut rx_buf, 15_000);
    if n == 0 {
        tcp_close(id);
        return None;
    }

    let server_pub = match parse_server_hello(&rx_buf[..n], &mut transcript) {
        Some(k) => k,
        None => { tcp_close(id); return None; }
    };

    let shared = secret.diffie_hellman(&PublicKey::from(server_pub));

    let transcript_hash = transcript.clone().finalize();
    let (s_hs_key, s_hs_iv, c_hs_key, c_hs_iv) =
        derive_handshake_keys(shared.as_bytes(), &transcript_hash);

    let mut conn = TlsConn {
        id,
        transcript,
        server_hs_key: s_hs_key, server_hs_iv: s_hs_iv,
        client_hs_key: c_hs_key, client_hs_iv: c_hs_iv,
        server_app_key: [0u8; 16], server_app_iv: [0u8; 12],
        client_app_key: [0u8; 16], client_app_iv: [0u8; 12],
        server_seq: 0, client_seq: 0,
        app_server_seq: 0, app_client_seq: 0,
    };

    let mut ss = [0u8; 32];
    ss.copy_from_slice(shared.as_bytes());

    if !read_server_handshake(&mut conn, &ss, host) {
        tcp_close(conn.id);
        return None;
    }

    if !send_client_finished(&mut conn) {
        tcp_close(conn.id);
        return None;
    }

    Some(conn)
}

pub fn tls_get(host: &str, path: &str, buf: &mut [u8]) -> HttpResult {
    let mut conn = match tls_handshake(host, 443) {
        Some(c) => c,
        None => return HttpResult::TcpError,
    };

    let mut req = [0u8; 1024];
    let req_len = build_http_request(&mut req, host, path);
    if !send_app_data(&mut conn, &req[..req_len]) {
        tcp_close(conn.id);
        return HttpResult::TcpError;
    }

    let result = read_http_response(&mut conn, buf);
    tcp_close(conn.id);
    result
}

/// Send an HTTPS POST and read the response body into `buf`.
/// `content_type` defaults to `"application/json"` if empty.
pub fn tls_post(host: &str, path: &str, content_type: &str,
                body: &[u8], buf: &mut [u8]) -> HttpResult {
    let mut conn = match tls_handshake(host, 443) {
        Some(c) => c,
        None => return HttpResult::TcpError,
    };

    let ct = if content_type.is_empty() { "application/json" } else { content_type };
    let mut req = [0u8; 2048];
    let req_len = build_http_post_request(&mut req, host, path, ct, body);
    if !send_app_data(&mut conn, &req[..req_len]) {
        tcp_close(conn.id);
        return HttpResult::TcpError;
    }

    let result = read_http_response(&mut conn, buf);
    tcp_close(conn.id);
    result
}

// ── Key material generation ───────────────────────────────────────────────────

fn make_random_32() -> [u8; 32] {
    crate::csprng::random_bytes_32()
}

fn make_ephemeral_secret() -> EphemeralSecret {
    EphemeralSecret::random_from_rng(crate::csprng::Csprng)
}

// ── TLS record helpers ────────────────────────────────────────────────────────

fn tls_record(record_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(5 + payload.len());
    v.push(record_type);
    v.push(0x03); v.push(0x01); // legacy version TLS 1.0 for compat
    let len = payload.len() as u16;
    v.push((len >> 8) as u8);
    v.push(len as u8);
    v.extend_from_slice(payload);
    v
}

fn read_record(id: usize, buf: &mut [u8], timeout_ms: u32) -> usize {
    // Read 5-byte header
    let mut hdr = [0u8; 5];
    let mut got = 0usize;
    while got < 5 {
        if !tcp_wait_readable(id, timeout_ms) { return 0; }
        let n = tcp_read(id, &mut hdr[got..]);
        if n == 0 { return 0; }
        got += n;
    }
    let payload_len = ((hdr[3] as usize) << 8) | hdr[4] as usize;
    if payload_len + 5 > buf.len() { return 0; }
    buf[..5].copy_from_slice(&hdr);
    let mut body_got = 0;
    while body_got < payload_len {
        if !tcp_wait_readable(id, timeout_ms) { break; }
        let n = tcp_read(id, &mut buf[5 + body_got..5 + payload_len]);
        if n == 0 { break; }
        body_got += n;
    }
    5 + body_got
}

// ── ClientHello builder ───────────────────────────────────────────────────────

fn build_client_hello(host: &str, random: &[u8; 32], x25519_pub: &[u8; 32]) -> Vec<u8> {
    let mut extensions = Vec::new();

    // Server Name Indication (SNI)
    {
        let hn = host.as_bytes();
        let sni_entry_len = 1 + 2 + hn.len(); // type(1) + len(2) + name
        let mut e = Vec::new();
        e.extend_from_slice(&(sni_entry_len as u16).to_be_bytes()); // list length
        e.push(0x00); // host_name type
        e.extend_from_slice(&(hn.len() as u16).to_be_bytes());
        e.extend_from_slice(hn);
        push_ext(&mut extensions, EXT_SERVER_NAME, &e);
    }

    // Supported Groups: x25519 + secp256r1
    {
        let mut e = Vec::new();
        e.extend_from_slice(&4u16.to_be_bytes()); // list len (2 groups × 2 bytes)
        e.extend_from_slice(&GROUP_X25519.to_be_bytes());
        e.extend_from_slice(&0x0017u16.to_be_bytes()); // secp256r1
        push_ext(&mut extensions, EXT_SUPPORTED_GROUPS, &e);
    }

    // Signature Algorithms
    {
        let mut e = Vec::new();
        let algs: &[u16] = &[0x0403, 0x0804, 0x0401, 0x0503, 0x0601];
        e.extend_from_slice(&((algs.len() * 2) as u16).to_be_bytes());
        for &a in algs { e.extend_from_slice(&a.to_be_bytes()); }
        push_ext(&mut extensions, EXT_SIG_ALGS, &e);
    }

    // Supported Versions: TLS 1.3 (and 1.2 as fallback)
    {
        let mut e = Vec::new();
        e.push(4u8); // list byte length
        e.extend_from_slice(&TLS_VERSION_13.to_be_bytes());
        e.extend_from_slice(&TLS_VERSION_12.to_be_bytes());
        push_ext(&mut extensions, EXT_SUPPORTED_VERSIONS, &e);
    }

    // Key Share: x25519
    {
        let mut ks = Vec::new();
        ks.extend_from_slice(&GROUP_X25519.to_be_bytes());
        ks.extend_from_slice(&(x25519_pub.len() as u16).to_be_bytes());
        ks.extend_from_slice(x25519_pub);
        let mut e = Vec::new();
        e.extend_from_slice(&(ks.len() as u16).to_be_bytes());
        e.extend_from_slice(&ks);
        push_ext(&mut extensions, EXT_KEY_SHARE, &e);
    }

    // Build ClientHello body
    let mut body = Vec::new();
    body.extend_from_slice(&TLS_VERSION_12.to_be_bytes()); // legacy version
    body.extend_from_slice(random);
    body.push(0u8); // session id length = 0
    // Cipher suites: 2 suites × 2 bytes = 4
    body.extend_from_slice(&4u16.to_be_bytes());
    body.extend_from_slice(&CIPHER_AES128_GCM_SHA256.to_be_bytes());
    body.extend_from_slice(&0x1302u16.to_be_bytes()); // TLS_AES_256_GCM_SHA384
    body.push(1u8); // compression methods count
    body.push(0u8); // null compression
    // Extensions
    body.extend_from_slice(&(extensions.len() as u16).to_be_bytes());
    body.extend_from_slice(&extensions);

    // Wrap in handshake header: type(1) + length(3)
    let mut hs = Vec::new();
    hs.push(HT_CLIENT_HELLO);
    let blen = body.len() as u32;
    hs.push((blen >> 16) as u8);
    hs.push((blen >> 8) as u8);
    hs.push(blen as u8);
    hs.extend_from_slice(&body);
    hs
}

fn push_ext(out: &mut Vec<u8>, ext_type: u16, data: &[u8]) {
    out.extend_from_slice(&ext_type.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
}

// ── ServerHello parser ────────────────────────────────────────────────────────

fn parse_server_hello(data: &[u8], transcript: &mut Sha256) -> Option<[u8; 32]> {
    // data is: record_header(5) + handshake_message
    if data.len() < 9 { return None; }
    let rt = data[0];
    if rt != RT_HANDSHAKE { return None; }
    let hs_data = &data[5..];
    if hs_data.is_empty() || hs_data[0] != HT_SERVER_HELLO { return None; }

    // Update transcript with the ServerHello handshake message
    transcript.update(hs_data);

    let body_start = 4usize; // after type(1) + length(3)
    if hs_data.len() < body_start { return None; }
    let body = &hs_data[body_start..];
    // ServerHello: version(2) + random(32) + session_id_len(1) + session_id + cipher(2) + comp(1) + ext_len(2) + exts
    if body.len() < 38 { return None; }
    let mut pos = 2 + 32; // skip version + random
    let sid_len = body[pos] as usize;
    pos += 1 + sid_len;
    if pos + 5 > body.len() { return None; }
    pos += 2 + 1; // cipher + compression
    let ext_len = u16::from_be_bytes([body[pos], body[pos+1]]) as usize;
    pos += 2;
    if pos + ext_len > body.len() { return None; }
    let exts = &body[pos..pos + ext_len];

    // Find key_share extension
    let mut epos = 0;
    while epos + 4 <= exts.len() {
        let etype = u16::from_be_bytes([exts[epos], exts[epos+1]]);
        let elen  = u16::from_be_bytes([exts[epos+2], exts[epos+3]]) as usize;
        epos += 4;
        if etype == EXT_KEY_SHARE && epos + elen <= exts.len() {
            let kse = &exts[epos..epos + elen];
            if kse.len() >= 4 {
                let group = u16::from_be_bytes([kse[0], kse[1]]);
                let klen  = u16::from_be_bytes([kse[2], kse[3]]) as usize;
                if group == GROUP_X25519 && klen == 32 && kse.len() >= 4 + klen {
                    let mut k = [0u8; 32];
                    k.copy_from_slice(&kse[4..4 + 32]);
                    return Some(k);
                }
            }
        }
        epos += elen;
    }
    None
}

// ── Key schedule ──────────────────────────────────────────────────────────────

fn hkdf_expand_label(secret: &[u8], label: &[u8], context: &[u8], len: usize) -> Vec<u8> {
    // HkdfLabel = length(2) + label_len(1) + "tls13 " + label + ctx_len(1) + ctx
    let mut full_label = Vec::new();
    full_label.extend_from_slice(b"tls13 ");
    full_label.extend_from_slice(label);

    let mut info = Vec::new();
    info.extend_from_slice(&(len as u16).to_be_bytes());
    info.push(full_label.len() as u8);
    info.extend_from_slice(&full_label);
    info.push(context.len() as u8);
    info.extend_from_slice(context);

    let hk = Hkdf::<Sha256>::from_prk(secret).expect("valid prk");
    let mut out = alloc::vec![0u8; len];
    hk.expand(&info, &mut out).expect("expand ok");
    out
}

fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> Vec<u8> {
    let (prk, _) = Hkdf::<Sha256>::extract(Some(salt), ikm);
    prk.to_vec()
}

fn derive_handshake_keys(
    shared_secret: &[u8],
    transcript_hash: &[u8],
) -> ([u8; 16], [u8; 12], [u8; 16], [u8; 12]) {
    let zeros32 = [0u8; 32];

    // early_secret = HKDF-Extract(0, 0)
    let early_secret = hkdf_extract(&zeros32, &zeros32);
    // derived_secret = HKDF-Expand-Label(early_secret, "derived", SHA256(""), 32)
    let empty_hash = Sha256::digest(b"").to_vec();
    let derived = hkdf_expand_label(&early_secret, b"derived", &empty_hash, 32);
    // handshake_secret = HKDF-Extract(derived, shared_secret)
    let hs_secret = hkdf_extract(&derived, shared_secret);

    let s_hs_ts = hkdf_expand_label(&hs_secret, b"s hs traffic", transcript_hash, 32);
    let c_hs_ts = hkdf_expand_label(&hs_secret, b"c hs traffic", transcript_hash, 32);

    let s_key = to_16(&hkdf_expand_label(&s_hs_ts, b"key", b"", 16));
    let s_iv  = to_12(&hkdf_expand_label(&s_hs_ts, b"iv",  b"", 12));
    let c_key = to_16(&hkdf_expand_label(&c_hs_ts, b"key", b"", 16));
    let c_iv  = to_12(&hkdf_expand_label(&c_hs_ts, b"iv",  b"", 12));

    (s_key, s_iv, c_key, c_iv)
}

fn derive_app_keys(
    shared_secret: &[u8],
    transcript_hash: &[u8],
) -> ([u8; 16], [u8; 12], [u8; 16], [u8; 12]) {
    let zeros32 = [0u8; 32];
    let early_secret = hkdf_extract(&zeros32, &zeros32);
    let empty_hash = Sha256::digest(b"").to_vec();
    let derived = hkdf_expand_label(&early_secret, b"derived", &empty_hash, 32);
    let hs_secret = hkdf_extract(&derived, shared_secret);

    let hs_derived = hkdf_expand_label(&hs_secret, b"derived", &empty_hash, 32);
    let master_secret = hkdf_extract(&hs_derived, &zeros32);

    let s_app_ts = hkdf_expand_label(&master_secret, b"s ap traffic", transcript_hash, 32);
    let c_app_ts = hkdf_expand_label(&master_secret, b"c ap traffic", transcript_hash, 32);

    let s_key = to_16(&hkdf_expand_label(&s_app_ts, b"key", b"", 16));
    let s_iv  = to_12(&hkdf_expand_label(&s_app_ts, b"iv",  b"", 12));
    let c_key = to_16(&hkdf_expand_label(&c_app_ts, b"key", b"", 16));
    let c_iv  = to_12(&hkdf_expand_label(&c_app_ts, b"iv",  b"", 12));

    (s_key, s_iv, c_key, c_iv)
}

fn to_16(v: &[u8]) -> [u8; 16] { let mut a = [0u8; 16]; a.copy_from_slice(&v[..16]); a }
fn to_12(v: &[u8]) -> [u8; 12] { let mut a = [0u8; 12]; a.copy_from_slice(&v[..12]); a }

// ── AES-128-GCM encrypt/decrypt ───────────────────────────────────────────────

fn aes_gcm_decrypt(key: &[u8; 16], iv_base: &[u8; 12], seq: u64, ciphertext: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    let nonce = make_nonce(iv_base, seq);
    let cipher = Aes128Gcm::new_from_slice(key).ok()?;
    let nonce_ga = aes_gcm::Nonce::from_slice(&nonce);
    cipher.decrypt(nonce_ga, Payload { msg: ciphertext, aad }).ok()
}

fn aes_gcm_encrypt(key: &[u8; 16], iv_base: &[u8; 12], seq: u64, plaintext: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    let nonce = make_nonce(iv_base, seq);
    let cipher = Aes128Gcm::new_from_slice(key).ok()?;
    let nonce_ga = aes_gcm::Nonce::from_slice(&nonce);
    cipher.encrypt(nonce_ga, Payload { msg: plaintext, aad }).ok()
}

fn make_nonce(iv_base: &[u8; 12], seq: u64) -> [u8; 12] {
    let mut nonce = *iv_base;
    let seq_bytes = seq.to_be_bytes();
    for i in 0..8 { nonce[4 + i] ^= seq_bytes[i]; }
    nonce
}

// ── Server handshake reader ───────────────────────────────────────────────────

fn read_server_handshake(conn: &mut TlsConn, shared_secret: &[u8; 32], host: &str) -> bool {
    let mut raw = [0u8; 16384];
    let mut got_server_finished = false;
    let mut app_keys_derived = false;

    for _ in 0..20 {
        let n = read_record(conn.id, &mut raw, 10_000);
        if n < 5 { break; }

        let outer_type = raw[0];
        let payload_len = ((raw[3] as usize) << 8) | raw[4] as usize;
        let payload = &raw[5..5 + payload_len.min(n.saturating_sub(5))];

        // Change-cipher-spec compatibility records: ignore
        if outer_type == RT_CHANGE_CIPHER_SPEC { continue; }

        if outer_type == RT_ALERT {
            return false;
        }

        // All post-ServerHello messages are encrypted ApplicationData records
        if outer_type != RT_APPLICATION_DATA { continue; }

        let aad: [u8; 5] = [RT_APPLICATION_DATA, 0x03, 0x03,
                             (payload.len() >> 8) as u8, payload.len() as u8];
        let pt = match aes_gcm_decrypt(
            &conn.server_hs_key, &conn.server_hs_iv, conn.server_seq, payload, &aad,
        ) {
            Some(v) => { conn.server_seq += 1; v }
            None => return false,
        };
        if pt.is_empty() { continue; }
        let inner_type = *pt.last().unwrap();
        let data = &pt[..pt.len() - 1];

        if inner_type == RT_HANDSHAKE {
            // May contain multiple handshake messages
            let mut hpos = 0;
            while hpos + 4 <= data.len() {
                let ht   = data[hpos];
                let hlen = ((data[hpos+1] as usize) << 16)
                         | ((data[hpos+2] as usize) << 8)
                         |  (data[hpos+3] as usize);
                let hend = hpos + 4 + hlen;
                if hend > data.len() { break; }

                let hs_msg = &data[hpos..hend];

                match ht {
                    HT_ENCRYPTED_EXTS | HT_CERT_VERIFY => {
                        conn.transcript.update(hs_msg);
                    }
                    HT_CERTIFICATE => {
                        // Certificate verification: hostname + validity (RFC 2818 / RFC 5280).
                        // msg_body starts after the 4-byte handshake header.
                        let msg_body = if hs_msg.len() > 4 { &hs_msg[4..] } else { &[] };
                        if !super::x509::verify_cert_chain(msg_body, host) {
                            return false;
                        }
                        conn.transcript.update(hs_msg);
                    }
                    HT_FINISHED => {
                        // Server Finished: update transcript
                        conn.transcript.update(hs_msg);
                        got_server_finished = true;

                        // Derive application traffic keys from transcript AFTER server Finished
                        if !app_keys_derived {
                            let th = conn.transcript.clone().finalize();
                            let (s_app_k, s_app_iv, c_app_k, c_app_iv) =
                                derive_app_keys(shared_secret, &th);
                            conn.server_app_key = s_app_k;
                            conn.server_app_iv  = s_app_iv;
                            conn.client_app_key = c_app_k;
                            conn.client_app_iv  = c_app_iv;
                            app_keys_derived = true;
                        }
                    }
                    _ => {}
                }
                hpos = hend;
            }
        }

        if got_server_finished { break; }
    }

    got_server_finished && app_keys_derived
}

// ── Client Finished ───────────────────────────────────────────────────────────

fn send_client_finished(conn: &mut TlsConn) -> bool {
    let transcript_hash = conn.transcript.clone().finalize();

    // RFC 8446 §4.4.4: finished_key = HKDF-Expand-Label(BaseKey, "finished", "", Hash.length)
    // BaseKey = client_handshake_traffic_secret. We derive it from client_hs_key as PRK.
    let finished_key = hkdf_expand_label(&conn.client_hs_key, b"finished", b"", 32);

    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&finished_key).unwrap();
    mac.update(&transcript_hash);
    let verify_data = mac.finalize().into_bytes();

    // Build Finished handshake message
    let mut hs = Vec::new();
    hs.push(HT_FINISHED);
    hs.push(0); hs.push(0); hs.push(32u8); // 3-byte length = 32
    hs.extend_from_slice(&verify_data);

    // Update transcript with client Finished
    conn.transcript.update(&hs);

    // Encrypt as ApplicationData with inner type = RT_HANDSHAKE
    let mut pt = hs.clone();
    pt.push(RT_HANDSHAKE); // inner content type suffix
    let ct_len = pt.len() + 16; // plaintext + inner type + GCM tag
    let aad: [u8; 5] = [RT_APPLICATION_DATA, 0x03, 0x03,
                         (ct_len >> 8) as u8, ct_len as u8];
    let ct = match aes_gcm_encrypt(
        &conn.client_hs_key, &conn.client_hs_iv, conn.client_seq, &pt, &aad,
    ) {
        Some(v) => { conn.client_seq += 1; v }
        None => return false,
    };

    let mut record = Vec::new();
    record.push(RT_APPLICATION_DATA);
    record.push(0x03); record.push(0x03);
    record.extend_from_slice(&(ct.len() as u16).to_be_bytes());
    record.extend_from_slice(&ct);

    tcp_write(conn.id, &record) > 0
}

// ── Application data send/recv ────────────────────────────────────────────────

fn send_app_data(conn: &mut TlsConn, data: &[u8]) -> bool {
    let mut pt = data.to_vec();
    pt.push(RT_APPLICATION_DATA); // inner content type suffix
    let ct_len = pt.len() + 16;   // + GCM tag
    let aad: [u8; 5] = [RT_APPLICATION_DATA, 0x03, 0x03,
                         (ct_len >> 8) as u8, ct_len as u8];
    let ct = match aes_gcm_encrypt(
        &conn.client_app_key, &conn.client_app_iv, conn.app_client_seq, &pt, &aad,
    ) {
        Some(v) => { conn.app_client_seq += 1; v }
        None => return false,
    };

    let mut record = Vec::new();
    record.push(RT_APPLICATION_DATA);
    record.push(0x03); record.push(0x03);
    record.extend_from_slice(&(ct.len() as u16).to_be_bytes());
    record.extend_from_slice(&ct);

    tcp_write(conn.id, &record) > 0
}

fn read_http_response(conn: &mut TlsConn, buf: &mut [u8]) -> HttpResult {
    let mut body: Vec<u8> = Vec::new();

    for _ in 0..100 {
        if tcp_readable(conn.id) == 0 {
            if !tcp_wait_readable(conn.id, 8_000) {
                let st = tcp_state(conn.id);
                match st {
                    TcpState::CloseWait | TcpState::Closed
                    | TcpState::Free    | TcpState::TimeWait => break,
                    _ => break,
                }
            }
        }

        let mut raw = [0u8; 8192];
        let n = read_record(conn.id, &mut raw, 5_000);
        if n < 5 { break; }

        let outer_type = raw[0];
        let payload_len = ((raw[3] as usize) << 8) | raw[4] as usize;
        let payload = &raw[5..5 + payload_len.min(n.saturating_sub(5))];

        if outer_type == RT_ALERT { break; }
        if outer_type != RT_APPLICATION_DATA { continue; }

        let aad: [u8; 5] = [RT_APPLICATION_DATA, 0x03, 0x03,
                             (payload.len() >> 8) as u8, payload.len() as u8];
        let pt = match aes_gcm_decrypt(
            &conn.server_app_key, &conn.server_app_iv, conn.app_server_seq, payload, &aad,
        ) {
            Some(v) => { conn.app_server_seq += 1; v }
            None => break,
        };

        if pt.is_empty() { continue; }
        let inner_type = *pt.last().unwrap();
        if inner_type == RT_ALERT { break; }
        let data = &pt[..pt.len() - 1];
        body.extend_from_slice(data);

        // Check connection state after each record
        let st = tcp_state(conn.id);
        match st {
            TcpState::CloseWait | TcpState::Closed | TcpState::Free
            | TcpState::TimeWait | TcpState::FinWait2 => break,
            _ => {}
        }
    }

    parse_http_from_decrypted(&body, buf)
}

fn parse_http_from_decrypted(data: &[u8], buf: &mut [u8]) -> HttpResult {
    // Find end of headers (\r\n\r\n)
    let mut hdr_end = 0;
    for i in 0..data.len().saturating_sub(3) {
        if &data[i..i+4] == b"\r\n\r\n" { hdr_end = i + 4; break; }
    }
    if hdr_end == 0 { return HttpResult::TcpError; }

    // Parse status code from "HTTP/x.y NNN ..."
    let status = {
        let hdr = &data[..hdr_end];
        if hdr.len() < 12 || &hdr[..5] != b"HTTP/" { return HttpResult::TcpError; }
        let mut i = 5usize;
        while i < hdr.len() && hdr[i] != b' ' { i += 1; }
        i += 1; // skip space
        let mut code = 0u16;
        for _ in 0..3 {
            if i >= hdr.len() { return HttpResult::TcpError; }
            if hdr[i] < b'0' || hdr[i] > b'9' { return HttpResult::TcpError; }
            code = code * 10 + (hdr[i] - b'0') as u16;
            i += 1;
        }
        code
    };
    if !(200..300).contains(&status) { return HttpResult::HttpError(status); }

    let body = &data[hdr_end..];
    if body.len() > buf.len() { return HttpResult::BufferTooSmall; }
    buf[..body.len()].copy_from_slice(body);
    HttpResult::Ok(body.len())
}

fn build_http_request(out: &mut [u8; 1024], host: &str, path: &str) -> usize {
    let mut p = 0usize;
    macro_rules! w {
        ($s:expr) => { for &b in $s.as_bytes() { if p < out.len() { out[p] = b; p += 1; } } };
    }
    w!("GET "); w!(path); w!(" HTTP/1.1\r\nHost: "); w!(host);
    w!("\r\nConnection: close\r\nUser-Agent: Aeglos/1.0\r\n\r\n");
    p
}

fn build_http_post_request(out: &mut [u8; 2048], host: &str, path: &str,
                           content_type: &str, body: &[u8]) -> usize {
    let mut p = 0usize;
    macro_rules! w {
        ($s:expr) => { for &b in $s.as_bytes() { if p < out.len() { out[p] = b; p += 1; } } };
    }
    macro_rules! n {
        ($v:expr) => {{
            let mut tmp = [0u8; 20];
            let mut n = $v as usize;
            let mut i = 20;
            if n == 0 { i -= 1; tmp[i] = b'0'; } else {
                while n > 0 { i -= 1; tmp[i] = b'0' + (n % 10) as u8; n /= 10; }
            }
            for &b in &tmp[i..] { if p < out.len() { out[p] = b; p += 1; } }
        }};
    }
    w!("POST "); w!(path); w!(" HTTP/1.1\r\nHost: "); w!(host);
    w!("\r\nContent-Type: "); w!(content_type);
    w!("\r\nContent-Length: "); n!(body.len());
    w!("\r\nConnection: close\r\nUser-Agent: Aeglos/1.0\r\n\r\n");
    for &b in body { if p < out.len() { out[p] = b; p += 1; } }
    p
}
