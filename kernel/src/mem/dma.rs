use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::{AllocError, Allocator, Layout};
use core::any::Any;
use core::fmt::Debug;
use core::marker::PhantomData;
use core::ops::DerefMut;
use core::ptr::NonNull;

pub type DmaBuff<'a> = Box<dyn DmaTarget + 'a>;

pub struct DmaGuard<T, C> {
    inner: core::mem::ManuallyDrop<C>,

    _phantom: PhantomData<T>,
    lock: Option<alloc::sync::Arc<core::sync::atomic::AtomicBool>>,
}

impl<T, C> Drop for DmaGuard<T, C> {
    fn drop(&mut self) {
        if !self
            .lock
            .take()
            .is_some_and(|v| v.load(atomic::Ordering::Acquire))
        {
            // SAFETY: Well, we definitely aren't using this anymore
            unsafe { core::mem::ManuallyDrop::drop(&mut self.inner) }
        }
    }
}

impl<T, C> DmaGuard<T, C>
where
    C: DmaPointer<T>,
{
    pub const fn new(data: C) -> Self {
        Self {
            inner: core::mem::ManuallyDrop::new(data),
            _phantom: PhantomData,
            lock: None,
        }
    }
}

impl<T, C> DmaGuard<T, C> {
    pub fn unwrap(mut self) -> C {
        if self
            .lock
            .take()
            .is_some_and(|v| v.load(atomic::Ordering::Acquire))
        {
            panic!("DmaGuard::unwrap(): Called while data was locked");
        }
        // SAFETY: `self` is forgotten immediately after this
        let t = unsafe { core::mem::ManuallyDrop::take(&mut self.inner) };
        core::mem::forget(self);
        t
    }
}

unsafe impl<T: 'static + Send, A: Allocator + Send + 'static> DmaTarget for DmaGuard<T, Vec<T, A>> {
    fn data_ptr(&mut self) -> *mut [u8] {
        let ptr = self.inner.as_mut_ptr();
        let elem_size = size_of::<T>();
        unsafe { core::slice::from_raw_parts_mut(ptr as *mut _, elem_size * self.inner.len()) }
    }
}

unsafe impl<T: Send + 'static, A: Send + Allocator + 'static> DmaTarget for DmaGuard<T, Box<T, A>> {
    fn data_ptr(&mut self) -> *mut [u8] {
        let ptr = self.inner.as_mut() as *mut T as *mut u8;
        let elem_size = size_of::<T>();
        unsafe { core::slice::from_raw_parts_mut(ptr, elem_size) }
    }
}

unsafe impl<'a, T: Send> DmaTarget for DmaGuard<T, &'a mut T> {
    fn data_ptr(&mut self) -> *mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(
                self.inner.deref_mut() as *mut _ as *mut u8,
                size_of_val(&*self.inner),
            )
        }
    }
}

unsafe impl<T, C> DmaClaimable for DmaGuard<T, C>
where
    Self: DmaTarget,
{
    fn claim<'a, 'b>(mut self) -> Option<(DmaClaimed<Self>, Box<dyn DmaTarget + 'b>)> {
        // Lazily constructed, because this may not actually be used.
        if let Some(lock) = self.lock.as_ref() {
            lock.compare_exchange(
                false,
                true,
                atomic::Ordering::Acquire,
                atomic::Ordering::Relaxed,
            )
            .ok()?;
        } else {
            self.lock = Some(alloc::sync::Arc::new(core::sync::atomic::AtomicBool::new(
                true,
            )));
        }

        let b = Box::new(BorrowedDmaGuard {
            data: self.data_ptr(),
            lock: self.lock.as_ref().unwrap().clone(), // Guaranteed to be some
            _phantom: PhantomData,
        });
        Some((DmaClaimed { inner: self }, b))
    }

    fn query_owned(&self) -> bool {
        self.lock
            .as_ref()
            .is_some_and(|v| v.load(atomic::Ordering::Acquire))
    }
}

struct BorrowedDmaGuard<'a> {
    data: *mut [u8],
    lock: alloc::sync::Arc<core::sync::atomic::AtomicBool>,
    _phantom: PhantomData<&'a mut [u8]>,
}

unsafe impl Send for BorrowedDmaGuard<'_> {}

unsafe impl DmaTarget for BorrowedDmaGuard<'_> {
    fn data_ptr(&mut self) -> *mut [u8] {
        self.data
    }
}

impl Drop for BorrowedDmaGuard<'_> {
    fn drop(&mut self) {
        self.lock.store(false, atomic::Ordering::Release);
    }
}

