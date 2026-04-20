use elf::relocation::Elf64_Rela;

#[inline(always)]
#[cfg(feature = "rela_self")]
/// Handles relocations for [elf::abi::R_X86_64_RELATIVE].
///
/// This function is `#[inline(always)]` and is intended to relocate a file when its relocations have not been set up.
pub unsafe fn relocate_simple(base: *const u8, relocations: *const Elf64_Rela, rela_count: usize) {
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

/// Relocates the image using an array of [Elf64_Rela].
///
/// `image` points to the whole loaded elf image. This should start at the base address,
/// *not* the first address that's been loaded. It should extend to the last byte in the executable image.
/// This fn will ensure that it does not write outside `image`.
/// `image` does *not* need to be entirely accessible or valid, just the regions referenced by `rela`.
///
/// `got` and `plt` should point to the "Global offset table" and "Procedure linkage table".
/// If these are not present (or necessary), the cn be set to [core::ptr::null].
///
/// # Safety
///
/// This will write to `image[rela[n].r_offset]`, the caller must ensure that these regions are loaded.
pub unsafe fn relocate_image(
    image: *mut [u8],
    got: *const u8,
    plt: *const u8,
    rela: impl Iterator<Item = Elf64_Rela>,
) -> Result<(), RelocError> {
    let base = image.cast::<u8>();
    for ref i in rela {
        let Ok(reloc): Result<X86_64RelocType, _> = i.try_into() else {
            return Err(RelocError::UnknownRelocType);
        };
        let calc = reloc.reloc_calc();
        let value = calc.calc(
            i.r_addend as usize,
            base.addr(),
            got.addr(),
            plt.addr(),
            i.r_offset as usize,
            0,
            reloc.size().size(),
        );
        // SAFETY: We below check that this is within the image range.
        let tgt_addr = unsafe { base.byte_offset(i.r_offset as isize) };
        let length = image.len();
        if (base.addr()..base.addr() + length).contains(&tgt_addr.addr()) {
            // SAFETY: Upheld by caller.
            unsafe { reloc.size().write(tgt_addr.cast(), value) }
        } else {
            return Err(RelocError::OutsideOfImage);
        }
    }
    Ok(())
}

#[derive(Debug)]
pub enum RelocError {
    OutsideOfImage,
    UnknownRelocType,
}

#[repr(u32)]
#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum X86_64RelocType {
    R_x86_64_64 = elf::abi::R_X86_64_64,
    R_x86_64_PC32 = elf::abi::R_X86_64_PC32,
    R_x86_64_GOT32 = elf::abi::R_X86_64_GOT32,
    R_x86_64_PLT32 = elf::abi::R_X86_64_PLT32,
    R_x86_64_GLOB_DAT = elf::abi::R_X86_64_GLOB_DAT,
    R_x86_64_JUMP_SLOT = elf::abi::R_X86_64_JUMP_SLOT,
    R_x86_64_RELATIVE = elf::abi::R_X86_64_RELATIVE,
    R_x86_64_GOTPCREL = elf::abi::R_X86_64_GOTPCREL,
    R_x86_64_32 = elf::abi::R_X86_64_32,
    R_x86_64_32S = elf::abi::R_X86_64_32S,
    R_x86_64_16 = elf::abi::R_X86_64_16,
    R_x86_64_PC16 = elf::abi::R_X86_64_PC16,
    R_x86_64_8 = elf::abi::R_X86_64_8,
    R_x86_64_PC8 = elf::abi::R_X86_64_PC8,
    R_x86_64_PC64 = elf::abi::R_X86_64_PC64,
    R_x86_64_GOTOFF64 = elf::abi::R_X86_64_GOTOFF64,
    R_x86_64_GOTPC32 = elf::abi::R_X86_64_GOTPC32,
    R_x86_64_SIZE32 = elf::abi::R_X86_64_SIZE32,
    R_x86_64_SIZE64 = elf::abi::R_X86_64_SIZE64,
}

