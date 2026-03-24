//! VirtIO Input Driver (Legacy MMIO Interface)
//!
//! Handles VirtIO input devices (keyboard, mouse, tablet).
//! Device ID 18.

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
const VIRTIO_DEVICE_INPUT: u32 = 18;

// ── Queue Configuration ───────────────────────────────────────────────────────

const QUEUE_SIZE:  usize = 16;
const QUEUE_ALIGN: usize = 4096;

const DESC_OFFSET:  usize = 0;
const AVAIL_OFFSET: usize = QUEUE_SIZE * 16;
const USED_OFFSET:  usize = QUEUE_ALIGN;

const VRING_DESC_F_WRITE: u16 = 2;

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
pub struct VirtIOInputEvent {
    pub type_: u16,
    pub code: u16,
    pub value: u32,
}

struct InputDevice {
    base: usize,
    vring_pa: usize,
    buffer_pa: usize,
    last_used_idx: u16,
    avail_idx: u16,
}

// We support up to 4 input devices
const MAX_DEVICES: usize = 4;
static mut DEVICES: [Option<InputDevice>; MAX_DEVICES] = [None, None, None, None];
static mut NUM_DEVICES: usize = 0;

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

    let p0 = crate::memory::alloc_page().expect("virtio_input: vring p0");
    let p1 = crate::memory::alloc_page().expect("virtio_input: vring p1");
    assert!(p1 == p0 + 4096, "virtio_input: vring pages not contiguous");

    core::ptr::write_bytes(crate::memory::vmm::phys_to_virt(p0) as *mut u8, 0, 8192);
    mmio_write(base, REG_QUEUE_PFN, (p0 >> 12) as u32);
    p0
}

unsafe fn init_device(base: usize) -> Option<InputDevice> {
    mmio_write(base, REG_STATUS, 0);
    let mut status = STATUS_ACK | STATUS_DRIVER;
    mmio_write(base, REG_STATUS, status);

    // Accept features
    let host_feat = mmio_read(base, REG_HOST_FEATURES);
    mmio_write(base, REG_GUEST_FEATURES, host_feat); // just accept what it offers
    mmio_write(base, REG_GUEST_PAGE_SIZE, 4096);

    // Setup EventQ (queue 0)
    let vring_pa = setup_queue(base, 0);
    
    // StatusQ (queue 1) not used, but let's select it and ignore it
    // mmio_write(base, REG_QUEUE_SEL, 1);
    
    status |= STATUS_DRIVER_OK;
    mmio_write(base, REG_STATUS, status);

    // Allocate a page for event buffers
    let buffer_pa = crate::memory::alloc_page().expect("virtio_input: buffer page");
    let buffer_va = crate::memory::vmm::phys_to_virt(buffer_pa) as *mut VirtIOInputEvent;
    core::ptr::write_bytes(buffer_va as *mut u8, 0, 4096);

    let vring_va = crate::memory::vmm::phys_to_virt(vring_pa);
    let desc_base = (vring_va + DESC_OFFSET) as *mut VirtqDesc;
    let avail_base = (vring_va + AVAIL_OFFSET) as *mut u16;

    // Fill the avail ring with all descriptors pointing to individual buffers
    for i in 0..QUEUE_SIZE {
        let d = &mut *desc_base.add(i);
        // Each descriptor points to an 8-byte event buffer
        d.addr = (buffer_pa + i * core::mem::size_of::<VirtIOInputEvent>()) as u64;
        d.len = core::mem::size_of::<VirtIOInputEvent>() as u32;
        d.flags = VRING_DESC_F_WRITE;
        d.next = 0;

        write_volatile(avail_base.add(2 + i), i as u16);
    }
    
    // Update avail index
    core::arch::asm!("dmb sy");
    write_volatile(avail_base.add(1), QUEUE_SIZE as u16);
    core::arch::asm!("dmb sy");

    // Notify device
    mmio_write(base, REG_QUEUE_NOTIFY, 0);

    Some(InputDevice {
        base,
        vring_pa,
        buffer_pa,
        last_used_idx: 0,
        avail_idx: QUEUE_SIZE as u16,
    })
}

// ── Public API ────────────────────────────────────────────────────────────────

