/// Remoboth — SMP-capable round-robin preemptive scheduler.
///
/// Each CPU has its own `CURRENT[cpu_id]` slot so cores independently track
/// which task they're executing.  A single global `TASKS` array (up to 16
/// tasks) is protected by `SCHED_LOCK` — a TAS spinlock.
///
/// Locking discipline
/// ──────────────────
/// • `schedule_next` is called from the timer IRQ handler; on AArch64,
///   exception entry automatically masks IRQs (PSTATE.DAIF.I = 1), so no
///   explicit IRQ masking is needed there.
/// • All other TASKS mutators (send_message, recv_message, yield_cpu, …)
///   may be called from task-body code at EL1h with IRQs enabled.  They
///   call `irq_save()` before acquiring the lock so a timer IRQ on the
///   same CPU cannot re-enter and deadlock.

use super::task::{Task, TaskPrivilege, TaskState, TrapFrame};
use crate::memory;
use crate::smp;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// ── CPU% counters ─────────────────────────────────────────────────────────────

/// Incremented on every timer tick across all CPUs.
static TOTAL_TICKS: AtomicUsize = AtomicUsize::new(0);
/// Incremented when the running task is an idle task at the time of the tick.
static IDLE_TICKS:  AtomicUsize = AtomicUsize::new(0);

// ── Scheduler constants ───────────────────────────────────────────────────────

/// Maximum number of concurrent non-idle tasks.
const MAX_TASKS: usize = 16;
/// Sentinel: no task is assigned to this CPU yet.
const NO_TASK: usize   = MAX_TASKS;

/// Kernel task stack: 256 pages = 1 MiB.
const TASK_STACK_PAGES: usize = 256;
const TASK_STACK_SIZE:  usize = TASK_STACK_PAGES * memory::PAGE_SIZE;

/// User task stack: 64 pages = 256 KiB.
const USER_STACK_PAGES: usize = 64;
const USER_STACK_SIZE:  usize = USER_STACK_PAGES * memory::PAGE_SIZE;

// ── TAS spinlock ──────────────────────────────────────────────────────────────

struct SchedLock(AtomicBool);

impl SchedLock {
    const fn new() -> Self { SchedLock(AtomicBool::new(false)) }

