//! VirtIO GPU Driver (Legacy MMIO Interface)
//!
//! QEMU virt machine scans VirtIO MMIO slots for device ID 16.
//! Uses legacy (v1) queue interface.
//!
//! Exposes a simple `fb_flush()` to push a framebuffer to the display.

use core::ptr::{read_volatile, write_volatile};

// ── MMIO Transport ────────────────────────────────────────────────────────────

const VIRTIO_MMIO_BASE:   usize = 0x0A00_0000 + crate::memory::vmm::KERNEL_VA_OFFSET;
const VIRTIO_MMIO_STRIDE: usize = 0x200;

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

const STATUS_ACK:       u32 = 1;
const STATUS_DRIVER:    u32 = 2;
const STATUS_DRIVER_OK: u32 = 4;

const VIRTIO_MAGIC:      u32 = 0x74726976;
const VIRTIO_DEVICE_GPU: u32 = 16;

// ── Queue Configuration ───────────────────────────────────────────────────────

const QUEUE_SIZE:  usize = 16;
const QUEUE_ALIGN: usize = 4096;

const DESC_OFFSET:  usize = 0;
const AVAIL_OFFSET: usize = QUEUE_SIZE * 16;
const USED_OFFSET:  usize = QUEUE_ALIGN;

const VRING_DESC_F_NEXT:  u16 = 1;
const VRING_DESC_F_WRITE: u16 = 2;

// ── VirtIO GPU Constants ──────────────────────────────────────────────────────

const VIRTIO_GPU_F_VIRGL: u32 = 1 << 0;
const VIRTIO_GPU_F_EDID:  u32 = 1 << 1;

// Commands
const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32 = 0x0102;
const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const VIRTIO_GPU_CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
const VIRTIO_GPU_CMD_GET_CAPSET_INFO: u32 = 0x0108;
const VIRTIO_GPU_CMD_GET_CAPSET: u32 = 0x0109;
const VIRTIO_GPU_CMD_GET_EDID: u32 = 0x010a;

// Responses (Device -> Guest)
const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const VIRTIO_GPU_RESP_OK_CAPSET_INFO: u32 = 0x1102;
const VIRTIO_GPU_RESP_OK_CAPSET: u32 = 0x1103;
const VIRTIO_GPU_RESP_OK_EDID: u32 = 0x1104;
const VIRTIO_GPU_RESP_ERR_UNSPEC: u32 = 0x1200;

// Formats
const VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM: u32 = 1;
const VIRTIO_GPU_FORMAT_B8G8R8X8_UNORM: u32 = 2;
const VIRTIO_GPU_FORMAT_A8R8G8B8_UNORM: u32 = 3;
const VIRTIO_GPU_FORMAT_X8R8G8B8_UNORM: u32 = 4;
const VIRTIO_GPU_FORMAT_R8G8B8A8_UNORM: u32 = 67;
const VIRTIO_GPU_FORMAT_X8B8G8R8_UNORM: u32 = 68;

const VIRTIO_GPU_FORMAT_R8G8B8X8_UNORM: u32 = 134;

// ── Structures ────────────────────────────────────────────────────────────────

