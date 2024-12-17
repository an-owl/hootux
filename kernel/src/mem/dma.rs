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


    /// Splits self into generic metadata and untyped data which can be used for dynamic dispatch.
    ///
    /// The returned types can be combined using [Self::from_untyped] to recombine into self.
    pub fn into_untyped(self) -> (UntypedDmaGuard,TypedDmaGuardMetadata<T,C>) {
        (
            UntypedDmaGuard {
                data: self.get_raw(),
            },
            TypedDmaGuardMetadata {
            raw: core::mem::MaybeUninit::new(self.inner),
            _phantom: Default::default(),
            }
        )
    }

    /// Combines data and metadata into self, allowing the data to be unwrapped.
    pub unsafe fn from_untyped(_ut: UntypedDmaGuard, meta: TypedDmaGuardMetadata<T,C>) -> Self {
        Self {
            inner: meta.raw.assume_init(),
            _phantom: Default::default(),
        }
    }
}

unsafe impl<T, A: Allocator> DmaTarget for DmaGuard<T,Vec<T, A>> {
    fn as_mut(&mut self) -> *mut [T] {
        let ptr = self.inner.as_mut_ptr();
        let elem_size = size_of::<T>();
        unsafe { core::slice::from_raw_parts_mut(ptr, elem_size * self.inner.len()) }
    }

    fn prd(&mut self) -> PhysicalRegionDescriber {
        let t = &mut *self.inner;
        let t = unsafe { core::slice::from_raw_parts_mut(t as *mut [T] as *mut u8, size_of_val(t)) as *mut [u8]};

        PhysicalRegionDescriber {
            data: t,
            next: 0,
            phantom: Default::default(),
        }
    }
}

unsafe impl<T> DmaTarget for DmaGuard<T,Box<T>> {
    fn as_mut(&mut self) -> *mut [u8] {
        let ptr = self.inner.as_mut() as *mut T as *mut u8;
        let elem_size = size_of::<T>();
        unsafe { core::slice::from_raw_parts_mut(ptr, elem_size) }
    }

    fn prd(&mut self) -> PhysicalRegionDescriber {
        PhysicalRegionDescriber {
            data: self.get_raw(),
            next: 0,
            phantom: Default::default(),
        }
    }
}

impl<T> DmaGuard<T, &mut T> {
    /// Constructs self from a raw pointer.
    /// This can be used to allow stack allocated buffers or buffers that are otherwise unsafe to use.
    ///
    /// # Safety
    ///
    /// The caller must ensure that DMA operations are completed before accessing the owner of `data`.
    unsafe fn from_raw(data: &mut T) -> DmaGuard<T, &mut T> {
        Self {
            inner: data,
            _phantom: Default::default(),
        }
    }
}

unsafe impl<T> DmaTarget for DmaGuard<T, &mut T> {

    fn as_mut(&self) -> *mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.inner as *mut _, size_of_val(&*self.inner)) }
    }

    fn prd(&mut self) -> PhysicalRegionDescriber {
        PhysicalRegionDescriber {
            data: self.get_raw(),
            next: 0,
            phantom: Default::default(),
        }
    }
}

impl<T, C: DmaPointer> From<C> for DmaGuard<T, C> {
    fn from(inner: C) -> Self {
        DmaGuard { inner, _phantom: PhantomData }
    }
}


mod sealed {
    pub trait Sealed {}
}

trait DmaPointer: sealed::Sealed {}


impl<T,A:Allocator> sealed::Sealed for Vec<T,A> {}
impl<T,A:Allocator> DmaPointer for Vec<T,A> {}

impl<T,A:Allocator> sealed::Sealed for Box<T,A> {}
impl<T,A:Allocator> DmaPointer for Box<T,A> {}

pub struct PhysicalRegionDescriber<'a> {
    data: *mut [u8],
    next: usize,

    phantom: PhantomData<&'a ()>,
}

impl PhysicalRegionDescriber<'_> {
    fn next_chunk(&mut self, index: usize) -> Option<u64> {
        // SAFETY: I think this is unsound
        let data = unsafe { &*self.data };
        crate::mem::mem_map::translate_ptr(&data.get(index)?)
    }
}

impl Iterator for PhysicalRegionDescriber<'_> {
    type Item = PhysicalRegionDescription;

    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: I think this is unsound
        let base = self.next_chunk(self.next)?;
        let data = unsafe { & *self.data };

        let mut diff = (base as usize & (super::PAGE_SIZE-1)).min(data.len()); // diff between next index and base

        loop {
            if self.next_chunk(diff + self.next)? - diff as u64 == diff as u64 {
                diff += super::PAGE_SIZE;
                diff = diff.min(data.len()); // make sure we dont overflow
            } else {
                break
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
pub struct PhysicalRegionDescription {
    /// Starting physical address of the region.
    pub addr: u64,
    /// Length in bytes.
    pub size: usize,
}

struct UntypedDmaGuard {
    data: *mut [u8],
}

struct TypedDmaGuardMetadata<T,C> {
    raw: core::mem::MaybeUninit<C>,
    _phantom: PhantomData<T>,
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

    fn prd(&mut self) -> PhysicalRegionDescriber;
}