    fn acquire(&self) {
        while self.0
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn release(&self) {
        self.0.store(false, Ordering::Release);
    }
}

static SCHED_LOCK: SchedLock = SchedLock::new();

// ── IRQ masking helpers ───────────────────────────────────────────────────────

/// Save and mask IRQs on the calling CPU.  Returns the previous DAIF value.
/// Must be paired with `irq_restore(saved)`.
#[inline(always)]
fn irq_save() -> u64 {
    let flags: u64;
    unsafe {
        core::arch::asm!("mrs {}, daif", out(reg) flags, options(nomem, nostack));
        core::arch::asm!("msr daifset, #2",            options(nomem, nostack)); // mask IRQ
    }
    flags
}

/// Restore IRQ state saved by `irq_save()`.
#[inline(always)]
fn irq_restore(flags: u64) {
    unsafe { core::arch::asm!("msr daif, {}", in(reg) flags, options(nomem, nostack)); }
}

// ── CPU ID ────────────────────────────────────────────────────────────────────

/// Return MPIDR_EL1[7:0] — the physical CPU index (0 = primary).
#[inline(always)]
pub fn get_cpu_id() -> usize {
    let mpidr: u64;
    unsafe { core::arch::asm!("mrs {}, mpidr_el1", out(reg) mpidr, options(nomem, nostack)); }
    (mpidr & 0xFF) as usize
}

// ── Global state ──────────────────────────────────────────────────────────────

/// The task pool — shared across all CPUs, protected by SCHED_LOCK.
static mut TASKS: [Option<Task>; MAX_TASKS] = {
    const NONE: Option<Task> = None;
    [NONE; MAX_TASKS]
};

/// Per-CPU index of the currently running task in TASKS.
/// CPU 0 starts with slot 0 (idle task).  Secondary CPUs start at NO_TASK
/// until `secondary_idle_init()` is called.
static mut CURRENT: [usize; smp::MAX_CPUS] = {
    let mut arr = [NO_TASK; smp::MAX_CPUS];
    arr[0] = 0; // CPU 0 → idle task
    arr
};

/// Per-CPU last-installed TTBR0 value (avoids redundant TLB flushes).
static mut CURRENT_TTBR0: [usize; smp::MAX_CPUS] = [0; smp::MAX_CPUS];

/// Next task ID to assign.
static mut NEXT_TID: usize = 0;

/// Whether the scheduler is active (≥1 non-idle task exists).
static mut ACTIVE: bool = false;

// ── Initialization ────────────────────────────────────────────────────────────

/// Initialize the scheduler.  Creates the CPU 0 idle task (slot 0, TID 0).
pub fn init() {
    unsafe {
        let tid = NEXT_TID;
        NEXT_TID += 1;

        TASKS[0] = Some(Task {
            tid,
            name:            *b"idle0\0\0\0\0\0\0\0\0\0\0\0",
            state:           TaskState::Running,
            sp:              0,
            stack_base:      0,
            stack_size:      0,
            mailbox:         crate::ipc::Mailbox::new(),
            privilege:       TaskPrivilege::Kernel,
            capabilities:    !0u64,
            ttbr0:           memory::vmm::ttbr0(),
            exit_code:       0,
            wait_tid:        None,
            user_va_base:    0,
            user_va_top:     0,
            user_stack_base: 0,
            user_stack_top:  0,
            priority:        1,
            ticks_remaining: 1,
            ticks_used:      0,
            cpu_pin:         Some(0), // idle0 stays on CPU 0
            syscall_filter:  [!0u64; 2],
            pending_signals: 0,
            signal_mask:     0,
            signal_handler:  0,
        });
        CURRENT[0] = 0;
    }
}

/// Called by each secondary CPU from `secondary_main` after MMU enable.
///
/// Creates a dedicated idle-task slot for this CPU and sets `CURRENT[cpu_id]`
/// to it.  The secondary's WFI loop acts as its idle task.  No separate stack
/// is allocated — the secondary uses its SMP stack, whose SP is saved into
/// the task's `sp` field on the first context switch out.
pub fn secondary_idle_init(cpu_id: usize) {
    if cpu_id == 0 || cpu_id >= smp::MAX_CPUS { return; }

    let flags = irq_save();
    SCHED_LOCK.acquire();

    unsafe {
        // Find a free slot.
        let mut slot = MAX_TASKS;
        for i in 0..MAX_TASKS {
            if TASKS[i].is_none() { slot = i; break; }
        }

        if slot < MAX_TASKS {
            let tid = NEXT_TID;
            NEXT_TID += 1;

            let mut name = *b"idle0\0\0\0\0\0\0\0\0\0\0\0";
            name[4] = b'0' + cpu_id as u8;

            TASKS[slot] = Some(Task {
                tid,
                name,
                state:           TaskState::Running,
                sp:              0, // set on first context-switch out
                stack_base:      0, // SMP stack — managed by smp.rs; don't free here
                stack_size:      0,
                mailbox:         crate::ipc::Mailbox::new(),
                privilege:       TaskPrivilege::Kernel,
                capabilities:    !0u64,
                ttbr0:           memory::vmm::ttbr0(),
                exit_code:       0,
                wait_tid:        None,
                user_va_base:    0,
                user_va_top:     0,
                user_stack_base: 0,
                user_stack_top:  0,
                priority:        1,
                ticks_remaining: 1,
                ticks_used:      0,
                cpu_pin:         Some(cpu_id), // idle tasks stay on their own CPU
                syscall_filter:  [!0u64; 2],
                pending_signals: 0,
                signal_mask:     0,
                signal_handler:  0,
            });
            CURRENT[cpu_id] = slot;
        }
    }

    SCHED_LOCK.release();
    irq_restore(flags);
}

// ── Task spawning ─────────────────────────────────────────────────────────────

pub fn spawn(name: &str, entry: fn() -> !) -> Result<usize, &'static str> {
    let flags = irq_save();
    SCHED_LOCK.acquire();

