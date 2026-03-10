#[inline(always)]
#[cfg(feature = "rela_self")]
/// Handles relocations for [elf::abi::R_X86_64_RELATIVE].
///
/// This function is `#[inline(always)]` and is intended to relocate a file when its relocations have not been set up.
pub unsafe fn relocate_simple(
    base: *const u8,
    relocations: *const elf::relocation::Elf64_Rela,
    rela_count: usize,
) {
    unsafe {
        core::arch::asm!(
            "2:", // Top of loop, count-1 == last rela index
                "sub {count},1",
                "jc 9f", // Exit if count underflows

            "3:",
                // The entry size to multiply with the index.
                // We can do `mul imm` because we need to keep `count`
                "mov rax,{RELA_SIZE}",
                "xor rdx,rdx", // note rdx is freed at the end of the loop
                "mul {count}",

                // Loads the address of the rela entry into {entry}
                "lea rax,[{rela_start}+rax]",

                // Load and compare info type, jump to loop tail on fail.
                "mov rdx,[rax+{INFO_OFFSET}]",
                "cmp rdx,{TGT_TYPE}",
                "jne 2b",

                // Calculate address and store into offset
                "mov rdx,[rax+{ADDENED_OFFSET}]",
                "add rdx,{base}",
                "mov {dest},[rax+{OFFSET_OFFSET}]",
                "mov [{dest}+{base}],rdx",

                "jmp 2b",

            "9:", // Exit
            count = in(reg) rela_count,
            rela_start = in(reg) relocations,
            base = in(reg) base,
            out("rax") _,
            out("rdx") _, // Using rdx here because it's clobbered an needs to be used anyway
            dest = out(reg) _,
            ADDENED_OFFSET = const core::mem::offset_of!(elf::relocation::Elf64_Rela,r_addend),
            INFO_OFFSET = const core::mem::offset_of!(elf::relocation::Elf64_Rela,r_info),
            OFFSET_OFFSET = const core::mem::offset_of!(elf::relocation::Elf64_Rela,r_offset),
            TGT_TYPE = const elf::abi::R_X86_64_RELATIVE,
            RELA_SIZE = const core::mem::size_of::<elf::relocation::Elf64_Rela>(),

        );
    }
}
