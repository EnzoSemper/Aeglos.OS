/// Apple Interrupt Controller v2 (AIC2) driver.
///
/// Used on Apple Silicon M1 and later (M2, M3, M4).  AIC is NOT compatible
/// with ARM GICv2 — it has its own register layout, acknowledge cycle, and
/// delivers the virtual timer via FIQ rather than SPI.
///
/// Register offsets verified against the Asahi Linux apple_aic.c driver and
/// the m1n1 AIC implementation.
///
/// Boot flow:
///   1. Bootloader (m1n1 / U-Boot) passes AIC base via Device Tree `aic` node.
///   2. `aic::set_base(base)` stores it (called from main.rs after DTB parse).
///   3. `aic::init()` masks all HW IRQs, routes them to CPU 0.
///   4. FIQ vector in the exception table calls `aic::fiq_handler()` which
///      dispatches the virtual timer tick (timer is FIQ-delivered on Apple).
///   5. IRQ vector calls `irq_handler()` which calls `aic::acknowledge()` /
///      `aic::end_of_interrupt()` just like it does for the GIC.

use crate::memory::vmm::KERNEL_VA_OFFSET;

// ── Global config registers (relative to AIC MMIO base) ───────────────────────

const AIC_REV:        usize = 0x0000; // u32 RO: revision (2 for AIC2)
const AIC_INFO:       usize = 0x0004; // u32 RO: bits[15:0] = NR_HW_IRQs
const AIC_GLOBAL_CFG: usize = 0x0010; // u32 RW: global config (AIC2; AIC1 = 0x002C)

// ── Per-IRQ registers ─────────────────────────────────────────────────────────
// Each bank covers 32 IRQs; write bit n%32 to register base + (n/32)*4.

const AIC_SW_SET:     usize = 0x4000; // Software-trigger set
const AIC_SW_CLR:     usize = 0x4080; // Software-trigger clear
const AIC_MASK_SET:   usize = 0x4100; // Mask (disable) IRQ bank
const AIC_MASK_CLR:   usize = 0x4180; // Unmask (enable) IRQ bank
const AIC_TARGET_CPU: usize = 0x3000; // IRQ→CPU affinity, 4 bytes per IRQ

// ── Per-CPU registers (CPU 0 at AIC_BASE + AIC_PERCPU_BASE) ──────────────────
// Stride between CPUs is reported in AIC_INFO1[12:0]; for CPU 0 offset is 0.

const AIC_PERCPU_BASE:  usize = 0x5000; // start of per-CPU region
const AIC_CPU_WHOAMI:   usize = 0x0000; // (relative) which CPU is this?
const AIC_CPU_IACK:     usize = 0x0004; // Read: acknowledge + get event
//   bits[31:16] = event type  (1 = HW IRQ, 4 = IPI/FIQ)
//   bits[15:0]  = IRQ number  (when type = 1)
const AIC_CPU_IPI_ACK:  usize = 0x000C; // Write to clear IPI
const AIC_CPU_IPI_MASK_SET: usize = 0x0018;
const AIC_CPU_IPI_MASK_CLR: usize = 0x001C;

const AIC_EVENT_TYPE_HW:  u32 = 1;
const AIC_EVENT_TYPE_IPI: u32 = 4;

/// Sentinel returned by `acknowledge()` when no IRQ is pending or IRQ is spurious.
pub const AIC_SPURIOUS: u32 = 0xFFFF_FFFF;

// ── MMIO base ─────────────────────────────────────────────────────────────────

/// Physical base address of the AIC, discovered from the Device Tree.
/// Stored as a virtual address (physical + KERNEL_VA_OFFSET) once set.
static mut AIC_BASE_VA: usize = 0;

/// Provide the AIC physical base address (call this from main.rs after DTB
/// parsing has discovered the `aic` node `reg` property).
pub fn set_base(phys_base: usize) {
    unsafe { AIC_BASE_VA = phys_base + KERNEL_VA_OFFSET; }
}

/// True if a valid base address has been configured.
pub fn is_available() -> bool {
    unsafe { AIC_BASE_VA != 0 }
}

// ── Register accessors ────────────────────────────────────────────────────────

unsafe fn aic_read(offset: usize) -> u32 {
    let addr = unsafe { AIC_BASE_VA } + offset;
    let val: u32;
    core::arch::asm!(
        "ldr {:w}, [{}]",
        out(reg) val, in(reg) addr,
        options(nostack, preserves_flags, readonly)
    );
    val
}

unsafe fn aic_write(offset: usize, val: u32) {
    let addr = unsafe { AIC_BASE_VA } + offset;
    core::arch::asm!(
        "str {:w}, [{}]",
        in(reg) val, in(reg) addr,
        options(nostack, preserves_flags)
    );
}

