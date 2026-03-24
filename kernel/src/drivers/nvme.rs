/// NVMe (Non-Volatile Memory Express) PCIe block driver.
///
/// Implements a single-namespace, polled (no MSI/interrupt) NVMe client
/// suitable for both Apple Silicon ANS2 (vendor 0x106B) and QEMU nvme
/// (vendor 0x1B36, device 0x0010).
///
/// Architecture:
///   • One Admin queue pair (SQ + CQ, each 64 entries of 64/16 bytes).
///   • One I/O queue pair (SQ + CQ, same depth).
///   • All I/O is polled — the driver spins on the CQ phase bit.
///   • Block size = 512 bytes (sector), max transfer = 127 sectors (64 KB).
///
/// Usage (from kernel_main after PCIe enumeration):
///   nvme::init(bar0_phys);        // configure controller
///   nvme::read(lba, buf);         // 512-byte sector read
///   nvme::write(lba, buf);        // 512-byte sector write

extern crate alloc;
use alloc::boxed::Box;
use crate::memory::vmm::KERNEL_VA_OFFSET;
use crate::arch::aarch64::exceptions;

// ── NVMe controller register offsets ─────────────────────────────────────────

const NVME_CAP:  usize = 0x00; // Controller Capabilities (8 bytes)
const NVME_VS:   usize = 0x08; // Version (4 bytes)
const NVME_CC:   usize = 0x14; // Controller Configuration (4 bytes)
const NVME_CSTS: usize = 0x1C; // Controller Status (4 bytes)
const NVME_AQA:  usize = 0x24; // Admin Queue Attributes (4 bytes)
const NVME_ASQ:  usize = 0x28; // Admin SQ Base Address (8 bytes)
const NVME_ACQ:  usize = 0x30; // Admin CQ Base Address (8 bytes)

// Doorbell base: 0x1000 + 2*q*stride for SQ tail, 2*q*stride+stride for CQ head.
const NVME_DB_BASE: usize = 0x1000;

// CC fields
const CC_EN:   u32 = 1 << 0;   // Enable
const CC_IOSQES: u32 = 6 << 16; // I/O SQ entry size = 64 bytes (2^6)
const CC_IOCQES: u32 = 4 << 20; // I/O CQ entry size = 16 bytes (2^4)
const CC_MPS:  u32 = 0 << 7;   // Memory page size = 4 KB (2^(12+0))
const CC_CSS:  u32 = 0 << 4;   // NVM command set

// CSTS fields
const CSTS_RDY: u32 = 1 << 0;
const CSTS_CFS: u32 = 1 << 1;

// ── Queue geometry ────────────────────────────────────────────────────────────

const AQ_DEPTH: usize = 64;   // Admin queue depth
const IOQ_DEPTH: usize = 64;  // I/O queue depth

// Submission queue entry: 64 bytes
const SQE_SIZE: usize = 64;
// Completion queue entry: 16 bytes
const CQE_SIZE: usize = 16;

// ── Apple NVMe PCIe IDs ───────────────────────────────────────────────────────

pub const APPLE_NVME_VENDOR: u16  = 0x106B;
pub const APPLE_NVME_DEVICE_M1:  u16 = 0x2003;
pub const APPLE_NVME_DEVICE_M1P: u16 = 0x2005; // M1 Pro/Max
pub const APPLE_NVME_DEVICE_M2:  u16 = 0x200D;
pub const APPLE_NVME_DEVICE_M4:  u16 = 0x2019; // best-known at time of writing

/// QEMU nvme emulated device
pub const QEMU_NVME_VENDOR: u16 = 0x1B36;
pub const QEMU_NVME_DEVICE: u16 = 0x0010;

/// Match any Apple NVMe ANS device
pub fn is_apple_nvme(vendor: u16, device: u16) -> bool {
    vendor == APPLE_NVME_VENDOR
        && matches!(device,
                    APPLE_NVME_DEVICE_M1 | APPLE_NVME_DEVICE_M1P |
                    APPLE_NVME_DEVICE_M2 | APPLE_NVME_DEVICE_M4)
}

// ── Controller state ──────────────────────────────────────────────────────────

struct NvmeCtrl {
    base_va:  usize,  // BAR0 virtual address
    db_stride: usize, // doorbell stride in bytes (from CAP.DSTRD)

    // Admin queues (physical addresses stored for device)
    asq_pa:  u64,
    acq_pa:  u64,
    asq_va:  usize,
    acq_va:  usize,
    asq_tail: u16,
    acq_head: u16,
    acq_phase: u8,
    acid:    u16,     // next admin command ID

