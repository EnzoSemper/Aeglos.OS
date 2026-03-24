//! VirtIO Network Driver (Legacy MMIO Interface)
//!
//! QEMU virt machine: scans VirtIO MMIO slots for device ID 1 (net).
//! Uses legacy (v1) queue interface with two virtqueues:
//!   Queue 0 — RX (device writes incoming frames into driver-provided buffers)
//!   Queue 1 — TX (driver writes outgoing frames for device to send)
//!
//! Feature negotiation: accepts VIRTIO_NET_F_MAC (bit 5) to read the
//! hardware MAC address; rejects all GSO/checksum offload features so
//! we always send/receive plain Ethernet frames.
//!
//! ## RX operation
//!
//! The driver pre-fills the RX queue with `QUEUE_SIZE` static buffers at init.
//! Each buffer is 2048 bytes: first 10 bytes are the `virtio_net_hdr`, followed
//! by the actual Ethernet frame.  `receive()` polls the used ring, copies the
//! payload (past the header) into the caller's buffer, and immediately recycles
//! the descriptor back into the avail ring.
//!
//! ## TX operation
//!
//! `transmit()` prepends a zeroed 10-byte `virtio_net_hdr` (plain TX, no GSO)
//! to the caller's packet in the static `TX_BUF`, adds one descriptor to the
//! TX avail ring, notifies the device, and polls the TX used ring for completion.

use core::ptr::{read_volatile, write_volatile};

// ── MMIO Transport ────────────────────────────────────────────────────────────

const VIRTIO_MMIO_BASE:   usize = 0x0A00_0000 + crate::memory::vmm::KERNEL_VA_OFFSET;
const VIRTIO_MMIO_STRIDE: usize = 0x200;

// Register offsets (identical to virtio.rs block driver)
const REG_MAGIC:           usize = 0x000;
const REG_DEVICE_ID:       usize = 0x008;
const REG_HOST_FEATURES:   usize = 0x010;
const REG_GUEST_FEATURES:  usize = 0x020;
const REG_GUEST_PAGE_SIZE: usize = 0x028;
const REG_QUEUE_SEL:       usize = 0x030;
const REG_QUEUE_NUM:       usize = 0x038;
const REG_QUEUE_ALIGN:     usize = 0x03C;
const REG_QUEUE_PFN:       usize = 0x040;
const REG_QUEUE_NOTIFY:    usize = 0x050;
const REG_STATUS:          usize = 0x070;

// Net-specific: config space (MAC address at byte offsets 0–5)
const REG_NET_CFG: usize = 0x100;

// Status bits
const STATUS_ACK:       u32 = 1;
const STATUS_DRIVER:    u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;

// Feature bits
const VIRTIO_NET_F_MAC: u32 = 1 << 5;

const VIRTIO_MAGIC:      u32 = 0x74726976;
const VIRTIO_DEVICE_NET: u32 = 1;

// ── Queue Configuration ───────────────────────────────────────────────────────

const QUEUE_SIZE:  usize = 16;
const QUEUE_ALIGN: usize = 4096;

// Vring memory layout (same formula as block driver)
const DESC_OFFSET:  usize = 0;
const AVAIL_OFFSET: usize = QUEUE_SIZE * 16; // 256
const USED_OFFSET:  usize = QUEUE_ALIGN;     // 4096

const VRING_DESC_F_WRITE: u16 = 2; // device-writable descriptor

// ── Packet Layout ─────────────────────────────────────────────────────────────

/// Size of `virtio_net_hdr` without MRGRXBUF (10 bytes).
const NET_HDR_SIZE: usize = 10;
/// Per-buffer capacity: header + max Ethernet frame (1514 B) + headroom.
const RX_BUF_SIZE:  usize = 2048;

// ── Structures ────────────────────────────────────────────────────────────────

/// Virtqueue descriptor (16 bytes per spec).
#[repr(C)]
struct VirtqDesc {
    addr:  u64,
    len:   u32,
    flags: u16,
    next:  u16,
}

