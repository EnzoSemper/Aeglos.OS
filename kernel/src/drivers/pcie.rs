//! PCIe ECAM (Enhanced Configuration Access Mechanism) bus enumeration.
//!
//! QEMU virt machine: ECAM config space at physical 0x4010_0000_0000 (64-bit).
//! We access it via the kernel VA alias (PA + KERNEL_VA_OFFSET).
//!
//! Physical hardware: base from DTB /pcie node `reg` property (not yet parsed;
//! falls back to the QEMU constant).

extern crate alloc;

use crate::memory::vmm::KERNEL_VA_OFFSET;

// QEMU virt machine 64-bit PCIe ECAM window (physical address).
// The 32-bit fallback used by some older QEMU versions.
const PCIE_ECAM_PHYS_64: usize = 0x4010_0000_0000;
const PCIE_ECAM_PHYS_32: usize = 0x3F00_0000;
const PCIE_ECAM_PHYS_F0: usize = 0xF000_0000;

// PCIe config space register offsets (byte offsets within a BDF's 4 KiB region)
pub const PCI_VENDOR_ID:      usize = 0x00;
pub const PCI_DEVICE_ID:      usize = 0x02;
pub const PCI_COMMAND:        usize = 0x04;
pub const PCI_STATUS:         usize = 0x06;
pub const PCI_CLASS_REV:      usize = 0x08; // 32-bit: class[31:8] + rev[7:0]
pub const PCI_HEADER_TYPE:    usize = 0x0E;
pub const PCI_BAR0:           usize = 0x10;
pub const PCI_BAR1:           usize = 0x14;
pub const PCI_INTERRUPT_LINE: usize = 0x3C;
pub const PCI_INTERRUPT_PIN:  usize = 0x3D;

pub const PCI_CMD_IO_SPACE:    u16 = 1 << 0;
pub const PCI_CMD_MEM_SPACE:   u16 = 1 << 1;
pub const PCI_CMD_BUS_MASTER:  u16 = 1 << 2;
pub const PCI_CMD_INT_DISABLE: u16 = 1 << 10;

/// A discovered PCIe device.
#[derive(Clone, Debug)]
pub struct PciDevice {
    pub bus:      u8,
    pub dev:      u8,
    pub func:     u8,
    pub vendor:   u16,
    pub device:   u16,
    pub class:    u8,
    pub subclass: u8,
    /// BAR0 physical base address (decoded, type-flags cleared).
    pub bar0:     usize,
    /// BAR0 region size in bytes (probed by write-all-ones technique).
    pub bar0_size: usize,
    /// Legacy INTx line number from config space.
    pub irq:      u8,
}

// The kernel-VA base of the ECAM window currently in use.
// Set once in `enumerate()`, read by cfg_read*/cfg_write* helpers.
static mut ECAM_VA: usize = 0;
/// ECAM hint from DTB — set by main.rs before calling enumerate().
static mut DTB_ECAM_HINT: usize = 0;

/// Set the DTB-discovered ECAM base address so enumerate() tries it first.
pub fn set_dtb_ecam_hint(pa: usize) {
    unsafe { DTB_ECAM_HINT = pa; }
}

// ── Low-level config space helpers ───────────────────────────────────────────

/// Compute the kernel-VA pointer into ECAM config space for a given BDF + offset.
#[inline(always)]
fn ecam_ptr(bus: u8, dev: u8, func: u8, offset: usize) -> usize {
    let base = unsafe { ECAM_VA };
    // ECAM layout: bus[27:20] | dev[19:15] | func[14:12] | reg[11:0]
    base + ((bus  as usize) << 20)
        + ((dev  as usize) << 15)
        + ((func as usize) << 12)
        + offset
}

/// Read a 16-bit word from PCIe config space.
pub fn cfg_read16(bus: u8, dev: u8, func: u8, offset: usize) -> u16 {
    unsafe {
        core::ptr::read_volatile(ecam_ptr(bus, dev, func, offset) as *const u16)
    }
}

/// Read a 32-bit dword from PCIe config space.
pub fn cfg_read32(bus: u8, dev: u8, func: u8, offset: usize) -> u32 {
    unsafe {
        core::ptr::read_volatile(ecam_ptr(bus, dev, func, offset) as *const u32)
    }
}

/// Read a single byte from PCIe config space.
pub fn cfg_read8(bus: u8, dev: u8, func: u8, offset: usize) -> u8 {
    unsafe {
        core::ptr::read_volatile(ecam_ptr(bus, dev, func, offset) as *const u8)
    }
}

