//! DHCP client — stateful with retry and lease renewal.

#[derive(Copy, Clone, PartialEq)]
enum DhcpState { Idle, Discovering, Requesting, Bound, Renewing }

struct DhcpClient {
    state:            DhcpState,
    xid:              u32,
    offered_ip:       [u8; 4],
    server_ip:        [u8; 4],
    lease_secs:       u32,
    t1_secs:          u32,
    bound_tick:       u32,
    retry_count:      u8,
    next_action_tick: u32,
}

const FALLBACK_IP:   [u8; 4] = [10, 0, 2, 15];
const FALLBACK_MASK: [u8; 4] = [255, 255, 255, 0];
const FALLBACK_GW:   [u8; 4] = [10, 0, 2, 2];
const RETRY_TICKS:   u32 = 100; // 1 s at 100 Hz
const MAX_RETRIES:   u8  = 3;

static mut CLIENT: DhcpClient = DhcpClient {
    state:            DhcpState::Idle,
    xid:              0,
    offered_ip:       [0; 4],
    server_ip:        [0; 4],
    lease_secs:       3600,
    t1_secs:          1800,
    bound_tick:       0,
    retry_count:      0,
    next_action_tick: 0,
};

pub(super) fn init() {
    // Register DHCP on UDP port 68
    super::udp::udp_register(68, handle_dhcp_packet);
    send_discover();
}

fn make_xid() -> u32 {
    crate::arch::aarch64::timer::tick_count() as u32 ^ 0xAE01_0000
}

fn send_discover() {
    unsafe {
        CLIENT.xid = make_xid();
        CLIENT.state = DhcpState::Discovering;
        CLIENT.retry_count = 0;
        CLIENT.next_action_tick =
            crate::arch::aarch64::timer::tick_count() as u32 + RETRY_TICKS;
    }
    let xid = unsafe { CLIENT.xid };
    let mac = unsafe { super::NET_MAC };
    let mut data = [0u8; 300];
    data[0] = 1; data[1] = 1; data[2] = 6;
    data[4] = (xid >> 24) as u8; data[5] = (xid >> 16) as u8;
    data[6] = (xid >> 8) as u8;  data[7] = xid as u8;
    data[10] = 0x80;
    data[28..34].copy_from_slice(&mac);
    data[236..240].copy_from_slice(&[99, 130, 83, 99]);
    data[240] = 53; data[241] = 1; data[242] = 1; // DISCOVER
    data[243] = 255;
    super::udp::udp_send([255, 255, 255, 255], 67, 68, &data[..244]);
}

fn send_request(offered: [u8; 4], server: [u8; 4]) {
    let xid = unsafe { CLIENT.xid };
    let mac = unsafe { super::NET_MAC };
    let mut req = [0u8; 300];
    req[0] = 1; req[1] = 1; req[2] = 6;
    req[4] = (xid >> 24) as u8; req[5] = (xid >> 16) as u8;
    req[6] = (xid >> 8) as u8;  req[7] = xid as u8;
    req[10] = 0x80;
    req[28..34].copy_from_slice(&mac);
    req[236..240].copy_from_slice(&[99, 130, 83, 99]);
    req[240] = 53; req[241] = 1; req[242] = 3; // REQUEST
    req[243] = 50; req[244] = 4; req[245..249].copy_from_slice(&offered);
    req[249] = 54; req[250] = 4; req[251..255].copy_from_slice(&server);
    req[255] = 255;
    super::udp::udp_send([255, 255, 255, 255], 67, 68, &req[..256]);
}