// ── Driver State ──────────────────────────────────────────────────────────────

static mut NET_BASE: usize  = 0;
static mut NET_MAC:  [u8; 6] = [0; 6];

static mut RX_VRING:     usize = 0;
static mut RX_AVAIL_IDX: u16   = 0;
static mut RX_LAST_USED: u16   = 0;

static mut TX_VRING:     usize = 0;
static mut TX_AVAIL_IDX: u16   = 0;
static mut TX_LAST_USED: u16   = 0;

/// Pre-allocated receive buffers (static, in BSS).
static mut RX_BUFS: [[u8; RX_BUF_SIZE]; QUEUE_SIZE] = [[0; RX_BUF_SIZE]; QUEUE_SIZE];
/// Transmit buffer: [virtio_net_hdr(10)] + [packet payload].
static mut TX_BUF: [u8; RX_BUF_SIZE] = [0; RX_BUF_SIZE];

// ── MMIO Helpers ──────────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn mmio_read(base: usize, off: usize) -> u32 {
    let v: u32;
    core::arch::asm!("ldr {:w}, [{}]", out(reg) v, in(reg) base + off,
                     options(nostack, preserves_flags, readonly));
    v
}

#[inline(always)]
unsafe fn mmio_write(base: usize, off: usize, val: u32) {
    core::arch::asm!("str {:w}, [{}]", in(reg) val, in(reg) base + off,
                     options(nostack, preserves_flags));
}

/// Byte-level MMIO read (for config space).
#[inline(always)]
unsafe fn mmio_read_u8(base: usize, off: usize) -> u8 {
    let v: u32;
    core::arch::asm!("ldrb {:w}, [{}]", out(reg) v, in(reg) base + off,
                     options(nostack, preserves_flags, readonly));
    v as u8
}

// ── Internal Helpers ──────────────────────────────────────────────────────────

/// Allocate a vring (2 contiguous pages), configure the queue, and return
/// the physical address of the vring.
unsafe fn setup_queue(base: usize, queue_idx: u32) -> usize {
    mmio_write(base, REG_QUEUE_SEL, queue_idx);
    mmio_write(base, REG_QUEUE_NUM, QUEUE_SIZE as u32);
    mmio_write(base, REG_QUEUE_ALIGN, QUEUE_ALIGN as u32);

    let p0 = crate::memory::alloc_page().expect("virtio_net: vring p0");
    let p1 = crate::memory::alloc_page().expect("virtio_net: vring p1");
    assert!(p1 == p0 + 4096, "virtio_net: vring pages not contiguous");

    // p0 is PA; zero via high-VA alias (already zeroed by alloc_page, be explicit).
    core::ptr::write_bytes(crate::memory::vmm::phys_to_virt(p0) as *mut u8, 0, 8192);
    mmio_write(base, REG_QUEUE_PFN, (p0 >> 12) as u32);
    p0  // returned as PA; stored in RX_VRING/TX_VRING
}

