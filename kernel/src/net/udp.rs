//! UDP — port dispatch table (16 entries).

use crate::arch::aarch64::exceptions;

#[derive(Copy, Clone)]
struct UdpHandler {
    port:    u16,
    handler: fn([u8; 4], u16, &[u8]),
    valid:   bool,
}

const HANDLER_EMPTY: UdpHandler = UdpHandler { port: 0, handler: |_, _, _| {}, valid: false };
const TABLE_SIZE: usize = 16;

static mut HANDLERS: [UdpHandler; TABLE_SIZE] = [HANDLER_EMPTY; TABLE_SIZE];

/// Register a handler for the given UDP destination port.
/// Handler signature: `fn(src_ip: [u8;4], src_port: u16, data: &[u8])`.
/// Returns false if the table is full.
pub fn udp_register(port: u16, handler: fn([u8; 4], u16, &[u8])) -> bool {
    unsafe {
        // Update existing entry for this port.
        for h in HANDLERS.iter_mut() {
            if h.valid && h.port == port { h.handler = handler; return true; }
        }
        // Find empty slot.
        for h in HANDLERS.iter_mut() {
            if !h.valid { *h = UdpHandler { port, handler, valid: true }; return true; }
        }
    }
    false
}

/// Unregister a handler for the given port.
pub fn udp_unregister(port: u16) {
    unsafe {
        for h in HANDLERS.iter_mut() {
            if h.valid && h.port == port { h.valid = false; }
        }
    }
}

/// Dispatch an incoming UDP segment. Called from `net::handle_ipv4`.
pub(super) fn handle_udp_dispatch(src_ip: [u8; 4], payload: &[u8]) {
    if payload.len() < 8 { return; }
    let src_port = u16::from_be_bytes([payload[0], payload[1]]);
    let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
    let udp_len  = u16::from_be_bytes([payload[4], payload[5]]) as usize;
    let end      = udp_len.min(payload.len());
    let data     = &payload[8..end];

    unsafe {
        for h in HANDLERS.iter() {
            if h.valid && h.port == dst_port {
                (h.handler)(src_ip, src_port, data);
            }
        }
    }
}

/// Send a UDP datagram. Builds Ethernet + IP + UDP headers in the shared TX buffer.
pub fn udp_send(dst_ip: [u8; 4], dst_port: u16, src_port: u16, data: &[u8]) -> bool {
    if !unsafe { super::NET_UP } { return false; }

    let my_ip  = unsafe { super::NET_IP };
    let my_mac = unsafe { super::NET_MAC };
    let total  = 14 + 20 + 8 + data.len();
    if total > 2048 { return false; }

    let dst_mac = super::arp::resolve_mac_nonblocking(dst_ip);

    unsafe {
        let tb = &mut super::TX_BUF;

        tb[0..6].copy_from_slice(&dst_mac);
        tb[6..12].copy_from_slice(&my_mac);
        tb[12] = 0x08; tb[13] = 0x00;

        let ip_len = (20 + 8 + data.len()) as u16;
        tb[14] = 0x45; tb[15] = 0x00;
        tb[16] = (ip_len >> 8) as u8; tb[17] = (ip_len & 0xFF) as u8;
        tb[18] = 0x00; tb[19] = 0x01;
        tb[20] = 0x40; tb[21] = 0x00;
        tb[22] = 64; tb[23] = 17;
        tb[24] = 0x00; tb[25] = 0x00;
        tb[26..30].copy_from_slice(&my_ip);
        tb[30..34].copy_from_slice(&dst_ip);
        let csum = super::ip_checksum(&tb[14..34]);
        tb[24] = (csum >> 8) as u8; tb[25] = (csum & 0xFF) as u8;

        let udp_len = (8 + data.len()) as u16;
        tb[34] = (src_port >> 8) as u8; tb[35] = (src_port & 0xFF) as u8;
        tb[36] = (dst_port >> 8) as u8; tb[37] = (dst_port & 0xFF) as u8;
        tb[38] = (udp_len >> 8) as u8;  tb[39] = (udp_len & 0xFF) as u8;
        tb[40] = 0x00; tb[41] = 0x00;
        tb[42..42 + data.len()].copy_from_slice(data);

        exceptions::disable_irqs();
        let ok = super::net_send_frame(&tb[..total]);
        exceptions::enable_irqs();
        ok
    }
}
