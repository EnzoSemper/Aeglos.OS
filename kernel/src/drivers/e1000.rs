//! Intel e1000 NIC driver for Aeglos OS.
//!
//! Supports:
//!   * 82540EM (PCI vendor 0x8086, device 0x100E) — QEMU default `-device e1000`
//!   * 82574L / e1000e (device 0x10D3)
//!   * 82545EM (device 0x100F)
//!
//! Interface: MMIO via BAR0 (32-bit or 64-bit PCIe memory-mapped register block).
//!
//! Ring sizes: TX_RING_SIZE and RX_RING_SIZE are both 16 entries (power-of-two).
//!
//! Descriptor format: legacy (non-extended) 16-byte descriptors.
//!
//! MAC address: read from the EEPROM via the EERD register using the
//! "Start/Done" interface supported by 82540/82541/82545/82546/82547.
//!
//! DMA note: e1000 descriptors and packet buffers contain *physical* addresses.
//! All pointers obtained from the kernel allocator are high-VA; we convert them
//! with `vmm::to_dma_addr()` before writing into descriptor fields.
//! The MMIO register window (BAR0) sits in the 32-bit PCIe memory aperture
//! (< 4 GiB physical), which is identity-mapped via TTBR1, so we access it as
//! `PA + KERNEL_VA_OFFSET`.

extern crate alloc;

use core::ptr::{read_volatile, write_volatile};
use crate::memory::vmm::{KERNEL_VA_OFFSET, phys_to_virt, to_dma_addr};

// ── Register offsets (from BAR0) ─────────────────────────────────────────────

const E1000_CTRL:    usize = 0x0000; // Device Control
const E1000_STATUS:  usize = 0x0008; // Device Status
const E1000_EECD:    usize = 0x0010; // EEPROM/Flash Control/Data
const E1000_EERD:    usize = 0x0014; // EEPROM Read Register
const E1000_CTRL_EXT: usize = 0x0018; // Extended Device Control
const E1000_ICR:     usize = 0x00C0; // Interrupt Cause Read (write-to-clear)
const E1000_ITR:     usize = 0x00C4; // Interrupt Throttling Rate
const E1000_IMS:     usize = 0x00D0; // Interrupt Mask Set/Read
const E1000_IMC:     usize = 0x00D8; // Interrupt Mask Clear
const E1000_RCTL:    usize = 0x0100; // Receive Control
const E1000_TCTL:    usize = 0x0400; // Transmit Control
const E1000_TIPG:    usize = 0x0410; // TX Inter-Packet Gap
const E1000_RDBAL:   usize = 0x2800; // RX Descriptor Base Low
const E1000_RDBAH:   usize = 0x2804; // RX Descriptor Base High
const E1000_RDLEN:   usize = 0x2808; // RX Descriptor Length (bytes)
const E1000_RDH:     usize = 0x2810; // RX Descriptor Head
const E1000_RDT:     usize = 0x2818; // RX Descriptor Tail
const E1000_TDBAL:   usize = 0x3800; // TX Descriptor Base Low
const E1000_TDBAH:   usize = 0x3804; // TX Descriptor Base High
const E1000_TDLEN:   usize = 0x3808; // TX Descriptor Length (bytes)
const E1000_TDH:     usize = 0x3810; // TX Descriptor Head
const E1000_TDT:     usize = 0x3818; // TX Descriptor Tail
const E1000_MTA:     usize = 0x5200; // Multicast Table Array (128 × u32)
const E1000_RAL0:    usize = 0x5400; // Receive Address Low  (MAC[0..3])
const E1000_RAH0:    usize = 0x5404; // Receive Address High (MAC[4..5] + AV)

// ── Control register bits ─────────────────────────────────────────────────────

const CTRL_FD:     u32 = 1 <<  0; // Full-duplex
const CTRL_ASDE:   u32 = 1 <<  5; // Auto-speed detection enable
const CTRL_SLU:    u32 = 1 <<  6; // Set link up
const CTRL_RST:    u32 = 1 << 26; // Device reset

