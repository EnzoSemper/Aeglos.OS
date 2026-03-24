//! NDP — IPv6 Neighbor Discovery Protocol (RFC 4861) + SLAAC (RFC 4862).
//!
//! Implements:
//!   - Neighbor Cache (16 entries)
//!   - Neighbor Solicitation (NS) / Advertisement (NA) for MAC resolution
//!   - Router Solicitation (RS) to trigger Router Advertisement
//!   - Router Advertisement (RA) parsing → SLAAC global unicast configuration
//!   - Blocking `ndp_resolve_blocking()` for use in TCP connect

extern crate alloc;

use super::{
    NET_IP6, NET_IP6G, NET_GW6, NET_IP6_UP, NET_MAC,
    mac_to_link_local, solicited_node_mc, mc_ipv6_to_mac, TX_BUF,
    array6, array16, ipv6_upper_checksum,
};
use crate::arch::aarch64::timer;

// ── ICMPv6 type codes ──────────────────────────────────────────────────────────
const ICMPV6_RS:  u8 = 133; // Router Solicitation
const ICMPV6_NS:  u8 = 135; // Neighbor Solicitation
const ICMPV6_NA:  u8 = 136; // Neighbor Advertisement

// NDP option types
const OPT_SRC_LL: u8 = 1; // Source Link-Layer Address
const OPT_TGT_LL: u8 = 2; // Target Link-Layer Address
const OPT_PREFIX: u8 = 3; // Prefix Information

const CACHE_SIZE: usize    = 16;
const PENDING_SIZE: usize  = 4;
const RETRY_MAX: u32       = 3;
const RETRY_TICKS: u32     = 15;  // 150 ms at 100 Hz

// ── Neighbor cache ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct NdpEntry {
    ip:    [u8; 16],
    mac:   [u8; 6],
    valid: bool,
}

const NDP_EMPTY: NdpEntry = NdpEntry { ip: [0;16], mac: [0;6], valid: false };

static mut CACHE: [NdpEntry; CACHE_SIZE] = [NDP_EMPTY; CACHE_SIZE];

#[derive(Clone, Copy)]
struct NdpPending {
    ip:          [u8; 16],
    retry_count: u32,
    next_retry:  u32,
    valid:       bool,
}

const PEND_EMPTY: NdpPending = NdpPending { ip: [0;16], retry_count: 0, next_retry: 0, valid: false };
static mut PENDING: [NdpPending; PENDING_SIZE] = [PEND_EMPTY; PENDING_SIZE];

// ── Public: init ──────────────────────────────────────────────────────────────

/// Called from net::init after link-local address is set.
/// Sends a Router Solicitation so the QEMU router sends an RA back.
pub(super) fn init() {
    send_rs();
}

// ── Public: learn (called from frame dispatcher) ───────────────────────────────

/// Opportunistically add (or refresh) an NDP cache entry.
pub fn ndp_learn(ip: [u8; 16], mac: [u8; 6]) {
    if ip == [0u8; 16] { return; }
    unsafe {
        // Update existing entry if present
        for e in CACHE.iter_mut() {
            if e.valid && e.ip == ip { e.mac = mac; return; }
        }
        // Insert in first free slot, then evict slot 0 (LRU-ish)
        for e in CACHE.iter_mut() {
            if !e.valid { e.ip = ip; e.mac = mac; e.valid = true; return; }
        }
        CACHE[0] = NdpEntry { ip, mac, valid: true };
    }
}

/// Look up MAC for an IPv6 address in the neighbor cache.  Non-blocking.
pub fn ndp_lookup(ip: &[u8; 16]) -> Option<[u8; 6]> {
    unsafe {
        CACHE.iter()
            .find(|e| e.valid && &e.ip == ip)
            .map(|e| e.mac)
    }
}

// ── Public: blocking resolve ──────────────────────────────────────────────────

/// Resolve IPv6 → MAC, blocking up to ~450 ms (3 retries × 150 ms).
pub fn ndp_resolve_blocking(ip: [u8; 16]) -> Option<[u8; 6]> {
    // Fast path: already in cache
    if let Some(mac) = ndp_lookup(&ip) { return Some(mac); }

    // Solicited-node multicast — the NA will arrive at our unicast address.
    send_ns(ip);

    let deadline = timer::tick_count() as u32 + 45; // 450 ms
    loop {
        super::poll_rx_only();
        if let Some(mac) = ndp_lookup(&ip) { return Some(mac); }
        let now = timer::tick_count() as u32;
        if now >= deadline { break; }
        // Busy-wait a bit between polls
        for _ in 0..1000 { core::hint::spin_loop(); }
    }
    None
}