impl<T, C: DmaPointer<T>> From<C> for DmaGuard<T, C> {
    fn from(inner: C) -> Self {
        DmaGuard {
            inner: core::mem::ManuallyDrop::new(inner),
            _phantom: PhantomData,
            lock: None,
        }
    }
}

pub struct StackDmaGuard<'a, T: ?Sized + Send> {
    data: &'a mut T,
}

impl<'a, T: ?Sized + Send> StackDmaGuard<'a, T> {
    /// Constructs a StackDmaGuard for `data`.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `data` does not outlive `self` and that all futures `self`
    /// is given to are completed.
    pub unsafe fn new(data: &'a mut T) -> Self {
        Self { data }
    }
}

unsafe impl<T: ?Sized + Send> DmaTarget for StackDmaGuard<'_, T> {
    fn data_ptr(&mut self) -> *mut [u8] {
        let count = core::mem::size_of_val(self.data);
        let ptr = self.data as *mut _ as *mut u8;
        unsafe { core::slice::from_raw_parts_mut(ptr, count) }
    }
}

mod sealed {
    pub trait Sealed {}
}

pub trait DmaPointer<T>: sealed::Sealed {}

impl<T, A: Allocator> sealed::Sealed for Vec<T, A> {}
impl<T, A: Allocator> DmaPointer<T> for Vec<T, A> {}

impl<T, A: Allocator> sealed::Sealed for Box<T, A> {}
impl<T, A: Allocator> DmaPointer<T> for Box<T, A> {}

pub struct PhysicalRegionDescriber<'a> {
    data: *mut [u8],
    next: usize,

    phantom: PhantomData<&'a ()>,
}

impl PhysicalRegionDescriber<'_> {
    fn next_chunk(&mut self, index: usize) -> Option<u64> {
        // SAFETY: I think this is unsound
        let data = unsafe { &*self.data };
        crate::mem::mem_map::translate_ptr(data.get(index)?)
    }
}

impl Iterator for PhysicalRegionDescriber<'_> {
    type Item = PhysicalRegionDescription;

    fn next(&mut self) -> Option<Self::Item> {
        let base = self.next_chunk(self.next)?;
        // SAFETY: I think this is unsound
        let data = unsafe { &*self.data };

        let mut diff = super::PAGE_SIZE - (base as usize & (super::PAGE_SIZE - 1)).min(data.len()); // diff between next index and base

        loop {
            match self.next_chunk(diff + self.next) {
                // Ok(_) ensures that this is offset is valid
                // match guard checks that addr is contiguous
                Some(addr) if addr - base == diff as u64 => {
                    diff += super::PAGE_SIZE;
                    diff = diff.min(data.len()); // make sure we dont overflow
                }
                // When either of the above checks fail we have reached the end of the region
                _ => break,
            }
            if diff == data.len() {
                break;
            }
        }

        self.next += diff;

        Some(PhysicalRegionDescription {
            addr: base,
            size: diff,
        })
    }
}

/// Describes a contiguous region of physical memory.
///
/// This is used for building Scatter-Gather tables.
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub struct PhysicalRegionDescription {
    /// Starting physical address of the region.
    pub addr: u64,
    /// Length in bytes.
    pub size: usize,
}

impl PhysicalRegionDescription {
    /// Calls `adapt` on `self` until `None` is returned to allow converting `self` between formats.
    /// This can be used with [Iterator::flat_map] provide device specific physical region description formats.
    pub fn adapt<F>(self, adapt: F) -> impl Iterator<Item = PhysicalRegionDescription>
    where
        F: FnMut(&PhysicalRegionDescription) -> Option<PhysicalRegionDescription>,
    {
        PhysicalRegionAdapter {
            adapter: adapt,
            region: self,
        }
    }
    // todo: figure out how to merge this into Self::adapt
    pub fn adapt_iter<F, I>(self, mut adapt: F) -> impl Iterator<Item = PhysicalRegionDescription>
    where
        F: FnMut(&PhysicalRegionDescription) -> I,
        I: IntoIterator<Item = PhysicalRegionDescription>,
    {
        adapt(&self).into_iter()
    }

    /// Returns an adaptor where the maximum region size is limited to `len`
    pub fn limit(self, len: usize) -> impl Iterator<Item = PhysicalRegionDescription> {
        let mut state = 0;
        self.adapt(move |desc| {
            let remain = desc.size - state;
            if state >= desc.size {
                return None;
            }
            let len = desc.size.min(len).min(remain);

            let r = Some(PhysicalRegionDescription {
                addr: desc.addr + state as u64,
                size: len,
            });
            state += len;
            r
        })
    }
}