/// Pre-fill the RX virtqueue with `QUEUE_SIZE` static receive buffers.
/// Called once during `init()` after RX_VRING is set up.
unsafe fn fill_rx_queue() {
    // RX_VRING is stored as PA; CPU must access via high-VA alias.
    let vring_va  = crate::memory::vmm::phys_to_virt(RX_VRING);
    let desc_base = (vring_va + DESC_OFFSET)  as *mut VirtqDesc;
    let avail     = (vring_va + AVAIL_OFFSET) as *mut u16;
    // avail.flags = avail[0], avail.idx = avail[1], avail.ring = avail[2..]

    for i in 0..QUEUE_SIZE {
        let d = &mut *desc_base.add(i);
        // RX_BUFS is a BSS static at high VA; device needs PA.
        d.addr  = crate::memory::vmm::to_dma_addr(RX_BUFS[i].as_ptr() as usize) as u64;
        d.len   = RX_BUF_SIZE as u32;
        d.flags = VRING_DESC_F_WRITE; // device writes into this buffer
        d.next  = 0;
        write_volatile(avail.add(2 + i), i as u16);
    }

    core::arch::asm!("dmb sy");
    write_volatile(avail.add(1), QUEUE_SIZE as u16);
    core::arch::asm!("dmb sy");

    RX_AVAIL_IDX = QUEUE_SIZE as u16;
    mmio_write(NET_BASE, REG_QUEUE_NOTIFY, 0); // notify RX queue
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialize the VirtIO network device.
///
/// Scans VirtIO MMIO transports for a net device, negotiates features,
/// configures RX + TX virtqueues, reads the MAC address, and pre-fills
/// the RX queue.  Prints a message if no device is found (non-fatal).
pub unsafe fn init() {
    let uart = crate::drivers::uart::Uart::new();

    // Scan for net device (ID = 1)
    let mut base = 0usize;
    let mut found = false;
    for i in 0..32 {
        let b = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;
        if mmio_read(b, REG_MAGIC) == VIRTIO_MAGIC
            && mmio_read(b, REG_DEVICE_ID) == VIRTIO_DEVICE_NET
        {
            base  = b;
            found = true;
            break;
        }
    }

    if !found {
        uart.puts("[net]  VirtIO-net not found (add -device virtio-net-device to QEMU)\r\n");
        return;
    }

    NET_BASE = base;

    // Reset → ACK → DRIVER
    mmio_write(base, REG_STATUS, 0);
    let mut status = STATUS_ACK | STATUS_DRIVER;
    mmio_write(base, REG_STATUS, status);

    // Accept MAC feature only; reject GSO, checksum offload, etc.
    let host_feat = mmio_read(base, REG_HOST_FEATURES);
    mmio_write(base, REG_GUEST_FEATURES, host_feat & VIRTIO_NET_F_MAC);

    // Legacy: tell device our page size
    mmio_write(base, REG_GUEST_PAGE_SIZE, 4096);

    // Read MAC address from config space (byte-level)
    for i in 0..6usize {
        NET_MAC[i] = mmio_read_u8(base, REG_NET_CFG + i);
    }

    // Set up RX queue (index 0) and TX queue (index 1)
    RX_VRING = setup_queue(base, 0);
    TX_VRING = setup_queue(base, 1);

    // Driver ready
    status |= STATUS_DRIVER_OK;
    mmio_write(base, REG_STATUS, status);

    // Pre-fill RX queue with receive buffers
    fill_rx_queue();

    uart.puts("[net]  VirtIO-net ready  MAC=");
    for i in 0..6 {
        uart.put_hex(NET_MAC[i] as usize);
        if i < 5 { uart.puts(":"); }
    }
    uart.puts("\r\n");
}

/// Returns `true` if the driver was successfully initialized.
pub fn is_up() -> bool {
    unsafe { NET_BASE != 0 }
}

/// Returns the device MAC address.
pub fn mac() -> [u8; 6] {
    unsafe { NET_MAC }
}

/// Transmit a raw Ethernet frame (without FCS — the device appends it).
///
/// Prepends a zeroed 10-byte `virtio_net_hdr` (no GSO, no checksum) and
/// queues the combined buffer as a single device-readable descriptor.
/// Polls the TX used ring until the device signals completion.
///
/// Returns `false` if the driver is not up or the frame exceeds `RX_BUF_SIZE`.
pub unsafe fn transmit(packet: &[u8]) -> bool {
    if NET_BASE == 0 { return false; }
    let total = NET_HDR_SIZE + packet.len();
    if total > RX_BUF_SIZE { return false; }

    // Build TX_BUF = [hdr(10 zero bytes)] + [packet]
    for b in TX_BUF[..NET_HDR_SIZE].iter_mut() { *b = 0; }
    TX_BUF[NET_HDR_SIZE..total].copy_from_slice(packet);

    // TX_VRING is stored as PA; CPU must access via high-VA alias.
    let vring_va = crate::memory::vmm::phys_to_virt(TX_VRING);

    // Descriptor 0: device-readable, single-entry chain
    let desc = (vring_va + DESC_OFFSET) as *mut VirtqDesc;
    let d = &mut *desc.add(0);
    // TX_BUF is a BSS static at high VA; device needs PA.
    d.addr  = crate::memory::vmm::to_dma_addr(TX_BUF.as_ptr() as usize) as u64;
    d.len   = total as u32;
    d.flags = 0; // no NEXT, no WRITE → device reads this buffer
    d.next  = 0;

    // Add to TX avail ring
    let avail     = (vring_va + AVAIL_OFFSET) as *mut u16;
    let ring_slot = TX_AVAIL_IDX as usize % QUEUE_SIZE;
    write_volatile(avail.add(2 + ring_slot), 0u16); // head descriptor index = 0

    core::arch::asm!("dmb sy");
    TX_AVAIL_IDX = TX_AVAIL_IDX.wrapping_add(1);
    write_volatile(avail.add(1), TX_AVAIL_IDX);
    core::arch::asm!("dmb sy");

    mmio_write(NET_BASE, REG_QUEUE_NOTIFY, 1); // notify TX queue

    // Poll TX used ring
    let used_idx_ptr = (vring_va + USED_OFFSET + 2) as *const u16;
    let expected     = TX_LAST_USED.wrapping_add(1);
    loop {
        core::arch::asm!("dmb sy");
        if read_volatile(used_idx_ptr) == expected { break; }
        core::hint::spin_loop();
    }
    TX_LAST_USED = expected;
    true
}

/// Poll for a received Ethernet frame (non-blocking).
///
/// If a frame is available, copies the payload (past the `virtio_net_hdr`)
/// into `buf` and returns `Some(frame_length)`. Returns `None` if no frame
/// is ready. The receive buffer is immediately recycled after the copy.
pub unsafe fn receive(buf: &mut [u8]) -> Option<usize> {
    if NET_BASE == 0 { return None; }

    // RX_VRING is stored as PA; CPU must access via high-VA alias.
    let vring_va     = crate::memory::vmm::phys_to_virt(RX_VRING);
    let used_idx_ptr = (vring_va + USED_OFFSET + 2) as *const u16;

    core::arch::asm!("dmb sy");
    if read_volatile(used_idx_ptr) == RX_LAST_USED {
        return None;
    }

    // Used ring element: struct { u32 id, u32 len } at used.ring[i]
    let used_elems = (vring_va + USED_OFFSET + 4) as *const u32;
    let slot       = RX_LAST_USED as usize % QUEUE_SIZE;
    let desc_id    = read_volatile(used_elems.add(slot * 2))     as usize;
    let total_len  = read_volatile(used_elems.add(slot * 2 + 1)) as usize;

    RX_LAST_USED = RX_LAST_USED.wrapping_add(1);

    // Payload starts after the 10-byte virtio_net_hdr
    let pkt_len  = if total_len > NET_HDR_SIZE { total_len - NET_HDR_SIZE } else { 0 };
    let copy_len = pkt_len.min(buf.len());
    if copy_len > 0 {
        buf[..copy_len]
            .copy_from_slice(&RX_BUFS[desc_id][NET_HDR_SIZE..NET_HDR_SIZE + copy_len]);
    }

    // Recycle descriptor back to the RX avail ring
    let avail      = (vring_va + AVAIL_OFFSET) as *mut u16;
    let avail_slot = RX_AVAIL_IDX as usize % QUEUE_SIZE;
    write_volatile(avail.add(2 + avail_slot), desc_id as u16);

    core::arch::asm!("dmb sy");
    RX_AVAIL_IDX = RX_AVAIL_IDX.wrapping_add(1);
    write_volatile(avail.add(1), RX_AVAIL_IDX);
    core::arch::asm!("dmb sy");

    mmio_write(NET_BASE, REG_QUEUE_NOTIFY, 0); // tell device there's a fresh RX buffer

    Some(copy_len)
}
