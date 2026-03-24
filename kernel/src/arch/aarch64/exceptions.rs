/// AArch64 exception vector table and handlers.
///
/// The vector table has 16 entries (4 groups x 4 types), each 0x80 (128) bytes.
/// We only care about IRQs from the current EL with SP_ELx for now.
///
/// The IRQ handler saves all registers (x0-x30, ELR_EL1, SPSR_EL1) to the
/// stack as a trap frame, handles the interrupt, then optionally switches
/// to a different task's stack before restoring and returning via eret.

use core::arch::global_asm;

/// Install the exception vector table by writing its address to VBAR_EL1.
pub fn init() {
    extern "C" {
        static __vectors: u8;
    }
    unsafe {
        let vbar = &__vectors as *const u8 as u64;
        core::arch::asm!("msr vbar_el1, {}", in(reg) vbar);
        core::arch::asm!("isb");
    }
}

/// Enable IRQ interrupts by clearing the I bit in DAIF.
pub fn enable_irqs() {
    unsafe {
        core::arch::asm!("msr daifclr, #2");
    }
}

/// Disable IRQ interrupts by setting the I bit in DAIF.
pub fn disable_irqs() {
    unsafe {
        core::arch::asm!("msr daifset, #2");
    }
}

/// Allow FP/SIMD instructions at EL0 and EL1 by setting CPACR_EL1.FPEN = 0b11.
///
/// Without this, any FP/SIMD instruction at EL0 (including compiler-generated
/// callee-save of d-registers in function prologues) causes a SIMD/FP Access
/// trap (EC=0x07 or 0x1C), which crashes the task silently.
pub fn enable_fp() {
    unsafe {
        let mut cpacr: u64;
        core::arch::asm!("mrs {}, cpacr_el1", out(reg) cpacr);
        cpacr |= 3 << 20; // FPEN[21:20] = 0b11: no FP/SIMD trap at EL0 or EL1
        core::arch::asm!("msr cpacr_el1, {}", in(reg) cpacr);
        core::arch::asm!("isb");
    }
}

/// Called from the FIQ vector.  On Apple Silicon the virtual timer fires as
/// FIQ; on QEMU/GIC it fires as SPI #27 through the IRQ path.
/// Dispatches to the AIC FIQ handler when AIC is active; otherwise no-op.
#[no_mangle]
pub extern "C" fn fiq_handler(sp: u64) -> u64 {
    // Delegate to AIC FIQ handler (which checks cntv_ctl_el0.ISTATUS).
    // On QEMU this function is still reachable if anything raises a FIQ, but
    // the timer ISTATUS will be clear (timer fires via IRQ on QEMU), so it's
    // a fast return.
    super::aic::fiq_handler_inner(sp)
}

/// Called from the IRQ vector. Handles the interrupt and returns the
/// stack pointer to use for restoring context (may be different if
/// a task switch occurred).
#[no_mangle]
extern "C" fn irq_handler(sp: u64) -> u64 {
    use super::gic;
    use super::timer;

    let irq = gic::acknowledge();

    if irq == 1023 {
        return sp; // Spurious
    }

    // Send EOI early — timer handler may switch tasks
    gic::end_of_interrupt(irq);

    match irq {
        27 => timer::handle_irq(sp),
        _ => {
            let uart = crate::drivers::uart::Uart::new();
            uart.puts("[irq] Unhandled IRQ: ");
            uart.put_dec(irq as usize);
            uart.puts("\r\n");
            sp
        }
    }
}

