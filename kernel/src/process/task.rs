/// Remoboth — Task structures.
///
/// Each task has a saved stack pointer (pointing to its trap frame)
/// and a state machine (Ready, Running, Blocked, Dead).

/// Task states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    #[allow(dead_code)]
    Blocked,
    Dead,
}

/// Task privilege level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPrivilege {
    /// Runs in EL1 (kernel mode). Full hardware access.
    Kernel,
    /// Runs in EL0 (user mode). Restricted to granted capabilities.
    User,
}

/// Maximum length of a task name.
const NAME_LEN: usize = 16;

/// Trap frame layout — matches SAVE_REGS/RESTORE_REGS in exceptions.rs.
/// All 31 general-purpose registers + ELR_EL1 + SPSR_EL1 + SP_EL0.
///
/// Offsets:
///   [0..240]  x0-x30   (31 × 8 bytes)
///   [248]     ELR_EL1
///   [256]     SPSR_EL1
///   [264]     SP_EL0   (user stack pointer, 0 for kernel tasks)
///   [272]     padding  (16-byte alignment)
#[repr(C)]
pub struct TrapFrame {
    pub regs:   [u64; 31], // x0–x30
    pub elr:    u64,       // Exception Link Register (return PC)
    pub spsr:   u64,       // Saved Program Status Register
    pub sp_el0: u64,       // User-mode stack pointer (SP_EL0)
    pub _pad:   u64,       // Padding for 16-byte frame alignment
}

/// Size of TrapFrame in bytes (must stay 16-byte aligned).
/// 31×8 + 8 + 8 + 8 + 8 = 288 bytes.
pub const TRAP_FRAME_SIZE: usize = 288;

/// Task Control Block.
pub struct Task {
    pub tid:          usize,
    pub name:         [u8; NAME_LEN],
    pub state:        TaskState,
    pub sp:           u64,        // Saved kernel stack pointer (points to TrapFrame)
    pub stack_base:   usize,      // Kernel stack base address
    pub stack_size:   usize,      // Kernel stack size in bytes
    pub mailbox:      crate::ipc::Mailbox,
    pub privilege:    TaskPrivilege,
    pub capabilities: u64,        // Capability bitmask (CAP_* constants)
    pub ttbr0:        usize,      // Page table base (TTBR0_EL1 value for this task)
    pub exit_code:    i32,        // Exit code stored when state transitions to Dead
    pub wait_tid:     Option<usize>, // TID we are Blocked waiting for (SYS_WAIT)
    /// Lowest VA in this task's ELF code/data segments (for TLB flush).
    pub user_va_base: usize,
    /// Highest VA + 1 in this task's ELF code/data segments (exclusive).
    pub user_va_top:  usize,
    /// User stack base (low address, for TLB flush).
    pub user_stack_base: usize,
    /// User stack top (high address = initial SP, for TLB flush).
    pub user_stack_top:  usize,

    // ── AI-informed scheduling ──────────────────────────────────────────────
    /// Number of consecutive timer ticks this task gets per scheduling round.
    /// 1 = normal, 2 = elevated, 3 = high (set by Numenor via SYS_SET_PRIORITY).
    pub priority:        u8,
    /// Countdown within the current quantum; preempted when it reaches 0.
    pub ticks_remaining: u8,
    /// Cumulative timer ticks charged to this task (for AI observation).
    pub ticks_used:      u64,

    // ── SMP affinity ────────────────────────────────────────────────────────
    /// Hard CPU pin.  `None` = migratable (any CPU may run this task).
    /// `Some(n)` = only CPU n may run this task; other CPUs skip it.
    /// Use `scheduler::set_affinity(tid, cpu)` to set after spawn.
    pub cpu_pin: Option<usize>,

    // ── Per-task syscall allowlist (seccomp-like, 7.4) ───────────────────────
    /// 128-bit bitmap (2 × u64) where bit N = syscall N is permitted.
    /// Bit 0 of syscall_filter[0] = syscall 0, bit 63 = syscall 63,
    /// bit 0 of syscall_filter[1] = syscall 64, …
    /// All-ones (`[!0, !0]`) means "allow everything" (default for new tasks).
    pub syscall_filter: [u64; 2],

    // ── Async signals (POSIX-like, 1.4) ─────────────────────────────────────
    /// Bitmask of pending signals (bit 0 = SIGTERM, bit 1 = SIGKILL,
    /// bit 2 = SIGUSR1, bit 3 = SIGUSR2, bit 4 = SIGCHLD, etc.).
    pub pending_signals: u32,
    /// Bitmask of blocked (masked) signals.  Pending & ~blocked are deliverable.
    pub signal_mask:     u32,
    /// EL0 user-space signal trampoline address (set by SYS_SIGACTION).
    /// When non-zero and a signal fires, the task is redirected to this address
    /// on next return-to-EL0.  x0 = signal number.
    pub signal_handler:  usize,
}

