//! Dual-stack (IPv4 + IPv6) network stack for Aeglos OS.
//!
//! Submodules:
//!   arp     — ARP table + blocking resolve (IPv4)
//!   ndp     — NDP neighbour discovery + SLAAC (IPv6)
//!   icmp    — ICMPv4 echo reply + outbound ping
//!   icmpv6  — ICMPv6 echo reply + NDP dispatch
//!   udp     — 16-entry port dispatch table
//!   dhcp    — Stateful DHCP client
//!   tcp     — Dual-stack TCP (IPv4 + IPv6) state machine
//!   dns     — A + AAAA resolver with TTL cache
//!   http    — HTTP/1.1 GET client
//!   tls     — TLS 1.3 client with cert verification
//!   x509    — ASN.1 / X.509 certificate parser

use crate::drivers::virtio_net;
use crate::drivers::e1000;

pub mod arp;
pub mod ndp;
pub mod icmp;
pub mod icmpv6;
pub mod udp;
pub mod dhcp;
pub mod tcp;
pub mod dns;
pub mod http;
pub mod tls;
pub mod ws;
pub mod httpd;
pub mod x509;

// ── IP address type ───────────────────────────────────────────────────────────

/// A dual-stack IP address (IPv4 or IPv6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IpAddr {
    V4([u8; 4]),
    V6([u8; 16]),
}

impl IpAddr {
    pub fn is_v6(&self) -> bool { matches!(self, IpAddr::V6(_)) }

    pub fn v4(a: u8, b: u8, c: u8, d: u8) -> Self { IpAddr::V4([a, b, c, d]) }

    /// Return the inner IPv4 bytes, or None if this is IPv6.
    pub fn as_v4(&self) -> Option<[u8; 4]> {
        match self { IpAddr::V4(a) => Some(*a), _ => None }
    }
    /// Return the inner IPv6 bytes, or None if this is IPv4.
    pub fn as_v6(&self) -> Option<[u8; 16]> {
        match self { IpAddr::V6(a) => Some(*a), _ => None }
    }
}

// ── Shared statics ────────────────────────────────────────────────────────────

static mut NET_IP:    [u8; 4]  = [0; 4];
static mut NET_MASK:  [u8; 4]  = [0; 4];
static mut NET_GW:    [u8; 4]  = [0; 4];
static mut NET_MAC:   [u8; 6]  = [0; 6];
static mut NET_UP:    bool     = false;
/// True when the e1000 driver is active and should be used instead of virtio_net.
static mut USE_E1000: bool     = false;

// IPv6 link-local address (fe80::<EUI-64>) and global unicast (from SLAAC RA).
pub(super) static mut NET_IP6:  [u8; 16] = [0; 16];
pub(super) static mut NET_GW6:  [u8; 16] = [0; 16];
pub(super) static mut NET_IP6G: [u8; 16] = [0; 16]; // global unicast (SLAAC)
pub(super) static mut NET_IP6_UP: bool   = false;

static mut RX_BUF: [u8; 2048] = [0u8; 2048];
pub(super) static mut TX_BUF: [u8; 2048] = [0u8; 2048];

// ── Public API ────────────────────────────────────────────────────────────────

/// Return the current IPv4 address.
pub fn get_ip() -> [u8; 4] { unsafe { NET_IP } }

/// Return the current link-local IPv6 address.
pub fn get_ip6() -> [u8; 16] { unsafe { NET_IP6 } }

/// Return the current global unicast IPv6 address (from SLAAC), or all-zeros.
pub fn get_ip6_global() -> [u8; 16] { unsafe { NET_IP6G } }

/// Return true if IPv6 is configured.
pub fn ip6_up() -> bool { unsafe { NET_IP6_UP } }

/// Return the MAC address of the active network interface.
pub fn get_mac() -> [u8; 6] { unsafe { NET_MAC } }

/// Override the IP / mask / gateway (e.g., for static configuration).
pub fn set_ip(ip: [u8; 4], mask: [u8; 4], gw: [u8; 4]) {
    unsafe { NET_IP = ip; NET_MASK = mask; NET_GW = gw; }
}

