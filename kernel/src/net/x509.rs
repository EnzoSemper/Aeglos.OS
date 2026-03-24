//! Minimal X.509 / ASN.1 DER certificate parser for TLS certificate verification.
//!
//! Implements:
//!   - ASN.1 DER tag-length-value walker
//!   - X.509 v3 certificate field extraction (CN, SAN, validity, SPKI)
//!   - Hostname verification per RFC 2818 / RFC 6125 (including wildcards)
//!   - Validity period verification against the PL031 RTC
//!
//! Security model:
//!   - Verifies the server certificate hostname matches the requested SNI.
//!   - Verifies the certificate is within its validity window.
//!   - Does NOT verify the CA signature chain (no embedded root certs / no
//!     RSA/ECDSA public-key crypto in the kernel).  A certificate that passes
//!     hostname + validity checks but was issued by an unknown or rogue CA will
//!     still be accepted.  Adding CA pinning (embed root DER + ECDSA verify)
//!     is the natural next step.
//!
//! References: RFC 5280 (X.509), RFC 4861, RFC 2818, RFC 6125.

// ── ASN.1 DER constants ───────────────────────────────────────────────────────

const TAG_INTEGER:     u8 = 0x02;
const TAG_BITSTRING:   u8 = 0x03;
#[allow(dead_code)]
const TAG_OCTETSTR:    u8 = 0x04;
const TAG_OID:         u8 = 0x06;
const TAG_UTF8STR:     u8 = 0x0C;
const TAG_PRINTSTR:    u8 = 0x13;
const TAG_T61STR:      u8 = 0x14;
const TAG_IA5STR:      u8 = 0x16;
const TAG_UTCTIME:     u8 = 0x17;
const TAG_GENTIME:     u8 = 0x18;
const TAG_SEQUENCE:    u8 = 0x30;
const TAG_SET:         u8 = 0x31;
const TAG_CTX0:        u8 = 0xA0; // [0] EXPLICIT — version
const TAG_CTX3:        u8 = 0xA3; // [3] EXPLICIT — extensions
const TAG_DNSNAME:     u8 = 0x82; // GeneralName [2] dNSName (context primitive)

// OIDs (raw DER encoding without tag/length)
const OID_CN:  &[u8] = &[0x55, 0x04, 0x03]; // 2.5.4.3
const OID_SAN: &[u8] = &[0x55, 0x1D, 0x11]; // 2.5.29.17

// ── DER walker ────────────────────────────────────────────────────────────────

/// Lightweight zero-copy ASN.1 DER element iterator.
struct Der<'a> {
    data: &'a [u8],
    pos:  usize,
}