    let result = unsafe {
        let mut slot = MAX_TASKS;
        for i in 0..MAX_TASKS {
            if TASKS[i].is_none() { slot = i; break; }
        }
        if slot == MAX_TASKS {
            SCHED_LOCK.release();
            irq_restore(flags);
            return Err("task pool full");
        }

        let stack_base = match memory::alloc_pages(TASK_STACK_PAGES) {
            Some(p) => p,
            None => {
                SCHED_LOCK.release();
                irq_restore(flags);
                return Err("out of memory for task stack");
            }
        };

        let tid = NEXT_TID;
        NEXT_TID += 1;
        TASKS[slot] = Some(Task::new(tid, name, entry as usize, stack_base, TASK_STACK_SIZE));
        ACTIVE = true;
        Ok(tid)
    };

    SCHED_LOCK.release();
    irq_restore(flags);
    result
}

pub fn spawn_user(name: &str, entry: fn() -> !, caps: u64) -> Result<usize, &'static str> {
    spawn_user_entry(name, entry as usize, caps)
}

pub fn spawn_user_entry_with_segs(
    name:  &str,
    entry: usize,
    caps:  u64,
    segs:  &[memory::vmm::ProcSegment],
) -> Result<usize, &'static str> {
    // Stack allocations happen outside the lock (page alloc is its own concern).
    let kernel_stack = memory::alloc_pages(TASK_STACK_PAGES)
        .ok_or("out of memory for kernel stack")?;
    let user_stack = memory::alloc_pages(USER_STACK_PAGES)
        .ok_or("out of memory for user stack")?;
    let user_sp = user_stack + USER_STACK_SIZE;

    let mut all_segs: [memory::vmm::ProcSegment; 18] = core::array::from_fn(|_| {
        memory::vmm::ProcSegment { pa: 0, size: 0, el0: false, exec: false, dev: false }
    });
    let mut n = 0;
    for s in segs {
        if n >= 17 { break; }
        all_segs[n] = memory::vmm::ProcSegment {
            pa: s.pa, size: s.size, el0: s.el0, exec: s.exec, dev: s.dev,
        };
        n += 1;
    }
    all_segs[n] = memory::vmm::ProcSegment {
        pa: user_stack, size: USER_STACK_SIZE, el0: true, exec: false, dev: false,
    };
    n += 1;

    let ttbr0 = memory::vmm::create_process_table(&all_segs[..n]);

    let mut va_base = usize::MAX;
    let mut va_top  = 0usize;
    for s in segs {
        if s.el0 && s.size > 0 {
            va_base = va_base.min(s.pa);
            va_top  = va_top.max(s.pa + s.size);
        }
    }
    if va_base == usize::MAX { va_base = 0; }

    let stack_va_base = user_stack;
    let stack_va_top  = user_stack + USER_STACK_SIZE;

    let flags = irq_save();
    SCHED_LOCK.acquire();

    let result = unsafe {
        let mut slot = MAX_TASKS;
        for i in 0..MAX_TASKS {
            if TASKS[i].is_none() { slot = i; break; }
        }
        if slot == MAX_TASKS {
            SCHED_LOCK.release();
            irq_restore(flags);
            return Err("task pool full");
        }

        let tid = NEXT_TID;
        NEXT_TID += 1;

        let task = Task::new_user(
            tid, name, entry,
            kernel_stack, TASK_STACK_SIZE,
            user_sp, caps, ttbr0,
            va_base, va_top,
            stack_va_base, stack_va_top,
        );
        TASKS[slot] = Some(task);
        ACTIVE = true;
        Ok(tid)
    };

    SCHED_LOCK.release();
    irq_restore(flags);
    result
}

pub fn spawn_user_entry(name: &str, entry: usize, caps: u64) -> Result<usize, &'static str> {
    spawn_user_entry_with_segs(name, entry, caps, &[])
}

