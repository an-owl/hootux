use core::cell::{Cell, UnsafeCell};
use core::ops::{Deref, DerefMut};
use core::sync::atomic;
use core::sync::atomic::Ordering;

pub struct Mutex<T> {
    lock: atomic::AtomicBool,
    inner: UnsafeCell<T>,
}

/// Native implementation of Mutex, capable of being forcibly acquired without being unlocked.
impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            inner: UnsafeCell::new(data),
            lock: atomic::AtomicBool::new(false),
        }
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<T> {
        loop {
            if let Some(t) = self.try_lock() {
                return t;
            } else {
                core::hint::spin_loop()
            }
        }
    }

    #[inline]
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
    lock: &'a atomic::AtomicBool,
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

/// A Reentrant Mutex allows multiple locks within the same thread this is useful for memory
/// allocators which may require each other allocators to operate correctly which may cause a
/// deadlock.
///
/// This struct works by using a control bit to manage access to the lock metadata if the owner is
/// unset then the current thread may set itself, if the owner is set and the current owner is the
/// current thread it may lock the contained data if it is not then the current thread must wait.
///
/// #Safety
/// While this type is safe it should not be used in interrupts as the contained data may cause UB.
/// This may occur when an interrupt is called while modifying `self.data`
pub struct ReentrantMutex<T> {
    data: UnsafeCell<T>,
    control: atomic::AtomicBool,
    owner: Cell<Option<u32>>,
    lock_count: Cell<usize>,
}

unsafe impl<T: Send> Send for ReentrantMutex<T> {}
unsafe impl<T: Send> Sync for ReentrantMutex<T> {}

pub struct ReentrantMutexGuard<'a, T> {
    master: &'a ReentrantMutex<T>,
    _marker: core::marker::PhantomData<T>,
    _unsend: super::PhantomUnsend,
}

impl<'a, T> ReentrantMutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            data: UnsafeCell::new(data),
            lock_count: Cell::new(0),
            owner: Cell::new(None),
            control: atomic::AtomicBool::new(false),
        }
    }

    /// Spins until control lock bit can be acquired
    ///
    /// #Safety
    ///
    /// [Self::desync] must be called afterward. Failure to do so is not UB will almost definitely
    /// deadlock. `desync` must also be called only after all modifications are made making making
    /// changes without sync is UB.
    unsafe fn sync(&self) {
        while let Err(_) =
            self.control
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        {
            core::hint::spin_loop();
        }
    }

    /// Safely unlocks control of self. [Self::sync] should've been called before this
    fn desync(&self) {
        self.control.store(false, Ordering::Release);
    }

    /// Attempts to lock control bit returns None if control bit is locked.
    #[inline]
    pub fn try_lock_inner(&self) -> Option<ReentrantMutexGuard<T>> {
        if let Ok(_) =
            self.control
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        {
            match self.owner.get() {
                None => {
                    let r = ReentrantMutexGuard {
                        master: &self,
                        _marker: core::marker::PhantomData,
                        _unsend: super::PhantomUnsend::default(),
                    };

                    self.owner.set(Some(crate::who_am_i()));

                    self.lock_count.set(1);
                    self.control.store(false, Ordering::Release);
                    return Some(r);
                }

                Some(owner) if owner == crate::who_am_i() => {
                    let r = ReentrantMutexGuard {
                        master: &self,
                        _marker: core::marker::PhantomData,
                        _unsend: super::PhantomUnsend::default(),
                    };

                    let nc = self
                        .lock_count
                        .get()
                        .checked_add(1)
                        .expect("ReentrantMutex lock overflow");
                    self.lock_count.set(nc);

                    self.control.store(false, Ordering::Release);
                    return Some(r);
                }

                _ => {}
            }
        }
        None
    }

    /// Attempts to lock the inner data returns `None` if data is already locked.
    /// This fn is different from [Self::try_lock_inner] because it will spin while waiting for the
    /// control bit, and exits if the mutex is already locked.
    #[inline]
    pub fn try_lock(&self) -> Option<ReentrantMutexGuard<T>> {
        loop {
            if let Some(t) = self.try_lock_inner() {
                return Some(t);
            } else {
                core::hint::spin_loop();
            }
        }
    }

    #[inline]
    pub fn lock(&self) -> ReentrantMutexGuard<T> {
        loop {
            if let Some(t) = self.try_lock() {
                return t;
            } else {
                core::hint::spin_loop();
            }
        }
    }
}