// ── Public: tick (called from net::poll) ──────────────────────────────────────

pub fn ndp_tick() {
    let now = timer::tick_count() as u32;
    unsafe {
        for p in PENDING.iter_mut() {
            if !p.valid { continue; }
            if now < p.next_retry { continue; }
            if p.retry_count >= RETRY_MAX { p.valid = false; continue; }
            send_ns(p.ip);
            p.retry_count += 1;
            p.next_retry = now + RETRY_TICKS;
        }
    }
}

// ── Public: handle NA (called from icmpv6) ────────────────────────────────────

/// Process an incoming Neighbor Advertisement.
/// Extracts target address + TLL option and updates the cache.
pub fn handle_na(data: &[u8]) {
    // NA body: 4 reserved bytes, then target address (16 bytes), then options
    if data.len() < 20 { return; }
    let target = array16(&data[4..20]);
    // Parse options to find Target Link-Layer Address (type 2)
    let mac = parse_option_tll(&data[20..]);
    if let Some(m) = mac { ndp_learn(target, m); }
}

/// Process a Neighbor Solicitation targeted at us — send a Neighbor Advertisement.
pub fn handle_ns(src_ip: [u8; 16], src_mac: [u8; 6], data: &[u8]) {
    if data.len() < 20 { return; }
    let target = array16(&data[4..20]);
    let my_ll = unsafe { NET_IP6 };
    let my_g  = unsafe { NET_IP6G };
    if target != my_ll && target != my_g { return; }
    // Learn the solicitor
    ndp_learn(src_ip, src_mac);
    // Send NA back
    send_na(src_ip, src_mac, target);
}

// ── Public: handle RA (SLAAC) ─────────────────────────────────────────────────

/// Parse a Router Advertisement.  Extracts the first on-link prefix and
/// configures the global unicast address via stateless address autoconfiguration.
pub fn handle_ra(src_ip: [u8; 16], src_mac: [u8; 6], data: &[u8]) {
    // RA body: cur_hop_limit(1), M/O flags(1), router_lifetime(2), reachable(4), retrans(4) = 12
    if data.len() < 12 { return; }

    // Set default gateway from RA source
    unsafe { NET_GW6 = src_ip; }
    ndp_learn(src_ip, src_mac);

    // Parse options looking for Prefix Information (type 3, length 4×8=32 bytes)
    let mut opt_off = 12;
    while opt_off + 2 <= data.len() {
        let opt_type = data[opt_off];
        let opt_len  = (data[opt_off + 1] as usize) * 8; // in units of 8 bytes
        if opt_len == 0 || opt_off + opt_len > data.len() { break; }

        if opt_type == OPT_PREFIX && opt_len >= 32 {
            let prefix_len   = data[opt_off + 2];
            let flags        = data[opt_off + 3];
            let on_link      = (flags & 0x80) != 0; // L flag
            let auto         = (flags & 0x40) != 0; // A flag

            if on_link && auto && prefix_len == 64 {
                // Prefix is in bytes [opt_off+8 .. opt_off+24]
                let prefix = array16(&data[opt_off + 8..opt_off + 24]);
                // Build global address: prefix[0..8] + EUI-64[8..16]
                let mac = unsafe { NET_MAC };
                let ll  = mac_to_link_local(mac);
                let mut global = prefix;
                global[8..16].copy_from_slice(&ll[8..16]);

                unsafe {
                    NET_IP6G  = global;
                    NET_IP6_UP = true;
                }

                let uart = crate::drivers::uart::Uart::new();
                uart.puts("[net6] SLAAC global ");
                super::put_ip6(global);
                uart.puts("  gw ");
                super::put_ip6(src_ip);
                uart.puts("\r\n");
            }
        }
        opt_off += opt_len;
    }
}

// ── Send helpers ──────────────────────────────────────────────────────────────

/// Send a Router Solicitation to ff02::2 (all routers).
fn send_rs() {
    let my_ll  = unsafe { NET_IP6 };
    let my_mac = unsafe { NET_MAC };
    if my_ll == [0u8; 16] { return; }

    let all_routers: [u8; 16] = [0xFF,0x02,0,0, 0,0,0,0, 0,0,0,0, 0,0,0,2];
    let dst_mac = [0x33u8, 0x33, 0x00, 0x00, 0x00, 0x02];

    // ICMPv6 RS body: type(1) code(1) cksum(2) reserved(4)
    // + Source Link-Layer Address option (type=1, len=1, mac=6)  → total 16 bytes body
    let mut body = [0u8; 16];
    body[0] = ICMPV6_RS;  // type
    // code, reserved = 0
    // Option: source link-layer address
    body[8]  = OPT_SRC_LL;
    body[9]  = 1;          // length in units of 8 bytes = 8 bytes total
    body[10..16].copy_from_slice(&my_mac);

    let csum = ipv6_upper_checksum(&my_ll, &all_routers, 58, &body);
    body[2] = (csum >> 8) as u8; body[3] = csum as u8;

    build_and_send_v6(&my_ll, &all_routers, &dst_mac, &my_mac, &body);
}