// ── Receive Control register bits ────────────────────────────────────────────

const RCTL_EN:        u32 = 1 <<  1; // Receiver enable
const RCTL_SBP:       u32 = 1 <<  2; // Store bad packets
const RCTL_UPE:       u32 = 1 <<  3; // Unicast promiscuous enable
const RCTL_MPE:       u32 = 1 <<  4; // Multicast promiscuous enable
const RCTL_LPE:       u32 = 1 <<  5; // Long packet enable
const RCTL_BAM:       u32 = 1 << 15; // Accept broadcast
const RCTL_BSIZE_2048: u32 = 0 << 16; // Buffer size 2048 B (default)
const RCTL_SECRC:     u32 = 1 << 26; // Strip Ethernet CRC

// ── Transmit Control register bits ───────────────────────────────────────────

const TCTL_EN:    u32 = 1 <<  1; // Transmitter enable
const TCTL_PSP:   u32 = 1 <<  3; // Pad short packets

// ── Descriptor status/command bits ───────────────────────────────────────────

const TXD_CMD_EOP:  u8 = 1 << 0; // End of packet
const TXD_CMD_IFCS: u8 = 1 << 1; // Insert FCS/CRC
const TXD_CMD_RS:   u8 = 1 << 3; // Report status (sets DD when done)
const TXD_STAT_DD:  u8 = 1 << 0; // Descriptor done

const RXD_STAT_DD:  u8 = 1 << 0; // Descriptor done (frame received)
const RXD_STAT_EOP: u8 = 1 << 1; // End of packet

// ── EERD (EEPROM Read) bits ───────────────────────────────────────────────────

const EERD_START: u32 = 1 <<  0;
const EERD_DONE:  u32 = 1 <<  4; // 82540/82541 done bit position
const EERD_DATA_SHIFT: u32 = 16;
const EERD_ADDR_SHIFT: u32 = 8;

// ── Ring sizes ────────────────────────────────────────────────────────────────

const TX_RING_SIZE: usize = 16;
const RX_RING_SIZE: usize = 16;
const RX_BUF_SIZE:  usize = 2048;

// ── Legacy TX descriptor (16 bytes, §3.4 of Intel e1000 SDM) ─────────────────

#[repr(C, packed)]
struct TxDesc {
    buffer_addr: u64, // Physical address of packet buffer
    length:      u16, // Packet length in bytes
    cso:         u8,  // Checksum offset (unused — 0)
    cmd:         u8,  // Command byte: EOP | IFCS | RS
    status:      u8,  // Status: bit 0 = DD
    css:         u8,  // Checksum start (unused — 0)
    special:     u16, // VLAN (unused — 0)
}

// ── Legacy RX descriptor (16 bytes, §3.2 of Intel e1000 SDM) ─────────────────

#[repr(C, packed)]
struct RxDesc {
    buffer_addr: u64, // Physical address of receive buffer
    length:      u16, // Bytes received (filled by hardware)
    checksum:    u16, // Packet checksum (filled by hardware)
    status:      u8,  // Status: bit 0=DD, bit 1=EOP
    errors:      u8,  // Error flags
    special:     u16, // VLAN info (unused)
}

// ── Driver state ──────────────────────────────────────────────────────────────

/// Kernel-virtual base of the e1000 MMIO register window.
static mut MMIO_VA: usize = 0;

/// True once `init()` succeeds.
static mut E1000_UP: bool = false;

/// Device MAC address (read from EEPROM at init).
static mut MAC: [u8; 6] = [0u8; 6];

// TX ring — virtual pointer (descriptor array + associated packet buffers)
static mut TX_DESCS: *mut TxDesc = core::ptr::null_mut();
static mut TX_BUFS:  *mut u8     = core::ptr::null_mut(); // TX_RING_SIZE × 2048
static mut TX_TAIL:  usize       = 0;