/// Handle a received DHCP packet (called from UDP port 68 handler).
fn handle_dhcp_packet(_src_ip: [u8; 4], src_port: u16, data: &[u8]) {
    if src_port != 67 { return; }
    if data.len() < 240 || data[0] != 2 { return; }

    let xid = unsafe { CLIENT.xid };
    let pkt_xid = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
    if pkt_xid != xid { return; }

    let yiaddr = super::array4(&data[16..20]);
    let mut msg_type = 0u8;
    let mut server_ip = [0u8; 4];
    let mut lease_time = 3600u32;
    let mut subnet = [255u8, 255, 255, 0];
    let mut gw = [0u8; 4];
    let mut dns = [0u8; 4];
    let mut t1 = 0u32;

    let mut i = 240usize;
    while i + 1 < data.len() {
        let tag = data[i];
        if tag == 255 { break; }
        if tag == 0  { i += 1; continue; }
        if i + 1 >= data.len() { break; }
        let len = data[i + 1] as usize;
        if i + 2 + len > data.len() { break; }
        let v = &data[i + 2..i + 2 + len];
        match tag {
            1  if len >= 4 => subnet   = super::array4(&v[..4]),
            3  if len >= 4 => gw       = super::array4(&v[..4]),
            6  if len >= 4 => dns      = super::array4(&v[..4]),
            51 if len >= 4 => lease_time = u32::from_be_bytes([v[0],v[1],v[2],v[3]]),
            53 if len >= 1 => msg_type = v[0],
            54 if len >= 4 => server_ip = super::array4(&v[..4]),
            58 if len >= 4 => t1       = u32::from_be_bytes([v[0],v[1],v[2],v[3]]),
            _ => {}
        }
        i += 2 + len;
    }
    let _ = (dns, subnet, gw); // parsed but gw handled below

    let state = unsafe { CLIENT.state };
    if msg_type == 2 && state == DhcpState::Discovering {
        // OFFER
        unsafe {
            CLIENT.offered_ip = yiaddr;
            CLIENT.server_ip  = server_ip;
            CLIENT.state = DhcpState::Requesting;
            CLIENT.retry_count = 0;
            CLIENT.next_action_tick =
                crate::arch::aarch64::timer::tick_count() as u32 + RETRY_TICKS;
        }
        send_request(yiaddr, server_ip);
    } else if msg_type == 5 && (state == DhcpState::Requesting || state == DhcpState::Renewing) {
        // ACK
        let t1_ticks = if t1 > 0 { t1 } else { lease_time / 2 };
        unsafe {
            super::NET_IP   = yiaddr;
            super::NET_MASK = subnet;
            super::NET_GW   = if gw == [0;4] {
                [yiaddr[0], yiaddr[1], yiaddr[2], 2]
            } else { gw };
            CLIENT.state       = DhcpState::Bound;
            CLIENT.lease_secs  = lease_time;
            CLIENT.t1_secs     = t1_ticks;
            CLIENT.bound_tick  = crate::arch::aarch64::timer::tick_count() as u32;
            // next_action_tick = bound_tick + t1_ticks * 100 (ticks per second)
            CLIENT.next_action_tick = CLIENT.bound_tick + t1_ticks * 100;
        }
        let uart = crate::drivers::uart::Uart::new();
        uart.puts("[net]  DHCP ACK  IP=");
        super::put_ip(yiaddr);
        uart.puts("\r\n");
    }
}

/// Drive the DHCP state machine. Called from `poll()` every 100 Hz tick.
pub(super) fn dhcp_tick() {
    let now = crate::arch::aarch64::timer::tick_count() as u32;
    let (state, next, retry) = unsafe {
        (CLIENT.state, CLIENT.next_action_tick, CLIENT.retry_count)
    };
    if now < next { return; }

    match state {
        DhcpState::Discovering => {
            if retry >= MAX_RETRIES {
                // Fallback to static IP
                unsafe {
                    super::NET_IP   = FALLBACK_IP;
                    super::NET_MASK = FALLBACK_MASK;
                    super::NET_GW   = FALLBACK_GW;
                    CLIENT.state    = DhcpState::Bound;
                    CLIENT.next_action_tick = now + 360000; // 1 hour
                }
                let uart = crate::drivers::uart::Uart::new();
                uart.puts("[net]  DHCP timeout — using fallback 10.0.2.15\r\n");
            } else {
                unsafe {
                    CLIENT.retry_count += 1;
                    CLIENT.next_action_tick = now + RETRY_TICKS;
                }
                send_discover();
            }
        }
        DhcpState::Requesting => {
            if retry >= MAX_RETRIES {
                send_discover();
            } else {
                let (offered, server) = unsafe { (CLIENT.offered_ip, CLIENT.server_ip) };
                unsafe { CLIENT.retry_count += 1; CLIENT.next_action_tick = now + RETRY_TICKS; }
                send_request(offered, server);
            }
        }
        DhcpState::Bound => {
            // T1 expired → renew
            let ip = unsafe { super::NET_IP };
            let server = unsafe { CLIENT.server_ip };
            unsafe {
                CLIENT.state = DhcpState::Renewing;
                CLIENT.retry_count = 0;
                CLIENT.next_action_tick = now + RETRY_TICKS;
            }
            send_request(ip, server);
        }
        DhcpState::Renewing => {
            if retry >= MAX_RETRIES {
                // Lease expired — restart from scratch
                send_discover();
            } else {
                let (ip, server) = unsafe { (super::NET_IP, CLIENT.server_ip) };
                unsafe { CLIENT.retry_count += 1; CLIENT.next_action_tick = now + RETRY_TICKS; }
                send_request(ip, server);
            }
        }
        DhcpState::Idle => {}
    }
}
