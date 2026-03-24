/// ARM GICv2 driver for QEMU virt machine.
///
/// QEMU virt memory map:
///   GIC Distributor (GICD): 0x0800_0000
///   GIC CPU Interface (GICC): 0x0801_0000

const GICD_BASE: usize = 0x0800_0000 + crate::memory::vmm::KERNEL_VA_OFFSET;
const GICC_BASE: usize = 0x0801_0000 + crate::memory::vmm::KERNEL_VA_OFFSET;

// Distributor registers
const GICD_CTLR: usize = GICD_BASE + 0x000;
const GICD_ISENABLER: usize = GICD_BASE + 0x100; // +4 per 32 IRQs
const GICD_ICENABLER: usize = GICD_BASE + 0x180;
const GICD_IPRIORITYR: usize = GICD_BASE + 0x400; // +4 per 4 IRQs
const GICD_ITARGETSR: usize = GICD_BASE + 0x800; // +4 per 4 IRQs
const GICD_ICFGR: usize = GICD_BASE + 0xC00;

// CPU interface registers
const GICC_CTLR: usize = GICC_BASE + 0x000;
const GICC_PMR: usize = GICC_BASE + 0x004;
const GICC_IAR: usize = GICC_BASE + 0x00C;
const GICC_EOIR: usize = GICC_BASE + 0x010;

unsafe fn write_reg(addr: usize, val: u32) {
    core::arch::asm!("str {:w}, [{}]", in(reg) val, in(reg) addr, options(nostack, preserves_flags));
}

unsafe fn read_reg(addr: usize) -> u32 {
    let val: u32;
    core::arch::asm!("ldr {:w}, [{}]", out(reg) val, in(reg) addr, options(nostack, preserves_flags, readonly));
    val
}

/// Initialize the GIC distributor and CPU interface.
pub fn init() {
    unsafe {
        // Disable distributor while configuring
        write_reg(GICD_CTLR, 0);

        // Set all SPIs (IRQ 32+) to target CPU 0, priority 0xa0, level-triggered
        // QEMU virt has up to 288 IRQs; configure IRQs 32..288
        for irq in (32..288).step_by(4) {
            let offset = (irq / 4) * 4;
            write_reg(GICD_IPRIORITYR + offset, 0xa0a0a0a0);
            write_reg(GICD_ITARGETSR + offset, 0x01010101); // CPU 0
        }

        // Disable all SPIs by default
        for irq in (32..288).step_by(32) {
            let offset = (irq / 32) * 4;
            write_reg(GICD_ICENABLER + offset, 0xFFFF_FFFF);
        }

        // Enable distributor
        write_reg(GICD_CTLR, 1);

        // CPU interface: set priority mask to accept all, enable interface
        write_reg(GICC_PMR, 0xFF);
        write_reg(GICC_CTLR, 1);
    }
}

/// Enable a specific IRQ number in the GIC.
pub fn enable_irq(irq: u32) {
    unsafe {
        let reg = GICD_ISENABLER + ((irq / 32) * 4) as usize;
        let bit = 1u32 << (irq % 32);
        write_reg(reg, bit);
    }
}

/// Acknowledge an interrupt — returns the IRQ number.
/// Call this at the start of your IRQ handler.
pub fn acknowledge() -> u32 {
    unsafe { read_reg(GICC_IAR) & 0x3FF }
}

/// Signal end-of-interrupt to the GIC.
/// Call this when you've finished handling the IRQ.
pub fn end_of_interrupt(irq: u32) {
    unsafe { write_reg(GICC_EOIR, irq); }
}

/// Initialize only the per-CPU GIC CPU interface (banked registers).
/// Called on each secondary core after bring-up; the distributor is
/// already configured by the primary core's `init()` call.
pub fn init_cpu_interface() {
    unsafe {
        write_reg(GICC_PMR, 0xFF);  // accept all priorities
        write_reg(GICC_CTLR, 1);    // enable CPU interface
    }
}
