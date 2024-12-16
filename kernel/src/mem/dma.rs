use alloc::boxed::Box;
use alloc::vec::Vec;
use core::alloc::Allocator;
use core::marker::PhantomData;

pub struct DmaGuard<T,C: DmaTarget> {
    inner: C,

    _phantom: PhantomData<T>,
}

impl<T,C: DmaTarget> DmaGuard<T,C> {
    pub fn unwrap(self) -> C {
        self.inner
    }
}

impl<T, A: Allocator> DmaGuard<T,Vec<T, A>> {
    fn get_raw(&mut self) -> *mut [T] {
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

impl<T> DmaGuard<T,Box<T>> {
    pub fn get_raw(&mut self) -> *mut [u8] {
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

impl<T, C: DmaTarget> From<C> for DmaGuard<T, C> {
    fn from(inner: C) -> Self {
        DmaGuard { inner, _phantom: PhantomData }
    }
}


mod sealed {
    pub trait Sealed {}
}

trait DmaTarget: sealed::Sealed {}


impl<T,A:Allocator> sealed::Sealed for Vec<T,A> {}
impl<T,A:Allocator> DmaTarget for Vec<T,A> {}

impl<T,A:Allocator> sealed::Sealed for Box<T,A> {}
impl<T,A:Allocator> DmaTarget for Box<T,A> {}

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

struct PhysicalRegionDescription {
    pub addr: u64,
    pub size: usize,
}