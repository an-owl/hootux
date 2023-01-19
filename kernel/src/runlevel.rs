//! This module is for indicating and setting the current Run-Level of the system, indicating what
//! resources are available, The runlevel can be used to restrict some capabilities such as
//! `#[thread_local]` statics.

static RUNLEVEL: core::sync::atomic::AtomicU8 =
    core::sync::atomic::AtomicU8::new(Runlevel::PreInit as u8);

pub fn runlevel() -> Runlevel {
    RUNLEVEL.load(core::sync::atomic::Ordering::Relaxed).into()
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
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum Runlevel {
    /// - Indicates that the system is in a very limited state.
    /// - Heap May be available
    /// - MP disabled
    ///
    /// This state is ended when the kernels memory management is fully initialized.
    PreInit = 0,
    /// - Heap is available
    /// - TLS is available
    ///
    /// MP will become available during this runlevel.
    /// This runlevel is exited when the kernel initializes the kernel executor
    Init,
    ///
    Kernel,
}

impl From<u8> for Runlevel {
    fn from(value: u8) -> Self {
        match value {
            0 => Self::PreInit,
            1 => Self::Init,
            2 => Self::Kernel,
            n => panic!("Unknown RunLevel found {}", n),
        }
    }
}
