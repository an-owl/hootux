use core::ops::{Deref, DerefMut};
use core::sync::atomic::Ordering;

pub struct Mutex<T> {
    lock: core::sync::atomic::AtomicBool,
    inner: core::cell::UnsafeCell<T>,
}

/// Native implementation of Mutex, capable of being forcibly acquired without being unlocked.
impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            inner: core::cell::UnsafeCell::new(data),
            lock: core::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn lock(&self) -> MutexGuard<T> {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.is_locked() {
                core::hint::spin_loop();
            }
        }

        unsafe { self.make_guard() }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<T>> {
        if let Ok(_) =
            self.lock
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        {
            unsafe { Some(self.make_guard()) }
        } else {
            None
        }
    }

    /// Generates MutexGuard for self
    ///
    /// #Saftey
    ///
    /// This function is unsafe because it generates a reference to `self` regardless of whether
    /// self is locked. The programmer must ensure that `self` is locked before this fn is run
    unsafe fn make_guard(&self) -> MutexGuard<T> {
        MutexGuard {
            lock: &self.lock,
            data: &mut *self.inner.get(),
        }
    }

    pub fn is_locked(&self) -> bool {
        self.lock.load(Ordering::Relaxed)
    }

    /// Forcibly acquires `T`, **without** unlocking `self`. This is intended for interrupts and
    /// should not be used otherwise.
    ///
    /// #Saftey
    ///
    /// This function is incredibly unsafe. and should only be used in circumstances where `T` must
    /// be accessed. Any access should expect that `T` may be in an invalid state. Any changes that
    /// may affect data integrity should be done asynchronously.
    pub unsafe fn force_acquire(&self) -> &mut T {
        &mut *self.inner.get()
    }
}

unsafe impl<T> Sync for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    lock: &'a core::sync::atomic::AtomicBool,
    data: &'a mut T,
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.store(false, Ordering::Release);
    }
}
