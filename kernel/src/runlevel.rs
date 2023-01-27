//! This module is for indicating and setting the current Run-Level of the system, indicating what
//! resources are available, The runlevel can be used to restrict some capabilities such as
//! `#[thread_local]` statics.

use core::sync::atomic::Ordering;

static RUNLEVEL: atomic::Atomic<Runlevel> = atomic::Atomic::new(Runlevel::PreInit);

pub fn runlevel() -> Runlevel {
    RUNLEVEL.load(Ordering::Relaxed)
}

/*
Things that should be indicated
 - Heap allocation
 - Availability of thread local storage
 - Multiprocessing ( MP is suggested to be enabled even on systems that do not support it )
 - Kernel multitasking
 - User mode (multi user mode?)

 The end condition must be indicated,
 as well as anything that may become available during this runlevel

*/
#[non_exhaustive]
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
#[repr(u8)]
pub enum Runlevel {
    /// - Indicates that the system is in a very limited state.
    /// - Heap May be available
    /// - MP disabled
    ///
    /// This state is ended when the kernels memory is fully initialized, and is updated when the TLS
    /// for the BSP is loaded.
    PreInit,

    /// - Heap is available
    /// - TLS is available
    ///
    /// MP will become available during this runlevel.
    /// This runlevel is exited when the kernel initializes the kernel executor
    // todo: Not currently implemented
    Init,

    /// Indicates that Kernel multitasking is running
    /// Multiprocessing (where available) is running
    Kernel,

    /// Panic state all services may or may not be available, make no assumptions.
    Panic,
}

/// Sets the new runlevel to `new_runlevel`. Blocks until all cpu's ready for update
pub(crate) fn update_runlevel(new_runlevel: Runlevel) {
    assert!(new_runlevel > runlevel(), "Tried to set invalid runlevel");
    // todo MP: Message all CPU's to ready for update. Block until all CPU's respond with ready.
    log::info!("Updating RUNLEVEL to {:?}", new_runlevel);
    RUNLEVEL.store(new_runlevel, Ordering::Release);
}

/// Sets the runlevel to Panic indicating the system is panicked.
/// This does not block for other CPU's it assumes `panic!` has already message other CPU's
///
/// # Safety
///
/// This fn assumes that all CPU's have been notified to panic
pub unsafe fn set_panic() {
    log::info!("Updating RUNLEVEL to {:?}", Runlevel::Panic);
    RUNLEVEL.store(Runlevel::Panic, Ordering::Release)
}