/// This struct helps with adapting [PhysicalRegionDescription] into different formats.
/// This takes a `Fn` as an argument which takes an immutable reference to a [PhysicalRegionDescription]
/// which contains the region that [PhysicalRegionDescription::adapt] was called on, and a mutable
/// reference which can be used to maintain the adaptors state.
///
/// This implements [Iterator] and will return data from `F`.
#[doc(hidden)]
pub struct PhysicalRegionAdapter<F>
where
    F: FnMut(&PhysicalRegionDescription) -> Option<PhysicalRegionDescription>,
{
    adapter: F,
    region: PhysicalRegionDescription,
}

impl<F> Iterator for PhysicalRegionAdapter<F>
where
    F: FnMut(&PhysicalRegionDescription) -> Option<PhysicalRegionDescription>,
{
    type Item = PhysicalRegionDescription;

    fn next(&mut self) -> Option<Self::Item> {
        let ad = &mut self.adapter;
        ad(&self.region)
    }
}

/// A type that implements DmaTarget can be used for DMA operations.
///
/// `async` DMA operations **must** use an implementor of DmaTarget to safely operate. The argument **must** be
/// taken by value and not by reference, the future should return ownership of the DmaTarget when it completes.
/// See [Embedonomicon](https://docs.rust-embedded.org/embedonomicon/dma.html) for details.
///
/// # Safety
///
/// An implementor must ensure that the DMA region returned by [Self::data_ptr] is owned by `self` is treated as volatile.
///
/// A `DmaTarget` **must** outlive a future that it's used within, regardless of whether the future completes or not,
/// this can be done by ensuring that it is safe for a future to upcast a `dyn DmaTarget + 'a` into `dyn DmaTarget + 'static`.
pub unsafe trait DmaTarget: Send {
    /// Returns a pointer into the target buffer.
    ///
    /// # Safety
    ///
    /// Except exclusive access, implementations must ensure that the returned pointer can be safely cast to a reference.
    fn data_ptr(&mut self) -> *mut [u8];

    /// Returns a Physical region describer, which is an iterator over contiguous regions of
    /// physical memory describing the layout of `self.data_ptr`. All intermediate region boundaries
    /// (not the start or end boundary) must be aligned to [hootux::mem::PAGE_SIZE].
    ///
    /// This takes `self` as `&mut` but does not actually mutate `self` this is to prevent all
    /// accesses to `self` while the PRD is alive.
    fn prd(&mut self) -> PhysicalRegionDescriber<'_> {
        PhysicalRegionDescriber {
            data: self.data_ptr(),
            next: 0,
            phantom: Default::default(),
        }
    }

    fn len(&mut self) -> usize {
        self.data_ptr().len()
    }
}

/// Claimable is intended to solve a problem in [DmaGuard] where a user may want to wrap a
/// `Vec<u64>` read a [crate::fs::file::Read] into it and unwrap back into a `Vec<u64>`.
/// This may only be done by downcasting through [core::any::Any], this is inconvenient,
/// because it requires declaring a type, erasing the type data then trying to re-determine our type data.
///
/// The intention of this trait is to provide a RAII guard similar to a mutex.
/// `self` may not drop or access its wrapped buffer until the return value of [DmaClaimable::claim] is dropped.
///
/// If `self` is dropped while the data is borrowed then the data must be leaked.
pub unsafe trait DmaClaimable: DmaTarget {
    /// This fn returns a [DmaTarget] using the same target buffer as `self`.
    ///
    /// When this fn completes successfully then the returned type (`'b`) "owns" the target data of self (`'a`),
    /// when the returned `'b` is dropped it must return ownership of the target buffer to `'a`.
    /// If `'a` is dropped before `'b` then `'a` must not drop the inner data.
    ///
    /// On completion `self` will be returned as [DmaClaimed]
    ///
    /// The value of [Self::query_owned] indicates whether this function will succeed.
    ///
    /// This is intended for use with futures where the target buffer must use dynamic dispatch.
    /// This allows a borrow to occur while passing ownership of the target data without erasing the
    /// type of `self` thus skipping a downcast back into `Self`
    ///
    /// The lifetimes should be treated as `fn('a) -> 'a` by the caller but `fn('a) -> 'b` must be safe.
    fn claim<'a, 'b>(self) -> Option<(DmaClaimed<Self>, Box<dyn DmaTarget + 'b>)>
    where
        Self: Sized;

    /// Returns `true` if self currently owned the buffer.
    fn query_owned(&self) -> bool;
}

/// A wrapper around a claimed [DmaClaimable] to prevent accessing the buffer.
///
/// Calling [Self::unwrap] will attempt to extract the buffer from the wrapper.
pub struct DmaClaimed<T: DmaClaimable> {
    inner: T,
}

impl<T: DmaClaimable> core::fmt::Debug for DmaClaimed<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DmaClaimed")
            .field("T", &core::any::type_name::<T>())
            .field("can_unwrap", &!self.inner.query_owned())
            .finish()
    }
}

