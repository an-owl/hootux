use core::fmt::{Debug, Formatter};

#[repr(transparent)]
pub(crate) struct Register<T, M = ReadWrite> {
    inner: T,
    _phantom: core::marker::PhantomData<M>,
}

impl<T, M> Debug for Register<T, M>
where
    T: Debug,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        Debug::fmt(&self.inner, f)
    }
}

impl<T, M> Register<T, M> {
    /// Performs a volatile read and returns the inner data
    pub(crate) fn read(&self) -> T {
        unsafe { core::ptr::read_volatile(&self.inner) }
    }

    /// Uses a volatile write to store the data
    pub(crate) fn write(&mut self, mut src: T)
    where
        M: ReadWriteMarker,
        T: ClearReserved,
    {
        src.clear_reserved();
        unsafe { core::ptr::write_volatile(&mut self.inner, src) }
    }

    pub(crate) fn clear<C: Acknowledge<T>>(&mut self, src: C)
    where
        M: Read1ClearMarker<C>,
    {
        unsafe { core::ptr::write_volatile(&mut self.inner, src.ack()) }
    }

    /// Runs the fn `f` on the inner value writing the result into self
    pub(crate) fn update<F>(&mut self, f: F)
    where
        F: FnOnce(&mut T),
        M: ReadWriteMarker,
        T: ClearReserved,
    {
        let t = &mut self.inner;
        f(t);
        core::hint::black_box(self);
    }

    /// Allows non-volatile access to the inner data
    pub(crate) fn inner(&self) -> &T {
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

/// This struct contains a physical address used by the HBA. The HBA may only support 32 bit addressing
#[derive(Copy, Clone, Debug)]
pub(crate) struct HbaAddr<const ALIGN: u32>(u64);

impl<const ALIGN: u32> HbaAddr<ALIGN> {
    const fn new() -> Self {
        Self(0)
    }

    /// Sets the address of self to `addr`.
    ///
    /// # Panics
    ///
    /// This fn will panic if `addr` is not aligned to `ALIGN`
    pub(crate) fn set(&mut self, addr: u64) {
        assert_eq!(addr & (ALIGN - 1) as u64, 0);
        unsafe { core::ptr::write_volatile(&mut self.0, addr) }
    }

    /// Sets only the low half of the address to `addr`.
    ///
    ///
    ///# Panics
    ///
    /// This fn will panic if `addr` is not aligned to `ALIGN`
    ///
    /// # Safety
    ///
    /// The high 32bits if the address will be not be modified. This should only be used on devices
    /// that use 32bit addressing
    #[allow(dead_code)]
    pub(crate) unsafe fn set_low(&mut self, addr: u32) {
        assert_eq!(addr & ALIGN - 1, 0);
        // casts to u64 to u32
        unsafe { core::ptr::write_volatile(&mut (self.0 as u32), addr) }
    }

    pub(crate) fn read(&self) -> u64 {
        unsafe { core::ptr::read_volatile(&self.0) }
    }
}