    // I/O queue 1 (physical + virtual)
    iosq_pa: u64,
    iocq_pa: u64,
    iosq_va: usize,
    iocq_va: usize,
    iosq_tail: u16,
    iocq_head: u16,
    iocq_phase: u8,
    ioid:    u16,     // next I/O command ID

    ns_lba_count: u64,
    ns_lba_size:  u32,
}

static mut CTRL: Option<NvmeCtrl> = None;

// ── MMIO helpers ──────────────────────────────────────────────────────────────

unsafe fn reg_read32(base: usize, off: usize) -> u32 {
    let p = (base + off) as *const u32;
    core::ptr::read_volatile(p)
}

unsafe fn reg_write32(base: usize, off: usize, val: u32) {
    let p = (base + off) as *mut u32;
    core::ptr::write_volatile(p, val);
}

unsafe fn reg_read64(base: usize, off: usize) -> u64 {
    let p = (base + off) as *const u64;
    core::ptr::read_volatile(p)
}

unsafe fn reg_write64(base: usize, off: usize, val: u64) {
    let p = (base + off) as *mut u64;
    core::ptr::write_volatile(p, val);
}

// ── Memory allocation helpers ─────────────────────────────────────────────────

/// Allocate a physically-contiguous zero-filled page for a queue.
/// Returns (virtual_addr, physical_addr).
fn alloc_queue(size_bytes: usize) -> (usize, u64) {
    let pages = (size_bytes + 4095) / 4096;
    let pa = crate::memory::alloc_pages(pages)
        .expect("[nvme] queue page alloc failed") as u64;
    let va = pa as usize + KERNEL_VA_OFFSET;
    // Zero the queue memory
    unsafe {
        core::ptr::write_bytes(va as *mut u8, 0, pages * 4096);
    }
    (va, pa)
}

// ── Submission / Completion helpers ──────────────────────────────────────────

/// Write a 64-byte SQE at the current SQ tail and ring the doorbell.
unsafe fn admin_submit(c: &mut NvmeCtrl, sqe: &[u8; 64]) {
    let slot = c.asq_tail as usize;
    let dst = (c.asq_va + slot * SQE_SIZE) as *mut u8;
    core::ptr::copy_nonoverlapping(sqe.as_ptr(), dst, 64);
    c.asq_tail = (c.asq_tail + 1) % AQ_DEPTH as u16;

    // Ring SQ0 tail doorbell (queue 0, stride offset 0)
    let db_off = NVME_DB_BASE + 0 * 2 * c.db_stride;
    reg_write32(c.base_va, db_off, c.asq_tail as u32);
}

/// Poll Admin CQ until a CQE with matching CID appears.  Returns status.
unsafe fn admin_poll(c: &mut NvmeCtrl, cid: u16) -> u16 {
    loop {
        let cqe_ptr = (c.acq_va + c.acq_head as usize * CQE_SIZE) as *const u16;
        // CQE word 7 (bytes 14-15): bits[15:1] = status, bit[0] = phase
        let status_phase = core::ptr::read_volatile(cqe_ptr.add(7));
        let phase = (status_phase & 1) as u8;
        if phase == c.acq_phase {
            // This CQE is valid
            let cqe_cid = core::ptr::read_volatile(cqe_ptr.add(6)); // bytes 12-13
            if cqe_cid == cid {
                let status = status_phase >> 1;
                c.acq_head = (c.acq_head + 1) % AQ_DEPTH as u16;
                if c.acq_head == 0 { c.acq_phase ^= 1; }
                // Ring CQ0 head doorbell
                let db_off = NVME_DB_BASE + 1 * c.db_stride;
                reg_write32(c.base_va, db_off, c.acq_head as u32);
                return status;
            }
        }
        core::hint::spin_loop();
    }
}

