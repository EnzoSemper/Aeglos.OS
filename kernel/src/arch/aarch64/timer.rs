/// ARM Generic Timer — EL1 physical timer.
///
/// On QEMU virt, the Virtual timer is PPI 27 (IRQ 27).
/// Timer runs at 100 Hz for preemptive scheduling.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Counts how many timer ticks have occurred since boot.
static TICK_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Timer frequency in Hz (read from hardware once).
static mut TIMER_FREQ: u64 = 0;

/// Timer fires 100 times per second for preemptive scheduling.
const TICKS_PER_SECOND: u64 = 100;

/// Initialize the timer: read frequency, set first countdown, enable.
pub fn init() {
    let freq: u64;
    unsafe {
        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) freq);
        TIMER_FREQ = freq;
    }

    let interval = freq / TICKS_PER_SECOND;
    unsafe {
        core::arch::asm!("msr cntv_tval_el0, {}", in(reg) interval);
        core::arch::asm!("msr cntv_ctl_el0, {}", in(reg) 1u64);
    }

    super::gic::enable_irq(27);
}

/// Handle timer IRQ. Takes the current SP (trap frame pointer) and
/// returns the SP to use for restoring context (may be a different
/// task's stack if a context switch occurs).
pub fn handle_irq(sp: u64) -> u64 {
    let count = TICK_COUNT.fetch_add(1, Ordering::Relaxed) + 1;

    if count % 100 == 0 {
        let uart = crate::drivers::uart::Uart::new();
        uart.puts("[timer] ");
        uart.put_dec(count / 100);
        uart.puts("s\r\n");
    }

    // Re-arm the timer
    let interval = unsafe { TIMER_FREQ } / TICKS_PER_SECOND;
    unsafe {
        core::arch::asm!("msr cntv_tval_el0, {}", in(reg) interval);
    }

    // Poll the network stack (drain VirtIO-net RX queue)
    crate::net::poll();

    // One-shot HDA melody stop
    crate::drivers::hda::tick();

    // Call scheduler — may return a different SP for context switch
    crate::process::scheduler::schedule_next(sp)
}

/// Get the current tick count (100 Hz — 1 tick = 10 ms).
pub fn ticks() -> usize {
    TICK_COUNT.load(Ordering::Relaxed)
}

/// Alias used by the network stack.
#[inline(always)]
pub fn tick_count() -> usize {
    TICK_COUNT.load(Ordering::Relaxed)
}

/// Read the current virtual timer counter (cntvct_el0).
/// Used for high-resolution RTT measurement in the network stack.
pub fn physical_timer_count() -> u64 {
    let count: u64;
    unsafe { core::arch::asm!("mrs {}, cntvct_el0", out(reg) count); }
    count
}

/// Return the timer frequency in Hz.
pub fn physical_timer_freq() -> u64 {
    unsafe { TIMER_FREQ }
}

/// Read the virtual counter — alias used by TLS for ephemeral key seeding.
#[inline(always)]
pub fn read_counter() -> u64 {
    physical_timer_count()
}
