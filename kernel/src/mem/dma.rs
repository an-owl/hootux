use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Allocator;
use core::marker::PhantomData;

pub struct DmaGuard<T,C> {
    inner: C,

    _phantom: PhantomData<T>,
}

impl<T,C> DmaGuard<T,C> {
    pub fn unwrap(self) -> C {
        self.inner
    }
}

unsafe impl<T, A: Allocator> DmaTarget for DmaGuard<T,Vec<T, A>> {
    fn as_mut(&mut self) -> *mut [u8] {
        let ptr = self.inner.as_mut_ptr();
        let elem_size = size_of::<T>();
        unsafe { core::slice::from_raw_parts_mut(ptr as *mut _, elem_size * self.inner.len()) }
    }
}

unsafe impl<T, A: Allocator> DmaTarget for DmaGuard<T,Box<T,A>> {
    fn as_mut(&mut self) -> *mut [u8] {
        let ptr = self.inner.as_mut() as *mut T as *mut u8;
        let elem_size = size_of::<T>();
        unsafe { core::slice::from_raw_parts_mut(ptr, elem_size) }
    }
}

impl<'a, T> DmaGuard<T, &'a mut T> {
    /// Constructs self from a raw pointer.
    /// This can be used to allow stack allocated buffers or buffers that are otherwise unsafe to use.
    ///
    /// # Safety
    ///
    /// The caller must ensure that DMA operations are completed before accessing the owner of `data`.
    pub unsafe fn from_raw(data: &'a mut T) -> DmaGuard<T, &'a mut T> {
        Self {
            inner: data,
            _phantom: Default::default(),
        }
    }
}

unsafe impl<T> DmaTarget for DmaGuard<T, &mut T> {

    fn as_mut(&mut self) -> *mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.inner as *mut _ as *mut u8, size_of_val(&*self.inner)) }
    }
}

impl<T, C: DmaPointer<T>> From<C> for DmaGuard<T, C> {
    fn from(inner: C) -> Self {
        DmaGuard { inner, _phantom: PhantomData }
    }
}


mod sealed {
    pub trait Sealed {}
}

trait DmaPointer<T>: sealed::Sealed {}


impl<T,A:Allocator> sealed::Sealed for Vec<T,A> {}
impl<T,A:Allocator> DmaPointer<T> for Vec<T,A> {}

impl<T,A:Allocator> sealed::Sealed for Box<T,A> {}
impl<T,A:Allocator> DmaPointer<T> for Box<T,A> {}

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
        let data = unsafe { & *self.data };

        let mut diff = (base as usize & (super::PAGE_SIZE-1)).min(data.len()); // diff between next index and base

        loop {
            if self.next_chunk(diff + self.next)? - diff as u64 == diff as u64 {
                diff += super::PAGE_SIZE;
                diff = diff.min(data.len()); // make sure we dont overflow
            } else {
                break
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
#[derive(Debug)]
pub struct PhysicalRegionDescription {
    /// Starting physical address of the region.
    pub addr: u64,
    /// Length in bytes.
    pub size: usize,
}

/// A type that implements DmaTarget can be used for DMA operations.
///
/// async DMA operations *must* use an implementor of DmaTarget to safely operate. The argument *must* be
/// taken by value and not by reference, the future should return ownership of the DmaTarget when it completes.
/// See [Embedonomicon](https://docs.rust-embedded.org/embedonomicon/dma.html) for details.
///
/// # Safety
///
/// An implementor must ensure that the DMA region returned by [Self::as_mut] is owned by `self` is treated as volatile.
pub unsafe trait DmaTarget {
    fn as_mut(&mut self) -> *mut [u8];

    /// Returns a Physical region describer.
    ///
    /// This takes `self` as `&mut` but does not actually mutate `self` this is to prevent all
    /// accesses to `self` while the PRD is alive.
    fn prd(&mut self) -> PhysicalRegionDescriber {
        PhysicalRegionDescriber {
            data: self.as_mut(),
            next: 0,
            phantom: Default::default(),
        }
    }
}