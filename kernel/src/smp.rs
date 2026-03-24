//! SMP secondary-core bring-up.
//!
//! Secondary cores start in the `.Lpark` WFE loop in `boot.rs`.
//! `init()` wakes each core via PSCI CPU_ON (or a bare SEV for spin-table
//! boards), passing `_secondary_start` as the entry point.
//!
//! Each secondary:
//!   1. Runs `_secondary_start` (assembly in boot.rs) at its low PA.
//!   2. Inherits the boot page tables (TTBR0 identity + TTBR1 high-VA).
//!   3. Enables the MMU with the same MAIR / TCR as the primary.
//!   4. Switches SP to the high-VA stack pre-allocated here.
//!   5. Branches to `secondary_main(cpu_id)`.

use crate::memory;
use crate::memory::vmm::phys_to_virt;

pub const MAX_CPUS: usize = 4;
const STACK_PAGES: usize = 64; // 256 KiB per secondary CPU

// ── Shared data written by primary, read by secondary entry assembly ──────────

/// High-VA stack tops for CPUs 1..MAX_CPUS-1.
/// Index 0 = CPU 1, index 1 = CPU 2, etc.
/// The primary writes this before waking secondaries; the secondary reads it
/// after its MMU enable (TTBR1 makes the high-VA address accessible).
#[unsafe(no_mangle)]
pub static mut SMP_STACK_TOPS: [usize; MAX_CPUS - 1] = [0; MAX_CPUS - 1];

// ── PSCI ──────────────────────────────────────────────────────────────────────

const PSCI_CPU_ON_64: u64 = 0xC400_0003;
const PSCI_SUCCESS:   i64 = 0;
const PSCI_ALREADY_ON: i64 = -4;

fn psci_cpu_on(mpidr: u64, entry_pa: u64, context: u64) -> i64 {
    let result: i64;
    unsafe {
        core::arch::asm!(
            "smc #0",
            inout("x0") PSCI_CPU_ON_64 as i64 => result,
            in("x1") mpidr,
            in("x2") entry_pa,
            in("x3") context,
            options(nomem, nostack),
        );
    }
    result
}

// ── Online counter ────────────────────────────────────────────────────────────

static mut SECONDARIES_ONLINE: usize = 0;

/// Number of secondaries that have completed bring-up.
pub fn secondaries_online() -> usize {
    unsafe { SECONDARIES_ONLINE }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Bring up `cpu_count - 1` secondary cores.
///
/// Must be called after the interrupt controller is initialized on the primary
/// and before `timer::init()` / IRQ enable.
pub fn init(cpu_count: usize, psci: bool) {
    extern "C" { fn _secondary_start(); }
    let uart = crate::drivers::uart::Uart::new();

    let count = cpu_count.min(MAX_CPUS);
    if count <= 1 {
        uart.puts("[smp]  1 CPU (uniprocessor)\r\n");
        return;
    }

    uart.puts("[smp]  Waking ");
    uart.put_dec(count - 1);
    uart.puts(" secondary core(s)...\r\n");

    let entry_pa = _secondary_start as *const () as usize as u64;

    for cpu in 1..count {
        // Allocate a 256 KiB kernel stack for this secondary.
        let Some(stack_pa) = memory::alloc_pages(STACK_PAGES) else {
            uart.puts("[smp]  OOM: no stack for CPU ");
            uart.put_dec(cpu);
            uart.puts("\r\n");
            continue;
        };

        let stack_top_va = phys_to_virt(stack_pa) + STACK_PAGES * memory::PAGE_SIZE;
        unsafe { SMP_STACK_TOPS[cpu - 1] = stack_top_va; }

        uart.puts("[smp]  CPU");
        uart.put_dec(cpu);
        uart.puts(" stack top=");
        uart.put_hex(stack_top_va);
        uart.puts("\r\n");

        // Memory barrier: ensure SMP_STACK_TOPS write is visible before SEV/SMC.
        unsafe { core::arch::asm!("dsb sy", "isb", options(nomem, nostack)); }

        if psci {
            // QEMU virt: MPIDR[7:0] == cpu index for a flat cluster.
            let mpidr = cpu as u64;
            let ret = psci_cpu_on(mpidr, entry_pa, cpu as u64);
            if ret == PSCI_SUCCESS || ret == PSCI_ALREADY_ON {
                uart.puts("[smp]  CPU");
                uart.put_dec(cpu);
                uart.puts(" powered on via PSCI\r\n");
            } else {
                uart.puts("[smp]  PSCI CPU_ON failed for CPU");
                uart.put_dec(cpu);
                uart.puts(" ret=");
                uart.put_dec((-ret) as usize);
                uart.puts("\r\n");
            }
        } else {
            // Spin-table: issue SEV to wake the WFE loop.
            // The core re-checks a release address; this is a best-effort wake.
            unsafe { core::arch::asm!("sev", options(nomem, nostack)); }
            uart.puts("[smp]  CPU");
            uart.put_dec(cpu);
            uart.puts(" (spin-table SEV)\r\n");
        }
    }
}

// ── Secondary entry point (called from _secondary_start in boot.rs) ───────────

/// Called by each secondary CPU after MMU enable and stack switch.
/// `cpu_id` = MPIDR[7:0] (1, 2, 3 …).
#[unsafe(no_mangle)]
pub extern "C" fn secondary_main(cpu_id: usize) -> ! {
    let uart = crate::drivers::uart::Uart::new();
    uart.puts("[smp]  CPU");
    uart.put_dec(cpu_id);
    uart.puts(" secondary_main reached\r\n");

    // Register this CPU's idle task in the scheduler.
    // Must happen before enabling IRQs so schedule_next sees a valid CURRENT slot.
    crate::process::scheduler::secondary_idle_init(cpu_id);

    // Init this CPU's GIC CPU interface (distributor was set up by primary).
    crate::arch::aarch64::gic::init_cpu_interface();

    // Ensure FP/SIMD is accessible at EL1 (should already be set globally,
    // but CPACR_EL1 is banked per-cluster on some implementations).
    unsafe {
        let mut cpacr: u64;
        core::arch::asm!("mrs {}, cpacr_el1", out(reg) cpacr);
        cpacr |= 3 << 20; // FPEN = 11: EL0+EL1 FP/SIMD
        core::arch::asm!("msr cpacr_el1, {}", in(reg) cpacr);
        core::arch::asm!("isb");
    }

    // Announce online.
    unsafe { SECONDARIES_ONLINE += 1; }
    uart.puts("[smp]  CPU");
    uart.put_dec(cpu_id);
    uart.puts(" online — entering idle loop\r\n");

    // Enable IRQs so the timer preempts us and can schedule tasks here.
    unsafe { core::arch::asm!("msr daifclr, #2", options(nomem, nostack)); }

    // Secondary idle loop.  The scheduler runs here via timer IRQ preemption
    // just as it does on the primary.
    loop {
        unsafe { core::arch::asm!("wfi", options(nostack)); }
    }
}