/// Initialise the network stack.
pub fn init() {
    let uart = crate::drivers::uart::Uart::new();

    unsafe {
        if virtio_net::is_up() {
            NET_MAC   = virtio_net::mac();
            NET_UP    = true;
            USE_E1000 = false;
            uart.puts("[net]  VirtIO-net up  MAC=");
        } else if e1000::is_up() {
            NET_MAC   = e1000::mac();
            NET_UP    = true;
            USE_E1000 = true;
            uart.puts("[net]  e1000 NIC up  MAC=");
        } else {
            uart.puts("[net]  No NIC found — networking disabled\r\n");
            return;
        }
    }

    put_mac(unsafe { NET_MAC });
    uart.puts("\r\n");

    // Derive IPv6 link-local address from MAC (EUI-64).
    let mac = unsafe { NET_MAC };
    let ll = mac_to_link_local(mac);
    unsafe { NET_IP6 = ll; }
    uart.puts("[net6] link-local ");
    put_ip6(ll);
    uart.puts("\r\n");

    dhcp::init();   // registers on UDP port 68, sends DISCOVER
    dns::init();    // registers on UDP port 5300
    ndp::init();    // sends RS to solicit RA from router
}

/// Poll RX and drive all state machines. Called from 100 Hz timer IRQ.
pub fn poll() {
    if !unsafe { NET_UP } { return; }
    poll_rx_only();
    arp::arp_tick();
    dhcp::dhcp_tick();
    tcp::tcp_tick();
    dns::dns_tick();
    ndp::ndp_tick();
}

/// Send an ICMPv4 echo request. Returns RTT in ms or -1.
pub fn send_ping(dst_ip: [u8; 4], timeout_ms: u32) -> isize {
    icmp::send_ping(dst_ip, timeout_ms)
}

/// Send an ICMPv6 echo request. Returns RTT in ms or -1.
pub fn send_ping6(dst_ip: [u8; 16], timeout_ms: u32) -> isize {
    icmpv6::send_ping6(dst_ip, timeout_ms)
}

/// Send a UDP datagram (IPv4).
pub fn udp_send(dst_ip: [u8; 4], dst_port: u16, src_port: u16, data: &[u8]) -> bool {
    udp::udp_send(dst_ip, dst_port, src_port, data)
}

/// Register a UDP port handler.
pub fn udp_register(port: u16, handler: fn([u8;4], u16, &[u8])) -> bool {
    udp::udp_register(port, handler)
}

/// Broadcast a gratuitous ARP.
pub fn arp_announce() { arp::arp_announce(); }

// ── Internal: unified send/recv helpers ───────────────────────────────────────

/// Send a raw Ethernet frame via whichever NIC is active.
pub(crate) unsafe fn net_send_frame(data: &[u8]) -> bool {
    if USE_E1000 { e1000::send(data) } else { virtio_net::transmit(data) }
}

// ── Internal: RX-only poll ────────────────────────────────────────────────────

pub(super) fn poll_rx_only() {
    if !unsafe { NET_UP } { return; }
    unsafe {
        if USE_E1000 {
            loop {
                let n = e1000::recv(&mut RX_BUF);
                if n == 0 { break; }
                handle_frame(&RX_BUF[..n]);
            }
        } else {
            loop {
                match virtio_net::receive(&mut RX_BUF) {
                    None    => break,
                    Some(n) => handle_frame(&RX_BUF[..n]),
                }
            }
        }
    }
}

// ── Frame dispatcher ──────────────────────────────────────────────────────────

fn handle_frame(frame: &[u8]) {
    if frame.len() < 14 { return; }
    let dst_mac   = &frame[0..6];
    let my_mac    = unsafe { NET_MAC };
    let is_us     = dst_mac == my_mac
                 || dst_mac == &[0xFF; 6]
                 || (dst_mac[0] == 0x33 && dst_mac[1] == 0x33); // IPv6 multicast
    if !is_us { return; }

    let src_mac   = &frame[6..12];
    let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
    let payload   = &frame[14..];

    match ethertype {
        0x0806 => arp::handle_arp(src_mac, payload),
        0x0800 => handle_ipv4(src_mac, payload),
        0x86DD => handle_ipv6(src_mac, payload),
        _      => {}
    }
}