// ── Public API (mirrors gic.rs interface) ─────────────────────────────────────

/// Initialise the AIC: mask all HW IRQs, route everything to CPU 0, enable.
pub fn init() {
    unsafe {
        let rev = aic_read(AIC_REV);
        let info = aic_read(AIC_INFO);
        let nr_irq = (info & 0xFFFF) as usize;

        {
            let uart = crate::drivers::uart::Uart::new();
            uart.puts("[aic]  AIC v");
            uart.put_dec(rev as usize);
            uart.puts("  nr_irq=");
            uart.put_dec(nr_irq);
            uart.puts("\r\n");
        }

        // Mask all HW IRQs
        let banks = (nr_irq + 31) / 32;
        for b in 0..banks {
            aic_write(AIC_MASK_SET + b * 4, 0xFFFF_FFFF);
        }

        // Route all IRQs to CPU 0 (bit 0 = CPU 0)
        for n in 0..nr_irq {
            aic_write(AIC_TARGET_CPU + n * 4, 1);
        }

        // Unmask IPI on CPU 0
        aic_write(AIC_PERCPU_BASE + AIC_CPU_IPI_MASK_CLR, 0x1);

        // Enable global dispatch
        aic_write(AIC_GLOBAL_CFG, 1);
    }
}

/// Enable (unmask) a specific HW IRQ.
pub fn enable_irq(irq: u32) {
    unsafe {
        let bank = (irq / 32) as usize;
        let bit  = 1u32 << (irq % 32);
        aic_write(AIC_MASK_CLR + bank * 4, bit);
    }
}

/// Disable (mask) a specific HW IRQ.
pub fn disable_irq(irq: u32) {
    unsafe {
        let bank = (irq / 32) as usize;
        let bit  = 1u32 << (irq % 32);
        aic_write(AIC_MASK_SET + bank * 4, bit);
    }
}

/// Acknowledge the current interrupt.  Returns the HW IRQ number, or
/// `AIC_SPURIOUS` if the event is not a HW IRQ (e.g. IPI or spurious).
///
/// Reading AIC_CPU_IACK is the acknowledgement — no separate EOI register.
pub fn acknowledge() -> u32 {
    unsafe {
        let event = aic_read(AIC_PERCPU_BASE + AIC_CPU_IACK);
        let ev_type = event >> 16;
        let irq     = event & 0xFFFF;
        if ev_type == AIC_EVENT_TYPE_HW {
            irq
        } else if ev_type == AIC_EVENT_TYPE_IPI {
            // Clear IPI
            aic_write(AIC_PERCPU_BASE + AIC_CPU_IPI_ACK, 1);
            AIC_SPURIOUS
        } else {
            AIC_SPURIOUS
        }
    }
}

/// Signal end-of-interrupt.
///
/// For AIC, reading IACK is the ACK.  After the IRQ source is cleared by the
/// device driver, AIC automatically re-enables delivery.  Re-mask→re-unmask
/// ensures level-triggered sources re-arm correctly.
pub fn end_of_interrupt(irq: u32) {
    // Re-enable the IRQ so that level-triggered devices re-assert correctly.
    enable_irq(irq);
}

// ── FIQ handler (timer on Apple Silicon) ─────────────────────────────────────

/// Inner FIQ handler — called from `exceptions::fiq_handler()`.
///
/// On Apple Silicon the virtual timer fires as FIQ rather than as an AIC SPI.
/// We check `CNTV_CTL_EL0.ISTATUS` to confirm it's the timer and forward to
/// the existing timer handler.  Returns the (possibly new) stack pointer.
pub fn fiq_handler_inner(sp: u64) -> u64 {
    let cntv_ctl: u64;
    unsafe {
        core::arch::asm!("mrs {}, cntv_ctl_el0", out(reg) cntv_ctl);
    }

    // ENABLE=bit0, IMASK=bit1, ISTATUS=bit2
    // Timer is pending when ENABLE=1, IMASK=0, ISTATUS=1 → value & 0x7 == 0x5
    if cntv_ctl & 0x7 == 0x5 {
        // Delegate to the existing timer handler (which reloads CNTV_TVAL and
        // calls the scheduler tick).
        return crate::arch::aarch64::timer::handle_irq(sp);
    }

    // Unknown FIQ — log and continue.
    let uart = crate::drivers::uart::Uart::new();
    uart.puts("[fiq] Unhandled FIQ  cntv_ctl=0x");
    uart.put_hex(cntv_ctl as usize);
    uart.puts("\r\n");
    sp
}
