//! This module contains components for using the write combining cache mode.
//! These features are not supported on all CPUs architectures which do not support write combining
//! or which arent supported by this module will transparently use uncached memory.
//!
//! # Usage
//!
//! Write combining improves performance over using uncached memory by buffering and cascading
//! memory writes allowing them to be completed asynchronously.
//! This however means that writes are weakly ordered and ordering and ordering must be handled by software.
//! This can be handled using the [wc_sync] fn.
//!
//! The [WcWrap] struct is a wrapper around its inner data and provides methods to access the inner data.
//! The write combining allocators wrap another allocator

mod alloc;

pub use alloc::*;
use core::ops::{Deref, DerefMut};

pub(crate) fn init_wc() {
    #[cfg(all(target_arch = "x86_64", feature = "write-combining"))]
    {
        // See the Intel-SDM "MTRR considerations in MP systems" this follows its process
        // steps 8-10 are skipped and replaced with changing the PAT

        use x86_64::registers::control;
        use x86_msr::Msr;

        // SAFETY: No read effects, supported on all x86_64 systems.
        x86_64::instructions::interrupts::without_interrupts(|| {
            let mut reg = control::Cr0::read();
            reg.set(control::Cr0Flags::CACHE_DISABLE, true);
            reg.set(control::Cr0Flags::NOT_WRITE_THROUGH, false);
            // SAFETY: Completely disables caching, this is safe
            unsafe { control::Cr0::write(reg) }

            // invalidating cache is only required if self snooping is not supported cpuid.1.edx[27]
            if raw_cpuid::cpuid!(1).edx & (1 << 27) == 0 {
                // SAFETY: Doesn't do anything sketchey
                unsafe { core::arch::asm!("wbinvd", options(nomem, nostack, preserves_flags)) }
            }

            let pge = if control::Cr4::read().contains(control::Cr4Flags::PAGE_GLOBAL) {
                let mut f = control::Cr4::read();
                f.set(control::Cr4Flags::PAGE_GLOBAL, false);
                // SAFETY: Disables global paging, this is safe
                unsafe { control::Cr4::write(f) }
                true
            } else {
                x86_64::structures::paging::mapper::MapperFlushAll::new().flush_all();
                false
            };

            let mut pat = unsafe { x86_msr::architecture::Pat::read() };
            if pat[3] != x86_msr::architecture::PatType::Uncachable {
                log::warn!(
                    "PAT[3] is {:?}, expected {:?}... Ignoring",
                    pat[3],
                    x86_msr::architecture::PatType::Uncachable
                )
            }

            pat[4] = x86_msr::architecture::PatType::WriteCombining;
            unsafe { x86_msr::architecture::Pat::write(pat) }

            // Manual says TLB flush is always required, and `wbinvd` may not be required but does not specify how to check
            x86_64::structures::paging::mapper::MapperFlushAll::new().flush_all();
            // I assume that this is one of the checks though
            if raw_cpuid::cpuid!(1).edx & (1 << 27) == 0 {
                // SAFETY: Doesn't do anything sketchey
                unsafe { core::arch::asm!("wbinvd", options(nomem, nostack, preserves_flags)) }
            }

            if pge {
                let mut f = control::Cr4::read();
                f.set(control::Cr4Flags::PAGE_GLOBAL, true);
                // SAFETY: Restores cr4 to its original value
                unsafe { control::Cr4::write(f) }
            }
            reg.set(control::Cr0Flags::CACHE_DISABLE, false);
            unsafe { control::Cr0::write(reg) }
        });
    }
    #[cfg(not(all(target_arch = "x86_64", feature = "write-combining")))]
    log::trace!("WC disabled");
}

pub fn set_wc_data(
    region: &core::ptr::NonNull<[u8]>,
) -> Result<(), super::mem_map::UpdateFlagsErr> {
    #[cfg(all(target_arch = "x86_64", feature = "write-combining"))]
    {
        let start = region.as_ptr().cast::<u8>() as usize;
        //fixme
        // SAFETY: Not safe. This doesnt actually read the data but the compiler might try to.
        let end = unsafe { (&*region.as_ptr()).len() + start };

        // wc uses PAT[3], cache_disable & write_through selects PAT[3]
        let flags = {
            use x86_64::structures::paging::PageTableFlags;
            PageTableFlags::PRESENT
                | PageTableFlags::WRITABLE
                | PageTableFlags::NO_EXECUTE
                | PageTableFlags::NO_CACHE
                | PageTableFlags::WRITE_THROUGH
        };

        crate::mem::mem_map::update_flags!(
            flags,
            x86_64::VirtAddr::new(start as u64),
            x86_64::VirtAddr::new(end as u64)
        )
        //super::mem_map::set_flags_iter(range, flags)
    }
    #[cfg(not(all(
        any(target_arch = "x86", target_arch = "x86_64"),
        feature = "write-combining"
    )))]
    {
        Ok(())
    } // pretend everything's working
}

/// Forces the write combining buffer to be dumped.
#[inline]
pub fn wc_sync() {
    // x86* when wc enabled
    #[cfg(all(
        any(target_arch = "x86", target_arch = "x86_64"),
        feature = "write-combining"
    ))]
    {
        // This doesnt use a normal atomic fence because I cant guarantee that an `sfence` instruction will be emitted.
        // Using an asm block does guarantee this.
        // SAFETY: This doesnt do anything dodgey
        unsafe { core::arch::asm!("sfence", options(nomem, preserves_flags, nostack)) }
        core::sync::atomic::compiler_fence(atomic::Ordering::Release);
    }

    // Default path
    #[cfg(not(all(
        any(target_arch = "x86", target_arch = "x86_64"),
        feature = "write-combining"
    )))]
    {
        atomic::fence(atomic::Ordering::Release)
    }
}

/// A WcWrap is a wrapper around a type using write combining memory.
/// WcWrap provides methods for interacting with the inner type.
// God i love the WcMonalds WcWrap
#[repr(transparent)]
pub struct WcWrap<T> {
    inner: T,
}

#[must_use]
pub struct WcWrapGuard<'a, T> {
    data: &'a mut WcWrap<T>,
}

impl<T> WcWrap<T> {
    /// Wraps `t` with WcWrap.
    /// This does not set the cache mode, this must be done beforehand.
    pub fn new(t: T) -> Self {
        Self { inner: t }
    }

    pub fn read(&self) -> &T {
        &self.inner
    }

    /// Returns a mutable reference to the wrapped data. This will not forceably dump the WC buffer unlike [Self::write]
    pub fn write_no_sync(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Returns a struct which can be used to write to the inner value.
    /// When the guard is dropped the WC buffer will be dumped into system memory.
    pub fn write(&mut self) -> WcWrapGuard<T> {
        WcWrapGuard { data: self }
    }

    pub fn unwrap(self) -> T {
        self.inner
    }
}

impl<'a, T> Drop for WcWrapGuard<'a, T> {
    fn drop(&mut self) {
        wc_sync();
    }
}

impl<'a, T> Deref for WcWrapGuard<'a, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.data.inner
    }
}

impl<'a, T> DerefMut for WcWrapGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data.inner
    }
}
