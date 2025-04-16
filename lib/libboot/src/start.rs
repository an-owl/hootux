core::arch::global_asm!(
    "
    .code32
    .global _start
    .section .text.hatcher.common.start
    _start:
        cmp eax,{MULTIBOOT2_MAGIC}
        je .L_multiboot2_resolve

        ud2 // If we hit this then an unrecognized boot protocol was used

    .L_multiboot2_resolve:
        mov edi,eax
        mov ecx,{EFER_ADDR}
        rdmsr
        bt eax,10 // check if EFER.LME is enabled.
        mov eax,edi
        jc _kernel_preload_entry_mb2efi64 // If it is then we have booted with MB2_EFI64
        jmp hatcher_multiboot2_pm_entry // If not then we must have booted with MB2_PM. Because we do not support MB2_EFI32
    ",
    MULTIBOOT2_MAGIC = const 0x36d76289u32,
    EFER_ADDR = const 0xC000_0080u32,
);

/// This is the entry point for the multiboot2 EFI64 entry.
/// This must be passed to the multiboot2 entry as a u32, this is checked at link and will
/// result in a very unsightly linker error if it occurs.
///
/// A section named ` .text.hatcher.multiboot2.kernel_preload_entry_efi64` stores this function
/// and may be linked separately to allow this address to be safely truncated while linking the
/// rest of this code elsewhere.
#[cfg(all(feature = "multiboot2", feature = "uefi", target_arch = "x86_64"))]
pub use crate::multiboot2_entry::_kernel_preload_entry_mb2efi64;

/// This the entry point for the multiboot2 protected mode entry point.
///
/// This entry point is defined in the section ` .text.hatcher.entry.multiboot2.protected_mode`
#[cfg(all(feature = "multiboot2", target_arch = "x86_64"))]
pub use crate::multiboot2_entry::pm::hatcher_multiboot2_pm_entry;
