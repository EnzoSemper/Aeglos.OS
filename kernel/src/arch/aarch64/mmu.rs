/// AArch64 MMU page table switcher.
///
/// After the TTBR1 split the MMU is enabled in boot assembly (_start) with
/// boot page tables.  kernel_main calls `init(ttbr0, ttbr1)` once to replace
/// the boot tables with the runtime tables built by `memory::vmm::init()`.
/// No re-enabling is needed — just TTBR register updates + barriers.

use core::arch::asm;

/// Switch to the runtime page tables.
///
/// `ttbr0` — physical address of the kernel identity L0 table (for kernel tasks
///            and as the default TTBR0 when no user process is scheduled).
/// `ttbr1` — physical address of the high-VA kernel L0 table (always active;
///            maps MMIO and RAM at VA = PA + KERNEL_VA_OFFSET).
///
/// Both tables are built by `memory::vmm::init()` before this call.
pub fn init(ttbr0: usize, ttbr1: usize) {
    unsafe {
        asm!("dsb sy", options(nostack));
        asm!("msr ttbr0_el1, {0}", in(reg) ttbr0 as u64, options(nostack));
        asm!("msr ttbr1_el1, {0}", in(reg) ttbr1 as u64, options(nostack));
        asm!("isb",              options(nostack));
        // The boot tables and runtime tables map identical PA ranges at the
        // same VAs, so TLB entries from the boot phase remain valid.
        // No global flush needed here.
    }
}