impl TryFrom<&Elf64_Rela> for X86_64RelocType {
    type Error = ();
    fn try_from(value: &Elf64_Rela) -> Result<Self, Self::Error> {
        Ok(match value.r_info as u32 {
            elf::abi::R_X86_64_64 => X86_64RelocType::R_x86_64_64,
            elf::abi::R_X86_64_PC32 => X86_64RelocType::R_x86_64_PC32,
            elf::abi::R_X86_64_GOT32 => X86_64RelocType::R_x86_64_GOT32,
            elf::abi::R_X86_64_PLT32 => X86_64RelocType::R_x86_64_PLT32,
            elf::abi::R_X86_64_GLOB_DAT => X86_64RelocType::R_x86_64_GLOB_DAT,
            elf::abi::R_X86_64_JUMP_SLOT => X86_64RelocType::R_x86_64_JUMP_SLOT,
            elf::abi::R_X86_64_RELATIVE => X86_64RelocType::R_x86_64_RELATIVE,
            elf::abi::R_X86_64_GOTPCREL => X86_64RelocType::R_x86_64_GOTPCREL,
            elf::abi::R_X86_64_32 => X86_64RelocType::R_x86_64_32,
            elf::abi::R_X86_64_32S => X86_64RelocType::R_x86_64_32S,
            elf::abi::R_X86_64_16 => X86_64RelocType::R_x86_64_16,
            elf::abi::R_X86_64_PC16 => X86_64RelocType::R_x86_64_PC16,
            elf::abi::R_X86_64_8 => X86_64RelocType::R_x86_64_8,
            elf::abi::R_X86_64_PC8 => X86_64RelocType::R_x86_64_PC8,
            elf::abi::R_X86_64_PC64 => X86_64RelocType::R_x86_64_PC64,
            elf::abi::R_X86_64_GOTOFF64 => X86_64RelocType::R_x86_64_GOTOFF64,
            elf::abi::R_X86_64_GOTPC32 => X86_64RelocType::R_x86_64_GOTPC32,
            elf::abi::R_X86_64_SIZE32 => X86_64RelocType::R_x86_64_SIZE32,
            elf::abi::R_X86_64_SIZE64 => X86_64RelocType::R_x86_64_SIZE64,
            _ => return Err(()),
        })
    }
}

impl X86_64RelocType {
    const fn size(self) -> RelocSize {
        match self {
            Self::R_x86_64_64
            | Self::R_x86_64_GLOB_DAT
            | Self::R_x86_64_JUMP_SLOT
            | Self::R_x86_64_RELATIVE
            | Self::R_x86_64_PC64
            | Self::R_x86_64_GOTOFF64
            | Self::R_x86_64_SIZE64 => RelocSize::U64,
            Self::R_x86_64_PC32
            | Self::R_x86_64_GOT32
            | Self::R_x86_64_SIZE32
            | Self::R_x86_64_GOTPCREL
            | Self::R_x86_64_32
            | Self::R_x86_64_32S
            | Self::R_x86_64_GOTPC32
            | X86_64RelocType::R_x86_64_PLT32 => RelocSize::U32,
            Self::R_x86_64_16 | Self::R_x86_64_PC16 => RelocSize::U16,
            Self::R_x86_64_PC8 | Self::R_x86_64_8 => RelocSize::U8,
        }
    }