impl<'a, T> Deref for ReentrantMutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: This is safe because the inner data may only be locked by a single thread
        unsafe { &*self.master.data.get() }
    }
}

impl<'a, T> DerefMut for ReentrantMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: This is safe because the inner data may only be locked by a single thread
        unsafe { &mut *self.master.data.get() }
    }
}

impl<'a, T> Drop for ReentrantMutexGuard<'a, T> {
    fn drop(&mut self) {
        // could possibly store original count for error checking
        // SAFETY: this is safe because desync is called
        unsafe { self.master.sync() };

        let nc = self.master.lock_count.get() - 1;
        self.master.lock_count.set(nc);

        if nc == 0 {
            self.master.owner.set(None);
        }

        self.master.desync()
    }
}

/// MentallyUnstableMutex is for storing data that shouldn't be shared across CPUs but requires [Send] regardless,
/// and is intended to be used for debugging.
/// In debug builds this struct will panic if [MentallyUnstableMutex::lock] is called while `self` is already locked.
/// In release builds this does not act as a mutex and locks are skipped.
pub struct MentallyUnstableMutex<T> {
    #[cfg(debug_assertions)]
    lock: atomic::AtomicBool,
    inner: UnsafeCell<T>,
}

impl<T> MentallyUnstableMutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            #[cfg(debug_assertions)]
            lock: atomic::AtomicBool::new(false),
            inner: UnsafeCell::new(data),
        }
    }

    #[cfg_attr(debug_assertions, track_caller)]
    /// Locks self and returns a reference to the inner data
    ///
    /// # Panics
    ///
    /// Debug builds will panic if `self` is already locked
    ///
    /// # Safety
    ///
    /// In release mode this is not safe.
    pub fn lock(&self) -> impl MutexGuardTrait<T> {
        #[cfg(debug_assertions)]
        {
            if let Some(_) =
                self.lock
                    .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            {
                MutexGuard {
                    lock: &self.lock,
                    data: unsafe { &mut *self.inner.get() },
                }
            } else {
                panic!(
                    "⣿⣿⣿⣿⣿⣿⣿⣿⣿⣧⣼⣧⣴⣷⣞⠛⢷⡿⠓⠶⣿⣿⣦⣄⢀⣤⣤⣭⠉⠁⡀⣼⢿⣟⣓⢠⡀⠀⢸⣇⠀⠀⠀⢀⡀⢸⠞⠋⠀⠀⠀⠀⠀⠀⠁⠈⠀⣙⣻⣿⣿⣿⣿⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡏⢠⣶⡟⠀⣠⡀⠀⠀⠋⠻⢷⣄⠁⣈⣠⣶⠶⠛⠛⠉⠉⠉⠉⠛⠳⢦⣄⡈⠙⠂⠲⠀⠀⠀⠀⠀⠀⢰⡖⢀⣴⣿⣿⣿⣿⣿⣿⣿⣿
⡏⠉⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣍⣹⣿⣿⣿⣆⣼⣇⠀⠀⢀⣼⠿⠋⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⠛⢷⣄⠀⠀⣄⠀⣀⢀⣠⣴⡟⠉⠉⢀⠈⢹⣿⣿⣿⣿⣿
⣛⢢⣴⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠁⠀⣀⣴⠟⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠻⣆⢠⣤⣤⣽⣟⠛⠿⠿⢿⠆⠀⢤⡈⢻⣿⣿⣿⣿
⣿⣿⡟⠻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⣷⠟⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢉⣿⠟⣍⠉⠻⣿⣿⣶⣾⣿⣷⣿⣿⣦⣽⣻⣿⣿
⣿⣯⡀⢤⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⠏⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢸⡟⢸⡟⣆⠀⠘⣿⣿⣿⣿⣿⡿⢿⣿⣿⣿⣿⣿
⣿⣿⣷⣾⣼⣿⣿⣿⣿⣿⣿⣿⣿⣿⠋⢻⣿⣿⣿⠀⢙⣿⠃⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢸⡇⠸⣇⣿⠀⠀⣹⣿⣿⣿⣿⣶⣶⣾⣿⣼⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠀⠀⠉⠛⣿⡿⠏⠀⠀⠀⠀⠀⠀⠀⠀⢀⡀⠀⠀⠀⠀⠀⠀⢀⡂⠀⠀⠀⠀⠀⠀⠰⠟⠁⢶⣬⣍⣤⣶⠿⣿⣿⡿⣿⣿⣿⢿⣿⢿⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⢿⡀⠀⣠⣞⡉⠀⠀⠀⠀⠀⠀⣀⣤⣾⣿⣿⣿⣿⣦⡀⠀⠀⠀⠀⠹⣦⡀⠀⠀⠀⠀⠀⠀⣴⡿⠋⠉⠈⠁⣀⣾⣿⡃⢉⡙⠻⡿⣿⠸⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⣴⣿⠟⠻⣦⠀⠀⠀⠀⢰⡿⠟⠁⠀⠀⠈⠙⠛⠻⣦⡀⠀⠀⠀⠈⣿⣷⡀⠀⠀⠀⣾⠛⠷⠀⠀⠀⠀⠉⠉⠉⠀⠈⠀⠀⢐⠀⠀⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠟⣻⣿⣿⠀⠀⠈⢷⡀⠀⠀⢸⡗⠀⠀⠀⠀⠀⠀⠀⠀⠈⠙⠂⠀⠀⠀⣼⣿⠃⠀⠀⠀⠁⠀⠀⠀⠀⠀⠀⠘⠓⠀⠀⢀⣤⡒⣴⡖⠀⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⢿⣿⣴⣿⣿⣿⡄⠀⠀⠀⣷⡀⠀⠸⣧⠀⠸⡆⠀⠀⠀⠀⠀⠀⠀⣀⣠⡾⠛⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠾⣿⣷⡿⢠⣿⣿⣿
⣿⣿⣿⣿⣯⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣤⣀⣀⣈⣳⠀⠀⢻⡄⣠⣷⣶⡟⢛⠛⢳⣶⣾⢻⠏⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠠⣤⣶⣿⣿⣿⣿
⣿⣿⣿⣿⣼⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⡇⠀⣌⣿⠃⠀⠀⠀⠻⣿⣝⡃⠼⣾⠾⠃⣠⠟⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⠻⢿⣿⣿⣿
⣇⣻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠃⠷⢼⣿⠏⠀⠀⠀⠀⠀⠀⠉⠉⠩⠥⠖⠛⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠙⢿⣿
⣿⣿⣿⣿⣿⡻⢿⣽⣿⣿⣿⣿⣿⣿⣿⣿⣿⡏⠀⠀⠉⠀⠀⠀⠀⠀⠀⠀⠀⣀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠙
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣷⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⢻⣶⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⣴
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠿⣿⡆⠀⠀⠀⠀⠀⠀⣠⣤⣤⣴⣟⢿⣷⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢠⣾⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣁⢠⣿⡄⠀⠀⢤⣠⠊⠿⣿⣿⣿⣿⠿⠛⠃⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢸⣿⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠃⣿⠷⢟⣾⡛⣿⣆⠀⠀⠙⠳⠶⠿⠛⠋⠁⠀⠀⠀⠀⠒⢦⣄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢠⣿⣿⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡿⠁⠀⣿⢀⣿⣿⣷⠹⣿⣧⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠈⠙⢦⣀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢠⣿⣿⣿⡿⢿
⣿⣶⣿⣿⣿⣿⣿⣿⣿⣿⣿⠃⠀⢠⣿⢸⣿⢹⣿⡇⢻⣟⣆⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠙⢧⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣿⡿⢿⡿⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡇⠀⠀⢸⠟⣷⣿⣪⣻⣇⢺⣿⢻⣆⠠⢄⡀⠠⠤⠤⠶⠶⠛⠛⠛⠛⠛⠛⠂⠀⠀⢿⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢼⣿⠣⢺⣿⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠀⠀⠀⢸⣿⣿⣟⣿⠘⠻⠸⣿⣽⣿⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢠⣴⡿⠀⢠⣾⣿⡇⣸
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠀⠀⠀⢸⣧⣿⢿⣷⡉⠀⠸⣿⣿⡈⣷⡀⠀⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣰⣿⢋⣤⣶⣾⣿⠉⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⠀⠀⠀⠈⣿⡿⣾⣿⣧⠀⢰⣿⣿⣿⢹⣿⡄⠀⠁⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣰⡿⢇⣴⣿⣿⣟⢿⣿⠿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡀⠀⠀⠀⠹⣇⣿⠙⣿⣥⣿⣹⣿⣿⣟⣙⣿⡄⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⡤⠀⠀⠀⠀⠀⣀⣤⡶⢋⡵⣣⣾⣿⣿⢿⣿⣿⡏⠀⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡇⠀⠀⠀⠀⢿⡎⠻⢿⣟⢹⣿⣷⣿⣿⠟⢿⣧⠘⢦⣀⠀⠀⠀⠀⠀⠀⠀⠀⠀⣀⡤⠚⠋⠀⠀⠀⠀⣠⡼⣿⣛⠒⢋⣴⣿⣿⣿⣿⣿⣿⣿⡄⣛⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⡀⠀⠀⠀⠘⣷⣾⣿⣿⣿⣿⣿⣿⠻⣿⣏⠙⣧⠀⠈⠳⣖⣶⣤⣤⣤⣶⣶⣟⣫⣒⡈⢠⣤⣤⣾⣟⣻⠗⠋⣀⣶⣿⣿⣿⣿⣿⣯⣿⣿⣿⣿⣿⣿
⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣇⠀⠀⠀⠀⣿⣝⣿⣿⣿⣿⣿⣋⣷⣿⣿⣧⡌⢷⣄⠀⠈⠙⢥⣤⣍⣻⡟⡛⢻⠛⢛⢻⣿⣑⠇⠉⣉⣴⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣾
⣿⣿⣿⡿⢿⡿⣿⣿⣿⣿⣿⣿⠀⠀⠀⠀⢿⣿⣏⣿⣿⣿⣿⣿⢿⣿⣿⣿⢿⣦⡨⣷⣆⡀⠀⠀⠂⠀⠈⠓⠾⠉⠟⠛⠀⣀⣴⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⢿⣿
⣀⠀⠀⠀⠛⠛⠛⠛⠛⠛⣻⣟⣧⠀⠀⠀⢸⣟⣿⣾⢿⣧⣿⣿⣆⣿⡿⣿⣼⣿⣿⣿⣎⣻⣦⣀⠀⠀⠀⠀⠀⠀⠀⢀⣼⣿⣿⣾⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿
⠀⠀⠀⠀⠀⠀⠤⡤⠤⠤⠤⣤⣼⠀⠀⠀⠈⣿⣟⣿⣿⣿⡻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣦⣄⣤⣤⣤⣶⣿⣿⣿⣿⣿⣿⡟⢻⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿"
                )
            }
        }

        #[cfg(not(debug_assertions))]
        unsafe {
            &mut *self.inner.get()
        }
    }
}

// SAFETY: Boi this shit ain't safe at all.
unsafe impl<T> Send for MentallyUnstableMutex<T> {}
unsafe impl<T> Sync for MentallyUnstableMutex<T> {}

impl<'a, T> MutexGuardTrait<'a, T> for MentallyUnstableMutexGuard<'a, T> {}
impl<'a, T> MutexGuardTrait<'a, T> for &'a mut T {}

trait MutexGuardTrait<'a, T>: DerefMut<Target = T> {}
