//! TCP — client-only state machine, 16 connections, 32 KB per-connection ring buffers.

extern crate alloc;
use alloc::boxed::Box;
use alloc::vec::Vec;
use crate::arch::aarch64::exceptions;

// ── Constants ─────────────────────────────────────────────────────────────────

const MAX_CONNS:    usize = 16;
const BUF_SIZE:     usize = 32 * 1024;
const MSS_DEFAULT:  u16   = 1460;

const TCP_FIN: u8 = 0x01;
const TCP_SYN: u8 = 0x02;
const TCP_RST: u8 = 0x04;
const TCP_ACK: u8 = 0x10;

const RETX_INIT:      u32   = 50;               // 500 ms at 100 Hz
const RETX_MAX:       u8    = 5;
const TIMEWAIT_TICKS: u32   = 100;              // 1 s
const CWND_INIT:      u32   = MSS_DEFAULT as u32 * 2; // 2 × MSS slow-start
const SSTHRESH_INIT:  u32   = 64 * 1024;        // 64 KB
const PERSIST_INIT:   u32   = 100;              // 1 s initial probe interval
const PERSIST_MAX:    u32   = 6000;             // 60 s max probe interval
const LASTACK_TIMEOUT:u32   = 600;              // 6 s → retransmit FIN-ACK
const MAX_FLUSH_SEGS: usize = 8;                // max segs per flush_tx call

/// Type alias for connection indices.
pub type ConnId = usize;

// ── Ring buffer ───────────────────────────────────────────────────────────────

struct RingBuf {
    buf:  Vec<u8>,
    head: usize,
    tail: usize,
    len:  usize,
}

impl RingBuf {
    fn new(cap: usize) -> Self {
        let mut v = Vec::new();
        v.resize(cap, 0u8);
        RingBuf { buf: v, head: 0, tail: 0, len: 0 }
    }

    fn cap(&self) -> usize { self.buf.len() }
    fn avail(&self) -> usize { self.len }
    fn free(&self) -> usize { self.cap() - self.len }

    fn push(&mut self, data: &[u8]) -> usize {
        let n = data.len().min(self.free());
        for &b in &data[..n] {
            self.buf[self.tail] = b;
            self.tail = (self.tail + 1) % self.cap();
            self.len += 1;
        }
        n
    }

    fn pop(&mut self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.len);
        for b in &mut buf[..n] {
            *b = self.buf[self.head];
            self.head = (self.head + 1) % self.cap();
            self.len -= 1;
        }
        n
    }

    /// Peek without advancing head.
    fn peek(&self, buf: &mut [u8]) -> usize {
        let n = buf.len().min(self.len);
        for (i, b) in buf[..n].iter_mut().enumerate() {
            *b = self.buf[(self.head + i) % self.cap()];
        }
        n
    }

    /// Peek `buf.len()` bytes starting at `offset` bytes from head (without advancing head).
    fn peek_from(&self, offset: usize, buf: &mut [u8]) -> usize {
        let readable = self.len.saturating_sub(offset);
        let n = buf.len().min(readable);
        for (i, b) in buf[..n].iter_mut().enumerate() {
            *b = self.buf[(self.head + offset + i) % self.cap()];
        }
        n
    }

    /// Discard `n` bytes from the head.
    fn discard(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % self.cap();
        self.len -= n;
    }
}

// ── Connection slot ───────────────────────────────────────────────────────────

#[derive(Copy, Clone, PartialEq)]
pub enum TcpState {
    Free,
    // ── Client-side ────────
    SynSent,
    // ── Server-side ────────
    Listen,     // Awaiting incoming SYN
    SynRcvd,    // SYN received, SYN-ACK sent, awaiting final ACK
    // ── Shared ─────────────
    Established,
    FinWait1,
    FinWait2,
    Closing,    // simultaneous close
    CloseWait,
    LastAck,
    TimeWait,
    Closed,
}

struct TcpConn {
    state:       TcpState,
    remote_ip:   super::IpAddr,
    remote_port: u16,
    local_port:  u16,

    snd_nxt:     u32,
    snd_una:     u32,
    snd_cur:     usize, // bytes already sent past snd_una (offset into TX ring for next send)
    rcv_nxt:     u32,
    snd_wnd:     u16,
    mss:         u16,

    retx_ticks:  u32,
    retx_count:  u8,
    retx_backoff: u8,
    state_tick:  u32,

    // Congestion control (RFC 5681)
    cwnd:         u32,  // congestion window (bytes)
    ssthresh:     u32,  // slow-start threshold
    dup_acks:     u8,   // consecutive duplicate ACK count
    last_ack_rcvd:u32,  // ack_num of last ACK (dup detection)

    // Zero-window persist
    persist_ticks:u32,  // tick deadline for next probe
    persist_arm:  bool, // true = probe timer is running

    // Server-side accept tracking
    is_server:   bool,  // true = accepted from Listen slot (not yet claimed)
    accept_port: u16,   // the listening port this connection was accepted on

    rx: Option<Box<RingBuf>>,
    tx: Option<Box<RingBuf>>,
}

impl TcpConn {
    const fn zeroed() -> Self {
        TcpConn {
            state: TcpState::Free,
            remote_ip: super::IpAddr::V4([0;4]), remote_port: 0, local_port: 0,
            snd_nxt: 0, snd_una: 0, snd_cur: 0, rcv_nxt: 0, snd_wnd: 8192, mss: MSS_DEFAULT,
            retx_ticks: 0, retx_count: 0, retx_backoff: 1, state_tick: 0,
            cwnd: CWND_INIT, ssthresh: SSTHRESH_INIT,
            dup_acks: 0, last_ack_rcvd: 0,
            persist_ticks: 0, persist_arm: false,
            is_server: false, accept_port: 0,
            rx: None, tx: None,
        }
    }
}

