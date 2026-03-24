//! ICMP — echo reply + outbound ping.


/// Tick stamp (from physical_timer_count) when the last echo reply arrived.
pub(super) static mut ICMP_PING_REPLY_TICKS: Option<u64> = None;

pub(super) fn handle_icmp(src_mac: &[u8], src_ip: [u8; 4], payload: &[u8]) {
    if payload.len() < 8 { return; }
    match payload[0] {
        8 => send_icmp_echo_reply(src_mac, src_ip, payload), // echo request
        0 => unsafe {
            ICMP_PING_REPLY_TICKS =
                Some(crate::arch::aarch64::timer::physical_timer_count());
        },
        _ => {}
    }
}

fn send_icmp_echo_reply(dst_mac_slice: &[u8], dst_ip: [u8; 4], icmp_req: &[u8]) {
    unsafe {
        let my_mac = super::NET_MAC;
        let my_ip  = super::NET_IP;
        let tb     = &mut super::TX_BUF;

        let data_len    = icmp_req.len();
        let ip_total    = 20 + data_len;
        let frame_total = 14 + ip_total;
        if frame_total > 2048 { return; }

        let dst_mac = super::array6(dst_mac_slice);

        tb[0..6].copy_from_slice(&dst_mac);
        tb[6..12].copy_from_slice(&my_mac);
        tb[12] = 0x08; tb[13] = 0x00;

        tb[14] = 0x45; tb[15] = 0x00;
        tb[16] = (ip_total >> 8) as u8; tb[17] = (ip_total & 0xFF) as u8;
        tb[18] = 0x00; tb[19] = 0x00;
        tb[20] = 0x40; tb[21] = 0x00;
        tb[22] = 64; tb[23] = 1;
        tb[24] = 0x00; tb[25] = 0x00;
        tb[26..30].copy_from_slice(&my_ip);
        tb[30..34].copy_from_slice(&dst_ip);
        let csum = super::ip_checksum(&tb[14..34]);
        tb[24] = (csum >> 8) as u8; tb[25] = (csum & 0xFF) as u8;

        tb[34..34 + data_len].copy_from_slice(icmp_req);
        tb[34] = 0; tb[35] = 0; // type=0 (reply), code=0
        tb[36] = 0; tb[37] = 0; // clear checksum
        let icmp_csum = super::ip_checksum(&tb[34..34 + data_len]);
        tb[36] = (icmp_csum >> 8) as u8; tb[37] = (icmp_csum & 0xFF) as u8;

        super::net_send_frame(&tb[..frame_total]);
    }
}

/// Send an ICMP echo request and block until a reply arrives or timeout.
/// Safe to call from SVC context; uses physical timer for timing.
pub fn send_ping(dst_ip: [u8; 4], timeout_ms: u32) -> isize {
    if !unsafe { super::NET_UP } { return -1; }

    let my_ip  = unsafe { super::NET_IP };
    let my_mac = unsafe { super::NET_MAC };
    let Some(dst_mac) = super::arp::arp_resolve_blocking(dst_ip) else {
        return -1;
    };

    unsafe { ICMP_PING_REPLY_TICKS = None; }

    unsafe {
        let tb = &mut super::TX_BUF;
        let data_len    = 8 + 32;
        let ip_total    = 20 + data_len;
        let frame_total = 14 + ip_total;

        tb[0..6].copy_from_slice(&dst_mac);
        tb[6..12].copy_from_slice(&my_mac);
        tb[12] = 0x08; tb[13] = 0x00;
        tb[14] = 0x45; tb[15] = 0x00;
        tb[16] = (ip_total >> 8) as u8; tb[17] = (ip_total & 0xFF) as u8;
        tb[18] = 0x12; tb[19] = 0x34;
        tb[20] = 0x40; tb[21] = 0x00;
        tb[22] = 64; tb[23] = 1;
        tb[24] = 0x00; tb[25] = 0x00;
        tb[26..30].copy_from_slice(&my_ip);
        tb[30..34].copy_from_slice(&dst_ip);
        let csum = super::ip_checksum(&tb[14..34]);
        tb[24] = (csum >> 8) as u8; tb[25] = (csum & 0xFF) as u8;
        tb[34] = 8; tb[35] = 0; tb[36] = 0; tb[37] = 0;
        tb[38] = 0x00; tb[39] = 0x01; tb[40] = 0x00; tb[41] = 0x01;
        for i in 0..32 { tb[42 + i] = i as u8; }
        let icmp_csum = super::ip_checksum(&tb[34..frame_total - 14]);
        tb[36] = (icmp_csum >> 8) as u8; tb[37] = (icmp_csum & 0xFF) as u8;
        super::net_send_frame(&tb[..frame_total]);
    }

    let start = crate::arch::aarch64::timer::physical_timer_count();
    let freq  = crate::arch::aarch64::timer::physical_timer_freq();
    let end   = start + (timeout_ms as u64 * freq) / 1000;

    loop {
        super::poll_rx_only();
        if let Some(r) = unsafe { ICMP_PING_REPLY_TICKS } {
            return ((r.saturating_sub(start)) * 1000 / freq) as isize;
        }
        if crate::arch::aarch64::timer::physical_timer_count() >= end { return -1; }
        unsafe { core::arch::asm!("yield"); }
    }
}