// Exception vector table + save/restore stubs in assembly.
//
// Trap frame layout (288 bytes, 16-byte aligned):
//   [sp, #0]   .. [sp, #240] : x0 - x30 (31 regs × 8 bytes)
//   [sp, #248]               : ELR_EL1
//   [sp, #256]               : SPSR_EL1
//   [sp, #264]               : SP_EL0  (user stack pointer)
//   [sp, #272]               : padding (16-byte alignment)
global_asm!(
    r#"
.section .text

.macro SAVE_REGS
    sub     sp, sp, #288
    stp     x0,  x1,  [sp, #0]
    stp     x2,  x3,  [sp, #16]
    stp     x4,  x5,  [sp, #32]
    stp     x6,  x7,  [sp, #48]
    stp     x8,  x9,  [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30,      [sp, #240]
    mrs     x21, elr_el1
    mrs     x22, spsr_el1
    stp     x21, x22, [sp, #248]
    mrs     x23, sp_el0
    str     x23,      [sp, #264]
.endm

.macro RESTORE_REGS
    ldp     x21, x22, [sp, #248]
    msr     elr_el1,  x21
    msr     spsr_el1, x22
    ldr     x23,      [sp, #264]
    msr     sp_el0,   x23
    ldp     x0,  x1,  [sp, #0]
    ldp     x2,  x3,  [sp, #16]
    ldp     x4,  x5,  [sp, #32]
    ldp     x6,  x7,  [sp, #48]
    ldp     x8,  x9,  [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30,      [sp, #240]
    add     sp, sp, #288
.endm

// Vector table must be 2048-byte aligned (0x800)
.balign 0x800
.global __vectors
__vectors:

// ─── Current EL, SP_EL0 ───
// Synchronous
.balign 0x80
    b       .unhandled_exception
// IRQ
.balign 0x80
    b       .unhandled_exception
// FIQ
.balign 0x80
    b       .unhandled_exception
// SError
.balign 0x80
    b       .unhandled_exception

// ─── Current EL, SP_ELx ─── (this is the one we use)
// Synchronous
.balign 0x80
    b       sync_trampoline
// IRQ
.balign 0x80
    b       irq_trampoline
// FIQ — on Apple Silicon the virtual timer fires here instead of as SPI #27
.balign 0x80
    b       fiq_trampoline
// SError
.balign 0x80
    b       .unhandled_exception

// ─── Lower EL, AArch64 ─── (user tasks running at EL0)
// Synchronous (SVC, page faults, etc.)
.balign 0x80
    b       sync_trampoline
// IRQ
.balign 0x80
    b       irq_trampoline
// FIQ
.balign 0x80
    b       fiq_trampoline
// SError
.balign 0x80
    b       .unhandled_exception

// ─── Lower EL, AArch32 ───
// Synchronous
.balign 0x80
    b       .unhandled_exception
// IRQ
.balign 0x80
    b       .unhandled_exception
// FIQ
.balign 0x80
    b       .unhandled_exception
// SError
.balign 0x80
    b       .unhandled_exception

sync_trampoline:
    SAVE_REGS
    mov     x0, sp
    bl      sync_handler
    mov     sp, x0
    // Safety: check ELR in trap frame
    ldr     x1, [sp, #248]
    cbnz    x1, 2f
    bl      bad_eret_handler
2:
    RESTORE_REGS
    eret

irq_trampoline:
    SAVE_REGS
    mov     x0, sp              // Pass current SP (trap frame pointer) to handler
    bl      irq_handler         // Returns SP to use for restore (may be different task)
    mov     sp, x0              // Use returned SP (possibly switched task)
    // Safety: check ELR in trap frame before restoring — catch bad context switch
    ldr     x1, [sp, #248]      // Load ELR from trap frame
    cbnz    x1, 1f              // If non-zero, proceed normally
    // ELR is 0 — bad context switch! Fall into unhandled handler
    bl      bad_eret_handler
1:
    RESTORE_REGS
    eret

fiq_trampoline:
    SAVE_REGS
    mov     x0, sp
    bl      fiq_handler
    mov     sp, x0
    ldr     x1, [sp, #248]
    cbnz    x1, 3f
    bl      bad_eret_handler
3:
    RESTORE_REGS
    eret

.unhandled_exception:
    SAVE_REGS
    bl      unhandled_exception_handler
    RESTORE_REGS
    eret
"#
);

#[no_mangle]
extern "C" fn sync_handler(sp: u64) -> u64 {
    let esr: u64;
    unsafe { core::arch::asm!("mrs {}, esr_el1", out(reg) esr); }
    let ec = (esr >> 26) & 0x3F;

    // EC 0x15 = SVC instruction execution in AArch64 state
    if ec == 0x15 {
        let tf = unsafe { &mut *(sp as *mut crate::process::task::TrapFrame) };
        let syscall_num = tf.regs[8] as usize; // x8
        let arg0 = tf.regs[0] as usize;
        let arg1 = tf.regs[1] as usize;
        let arg2 = tf.regs[2] as usize;
        let arg3 = tf.regs[3] as usize;
        let arg4 = tf.regs[4] as usize;
        let arg5 = tf.regs[5] as usize;

        let ret = crate::syscall::dispatch(syscall_num, arg0, arg1, arg2, arg3, arg4, arg5);

        tf.regs[0] = ret as u64;
        // Note: On AArch64, ELR_EL1 for SVC already points to the instruction
        // AFTER the SVC (the "preferred return address"). No need to advance.

        // Signal delivery (1.4): check for pending signals on return to EL0.
        // Only deliver to User tasks (SPSR bit 0 clear → EL0).
        if tf.spsr & 0xF == 0 {
            if let Some(signum) = crate::process::scheduler::pop_pending_signal() {
                let handler = crate::process::scheduler::current_signal_handler();
                if handler != 0 {
                    // Redirect: save interrupted return address in x30 (LR),
                    // put signal number in x0, jump to handler.
                    // The handler calls SYS_SIGRETURN or `ret` to resume via x30.
                    tf.regs[30] = tf.elr; // save return PC in LR
                    tf.regs[0]  = signum as u64; // signal number in x0
                    tf.elr      = handler as u64; // redirect to handler
                }
            }
        }

        // Call schedule_next so that blocking syscalls (SYS_WAIT, SYS_EXIT)
        // immediately switch to another task rather than returning to a Dead
        // or Blocked task.  If no switch is needed, schedule_next returns the
        // same SP unchanged.
        return crate::process::scheduler::schedule_next(sp);
    }

    // EC 0x20 = Instruction Abort from lower EL (EL0 instruction fetch fault)
    // EC 0x24 = Data Abort from lower EL (EL0 data access fault)
    //
    // These are EL0 page faults — kill the offending task and schedule the next one.
    if ec == 0x20 || ec == 0x24 {
        let far: u64;
        unsafe { core::arch::asm!("mrs {}, far_el1", out(reg) far); }
        let tf = unsafe { &*(sp as *const crate::process::task::TrapFrame) };
        let uart = crate::drivers::uart::Uart::new();
        uart.puts("\n[pgfault] ");
        uart.puts(if ec == 0x20 { "Instruction Abort" } else { "Data Abort" });
        uart.puts(" from EL0\n  FAR=");
        uart.put_hex(far as usize);
        uart.puts(" ELR=");
        uart.put_hex(tf.elr as usize);
        uart.puts(" ESR=");
        uart.put_hex(esr as usize);
        uart.puts("\n  TID=");
        uart.put_dec(crate::process::current_tid());
        uart.puts(" DFSC/IFSC=");
        uart.put_hex((esr & 0x3F) as usize);
        uart.puts("\n");
        crate::process::scheduler::exit_task(-11); // -11 = SIGSEGV
        return crate::process::scheduler::schedule_next(sp);
    }

    let uart = crate::drivers::uart::Uart::new();
    uart.puts("\n!!! UNHANDLED SYNC EXCEPTION !!!\n");
    uart.puts("  ESR_EL1: ");
    uart.put_hex(esr as usize);
    uart.puts("  EC: ");
    uart.put_hex(ec as usize);

    // Read ELR/FAR from trap frame (more reliable than system regs)
    let tf = unsafe { &*(sp as *const crate::process::task::TrapFrame) };
    uart.puts("\n  ELR(tf): ");
    uart.put_hex(tf.elr as usize);
    uart.puts("  SPSR(tf): ");
    uart.put_hex(tf.spsr as usize);
    uart.puts("\n  x30(tf): ");
    uart.put_hex(tf.regs[30] as usize);
    uart.puts("  SP(frame): ");
    uart.put_hex(sp as usize);

    let far: u64;
    unsafe { core::arch::asm!("mrs {}, far_el1 ", out(reg) far); }
    uart.puts("\n  FAR_EL1: ");
    uart.put_hex(far as usize);

    // Current task info
    uart.puts("\n  Task slot: ");
    uart.put_dec(crate::process::scheduler::current_slot());
    uart.puts("  TID: ");
    uart.put_dec(crate::process::current_tid());
    uart.puts("\n");
    loop { core::hint::spin_loop(); }
}

#[no_mangle]
extern "C" fn bad_eret_handler() {
    let uart = crate::drivers::uart::Uart::new();
    uart.puts("\n!!! BAD ERET: ELR=0 in trap frame !!!\n");
    uart.puts("  Task slot: ");
    uart.put_dec(crate::process::scheduler::current_slot());
    uart.puts("  TID: ");
    uart.put_dec(crate::process::current_tid());
    uart.puts("\r\n");
    loop { core::hint::spin_loop(); }
}

#[no_mangle]
extern "C" fn unhandled_exception_handler() {
    let uart = crate::drivers::uart::Uart::new();
    let esr: u64;
    unsafe { core::arch::asm!("mrs {}, esr_el1 ", out(reg) esr); }
    let elr: u64;
    unsafe { core::arch::asm!("mrs {}, elr_el1 ", out(reg) elr); }
    let far: u64;
    unsafe { core::arch::asm!("mrs {}, far_el1 ", out(reg) far); }

    uart.puts("\nUNHANDLED EXCEPTION\n");
    uart.puts("ESR: ");
    uart.put_hex(esr as usize);
    uart.puts("\nELR: ");
    uart.put_hex(elr as usize);
    uart.puts("\nFAR: ");
    uart.put_hex(far as usize);
    uart.puts("\n");

    loop {
        core::hint::spin_loop();
    }
}