pub unsafe fn init() {
    let uart = crate::drivers::uart::Uart::new();

    // Scan for VirtIO input devices
    for i in 0..32 {
        let b = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;
        let magic = mmio_read(b, REG_MAGIC);
        if magic == VIRTIO_MAGIC {
            let id = mmio_read(b, REG_DEVICE_ID);
            if id == VIRTIO_DEVICE_INPUT {
                if NUM_DEVICES < MAX_DEVICES {
                    if let Some(dev) = init_device(b) {
                        DEVICES[NUM_DEVICES] = Some(dev);
                        NUM_DEVICES += 1;
                        uart.puts("[input] VirtIO-input initialized at ");
                        uart.put_hex(b);
                        uart.puts("\r\n");
                    }
                }
            }
        }
    }
}

/// Poll all input devices for new events.
pub unsafe fn poll<F>(mut callback: F)
where
    F: FnMut(&VirtIOInputEvent)
{
    for i in 0..NUM_DEVICES {
        if let Some(ref mut dev) = DEVICES[i] {
            let vring_va = crate::memory::vmm::phys_to_virt(dev.vring_pa);
            let used_idx_ptr = (vring_va + USED_OFFSET + 2) as *const u16;
            let used_ring = (vring_va + USED_OFFSET + 4) as *const u32; // struct vring_used_elem { id: u32, len: u32 }
            
            let avail_base = (vring_va + AVAIL_OFFSET) as *mut u16;

            let mut current_used = read_volatile(used_idx_ptr);
            while dev.last_used_idx != current_used {
                let slot = (dev.last_used_idx as usize) % QUEUE_SIZE;
                
                // Each element in used ring is 8 bytes. ID is the first 4 bytes.
                let id = read_volatile(used_ring.add(slot * 2));
                
                let buffer_va = crate::memory::vmm::phys_to_virt(dev.buffer_pa) as *const VirtIOInputEvent;
                let event = read_volatile(buffer_va.add(id as usize));
                
                // Call the callback
                callback(&event);

                // Put the buffer back into the avail ring
                let avail_slot = (dev.avail_idx as usize) % QUEUE_SIZE;
                write_volatile(avail_base.add(2 + avail_slot), id as u16);
                
                core::arch::asm!("dmb sy");
                dev.avail_idx = dev.avail_idx.wrapping_add(1);
                write_volatile(avail_base.add(1), dev.avail_idx);
                core::arch::asm!("dmb sy");

                dev.last_used_idx = dev.last_used_idx.wrapping_add(1);
                
                // Keep checking in case more arrived
                current_used = read_volatile(used_idx_ptr);
            }

            // Notify device that new buffers are available
            mmio_write(dev.base, REG_QUEUE_NOTIFY, 0);
        }
    }
}

pub unsafe fn poll_sys(out_events: *mut VirtIOInputEvent, max_events: usize) -> usize {
    let mut count = 0;
    for i in 0..NUM_DEVICES {
        if count >= max_events { break; }
        if let Some(ref mut dev) = DEVICES[i] {
            let vring_va = crate::memory::vmm::phys_to_virt(dev.vring_pa);
            let used_idx_ptr = (vring_va + USED_OFFSET + 2) as *const u16;
            let used_ring = (vring_va + USED_OFFSET + 4) as *const u32; // struct vring_used_elem { id: u32, len: u32 }
            
            let avail_base = (vring_va + AVAIL_OFFSET) as *mut u16;

            let mut current_used = read_volatile(used_idx_ptr);
            while dev.last_used_idx != current_used {
                if count >= max_events { break; }
                
                let slot = (dev.last_used_idx as usize) % QUEUE_SIZE;
                let id = read_volatile(used_ring.add(slot * 2));
                
                let buffer_va = crate::memory::vmm::phys_to_virt(dev.buffer_pa) as *const VirtIOInputEvent;
                let event = read_volatile(buffer_va.add(id as usize));
                
                *out_events.add(count) = event;
                count += 1;

                let avail_slot = (dev.avail_idx as usize) % QUEUE_SIZE;
                write_volatile(avail_base.add(2 + avail_slot), id as u16);
                
                core::arch::asm!("dmb sy");
                dev.avail_idx = dev.avail_idx.wrapping_add(1);
                write_volatile(avail_base.add(1), dev.avail_idx);
                core::arch::asm!("dmb sy");

                dev.last_used_idx = dev.last_used_idx.wrapping_add(1);
                current_used = read_volatile(used_idx_ptr);
            }

            mmio_write(dev.base, REG_QUEUE_NOTIFY, 0);
        }
    }
    count
}
