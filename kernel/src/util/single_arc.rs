//! Similar to a [alloc::sync::Arc] allows only one strong reference which can be mutable.
//!
//! Internally uses a box so the data can be freed and memory recovered before all [Weak] are dropped.

use alloc::boxed::Box;

/// Provides a shared reference to some data. Unlike [alloc::sync::Arc] This allows mutable access.
/// However, any data may only have one SingleArc pointing to it. When this is dropped the data is
/// dropped and the memory is freed.
pub struct SingleArc<T: ?Sized> {
    // SAFETY: We can freely dereference this. While self exists so does *inner
    inner: *mut SingleArcInner<T>,
}

unsafe impl<T> Send for SingleArc<T> where T: Send {}
unsafe impl<T> Sync for SingleArc<T> where T: Sync {}

impl<T: ?Sized> SingleArc<T> {
    pub fn new(data: Box<T>) -> Self {
        let inner = Box::leak(Box::new(SingleArcInner {
            weak_count: atomic::Atomic::new(0),
            data: core::cell::UnsafeCell::new(Some(data)),
        }));
        Self { inner }
    }

    pub fn downgrade(&self) -> Weak<T> {
        unsafe { &*self.inner }
            .weak_count
            .fetch_add(1, atomic::Ordering::Acquire);
        Weak {
            inner: spin::Mutex::new(Some(self.inner)),
        }
    }

    pub fn take(self) -> Box<T> {
        let inner = unsafe { &*self.inner };
        // SAFETY: SingleArc owns this data and can do what it wants with it
        unsafe { &mut *inner.data.get() }.take().unwrap()
    }
}

impl<T: ?Sized> core::ops::Deref for SingleArc<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // SAFETY: Only Self can access the data and only one may exist which references the data
        unsafe { (&*(&*self.inner).data.get()).as_ref().unwrap() }
    }
}

impl<T: ?Sized> core::ops::DerefMut for SingleArc<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: Only Self can access the data and only one may exist which references the data
        unsafe { (&mut *(&mut *self.inner).data.get()).as_mut().unwrap() }
    }
}

impl<T: ?Sized> Drop for SingleArc<T> {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: self.inner is synchronized and owned by self, so we can access it freely
            let inner = &mut *self.inner;
            // SAFETY: We own this data
            let _ = (&mut *inner.data.get()).take();
            if inner.weak_count.load(atomic::Ordering::Relaxed) == 0 {
                // SAFETY: this is constructed using Box::new and leaked, we just undo it here.
                // We also check that this is not aliased
                let _ = Box::from_raw(inner);
            }
        }
    }
}

impl<T: ?Sized> From<Box<T>> for SingleArc<T> {
    fn from(value: Box<T>) -> Self {
        let inner = Box::leak(Box::new(SingleArcInner {
            weak_count: atomic::Atomic::new(0),
            data: core::cell::UnsafeCell::new(Some(value)),
        }));

        Self { inner }
    }
}

/// Weak reference to the inner data.
pub struct Weak<T: ?Sized> {
    inner: spin::Mutex<Option<*mut SingleArcInner<T>>>,
}

impl<T: ?Sized> Default for Weak<T> {
    fn default() -> Self {
        Self {
            inner: spin::Mutex::new(None),
        }
    }
}

impl<T: ?Sized> Drop for Weak<T> {
    fn drop(&mut self) {
        unsafe {
            if let Some(inner) = self.inner.lock().as_mut() {
                // SAFETY: We dont modify any non-atomics unless we guarantee that weare the only
                // reference to the data.
                let inner = &mut **inner;
                // Check that we are the only weak and if the Strong was dropped.
                if inner.weak_count.fetch_sub(1, atomic::Ordering::Acquire) == 1
                    && (&*inner.data.get()).is_none()
                {
                    // SAFETY: this is constructed using Box::new and leaked, we just undo it here
                    // We also check that this is not aliased
                    let _ = Box::from_raw(inner);
                }
            }
        };
    }
}

unsafe impl<T: Sync + ?Sized> Sync for Weak<T> {}
unsafe impl<T: Send + ?Sized> Send for Weak<T> {}

impl<T: ?Sized> Weak<T> {
    pub fn get(&self) -> Option<*mut T> {
        // Absolute abomination
        // SAFETY: All data in here is guaranteed to be accessible.
        // SAFETY: This *could* cause a use after free. But its up to the caller to actually
        // validate that the returned pointer is valid.
        let l = self.inner.lock();
        let arc = unsafe { &mut **l.as_ref()? };
        let data = unsafe { &mut *arc.data.get() };
        Some(&mut **data.as_mut()? as *mut T)
    }

    /// Drops any inner references, freeing memory is possible.
    pub fn clear(&mut self) {
        core::mem::take(self);
    }

    /// Sets self to point to the data contained within `strong`.
    ///
    /// returns whether this was successfully rebound.
    pub fn set(&mut self, strong: &SingleArc<T>) -> Result<(), ()> {
        if self.inner.lock().is_none() {
            // If self is unbound
            *self = strong.downgrade();
            return Ok(());
        }
        let l = self.inner.lock();
        if let Some(ref inner) = *l {
            // else if the data has been dropped
            let inner = unsafe { &mut **inner };
            let data = unsafe { &mut *inner.data.get() };
            if data.is_none() {
                drop(l);
                // drop data and bind self
                self.clear();
                *self = strong.downgrade();
                return Ok(());
            }
        }
        Err(())
    }

    /// Compares the inner value with `other` if they have the same address then this returns true.
    /// If `other` and the inner value of `self` do not have the same address, or `self` does not
    /// contain any data then returns true.
    pub fn cmp_t(&self, other: *const T) -> bool {
        if let Some(inner) = self.get() {
            core::ptr::eq(inner, other)
        } else {
            true
        }
    }
}

struct SingleArcInner<T: ?Sized> {
    weak_count: atomic::Atomic<usize>,
    data: core::cell::UnsafeCell<Option<Box<T>>>,
}