/// Submit one I/O SQE and poll for its completion.  Returns status.
unsafe fn io_submit_poll(c: &mut NvmeCtrl, sqe: &[u8; 64]) -> u16 {
    let slot = c.iosq_tail as usize;
    let dst = (c.iosq_va + slot * SQE_SIZE) as *mut u8;
    core::ptr::copy_nonoverlapping(sqe.as_ptr(), dst, 64);
    c.iosq_tail = (c.iosq_tail + 1) % IOQ_DEPTH as u16;

    // Ring SQ1 tail doorbell (queue 1)
    let db_off = NVME_DB_BASE + 2 * c.db_stride; // SQ1 doorbell
    reg_write32(c.base_va, db_off, c.iosq_tail as u32);

    let cid = c.ioid.wrapping_sub(1); // was incremented before call
    loop {
        let cqe_ptr = (c.iocq_va + c.iocq_head as usize * CQE_SIZE) as *const u16;
        let status_phase = core::ptr::read_volatile(cqe_ptr.add(7));
        let phase = (status_phase & 1) as u8;
        if phase == c.iocq_phase {
            let cqe_cid = core::ptr::read_volatile(cqe_ptr.add(6));
            if cqe_cid == cid {
                let status = status_phase >> 1;
                c.iocq_head = (c.iocq_head + 1) % IOQ_DEPTH as u16;
                if c.iocq_head == 0 { c.iocq_phase ^= 1; }
                // Ring CQ1 head doorbell
                let db_off = NVME_DB_BASE + 3 * c.db_stride; // CQ1 doorbell
                reg_write32(c.base_va, db_off, c.iocq_head as u32);
                return status;
            }
        }
        core::hint::spin_loop();
    }
}

// ── Build SQE helpers ─────────────────────────────────────────────────────────

fn build_identify(cid: u16, nsid: u32, prp1_pa: u64, cns: u32) -> [u8; 64] {
    let mut sqe = [0u8; 64];
    // DWORD0: OPC=0x06 (Identify), CID
    sqe[0] = 0x06;
    sqe[2] = (cid & 0xFF) as u8;
    sqe[3] = (cid >> 8)   as u8;
    // NSID
    sqe[4] = (nsid & 0xFF) as u8;
    sqe[5] = ((nsid >>  8) & 0xFF) as u8;
    sqe[6] = ((nsid >> 16) & 0xFF) as u8;
    sqe[7] = ((nsid >> 24) & 0xFF) as u8;
    // PRP1
    sqe[16..24].copy_from_slice(&prp1_pa.to_le_bytes());
    // CDW10: CNS (1=controller, 2=namespace)
    sqe[40] = (cns & 0xFF) as u8;
    sqe
}

fn build_create_cq(cid: u16, qid: u16, pa: u64, depth: u16) -> [u8; 64] {
    let mut sqe = [0u8; 64];
    sqe[0] = 0x05; // Create CQ
    sqe[2] = (cid & 0xFF) as u8; sqe[3] = (cid >> 8) as u8;
    sqe[16..24].copy_from_slice(&pa.to_le_bytes());
    // CDW10: QSIZE[31:16], QID[15:0]
    sqe[40] = (qid & 0xFF) as u8; sqe[41] = (qid >> 8) as u8;
    sqe[42] = ((depth - 1) & 0xFF) as u8; sqe[43] = ((depth - 1) >> 8) as u8;
    // CDW11: PC=1 (physically contiguous), IEN=0 (polling)
    sqe[44] = 1;
    sqe
}

fn build_create_sq(cid: u16, qid: u16, pa: u64, depth: u16, cqid: u16) -> [u8; 64] {
    let mut sqe = [0u8; 64];
    sqe[0] = 0x01; // Create SQ
    sqe[2] = (cid & 0xFF) as u8; sqe[3] = (cid >> 8) as u8;
    sqe[16..24].copy_from_slice(&pa.to_le_bytes());
    // CDW10: QSIZE[31:16], QID[15:0]
    sqe[40] = (qid & 0xFF) as u8; sqe[41] = (qid >> 8) as u8;
    sqe[42] = ((depth - 1) & 0xFF) as u8; sqe[43] = ((depth - 1) >> 8) as u8;
    // CDW11: PC=1, QPRIO=0, CQID[31:16]
    sqe[44] = 1;
    sqe[46] = (cqid & 0xFF) as u8; sqe[47] = (cqid >> 8) as u8;
    sqe
}

