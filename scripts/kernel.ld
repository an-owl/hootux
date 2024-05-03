ENTRY(_start)

SECTIONS {

    . = 1M;
    .multiboot_header : ALIGN(8) {
        KEEP(*(.multiboot2_header))
    }
    .text 0x200000 : ALIGN(0x1000) {
        KEEP(*(.text.libboot.entry.mb2_efi64)) *(.text) *(.text.*) /* linker does not know that *.libboot.entry contains real entry points */
    }
    .rodata : ALIGN(0x1000) {
        *(.rodata) *(.rodata.*)
    }
    .rodata : ALIGN(0x1000) {
        *(.rodata) *(.rodata.*)
    }
    .eh_frame_hdr : ALIGN(0x1000) {
        *(.eh_frame_hdr) *(.eh_frame_hdr.*)
    }
    .eh_frame : {
        *(.eh_frame) *(.eh_frame.*)
    }
    .tdata : ALIGN(0x1000) {
        *(.tdata) *(.tdata.*)
    }
    .tbss :  {
        *(.tbss) *(.tbss.*)
    }
    .data : ALIGN(0x1000) {
        *(.data) *(.data.*)
    }
    .bss : ALIGN(0x1000) {
        *(.bss) *(.bss.*)
    }
}