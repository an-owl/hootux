pub mod mutex;
pub mod static_protected;
mod unsafe_box;
mod worm;

pub use mutex::{Mutex, MutexGuard};
pub use static_protected::{KernelStatic, UnlockedStatic};
pub(crate) use unsafe_box::UnsafeBox;
pub(crate) use worm::Worm;

/// Struct to provide !Send, !Sync traits
#[derive(Default, Copy, Clone)]
#[repr(transparent)]
pub struct PhantomUnsend {
    _t: core::marker::PhantomData<*mut ()>,
}