// SAFETY: single-core bare metal — no concurrent access.
unsafe impl Send for TcpConn {}
unsafe impl Sync for TcpConn {}

// We can't put Box<RingBuf> in a static directly; use Option and lazy-init.
// SAFETY: single-core, mutated only before/after IRQ safe points.
static mut CONNS: [TcpConn; MAX_CONNS] = {
    // Const array initialisation without Default.
    const C: TcpConn = TcpConn {
        state: TcpState::Free, remote_ip: super::IpAddr::V4([0;4]), remote_port: 0, local_port: 0,
        snd_nxt: 0, snd_una: 0, snd_cur: 0, rcv_nxt: 0, snd_wnd: 8192, mss: MSS_DEFAULT,
        retx_ticks: 0, retx_count: 0, retx_backoff: 1, state_tick: 0,
        cwnd: CWND_INIT, ssthresh: SSTHRESH_INIT,
        dup_acks: 0, last_ack_rcvd: 0,
        persist_ticks: 0, persist_arm: false,
        is_server: false, accept_port: 0,
        rx: None, tx: None,
    };
    [C; MAX_CONNS]
};

static mut NEXT_PORT: u16 = 49152;

// ── Checksum ──────────────────────────────────────────────────────────────────

/// TCP checksum over the pseudo-header + TCP segment.
fn tcp_checksum(src_ip: [u8;4], dst_ip: [u8;4], tcp_data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    // Pseudo-header
    for i in (0..4).step_by(2) {
        sum += u16::from_be_bytes([src_ip[i], src_ip[i+1]]) as u32;
        sum += u16::from_be_bytes([dst_ip[i], dst_ip[i+1]]) as u32;
    }
    sum += 6u32; // protocol TCP
    sum += tcp_data.len() as u32;
    // TCP data
    let mut i = 0usize;
    while i + 1 < tcp_data.len() {
        sum += u16::from_be_bytes([tcp_data[i], tcp_data[i+1]]) as u32;
        i += 2;
    }
    if i < tcp_data.len() { sum += (tcp_data[i] as u32) << 8; }
    while sum >> 16 != 0 { sum = (sum & 0xFFFF) + (sum >> 16); }
    !(sum as u16)
}

// ── Send primitives ───────────────────────────────────────────────────────────

/// Build and transmit a TCP segment using the shared TX_BUF.
fn send_segment(id: ConnId, flags: u8, data: &[u8]) {
    let c = unsafe { &CONNS[id] };
    let my_mac = unsafe { super::NET_MAC };

    let tcp_hdr_len = 20usize;
    let options_len = if flags & TCP_SYN != 0 { 4usize } else { 0 };
    let tcp_len     = tcp_hdr_len + options_len + data.len();

    match c.remote_ip {
        super::IpAddr::V4(dst_ip4) => {
            let my_ip   = unsafe { super::NET_IP };
            let dst_mac = super::arp::resolve_mac_nonblocking(dst_ip4);
            let ip_total    = 20 + tcp_len;
            let frame_total = 14 + ip_total;
            if frame_total > 2048 { return; }
            unsafe {
                let tb = &mut super::TX_BUF;
                tb[0..6].copy_from_slice(&dst_mac);
                tb[6..12].copy_from_slice(&my_mac);
                tb[12] = 0x08; tb[13] = 0x00;
                tb[14] = 0x45; tb[15] = 0x00;
                tb[16] = (ip_total >> 8) as u8; tb[17] = ip_total as u8;
                tb[18] = 0x00; tb[19] = 0x00; tb[20] = 0x40; tb[21] = 0x00;
                tb[22] = 64; tb[23] = 6;
                tb[24] = 0x00; tb[25] = 0x00;
                tb[26..30].copy_from_slice(&my_ip);
                tb[30..34].copy_from_slice(&dst_ip4);
                let ip_csum = super::ip_checksum(&tb[14..34]);
                tb[24] = (ip_csum >> 8) as u8; tb[25] = ip_csum as u8;
                build_tcp_header(tb, 34, id, flags, tcp_hdr_len, options_len, data);
                let csum = tcp_checksum(my_ip, dst_ip4, &tb[34..34 + tcp_len]);
                tb[50] = (csum >> 8) as u8; tb[51] = csum as u8;
                exceptions::disable_irqs();
                super::net_send_frame(&tb[..frame_total]);
                exceptions::enable_irqs();
            }
        }
        super::IpAddr::V6(dst_ip6) => {
            let my_g  = unsafe { super::NET_IP6G };
            let my_ll = unsafe { super::NET_IP6 };
            let my_ip6 = if my_g != [0u8;16] { my_g } else { my_ll };
            let dst_mac = match super::ndp::ndp_lookup(&dst_ip6) {
                Some(m) => m,
                None    => return,
            };
            let frame_total = 14 + 40 + tcp_len;
            if frame_total > 2048 { return; }
            unsafe {
                let tb = &mut super::TX_BUF;
                tb[0..6].copy_from_slice(&dst_mac);
                tb[6..12].copy_from_slice(&my_mac);
                tb[12] = 0x86; tb[13] = 0xDD;
                tb[14] = 0x60; tb[15] = 0; tb[16] = 0; tb[17] = 0;
                tb[18] = (tcp_len >> 8) as u8; tb[19] = tcp_len as u8;
                tb[20] = 6; tb[21] = 64; // next=TCP, hop limit
                tb[22..38].copy_from_slice(&my_ip6);
                tb[38..54].copy_from_slice(&dst_ip6);
                build_tcp_header(tb, 54, id, flags, tcp_hdr_len, options_len, data);
                // Zero checksum field before computing
                tb[70] = 0; tb[71] = 0;
                let csum = super::ipv6_upper_checksum(&my_ip6, &dst_ip6, 6, &tb[54..54+tcp_len]);
                tb[70] = (csum >> 8) as u8; tb[71] = csum as u8;
                exceptions::disable_irqs();
                super::net_send_frame(&tb[..frame_total]);
                exceptions::enable_irqs();
            }
        }
    }
}

