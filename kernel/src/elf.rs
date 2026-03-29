use bitflags::bitflags;
use x86_64::VirtAddr;
use x86_64::registers::control::{Cr0, Cr0Flags};
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::{Page, PageTableFlags, Size4KiB};

// Default base address is byte 0 of higher half.
// Because the first section isn't that low this should result in data being loaded well above it.
// This value is the default, it should be used as the base for KASLR
const DEFAULT_KERNEL_BASE: usize = 0xFFFF800000000000;

/// Reloads the kernel sections into `new_base`,
/// this ignores all sections without the "ALLOC" and "TLS" flags set.
///
/// This will not handle [elf::abi::SHT_RELA] sections.
///
/// This fn will return the address of the last allocated byte in `sections`
///
/// # Safety
///
/// The caller must ensure that `sections` correctly describes the elf sections of the currently
/// loaded image and the `sh_addr` value is set to the **current** address of the section.
unsafe fn reload_image(
    new_base: *mut u8,
    sections: impl Iterator<Item = elf::section::SectionHeader>,
) -> usize {
    let mut tail = 0;
    'section: for section in sections
        .filter(|sh| ElfFlags::from_bits_truncate(sh.sh_flags).contains(ElfFlags::SHF_ALLOC))
    {
        // SAFETY: Guaranteed by caller.
        let data = unsafe {
            core::slice::from_raw_parts(
                section.sh_addr as usize as *mut u8,
                section.sh_size as usize,
            )
        };

        tail = tail.max(section.sh_addr + section.sh_size);

        let dst_start = VirtAddr::from_ptr(new_base) + section.sh_addr;
        // We need to align the base *up*
        let tgt_start = (dst_start).align_down(crate::mem::PAGE_SIZE as u64);
        let tgt_end = (VirtAddr::from_ptr(new_base) + section.sh_addr + section.sh_size)
            .align_down(crate::mem::PAGE_SIZE as u64);

        let mut iter = PageRangeInclusive {
            start: Page::<Size4KiB>::from_start_address(tgt_start).unwrap(),
            end: Page::from_start_address(tgt_end).unwrap(),
        };

        // Sections might overlap, this checks the first page and skips it if they overlap.
        if crate::mem::mem_map::translate_ptr(tgt_start.as_ptr::<u8>()).is_some() {
            let _ = iter.next();
        }

        let flags = {
            let mut acc = PageTableFlags::PRESENT;
            let flags = ElfFlags::from_bits_truncate(section.sh_flags);

            if flags.contains(ElfFlags::SHF_WRITE) {
                acc |= PageTableFlags::WRITABLE;
            };

            if flags.contains(ElfFlags::SHF_TLS) {
                continue 'section;
            }

            acc
        };
        // todo: Change to use most appropriate
        crate::mem::mem_map::map_range(iter, flags);
        // SAFETY: We've just created this section.
        let dest = unsafe {
            core::slice::from_raw_parts_mut(dst_start.as_mut_ptr(), section.sh_size as usize)
        };

        let mut cr0f = Cr0::read();
        cr0f.set(Cr0Flags::WRITE_PROTECT, false);
        // SAFETY: This is literally the purpose of this bit described in the documentation.
        unsafe {
            Cr0::write(cr0f);
        }

        dest.copy_from_slice(data);

        cr0f.set(Cr0Flags::WRITE_PROTECT, true);
        // SAFETY: This is literally the purpose of this bit described in the documentation.
        unsafe {
            Cr0::write(cr0f);
        }
    }
    tail as usize
}

#[cfg(target_arch = "x86_64")]
bitflags! {
    struct ElfFlags: u64 {
        const SHF_WRITE = elf::abi::SHF_WRITE as u64;
        const SHF_ALLOC = elf::abi::SHF_ALLOC as u64;
        const SHF_EXECINSTR  = elf::abi::SHF_EXECINSTR as u64;
        const SHF_MERGE = elf::abi::SHF_MERGE as u64;
        const SHF_STRINGS = elf::abi::SHF_STRINGS as u64;
        const SHF_INFO_LINK = elf::abi::SHF_INFO_LINK as u64;
        const SHF_LINK_ORDER = elf::abi::SHF_LINK_ORDER as u64;
        const SHF_OS_NONCONFORMING = elf::abi::SHF_OS_NONCONFORMING as u64;
        const SHF_GROUP = elf::abi::SHF_GROUP as u64;
        const SHF_TLS = elf::abi::SHF_TLS as u64;
        const SHF_MASKOS = elf::abi::SHF_MASKOS as u64;
        const SHF_X86_64_LARGE = elf::abi::SHF_X86_64_LARGE;
    }
}

/// Returns the new kernel base. This fn will always return the same value.
///
/// Without KASLR this will return [DEFAULT_KERNEL_BASE].
fn get_base() -> *mut u8 {
    DEFAULT_KERNEL_BASE as *mut u8
}

/// Relocates the kernel into the higher half. Returns the new base of the kernel.
///
/// # Safety
///
/// The caller must ensure that `sections` points to the currently loaded elf sections.
#[must_use]
pub unsafe fn relocate(
    mut sections: impl Iterator<Item = elf::section::SectionHeader> + Clone,
) -> *const u8 {
    // SAFETY: Upheld by caller.
    let image_tail = unsafe { reload_image(get_base(), sections.clone()) };
    let new_image = core::ptr::from_raw_parts_mut(get_base(), image_tail);

    if let Some(sh_rela) = sections.find(|s| s.sh_type == elf::abi::SHT_RELA) {
        let entries = sh_rela.sh_size / sh_rela.sh_entsize;
        assert_eq!(
            sh_rela.sh_entsize as usize,
            size_of::<elf::relocation::Elf64_Rela>()
        );

        // SAFETY: Upheld by caller
        let rela =
            unsafe { core::slice::from_raw_parts(sh_rela.sh_addr as *mut _, entries as usize) };

        let mut cr0f = Cr0::read();
        cr0f.set(Cr0Flags::WRITE_PROTECT, false);
        // SAFETY: This is safe.
        // We must disable write protection because the read-only data is still being set-up
        unsafe { Cr0::write(cr0f) };

        // SAFETY: `reload_image` will load all `SHF_ALLOC` sections into the `new_image`
        unsafe {
            elfload::relocation::relocate_image(
                new_image,
                core::ptr::null(),
                core::ptr::null(),
                rela,
            )
        }
        .unwrap();

        cr0f.set(Cr0Flags::WRITE_PROTECT, true);
        // SAFETY: Re-enables write protection which is the expected system state.
        unsafe { Cr0::write(cr0f) };
    }
    x86_64::instructions::tlb::flush_all();
    get_base()
}

/// Changes execution to a new image location.
///
/// # Safety
///
/// The caller must ensure that
/// * The caller never returns.
/// * No references are held to static data in the old image.
/// * The new image is loaded and executable.
#[inline(never)]
pub unsafe fn switch_image(current_base: *const u8, new_base: *const u8) {
    let diff = new_base.addr() - current_base.addr();
    // SAFETY: I'm not even sure what to write here.
    let t = unsafe { crate::llvm::address_of_return_address!() }.unwrap();
    let ra = t.read().byte_offset(diff as isize);

    // SAFETY: Offset contains the address of the same code in a new location.
    t.write(ra);
}