impl<'a> Der<'a> {
    fn new(data: &'a [u8]) -> Self { Der { data, pos: 0 } }

    fn remaining(&self) -> bool { self.pos < self.data.len() }

    fn peek_tag(&self) -> Option<u8> {
        if self.pos < self.data.len() { Some(self.data[self.pos]) } else { None }
    }

    fn read_len(&mut self) -> Option<usize> {
        if self.pos >= self.data.len() { return None; }
        let b = self.data[self.pos]; self.pos += 1;
        if b < 0x80 { return Some(b as usize); }
        let n = (b & 0x7F) as usize;
        if n == 0 || n > 4 || self.pos + n > self.data.len() { return None; }
        let mut len = 0usize;
        for _ in 0..n {
            len = (len << 8) | self.data[self.pos] as usize;
            self.pos += 1;
        }
        Some(len)
    }

    /// Return the (tag, value) of the next TLV and advance past it.
    fn next_tlv(&mut self) -> Option<(u8, &'a [u8])> {
        if self.pos >= self.data.len() { return None; }
        let tag = self.data[self.pos]; self.pos += 1;
        let len = self.read_len()?;
        if self.pos + len > self.data.len() { return None; }
        let val = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Some((tag, val))
    }

    /// Skip the next TLV without returning its value.
    fn skip(&mut self) -> bool { self.next_tlv().is_some() }

    /// Record the byte-offset at which the NEXT TLV starts (before tag byte).
    fn current_offset(&self) -> usize { self.pos }

    /// Wrap a value slice as a sub-Der to iterate its contents.
    fn sub(val: &'a [u8]) -> Self { Der::new(val) }
}

// ── Time parsing ──────────────────────────────────────────────────────────────

/// Parse two ASCII digit bytes into a u32.
fn parse2(a: u8, b: u8) -> Option<u32> {
    if a.is_ascii_digit() && b.is_ascii_digit() {
        Some((a - b'0') as u32 * 10 + (b - b'0') as u32)
    } else {
        None
    }
}

/// Is the given year a Gregorian leap year?
fn is_leap(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// Days in month (1-indexed month, 1-12).
fn days_in_month(m: u32, y: u32) -> u32 {
    match m {
        1|3|5|7|8|10|12 => 31,
        4|6|9|11        => 30,
        2               => if is_leap(y) { 29 } else { 28 },
        _               => 0,
    }
}

/// Convert a calendar date/time (UTC) to a Unix timestamp (seconds since 1970-01-01).
/// Returns 0 on parse failure.
fn ymd_hms_to_unix(year: u32, mon: u32, day: u32,
                   hour: u32, min: u32, sec: u32) -> u64 {
    if year < 1970 || mon < 1 || mon > 12 || day < 1 || day > 31 { return 0; }
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    for m in 1..mon {
        days += days_in_month(m, year) as u64;
    }
    days += (day - 1) as u64;
    days * 86400 + hour as u64 * 3600 + min as u64 * 60 + sec as u64
}

/// Parse ASN.1 UTCTime (YYMMDDHHMMSSZ) → Unix timestamp.
fn parse_utctime(v: &[u8]) -> u64 {
    if v.len() < 13 { return 0; }
    let yy  = parse2(v[0], v[1]).unwrap_or(99);
    let year = if yy <= 49 { 2000 + yy } else { 1900 + yy };
    let mon  = parse2(v[2], v[3]).unwrap_or(0);
    let day  = parse2(v[4], v[5]).unwrap_or(0);
    let hour = parse2(v[6], v[7]).unwrap_or(0);
    let min  = parse2(v[8], v[9]).unwrap_or(0);
    let sec  = parse2(v[10], v[11]).unwrap_or(0);
    ymd_hms_to_unix(year, mon, day, hour, min, sec)
}

/// Parse ASN.1 GeneralizedTime (YYYYMMDDHHMMSSZ) → Unix timestamp.
fn parse_gentime(v: &[u8]) -> u64 {
    if v.len() < 15 { return 0; }
    let year = parse2(v[0], v[1]).unwrap_or(0) * 100 + parse2(v[2], v[3]).unwrap_or(0);
    let mon  = parse2(v[4], v[5]).unwrap_or(0);
    let day  = parse2(v[6], v[7]).unwrap_or(0);
    let hour = parse2(v[8], v[9]).unwrap_or(0);
    let min  = parse2(v[10], v[11]).unwrap_or(0);
    let sec  = parse2(v[12], v[13]).unwrap_or(0);
    ymd_hms_to_unix(year, mon, day, hour, min, sec)
}

// ── RTC accessor ──────────────────────────────────────────────────────────────

fn rtc_now() -> u64 {
    use crate::memory::vmm::KERNEL_VA_OFFSET;
    let va = 0x0901_0000usize + KERNEL_VA_OFFSET;
    unsafe { (va as *const u32).read_volatile() as u64 }
}

// ── X.509 CertInfo ────────────────────────────────────────────────────────────

/// Parsed fields from a single X.509 certificate.
struct CertInfo<'a> {
    cn:         Option<&'a [u8]>,            // Subject Common Name (bytes, UTF-8 compatible)
    san:        [Option<&'a [u8]>; 8],       // Subject Alternative Name dNSNames
    san_count:  usize,
    not_before: u64,                          // Unix timestamp
    not_after:  u64,                          // Unix timestamp
}

impl<'a> CertInfo<'a> {
    fn empty() -> Self {
        CertInfo { cn: None, san: [None; 8], san_count: 0, not_before: 0, not_after: 0 }
    }
}

// ── Name parser ───────────────────────────────────────────────────────────────

/// Walk a Name (SEQUENCE OF RDNSequence) and extract the CN value bytes.
fn extract_cn<'a>(name_val: &'a [u8]) -> Option<&'a [u8]> {
    let mut d = Der::sub(name_val); // SEQUENCE OF RDN
    while d.remaining() {
        let (t_rdn, rdn_val) = d.next_tlv()?;
        if t_rdn != TAG_SET { continue; }
        let mut rdn = Der::sub(rdn_val);
        while rdn.remaining() {
            let (t_atv, atv_val) = rdn.next_tlv()?;
            if t_atv != TAG_SEQUENCE { continue; }
            let mut atv = Der::sub(atv_val);
            let (t_oid, oid_val) = atv.next_tlv()?;
            if t_oid != TAG_OID { continue; }
            if oid_val != OID_CN { continue; }
            // Next TLV is the value (any string type)
            let (tv, vv) = atv.next_tlv()?;
            match tv {
                TAG_UTF8STR | TAG_PRINTSTR | TAG_T61STR | TAG_IA5STR => return Some(vv),
                _ => {}
            }
        }
    }
    None
}

// ── SAN extension parser ──────────────────────────────────────────────────────

/// Walk the extnValue of a SubjectAltName extension and fill SANs.
fn extract_san<'a>(extn_val: &'a [u8], info: &mut CertInfo<'a>) {
    // extnValue is OCTET STRING wrapping a SEQUENCE OF GeneralName
    let mut d = Der::sub(extn_val);
    let (t, seq_val) = match d.next_tlv() { Some(x) => x, None => return };
    if t != TAG_SEQUENCE { return; }
    let mut seq = Der::sub(seq_val);
    while seq.remaining() && info.san_count < 8 {
        let (gn_tag, gn_val) = match seq.next_tlv() { Some(x) => x, None => break };
        if gn_tag == TAG_DNSNAME {
            info.san[info.san_count] = Some(gn_val);
            info.san_count += 1;
        }
    }
}

