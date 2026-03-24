//! VirtIO Block Driver (Legacy MMIO Interface)
//!
//! QEMU virt machine: 32 VirtIO MMIO transports at 0x0A000000, stride 0x200.
//! Uses legacy (v1) queue interface with QueuePFN/QueueAlign.
//!
//! ## Vring Layout (QUEUE_SIZE=16, QUEUE_ALIGN=4096)
//!
//! The legacy VirtIO spec defines the vring as a contiguous region:
//!   - Descriptors at offset 0: 16 entries × 16 bytes = 256 bytes
//!   - Avail ring at offset 256: 2 + 2 + 2×16 + 2 = 38 bytes (ends at 294)
//!   - [Padding to next QUEUE_ALIGN boundary]
//!   - Used ring at offset 4096: 2 + 2 + 8×16 + 2 = 134 bytes (ends at 4230)
//!
//! Total: 2 pages (8192 bytes).

use core::ptr::{read_volatile, write_volatile};
use crate::memory;

// ── MMIO Transport Addresses ──

const VIRTIO_MMIO_BASE: usize = 0x0A00_0000 + crate::memory::vmm::KERNEL_VA_OFFSET;
const VIRTIO_MMIO_STRIDE: usize = 0x200;

// ── MMIO Register Offsets ──

const REG_MAGIC: usize         = 0x000;
const REG_VERSION: usize       = 0x004;
const REG_DEVICE_ID: usize     = 0x008;
const REG_VENDOR_ID: usize     = 0x00C;
const REG_HOST_FEATURES: usize = 0x010;
const REG_HOST_FEATURES_SEL: usize = 0x014;
const REG_GUEST_FEATURES: usize    = 0x020;
const REG_GUEST_FEATURES_SEL: usize = 0x024;
const REG_GUEST_PAGE_SIZE: usize = 0x028; // Legacy: sets guest page size for PFN calc
const REG_QUEUE_SEL: usize     = 0x030;
const REG_QUEUE_NUM_MAX: usize = 0x034;
const REG_QUEUE_NUM: usize     = 0x038;
const REG_QUEUE_ALIGN: usize   = 0x03C;
const REG_QUEUE_PFN: usize     = 0x040;
const REG_QUEUE_NOTIFY: usize  = 0x050;
const REG_INTERRUPT_STATUS: usize = 0x060;
const REG_INTERRUPT_ACK: usize    = 0x064;
const REG_STATUS: usize        = 0x070;

// ── Device Status Bits ──

const STATUS_ACK: u32        = 1;
const STATUS_DRIVER: u32     = 2;
const STATUS_DRIVER_OK: u32  = 4;

// ── Queue Configuration ──

const QUEUE_SIZE: usize  = 16;
const QUEUE_ALIGN: usize = 4096;

// Vring offsets (derived from QUEUE_SIZE and QUEUE_ALIGN):
//   desc:  0
//   avail: QUEUE_SIZE * 16 = 256
//   used:  align_up(256 + 4 + 2*QUEUE_SIZE + 2, 4096) = align_up(294, 4096) = 4096
const DESC_OFFSET: usize  = 0;
const AVAIL_OFFSET: usize = QUEUE_SIZE * 16;  // 256
const USED_OFFSET: usize  = QUEUE_ALIGN;      // 4096

// ── Descriptor Flags ──

const VRING_DESC_F_NEXT: u16  = 1;
const VRING_DESC_F_WRITE: u16 = 2;

// ── Block Request Types ──

const VIRTIO_BLK_T_IN: u32  = 0; // Read
const VIRTIO_BLK_T_OUT: u32 = 1; // Write

const VIRTIO_MAGIC: u32 = 0x74726976; // "virt"
const VIRTIO_DEVICE_BLK: u32 = 2;

// ── Structures ──

/// Virtqueue descriptor (16 bytes, matches spec).
#[repr(C)]
struct VirtqDesc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

/// Block device request header (16 bytes).
#[repr(C)]
struct VirtIOBlockReq {
    type_: u32,
    reserved: u32,
    sector: u64,
}

// ── Driver State ──

static mut BLK_BASE: usize = 0;       // MMIO base address of block device
static mut VRING_ADDR: usize = 0;     // Physical address of vring memory
static mut REQ_PAGE: usize = 0;       // Page for request header + status byte
static mut LAST_USED_IDX: u16 = 0;
static mut AVAIL_IDX: u16 = 0;

// ── Helpers ──

/// Read a 32-bit MMIO register.
#[inline(always)]
unsafe fn mmio_read(base: usize, offset: usize) -> u32 {
    let val: u32;
    core::arch::asm!("ldr {:w}, [{}]", out(reg) val, in(reg) base + offset, options(nostack, preserves_flags, readonly));
    val
}

/// Write a 32-bit MMIO register.
#[inline(always)]
unsafe fn mmio_write(base: usize, offset: usize, val: u32) {
    core::arch::asm!("str {:w}, [{}]", in(reg) val, in(reg) base + offset, options(nostack, preserves_flags));
}

// ── Public API ──

