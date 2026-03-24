/// QEMU fw-cfg driver using DMA interface for AArch64 virt machine.
///
/// fw-cfg exposes firmware configuration files to the guest.
/// We use the DMA interface for all data transfers — the byte-by-byte
/// MMIO data port approach has timing issues with QEMU's emulation.
///
/// QEMU virt memory map:
///   fw-cfg DMA: 0x0902_0010 (8 bytes, big-endian)

const FW_CFG_DMA: usize = 0x0902_0010 + crate::memory::vmm::KERNEL_VA_OFFSET;

/// fw-cfg directory selector.
const FW_CFG_FILE_DIR: u16 = 0x0019;

/// DMA control bits.
const FW_CFG_DMA_READ: u32 = 0x02;
const FW_CFG_DMA_SELECT: u32 = 0x08;
const FW_CFG_DMA_WRITE: u32 = 0x10;
const FW_CFG_DMA_ERROR: u32 = 0x01;

/// DMA access descriptor — all fields big-endian, 16-byte aligned.
#[repr(C, align(16))]
struct DmaAccess {
    control: u32,
    length: u32,
    address: u64,
}

/// Perform a single DMA transfer. Returns true on success.
fn dma_op(control: u32, buf: *mut u8, len: usize) -> bool {
    use crate::memory::vmm::to_dma_addr;
    // The fw-cfg device accesses memory by physical address.
    // Both the DMA descriptor (on the kernel stack) and the data buffer
    // must be given as PAs, not kernel high-VAs.
    let buf_pa = to_dma_addr(buf as usize) as u64;
    let mut desc = DmaAccess {
        control: control.to_be(),
        length:  (len as u32).to_be(),
        address: buf_pa.to_be(),
    };

    let desc_pa = to_dma_addr(&mut desc as *mut DmaAccess as usize) as u64;

    unsafe {
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        core::ptr::write_volatile(FW_CFG_DMA as *mut u64, desc_pa.to_be());
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        let result = core::ptr::read_volatile(&desc.control);
        (u32::from_be(result) & FW_CFG_DMA_ERROR) == 0
    }
}

/// Find a fw-cfg file by name. Returns its selector index, or None.
///
/// Reads the entire fw-cfg directory into a page-sized buffer via a
/// single DMA transfer, then parses the entries from RAM.
pub fn find_file(name: &str) -> Option<u16> {
    // Allocate a page to hold the directory (4096 bytes covers ~63 entries).
    // Use phys_to_virt so the CPU accesses via TTBR1; to_dma_addr in dma_op
    // converts the pointer back to PA for the fw-cfg device.
    let page = crate::memory::alloc_page()?;
    let buf = crate::memory::vmm::phys_to_virt(page) as *mut u8;

    let control = FW_CFG_DMA_SELECT | FW_CFG_DMA_READ | ((FW_CFG_FILE_DIR as u32) << 16);

    if !dma_op(control, buf, 4096) {
        crate::memory::free_page(page);
        return None;
    }

    let data = unsafe { core::slice::from_raw_parts(buf, 4096) };

    // First 4 bytes: entry count (big-endian u32)
    let count = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    if count > 63 {
        crate::memory::free_page(page);
        return None;
    }

    // Each entry: size(4) + selector(2) + reserved(2) + name(56) = 64 bytes
    for i in 0..count {
        let base = 4 + i * 64;
        let selector = u16::from_be_bytes([data[base + 4], data[base + 5]]);
        let name_bytes = &data[base + 8..base + 64];

        let name_len = name_bytes.iter()
            .position(|&b| b == 0)
            .unwrap_or(56);

        if name_len == name.len() && &name_bytes[..name_len] == name.as_bytes() {
            crate::memory::free_page(page);
            return Some(selector);
        }
    }

    crate::memory::free_page(page);
    None
}

/// Write data to a fw-cfg file using DMA.
pub fn dma_write(selector: u16, data: *const u8, len: usize) -> bool {
    let control = FW_CFG_DMA_SELECT | FW_CFG_DMA_WRITE | ((selector as u32) << 16);
    dma_op(control, data as *mut u8, len)
}