/// Write TCP header + options + payload into `tb[tcp_off..]`.
fn build_tcp_header(
    tb: &mut [u8], tcp_off: usize, id: ConnId,
    flags: u8, tcp_hdr_len: usize, options_len: usize, data: &[u8],
) {
    let c = unsafe { &CONNS[id] };
    let doff_byte = (((tcp_hdr_len + options_len) / 4) as u8) << 4;
    tb[tcp_off]   = (c.local_port  >> 8) as u8; tb[tcp_off+1] = c.local_port  as u8;
    tb[tcp_off+2] = (c.remote_port >> 8) as u8; tb[tcp_off+3] = c.remote_port as u8;
    let seq = c.snd_nxt;
    tb[tcp_off+4] = (seq>>24) as u8; tb[tcp_off+5] = (seq>>16) as u8;
    tb[tcp_off+6] = (seq>>8)  as u8; tb[tcp_off+7] = seq as u8;
    let ack = c.rcv_nxt;
    tb[tcp_off+8]  = (ack>>24) as u8; tb[tcp_off+9]  = (ack>>16) as u8;
    tb[tcp_off+10] = (ack>>8)  as u8; tb[tcp_off+11] = ack as u8;
    tb[tcp_off+12] = doff_byte; tb[tcp_off+13] = flags;
    let wnd: u16 = unsafe { CONNS[id].rx.as_ref()
        .map(|r| r.free().min(65535) as u16).unwrap_or(8192) };
    tb[tcp_off+14] = (wnd>>8) as u8; tb[tcp_off+15] = wnd as u8;
    tb[tcp_off+16] = 0; tb[tcp_off+17] = 0; // checksum placeholder
    tb[tcp_off+18] = 0; tb[tcp_off+19] = 0; // urgent pointer
    let mut off = tcp_off + 20;
    if flags & TCP_SYN != 0 {
        tb[off] = 2; tb[off+1] = 4;
        tb[off+2] = (MSS_DEFAULT >> 8) as u8; tb[off+3] = MSS_DEFAULT as u8;
        off += 4;
    }
    tb[off..off + data.len()].copy_from_slice(data);
}