fn handle_ipv4(src_mac: &[u8], payload: &[u8]) {
    if payload.len() < 20 { return; }
    if payload[0] >> 4 != 4 { return; }
    let ihl       = ((payload[0] & 0xF) * 4) as usize;
    let total_len = u16::from_be_bytes([payload[2], payload[3]]) as usize;
    if payload.len() < ihl || total_len > payload.len() { return; }

    let proto  = payload[9];
    let src_ip = array4(&payload[12..16]);
    let dst_ip = array4(&payload[16..20]);
    let my_ip  = unsafe { NET_IP };

    if dst_ip != my_ip && dst_ip != [255, 255, 255, 255] && my_ip != [0, 0, 0, 0] { return; }
    arp::arp_learn(src_ip, array6(src_mac));

    let ip_payload = &payload[ihl..total_len];
    match proto {
        1  => icmp::handle_icmp(src_mac, src_ip, ip_payload),
        6  => tcp::tcp_handle_segment(IpAddr::V4(src_ip), ip_payload),
        17 => udp::handle_udp_dispatch(src_ip, ip_payload),
        _  => {}
    }
}

/// Handle an incoming IPv6 packet.
fn handle_ipv6(src_mac: &[u8], payload: &[u8]) {
    if payload.len() < 40 { return; }
    if payload[0] >> 4 != 6 { return; }

    let payload_len = u16::from_be_bytes([payload[4], payload[5]]) as usize;
    let next_header = payload[6];
    let src_ip      = array16(&payload[8..24]);
    let dst_ip      = array16(&payload[24..40]);

    if payload.len() < 40 + payload_len { return; }
    let ip_payload = &payload[40..40 + payload_len];

    // Accept packets addressed to us (link-local or global) or multicast.
    let my_ll  = unsafe { NET_IP6 };
    let my_g   = unsafe { NET_IP6G };
    let is_mc  = dst_ip[0] == 0xFF;
    let is_us  = dst_ip == my_ll || (my_g != [0u8;16] && dst_ip == my_g) || is_mc;
    if !is_us { return; }

    // Learn the sender's link-layer address (opportunistic NDP learning).
    ndp::ndp_learn(src_ip, array6(src_mac));

    match next_header {
        58  => icmpv6::handle_icmpv6(src_mac, src_ip, dst_ip, ip_payload),
        6   => tcp::tcp_handle_segment(IpAddr::V6(src_ip), ip_payload),
        _   => {}
    }
}

// ── IPv6 address helpers ──────────────────────────────────────────────────────

/// Derive the EUI-64-based link-local address from a MAC address.
/// fe80::/10 + 54 bits of padding + EUI-64 (RFC 4291 §2.5.6)
pub(super) fn mac_to_link_local(mac: [u8; 6]) -> [u8; 16] {
    let mut addr = [0u8; 16];
    addr[0] = 0xFE; addr[1] = 0x80; // prefix fe80::/10
    // EUI-64 from MAC (48-bit → 64-bit):
    // Copy OUI, insert 0xFF 0xFE in the middle, flip universal/local bit.
    addr[8]  = mac[0] ^ 0x02; // flip U/L bit
    addr[9]  = mac[1];
    addr[10] = mac[2];
    addr[11] = 0xFF;
    addr[12] = 0xFE;
    addr[13] = mac[3];
    addr[14] = mac[4];
    addr[15] = mac[5];
    addr
}

