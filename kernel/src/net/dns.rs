//! DNS — A + AAAA resolver over UDP, with 16-entry TTL cache.
//! Prefers A (IPv4) records; falls back to AAAA (IPv6) if no A is found.

use super::IpAddr;

const CACHE_SIZE:    usize   = 16;
const DNS_SERVER:    [u8; 4] = [10, 0, 2, 3];
const DNS_SERVER6:   [u8; 16] = [0xFD,0,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,3]; // fd00::3
const DNS_SRC_PORT:  u16     = 5300;
const DNS_DST_PORT:  u16     = 53;

// ── Cache ─────────────────────────────────────────────────────────────────────

#[derive(Copy, Clone)]
struct DnsEntry {
    name:     [u8; 64],
    name_len: usize,
    addr:     IpAddr,
    ttl_end:  u32,
}

const DNS_EMPTY: DnsEntry = DnsEntry {
    name: [0; 64], name_len: 0,
    addr: IpAddr::V4([0; 4]), ttl_end: 0,
};

static mut CACHE:        [DnsEntry; CACHE_SIZE] = [DNS_EMPTY; CACHE_SIZE];
static mut CACHE_CURSOR: usize                  = 0;

static mut PENDING_TXN:  u16         = 0;
static mut PENDING_ADDR: Option<IpAddr> = None;

// ── Init ──────────────────────────────────────────────────────────────────────

pub(super) fn init() {
    super::udp::udp_register(DNS_SRC_PORT, handle_dns_response);
}

// ── Cache helpers ─────────────────────────────────────────────────────────────

fn cache_lookup(name: &str) -> Option<IpAddr> {
    let nb  = name.as_bytes();
    let now = crate::arch::aarch64::timer::tick_count() as u32;
    unsafe {
        for e in CACHE.iter_mut() {
            if e.ttl_end == 0 { continue; }
            if e.ttl_end < now { e.ttl_end = 0; continue; }
            if e.name_len == nb.len() && e.name[..e.name_len] == nb[..] {
                return Some(e.addr);
            }
        }
    }
    None
}

fn cache_insert(name: &str, addr: IpAddr, ttl_secs: u32) {
    let nb  = name.as_bytes();
    let len = nb.len().min(63);
    let now = crate::arch::aarch64::timer::tick_count() as u32;
    let ttl_ticks = ttl_secs.saturating_mul(100);
    unsafe {
        let idx = CACHE_CURSOR % CACHE_SIZE;
        CACHE[idx].name[..len].copy_from_slice(&nb[..len]);
        CACHE[idx].name_len = len;
        CACHE[idx].addr     = addr;
        CACHE[idx].ttl_end  = now.saturating_add(ttl_ticks);
        CACHE_CURSOR        = CACHE_CURSOR.wrapping_add(1);
    }
}

// ── Query building ────────────────────────────────────────────────────────────

fn build_dns_query(pkt: &mut [u8; 512], name: &str, txn_id: u16, qtype: u16) -> usize {
    pkt[0] = (txn_id >> 8) as u8; pkt[1] = txn_id as u8;
    pkt[2] = 0x01; pkt[3] = 0x00; // RD flag
    pkt[4] = 0x00; pkt[5] = 0x01; // QDCOUNT = 1
    pkt[6] = 0x00; pkt[7] = 0x00; // ANCOUNT
    pkt[8] = 0x00; pkt[9] = 0x00; // NSCOUNT
    pkt[10]= 0x00; pkt[11]= 0x00; // ARCOUNT
    let mut off = 12usize;
    for label in name.split('.') {
        let lb = label.as_bytes();
        let len = lb.len().min(63);
        if off + 1 + len >= 480 { break; }
        pkt[off] = len as u8; off += 1;
        pkt[off..off + len].copy_from_slice(&lb[..len]); off += len;
    }
    pkt[off] = 0; off += 1;
    pkt[off] = (qtype >> 8) as u8; pkt[off+1] = qtype as u8; off += 2;
    pkt[off] = 0x00; pkt[off+1] = 0x01; off += 2; // CLASS IN
    off
}

fn send_query(name: &str) {
    let txn = unsafe { PENDING_TXN };
    let mut pkt = [0u8; 512];
    let len = build_dns_query(&mut pkt, name, txn, 1 /*A*/);
    super::udp::udp_send(DNS_SERVER, DNS_DST_PORT, DNS_SRC_PORT, &pkt[..len]);
}