// RX ring
static mut RX_DESCS: *mut RxDesc = core::ptr::null_mut();
static mut RX_BUFS:  *mut u8     = core::ptr::null_mut(); // RX_RING_SIZE × 2048
static mut RX_TAIL:  usize       = 0;
static mut RX_HEAD_SHADOW: usize = 0; // tracks next descriptor to consume

// ── MMIO helpers ─────────────────────────────────────────────────────────────

#[inline(always)]
unsafe fn reg_read(off: usize) -> u32 {
    read_volatile((MMIO_VA + off) as *const u32)
}

#[inline(always)]
unsafe fn reg_write(off: usize, val: u32) {
    write_volatile((MMIO_VA + off) as *mut u32, val);
}

// ── EEPROM read (82540/82541 "Start/Done" interface) ─────────────────────────

/// Read a 16-bit word from EEPROM address `addr`.
unsafe fn eeprom_read(addr: u16) -> u16 {
    // Write address + START bit
    reg_write(E1000_EERD,
        EERD_START | ((addr as u32) << EERD_ADDR_SHIFT));
    // Poll DONE bit (bit 4 for 82540, bit 1 for some variants — check both)
    let mut tries = 0u32;
    loop {
        let v = reg_read(E1000_EERD);
        if (v & EERD_DONE) != 0 || (v & (1 << 1)) != 0 {
            return (v >> EERD_DATA_SHIFT) as u16;
        }
        tries += 1;
        if tries > 100_000 {
            // Timed out — return a sentinel value; MAC bytes will be 0xFF
            return 0xFFFF;
        }
        core::hint::spin_loop();
    }
}

// ── Descriptor ring allocation ────────────────────────────────────────────────