/// Spawn a new EL0 thread that **shares the caller's TTBR0** (address space).
/// The caller provides the entry point and the top of a pre-allocated user
/// stack (`user_sp`).  A fresh kernel stack is allocated; no new process table
/// is created.  Because threads share TTBR0, context-switching between them
/// never triggers a TLB flush.
///
/// * `entry`   — EL0 instruction address where the new thread starts
/// * `user_sp` — initial SP_EL0 for the new thread (top of its user stack)
/// * `caps`    — capability mask (already attenuated to caller's caps)
/// * `ttbr0`   — parent's TTBR0 value (shared page table)
pub fn spawn_user_thread(
    name:    &str,
    entry:   usize,
    user_sp: usize,
    caps:    u64,
    ttbr0:   usize,
) -> Result<usize, &'static str> {
    let kernel_stack = memory::alloc_pages(TASK_STACK_PAGES)
        .ok_or("out of memory for kernel stack")?;

    let flags = irq_save();
    SCHED_LOCK.acquire();

    let result = unsafe {
        let mut slot = MAX_TASKS;
        for i in 0..MAX_TASKS {
            if TASKS[i].is_none() { slot = i; break; }
        }
        if slot == MAX_TASKS {
            SCHED_LOCK.release();
            irq_restore(flags);
            return Err("task pool full");
        }

        let tid = NEXT_TID;
        NEXT_TID += 1;

        // user_va_base/top = 0,0 → no ELF-range TLB flush on switch (shared table).
        // user_stack_base/top = 0,0 → no stack TLB flush either.
        // The TTBR0 guard (`next_ttbr0 != CURRENT_TTBR0`) prevents any switch
        // when sibling threads share the same table, so no flush is needed.
        let task = Task::new_user(
            tid, name, entry,
            kernel_stack, TASK_STACK_SIZE,
            user_sp, caps, ttbr0,
            0, 0,   // user_va_base, user_va_top
            0, 0,   // user_stack_base, user_stack_top
        );
        TASKS[slot] = Some(task);
        ACTIVE = true;
        Ok(tid)
    };

    SCHED_LOCK.release();
    irq_restore(flags);
    result
}

// ── Core scheduling ───────────────────────────────────────────────────────────