/// Compute the solicited-node multicast address for a given IPv6 address.
/// ff02::1:ff<lower 24 bits of addr>
pub(super) fn solicited_node_mc(addr: &[u8; 16]) -> [u8; 16] {
    let mut mc = [0u8; 16];
    mc[0] = 0xFF; mc[1] = 0x02;
    mc[11] = 0x00; mc[12] = 0x01; mc[13] = 0xFF;
    mc[13] = addr[13]; mc[14] = addr[14]; mc[15] = addr[15];
    // Fix: ff02::1:ff<24 bits>
    mc[0]  = 0xFF; mc[1]  = 0x02;
    mc[11] = 0x00; mc[12] = 0x01; mc[13] = 0xFF;
    mc[13] = addr[13]; mc[14] = addr[14]; mc[15] = addr[15];
    mc
}

/// Compute the multicast Ethernet MAC for a given IPv6 multicast address.
/// 33:33:<lower 32 bits of IPv6 MC addr>
pub(super) fn mc_ipv6_to_mac(mc: &[u8; 16]) -> [u8; 6] {
    [0x33, 0x33, mc[12], mc[13], mc[14], mc[15]]
}

// ── Utility functions (accessible to child modules via super::) ───────────────

pub(super) fn ip_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() { sum += (data[i] as u32) << 8; }
    while sum >> 16 != 0 { sum = (sum & 0xFFFF) + (sum >> 16); }
    !(sum as u16)
}

/// One's-complement sum over a byte slice (for ICMPv6 / TCP-v6 pseudo-header).
pub(super) fn ones_complement_sum(data: &[u8], initial: u32) -> u16 {
    let mut sum = initial;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() { sum += (data[i] as u32) << 8; }
    while sum >> 16 != 0 { sum = (sum & 0xFFFF) + (sum >> 16); }
    !(sum as u16)
}

/// ICMPv6 / TCPv6 checksum using IPv6 pseudo-header (RFC 2460 §8.1).
pub(super) fn ipv6_upper_checksum(
    src: &[u8; 16], dst: &[u8; 16],
    next_header: u8, data: &[u8],
) -> u16 {
    let len = data.len() as u32;
    let mut sum: u32 = 0;
    // src and dst addresses
    for i in (0..16).step_by(2) {
        sum += u16::from_be_bytes([src[i], src[i+1]]) as u32;
        sum += u16::from_be_bytes([dst[i], dst[i+1]]) as u32;
    }
    // upper-layer packet length (u32 BE)
    sum += (len >> 16) & 0xFFFF;
    sum += len & 0xFFFF;
    // next header
    sum += next_header as u32;
    ones_complement_sum(data, sum)
}

#[inline]
pub(super) fn array4(s: &[u8]) -> [u8; 4] { [s[0], s[1], s[2], s[3]] }

#[inline]
pub(super) fn array6(s: &[u8]) -> [u8; 6] { [s[0], s[1], s[2], s[3], s[4], s[5]] }

#[inline]
pub(super) fn array16(s: &[u8]) -> [u8; 16] {
    let mut a = [0u8; 16];
    a.copy_from_slice(&s[..16]);
    a
}

pub fn put_ipaddr(ip: IpAddr) {
    match ip {
        IpAddr::V4(a) => put_ip(a),
        IpAddr::V6(a) => put_ip6(a),
    }
}

pub(super) fn put_ip(ip: [u8; 4]) {
    let uart = crate::drivers::uart::Uart::new();
    uart.put_dec(ip[0] as usize); uart.puts(".");
    uart.put_dec(ip[1] as usize); uart.puts(".");
    uart.put_dec(ip[2] as usize); uart.puts(".");
    uart.put_dec(ip[3] as usize);
}

pub(super) fn put_ip6(ip: [u8; 16]) {
    let uart = crate::drivers::uart::Uart::new();
    for i in (0..16).step_by(2) {
        let word = u16::from_be_bytes([ip[i], ip[i+1]]);
        uart.put_hex(word as usize);
        if i < 14 { uart.puts(":"); }
    }
}

pub(super) fn put_mac(mac: [u8; 6]) {
    let uart = crate::drivers::uart::Uart::new();
    for (i, b) in mac.iter().enumerate() {
        uart.put_hex(*b as usize);
        if i < 5 { uart.puts(":"); }
    }
}
