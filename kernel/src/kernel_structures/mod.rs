pub mod mutex;
pub mod static_protected;

pub use mutex::{Mutex, MutexGuard};
pub use static_protected::{KernelStatic, UnlockedStatic};
pub(crate) use worm::Worm;

mod worm;

/// Struct to provide !Send, !Sync traits
#[derive(Default, Copy, Clone)]
#[repr(transparent)]
pub struct PhantomUnsend {
    _t: core::marker::PhantomData<*mut ()>,
}