impl<T: DmaClaimable> DmaClaimed<T> {
    /// Attempts to unwrap the buffer calling [DmaClaimable::query_owned] to determine if the inner
    /// value has ownership of its buffer.
    pub fn unwrap(self) -> Result<T, Self> {
        if !self.inner.query_owned() {
            Ok(self.inner)
        } else {
            Err(self)
        }
    }
}

/// Bogus buffer for when no data IO occurs.
///
/// This is just intended for optimisation.
pub struct BogusBuffer;

impl BogusBuffer {
    pub fn boxed() -> Box<Self> {
        Box::new(BogusBuffer)
    }
}

unsafe impl DmaTarget for BogusBuffer {
    fn data_ptr(&mut self) -> *mut [u8] {
        // SAFETY: Always empty
        &mut [] as *mut [u8]
    }
}

/// This represents a region of memory which can be used for a DMA operation.
/// DmaBuffer can be constructed from a `Box<[u8]` or `Vec<u8>`.
///
/// Functions that intend to perform DMA **must** take a `DmaBuffer` by value.
/// See [embedonomicon](https://docs.rust-embedded.org/embedonomicon/dma.html) for more info.
///
/// DmaBuffer acts as as a smart pointer and will free data when dropped.
/// However because DmaBuffer must be used in object safe traits we cannot store the allocator type as a trait,
/// so it is stored as a trait object. This means that constructing a DmaBuffer may require
///
/// When constructing DmaBuffer from a Vec this will note the capacity to prevent leaks but the len
/// will be used as len of the returned DmaBuffer.
///
/// ```
/// #use hootux::mem::dma::*;
///
/// let v = Vec::with_capacity(64);
/// let b: DmaBuffer = v.into();
/// assert_eq!(b,0);
///
///
/// ```
pub struct DmaBuffer {
    ptr: NonNull<u8>,
    len: usize,
    capacity: usize,
    alloc: Box<dyn CastableAllocator>, // We must erase they type to ensure this can be used in dyn compatible traits.
}

impl DmaBuffer {
    pub fn to_box<A: CastableAllocator>(mut self) -> Result<Box<[u8], A>, FromDmaBufferError> {
        if self.alloc.ty_eq::<A>() {
            let ptr = self.ptr;
            let capacity = self.capacity;
            let alloc = core::mem::replace(&mut self.alloc, Box::new(DummyAllocator));

            // SAFETY: `self` must be constructed from a Box/Vec and metadata may mot be modified by `self`
            let rc = unsafe {
                Ok(Box::from_raw_in(
                    core::slice::from_raw_parts_mut(ptr.as_ptr(), capacity),
                    *alloc.as_any().downcast().unwrap(),
                ))
            };

            rc
        } else {
            Err(FromDmaBufferError::IncorrectAllocator(self))
        }
    }

    /// Returns a mutable raw slice into the target buffer for setting up DMA operations.
    ///
    /// # Safety
    ///
    /// Drivers must not use [core::ops::Deref] ot [DerefMut] to set up DMA operations.
    pub fn data_mut(&mut self) -> *mut [u8] {
        &mut **self
    }

    #[allow(unused_mut)]
    pub fn prd(&mut self) -> PhysicalRegionDescriber<'_> {
        PhysicalRegionDescriber {
            data: self.data_mut(),
            next: 0,
            phantom: Default::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns a bogus buffer with a size of `0`
    // I cant make this const :(
    pub fn bogus() -> Self {
        Self {
            ptr: NonNull::dangling(),
            len: 0,
            capacity: 0,
            alloc: Box::new(alloc::alloc::Global), // This is a ZST
        }
    }
}

impl Debug for DmaBuffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DmaBuffer")
            .field("ptr", &self.ptr)
            .field("len", &self.len)
            .field("capacity", &self.capacity)
            .field("alloc", &self.alloc.ty_name())
            .finish()
    }
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        // SAFETY:
        // - from_size_align_unchecked: align is 1 so it will not overflow. `align` is not `0` and is 2^0
        // - deallocate: We are the sole owner of the memory referenced by the pointer.
        // The layout accurately describes the memory region because self.capacity is fetched
        // from the smart pointer that was used to initially construct `self`.
        unsafe {
            self.alloc.deallocate(
                self.ptr,
                Layout::from_size_align_unchecked(self.capacity, 1),
            )
        }
    }
}