/// Write a 16-bit word to PCIe config space.
pub fn cfg_write16(bus: u8, dev: u8, func: u8, offset: usize, val: u16) {
    unsafe {
        core::ptr::write_volatile(ecam_ptr(bus, dev, func, offset) as *mut u16, val);
    }
}

/// Write a 32-bit dword to PCIe config space.
pub fn cfg_write32(bus: u8, dev: u8, func: u8, offset: usize, val: u32) {
    unsafe {
        core::ptr::write_volatile(ecam_ptr(bus, dev, func, offset) as *mut u32, val);
    }
}

// ── BAR size probing ──────────────────────────────────────────────────────────

/// Probe a BAR to determine its required memory window size.
/// Saves and restores the original BAR value.
fn bar_probe_size(bus: u8, dev: u8, func: u8, bar_reg: usize) -> usize {
    let orig = cfg_read32(bus, dev, func, bar_reg);
    cfg_write32(bus, dev, func, bar_reg, 0xFFFF_FFFF);
    let readback = cfg_read32(bus, dev, func, bar_reg);
    cfg_write32(bus, dev, func, bar_reg, orig);

    if readback == 0 || readback == 0xFFFF_FFFF {
        return 0;
    }
    // Mask off the type/prefetchable flags in bits [3:0]
    let mask = readback & 0xFFFF_FFF0;
    (!(mask as usize)).wrapping_add(1)
}

// ── Enumeration ───────────────────────────────────────────────────────────────