/// Initialize the VirtIO block device. Scans MMIO transports for a block
/// device, negotiates features, and sets up the virtqueue.
pub unsafe fn init() {
    let uart = crate::drivers::uart::Uart::new();

    // Scan for block device.
    //
    // QEMU 10.x assigns VirtIO MMIO slots top-down: the first `-device
    // virtio-blk-device` on the command line lands at slot 31 (highest),
    // the second at slot 30, etc.  Scanning low-to-high would find the
    // ISO (second device, slot 30, F_RO=1) before drive.img (first device,
    // slot 31, F_RO=0).  Scan high-to-low so we find drive.img first.
    let mut base: usize = 0;
    let mut found = false;
    let mut i: usize = 32;
    while i > 0 {
        i -= 1;
        let b = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;
        let magic = mmio_read(b, REG_MAGIC);
        let dev_id = mmio_read(b, REG_DEVICE_ID);
        if magic == VIRTIO_MAGIC && dev_id == VIRTIO_DEVICE_BLK {
            base = b;
            found = true;
            break;
        }
    }

    if !found {
        uart.puts("[virtio] Block device not found\r\n");
        return;
    }

    BLK_BASE = base;

    // 1. Reset
    mmio_write(base, REG_STATUS, 0);

    // 2. Acknowledge
    let mut status = STATUS_ACK;
    mmio_write(base, REG_STATUS, status);

    // 3. Driver
    status |= STATUS_DRIVER;
    mmio_write(base, REG_STATUS, status);

    // 4. Negotiate features — reject INDIRECT_DESC (28) and EVENT_IDX (29)
    mmio_write(base, REG_HOST_FEATURES_SEL, 0);
    let features = mmio_read(base, REG_HOST_FEATURES);
    let accept = features & !(1 << 28) & !(1 << 29);
    mmio_write(base, REG_GUEST_FEATURES_SEL, 0);
    mmio_write(base, REG_GUEST_FEATURES, accept);

    // 5. Set guest page size (legacy requirement)
    // Without this, QEMU's guest_page_shift defaults to 0 and
    // QueuePFN << 0 = PFN, not the actual physical address.
    mmio_write(base, REG_GUEST_PAGE_SIZE, 4096);

    // 6. Setup queue 0
    mmio_write(base, REG_QUEUE_SEL, 0);

    let qmax = mmio_read(base, REG_QUEUE_NUM_MAX);
    if (qmax as usize) < QUEUE_SIZE {
        uart.puts("[virtio] Queue too small\r\n");
        return;
    }

    mmio_write(base, REG_QUEUE_NUM, QUEUE_SIZE as u32);

    // Allocate vring memory: 2 contiguous pages
    //   Page 0: descriptors + avail ring
    //   Page 1: used ring
    let p0 = memory::alloc_page().expect("virtio: alloc failed");
    let p1 = memory::alloc_page().expect("virtio: alloc failed");
    if p1 != p0 + 4096 {
        uart.puts("[virtio] ERROR: vring pages not contiguous\r\n");
        return;
    }

    // Zero the vring (use VA — alloc_page returns PA; page.rs already zeroes
    // on alloc via phys_to_virt, but be explicit here for the vring).
    let ptr = crate::memory::vmm::phys_to_virt(p0) as *mut u8;
    for i in 0..8192 {
        write_volatile(ptr.add(i), 0);
    }

    VRING_ADDR = p0;  // stored as PA

    // Allocate a page for request header + status byte
    REQ_PAGE = memory::alloc_page().expect("virtio: req page alloc failed");
    // REQ_PAGE stored as PA; already zeroed by alloc_page via phys_to_virt.

    // Tell device: alignment and page frame number
    mmio_write(base, REG_QUEUE_ALIGN, QUEUE_ALIGN as u32);
    mmio_write(base, REG_QUEUE_PFN, (p0 >> 12) as u32);

    // 7. Driver OK
    status |= STATUS_DRIVER_OK;
    mmio_write(base, REG_STATUS, status);

    LAST_USED_IDX = 0;
    AVAIL_IDX = 0;

    uart.puts("[virtio] Block device ready (vring=");
    uart.put_hex(p0);
    uart.puts(")\r\n");
}

const IO_BUFFER_SIZE: usize = 1048576; // 1MB
static mut IO_BUFFER: [u8; IO_BUFFER_SIZE] = [0; IO_BUFFER_SIZE];

/// Read multiple sectors into a buffer.
/// Handles chunking via internal physically contiguous buffer.
pub unsafe fn read_sectors(start_sector: u64, buf: &mut [u8]) {
    // let uart = crate::drivers::uart::Uart::new();
    // if buf.len() > 512 {
    //      uart.puts("[virtio] read_sectors len=");
    //      uart.put_dec(buf.len());
    //      uart.puts("\r\n");
    // }
    let mut bytes_left = buf.len();
    let mut buf_offset = 0;
    let mut sector = start_sector;

    while bytes_left >= 512 {
        let chunk_size = core::cmp::min(bytes_left, IO_BUFFER_SIZE);
        // Ensure multiple of 512
        let aligned_size = (chunk_size / 512) * 512;
        
        blk_op(sector, IO_BUFFER.as_mut_ptr(), aligned_size as u32, false);
        
        // Copy to user buffer
        core::ptr::copy_nonoverlapping(
            IO_BUFFER.as_ptr(), 
            buf.as_mut_ptr().add(buf_offset), 
            aligned_size
        );

        bytes_left -= aligned_size;
        buf_offset += aligned_size;
        sector += (aligned_size as u64) / 512;
    }
}