fn build_io_rw(cid: u16, opc: u8, nsid: u32, lba: u64, prp1_pa: u64, nlb: u16) -> [u8; 64] {
    let mut sqe = [0u8; 64];
    sqe[0] = opc;
    sqe[2] = (cid & 0xFF) as u8; sqe[3] = (cid >> 8) as u8;
    sqe[4] = (nsid & 0xFF) as u8; sqe[5] = (nsid >> 8) as u8;
    sqe[6] = (nsid >> 16) as u8;  sqe[7] = (nsid >> 24) as u8;
    sqe[16..24].copy_from_slice(&prp1_pa.to_le_bytes());
    sqe[40..48].copy_from_slice(&lba.to_le_bytes()); // CDW10-11: SLBA
    sqe[48] = (nlb & 0xFF) as u8; sqe[49] = (nlb >> 8) as u8; // CDW12: NLB
    sqe
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Initialise the NVMe controller at the given BAR0 physical address.
/// Returns true on success.
pub fn init(bar0_phys: usize) -> bool {
    let uart = crate::drivers::uart::Uart::new();
    let base_va = bar0_phys + KERNEL_VA_OFFSET;

    uart.puts("[nvme] init BAR0=");
    uart.put_hex(bar0_phys);
    uart.puts("\r\n");

    unsafe {
        // Read CAP to get doorbell stride and MQES
        let cap = reg_read64(base_va, NVME_CAP);
        let dstrd = ((cap >> 32) & 0xF) as usize;
        let db_stride = 4 << dstrd; // 4 bytes * 2^DSTRD
        let mqes = (cap & 0xFFFF) as usize + 1; // max queue entries

        let vs = reg_read32(base_va, NVME_VS);
        uart.puts("[nvme] VS=");
        uart.put_hex(vs as usize);
        uart.puts("  MQES=");
        uart.put_dec(mqes);
        uart.puts("  db_stride=");
        uart.put_dec(db_stride);
        uart.puts("\r\n");

        // 1. Disable controller
        reg_write32(base_va, NVME_CC, 0);
        // Wait for RDY=0
        let mut timeout = 50_000u32;
        while reg_read32(base_va, NVME_CSTS) & CSTS_RDY != 0 {
            core::hint::spin_loop();
            timeout -= 1;
            if timeout == 0 {
                uart.puts("[nvme] timeout waiting for disable\r\n");
                return false;
            }
        }

        // 2. Allocate Admin queues
        let (asq_va, asq_pa) = alloc_queue(AQ_DEPTH * SQE_SIZE);
        let (acq_va, acq_pa) = alloc_queue(AQ_DEPTH * CQE_SIZE);

        // 3. Configure Admin Queue Attributes and Base Addresses
        let aqa = ((AQ_DEPTH - 1) as u32) | (((AQ_DEPTH - 1) as u32) << 16);
        reg_write32(base_va, NVME_AQA, aqa);
        reg_write64(base_va, NVME_ASQ, asq_pa);
        reg_write64(base_va, NVME_ACQ, acq_pa);

        // 4. Enable controller
        let cc = CC_EN | CC_IOSQES | CC_IOCQES | CC_MPS | CC_CSS;
        reg_write32(base_va, NVME_CC, cc);
        timeout = 100_000;
        loop {
            let csts = reg_read32(base_va, NVME_CSTS);
            if csts & CSTS_CFS != 0 {
                uart.puts("[nvme] CFS=1, controller fatal\r\n");
                return false;
            }
            if csts & CSTS_RDY != 0 { break; }
            core::hint::spin_loop();
            timeout -= 1;
            if timeout == 0 {
                uart.puts("[nvme] timeout waiting for RDY\r\n");
                return false;
            }
        }
        uart.puts("[nvme] Controller ready\r\n");

        let mut ctrl = NvmeCtrl {
            base_va, db_stride,
            asq_pa, acq_pa, asq_va, acq_va,
            asq_tail: 0, acq_head: 0, acq_phase: 1, acid: 1,
            iosq_pa: 0, iocq_pa: 0, iosq_va: 0, iocq_va: 0,
            iosq_tail: 0, iocq_head: 0, iocq_phase: 1, ioid: 1,
            ns_lba_count: 0, ns_lba_size: 512,
        };

        // 5. Identify controller (admin cmd 0x06, CNS=1)
        let (id_va, id_pa) = alloc_queue(4096);
        let cid = ctrl.acid; ctrl.acid += 1;
        let sqe = build_identify(cid, 0, id_pa, 1);
        admin_submit(&mut ctrl, &sqe);
        let st = admin_poll(&mut ctrl, cid);
        if st != 0 {
            uart.puts("[nvme] Identify controller failed st=");
            uart.put_dec(st as usize);
            uart.puts("\r\n");
            return false;
        }
        // ID data: bytes 24..27 = MDTS (max data transfer in pages)
        let model = core::str::from_utf8(&*(id_va as *const [u8; 40]).add(1))
            .unwrap_or("(invalid)");
        uart.puts("[nvme] Model: ");
        // Print first 20 chars of model string (starts at byte 24 of identify)
        let model_bytes = &*(id_va as *const u8).add(24) as *const u8;
        for i in 0..20usize {
            let b = *model_bytes.add(i);
            if b == 0 || b == b' ' && i > 0 { break; }
            if b >= 0x20 && b < 0x7F { uart.putc(b); }
        }
        uart.puts("\r\n");
        let _ = model; // suppress warning

        // 6. Identify namespace 1
        let cid = ctrl.acid; ctrl.acid += 1;
        core::ptr::write_bytes(id_va as *mut u8, 0, 4096);
        let sqe = build_identify(cid, 1, id_pa, 2);
        admin_submit(&mut ctrl, &sqe);
        let st = admin_poll(&mut ctrl, cid);
        if st != 0 {
            uart.puts("[nvme] Identify namespace failed st=");
            uart.put_dec(st as usize);
            uart.puts("\r\n");
            return false;
        }
        // NSZE at bytes 0..8, LBADS at offset 128 in LBAF[0]
        ctrl.ns_lba_count = core::ptr::read_unaligned(id_va as *const u64);
        let lbads = *((id_va + 128) as *const u8); // LBAF[0].LBADS
        ctrl.ns_lba_size = 1u32 << lbads;
        uart.puts("[nvme] NS1: lba_count=");
        uart.put_dec(ctrl.ns_lba_count as usize);
        uart.puts("  lba_size=");
        uart.put_dec(ctrl.ns_lba_size as usize);
        uart.puts("\r\n");

        // 7. Create I/O CQ 1
        let (iocq_va, iocq_pa) = alloc_queue(IOQ_DEPTH * CQE_SIZE);
        let cid = ctrl.acid; ctrl.acid += 1;
        let sqe = build_create_cq(cid, 1, iocq_pa, IOQ_DEPTH as u16);
        admin_submit(&mut ctrl, &sqe);
        let st = admin_poll(&mut ctrl, cid);
        if st != 0 {
            uart.puts("[nvme] Create CQ failed\r\n");
            return false;
        }

        // 8. Create I/O SQ 1
        let (iosq_va, iosq_pa) = alloc_queue(IOQ_DEPTH * SQE_SIZE);
        let cid = ctrl.acid; ctrl.acid += 1;
        let sqe = build_create_sq(cid, 1, iosq_pa, IOQ_DEPTH as u16, 1);
        admin_submit(&mut ctrl, &sqe);
        let st = admin_poll(&mut ctrl, cid);
        if st != 0 {
            uart.puts("[nvme] Create SQ failed\r\n");
            return false;
        }

        ctrl.iocq_va = iocq_va; ctrl.iocq_pa = iocq_pa;
        ctrl.iosq_va = iosq_va; ctrl.iosq_pa = iosq_pa;

        uart.puts("[nvme] I/O queues ready\r\n");

        CTRL = Some(ctrl);
        true
    }
}

/// True if an NVMe controller has been initialised.
pub fn is_up() -> bool {
    unsafe { CTRL.is_some() }
}

/// Synchronous 512-byte sector read.
/// `lba` is the logical block address; `buf` must be exactly 512 bytes.
/// Returns true on success.
pub fn read_sector(lba: u64, buf: &mut [u8; 512]) -> bool {
    unsafe {
        let c = match CTRL.as_mut() { Some(c) => c, None => return false };

        exceptions::disable_irqs();

        // Use a single-page DMA buffer (physical alloc required for DMA)
        let pa = crate::memory::alloc_pages(1)
            .expect("[nvme] DMA page alloc failed") as u64;
        let va = pa as usize + KERNEL_VA_OFFSET;

        let cid = c.ioid; c.ioid = c.ioid.wrapping_add(1);
        let sqe = build_io_rw(cid, 0x02 /* Read */, 1, lba, pa, 0 /* NLB=1 sector */);
        let st = io_submit_poll(c, &sqe);

        if st == 0 {
            core::ptr::copy_nonoverlapping((va) as *const u8, buf.as_mut_ptr(), 512);
        }

        crate::memory::release_pages(pa as usize, 1);
        exceptions::enable_irqs();

        st == 0
    }
}

/// Synchronous 512-byte sector write.
pub fn write_sector(lba: u64, buf: &[u8; 512]) -> bool {
    unsafe {
        let c = match CTRL.as_mut() { Some(c) => c, None => return false };

        exceptions::disable_irqs();

        let pa = crate::memory::alloc_pages(1)
            .expect("[nvme] DMA page alloc failed") as u64;
        let va = pa as usize + KERNEL_VA_OFFSET;
        core::ptr::copy_nonoverlapping(buf.as_ptr(), va as *mut u8, 512);

        let cid = c.ioid; c.ioid = c.ioid.wrapping_add(1);
        let sqe = build_io_rw(cid, 0x01 /* Write */, 1, lba, pa, 0);
        let st = io_submit_poll(c, &sqe);

        crate::memory::release_pages(pa as usize, 1);
        exceptions::enable_irqs();

        st == 0
    }
}
