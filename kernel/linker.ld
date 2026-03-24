/* Aeglos OS — AArch64 Linker Script — TTBR1 kernel/user VA split     */
/* Boot code and tables stay at low PA (VMA == LMA == PA).            */
/* Main kernel sections are at VMA = PA + KERNEL_VA_OFFSET.           */
/* This gives true kernel/user isolation: EL1 code runs via TTBR1     */
/* (high VA), user TTBR0 contains only per-process ELF/stack maps.    */

ENTRY(_start)

KERNEL_VA_OFFSET = 0xFFFF000000000000;
KERNEL_LOAD_BASE = 0x40080000;

SECTIONS
{
    /* ── Boot section: VMA == LMA == PA.  Executes before MMU. ── */
    . = KERNEL_LOAD_BASE;

    .text.boot : {
        KEEP(*(.text.boot))
    }

    /* Boot page tables — 8 × 4 KiB, pre-MMU access. */
    .boot_tables : ALIGN(4096) {
        KEEP(*(.boot_tables))
    }

    __boot_lma_end = .;          /* PA immediately after the boot region */

    /* ── Main kernel: VMA = PA + KERNEL_VA_OFFSET ──────────────── */
    /* AT(__boot_lma_end) sets the LMA of .text; subsequent sections */
    /* auto-increment LMA from that base (LLD behaviour).            */
    . = __boot_lma_end + KERNEL_VA_OFFSET;

    .text : AT(__boot_lma_end) {
        *(.text .text.*)
    }

    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4096) {
        *(.data .data.*)
    }

    .bss : ALIGN(4096) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    .data.rel.ro : ALIGN(4096) {
        *(.data.rel.ro.local*) *(.data.rel.ro*)
    }

    .gcc_except_table : ALIGN(4096) {
        *(.gcc_except_table*)
    }

    .got : ALIGN(4096) {
        *(.got*)
    }

    .init_array : ALIGN(4096) {
        __init_array_start = .;
        KEEP(*(.init_array*))
        __init_array_end = .;
    }

    . = ALIGN(4096);

    /* PA symbols — absolute values (not addresses) used by:         */
    /*   boot assembly  : __bss_start_pa, __bss_end_pa, __stack_top  */
    /*   page allocator : __kernel_end (first free PA)               */
    PROVIDE(__bss_start_pa = LOADADDR(.bss));
    PROVIDE(__bss_end_pa   = LOADADDR(.bss) + SIZEOF(.bss));
    PROVIDE(__stack_top_pa = KERNEL_LOAD_BASE);
    PROVIDE(__kernel_end   = LOADADDR(.init_array) + SIZEOF(.init_array));

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
    }
}
