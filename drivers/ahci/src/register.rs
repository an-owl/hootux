#[repr(transparent)]
pub struct Register<T, M = ReadWrite> {
    inner: T,
    _phantom: core::marker::PhantomData<M>,
}

impl<T, M> Register<T, M> {
    /// Performs a volatile read and returns the inner data
    pub fn read(&self) -> T {
        unsafe { core::ptr::read_volatile(&self.inner) }
    }

    /// Uses a volatile write to store the data
    pub fn write(&mut self, mut src: T)
    where
        M: ReadWriteMarker,
        T: ClearReserved,
    {
        src.clear_reserved();
        unsafe { core::ptr::write_volatile(&mut self.inner, src) }
    }

    pub fn clear<C: Acknowledge<T>>(&mut self, src: C)
    where
        M: Read1ClearMarker<C>,
    {
        unsafe { core::ptr::write_volatile(&mut self.inner, src.ack()) }
    }

    /// Runs the fn `f` on the inner value writing the result into self
    pub fn update<F>(&mut self, f: F)
    where
        F: FnOnce(T) -> T,
        M: ReadWriteMarker,
        T: ClearReserved,
    {
        let t = self.read();
        self.write(f(t));
    }

    /// Allows non-volatile access to the inner data
    pub fn inner(&self) -> &T {
        &self.inner
    }
}

/// This struct is used to clear flags that require writing 1 to clear.
///
/// # Safety
///
/// It is the responsibility of the implementation to ensure that reserved bits are cleared to 0.
pub unsafe trait Acknowledge<T> {
    /// Creates an acknowledgement for `other`. This allows responding while preserving reserved bits.
    fn ack(self) -> T
    where
        Self: Sized;
}

/// Used by [Register::write] to clear all reserved bits.
///
/// # Safety
///
/// It is the responsibility of the implementation to ensure that all reserved bits are set to 0.
pub unsafe trait ClearReserved {
    fn clear_reserved(&mut self);
}

pub struct ReadWrite;

pub trait ReadWriteMarker {}

impl ReadWriteMarker for ReadWrite {}

pub struct ReadOnly;

pub trait ReadOnlyMarker {}

impl ReadOnlyMarker for ReadOnly {}

#[derive(Default)]
pub struct ReadWriteClear<T> {
    _phantom: core::marker::PhantomData<T>,
}

impl<T> Read1ClearMarker<T> for ReadWriteClear<T> {}

pub trait Read1ClearMarker<T> {}

#[derive(Default)]
/// A bit of everything
pub struct Aboe<T> {
    _phantom: core::marker::PhantomData<T>,
}

impl<T> ReadWriteMarker for Aboe<T> {}

impl<T> Read1ClearMarker<T> for Aboe<T> {}
