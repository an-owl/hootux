ENTRY(_start)

HIGHER_HALF = /* 0xFFFF800000000000 */ 0;

SECTIONS {



    /*. = 0xFFFF800000200000;*/
    . = 0x200000;
    .multiboot_header : ALIGN(8) {
        KEEP(*(.multiboot2_header))
    }

    .text.boot .-HIGHER_HALF : ALIGN(0x1000) {
        *(*.text.hatcher)
        *(*.text.hatcher.*)
        KEEP(*(.text.hatcher.*))
    }

    mb2_pm_stack_offset = MB2_PB_ARENA-hatcher_multiboot2_pm_entry;
    .text .+0x200000-SIZEOF(.multiboot_header) : AT(.-HIGHER_HALF) ALIGN(0x1000) {

        *(.text)
        *(.text.*)
    }
    .rodata : AT(.-HIGHER_HALF) ALIGN(0x1000) {
        *(.rodata) *(.rodata.*)
    }
    .rodata : AT(.-HIGHER_HALF) ALIGN(0x1000) {
        *(.rodata) *(.rodata.*)
    }
    .tdata : AT(.-HIGHER_HALF) ALIGN(0x1000) {
        *(.tdata) *(.tdata.*)
    }
    .tbss : AT(.-HIGHER_HALF)  {
        *(.tbss) *(.tbss.*)
    }
    .data : AT(.-HIGHER_HALF) ALIGN(0x1000) {
        *(.data) *(.data.*)
    }
    .bss : AT(.-HIGHER_HALF) ALIGN(0x1000) {
        *(.bss) *(.bss.*)
    }
    .dynsym : AT(.-HIGHER_HALF) {
        *(.dynsym) *(.dynsym.*)
    }

    /*
    /DISCARD/ : {
        *(.eh_frame)
        *(.eh_frame.*)
    }
    */

}