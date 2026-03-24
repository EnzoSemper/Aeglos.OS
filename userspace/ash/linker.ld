OUTPUT_FORMAT(elf64-littleaarch64)
OUTPUT_ARCH(aarch64)
ENTRY(ash_entry)

SECTIONS {
    . = 0x48000000;

    .text : {
        KEEP(*(.text.ash_entry))
        *(.text .text.*)
    }

    . = ALIGN(8);
    .rodata : {
        *(.rodata .rodata.*)
    }

    . = ALIGN(8);
    .data : {
        *(.data .data.*)
    }

    . = ALIGN(8);
    .bss (NOLOAD) : {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(8);
        __bss_end = .;
    }
}