/// Allocate `count` bytes of kernel heap memory with `align`-byte alignment.
/// Returns a kernel-VA pointer.
unsafe fn alloc_aligned(size: usize, align: usize) -> *mut u8 {
    use alloc::alloc::{alloc, Layout};
    let layout = Layout::from_size_align(size, align)
        .expect("e1000: invalid layout");
    let ptr = alloc(layout);
    if ptr.is_null() {
        panic!("e1000: allocation failed ({} bytes, align {})", size, align);
    }
    // Zero the region
    core::ptr::write_bytes(ptr, 0, size);
    ptr
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the e1000 NIC at the given BAR0 physical address.
///
/// `bar0_pa` is the *physical* base of the e1000 register window.
/// It is mapped into the kernel VA space by adding `KERNEL_VA_OFFSET`
/// (works because BAR0 is always in the 32-bit PCIe window < 4 GiB, and
/// the TTBR1 identity+offset map covers all physical RAM + MMIO).
///
/// Returns `true` on success.
pub fn init(bar0_pa: usize) -> bool {
    if bar0_pa == 0 {
        return false;
    }

    let uart = crate::drivers::uart::Uart::new();

    // Map BAR0 into kernel VA.  The BAR is in the 32-bit PCIe window so it
    // is covered by the TTBR1 device-attribute identity mapping.
    let mmio_va = bar0_pa + KERNEL_VA_OFFSET;
    unsafe { MMIO_VA = mmio_va; }

    uart.puts("[e1000] BAR0 phys=0x");
    uart.put_hex(bar0_pa);
    uart.puts(" → VA=0x");
    uart.put_hex(mmio_va);
    uart.puts("\r\n");

    unsafe {
        // ── 1. Reset the device ───────────────────────────────────────────
        reg_write(E1000_CTRL, reg_read(E1000_CTRL) | CTRL_RST);
        // Brief spin to let reset complete (~1 µs is enough, but we do more)
        for _ in 0..10_000u32 { core::hint::spin_loop(); }
        // Mask all interrupts immediately after reset
        reg_write(E1000_IMC, 0xFFFF_FFFF);
        let _ = reg_read(E1000_ICR); // clear any pending

        // ── 2. Verify we can read a sensible STATUS ───────────────────────
        let status = reg_read(E1000_STATUS);
        uart.puts("[e1000] STATUS=0x");
        uart.put_hex(status as usize);
        uart.puts("\r\n");

        // ── 3. Read MAC from EEPROM ───────────────────────────────────────
        let w0 = eeprom_read(0);
        let w1 = eeprom_read(1);
        let w2 = eeprom_read(2);
        // EEPROM words are little-endian pairs of MAC bytes
        MAC[0] = (w0 & 0xFF) as u8;
        MAC[1] = (w0 >> 8)   as u8;
        MAC[2] = (w1 & 0xFF) as u8;
        MAC[3] = (w1 >> 8)   as u8;
        MAC[4] = (w2 & 0xFF) as u8;
        MAC[5] = (w2 >> 8)   as u8;

        // If EEPROM timed out (all 0xFF), fall back to RAL0/RAH0
        if MAC == [0xFF; 6] || MAC == [0x00; 6] {
            let ral = reg_read(E1000_RAL0);
            let rah = reg_read(E1000_RAH0);
            MAC[0] = (ral & 0xFF) as u8;
            MAC[1] = ((ral >>  8) & 0xFF) as u8;
            MAC[2] = ((ral >> 16) & 0xFF) as u8;
            MAC[3] = ((ral >> 24) & 0xFF) as u8;
            MAC[4] = (rah & 0xFF) as u8;
            MAC[5] = ((rah >> 8) & 0xFF) as u8;
        }

        uart.puts("[e1000] MAC=");
        for i in 0..6usize {
            uart.put_hex(MAC[i] as usize);
            if i < 5 { uart.puts(":"); }
        }
        uart.puts("\r\n");

        // ── 4. Program Receive Address (filter our own MAC + broadcasts) ──
        let ral_val: u32 = (MAC[0] as u32)
                         | ((MAC[1] as u32) <<  8)
                         | ((MAC[2] as u32) << 16)
                         | ((MAC[3] as u32) << 24);
        let rah_val: u32 = (MAC[4] as u32)
                         | ((MAC[5] as u32) << 8)
                         | (1 << 31); // AV = Address Valid
        reg_write(E1000_RAL0, ral_val);
        reg_write(E1000_RAH0, rah_val);

        // Clear the Multicast Table Array (receive no multicast by default)
        for i in 0..128usize {
            reg_write(E1000_MTA + i * 4, 0);
        }

        // ── 5. Set up TX descriptor ring ─────────────────────────────────
        //
        // Allocate the descriptor array (16-byte aligned, as required by HW).
        let tx_desc_size = TX_RING_SIZE * core::mem::size_of::<TxDesc>();
        let tx_desc_va   = alloc_aligned(tx_desc_size, 16);
        TX_DESCS = tx_desc_va as *mut TxDesc;

        // Allocate TX packet buffers (one 2048-byte buffer per descriptor).
        let tx_buf_size = TX_RING_SIZE * RX_BUF_SIZE;
        let tx_buf_va   = alloc_aligned(tx_buf_size, 16);
        TX_BUFS = tx_buf_va as *mut u8;

        // Fill TX descriptors with buffer PAs (status will be set per-send).
        for i in 0..TX_RING_SIZE {
            let buf_va  = tx_buf_va.add(i * RX_BUF_SIZE) as usize;
            let buf_pa  = to_dma_addr(buf_va) as u64;
            let desc    = &mut *TX_DESCS.add(i);
            desc.buffer_addr = buf_pa;
            desc.length      = 0;
            desc.cso         = 0;
            desc.cmd         = 0;
            desc.status      = TXD_STAT_DD; // mark as done so driver can use them
            desc.css         = 0;
            desc.special     = 0;
        }
        TX_TAIL = 0;

        // Program TX ring base address and length into hardware.
        let tx_desc_pa = to_dma_addr(tx_desc_va as usize) as u64;
        reg_write(E1000_TDBAL, (tx_desc_pa & 0xFFFF_FFFF) as u32);
        reg_write(E1000_TDBAH, (tx_desc_pa >> 32) as u32);
        reg_write(E1000_TDLEN, (TX_RING_SIZE * core::mem::size_of::<TxDesc>()) as u32);
        reg_write(E1000_TDH, 0);
        reg_write(E1000_TDT, 0);

        // TX control: enable, pad short packets, collision threshold=0x0F, distance=0x40
        let tctl = TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x40 << 12);
        reg_write(E1000_TCTL, tctl);

        // TX Inter-Packet Gap: recommended value for 802.3 from datasheet §13.4.34
        reg_write(E1000_TIPG, 0x0060_2006); // IPGT=6, IPGR1=6, IPGR2=6

        // ── 6. Set up RX descriptor ring ─────────────────────────────────
        let rx_desc_size = RX_RING_SIZE * core::mem::size_of::<RxDesc>();
        let rx_desc_va   = alloc_aligned(rx_desc_size, 16);
        RX_DESCS = rx_desc_va as *mut RxDesc;

        // Allocate RX packet buffers.
        let rx_buf_size = RX_RING_SIZE * RX_BUF_SIZE;
        let rx_buf_va   = alloc_aligned(rx_buf_size, 16);
        RX_BUFS = rx_buf_va as *mut u8;

        // Fill RX descriptors — point each at a unique 2048-byte buffer.
        for i in 0..RX_RING_SIZE {
            let buf_va  = rx_buf_va.add(i * RX_BUF_SIZE) as usize;
            let buf_pa  = to_dma_addr(buf_va) as u64;
            let desc    = &mut *RX_DESCS.add(i);
            desc.buffer_addr = buf_pa;
            desc.length   = 0;
            desc.checksum = 0;
            desc.status   = 0;
            desc.errors   = 0;
            desc.special  = 0;
        }
        RX_TAIL        = 0;
        RX_HEAD_SHADOW = 0;

        // Program RX ring base address and length.
        let rx_desc_pa = to_dma_addr(rx_desc_va as usize) as u64;
        reg_write(E1000_RDBAL, (rx_desc_pa & 0xFFFF_FFFF) as u32);
        reg_write(E1000_RDBAH, (rx_desc_pa >> 32) as u32);
        reg_write(E1000_RDLEN, rx_desc_size as u32);
        reg_write(E1000_RDH, 0);
        // Set tail to (RX_RING_SIZE - 1) to give the HW all but one descriptor.
        reg_write(E1000_RDT, (RX_RING_SIZE - 1) as u32);
        RX_TAIL = RX_RING_SIZE - 1;

        // ── 7. RX Control ─────────────────────────────────────────────────
        let rctl = RCTL_EN
                 | RCTL_BAM        // accept broadcasts
                 | RCTL_UPE        // unicast promiscuous
                 | RCTL_MPE        // multicast promiscuous
                 | RCTL_BSIZE_2048 // 2 KiB buffers
                 | RCTL_SECRC;     // strip FCS
        reg_write(E1000_RCTL, rctl);

        // ── 8. Link up ────────────────────────────────────────────────────
        let ctrl = reg_read(E1000_CTRL);
        reg_write(E1000_CTRL, ctrl | CTRL_SLU | CTRL_ASDE | CTRL_FD);

        // All interrupts remain masked (polling mode).
        reg_write(E1000_IMC, 0xFFFF_FFFF);

        E1000_UP = true;
    }

    uart.puts("[e1000] Initialized\r\n");
    true
}