/// Enumerate PCIe devices on buses 0–4.
///
/// Probes well-known ECAM physical addresses, picks the first one that
/// produces a valid vendor ID on bus 0 / dev 0 / func 0, then scans all
/// slots and functions.  Returns a `Vec` of discovered devices.
pub fn enumerate() -> alloc::vec::Vec<PciDevice> {
    use alloc::vec::Vec;

    let uart = crate::drivers::uart::Uart::new();

    // Candidate ECAM physical bases in priority order.
    // DTB hint (if non-zero) is tried first; then well-known QEMU bases.
    // Access via kernel VA alias (PA + KERNEL_VA_OFFSET).
    let dtb_hint = unsafe { DTB_ECAM_HINT };
    let mut candidates_buf = [0usize; 4];
    let candidates: &[usize] = if dtb_hint != 0 {
        candidates_buf[0] = dtb_hint;
        candidates_buf[1] = PCIE_ECAM_PHYS_64;
        candidates_buf[2] = PCIE_ECAM_PHYS_32;
        candidates_buf[3] = PCIE_ECAM_PHYS_F0;
        &candidates_buf
    } else {
        &[PCIE_ECAM_PHYS_64, PCIE_ECAM_PHYS_32, PCIE_ECAM_PHYS_F0]
    };

    // Size needed for buses 0-4: 5 buses × 32 devs × 8 funcs × 4 KiB = 5 MiB.
    // Map 8 MiB per candidate so all functions are accessible.
    const ECAM_MAP_SIZE: usize = 8 * 1024 * 1024;

    let mut found_base = false;
    for &phys in candidates {
        // Map this candidate ECAM window into the kernel TTBR1 table
        // so that cfg_read* accesses at VA = phys + KERNEL_VA_OFFSET work.
        crate::memory::vmm::map_kernel_mmio(phys, ECAM_MAP_SIZE);

        // The 64-bit base at 0x4010_0000_0000 is above the 32-bit range;
        // adding KERNEL_VA_OFFSET wraps it into the 64-bit kernel VA space.
        let va = phys.wrapping_add(KERNEL_VA_OFFSET);
        unsafe { ECAM_VA = va; }

        let vid = cfg_read16(0, 0, 0, PCI_VENDOR_ID);
        if vid != 0xFFFF && vid != 0x0000 {
            uart.puts("[pcie] ECAM found at phys=0x");
            uart.put_hex(phys);
            uart.puts(" (VA=0x");
            uart.put_hex(va);
            uart.puts(")\r\n");
            found_base = true;
            break;
        }
    }

    if !found_base {
        uart.puts("[pcie] No ECAM base found — PCIe enumeration skipped\r\n");
        unsafe { ECAM_VA = 0; }
        return Vec::new();
    }

    let mut found: Vec<PciDevice> = Vec::new();

    // Simple BAR allocator: hand out addresses from the 32-bit PCIe MMIO window.
    // QEMU virt: 0x1000_0000–0x3EFF_0000 is available for 32-bit PCIe MMIO.
    // We start just above 0x1000_0000; each BAR is aligned to its size.
    let mut bar_alloc: usize = 0x1000_0000;

    for bus in 0u8..=4 {
        for dev in 0u8..32 {
            // Quick-check: if func 0 has no device, skip all functions.
            let vid0 = cfg_read16(bus, dev, 0, PCI_VENDOR_ID);
            if vid0 == 0xFFFF {
                continue;
            }

            let header_type = cfg_read8(bus, dev, 0, PCI_HEADER_TYPE);
            let is_multifunction = (header_type & 0x80) != 0;
            let max_func: u8 = if is_multifunction { 8 } else { 1 };

            for func in 0..max_func {
                let vendor = cfg_read16(bus, dev, func, PCI_VENDOR_ID);
                if vendor == 0xFFFF {
                    continue;
                }
                let device_id = cfg_read16(bus, dev, func, PCI_DEVICE_ID);

                // class_rev: bits[31:24]=class, [23:16]=subclass, [15:8]=prog_if, [7:0]=rev
                let class_rev = cfg_read32(bus, dev, func, PCI_CLASS_REV);
                let class    = ((class_rev >> 24) & 0xFF) as u8;
                let subclass = ((class_rev >> 16) & 0xFF) as u8;

                // Only assign/probe BARs for non-bridge devices (class != 0x06).
                // Bridge BARs can be enormous (GiB) and would require huge TLB flushes.
                let bar0_raw  = cfg_read32(bus, dev, func, PCI_BAR0) as usize;
                let (bar0, bar0_size) = if class != 0x06 {
                    let bar0_size_probed = bar_probe_size(bus, dev, func, PCI_BAR0);
                    // Use probed size if valid; fall back to 64 KiB (covers HDA 16 KiB).
                    // QEMU returns 0xFFFFFFFF for uninitialized BARs, making probe return 0.
                    let bar0_size = if bar0_size_probed > 0 && bar0_size_probed <= 0x100_0000 {
                        bar0_size_probed
                    } else {
                        65536 // 64 KiB default
                    };
                    let bar0 = if bar0_raw & !0xF == 0 {
                        // BAR uninitialized — assign from our allocator.
                        let align = bar0_size.max(4096);
                        bar_alloc = (bar_alloc + align - 1) & !(align - 1);
                        let assigned = bar_alloc;
                        bar_alloc += bar0_size;
                        cfg_write32(bus, dev, func, PCI_BAR0, assigned as u32);
                        crate::memory::vmm::map_kernel_mmio(assigned, bar0_size);
                        uart.puts("[pcie]   assigned BAR0=0x");
                        uart.put_hex(assigned);
                        uart.puts(" size=0x");
                        uart.put_hex(bar0_size);
                        uart.puts("\r\n");
                        assigned
                    } else {
                        bar0_raw & !0xFusize
                    };
                    (bar0, bar0_size)
                } else {
                    (bar0_raw & !0xFusize, 0)
                };

                let irq = cfg_read8(bus, dev, func, PCI_INTERRUPT_LINE);

                // Enable Memory Space + Bus Mastering; mask legacy INTx.
                let cmd = cfg_read16(bus, dev, func, PCI_COMMAND);
                cfg_write16(bus, dev, func, PCI_COMMAND,
                    cmd | PCI_CMD_MEM_SPACE | PCI_CMD_BUS_MASTER | PCI_CMD_INT_DISABLE);

                uart.puts("[pcie] ");
                uart.put_hex(bus as usize);
                uart.puts(":");
                uart.put_hex(dev as usize);
                uart.puts(".");
                uart.put_hex(func as usize);
                uart.puts("  ");
                uart.put_hex(vendor as usize);
                uart.puts(":");
                uart.put_hex(device_id as usize);
                uart.puts("  class=");
                uart.put_hex(class as usize);
                uart.puts("/");
                uart.put_hex(subclass as usize);
                uart.puts("  bar0=0x");
                uart.put_hex(bar0);
                uart.puts("\r\n");

                found.push(PciDevice {
                    bus, dev, func, vendor, device: device_id,
                    class, subclass, bar0, bar0_size, irq,
                });
            }
        }
    }

    uart.puts("[pcie] Enumeration complete: ");
    uart.put_dec(found.len());
    uart.puts(" device(s)\r\n");
    found
}
