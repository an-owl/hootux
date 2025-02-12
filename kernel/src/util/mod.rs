/// This crate provides tools to help with things that are otherwise simple or do not belong
/// elsewhere within the kernel.

pub mod mutex;
pub mod static_protected;
mod unsafe_box;
mod worm;
mod single_arc;
mod io_write;


pub use mutex::Mutex;
pub use static_protected::{KernelStatic, UnlockedStatic};
pub(crate) use unsafe_box::UnsafeBox;
pub(crate) use worm::Worm;
pub use single_arc::*;
pub use io_write::{ToWritableBuffer, WriteableBuffer};

/// Struct to provide !Send, !Sync traits
#[derive(Default, Copy, Clone)]
#[repr(transparent)]
pub struct PhantomUnsend {
    _t: core::marker::PhantomData<*mut ()>,
}