/// Returns `true` if the e1000 driver is up.
pub fn is_up() -> bool {
    unsafe { E1000_UP }
}

/// Returns the device MAC address.
pub fn mac() -> [u8; 6] {
    unsafe { MAC }
}

/// Returns `true` if the link is up (STATUS.LU bit set).
pub fn link_up() -> bool {
    unsafe {
        if !E1000_UP { return false; }
        (reg_read(E1000_STATUS) & (1 << 1)) != 0
    }
}

/// Transmit a raw Ethernet frame (without FCS).
///
/// The caller provides the full Ethernet frame starting from the destination
/// MAC.  Returns `true` on success, `false` if the ring is full or the driver
/// is not up.
pub fn send(data: &[u8]) -> bool {
    if data.len() > RX_BUF_SIZE { return false; }
    unsafe {
        if !E1000_UP { return false; }

        // Check that the next TX slot is free (DD bit set by hardware).
        let next = TX_TAIL % TX_RING_SIZE;
        let desc = &mut *TX_DESCS.add(next);

        // Read status via volatile to avoid optimising away the load.
        // Use addr_of! because TxDesc is #[repr(C, packed)] — fields may be unaligned.
        let stat = read_volatile(core::ptr::addr_of!(desc.status));
        if (stat & TXD_STAT_DD) == 0 {
            // Ring full — caller should retry later.
            return false;
        }

        // Copy frame into the pre-allocated TX buffer (already PA-registered
        // in the descriptor at init time — no descriptor.buffer_addr update needed).
        let buf_va = TX_BUFS.add(next * RX_BUF_SIZE);
        core::ptr::copy_nonoverlapping(data.as_ptr(), buf_va, data.len());

        // Fill in length and command fields.
        write_volatile(core::ptr::addr_of_mut!(desc.length), data.len() as u16);
        write_volatile(core::ptr::addr_of_mut!(desc.cmd), TXD_CMD_EOP | TXD_CMD_IFCS | TXD_CMD_RS);
        write_volatile(core::ptr::addr_of_mut!(desc.status), 0u8); // clear DD so HW knows to send

        // Memory barrier before bumping tail.
        core::arch::asm!("dmb sy", options(nostack, preserves_flags));

        TX_TAIL = TX_TAIL.wrapping_add(1);
        reg_write(E1000_TDT, (TX_TAIL % TX_RING_SIZE) as u32);

        true
    }
}