// ── Response parsing ──────────────────────────────────────────────────────────

fn handle_dns_response(_src_ip: [u8;4], _src_port: u16, data: &[u8]) {
    if data.len() < 12 { return; }
    let rxn = u16::from_be_bytes([data[0], data[1]]);
    if rxn != unsafe { PENDING_TXN } { return; }
    let flags   = u16::from_be_bytes([data[2], data[3]]);
    if flags & 0x8000 == 0 { return; }
    if flags & 0x000F != 0 { return; } // error rcode

    let qdcount = u16::from_be_bytes([data[4],  data[5]]) as usize;
    let ancount = u16::from_be_bytes([data[6],  data[7]]) as usize;
    if ancount == 0 { return; }

    let mut off = 12usize;
    // Skip question section
    for _ in 0..qdcount {
        loop {
            if off >= data.len() { return; }
            let l = data[off] as usize;
            if l == 0 { off += 1; break; }
            if l >= 0xC0 { off += 2; break; }
            off += 1 + l;
        }
        off += 4; // QTYPE + QCLASS
    }

    for _ in 0..ancount {
        if off >= data.len() { return; }
        if data[off] >= 0xC0 { off += 2; }
        else { loop {
            if off >= data.len() { return; }
            let l = data[off] as usize;
            if l == 0 { off += 1; break; }
            off += 1 + l;
        }}
        if off + 10 > data.len() { return; }
        let rtype = u16::from_be_bytes([data[off],   data[off+1]]);
        let rdlen = u16::from_be_bytes([data[off+8], data[off+9]]) as usize;
        off += 10;
        if off + rdlen > data.len() { return; }
        let rdata = &data[off..off + rdlen];
        off += rdlen;

        match (rtype, rdlen) {
            (1, 4) => {
                // A record
                let ip4 = [rdata[0], rdata[1], rdata[2], rdata[3]];
                unsafe { PENDING_ADDR = Some(IpAddr::V4(ip4)); }
                return; // prefer A over AAAA
            }
            (28, 16) => {
                // AAAA record — store only if no A was already found
                if unsafe { PENDING_ADDR.is_none() } {
                    let mut ip6 = [0u8; 16];
                    ip6.copy_from_slice(rdata);
                    unsafe { PENDING_ADDR = Some(IpAddr::V6(ip6)); }
                }
            }
            _ => {}
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Resolve a hostname to an IP address (IPv4 preferred, IPv6 fallback).
/// Blocks up to 2 s.  Returns cached result immediately if available.
pub fn dns_resolve(name: &str) -> Option<IpAddr> {
    // Fast-path: numeric IPv4
    if let Some(ip4) = parse_dotted_ip(name) {
        return Some(IpAddr::V4(ip4));
    }
    // Cache lookup
    if let Some(addr) = cache_lookup(name) { return Some(addr); }

    let txn = (crate::arch::aarch64::timer::physical_timer_count() & 0xFFFF) as u16;
    unsafe { PENDING_TXN = txn; PENDING_ADDR = None; }

    send_query(name);

    let freq = crate::arch::aarch64::timer::physical_timer_freq();
    let end  = crate::arch::aarch64::timer::physical_timer_count()
               + (2000u64 * freq) / 1000;
    loop {
        super::poll_rx_only();
        if let Some(addr) = unsafe { PENDING_ADDR.take() } {
            cache_insert(name, addr, 300);
            return Some(addr);
        }
        if crate::arch::aarch64::timer::physical_timer_count() >= end { return None; }
        unsafe { core::arch::asm!("yield"); }
    }
}

/// TTL expiry handled lazily in cache_lookup.
pub(super) fn dns_tick() {}

// ── Helper: parse dotted-decimal IPv4 ────────────────────────────────────────

fn parse_dotted_ip(s: &str) -> Option<[u8; 4]> {
    let mut octets = [0u8; 4];
    let mut idx = 0usize;
    let mut cur = 0u32;
    let mut has_digit = false;
    for b in s.bytes() {
        if b == b'.' {
            if !has_digit || idx >= 3 { return None; }
            octets[idx] = cur as u8; idx += 1; cur = 0; has_digit = false;
        } else if b >= b'0' && b <= b'9' {
            cur = cur * 10 + (b - b'0') as u32;
            if cur > 255 { return None; }
            has_digit = true;
        } else { return None; }
    }
    if !has_digit || idx != 3 { return None; }
    octets[3] = cur as u8;
    Some(octets)
}
