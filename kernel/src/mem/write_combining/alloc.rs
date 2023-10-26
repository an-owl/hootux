use super::set_wc_data;
use core::alloc::{AllocError, Allocator, Layout};
use core::ptr::NonNull;

/// This is a write combining wrapper for [super::super::allocator::alloc_interface::MmioAlloc].
/// Allocations using WcMmioAlloc all use the write combining cache mode.
pub struct WcMmioAlloc {
    inner: super::super::allocator::alloc_interface::MmioAlloc,
}

impl WcMmioAlloc {
    pub unsafe fn new(addr: u64) -> Self {
        Self {
            inner: crate::alloc_interface::MmioAlloc::new(addr as usize),
        }
    }
}

unsafe impl Allocator for WcMmioAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let r = self.inner.allocate(layout)?;
        set_wc_data(&r).expect("Failed to set WC flags");
        Ok(r)
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // panics
        self.inner.allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.inner.deallocate(ptr, layout);
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let n = self.inner.grow(ptr, old_layout, new_layout)?;
        set_wc_data(&n).expect("Failed to set WC flags");
        Ok(n)
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // will panic
        self.inner.grow_zeroed(ptr, old_layout, new_layout)
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let r = self.inner.shrink(ptr, old_layout, new_layout)?;
        set_wc_data(&r).expect("Failed to set WC flags");
        Ok(r)
    }
}

pub struct WcDmaAlloc {
    inner: super::super::allocator::alloc_interface::DmaAlloc,
}

/// Write combining wrapper for [super::super::allocator::alloc_interface::DmaAlloc].
/// Allocations using WcDmaAlloc will use the write combining
impl WcDmaAlloc {
    /// Initialises self using the given memory region and alignment. `phys_align` must be a power of two
    pub fn new(region: super::super::MemRegion, phys_align: usize) -> Self {
        Self {
            inner: super::super::allocator::alloc_interface::DmaAlloc::new(region, phys_align),
        }
    }
}

unsafe impl Allocator for WcDmaAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let r = self.inner.allocate(layout)?;
        set_wc_data(&r).expect("Failed to set WC flags");
        Ok(r)
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let r = self.inner.allocate_zeroed(layout)?;
        set_wc_data(&r).expect("Failed to set WC flags");
        Ok(r)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.inner.deallocate(ptr, layout)
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let r = self.inner.grow(ptr, old_layout, new_layout)?;
        set_wc_data(&r).expect("Failed to set WC flags");
        Ok(r)
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let r = self.inner.grow_zeroed(ptr, old_layout, new_layout)?;
        set_wc_data(&r).expect("Failed to set WC flags");
        Ok(r)
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        let r = self.inner.shrink(ptr, old_layout, new_layout)?;
        set_wc_data(&r).expect("Failed to set WC flags");
        Ok(r)
    }
}