/// Read a single 512-byte sector (legacy wrapper)
pub unsafe fn read_block(sector: u64, buf: &mut [u8]) {
    blk_op(sector, buf.as_mut_ptr(), 512, false);
}

/// Write a single 512-byte sector
pub unsafe fn write_block(sector: u64, buf: &[u8]) {
    blk_op(sector, buf.as_ptr() as *mut u8, 512, true);
}

/// Perform a block I/O operation.
unsafe fn blk_op(sector: u64, buf: *mut u8, len: u32, write: bool) {
    if BLK_BASE == 0 {
        panic!("virtio: not initialized");
    }

    // VRING_ADDR is stored as PA; the CPU must access it via its high-VA alias.
    let vring_va = crate::memory::vmm::phys_to_virt(VRING_ADDR);

    // REQ_PAGE is stored as PA; CPU access via high-VA alias.
    let req_va = crate::memory::vmm::phys_to_virt(REQ_PAGE);

    // ── Request header (in REQ_PAGE at offset 0) ──
    let header = req_va as *mut VirtIOBlockReq;
    (*header).type_ = if write { VIRTIO_BLK_T_OUT } else { VIRTIO_BLK_T_IN };
    (*header).reserved = 0;
    (*header).sector = sector;

    // ── Status byte (in REQ_PAGE at offset 64) ──
    let status_ptr = (req_va + 64) as *mut u8;
    write_volatile(status_ptr, 0xFF);

    // ── Build descriptor chain ──
    // Descriptor table is at the start of the vring (CPU accesses via VA).
    let desc_base = (vring_va + DESC_OFFSET) as *mut VirtqDesc;

    // Desc 0: request header — DMA addr is the PA of REQ_PAGE
    let d0 = &mut *desc_base.add(0);
    d0.addr = REQ_PAGE as u64;
    d0.len = 16;
    d0.flags = VRING_DESC_F_NEXT;
    d0.next = 1;

    // Desc 1: data buffer — to_dma_addr handles both PA and high-VA pointers
    let d1 = &mut *desc_base.add(1);
    d1.addr = crate::memory::vmm::to_dma_addr(buf as usize) as u64;
    d1.len = len;
    d1.flags = VRING_DESC_F_NEXT | if write { 0 } else { VRING_DESC_F_WRITE };
    d1.next = 2;

    // Desc 2: status byte — DMA addr is PA (REQ_PAGE + 64)
    let d2 = &mut *desc_base.add(2);
    d2.addr = (REQ_PAGE + 64) as u64;
    d2.len = 1;
    d2.flags = VRING_DESC_F_WRITE;
    d2.next = 0;

    // ── Update avail ring ──
    let avail_base = (vring_va + AVAIL_OFFSET) as *mut u16;
    let avail_idx_ptr = avail_base.add(1);
    let avail_ring = avail_base.add(2);

    let ring_slot = AVAIL_IDX as usize % QUEUE_SIZE;
    write_volatile(avail_ring.add(ring_slot), 0); // Head descriptor index = 0

    core::arch::asm!("dmb sy");
    AVAIL_IDX = AVAIL_IDX.wrapping_add(1);
    write_volatile(avail_idx_ptr, AVAIL_IDX);
    core::arch::asm!("dmb sy");

    // ── Notify device ──
    mmio_write(BLK_BASE, REG_QUEUE_NOTIFY, 0);

    // ── Poll used ring ──
    let used_idx_ptr = (vring_va + USED_OFFSET + 2) as *const u16;
    let expected = LAST_USED_IDX.wrapping_add(1);

    let mut spin: u32 = 0;
    loop {
        core::arch::asm!("dmb sy");
        if read_volatile(used_idx_ptr) == expected {
            break;
        }
        spin += 1;
        if spin == 5_000_000 {
            let uart = crate::drivers::uart::Uart::new();
            uart.puts("[virtio] POLL STUCK: avail=");
            uart.put_dec(AVAIL_IDX as usize);
            uart.puts(" last=");
            uart.put_dec(LAST_USED_IDX as usize);
            uart.puts(" exp=");
            uart.put_dec(expected as usize);
            uart.puts(" got=");
            uart.put_dec(read_volatile(used_idx_ptr) as usize);
            uart.puts(" sec=");
            uart.put_dec(sector as usize);
            uart.puts("\r\n");
        }
        core::hint::spin_loop();
    }

    LAST_USED_IDX = expected;

    // ── Check status ──
    core::arch::asm!("dmb sy");
    let io_status = read_volatile(status_ptr);
    if io_status != 0 {
         let uart = crate::drivers::uart::Uart::new();
         uart.puts("[virtio] I/O error\r\n");
    }
}