/// Called from the IRQ handler (assembly) with the current SP pointing to the
/// trap frame.  Returns the SP for RESTORE_REGS.  IRQs are already masked by
/// AArch64 exception entry.
#[unsafe(no_mangle)]
pub extern "C" fn schedule_next(current_sp: u64) -> u64 {
    let cpu = get_cpu_id().min(smp::MAX_CPUS - 1);

    SCHED_LOCK.acquire();

    unsafe {
        if !ACTIVE {
            SCHED_LOCK.release();
            return current_sp;
        }

        TOTAL_TICKS.fetch_add(1, Ordering::Relaxed);

        let cur_slot = CURRENT[cpu];

        // If this CPU has no task yet (secondary not yet initialised), do nothing.
        if cur_slot == NO_TASK {
            SCHED_LOCK.release();
            return current_sp;
        }

        // Is the current task an idle task?
        let is_idle = TASKS[cur_slot]
            .as_ref()
            .map(|t| t.stack_base == 0)
            .unwrap_or(true);
        if is_idle { IDLE_TICKS.fetch_add(1, Ordering::Relaxed); }

        // Save SP and charge a tick.  Preempt only when the quantum expires.
        let mut quantum_expired = false;
        if let Some(ref mut cur) = TASKS[cur_slot] {
            cur.sp         = current_sp;
            cur.ticks_used = cur.ticks_used.wrapping_add(1);
            if cur.ticks_remaining > 1 {
                cur.ticks_remaining -= 1;
                if cur.state == TaskState::Running {
                    SCHED_LOCK.release();
                    return current_sp;
                }
            } else {
                quantum_expired = true;
                if cur.state == TaskState::Running {
                    cur.state = TaskState::Ready;
                }
            }
        }

        // ── Work-stealing round-robin ────────────────────────────────────────
        //
        // Phase 1 — tasks hard-pinned to *this* CPU.
        // Phase 2 — migratable tasks (cpu_pin == None); any CPU may run them.
        //           This is the "steal" step: CPU 1 picking up tasks that were
        //           running on CPU 0 when CPU 0 goes idle (and vice-versa).
        // Phase 3 — last resort: any Ready task including idle stubs.
        //
        // Hard-pinned tasks (cpu_pin == Some(other)) are NEVER touched by
        // a different CPU, giving Numenor exclusive use of CPU 1.

        let start = cur_slot;
        let mut idx   = NO_TASK;
        let mut found = false;

        // Phase 1: pinned to this CPU (non-idle real tasks first).
        {
            let mut i = (start + 1) % MAX_TASKS;
            while i != start {
                if let Some(ref t) = TASKS[i] {
                    if t.state == TaskState::Ready
                        && t.stack_base != 0
                        && t.cpu_pin == Some(cpu)
                    {
                        idx   = i;
                        found = true;
                        break;
                    }
                }
                i = (i + 1) % MAX_TASKS;
            }
        }

        // Phase 2: migratable tasks (any CPU can run them).
        if !found {
            let mut i = (start + 1) % MAX_TASKS;
            while i != start {
                if let Some(ref t) = TASKS[i] {
                    if t.state == TaskState::Ready
                        && t.stack_base != 0
                        && t.cpu_pin.is_none()
                    {
                        idx   = i;
                        found = true;
                        break;
                    }
                }
                i = (i + 1) % MAX_TASKS;
            }
        }

        // Phase 3: fall back to any Ready task (idle stubs, etc.).
        if !found {
            let mut i = (start + 1) % MAX_TASKS;
            while i != start {
                if let Some(ref t) = TASKS[i] {
                    if t.state == TaskState::Ready
                        && (t.cpu_pin.is_none() || t.cpu_pin == Some(cpu))
                    {
                        idx   = i;
                        found = true;
                        break;
                    }
                }
                i = (i + 1) % MAX_TASKS;
            }
        }

        if !found {
            // Stay on current; reset quantum.
            if let Some(ref mut cur) = TASKS[cur_slot] {
                cur.state           = TaskState::Running;
                cur.ticks_remaining = cur.priority.max(1);
            }
            SCHED_LOCK.release();
            return current_sp;
        }

        // Reset outgoing task's quantum.
        if quantum_expired {
            if let Some(ref mut cur) = TASKS[cur_slot] {
                cur.ticks_remaining = cur.priority.max(1);
            }
        }

        // Switch.
        CURRENT[cpu] = idx;
        let next_sp;

        if let Some(ref mut next) = TASKS[idx] {
            next.state           = TaskState::Running;
            next.ticks_remaining = next.priority.max(1);

            // TTBR0 switch if needed.
            let next_ttbr0    = next.ttbr0;
            let next_va_base  = next.user_va_base;
            let next_va_top   = next.user_va_top;
            let next_stk_base = next.user_stack_base;
            let next_stk_top  = next.user_stack_top;

            if next_ttbr0 != 0 && next_ttbr0 != CURRENT_TTBR0[cpu] {
                CURRENT_TTBR0[cpu] = next_ttbr0;
                core::arch::asm!("dsb sy",                                         options(nostack));
                core::arch::asm!("msr ttbr0_el1, {0}", in(reg) next_ttbr0 as u64, options(nostack));
                core::arch::asm!("isb",                                            options(nostack));

                // Flush ELF code/data range.
                let flush_base = (next_va_base & !0xFFF) as u64;
                let flush_top  = ((next_va_top + 0xFFF) & !0xFFF) as u64;
                let mut va = flush_base;
                while va < flush_top {
                    core::arch::asm!("tlbi vaae1, {0}", in(reg) va >> 12, options(nostack));
                    va += 0x1000;
                }

                // Flush user stack range separately (avoid spanning the gap).
                if next_stk_base < next_stk_top {
                    let mut va = (next_stk_base & !0xFFF) as u64;
                    let top    = ((next_stk_top + 0xFFF) & !0xFFF) as u64;
                    while va < top {
                        core::arch::asm!("tlbi vaae1, {0}", in(reg) va >> 12, options(nostack));
                        va += 0x1000;
                    }
                }

                core::arch::asm!("dsb sy", options(nostack));
                core::arch::asm!("isb",    options(nostack));
            }

            next_sp = next.sp;
        } else {
            next_sp = current_sp;
        }

        SCHED_LOCK.release();
        next_sp
    }
}