/// Send RST to abort a connection without looking up a conn slot.
fn send_rst_raw(dst_ip: super::IpAddr, dst_port: u16, src_port: u16, seq: u32) {
    if let super::IpAddr::V4(dst_ip4) = dst_ip {
        let my_ip  = unsafe { super::NET_IP };
        let my_mac = unsafe { super::NET_MAC };
        let dst_mac = super::arp::resolve_mac_nonblocking(dst_ip4);
        unsafe {
            let tb = &mut super::TX_BUF;
            let tcp_len = 20usize;
            let ip_total = 20 + tcp_len;
            let frame_total = 14 + ip_total;
            tb[0..6].copy_from_slice(&dst_mac);
            tb[6..12].copy_from_slice(&my_mac);
            tb[12] = 0x08; tb[13] = 0x00;
            tb[14] = 0x45; tb[15] = 0x00;
            tb[16] = (ip_total >> 8) as u8; tb[17] = ip_total as u8;
            tb[18] = 0; tb[19] = 0; tb[20] = 0x40; tb[21] = 0x00;
            tb[22] = 64; tb[23] = 6;
            tb[24] = 0; tb[25] = 0;
            tb[26..30].copy_from_slice(&my_ip);
            tb[30..34].copy_from_slice(&dst_ip4);
            let ip_csum = super::ip_checksum(&tb[14..34]);
            tb[24] = (ip_csum >> 8) as u8; tb[25] = ip_csum as u8;
            tb[34] = (src_port >> 8) as u8; tb[35] = src_port as u8;
            tb[36] = (dst_port >> 8) as u8; tb[37] = dst_port as u8;
            tb[38] = (seq >> 24) as u8; tb[39] = (seq >> 16) as u8;
            tb[40] = (seq >> 8) as u8; tb[41] = seq as u8;
            tb[42] = 0; tb[43] = 0; tb[44] = 0; tb[45] = 0;
            tb[46] = 0x50; tb[47] = TCP_RST;
            tb[48] = 0; tb[49] = 0; tb[50] = 0; tb[51] = 0; tb[52] = 0; tb[53] = 0;
            let csum = tcp_checksum(my_ip, dst_ip4, &tb[34..34+tcp_len]);
            tb[50] = (csum >> 8) as u8; tb[51] = csum as u8;
            super::net_send_frame(&tb[..frame_total]);
        }
    }
    // IPv6 RST omitted (rare in practice; peer closes on timeout)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initiate a TCP connection. Returns a ConnId on success.
/// The connection is in SynSent state; call `tcp_wait_established` to block.
pub fn tcp_connect(dst_ip: super::IpAddr, dst_port: u16) -> Option<ConnId> {
    // Find free slot
    let id = unsafe {
        CONNS.iter().position(|c| c.state == TcpState::Free)?
    };

    // MAC resolution (ARP for IPv4, NDP for IPv6)
    match dst_ip {
        super::IpAddr::V4(ip4) => { super::arp::arp_resolve_blocking(ip4)?; }
        super::IpAddr::V6(ip6) => { super::ndp::ndp_resolve_blocking(ip6)?; }
    };

    // Allocate buffers
    let rx = Box::new(RingBuf::new(BUF_SIZE));
    let tx = Box::new(RingBuf::new(BUF_SIZE));

    let local_port = unsafe {
        let p = NEXT_PORT;
        NEXT_PORT = if NEXT_PORT >= 65535 { 49152 } else { NEXT_PORT + 1 };
        p
    };

    let isn = crate::arch::aarch64::timer::physical_timer_count() as u32;
    let now = crate::arch::aarch64::timer::tick_count() as u32;

    unsafe {
        CONNS[id] = TcpConn {
            state: TcpState::SynSent,
            remote_ip: dst_ip, remote_port: dst_port, local_port,
            snd_nxt: isn, snd_una: isn, snd_cur: 0, rcv_nxt: 0, snd_wnd: 8192, mss: MSS_DEFAULT,
            retx_ticks: now + RETX_INIT, retx_count: 0, retx_backoff: 1,
            state_tick: now,
            cwnd: CWND_INIT, ssthresh: SSTHRESH_INIT,
            dup_acks: 0, last_ack_rcvd: isn,
            persist_ticks: 0, persist_arm: false,
            is_server: false, accept_port: 0,
            rx: Some(rx), tx: Some(tx),
        };
    }

    // Log the outgoing SYN
    {
        let uart = crate::drivers::uart::Uart::new();
        uart.puts("[tcp]  SYN -> ");
        match dst_ip {
            super::IpAddr::V4(ip4) => super::put_ip(ip4),
            super::IpAddr::V6(ip6) => super::put_ip6(ip6),
        }
        uart.puts(":");
        uart.put_dec(dst_port as usize);
        uart.puts(" sport=");
        uart.put_dec(local_port as usize);
        uart.puts("\r\n");
    }
    send_segment(id, TCP_SYN, &[]);
    unsafe { CONNS[id].snd_nxt = CONNS[id].snd_nxt.wrapping_add(1); } // SYN counts as 1

    Some(id)
}

/// Block until the connection is Established or an error occurs.
/// Returns true on Established, false on timeout or RST.
pub fn tcp_wait_established(id: ConnId, timeout_ms: u32) -> bool {
    let freq  = crate::arch::aarch64::timer::physical_timer_freq();
    let end   = crate::arch::aarch64::timer::physical_timer_count()
                + (timeout_ms as u64 * freq) / 1000;
    loop {
        super::poll_rx_only();
        let st = unsafe { CONNS[id].state };
        if st == TcpState::Established { return true; }
        if st == TcpState::Closed || st == TcpState::Free {
            let uart = crate::drivers::uart::Uart::new();
            uart.puts("[tcp]  connect RST/refused\r\n");
            return false;
        }
        if crate::arch::aarch64::timer::physical_timer_count() >= end {
            let uart = crate::drivers::uart::Uart::new();
            uart.puts("[tcp]  connect TIMEOUT (no SYN-ACK)\r\n");
            return false;
        }
        // Retransmit SYN if timer expired
        let now = crate::arch::aarch64::timer::tick_count() as u32;
        let (rt, rc) = unsafe { (CONNS[id].retx_ticks, CONNS[id].retx_count) };
        if now >= rt && rc < RETX_MAX {
            unsafe {
                let isn = CONNS[id].snd_una;
                CONNS[id].snd_nxt = isn; // reset nxt for retransmit
            }
            send_segment(id, TCP_SYN, &[]);
            unsafe {
                CONNS[id].snd_nxt = CONNS[id].snd_una.wrapping_add(1);
                CONNS[id].retx_count += 1;
                CONNS[id].retx_backoff = CONNS[id].retx_backoff.saturating_mul(2).min(16);
                CONNS[id].retx_ticks = now + RETX_INIT * CONNS[id].retx_backoff as u32;
            }
        }
        unsafe { core::arch::asm!("yield"); }
    }
}

/// Write data into the connection's TX ring and send it. Returns bytes queued.
pub fn tcp_write(id: ConnId, data: &[u8]) -> usize {
    if id >= MAX_CONNS { return 0; }
    let st = unsafe { CONNS[id].state };
    if st != TcpState::Established && st != TcpState::CloseWait { return 0; }

    let pushed = unsafe {
        if let Some(tx) = CONNS[id].tx.as_mut() { tx.push(data) } else { return 0; }
    };
    flush_tx(id);
    pushed
}

/// Flush TX ring: send up to MAX_FLUSH_SEGS new (unsent) segments.
/// Effective window = min(peer snd_wnd, cwnd). Bytes stay in TX ring until ACKed.
fn flush_tx(id: ConnId) {
    let mut sent_segs = 0usize;
    loop {
        if sent_segs >= MAX_FLUSH_SEGS { break; }

        let (avail, snd_cur, mss, peer_wnd, cwnd) = unsafe {
            let c = &CONNS[id];
            (
                c.tx.as_ref().map(|t| t.avail()).unwrap_or(0),
                c.snd_cur,
                c.mss as usize,
                c.snd_wnd as u32,
                c.cwnd,
            )
        };

        // Effective window = min(peer window, congestion window)
        let eff_wnd = peer_wnd.min(cwnd) as usize;
        let in_flight = snd_cur; // bytes sent but not yet ACKed
        if in_flight >= eff_wnd { break; }

        let unsent = avail.saturating_sub(snd_cur);
        if unsent == 0 { break; }

        let send_n = unsent.min(mss).min(eff_wnd.saturating_sub(in_flight));
        if send_n == 0 { break; }

        let mut seg = [0u8; 1460];
        let read = unsafe {
            CONNS[id].tx.as_ref()
                .map(|t| t.peek_from(snd_cur, &mut seg[..send_n]))
                .unwrap_or(0)
        };
        if read == 0 { break; }

        send_segment(id, TCP_ACK, &seg[..read]);
        unsafe {
            let first_send = CONNS[id].snd_cur == 0 && CONNS[id].snd_una == CONNS[id].snd_nxt;
            CONNS[id].snd_nxt = CONNS[id].snd_nxt.wrapping_add(read as u32);
            CONNS[id].snd_cur += read;
            if first_send {
                // Arm retransmit timer on first unACKed byte
                let now = crate::arch::aarch64::timer::tick_count() as u32;
                CONNS[id].retx_ticks   = now + RETX_INIT;
                CONNS[id].retx_count   = 0;
                CONNS[id].retx_backoff = 1;
            }
        }
        sent_segs += 1;
    }
}

/// Read received data into `buf`. Returns bytes read.
pub fn tcp_read(id: ConnId, buf: &mut [u8]) -> usize {
    if id >= MAX_CONNS { return 0; }
    unsafe {
        if let Some(rx) = CONNS[id].rx.as_mut() { rx.pop(buf) } else { 0 }
    }
}

/// Block until data is readable or the connection closes.
pub fn tcp_wait_readable(id: ConnId, timeout_ms: u32) -> bool {
    let freq  = crate::arch::aarch64::timer::physical_timer_freq();
    let end   = crate::arch::aarch64::timer::physical_timer_count()
                + (timeout_ms as u64 * freq) / 1000;
    loop {
        super::poll_rx_only();
        // Flush any pending outbound data (ACKs may have opened window).
        flush_tx(id);
        let avail = unsafe { CONNS[id].rx.as_ref().map(|r| r.avail()).unwrap_or(0) };
        let st    = unsafe { CONNS[id].state };
        if avail > 0 { return true; }
        // Peer has closed its send direction — no more data will arrive.
        if st == TcpState::Closed   || st == TcpState::Free
            || st == TcpState::CloseWait || st == TcpState::Closing
            || st == TcpState::FinWait2  || st == TcpState::TimeWait { return false; }
        if crate::arch::aarch64::timer::physical_timer_count() >= end { return false; }
        unsafe { core::arch::asm!("yield"); }
    }
}

/// Check whether there's data available to read (non-blocking).
pub fn tcp_readable(id: ConnId) -> usize {
    if id >= MAX_CONNS { return 0; }
    unsafe { CONNS[id].rx.as_ref().map(|r| r.avail()).unwrap_or(0) }
}

/// Check whether the connection is in a readable state (Established or CloseWait).
pub fn tcp_state(id: ConnId) -> TcpState {
    if id >= MAX_CONNS { return TcpState::Free; }
    unsafe { CONNS[id].state }
}

/// Initiate close: send FIN.
pub fn tcp_close(id: ConnId) {
    if id >= MAX_CONNS { return; }
    let st = unsafe { CONNS[id].state };
    match st {
        TcpState::Established => {
            send_segment(id, TCP_FIN | TCP_ACK, &[]);
            unsafe {
                CONNS[id].snd_nxt = CONNS[id].snd_nxt.wrapping_add(1);
                CONNS[id].state   = TcpState::FinWait1;
                CONNS[id].state_tick = crate::arch::aarch64::timer::tick_count() as u32;
            }
        }
        TcpState::CloseWait => {
            send_segment(id, TCP_FIN | TCP_ACK, &[]);
            unsafe {
                CONNS[id].snd_nxt = CONNS[id].snd_nxt.wrapping_add(1);
                CONNS[id].state   = TcpState::LastAck;
                CONNS[id].state_tick = crate::arch::aarch64::timer::tick_count() as u32;
            }
        }
        _ => {
            // Force-free the slot
            unsafe {
                CONNS[id].rx = None;
                CONNS[id].tx = None;
                CONNS[id].state = TcpState::Free;
            }
        }
    }
}

// ── Server-side API ───────────────────────────────────────────────────────────

/// Bind a local port and start listening for incoming connections.
/// Returns the ConnId of the listener slot (keep it alive while accepting).
pub fn tcp_listen(local_port: u16) -> Option<ConnId> {
    let id = unsafe {
        CONNS.iter().position(|c| c.state == TcpState::Free)?
    };
    unsafe {
        CONNS[id] = TcpConn {
            state: TcpState::Listen,
            remote_ip: super::IpAddr::V4([0;4]), remote_port: 0, local_port,
            snd_nxt: 0, snd_una: 0, snd_cur: 0, rcv_nxt: 0, snd_wnd: 8192, mss: MSS_DEFAULT,
            retx_ticks: 0, retx_count: 0, retx_backoff: 1,
            state_tick: crate::arch::aarch64::timer::tick_count() as u32,
            cwnd: CWND_INIT, ssthresh: SSTHRESH_INIT,
            dup_acks: 0, last_ack_rcvd: 0,
            persist_ticks: 0, persist_arm: false,
            is_server: false, accept_port: 0,
            rx: None, tx: None,
        };
    }
    let uart = crate::drivers::uart::Uart::new();
    uart.puts("[tcp]  Listen port=");
    uart.put_dec(local_port as usize);
    uart.puts("\r\n");
    Some(id)
}

/// Block until a new connection arrives on the listener's port.
/// Returns a ConnId for the Established connection, or None on timeout.
pub fn tcp_accept(listener: ConnId, timeout_ms: u32) -> Option<ConnId> {
    if listener >= MAX_CONNS { return None; }
    let listen_port = unsafe { CONNS[listener].local_port };

    let freq = crate::arch::aarch64::timer::physical_timer_freq();
    let end  = crate::arch::aarch64::timer::physical_timer_count()
               + (timeout_ms as u64 * freq) / 1000;

    loop {
        super::poll_rx_only();

        // Scan for a server slot that has just become Established (is_server=true)
        let found = unsafe {
            CONNS.iter().position(|c|
                c.state       == TcpState::Established
                && c.is_server
                && c.accept_port == listen_port)
        };
        if let Some(id) = found {
            // Mark as claimed so subsequent tcp_accept calls don't return it again.
            unsafe { CONNS[id].is_server = false; }
            return Some(id);
        }

        if crate::arch::aarch64::timer::physical_timer_count() >= end {
            return None;
        }
        unsafe { core::arch::asm!("yield"); }
    }
}

// ── Incoming segment handler ──────────────────────────────────────────────────

pub(super) fn tcp_handle_segment(src_ip: super::IpAddr, payload: &[u8]) {
    if payload.len() < 20 { return; }

    let src_port = u16::from_be_bytes([payload[0], payload[1]]);
    let dst_port = u16::from_be_bytes([payload[2], payload[3]]);
    let seq      = u32::from_be_bytes([payload[4],payload[5],payload[6],payload[7]]);
    let ack_num  = u32::from_be_bytes([payload[8],payload[9],payload[10],payload[11]]);
    let doff     = ((payload[12] >> 4) as usize) * 4;
    let flags    = payload[13];
    let wnd      = u16::from_be_bytes([payload[14], payload[15]]);
    let data     = if doff <= payload.len() { &payload[doff..] } else { &[] };

    // Find matching connection
    let id = unsafe {
        CONNS.iter().position(|c|
            c.state != TcpState::Free
            && c.remote_ip   == src_ip
            && c.remote_port == src_port
            && c.local_port  == dst_port)
    };

    let id = match id {
        Some(i) => i,
        None => {
            // No established/connecting slot matches — check for a Listen slot on this port.
            if flags & TCP_SYN != 0 && flags & TCP_RST == 0 && flags & TCP_ACK == 0 {
                // Pure SYN: look for a Listen slot on dst_port.
                let listener = unsafe {
                    CONNS.iter().position(|c|
                        c.state == TcpState::Listen && c.local_port == dst_port)
                };
                if let Some(_listener_id) = listener {
                    // Allocate a new slot for the incoming connection.
                    if let Some(new_id) = unsafe {
                        CONNS.iter().position(|c| c.state == TcpState::Free)
                    } {
                        let rx = Box::new(RingBuf::new(BUF_SIZE));
                        let tx = Box::new(RingBuf::new(BUF_SIZE));
                        let isn = crate::arch::aarch64::timer::physical_timer_count() as u32
                                  ^ ((dst_port as u32) << 16);
                        let now = crate::arch::aarch64::timer::tick_count() as u32;
                        unsafe {
                            CONNS[new_id] = TcpConn {
                                state:        TcpState::SynRcvd,
                                remote_ip:    src_ip, // already IpAddr
                                remote_port:  src_port,
                                local_port:   dst_port,
                                snd_nxt:      isn,
                                snd_una:      isn,
                                snd_cur:      0,
                                rcv_nxt:      seq.wrapping_add(1),
                                snd_wnd:      wnd,
                                mss:          MSS_DEFAULT,
                                retx_ticks:   now + RETX_INIT,
                                retx_count:   0,
                                retx_backoff: 1,
                                state_tick:   now,
                                cwnd:         CWND_INIT,
                                ssthresh:     SSTHRESH_INIT,
                                dup_acks:     0,
                                last_ack_rcvd:isn,
                                persist_ticks:0,
                                persist_arm:  false,
                                is_server:    true,
                                accept_port:  dst_port,
                                rx:           Some(rx),
                                tx:           Some(tx),
                            };
                        }
                        // Send SYN-ACK (seq=ISN, ack=rcv_nxt=seq+1)
                        send_segment(new_id, TCP_SYN | TCP_ACK, &[]);
                        unsafe {
                            CONNS[new_id].snd_nxt = CONNS[new_id].snd_nxt.wrapping_add(1);
                        }
                    }
                    return;
                }
            }
            // No matching connection at all — send RST for non-RST segments
            if flags & TCP_RST == 0 {
                send_rst_raw(src_ip, src_port, dst_port, ack_num);
            }
            return;
        }
    };

    let c = unsafe { &mut CONNS[id] };

    // RST: close immediately
    if flags & TCP_RST != 0 {
        c.rx = None; c.tx = None; c.state = TcpState::Closed;
        return;
    }

    c.snd_wnd = wnd;

    match c.state {
        TcpState::SynRcvd => {
            // Awaiting the client's final ACK of our SYN-ACK.
            if flags & TCP_RST != 0 {
                c.rx = None; c.tx = None; c.state = TcpState::Free;
                return;
            }
            if flags & TCP_ACK != 0 && ack_num == c.snd_nxt {
                c.snd_una = ack_num;
                c.state   = TcpState::Established;
                c.retx_count = 0; c.retx_backoff = 1;
                // Process any data piggybacked on the final ACK.
                if !data.is_empty() && seq == c.rcv_nxt {
                    let pushed = if let Some(rx) = c.rx.as_mut() { rx.push(data) } else { 0 };
                    c.rcv_nxt = c.rcv_nxt.wrapping_add(pushed as u32);
                    send_segment(id, TCP_ACK, &[]);
                }
            }
        }

        TcpState::SynSent => {
            if flags & TCP_SYN != 0 && flags & TCP_ACK != 0 {
                // SYN-ACK received
                let uart = crate::drivers::uart::Uart::new();
                uart.puts("[tcp]  SYN-ACK from ");
                super::put_ipaddr(src_ip);
                uart.puts("\r\n");
                c.rcv_nxt = seq.wrapping_add(1);
                c.snd_una = ack_num;
                c.retx_count = 0; c.retx_backoff = 1;
                // Parse peer MSS from options
                let opts = &payload[20..doff.min(payload.len())];
                let mut oi = 0usize;
                while oi + 1 < opts.len() {
                    match opts[oi] {
                        0 => break,
                        1 => { oi += 1; continue; }
                        2 if oi + 3 < opts.len() => {
                            c.mss = u16::from_be_bytes([opts[oi+2], opts[oi+3]]);
                            oi += 4;
                        }
                        k => {
                            let l = opts[oi+1] as usize;
                            oi += 2 + l;
                            let _ = k;
                        }
                    }
                }
                c.state = TcpState::Established;
                // Send ACK
                send_segment(id, TCP_ACK, &[]);
            }
        }

        TcpState::Established | TcpState::FinWait1 | TcpState::FinWait2 => {
            // ── ACK processing with congestion control ───────────────────────
            if flags & TCP_ACK != 0 {
                let new_una = ack_num;
                let acked = new_una.wrapping_sub(c.snd_una) as usize;

                if acked > 0 && acked <= c.snd_cur.saturating_add(1) {
                    // ── New data acknowledged ─────────────────────────────────
                    // Congestion control (RFC 5681)
                    if c.cwnd < c.ssthresh {
                        // Slow start: increase cwnd by min(acked, MSS) per ACK
                        c.cwnd = c.cwnd.saturating_add(acked.min(c.mss as usize) as u32);
                    } else {
                        // Congestion avoidance: AIMD — +MSS per RTT (approx)
                        let inc = (c.mss as u32)
                            .saturating_mul(acked as u32)
                            .saturating_div(c.cwnd.max(1))
                            .max(1);
                        c.cwnd = c.cwnd.saturating_add(inc);
                    }

                    // Slide window
                    c.snd_una = new_una;
                    if let Some(tx) = c.tx.as_mut() { tx.discard(acked); }
                    c.snd_cur = c.snd_cur.saturating_sub(acked);
                    c.retx_count = 0;
                    c.retx_backoff = 1;
                    c.dup_acks = 0;
                    c.last_ack_rcvd = new_una;

                    // Disarm persist timer if window re-opened
                    if c.snd_wnd > 0 { c.persist_arm = false; }

                    // Try to send more data now that window opened
                    flush_tx(id);
                } else if acked == 0 && ack_num == c.last_ack_rcvd && data.is_empty() {
                    // ── Duplicate ACK ─────────────────────────────────────────
                    c.dup_acks = c.dup_acks.saturating_add(1);

                    if c.dup_acks == 3 {
                        // Fast retransmit (RFC 5681 §3.2)
                        let in_flight = c.snd_nxt.wrapping_sub(c.snd_una) as u32;
                        c.ssthresh = (in_flight / 2).max(c.mss as u32 * 2);
                        c.cwnd     = c.ssthresh.saturating_add(c.mss as u32 * 3);
                        // Reset send pointer to retransmit from snd_una
                        c.snd_nxt = c.snd_una;
                        c.snd_cur = 0;
                        flush_tx(id);
                    } else if c.dup_acks > 3 {
                        // Inflate cwnd to allow a new segment per dup ACK
                        c.cwnd = c.cwnd.saturating_add(c.mss as u32);
                        flush_tx(id);
                    }
                }

                // ── Arm zero-window persist timer ─────────────────────────────
                if c.snd_wnd == 0 && !c.persist_arm {
                    let now = crate::arch::aarch64::timer::tick_count() as u32;
                    c.persist_ticks = now + PERSIST_INIT;
                    c.persist_arm   = true;
                }
            }

            // ── Deliver in-order data ─────────────────────────────────────────
            if !data.is_empty() && seq == c.rcv_nxt {
                let pushed = if let Some(rx) = c.rx.as_mut() { rx.push(data) } else { 0 };
                c.rcv_nxt = c.rcv_nxt.wrapping_add(pushed as u32);
                send_segment(id, TCP_ACK, &[]); // ACK received data
            }

            // ── FIN from peer ─────────────────────────────────────────────────
            if flags & TCP_FIN != 0 {
                c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
                send_segment(id, TCP_ACK, &[]);
                match c.state {
                    TcpState::Established => { c.state = TcpState::CloseWait; }
                    TcpState::FinWait1    => { c.state = TcpState::Closing; }
                    TcpState::FinWait2    => {
                        c.state      = TcpState::TimeWait;
                        c.state_tick = crate::arch::aarch64::timer::tick_count() as u32;
                    }
                    _ => {}
                }
            }

            // ── Our FIN acknowledged → FinWait2 ──────────────────────────────
            if c.state == TcpState::FinWait1 && flags & TCP_ACK != 0 && ack_num == c.snd_nxt {
                c.state = TcpState::FinWait2;
            }
        }

        TcpState::LastAck => {
            if flags & TCP_ACK != 0 && ack_num == c.snd_nxt {
                c.rx = None; c.tx = None;
                c.state = TcpState::Free;
            }
        }

        _ => {}
    }
}

// ── Retransmit timer ──────────────────────────────────────────────────────────

pub(super) fn tcp_tick() {
    let now = crate::arch::aarch64::timer::tick_count() as u32;
    for id in 0..MAX_CONNS {
        let (state, snd_nxt, snd_una, retx_ticks, retx_count, retx_backoff,
             state_tick, mss, persist_arm, persist_ticks) = unsafe {
            let c = &CONNS[id];
            (c.state, c.snd_nxt, c.snd_una, c.retx_ticks,
             c.retx_count, c.retx_backoff, c.state_tick, c.mss as usize,
             c.persist_arm, c.persist_ticks)
        };

        match state {
            // ── SynRcvd: retransmit SYN-ACK or time out ──────────────────────
            TcpState::SynRcvd => {
                if now.wrapping_sub(state_tick) >= RETX_INIT * 10 {
                    // Timed out waiting for final ACK — retry or give up
                    if retx_count >= RETX_MAX {
                        unsafe { CONNS[id].rx = None; CONNS[id].tx = None;
                                 CONNS[id].state = TcpState::Free; }
                    } else {
                        send_segment(id, TCP_SYN | TCP_ACK, &[]);
                        unsafe {
                            CONNS[id].retx_count   += 1;
                            CONNS[id].state_tick    = now;
                        }
                    }
                }
            }

            // ── TimeWait / simultaneous-close expiry ─────────────────────────
            TcpState::TimeWait | TcpState::Closing => {
                if now.wrapping_sub(state_tick) >= TIMEWAIT_TICKS {
                    unsafe { CONNS[id].rx = None; CONNS[id].tx = None;
                             CONNS[id].state = TcpState::Free; }
                }
            }

            // ── LastAck: retransmit FIN-ACK if final ACK was lost ────────────
            TcpState::LastAck => {
                if now.wrapping_sub(state_tick) >= LASTACK_TIMEOUT {
                    if retx_count >= RETX_MAX {
                        unsafe { CONNS[id].rx = None; CONNS[id].tx = None;
                                 CONNS[id].state = TcpState::Free; }
                    } else {
                        send_segment(id, TCP_FIN | TCP_ACK, &[]);
                        unsafe {
                            CONNS[id].retx_count += 1;
                            CONNS[id].state_tick  = now;
                        }
                    }
                }
            }

            // ── Established / FinWait: retransmit + persist probe ────────────
            TcpState::Established | TcpState::FinWait1 | TcpState::FinWait2 => {
                // Zero-window persist probe
                if persist_arm && now >= persist_ticks {
                    // Send 1-byte probe to solicit a window update
                    let mut probe = [0u8; 1];
                    let has = unsafe {
                        CONNS[id].tx.as_ref()
                            .map(|t| t.peek_from(0, &mut probe[..]))
                            .unwrap_or(0)
                    };
                    if has > 0 {
                        send_segment(id, TCP_ACK, &probe[..1]);
                    } else {
                        send_segment(id, TCP_ACK, &[]);
                    }
                    // Exponential back-off for probe, capped at PERSIST_MAX
                    unsafe {
                        let interval = (PERSIST_INIT * CONNS[id].retx_backoff as u32)
                            .min(PERSIST_MAX);
                        CONNS[id].retx_backoff = CONNS[id].retx_backoff
                            .saturating_mul(2).min(64);
                        CONNS[id].persist_ticks = now + interval;
                    }
                }

                // Retransmit timer (only fires when there's unACKed data)
                let unacked = snd_nxt.wrapping_sub(snd_una) as usize;
                if unacked > 0 && now >= retx_ticks {
                    if retx_count >= RETX_MAX {
                        unsafe { CONNS[id].rx = None; CONNS[id].tx = None;
                                 CONNS[id].state = TcpState::Closed; }
                        continue;
                    }
                    // Timeout: enter slow start (RFC 5681 §5.1)
                    unsafe {
                        let in_flight = snd_nxt.wrapping_sub(snd_una) as u32;
                        CONNS[id].ssthresh = (in_flight / 2).max(CONNS[id].mss as u32 * 2);
                        CONNS[id].cwnd     = CONNS[id].mss as u32; // reset to 1 MSS
                        // Reset send pointer to retransmit from snd_una
                        CONNS[id].snd_nxt = snd_una;
                        CONNS[id].snd_cur = 0;
                    }
                    let avail = unsafe {
                        CONNS[id].tx.as_ref().map(|t| t.avail().min(mss)).unwrap_or(0)
                    };
                    let mut seg = [0u8; 1460];
                    let n = if avail > 0 {
                        unsafe { CONNS[id].tx.as_ref()
                                   .map(|t| t.peek_from(0, &mut seg[..avail])).unwrap_or(0) }
                    } else { 0 };
                    let flags = if state == TcpState::FinWait1 { TCP_FIN | TCP_ACK } else { TCP_ACK };
                    send_segment(id, flags, &seg[..n]);
                    unsafe {
                        CONNS[id].snd_nxt = CONNS[id].snd_nxt.wrapping_add(n as u32);
                        CONNS[id].snd_cur = n;
                        let new_bo = retx_backoff.saturating_mul(2).min(16);
                        CONNS[id].retx_count   = retx_count + 1;
                        CONNS[id].retx_backoff = new_bo;
                        CONNS[id].retx_ticks   = now + RETX_INIT * new_bo as u32;
                    }
                }
            }
            _ => {}
        }
    }
}