impl Task {
    /// Create a new **kernel** task (EL1h, IRQs enabled, full capabilities).
    pub fn new(tid: usize, name: &str, entry: usize, stack_base: usize, stack_size: usize) -> Self {
        let mut name_buf = [0u8; NAME_LEN];
        let len = name.len().min(NAME_LEN - 1);
        name_buf[..len].copy_from_slice(&name.as_bytes()[..len]);

        // Place a fake trap frame at the top of the kernel stack.
        //
        // After the TTBR1 split, the kernel stack PA must be accessed via
        // TTBR1 (high VA = PA + KERNEL_VA_OFFSET) so that the exception
        // trampolines can push/pop stack frames regardless of which TTBR0
        // is currently installed (identity or a user-only per-process table).
        let kva_base  = crate::memory::vmm::phys_to_virt(stack_base);
        let stack_top = (kva_base + stack_size) & !0xF; // 16-byte aligned
        let frame_sp  = stack_top - TRAP_FRAME_SIZE;

        let frame = frame_sp as *mut TrapFrame;
        unsafe {
            (*frame).regs   = [0u64; 31];
            (*frame).elr    = entry as u64;
            // EL1h with all DAIF bits clear (IRQs enabled): SPSR = 0x5
            (*frame).spsr   = 0x5;
            (*frame).sp_el0 = 0; // Unused for kernel tasks
            (*frame)._pad   = 0;
        }

        Task {
            tid,
            name:         name_buf,
            state:        TaskState::Ready,
            sp:           frame_sp as u64,
            stack_base,
            stack_size,
            mailbox:      crate::ipc::Mailbox::new(),
            privilege:    TaskPrivilege::Kernel,
            capabilities: !0u64, // All capabilities
            ttbr0:        crate::memory::vmm::ttbr0(),
            exit_code:    0,
            wait_tid:     None,
            user_va_base:    0,
            user_va_top:     0,
            user_stack_base: 0,
            user_stack_top:  0,
            priority:        1,
            ticks_remaining: 1,
            ticks_used:      0,
            cpu_pin:         None,
            syscall_filter:  [!0u64; 2], // Allow all syscalls by default
            pending_signals: 0,
            signal_mask:     0,
            signal_handler:  0,
        }
    }

    /// Create a new **user** task (EL0t, IRQs enabled in EL0).
    ///
    /// * `entry`               — user-mode entry point
    /// * `kernel_stack_*`      — stack used by the kernel when this task traps
    /// * `user_sp`             — initial user-mode stack pointer (SP_EL0)
    /// * `caps`                — granted capability bitmask
    /// * `ttbr0`               — user page-table base address
    /// * `user_va_base/top`    — ELF code/data VA range (for TLB flush, kept small)
    /// * `user_stack_base/top` — user stack VA range (flushed separately from ELF range)
    #[allow(clippy::too_many_arguments)]
    pub fn new_user(
        tid:          usize,
        name:         &str,
        entry:        usize,
        kernel_stack_base: usize,
        kernel_stack_size: usize,
        user_sp:      usize,
        caps:         u64,
        ttbr0:        usize,
        user_va_base:   usize,
        user_va_top:    usize,
        user_stack_base: usize,
        user_stack_top:  usize,
    ) -> Self {
        let mut name_buf = [0u8; NAME_LEN];
        let len = name.len().min(NAME_LEN - 1);
        name_buf[..len].copy_from_slice(&name.as_bytes()[..len]);

        // Fake trap frame on top of the kernel stack.
        //
        // After the TTBR1 kernel/user VA split the kernel stack PA is only
        // accessible via TTBR1 (high VA = PA + KERNEL_VA_OFFSET).  When
        // this task is first scheduled, the hardware switches TTBR0 to the
        // per-process user table *before* RESTORE_REGS runs.  If task.sp
        // held a raw PA the restore load would go through TTBR0 (bit 63
        // clear), which is not mapped in the user table → Data Abort.
        //
        // Using the high-VA alias (PA + KERNEL_VA_OFFSET) routes all kernel
        // stack accesses through TTBR1, regardless of which TTBR0 is active.
        // task.stack_base intentionally stays as the PA so release_pages()
        // receives the correct physical address later.
        let kva_base  = crate::memory::vmm::phys_to_virt(kernel_stack_base);
        let stack_top = (kva_base + kernel_stack_size) & !0xF;
        let frame_sp  = stack_top - TRAP_FRAME_SIZE;

        let frame = frame_sp as *mut TrapFrame;
        unsafe {
            (*frame).regs   = [0u64; 31];
            (*frame).elr    = entry as u64;
            // EL0t with DAIF.I clear (IRQs enabled at EL0): SPSR = 0x0
            (*frame).spsr   = 0x0;
            (*frame).sp_el0 = user_sp as u64;
            (*frame)._pad   = 0;
        }

        Task {
            tid,
            name:         name_buf,
            state:        TaskState::Ready,
            sp:           frame_sp as u64,
            stack_base:   kernel_stack_base,
            stack_size:   kernel_stack_size,
            mailbox:      crate::ipc::Mailbox::new(),
            privilege:    TaskPrivilege::User,
            capabilities: caps,
            ttbr0,
            exit_code:    0,
            wait_tid:     None,
            user_va_base,
            user_va_top,
            user_stack_base,
            user_stack_top,
            priority:        1,
            ticks_remaining: 1,
            ticks_used:      0,
            cpu_pin:         None,
            syscall_filter:  [!0u64; 2], // Allow all syscalls by default
            pending_signals: 0,
            signal_mask:     0,
            signal_handler:  0,
        }
    }

    /// Get the task name as a string slice.
    pub fn name_str(&self) -> &str {
        let len = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        unsafe { core::str::from_utf8_unchecked(&self.name[..len]) }
    }
}
