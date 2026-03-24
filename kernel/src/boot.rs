/// AArch64 boot sequence — TTBR1 kernel/user VA split.
///
/// The kernel is now linked at VMA = PA + KERNEL_VA_OFFSET, so the boot code
/// must:
///   1. Run entirely at low PA (in .text.boot, VMA == LMA) until the MMU is on.
///   2. Use PA-value linker symbols (__bss_start_pa, __bss_end_pa) instead of
///      the high-VA __bss_start / __bss_end, which are unusable pre-MMU.
///   3. Build minimal boot page tables:
///        BOOT_TTBR0_L0 — identity map (low VA = PA): MMIO + RAM
///        BOOT_TTBR1_L0 — high-VA map  (VA = PA + KERNEL_VA_OFFSET): MMIO + RAM
///      Both tables share the same L1/L2 subtables (AArch64 entries store PAs).
///   4. Enable the MMU with both TTBR0 and TTBR1.
///   5. Switch SP to high VA.
///   6. Jump to kernel_main at its high VMA via indirect branch.
///
/// After this, EL1 kernel code always runs via TTBR1 (high VA).  TTBR0 holds
/// the kernel identity table for kernel tasks, or a per-process user table for
/// EL0 tasks.
///
/// Register state at entry (QEMU -kernel / GRUB / UEFI):
///   x0 = physical address of DTB, or 0
///   x1-x3 = 0 (reserved)

use core::arch::global_asm;

