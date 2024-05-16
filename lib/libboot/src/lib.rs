#![no_std]

//! This library is a unified bot library designed to allow a kernel to boot using multiple methods 
//! and minimal configuration.
//! 
//! This library works by defining a number of entry points which can be used by different boot 
//! methods such as multiboot2. These entry points do a minimal amount of work to get the kernel 
//! into a unified entry point which does not need to discriminate between boot methods.
//! All entry points are re-exported from the [entry_points] module
//! 
//! The binary using this library must provide libboot an entry point named "_libboot_entry" see 
//! [kernel_entry] macro for details
// todo add config file to configure libBoot behaviour

pub mod boot_info;
#[cfg(feature = "multiboot2")]
mod multiboot2_entry;
pub(crate) mod common;

// Throw an error if all loader features are disabled
#[cfg(not(any(feature = "multiboot2")))]
compile_error!("Must use at least one bootloader feature");

pub mod entry_points {
    /// This is the entry point for the multiboot2 EFI64 entry.
    /// This must be passed to the multiboot2 entry as a u32, this is checked at link and will
    /// result in a very unsightly linker error if it occurs.
    ///
    /// A section named ` .text.libboot.multiboot2.kernel_preload_entry_efi64` stores this function
    /// and may be linked separately to allow this address to be safely truncated while linking the
    /// rest of this code elsewhere.
    #[cfg(all(feature = "multiboot2", feature = "uefi", target_arch = "x86_64"))]
    pub use crate::multiboot2_entry::_kernel_preload_entry_mb2efi64;
}

extern "C" {
    #[allow(improper_ctypes)]
    // This is intended to be a rust fn anyway, but it needs a stable ABI
    // will switch this to crabi when its stable.
    fn _libboot_entry(info: *mut boot_info::BootInfo) ->!;
}

/// libboot requires an entry point to call in order to hand over control to the kernel.
/// This macro ensures that the intended entry point has the correct signature.
/// This will cause a compile time error if the signature is incorrect
/// The entry point must have the link name "_libboot_entry"
///
/// If you have a linker error complaining that "_libboot_entry" is not found then you omitted this entry point.
///
/// The pointer given is guaranteed to be valid and read/writable however the pointer may be null, for
/// this reason the user should use [core::ptr::read] instead of dereferencing it.
// fixme UEFI code currently casts raw ptrs to references
#[macro_export]
macro_rules! kernel_entry {
    ($entry_name:ident) => {
        const _LIBBOOT_MACRO_ASSERT_ENTRY: extern "C" fn (*mut $crate::boot_info::BootInfo) -> ! = $entry_name;
    };
}