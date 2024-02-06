/// Write Once Read Many
///
/// Worm is designed to be used to store system statics that need to be initialized at runtime.
#[derive(Default, Debug)]
pub(crate) struct Worm<T> {
    inner: core::cell::UnsafeCell<Option<T>>,
}

impl<T> Worm<T> {
    pub(crate) const fn new() -> Self {
        Self {
            inner: core::cell::UnsafeCell::new(None),
        }
    }
    /// Sets the inner value
    ///
    /// # Panics
    ///
    /// This fn will panic if this fn has already been called on `self`.
    ///
    /// # Safety
    ///
    /// This is racy it should not be called after multiprocessing has been started.
    #[cfg_attr(debug_assertions, track_caller)]
    pub(crate) unsafe fn write(&self, v: T) {
        let t = unsafe { &mut *self.inner.get() };
        if t.is_none() {
            *t = Some(v)
        } else {
            panic!("Worm already initialized");
        };
    }

    /// Returns a reference to the inner data
    ///
    /// # Panics
    ///
    /// This fn will panic if [Self::write] has not been called on `self`
    #[cfg_attr(debug_assertions, track_caller)]
    pub(crate) fn read(&self) -> &T {
        // SAFETY: This is safe because the returned value is not mutably referenced
        let t = unsafe { &*self.inner.get() };
        let r = t.as_ref();
        if t.is_some() {
            r.unwrap()
        } else {
            panic!("Worm not initialized");
        }
    }

    #[allow(dead_code)]
    /// Returns whether or not [Self::write] has been called
    pub(crate) fn is_set(&self) -> bool {
        // SAFETY: see Self::read
        unsafe { &*self.inner.get() }.is_some()
    }
}

/// See [Worm::read] for info
impl<T> core::ops::Deref for Worm<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.read()
    }
}

unsafe impl<T> Sync for Worm<T> where T: Sync {}
unsafe impl<T> Send for Worm<T> where T: Send {}