// ── Extensions parser ─────────────────────────────────────────────────────────

/// Walk [3] EXPLICIT Extensions and populate SAN in CertInfo.
fn parse_extensions<'a>(ext_val: &'a [u8], info: &mut CertInfo<'a>) {
    // ext_val is the body of [3] EXPLICIT: a SEQUENCE OF Extension
    let mut outer = Der::sub(ext_val);
    let (t_outer, seq_val) = match outer.next_tlv() { Some(x) => x, None => return };
    if t_outer != TAG_SEQUENCE { return; }
    let mut exts = Der::sub(seq_val);
    while exts.remaining() {
        let (t_ext, ext_body) = match exts.next_tlv() { Some(x) => x, None => break };
        if t_ext != TAG_SEQUENCE { continue; }
        let mut ext = Der::sub(ext_body);
        // extnID OID
        let (t_oid, oid_val) = match ext.next_tlv() { Some(x) => x, None => continue };
        if t_oid != TAG_OID { continue; }
        let is_san = oid_val == OID_SAN;
        // skip optional critical BOOLEAN
        if let Some(0x01) = ext.peek_tag() { ext.skip(); }
        // extnValue OCTET STRING
        let (t_oct, oct_val) = match ext.next_tlv() { Some(x) => x, None => continue };
        if t_oct != 0x04 { continue; } // TAG_OCTETSTR
        if is_san { extract_san(oct_val, info); }
    }
}

// ── TBSCertificate parser ─────────────────────────────────────────────────────

/// Parse a single X.509 certificate DER blob and extract fields into CertInfo.
fn parse_cert<'a>(der: &'a [u8]) -> Option<CertInfo<'a>> {
    let mut d = Der::new(der);
    // Certificate SEQUENCE
    let (t0, cert_val) = d.next_tlv()?;
    if t0 != TAG_SEQUENCE { return None; }
    let mut cert = Der::sub(cert_val);
    // TBSCertificate SEQUENCE
    let (t1, tbs_val) = cert.next_tlv()?;
    if t1 != TAG_SEQUENCE { return None; }
    let mut tbs = Der::sub(tbs_val);

    let mut info = CertInfo::empty();

    // version [0] EXPLICIT INTEGER OPTIONAL
    if tbs.peek_tag() == Some(TAG_CTX0) { tbs.skip(); }

    // serialNumber INTEGER
    let (t_ser, _) = tbs.next_tlv()?;
    if t_ser != TAG_INTEGER { return None; }

    // signature AlgorithmIdentifier SEQUENCE
    let (t_sig, _) = tbs.next_tlv()?;
    if t_sig != TAG_SEQUENCE { return None; }

    // issuer Name SEQUENCE
    let (t_iss, _) = tbs.next_tlv()?;
    if t_iss != TAG_SEQUENCE { return None; }

    // validity SEQUENCE
    let (t_val, val_body) = tbs.next_tlv()?;
    if t_val != TAG_SEQUENCE { return None; }
    let mut validity = Der::sub(val_body);
    let (t_nb, nb_val) = validity.next_tlv()?;
    info.not_before = match t_nb {
        TAG_UTCTIME => parse_utctime(nb_val),
        TAG_GENTIME => parse_gentime(nb_val),
        _ => 0,
    };
    let (t_na, na_val) = validity.next_tlv()?;
    info.not_after = match t_na {
        TAG_UTCTIME => parse_utctime(na_val),
        TAG_GENTIME => parse_gentime(na_val),
        _ => u64::MAX,
    };

    // subject Name SEQUENCE
    let (t_sub, sub_val) = tbs.next_tlv()?;
    if t_sub != TAG_SEQUENCE { return None; }
    info.cn = extract_cn(sub_val);

    // subjectPublicKeyInfo SEQUENCE — skip (future: hash for pinning)
    let (t_spki, _) = tbs.next_tlv()?;
    if t_spki != TAG_SEQUENCE { return None; }

    // optional: issuerUniqueID [1], subjectUniqueID [2]
    while let Some(tag) = tbs.peek_tag() {
        if tag == TAG_CTX3 { break; }
        tbs.skip();
    }

    // extensions [3] EXPLICIT OPTIONAL
    if tbs.peek_tag() == Some(TAG_CTX3) {
        let (_, ext_val) = tbs.next_tlv()?;
        parse_extensions(ext_val, &mut info);
    }

    Some(info)
}