// ── Accessors (all lock-protected) ───────────────────────────────────────────

pub fn current_task_ttbr0() -> usize {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let slot = CURRENT[cpu];
        if slot < MAX_TASKS { TASKS[slot].as_ref().map(|t| t.ttbr0).unwrap_or(0) } else { 0 }
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn current_tid() -> usize {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let slot = CURRENT[cpu];
        if slot < MAX_TASKS { TASKS[slot].as_ref().map(|t| t.tid).unwrap_or(0) } else { 0 }
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn current_caps() -> u64 {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let slot = CURRENT[cpu];
        if slot < MAX_TASKS { TASKS[slot].as_ref().map(|t| t.capabilities).unwrap_or(0) } else { 0 }
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn current_slot() -> usize {
    let cpu = get_cpu_id().min(smp::MAX_CPUS - 1);
    unsafe { CURRENT[cpu] }
}

pub fn get_task_caps(tid: usize) -> Option<u64> {
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let mut result = None;
        for i in 0..MAX_TASKS {
            if let Some(ref t) = TASKS[i] {
                if t.tid == tid { result = Some(t.capabilities); break; }
            }
        }
        result
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

/// Return the syscall filter bitmask for the currently running task.
pub fn current_syscall_filter() -> [u64; 2] {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let slot = CURRENT[cpu];
        if slot < MAX_TASKS {
            TASKS[slot].as_ref().map(|t| t.syscall_filter).unwrap_or([!0u64; 2])
        } else {
            [!0u64; 2]
        }
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

/// Set or clear individual bits in a task's syscall filter.
/// `allow_bits[i]` has bit N set ↔ syscall (64*i + N) should be allowed.
/// Returns true if the task was found.
pub fn set_task_syscall_filter(tid: usize, filter: [u64; 2]) -> bool {
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let mut ok = false;
        for i in 0..MAX_TASKS {
            if let Some(ref mut t) = TASKS[i] {
                if t.tid == tid { t.syscall_filter = filter; ok = true; break; }
            }
        }
        ok
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

/// Read the syscall filter for a task.
// ── Signal delivery helpers (1.4) ────────────────────────────────────────────

/// Send signal `sig` (bit number) to task `tid`.
/// Returns `true` if the task was found.
pub fn send_signal(tid: usize, sig: u32) -> bool {
    if sig >= 32 { return false; }
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let mut ok = false;
        for i in 0..MAX_TASKS {
            if let Some(ref mut t) = TASKS[i] {
                if t.tid == tid {
                    t.pending_signals |= 1u32 << sig;
                    // Wake a Blocked task so it can handle the signal
                    if t.state == crate::process::task::TaskState::Blocked {
                        t.state = crate::process::task::TaskState::Ready;
                    }
                    ok = true;
                    break;
                }
            }
        }
        ok
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

/// Set the signal handler address and mask for the current task.
pub fn set_signal_handler(handler: usize, mask: u32) {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    unsafe {
        let slot = CURRENT[cpu];
        if slot < MAX_TASKS {
            if let Some(ref mut t) = TASKS[slot] {
                t.signal_handler = handler;
                t.signal_mask    = mask;
            }
        }
    }
    SCHED_LOCK.release();
    irq_restore(flags);
}

/// Pop the lowest-numbered deliverable pending signal for the current task.
/// Returns `Some(sig_num)` if a signal is pending and unmasked, else `None`.
/// Clears the bit in `pending_signals`.
pub fn pop_pending_signal() -> Option<u32> {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let slot = CURRENT[cpu];
        let mut result = None;
        if slot < MAX_TASKS {
            if let Some(ref mut t) = TASKS[slot] {
                let deliverable = t.pending_signals & !t.signal_mask;
                if deliverable != 0 {
                    let sig = deliverable.trailing_zeros();
                    t.pending_signals &= !(1u32 << sig);
                    result = Some(sig);
                }
            }
        }
        result
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

/// Return the signal handler address for the current task (0 = default/none).
pub fn current_signal_handler() -> usize {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let slot = CURRENT[cpu];
        if slot < MAX_TASKS {
            TASKS[slot].as_ref().map(|t| t.signal_handler).unwrap_or(0)
        } else { 0 }
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

/// Force a task to exit (used by SIGKILL).
pub fn force_exit(tid: usize, code: i32) -> bool {
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let mut ok = false;
        for i in 0..MAX_TASKS {
            if let Some(ref mut t) = TASKS[i] {
                if t.tid == tid {
                    let my_tid = t.tid;
                    t.state     = crate::process::task::TaskState::Dead;
                    t.exit_code = code;
                    // Wake any waiter
                    for j in 0..MAX_TASKS {
                        if let Some(ref mut w) = TASKS[j] {
                            if w.wait_tid == Some(my_tid) {
                                w.wait_tid = None;
                                w.state    = crate::process::task::TaskState::Ready;
                            }
                        }
                    }
                    ok = true;
                    break;
                }
            }
        }
        ok
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

/// Swap the signal mask for the current task; return the old mask.
pub fn swap_signal_mask(new_mask: u32) -> u32 {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let old = unsafe {
        let slot = CURRENT[cpu];
        if slot < MAX_TASKS {
            if let Some(ref mut t) = TASKS[slot] {
                let prev = t.signal_mask;
                t.signal_mask = new_mask;
                prev
            } else { 0 }
        } else { 0 }
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    old
}

pub fn get_task_syscall_filter(tid: usize) -> Option<[u64; 2]> {
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let mut result = None;
        for i in 0..MAX_TASKS {
            if let Some(ref t) = TASKS[i] {
                if t.tid == tid { result = Some(t.syscall_filter); break; }
            }
        }
        result
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn set_task_caps(tid: usize, new_caps: u64) -> bool {
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let mut ok = false;
        for i in 0..MAX_TASKS {
            if let Some(ref mut t) = TASKS[i] {
                if t.tid == tid { t.capabilities = new_caps; ok = true; break; }
            }
        }
        ok
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn send_message(target_tid: usize, msg: crate::ipc::Message) -> Result<(), ()> {
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let mut ok = Err(());
        for i in 0..MAX_TASKS {
            if let Some(ref mut task) = TASKS[i] {
                if task.tid == target_tid {
                    if task.mailbox.push(msg).is_ok() {
                        if task.state == TaskState::Blocked {
                            task.state = TaskState::Ready;
                        }
                        ok = Ok(());
                    }
                    break;
                }
            }
        }
        ok
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn recv_message() -> Option<crate::ipc::Message> {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let slot = CURRENT[cpu];
        let mut result = None;
        if slot < MAX_TASKS {
            if let Some(ref mut task) = TASKS[slot] {
                if let Some(msg) = task.mailbox.pop() {
                    result = Some(msg);
                } else {
                    task.state = TaskState::Blocked;
                }
            }
        }
        result
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn try_recv_message() -> Option<crate::ipc::Message> {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let slot = CURRENT[cpu];
        let mut result = None;
        if slot < MAX_TASKS {
            if let Some(ref mut task) = TASKS[slot] {
                result = task.mailbox.pop();
            }
        }
        result
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn yield_cpu() {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    unsafe {
        let slot = CURRENT[cpu];
        if slot < MAX_TASKS {
            if let Some(ref mut task) = TASKS[slot] {
                task.state = TaskState::Ready;
            }
        }
    }
    SCHED_LOCK.release();
    irq_restore(flags);
}

pub fn exit_task(code: i32) {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    unsafe {
        let slot = CURRENT[cpu];
        let my_tid = if slot < MAX_TASKS {
            TASKS[slot].as_ref().map(|t| t.tid).unwrap_or(0)
        } else { 0 };

        if slot < MAX_TASKS {
            if let Some(ref mut task) = TASKS[slot] {
                task.state     = TaskState::Dead;
                task.exit_code = code;
            }
        }
        // Wake any waiter blocked on our TID.
        for i in 0..MAX_TASKS {
            if let Some(ref mut t) = TASKS[i] {
                if t.wait_tid == Some(my_tid) {
                    t.wait_tid = None;
                    t.state    = TaskState::Ready;
                    let frame  = t.sp as *mut TrapFrame;
                    (*frame).regs[0] = code as u64;
                }
            }
        }
    }
    SCHED_LOCK.release();
    irq_restore(flags);
}

pub fn wait_for_tid(target_tid: usize) -> Option<i32> {
    let cpu   = get_cpu_id().min(smp::MAX_CPUS - 1);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let cur_slot = CURRENT[cpu];
        let mut result = Some(-1i32); // default: not found
        for i in 0..MAX_TASKS {
            if let Some(ref t) = TASKS[i] {
                if t.tid == target_tid {
                    if t.state == TaskState::Dead {
                        let code = t.exit_code;
                        let base = t.stack_base;
                        let size = t.stack_size;
                        TASKS[i] = None;
                        if base != 0 && size != 0 {
                            memory::release_pages(base, size / memory::PAGE_SIZE);
                        }
                        result = Some(code);
                    } else {
                        if cur_slot < MAX_TASKS {
                            if let Some(ref mut cur) = TASKS[cur_slot] {
                                cur.state    = TaskState::Blocked;
                                cur.wait_tid = Some(target_tid);
                            }
                        }
                        result = None;
                    }
                    break;
                }
            }
        }
        result
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn list_tasks(out: &mut [TaskRecord]) -> usize {
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let count = unsafe {
        let mut n = 0;
        for i in 0..MAX_TASKS {
            if n >= out.len() { break; }
            if let Some(ref t) = TASKS[i] {
                out[n].tid  = t.tid as u64;
                out[n].caps = t.capabilities;
                out[n].name = t.name;
                out[n]._pad = 0;
                n += 1;
            }
        }
        n
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    count
}

/// Pin `tid` to a specific CPU.  Pass `cpu >= MAX_CPUS` to clear the pin
/// (make the task migratable).  Returns false if the TID is not found.
pub fn set_affinity(tid: usize, cpu: usize) -> bool {
    let pin = if cpu < smp::MAX_CPUS { Some(cpu) } else { None };
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let mut ok = false;
        for i in 0..MAX_TASKS {
            if let Some(ref mut t) = TASKS[i] {
                if t.tid == tid { t.cpu_pin = pin; ok = true; break; }
            }
        }
        ok
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

pub fn set_task_priority(tid: usize, priority: u8) {
    let p     = priority.max(1).min(8);
    let flags = irq_save();
    SCHED_LOCK.acquire();
    unsafe {
        for i in 0..MAX_TASKS {
            if let Some(ref mut t) = TASKS[i] {
                if t.tid == tid { t.priority = p; break; }
            }
        }
    }
    SCHED_LOCK.release();
    irq_restore(flags);
}

pub fn task_ticks(tid: usize) -> (u64, u8) {
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let v = unsafe {
        let mut result = (0u64, 1u8);
        for i in 0..MAX_TASKS {
            if let Some(ref t) = TASKS[i] {
                if t.tid == tid { result = (t.ticks_used, t.priority); break; }
            }
        }
        result
    };
    SCHED_LOCK.release();
    irq_restore(flags);
    v
}

// ── Statistics ────────────────────────────────────────────────────────────────

/// Snapshot info for a running task — used by SYS_LIST_TASKS.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct TaskRecord {
    pub tid:  u64,
    pub caps: u64,
    pub name: [u8; 16],
    pub _pad: u64,
}

pub fn task_count() -> usize {
    let flags = irq_save();
    SCHED_LOCK.acquire();
    let n = unsafe { TASKS.iter().filter(|t| t.is_some()).count() };
    SCHED_LOCK.release();
    irq_restore(flags);
    n
}

pub fn idle_ticks()  -> usize { IDLE_TICKS.load(Ordering::Relaxed) }
pub fn total_ticks() -> usize { TOTAL_TICKS.load(Ordering::Relaxed) }
