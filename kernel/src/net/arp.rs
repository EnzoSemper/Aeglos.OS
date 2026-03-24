//! ARP — IPv4/Ethernet address resolution with retry logic.


#[derive(Copy, Clone)]
struct ArpEntry {
    ip:    [u8; 4],
    mac:   [u8; 6],
    valid: bool,
}

const ARP_EMPTY: ArpEntry = ArpEntry { ip: [0u8; 4], mac: [0u8; 6], valid: false };

static mut ARP_TABLE:  [ArpEntry; 16] = [ARP_EMPTY; 16];
static mut ARP_CURSOR: usize          = 0;

#[derive(Copy, Clone)]
struct ArpPending {
    ip:          [u8; 4],
    retry_count: u8,
    next_retry:  u32,
    valid:       bool,
}

const ARP_PENDING_EMPTY: ArpPending =
    ArpPending { ip: [0; 4], retry_count: 0, next_retry: 0, valid: false };

static mut ARP_PENDING: [ArpPending; 4] = [ARP_PENDING_EMPTY; 4];

const ARP_RETRY_MAX:   u8  = 3;
const ARP_RETRY_TICKS: u32 = 10; // 100 ms at 100 Hz

pub(super) fn handle_arp(src_mac: &[u8], payload: &[u8]) {
    if payload.len() < 28 { return; }

    let htype = u16::from_be_bytes([payload[0], payload[1]]);
    let ptype = u16::from_be_bytes([payload[2], payload[3]]);
    let oper  = u16::from_be_bytes([payload[6], payload[7]]);

    if htype != 1 || ptype != 0x0800 { return; }

    let sender_mac = super::array6(&payload[8..14]);
    let sender_ip  = super::array4(&payload[14..18]);
    let target_ip  = super::array4(&payload[24..28]);

    arp_learn(sender_ip, sender_mac);

    let my_ip = unsafe { super::NET_IP };
    if oper == 1 && target_ip == my_ip {
        send_arp_reply(sender_ip, sender_mac);
    }

    let _ = src_mac;
}

pub(super) fn arp_learn(ip: [u8; 4], mac: [u8; 6]) {
    unsafe {
        for e in ARP_TABLE.iter_mut() {
            if e.valid && e.ip == ip { e.mac = mac; return; }
        }
        let idx = ARP_CURSOR % 16;
        ARP_TABLE[idx] = ArpEntry { ip, mac, valid: true };
        ARP_CURSOR += 1;
    }
}

pub(super) fn arp_lookup(ip: [u8; 4]) -> Option<[u8; 6]> {
    unsafe {
        for e in ARP_TABLE.iter() {
            if e.valid && e.ip == ip { return Some(e.mac); }
        }
    }
    None
}

fn send_arp_request(target_ip: [u8; 4]) {
    unsafe {
        let my_mac = super::NET_MAC;
        let my_ip  = super::NET_IP;
        let tb = &mut super::TX_BUF;
        tb[0..6].copy_from_slice(&[0xFF; 6]);
        tb[6..12].copy_from_slice(&my_mac);
        tb[12] = 0x08; tb[13] = 0x06;
        tb[14] = 0x00; tb[15] = 0x01;
        tb[16] = 0x08; tb[17] = 0x00;
        tb[18] = 6; tb[19] = 4;
        tb[20] = 0x00; tb[21] = 0x01;
        tb[22..28].copy_from_slice(&my_mac);
        tb[28..32].copy_from_slice(&my_ip);
        tb[32..38].copy_from_slice(&[0u8; 6]);
        tb[38..42].copy_from_slice(&target_ip);
        super::net_send_frame(&tb[..42]);
    }
}

fn send_arp_reply(target_ip: [u8; 4], target_mac: [u8; 6]) {
    unsafe {
        let my_mac = super::NET_MAC;
        let my_ip  = super::NET_IP;
        let tb = &mut super::TX_BUF;
        tb[0..6].copy_from_slice(&target_mac);
        tb[6..12].copy_from_slice(&my_mac);
        tb[12] = 0x08; tb[13] = 0x06;
        tb[14] = 0x00; tb[15] = 0x01;
        tb[16] = 0x08; tb[17] = 0x00;
        tb[18] = 6; tb[19] = 4;
        tb[20] = 0x00; tb[21] = 0x02;
        tb[22..28].copy_from_slice(&my_mac);
        tb[28..32].copy_from_slice(&my_ip);
        tb[32..38].copy_from_slice(&target_mac);
        tb[38..42].copy_from_slice(&target_ip);
        super::net_send_frame(&tb[..42]);
    }
}