// ── Hostname matching ─────────────────────────────────────────────────────────

/// Case-insensitive comparison of two ASCII byte slices.
fn ascii_eq_ci(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() { return false; }
    a.iter().zip(b.iter()).all(|(x, y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

/// Match a single DNS name pattern (from cert) against the requested hostname.
/// Supports wildcard in the leftmost label only (e.g. *.example.com).
fn dns_name_match(pattern: &[u8], host: &[u8]) -> bool {
    if pattern.starts_with(b"*.") {
        // Wildcard: right part of pattern must match the right part of host
        // after the first label.  Also the wildcard covers exactly one label.
        let pat_suffix = &pattern[2..]; // e.g. "example.com"
        // Find the first dot in host to skip the first label
        if let Some(dot) = host.iter().position(|&b| b == b'.') {
            let host_suffix = &host[dot + 1..];
            return ascii_eq_ci(pat_suffix, host_suffix);
        }
        // No dot in host — wildcard can't match a bare hostname (no label to skip)
        return false;
    }
    ascii_eq_ci(pattern, host)
}

/// Verify that the certificate's subject covers the given hostname.
/// Checks SANs first (if any), falls back to CN.
fn hostname_match(info: &CertInfo, host: &str) -> bool {
    let host_bytes = host.as_bytes();
    if info.san_count > 0 {
        // RFC 2818 §3.1: if SANs are present, CN must be ignored.
        for i in 0..info.san_count {
            if let Some(san) = info.san[i] {
                if dns_name_match(san, host_bytes) { return true; }
            }
        }
        return false;
    }
    // No SANs: fall back to CN
    if let Some(cn) = info.cn {
        return dns_name_match(cn, host_bytes);
    }
    false
}

// ── Validity check ────────────────────────────────────────────────────────────

fn validity_ok(info: &CertInfo) -> bool {
    let now = rtc_now();
    if now == 0 {
        // RTC not set (epoch 0) — skip validity check to avoid false failures
        return true;
    }
    now >= info.not_before && now <= info.not_after
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Verify the TLS 1.3 Certificate message body against the expected hostname.
///
/// `msg_body` is the Certificate message content starting AFTER the 4-byte
/// handshake header (type byte + 3-byte length).  Layout per RFC 8446 §4.4.2:
///
/// ```text
///   u8     context_len                    (0 for server certs)
///   u24    cert_list_len
///   repeat:
///     u24  cert_data_len
///     ...  cert DER bytes
///     u16  extensions_len
///     ...  extensions (ignored)
/// ```
///
/// Returns `true` if:
///   - At least one certificate is present.
///   - The leaf certificate's hostname (CN or SAN) matches `host`.
///   - The leaf certificate's validity window contains the current RTC time.
///
/// Returns `false` (rejects connection) if any check fails.
pub fn verify_cert_chain(msg_body: &[u8], host: &str) -> bool {
    if msg_body.len() < 4 { return false; }

    let context_len = msg_body[0] as usize;
    if msg_body.len() < 1 + context_len + 3 { return false; }
    let list_len = (msg_body[1 + context_len] as usize) << 16
                 | (msg_body[2 + context_len] as usize) << 8
                 | (msg_body[3 + context_len] as usize);
    let mut pos = 1 + context_len + 3;
    let list_end = pos + list_len;
    if list_end > msg_body.len() { return false; }

    // We only verify the LEAF certificate (index 0).
    if pos + 3 > list_end { return false; }
    let cert_len = (msg_body[pos] as usize) << 16
                 | (msg_body[pos + 1] as usize) << 8
                 | (msg_body[pos + 2] as usize);
    pos += 3;
    if pos + cert_len > list_end { return false; }
    let cert_der = &msg_body[pos..pos + cert_len];

    let info = match parse_cert(cert_der) {
        Some(i) => i,
        None => {
            let uart = crate::drivers::uart::Uart::new();
            uart.puts("[tls]  cert parse failed\r\n");
            return false;
        }
    };

    // Validity period check
    if !validity_ok(&info) {
        let uart = crate::drivers::uart::Uart::new();
        uart.puts("[tls]  cert expired or not yet valid\r\n");
        return false;
    }

    // Hostname check
    if !hostname_match(&info, host) {
        let uart = crate::drivers::uart::Uart::new();
        uart.puts("[tls]  cert hostname mismatch for: ");
        uart.puts(host);
        uart.puts("\r\n");
        return false;
    }

    true
}