    fn reloc_calc(self) -> RelocCalc {
        match self {
            // S + A
            Self::R_x86_64_64
            | Self::R_x86_64_32
            | Self::R_x86_64_32S
            | Self::R_x86_64_16
            | Self::R_x86_64_8 => RelocCalc {
                tgt_symbol: true,
                base: true,
                ..Default::default()
            },

            // S + A - P
            Self::R_x86_64_PC32
            | Self::R_x86_64_PC16
            | Self::R_x86_64_PC8
            | Self::R_x86_64_PC64 => RelocCalc {
                tgt_symbol: true,
                base: true,
                sym_offset: Usage::Sub,
                ..Default::default()
            },

            // G + A
            Self::R_x86_64_GOT32 =>
            /*RelocCalc {
                addened: true,
                got_offset: true,
                ..Default::default()

            },*/
            {
                todo!("Unsupported reloc type")
            }

            // L + A - P
            Self::R_x86_64_PLT32 =>
            /* RelocCalc {
                plt: true,
                addened: true,
                sym_offset: Usage::Sub,
                ..Default::default()
            }, */
            {
                todo!("Unsupported reloc type")
            }

            // S
            Self::R_x86_64_GLOB_DAT | Self::R_x86_64_JUMP_SLOT => RelocCalc {
                tgt_symbol: true,
                ..Default::default()
            },

            // B + A
            Self::R_x86_64_RELATIVE => RelocCalc {
                base: true,
                addened: true,
                ..Default::default()
            },

            // G + GOT + A - P
            Self::R_x86_64_GOTPCREL =>
            /* RelocCalc {
                got: Usage::Add,
                got_offset: true,
                addened: true,
                sym_offset: Usage::Add,
                ..Default::default()
            }, */
            {
                todo!("Unsupported reloc type")
            }

            // S + A - GOT
            Self::R_x86_64_GOTOFF64 =>
            /* RelocCalc {
                tgt_symbol: true,
                addened: true,
                got: Usage::Sub,
                ..Default::default()
            }, */
            {
                todo!("Unsupported reloc type")
            }

            // GOT + A + P
            Self::R_x86_64_GOTPC32 =>
            /* RelocCalc {
                got: Usage::Add,
                addened: true,
                plt: true,
                ..Default::default()
            }, */
            {
                todo!("Unsupported reloc type")
            }

            // Z + A
            Self::R_x86_64_SIZE32 | Self::R_x86_64_SIZE64 => RelocCalc {
                addened: true,
                symbol_size: true,
                ..Default::default()
            },
        }
    }
}

enum RelocSize {
    U8,
    U16,
    U32,
    U64,
}

impl RelocSize {
    /// Writes `value` into `tgt` with the size of `self`.
    ///
    /// # Safety
    ///
    /// This is effectively a wrapper for [core::ptr::write_unaligned], see its safety section for
    /// information.
    unsafe fn write(&self, tgt: *mut u64, value: usize) {
        // SAFETY: Upheld by caller.
        unsafe {
            match self {
                RelocSize::U8 => tgt.cast::<u8>().write_unaligned(value.try_into().unwrap()),
                RelocSize::U16 => tgt.cast::<u16>().write_unaligned(value.try_into().unwrap()),
                RelocSize::U32 => tgt.cast::<u32>().write_unaligned(value.try_into().unwrap()),
                RelocSize::U64 => tgt.cast::<u64>().write_unaligned(value.try_into().unwrap()),
            }
        }
    }

    const fn size(self) -> usize {
        match self {
            RelocSize::U8 => 1,
            RelocSize::U16 => 2,
            RelocSize::U32 => 4,
            RelocSize::U64 => 8,
        }
    }
}

#[derive(Default)]
struct RelocCalc {
    // A
    addened: bool,
    /// Base address of the loaded file.
    // B
    base: bool,
    /// The offset from the symbol to the GOT.
    // G
    got_offset: bool,
    // GOT
    got: Usage,
    // L
    plt: bool,
    /// Offset of the symbol being relocated [elf::relocation::Elf64_Rela::r_offset] field.
    // P
    sym_offset: Usage,
    // S
    tgt_symbol: bool,
    // Z
    symbol_size: bool,
}

impl RelocCalc {
    const fn calc(
        &self,
        addened: usize,     // A
        base: usize,        // B
        got: usize,         // GOT
        plt: usize,         // L
        sym_offset: usize,  // r_offset
        dst_address: usize, // S
        sym_size: usize,    // Z
    ) -> usize {
        let mut sum = 0;
        if self.addened {
            sum += addened;
        }
        if self.base {
            sum += base;
        }
        if self.got_offset {
            sum += sym_offset - got;
        }
        if self.plt {
            sum += plt;
        }
        if self.tgt_symbol {
            sum += dst_address;
        }
        if self.symbol_size {
            sum += sym_size;
        }
        match self.got {
            Usage::None => {}
            Usage::Add => sum += got,
            Usage::Sub => sum -= got,
        }
        match self.sym_offset {
            Usage::None => {}
            Usage::Add => sum += sym_offset,
            Usage::Sub => sum -= sym_offset,
        }

        sum
    }
}

#[derive(Default)]
enum Usage {
    #[default]
    None,
    Add,
    Sub,
}
