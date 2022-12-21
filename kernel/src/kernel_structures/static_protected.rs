use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};

/// KernelStatic is used storing statics that are uninitialized at startup and provide a interior
/// mutability for the interior value.
///
/// Interrupts should not mutate interior values unless data security can be guaranteed. Where data
/// security cannot be guaranteed the interrupt should dispatch a task to write asynchronously.
///
/// #Panics
///
/// Attempting to access inner values will panic if the inner variable is not initialized.
pub struct KernelStatic<T> {
    inner: super::mutex::Mutex<Option<T>>,
}

impl<T> KernelStatic<T> {
    pub(crate) const fn new() -> Self {
        Self {
            inner: super::mutex::Mutex::new(None),
        }
    }

    pub fn init(&self, new: T) {
        *self.inner.lock() = Some(new);
    }

    pub fn get(&self) -> Ref<T> {
        let lock = self.inner.lock();
        if let Some(_) = *lock {
            Ref::new(lock)
        } else {
            panic!("Tried to get uninitialized kernel static")
        }
    }

    pub(crate) unsafe fn force_get_mut(&self) -> &mut T {
        if let Some(t) = self.inner.force_acquire() {
            return t;
        } else {
            panic!("Tried to get uninitialized kernel static")
        }
    }

    /// Attempts to lock inner returns None on fail.
    ///
    /// #Panics
    ///
    /// This fn will panic if if `self` is uninitialized
    pub fn try_get_mut(&self) -> Option<Ref<T>> {
        if let Some(guard) = self.inner.try_lock() {
            if let Some(_) = guard.as_ref() {
                Some(Ref::new(guard))
            } else {
                panic!("Tried to get uninitialized kernel static")
            }
        } else {
            None
        }
    }
}

/// Wrapper for static data, to prevent the need to unwrap the inner data.
pub struct Ref<'a, T> {
    inner: super::mutex::MutexGuard<'a, Option<T>>,
}

impl<'a, T> Ref<'a, T> {
    fn new(mutex: super::mutex::MutexGuard<'a, Option<T>>) -> Self {
        Self { inner: mutex }
    }
}

impl<'a, T> Deref for Ref<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        (*self.inner).as_ref().unwrap()
    }
}

impl<'a, T> DerefMut for Ref<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        (*self.inner).as_mut().unwrap()
    }
}

pub struct UnlockedStatic<T> {
    inner: UnsafeCell<Option<T>>,
}

impl<T> UnlockedStatic<T> {
    pub(crate) const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(None),
        }
    }

    pub fn init(&self, data: T) {
        unsafe { *self.inner.get() = Some(data) };
    }

    pub fn get(&self) -> &mut T {
        unsafe { (*self.inner.get()).as_mut().unwrap() }
    }
}