/// Send a Neighbor Solicitation for `target` to the solicited-node multicast.
fn send_ns(target: [u8; 16]) {
    let my_ll  = unsafe { NET_IP6 };
    let my_mac = unsafe { NET_MAC };
    if my_ll == [0u8; 16] { return; }

    let sn_mc   = solicited_node_mc(&target);
    let dst_mac = mc_ipv6_to_mac(&sn_mc);

    // NS body: type(1) code(1) cksum(2) reserved(4) target(16)
    // + Source Link-Layer Address option (8 bytes)  → 32 bytes
    let mut body = [0u8; 32];
    body[0] = ICMPV6_NS;
    body[4..20].copy_from_slice(&target);
    body[20] = OPT_SRC_LL; body[21] = 1;
    body[22..28].copy_from_slice(&my_mac);

    let csum = ipv6_upper_checksum(&my_ll, &sn_mc, 58, &body);
    body[2] = (csum >> 8) as u8; body[3] = csum as u8;

    build_and_send_v6(&my_ll, &sn_mc, &dst_mac, &my_mac, &body);
}

/// Send a Neighbor Advertisement in response to an NS.
fn send_na(dst_ip: [u8; 16], dst_mac: [u8; 6], my_target: [u8; 16]) {
    let my_ll  = unsafe { NET_IP6 };
    let my_mac = unsafe { NET_MAC };

    // NA body: type(1) code(1) cksum(2) flags(4) target(16)
    // + Target Link-Layer Address option (8 bytes)  → 32 bytes
    // Flags: S=1 (solicited), O=1 (override)
    let mut body = [0u8; 32];
    body[0] = ICMPV6_NA;
    body[4] = 0x60; // S=1, O=1 (bits 31:30 of the 32-bit flags field)
    body[8..24].copy_from_slice(&my_target);
    body[24] = OPT_TGT_LL; body[25] = 1;
    body[26..32].copy_from_slice(&my_mac);

    let csum = ipv6_upper_checksum(&my_ll, &dst_ip, 58, &body);
    body[2] = (csum >> 8) as u8; body[3] = csum as u8;

    build_and_send_v6(&my_ll, &dst_ip, &dst_mac, &my_mac, &body);
}

/// Build an IPv6 + Ethernet frame and transmit it.
fn build_and_send_v6(
    src_ip:  &[u8; 16], dst_ip: &[u8; 16],
    dst_mac: &[u8; 6],  src_mac: &[u8; 6],
    body:    &[u8],
) {
    let payload_len = body.len();
    let frame_total = 14 + 40 + payload_len;
    if frame_total > 2048 { return; }

    unsafe {
        let tb = &mut TX_BUF;
        // Ethernet header
        tb[0..6].copy_from_slice(dst_mac);
        tb[6..12].copy_from_slice(src_mac);
        tb[12] = 0x86; tb[13] = 0xDD; // IPv6

        // IPv6 header (40 bytes)
        tb[14] = 0x60;            // version=6, TC=0, FL=0
        tb[15] = 0; tb[16] = 0; tb[17] = 0;
        tb[18] = (payload_len >> 8) as u8; tb[19] = payload_len as u8;
        tb[20] = 58;  // Next header = ICMPv6
        tb[21] = 255; // Hop limit
        tb[22..38].copy_from_slice(src_ip);
        tb[38..54].copy_from_slice(dst_ip);

        // Payload
        tb[54..54 + payload_len].copy_from_slice(body);

        crate::arch::aarch64::exceptions::disable_irqs();
        super::net_send_frame(&tb[..frame_total]);
        crate::arch::aarch64::exceptions::enable_irqs();
    }
}

// ── Option parser helpers ─────────────────────────────────────────────────────

fn parse_option_tll(opts: &[u8]) -> Option<[u8; 6]> {
    let mut i = 0;
    while i + 2 <= opts.len() {
        let opt_type = opts[i];
        let opt_len  = (opts[i + 1] as usize) * 8;
        if opt_len == 0 || i + opt_len > opts.len() { break; }
        if opt_type == OPT_TGT_LL && opt_len >= 8 {
            return Some(array6(&opts[i + 2..]));
        }
        i += opt_len;
    }
    None
}