impl core::ops::Deref for DmaBuffer {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        // SAFETY: See Drop impl
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl core::ops::DerefMut for DmaBuffer {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl<A: CastableAllocator + Clone> From<Vec<u8, A>> for DmaBuffer {
    fn from(value: Vec<u8, A>) -> Self {
        let alloc = Box::new(value.allocator().clone());
        let capacity = value.capacity();
        let len = value.len();

        Self {
            ptr: NonNull::from_mut(Vec::leak(value)).cast(),
            len,
            capacity,
            alloc,
        }
    }
}

impl<A: CastableAllocator + Clone> From<Box<[u8], A>> for DmaBuffer {
    fn from(value: Box<[u8], A>) -> Self {
        let alloc = Box::new(Box::allocator(&value).clone());
        let len = value.len();

        Self {
            ptr: NonNull::from_mut(Box::leak(value)).cast(),
            len,
            capacity: len,
            alloc,
        }
    }
}

impl<A: CastableAllocator> TryFrom<DmaBuffer> for Vec<u8, A> {
    type Error = FromDmaBufferError;
    fn try_from(mut value: DmaBuffer) -> Result<Self, Self::Error> {
        if value.alloc.ty_eq::<A>() {
            let DmaBuffer {
                ptr, len, capacity, ..
            } = value;
            let alloc = core::mem::replace(&mut value.alloc, Box::new(DummyAllocator));
            core::mem::forget(value);
            unsafe {
                Ok(Vec::from_raw_parts_in(
                    ptr.as_ptr(),
                    len,
                    capacity,
                    *alloc.as_any().downcast().unwrap(),
                ))
            }
        } else {
            Err(FromDmaBufferError::IncorrectAllocator(value))
        }
    }
}

#[doc(hidden)]
pub trait CastableAllocator: Allocator + Any + 'static {
    fn ty_name(&self) -> &'static str;

    fn as_any(self: Box<Self>) -> Box<dyn Any>;

    fn ty_id(&self) -> core::any::TypeId;
}

impl<T> CastableAllocator for T
where
    T: Allocator + Any + 'static,
{
    fn ty_name(&self) -> &'static str {
        core::any::type_name::<T>()
    }

    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }

    fn ty_id(&self) -> core::any::TypeId {
        core::any::TypeId::of::<T>()
    }
}

impl dyn CastableAllocator {
    fn ty_eq<T: Any>(&self) -> bool {
        self.ty_id() == core::any::TypeId::of::<T>()
    }
}

#[derive(Debug)]
pub enum FromDmaBufferError {
    IncorrectAllocator(DmaBuffer),
}

/// This exists for the sole purpose of being able to swap out the allocator of a [DmaBuffer]
/// while guaranteeing a ZST.
#[derive(Copy, Clone)]
#[doc(hidden)]
struct DummyAllocator;

unsafe impl Allocator for DummyAllocator {
    fn allocate(&self, _layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        unimplemented!()
    }

    unsafe fn deallocate(&self, _ptr: NonNull<u8>, _layout: Layout) {
        unimplemented!()
    }
}

#[test_case]
#[cfg(test)]
fn test_dmaguard() {
    use crate::{alloc_interface, mem};
    let mut b =
        alloc::vec::Vec::new_in(alloc_interface::DmaAlloc::new(mem::MemRegion::Mem64, 4096));
    b.resize(0x4000, 0u8);
    let mut g = mem::dma::DmaGuard::from(b);

    let g_prd = g.prd();
    let mut prd_cmp = Vec::new();
    for i in g_prd {
        prd_cmp.push(alloc::format!("{:x?}", i));
    }

    let mut t = g.claim().unwrap();
    assert!(g.claim().is_none());
    for (p, c) in t.prd().zip(prd_cmp) {
        assert_eq!(c, alloc::format!("{:x?}", p))
    }

    drop(t);
    g.unwrap();

    let mut b =
        alloc::vec::Vec::new_in(alloc_interface::DmaAlloc::new(mem::MemRegion::Mem64, 4096));
    b.resize(0x4000, 0u8);
    let mut g = mem::dma::DmaGuard::from(b);
    let t = g.claim();
    let helper = g.data_ptr();

    drop(g);

    x86_64::instructions::nop();
}