global_asm!(
    r#"
// ── ARM64 Linux Image Header ────────────────────────────────────────────────
// 64 bytes at _start per Documentation/arm64/booting.rst.
// Bootloaders validate the header; QEMU -kernel executes from byte 0 (b).
.section .text.boot, "ax"
.global _start

_start:
    b       .Lstart_real             // code0: branch past header
    .long   0                        // code1
    .quad   0                        // text_offset (0 = any)
    .quad   0                        // image_size  (0 = auto)
    .quad   0x000a                   // flags: LE | 4K | anywhere
    .quad   0                        // res2
    .quad   0                        // res3
    .quad   0                        // res4
    .long   0x644d5241               // magic = "ARM\x64"
    .long   0                        // res5

// ── Boot entry ──────────────────────────────────────────────────────────────
.Lstart_real:
    // Save DTB pointer (x0) into a callee-saved register.
    mov     x19, x0

    // Park all cores except core 0.
    mrs     x0, mpidr_el1
    and     x0, x0, #0xFF
    cbnz    x0, .Lpark

    // Disable MMU, D-cache, I-cache (clean state after soft-reboot).
    mrs     x1, sctlr_el1
    bic     x1, x1, #(1 << 0)       // M  = MMU off
    bic     x1, x1, #(1 << 2)       // C  = D-cache off
    bic     x1, x1, #(1 << 12)      // I  = I-cache off
    msr     sctlr_el1, x1
    isb

    // Invalidate all TLBs (safe before MMU on).
    tlbi    vmalle1is
    dsb     ish
    isb

    // ── Boot stack at PA (grows down from load base 0x40080000) ──
    ldr     x0, =__stack_top_pa
    mov     sp, x0

    // ── Zero BSS (PA addresses — symbols are absolute values) ────
    ldr     x0, =__bss_start_pa
    ldr     x1, =__bss_end_pa
.Lzero_bss:
    cmp     x0, x1
    b.ge    .Lbss_done
    str     xzr, [x0], #8
    b       .Lzero_bss
.Lbss_done:

    // ── Enable FP/SIMD (required by ABI; llama.cpp uses NEON) ────
    mrs     x0, cpacr_el1
    orr     x0, x0, #(3 << 20)      // FPEN = 11: EL0+EL1 access
    msr     cpacr_el1, x0
    isb

    // ── Build boot page tables (low-PA subroutine) ────────────────
    bl      .Lbuild_boot_tables

    // ── Configure MMU ─────────────────────────────────────────────
    // MAIR_EL1:
    //   Attr0 = 0xFF: Normal, Write-Back Write-Allocate (inner+outer)
    //   Attr1 = 0x00: Device-nGnRnE (strongly ordered)
    mov     x0, #0xFF
    msr     mair_el1, x0

    // TCR_EL1:
    //   T0SZ=16  [5:0]    — 48-bit TTBR0 VA space
    //   IRGN0=01 [9:8]    — Normal, inner write-back cacheable
    //   ORGN0=01 [11:10]  — Normal, outer write-back cacheable
    //   SH0=11   [13:12]  — Inner shareable
    //   TG0=00   [15:14]  — 4 KB granule
    //   T1SZ=16  [21:16]  — 48-bit TTBR1 VA space
    //   EPD1=0   [23]     — ENABLE TTBR1 walks (was 1 before split)
    //   IRGN1=01 [25:24]  — Normal, inner write-back cacheable
    //   ORGN1=01 [27:26]  — Normal, outer write-back cacheable
    //   SH1=11   [29:28]  — Inner shareable
    //   TG1=10   [31:30]  — 4 KB granule for TTBR1
    //   IPS=010  [34:32]  — 40-bit physical address space
    //
    // Low 32 bits = 0xB510_3510
    // High 32 bits = 0x0000_0002
    movz    x0, #0x3510
    movk    x0, #0xB510, lsl #16
    movk    x0, #0x0002, lsl #32
    msr     tcr_el1, x0

    // TTBR0_EL1 — identity-map kernel table (low VA = PA).
    adr     x0, BOOT_TTBR0_L0
    msr     ttbr0_el1, x0

    // TTBR1_EL1 — high-VA kernel table (VA = PA + KERNEL_VA_OFFSET).
    adr     x0, BOOT_TTBR1_L0
    msr     ttbr1_el1, x0

    dsb     sy
    isb

    // Enable MMU, D-cache, I-cache.
    mrs     x0, sctlr_el1
    orr     x0, x0, #(1 << 0)       // M = MMU enable
    orr     x0, x0, #(1 << 2)       // C = D-cache enable
    orr     x0, x0, #(1 << 12)      // I = I-cache enable
    msr     sctlr_el1, x0
    isb

    // MMU is now on.  .text.boot instructions still execute from low PA
    // (TTBR0 identity map).  kernel_main is at a high VMA — we reach it
    // via indirect branch using a literal stored in .text.boot.

    // Switch SP to high VA: SP = SP + 0xFFFF_0000_0000_0000
    movz    x0, #0xFFFF, lsl #48
    add     sp, sp, x0

    // Jump to kernel_main at high VMA.  x19 = DTB pointer.
    mov     x0, x19
    ldr     x1, .Lkernel_main_addr
    blr     x1

    // kernel_main must not return; park if it does.
    b       .Lpark

.align 3
.Lkernel_main_addr:
    .quad   kernel_main

.Lpark:
    wfe
    b       .Lpark


// ── Secondary-core entry point ───────────────────────────────────────────────
//
// Called by PSCI CPU_ON (or woken from WFE on spin-table machines).
// Runs at low PA (identity-mapped via BOOT_TTBR0_L0).
//
// The primary has already:
//   • built BOOT_TTBR0_L0 / BOOT_TTBR1_L0 (shared)
//   • written SMP_STACK_TOPS[cpu_id - 1] = high-VA stack top
//
// This routine:
//   1. Reads CPU ID from MPIDR_EL1[7:0].
//   2. Disables caches (safe on a fresh secondary).
//   3. Enables FP/SIMD.
//   4. Configures MAIR_EL1, TCR_EL1, TTBR0_EL1, TTBR1_EL1.
//   5. Enables the MMU — identical to primary.
//   6. Loads SP from SMP_STACK_TOPS[cpu_id - 1] via TTBR1 (high VA).
//   7. Indirect-branches to secondary_main(cpu_id) at high VMA.
//
// No stack is needed in steps 1-6 (pure register manipulation).

.global _secondary_start
_secondary_start:
    // Identify this CPU.
    mrs     x20, mpidr_el1
    and     x20, x20, #0xFF             // x20 = cpu_id (1, 2, 3 …)

    // Disable D-cache + I-cache + MMU (in case firmware left them on).
    mrs     x0, sctlr_el1
    bic     x0, x0, #(1 << 0)           // M  = MMU off
    bic     x0, x0, #(1 << 2)           // C  = D-cache off
    bic     x0, x0, #(1 << 12)          // I  = I-cache off
    msr     sctlr_el1, x0
    isb

    // Invalidate TLBs.
    tlbi    vmalle1is
    dsb     ish
    isb

    // Enable FP/SIMD.
    mrs     x0, cpacr_el1
    orr     x0, x0, #(3 << 20)
    msr     cpacr_el1, x0
    isb

    // MAIR_EL1 — same as primary.
    mov     x0, #0xFF
    msr     mair_el1, x0

    // TCR_EL1 — same as primary.
    movz    x0, #0x3510
    movk    x0, #0xB510, lsl #16
    movk    x0, #0x0002, lsl #32
    msr     tcr_el1, x0

    // TTBR0 — identity-map boot table (shared with primary).
    adr     x0, BOOT_TTBR0_L0
    msr     ttbr0_el1, x0

    // TTBR1 — high-VA kernel table (shared with primary).
    adr     x0, BOOT_TTBR1_L0
    msr     ttbr1_el1, x0

    dsb     sy
    isb

    // Enable MMU + D-cache + I-cache.
    mrs     x0, sctlr_el1
    orr     x0, x0, #(1 << 0)
    orr     x0, x0, #(1 << 2)
    orr     x0, x0, #(1 << 12)
    msr     sctlr_el1, x0
    isb

    // MMU is now on.  TTBR0 identity-maps low PA (this code still runs here).
    // TTBR1 makes all high-VA kernel addresses accessible.

    // Load high-VA stack top from SMP_STACK_TOPS[cpu_id - 1].
    //   .Lsmp_stack_tops stores the high-VA *address* of the static array.
    //   Loading from that address (via TTBR1) gives us the stack top value.
    ldr     x1, .Lsmp_stack_tops        // x1 = &SMP_STACK_TOPS (high VA)
    sub     x2, x20, #1                 // cpu_id - 1
    lsl     x2, x2, #3                  // * 8 (usize = 8 bytes)
    ldr     x3, [x1, x2]               // x3 = SMP_STACK_TOPS[cpu_id - 1]
    mov     sp, x3                      // set stack pointer

    // Branch to secondary_main(cpu_id) at high VMA.
    mov     x0, x20                     // arg0 = cpu_id
    ldr     x1, .Lsecondary_main_addr
    blr     x1

    // Should never return.
    b       .Lpark

.align 3
.Lsmp_stack_tops:
    .quad   SMP_STACK_TOPS              // high-VA address of the Rust static
.Lsecondary_main_addr:
    .quad   secondary_main


// ── Boot page table builder ──────────────────────────────────────────────────
//
// Fills 8 static page tables in .boot_tables (low PA, pre-MMU accessible).
// Both TTBR0 (identity) and TTBR1 (VA+OFFSET) share the same L1/L2 trees
// because AArch64 page table entries store physical output addresses.
//
// Register usage: x0-x13 clobbered; x30 = return address (from bl).
//
// Layout built:
//   BOOT_TTBR0_L0[0] → BOOT_L1 (PA)
//   BOOT_TTBR1_L0[0] → BOOT_L1 (PA)  ← same physical L1 table
//   BOOT_L1[0] → BOOT_L2_MMIO  (first GB, device blocks)
//   BOOT_L1[1] → BOOT_L2_RAM_1 (0x4000_0000–0x7FFF_FFFF)
//   BOOT_L1[2] → BOOT_L2_RAM_2 (0x8000_0000–0xBFFF_FFFF)
//   BOOT_L1[3] → BOOT_L2_RAM_3 (0xC000_0000–0xFFFF_FFFF)
//   BOOT_L1[4] → BOOT_L2_RAM_4 (0x1_0000_0000–0x1_3FFF_FFFF)
//   BOOT_L2_MMIO[64..96] → device 2 MiB blocks (0x0800_0000–0x0C00_0000)
//   BOOT_L2_RAM_1..4[0..512] → normal 2 MiB blocks (kernel RAM)

.Lbuild_boot_tables:
    // Get PAs of all eight tables via ADR (PC-relative → exact PA pre-MMU).
    adr     x0,  BOOT_TTBR0_L0
    adr     x1,  BOOT_TTBR1_L0
    adr     x2,  BOOT_L1
    adr     x3,  BOOT_L2_MMIO
    adr     x4,  BOOT_L2_RAM_1
    adr     x5,  BOOT_L2_RAM_2
    adr     x6,  BOOT_L2_RAM_3
    adr     x7,  BOOT_L2_RAM_4

    // Zero all 8 tables (8 × 4096 = 32768 bytes).
    mov     x8,  x0
    add     x9,  x0, #(8 * 4096)
.Lzero_tables:
    stp     xzr, xzr, [x8], #16
    cmp     x8,  x9
    b.lt    .Lzero_tables

    // ── L0 tables: entry[0] → shared BOOT_L1 ────────────────────
    // TABLE descriptor: PTE_VALID(0) | PTE_TABLE(1) = 0x3
    orr     x8,  x2,  #3            // x8 = L1_PA | 0x3
    str     x8,  [x0]               // BOOT_TTBR0_L0[0]
    str     x8,  [x1]               // BOOT_TTBR1_L0[0]  (same L1!)

    // ── BOOT_L1: one entry per 1-GiB region ──────────────────────
    orr     x8,  x3,  #3
    str     x8,  [x2]               // L1[0] → BOOT_L2_MMIO
    orr     x8,  x4,  #3
    str     x8,  [x2, #8]           // L1[1] → BOOT_L2_RAM_1
    orr     x8,  x5,  #3
    str     x8,  [x2, #16]          // L1[2] → BOOT_L2_RAM_2
    orr     x8,  x6,  #3
    str     x8,  [x2, #24]          // L1[3] → BOOT_L2_RAM_3
    orr     x8,  x7,  #3
    str     x8,  [x2, #32]          // L1[4] → BOOT_L2_RAM_4

    // ── BOOT_L2_MMIO: device 2 MiB blocks 0x0800_0000–0x0C00_0000 ──
    // Block flags: valid(0) | AF(10) | SH_inner(3<<8) | AP_EL1_RW(0) |
    //              ATTR_device(2) | UXN(54) | PXN(53)
    //            = 0x0060_0000_0000_0705
    movz    x8,  #0x0705
    movk    x8,  #0x0060, lsl #48
    // Indices 64..96: 0x0800_0000 >> 21 = 64, 32 × 2 MiB = 64 MiB.
    mov     x9,  #64
    mov     x10, #96
.Lmmio_loop:
    cmp     x9,  x10
    b.ge    .Lmmio_done
    lsl     x11, x9,  #21           // PA = index << 21
    orr     x11, x11, x8            // PA | device_flags
    lsl     x12, x9,  #3            // byte_offset = index * 8
    str     x11, [x3, x12]
    add     x9,  x9,  #1
    b       .Lmmio_loop
.Lmmio_done:

    // ── BOOT_L2_RAM_1..4: normal EL1-only 2 MiB blocks ───────────
    // Block flags: valid(0) | AF(10) | SH_inner(3<<8) | AP_EL1_RW(0) |
    //              ATTR_normal(0) | UXN(54)  (PXN=0: EL1 can execute)
    //            = 0x0040_0000_0000_0701
    movz    x8,  #0x0701
    movk    x8,  #0x0040, lsl #48

    mov     x11, #512               // all tables have 512 entries

    // RAM_1: 0x4000_0000 + i*2MB  (L1[1], GB 1)
    movz    x9,  #0x4000, lsl #16
    mov     x10, #0
.Lram1_loop:
    cmp     x10, x11
    b.ge    .Lram1_done
    orr     x12, x9,  x8
    lsl     x13, x10, #3
    str     x12, [x4, x13]
    add     x9,  x9,  #0x200000
    add     x10, x10, #1
    b       .Lram1_loop
.Lram1_done:

    // RAM_2: 0x8000_0000 + i*2MB  (L1[2], GB 2)
    movz    x9,  #0x8000, lsl #16
    mov     x10, #0
.Lram2_loop:
    cmp     x10, x11
    b.ge    .Lram2_done
    orr     x12, x9,  x8
    lsl     x13, x10, #3
    str     x12, [x5, x13]
    add     x9,  x9,  #0x200000
    add     x10, x10, #1
    b       .Lram2_loop
.Lram2_done:

    // RAM_3: 0xC000_0000 + i*2MB  (L1[3], GB 3)
    movz    x9,  #0xC000, lsl #16
    mov     x10, #0
.Lram3_loop:
    cmp     x10, x11
    b.ge    .Lram3_done
    orr     x12, x9,  x8
    lsl     x13, x10, #3
    str     x12, [x6, x13]
    add     x9,  x9,  #0x200000
    add     x10, x10, #1
    b       .Lram3_loop
.Lram3_done:

    // RAM_4: 0x1_0000_0000 + i*2MB  (L1[4], GB 4)
    movz    x9,  #0x1, lsl #32
    mov     x10, #0
.Lram4_loop:
    cmp     x10, x11
    b.ge    .Lram4_done
    orr     x12, x9,  x8
    lsl     x13, x10, #3
    str     x12, [x7, x13]
    add     x9,  x9,  #0x200000
    add     x10, x10, #1
    b       .Lram4_loop
.Lram4_done:

    ret                             // return to caller (x30 = saved LR)


// ── Boot page tables ─────────────────────────────────────────────────────────
// 8 × 4 KiB = 32 KiB in .boot_tables section (low PA, VMA == LMA).
// Filled by .Lbuild_boot_tables at runtime before MMU enable.
// Discarded once runtime VMM tables take over in kernel_main.

.section .boot_tables, "aw", @progbits
.align 12

.global BOOT_TTBR0_L0
BOOT_TTBR0_L0: .space 4096

.global BOOT_TTBR1_L0
BOOT_TTBR1_L0: .space 4096

BOOT_L1:       .space 4096
BOOT_L2_MMIO:  .space 4096
BOOT_L2_RAM_1: .space 4096
BOOT_L2_RAM_2: .space 4096
BOOT_L2_RAM_3: .space 4096
BOOT_L2_RAM_4: .space 4096
"#
);