#[repr(C)]
struct VirtqDesc {
    addr:  u64,
    len:   u32,
    flags: u16,
    next:  u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuCtrlHdr {
    pub type_: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuDisplayOne {
    pub r: VirtIOGpuRect,
    pub enabled: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuRespDisplayInfo {
    pub hdr: VirtIOGpuCtrlHdr,
    pub pmodes: [VirtIOGpuDisplayOne; 16],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuResourceCreate2D {
    pub hdr: VirtIOGpuCtrlHdr,
    pub resource_id: u32,
    pub format: u32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuResourceAttachBacking {
    pub hdr: VirtIOGpuCtrlHdr,
    pub resource_id: u32,
    pub nr_entries: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuMemEntry {
    pub addr: u64,
    pub length: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuSetScanout {
    pub hdr: VirtIOGpuCtrlHdr,
    pub r: VirtIOGpuRect,
    pub scanout_id: u32,
    pub resource_id: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuTransferToHost2D {
    pub hdr: VirtIOGpuCtrlHdr,
    pub r: VirtIOGpuRect,
    pub offset: u64,
    pub resource_id: u32,
    pub padding: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtIOGpuResourceFlush {
    pub hdr: VirtIOGpuCtrlHdr,
    pub r: VirtIOGpuRect,
    pub resource_id: u32,
    pub padding: u32,
}

// ── Driver State ──────────────────────────────────────────────────────────────

static mut GPU_BASE: usize = 0;

static mut CTRL_VRING: usize = 0;
// Note: we don't currently use the cursor queue
static mut CURSOR_VRING: usize = 0;

static mut CTRL_AVAIL_IDX: u16 = 0;
static mut CTRL_LAST_USED: u16 = 0;

// Internal Frame Buffer properties
pub static mut FB_WIDTH: u32 = 1280;
pub static mut FB_HEIGHT: u32 = 720;
pub static mut FB_PITCH: u32 = 0;   // In bytes
pub static mut FB_FORMAT: u32 = VIRTIO_GPU_FORMAT_B8G8R8A8_UNORM;

static mut RESOURCE_ID: u32 = 1;

// The framebuffer backed by continuous pages
static mut FB_ADDR: usize = 0; // PA of the framebuffer allocation

// A page for allocating request/response structures
static mut REQ_PAGE: usize = 0; // PA

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

// ── Internal Helpers ──────────────────────────────────────────────────────────

unsafe fn setup_queue(base: usize, queue_idx: u32) -> usize {
    mmio_write(base, REG_QUEUE_SEL, queue_idx);
    mmio_write(base, REG_QUEUE_NUM, QUEUE_SIZE as u32);
    mmio_write(base, REG_QUEUE_ALIGN, QUEUE_ALIGN as u32);

    let p0 = crate::memory::alloc_page().expect("virtio_gpu: vring p0");
    let p1 = crate::memory::alloc_page().expect("virtio_gpu: vring p1");
    assert!(p1 == p0 + 4096, "virtio_gpu: vring pages not contiguous");

    core::ptr::write_bytes(crate::memory::vmm::phys_to_virt(p0) as *mut u8, 0, 8192);
    mmio_write(base, REG_QUEUE_PFN, (p0 >> 12) as u32);
    p0
}

unsafe fn alloc_req_page() -> usize {
    let pa = crate::memory::alloc_page().expect("virtio_gpu: req alloc page failed");
    core::ptr::write_bytes(crate::memory::vmm::phys_to_virt(pa) as *mut u8, 0, 4096);
    pa
}

// Low level queue command (assumes standard 1 req desc + 1 resp desc, both pre-allocated)
// The desc chain must already be configured properly. This just advances the avail ring
// and waits for the used ring.
unsafe fn submit_and_wait() {
    let vring_va = crate::memory::vmm::phys_to_virt(CTRL_VRING);
    let avail = (vring_va + AVAIL_OFFSET) as *mut u16;
    let ring_slot = CTRL_AVAIL_IDX as usize % QUEUE_SIZE;

    // We always use descriptor 0 as the head of the chain.
    write_volatile(avail.add(2 + ring_slot), 0u16);

    core::arch::asm!("dmb sy");
    CTRL_AVAIL_IDX = CTRL_AVAIL_IDX.wrapping_add(1);
    write_volatile(avail.add(1), CTRL_AVAIL_IDX);
    core::arch::asm!("dmb sy");

    mmio_write(GPU_BASE, REG_QUEUE_NOTIFY, 0); // queue 0

    let used_idx_ptr = (vring_va + USED_OFFSET + 2) as *const u16;
    let expected = CTRL_LAST_USED.wrapping_add(1);
    
    let mut spin = 0;
    loop {
        core::arch::asm!("dmb sy");
        if read_volatile(used_idx_ptr) == expected { break; }
        core::hint::spin_loop();
        spin += 1;
        if spin > 10_000_000 {
            crate::drivers::uart::Uart::new().puts("[virtio-gpu] ERROR: stuck waiting for queue used index\r\n");
            break; // Better to break than hang forever on a display frame
        }
    }
    CTRL_LAST_USED = expected;
}

// Executes a standard command that has a Req structure and uses the standard Hdr-only Response structure
unsafe fn exec_cmd_simple<T>(req: *const T, req_size: usize) -> u32 {
    let req_pa = REQ_PAGE;
    let req_va = crate::memory::vmm::phys_to_virt(req_pa);
    
    // Copy the request to the start of the REQ_PAGE
    core::ptr::copy_nonoverlapping(req as *const u8, req_va as *mut u8, req_size);

    // The response goes after the request (let's say offset 2048)
    let resp_offset = 2048;
    let resp_pa = req_pa + resp_offset as usize;
    let resp_va = req_va + resp_offset as usize;

    let vring_va = crate::memory::vmm::phys_to_virt(CTRL_VRING);
    let desc_base = (vring_va + DESC_OFFSET) as *mut VirtqDesc;

    // Desc 0: Request (read by device)
    let d0 = &mut *desc_base.add(0);
    d0.addr = req_pa as u64;
    d0.len = req_size as u32;
    d0.flags = VRING_DESC_F_NEXT;
    d0.next = 1;

    // Desc 1: Response (written by device)
    let d1 = &mut *desc_base.add(1);
    d1.addr = resp_pa as u64;
    d1.len = core::mem::size_of::<VirtIOGpuCtrlHdr>() as u32; // basic response is just the header
    d1.flags = VRING_DESC_F_WRITE;
    d1.next = 0;

    submit_and_wait();

    let resp_hdr = resp_va as *const VirtIOGpuCtrlHdr;
    (*resp_hdr).type_
}

// ── Public API ────────────────────────────────────────────────────────────────

pub unsafe fn init() {
    let uart = crate::drivers::uart::Uart::new();

    // 1. Scan for VirtIO GPU device
    let mut base = 0usize;
    let mut found = false;
    for i in 0..32 {
        let b = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;
        if mmio_read(b, REG_MAGIC) == VIRTIO_MAGIC && mmio_read(b, REG_DEVICE_ID) == VIRTIO_DEVICE_GPU {
            base = b;
            found = true;
            break;
        }
    }

    if !found {
        uart.puts("[gpu]  VirtIO-gpu not found\r\n");
        return;
    }

    GPU_BASE = base;

    // 2. Initialize device
    mmio_write(base, REG_STATUS, 0);
    let mut status = STATUS_ACK | STATUS_DRIVER;
    mmio_write(base, REG_STATUS, status);

    let host_feat = mmio_read(base, REG_HOST_FEATURES);
    // Ignore VIRGL/EDID for now
    let accept = host_feat & !VIRTIO_GPU_F_VIRGL & !VIRTIO_GPU_F_EDID;
    mmio_write(base, REG_GUEST_FEATURES, accept);
    
    mmio_write(base, REG_GUEST_PAGE_SIZE, 4096);

    CTRL_VRING = setup_queue(base, 0);
    CURSOR_VRING = setup_queue(base, 1);

    status |= STATUS_DRIVER_OK;
    mmio_write(base, REG_STATUS, status);

    REQ_PAGE = alloc_req_page();

    uart.puts("[gpu]  VirtIO-gpu initialized.\r\n");

    // Let's hardcode 1280x720, 32bpp (4 bytes per pixel)
    // You could query VIRTIO_GPU_CMD_GET_DISPLAY_INFO here to get best dimensions
    FB_WIDTH = 1280;
    FB_HEIGHT = 720;
    FB_PITCH = FB_WIDTH * 4;

    let total_bytes = FB_PITCH * FB_HEIGHT;
    // Round to page
    let pages_needed = (total_bytes as usize + 4095) / 4096;
    
    // Allocate physically contiguous pages for the FB
    FB_ADDR = crate::memory::alloc_pages(pages_needed).expect("virtio_gpu: failed to allocate FB memory");

    uart.puts("[gpu]  Framebuffer allocated: ");
    uart.put_dec(FB_WIDTH as usize);
    uart.puts("x");
    uart.put_dec(FB_HEIGHT as usize);
    uart.puts(" (");
    uart.put_dec(total_bytes as usize);
    uart.puts(" bytes) at PA ");
    uart.put_hex(FB_ADDR);
    uart.puts("\r\n");

    // Clear FB memory (white or black to test)
    let fb_va = crate::memory::vmm::phys_to_virt(FB_ADDR) as *mut u32;
    for i in 0..(FB_WIDTH * FB_HEIGHT) as usize {
        *fb_va.add(i) = 0xFF222222; // dark grey
    }

    // Command: RESOURCE_CREATE_2D
    let mut req_create = VirtIOGpuResourceCreate2D {
        hdr: VirtIOGpuCtrlHdr {
            type_: VIRTIO_GPU_CMD_RESOURCE_CREATE_2D,
            flags: 0, fence_id: 0, ctx_id: 0, padding: 0
        },
        resource_id: RESOURCE_ID,
        format: FB_FORMAT,
        width: FB_WIDTH,
        height: FB_HEIGHT,
    };
    let resp = exec_cmd_simple(&req_create, core::mem::size_of::<VirtIOGpuResourceCreate2D>());
    if resp != VIRTIO_GPU_RESP_OK_NODATA {
         uart.puts("[gpu]  ERROR: RESOURCE_CREATE_2D failed\r\n");
    }

    // Command: RESOURCE_ATTACH_BACKING
    // We construct a specific memory layout because it has a trailing array
    let attach_hdr = VirtIOGpuResourceAttachBacking {
        hdr: VirtIOGpuCtrlHdr {
            type_: VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING,
            flags: 0, fence_id: 0, ctx_id: 0, padding: 0
        },
        resource_id: RESOURCE_ID,
        nr_entries: 1,
    };

    let mem_entry = VirtIOGpuMemEntry {
        addr: FB_ADDR as u64,
        length: total_bytes,
        padding: 0,
    };

    let req_pa = REQ_PAGE;
    let req_va = crate::memory::vmm::phys_to_virt(req_pa);
    core::ptr::copy_nonoverlapping(&attach_hdr as *const _ as *const u8, req_va as *mut u8, core::mem::size_of::<VirtIOGpuResourceAttachBacking>());
    let arr_va = req_va + core::mem::size_of::<VirtIOGpuResourceAttachBacking>();
    core::ptr::copy_nonoverlapping(&mem_entry as *const _ as *const u8, arr_va as *mut u8, core::mem::size_of::<VirtIOGpuMemEntry>());

    let req_size = core::mem::size_of::<VirtIOGpuResourceAttachBacking>() + core::mem::size_of::<VirtIOGpuMemEntry>();

    // Using raw desc table logic since it doesn't map directly to exec_cmd_simple size wrapper
    let resp_offset = 2048;
    let resp_pa = req_pa + resp_offset;
    let resp_va = req_va + resp_offset;

    let vring_va = crate::memory::vmm::phys_to_virt(CTRL_VRING);
    let desc_base = (vring_va + DESC_OFFSET) as *mut VirtqDesc;

    let d0 = &mut *desc_base.add(0);
    d0.addr = req_pa as u64; d0.len = req_size as u32; d0.flags = VRING_DESC_F_NEXT; d0.next = 1;
    let d1 = &mut *desc_base.add(1);
    d1.addr = resp_pa as u64; d1.len = core::mem::size_of::<VirtIOGpuCtrlHdr>() as u32; d1.flags = VRING_DESC_F_WRITE; d1.next = 0;

    submit_and_wait();

    let resp_hdr = resp_va as *const VirtIOGpuCtrlHdr;
    if (*resp_hdr).type_ != VIRTIO_GPU_RESP_OK_NODATA {
        uart.puts("[gpu]  ERROR: RESOURCE_ATTACH_BACKING failed\r\n");
    }

    // Command: SET_SCANOUT
    let req_scanout = VirtIOGpuSetScanout {
        hdr: VirtIOGpuCtrlHdr {
            type_: VIRTIO_GPU_CMD_SET_SCANOUT,
            flags: 0, fence_id: 0, ctx_id: 0, padding: 0
        },
        r: VirtIOGpuRect { x: 0, y: 0, width: FB_WIDTH, height: FB_HEIGHT },
        scanout_id: 0,
        resource_id: RESOURCE_ID,
    };
    let resp = exec_cmd_simple(&req_scanout, core::mem::size_of::<VirtIOGpuSetScanout>());
    if resp != VIRTIO_GPU_RESP_OK_NODATA {
         uart.puts("[gpu]  ERROR: SET_SCANOUT failed\r\n");
    }

    uart.puts("[gpu]  Init complete.\r\n");
    
    // Do an initial flush
    flush();
}

pub fn get_framebuffer() -> (*mut u32, u32, u32, u32) {
    // Return null if no GPU was found (GPU_BASE == 0) or FB not allocated.
    if unsafe { GPU_BASE == 0 || FB_ADDR == 0 } {
        return (core::ptr::null_mut(), 0, 0, 0);
    }
    let fb_va = crate::memory::vmm::phys_to_virt(unsafe { FB_ADDR }) as *mut u32;
    unsafe { (fb_va, FB_WIDTH, FB_HEIGHT, FB_PITCH) }
}

/// Flush the entire framebuffer to the display
pub unsafe fn flush() {
    if GPU_BASE == 0 { return; }

    // 1. TRANSFER_TO_HOST_2D
    let req_transfer = VirtIOGpuTransferToHost2D {
        hdr: VirtIOGpuCtrlHdr {
            type_: VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D,
            flags: 0, fence_id: 0, ctx_id: 0, padding: 0
        },
        r: VirtIOGpuRect { x: 0, y: 0, width: FB_WIDTH, height: FB_HEIGHT },
        offset: 0,
        resource_id: RESOURCE_ID,
        padding: 0,
    };
    exec_cmd_simple(&req_transfer, core::mem::size_of::<VirtIOGpuTransferToHost2D>());

    // 2. RESOURCE_FLUSH
    let req_flush = VirtIOGpuResourceFlush {
        hdr: VirtIOGpuCtrlHdr {
            type_: VIRTIO_GPU_CMD_RESOURCE_FLUSH,
            flags: 0, fence_id: 0, ctx_id: 0, padding: 0
        },
        r: VirtIOGpuRect { x: 0, y: 0, width: FB_WIDTH, height: FB_HEIGHT },
        resource_id: RESOURCE_ID,
        padding: 0,
    };
    exec_cmd_simple(&req_flush, core::mem::size_of::<VirtIOGpuResourceFlush>());
}
