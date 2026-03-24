//! ICMPv6 — echo reply, echo request (ping6), and NDP dispatch.
//!
//! ICMPv6 message types handled:
//!   128 — Echo Request  → send Echo Reply
//!   129 — Echo Reply    → complete pending ping6
//!   133 — Router Solicitation  (sent by us, not received)
//!   134 — Router Advertisement → forward to NDP SLAAC handler
//!   135 — Neighbor Solicitation → forward to NDP
//!   136 — Neighbor Advertisement → forward to NDP

use super::{NET_IP6, NET_IP6G, NET_MAC, TX_BUF, ipv6_upper_checksum};
use crate::arch::aarch64::timer;

const ICMPV6_ECHO_REQUEST:  u8 = 128;
const ICMPV6_ECHO_REPLY:    u8 = 129;
const ICMPV6_RA:            u8 = 134;
const ICMPV6_NS:            u8 = 135;
const ICMPV6_NA:            u8 = 136;

// ── Ping6 state ───────────────────────────────────────────────────────────────

static mut PING6_ID:     u16       = 0xAE60;
static mut PING6_REPLY:  Option<u32> = None; // arrival tick count

// ── Public: dispatch ──────────────────────────────────────────────────────────

/// Dispatch an incoming ICMPv6 message to the appropriate handler.
pub fn handle_icmpv6(
    src_mac: &[u8],
    src_ip:  [u8; 16],
    dst_ip:  [u8; 16],
    data:    &[u8],
) {
    if data.len() < 4 { return; }
    let msg_type = data[0];
    let _code    = data[1];
    // Note: checksum validation is skipped (trusted internal network in QEMU)

    match msg_type {
        ICMPV6_ECHO_REQUEST => handle_echo_request(src_ip, dst_ip, data),
        ICMPV6_ECHO_REPLY   => handle_echo_reply(data),
        ICMPV6_RA           => {
            let mac = super::array6(src_mac);
            super::ndp::handle_ra(src_ip, mac, &data[4..]);
        }
        ICMPV6_NS => {
            let mac = super::array6(src_mac);
            super::ndp::handle_ns(src_ip, mac, &data[4..]);
        }
        ICMPV6_NA => {
            super::ndp::handle_na(&data[4..]);
        }
        _ => {}
    }
}

// ── Echo request ──────────────────────────────────────────────────────────────

fn handle_echo_request(src_ip: [u8; 16], my_ip: [u8; 16], data: &[u8]) {
    // data: type(1) code(1) cksum(2) id(2) seq(2) payload(...)
    if data.len() < 8 { return; }
    let my_mac = unsafe { NET_MAC };

    // Build reply: swap src/dst, set type=129
    let payload_len = data.len();
    let frame_total = 14 + 40 + payload_len;
    if frame_total > 2048 { return; }

    // Resolve dst MAC (the original sender)
    let dst_mac = match super::ndp::ndp_lookup(&src_ip) {
        Some(m) => m,
        None    => return,
    };

    unsafe {
        let tb = &mut TX_BUF;
        // Ethernet header
        tb[0..6].copy_from_slice(&dst_mac);
        tb[6..12].copy_from_slice(&my_mac);
        tb[12] = 0x86; tb[13] = 0xDD;

        // IPv6 header
        tb[14] = 0x60; tb[15] = 0; tb[16] = 0; tb[17] = 0;
        tb[18] = (payload_len >> 8) as u8; tb[19] = payload_len as u8;
        tb[20] = 58;   // ICMPv6
        tb[21] = 255;  // hop limit
        tb[22..38].copy_from_slice(&my_ip);
        tb[38..54].copy_from_slice(&src_ip);

        // ICMPv6 Echo Reply body (copy request, change type)
        tb[54..54 + payload_len].copy_from_slice(data);
        tb[54] = ICMPV6_ECHO_REPLY;
        tb[56] = 0; tb[57] = 0; // zero checksum before computing

        let icmp_data = &tb[54..54 + payload_len];
        let csum = ipv6_upper_checksum(&my_ip, &src_ip, 58, icmp_data);
        tb[56] = (csum >> 8) as u8; tb[57] = csum as u8;

        crate::arch::aarch64::exceptions::disable_irqs();
        super::net_send_frame(&tb[..frame_total]);
        crate::arch::aarch64::exceptions::enable_irqs();
    }
}

fn handle_echo_reply(data: &[u8]) {
    if data.len() < 8 { return; }
    let id = u16::from_be_bytes([data[4], data[5]]);
    if id == unsafe { PING6_ID } {
        unsafe { PING6_REPLY = Some(timer::tick_count() as u32); }
    }
}

// ── Public: ping6 ─────────────────────────────────────────────────────────────

/// Send an ICMPv6 Echo Request to `dst_ip6` and wait for reply.
/// Returns RTT in ms, or -1 on timeout.
pub fn send_ping6(dst_ip: [u8; 16], timeout_ms: u32) -> isize {
    let my_ll  = unsafe { NET_IP6 };
    let my_g   = unsafe { NET_IP6G };
    // Use global if available, else link-local
    let my_src = if my_g != [0u8; 16] { my_g } else { my_ll };
    if my_src == [0u8; 16] { return -1; }
    let my_mac = unsafe { NET_MAC };

    // Resolve the target MAC
    let dst_mac = match super::ndp::ndp_resolve_blocking(dst_ip) {
        Some(m) => m,
        None    => return -1,
    };

    let id  = unsafe { PING6_ID };
    let seq = 1u16;

    // Build Echo Request: type(1) code(1) cksum(2) id(2) seq(2) payload(8) = 16 bytes
    let mut body = [0u8; 16];
    body[0] = ICMPV6_ECHO_REQUEST;
    body[4] = (id >> 8) as u8; body[5] = id as u8;
    body[6] = (seq >> 8) as u8; body[7] = seq as u8;
    // payload: fill with 0xAE pattern
    for i in 8..16 { body[i] = 0xAE; }

    let csum = ipv6_upper_checksum(&my_src, &dst_ip, 58, &body);
    body[2] = (csum >> 8) as u8; body[3] = csum as u8;

    let frame_total = 14 + 40 + body.len();
    unsafe {
        let tb = &mut TX_BUF;
        tb[0..6].copy_from_slice(&dst_mac);
        tb[6..12].copy_from_slice(&my_mac);
        tb[12] = 0x86; tb[13] = 0xDD;
        tb[14] = 0x60; tb[15] = 0; tb[16] = 0; tb[17] = 0;
        tb[18] = 0; tb[19] = body.len() as u8;
        tb[20] = 58; tb[21] = 255;
        tb[22..38].copy_from_slice(&my_src);
        tb[38..54].copy_from_slice(&dst_ip);
        tb[54..54 + body.len()].copy_from_slice(&body);

        PING6_REPLY = None;
        let t0 = timer::tick_count() as u32;

        crate::arch::aarch64::exceptions::disable_irqs();
        super::net_send_frame(&tb[..frame_total]);
        crate::arch::aarch64::exceptions::enable_irqs();

        let timeout_ticks = timeout_ms / 10; // 100 Hz ticks
        loop {
            super::poll_rx_only();
            if let Some(t1) = PING6_REPLY {
                let rtt_ms = (t1.wrapping_sub(t0)) * 10;
                return rtt_ms as isize;
            }
            if timer::tick_count() as u32 >= t0 + timeout_ticks { break; }
            for _ in 0..1000 { core::hint::spin_loop(); }
        }
    }
    -1
}