/// Gratuitous ARP — broadcast our IP/MAC to populate neighbour tables.
pub fn arp_announce() {
    unsafe {
        let my_mac = super::NET_MAC;
        let my_ip  = super::NET_IP;
        let tb = &mut super::TX_BUF;
        tb[0..6].copy_from_slice(&[0xFF; 6]);
        tb[6..12].copy_from_slice(&my_mac);
        tb[12] = 0x08; tb[13] = 0x06;
        tb[14] = 0x00; tb[15] = 0x01;
        tb[16] = 0x08; tb[17] = 0x00;
        tb[18] = 6; tb[19] = 4;
        tb[20] = 0x00; tb[21] = 0x01;
        tb[22..28].copy_from_slice(&my_mac);
        tb[28..32].copy_from_slice(&my_ip);
        tb[32..38].copy_from_slice(&[0u8; 6]);
        tb[38..42].copy_from_slice(&my_ip);
        super::net_send_frame(&tb[..42]);
    }
}

/// Resolve the MAC for `dst_ip`. Returns a cached MAC or broadcast (and queues
/// an ARP request). Caller should retry after a `poll()`.
pub(super) fn resolve_mac_nonblocking(dst_ip: [u8; 4]) -> [u8; 6] {
    if dst_ip == [255, 255, 255, 255] { return [0xFF; 6]; }
    let my_ip = unsafe { super::NET_IP };
    let mask  = unsafe { super::NET_MASK };
    let same  = (0..4usize).all(|i| (dst_ip[i] & mask[i]) == (my_ip[i] & mask[i]));
    let look  = if same || my_ip == [0, 0, 0, 0] { dst_ip } else { unsafe { super::NET_GW } };
    if let Some(m) = arp_lookup(look) { return m; }
    send_arp_request(look);
    [0xFF; 6]
}

/// Blocking ARP resolve: retries up to 3 times with 100 ms each.
/// Safe to call from SVC context (uses physical timer, not tick_count).
pub fn arp_resolve_blocking(dst_ip: [u8; 4]) -> Option<[u8; 6]> {
    if dst_ip == [255, 255, 255, 255] { return Some([0xFF; 6]); }
    let my_ip = unsafe { super::NET_IP };
    let mask  = unsafe { super::NET_MASK };
    let same  = (0..4usize).all(|i| (dst_ip[i] & mask[i]) == (my_ip[i] & mask[i]));
    let look  = if same || my_ip == [0, 0, 0, 0] { dst_ip } else { unsafe { super::NET_GW } };

    if let Some(m) = arp_lookup(look) { return Some(m); }

    let freq = crate::arch::aarch64::timer::physical_timer_freq();
    let interval = freq / 10; // 100 ms

    for _ in 0..ARP_RETRY_MAX {
        send_arp_request(look);
        let deadline = crate::arch::aarch64::timer::physical_timer_count() + interval;
        loop {
            super::poll_rx_only();
            if let Some(m) = arp_lookup(look) { return Some(m); }
            if crate::arch::aarch64::timer::physical_timer_count() >= deadline { break; }
            unsafe { core::arch::asm!("yield"); }
        }
    }
    None
}

/// Drive pending ARP retries. Called from `poll()` every tick.
pub(super) fn arp_tick() {
    let now = crate::arch::aarch64::timer::tick_count() as u32;
    unsafe {
        for p in ARP_PENDING.iter_mut() {
            if !p.valid { continue; }
            if now < p.next_retry { continue; }
            if p.retry_count >= ARP_RETRY_MAX { p.valid = false; continue; }
            if arp_lookup(p.ip).is_some() { p.valid = false; continue; }
            send_arp_request(p.ip);
            p.retry_count += 1;
            p.next_retry = now + ARP_RETRY_TICKS;
        }
    }
}
