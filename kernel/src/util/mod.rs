mod io_write;
/// This crate provides tools to help with things that are otherwise simple or do not belong
/// elsewhere within the kernel.
pub mod mutex;
mod single_arc;
pub mod static_protected;
mod unsafe_box;
mod worm;

pub use io_write::{ToWritableBuffer, WriteableBuffer};
pub use mutex::Mutex;
pub use single_arc::*;
pub use static_protected::{KernelStatic, UnlockedStatic};
pub(crate) use unsafe_box::UnsafeBox;
pub(crate) use worm::Worm;

/// Struct to provide !Send, !Sync traits
#[derive(Default, Copy, Clone)]
#[repr(transparent)]
pub struct PhantomUnsend {
    _t: core::marker::PhantomData<*mut ()>,
}