/// Poll for a received Ethernet frame (non-blocking).
///
/// If a frame is available, copies it into `buf` and returns the frame length.
/// Returns `0` if no frame is ready or the driver is not up.
pub fn recv(buf: &mut [u8]) -> usize {
    unsafe {
        if !E1000_UP { return 0; }

        let next = RX_HEAD_SHADOW % RX_RING_SIZE;
        let desc = &mut *RX_DESCS.add(next);

        // Memory barrier before reading status.
        core::arch::asm!("dmb sy", options(nostack, preserves_flags));

        // Use addr_of! because RxDesc is #[repr(C, packed)].
        let stat = read_volatile(core::ptr::addr_of!(desc.status));
        if (stat & RXD_STAT_DD) == 0 {
            return 0; // No frame ready
        }

        let len = read_volatile(core::ptr::addr_of!(desc.length)) as usize;
        if len == 0 || len > buf.len() {
            // Recycle descriptor and return 0
            write_volatile(core::ptr::addr_of_mut!(desc.status), 0u8);
            RX_HEAD_SHADOW = RX_HEAD_SHADOW.wrapping_add(1);
            // Advance tail to give HW back the descriptor
            RX_TAIL = RX_TAIL.wrapping_add(1);
            reg_write(E1000_RDT, (RX_TAIL % RX_RING_SIZE) as u32);
            return 0;
        }

        // Copy received bytes into caller's buffer.
        let src = RX_BUFS.add(next * RX_BUF_SIZE);
        core::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), len);

        // Recycle: clear status, advance head shadow, bump tail.
        write_volatile(core::ptr::addr_of_mut!(desc.status), 0u8);
        RX_HEAD_SHADOW = RX_HEAD_SHADOW.wrapping_add(1);
        RX_TAIL = RX_TAIL.wrapping_add(1);
        reg_write(E1000_RDT, (RX_TAIL % RX_RING_SIZE) as u32);

        len
    }
}
