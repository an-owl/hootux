//! This module contains public interfaces for memory allocation

use core::{
    alloc::{AllocError, Allocator, Layout},
    ptr::NonNull,
};

use crate::{mem, mem::allocator::HeapAlloc};
use x86_64::{
    structures::paging::{page::PageRangeInclusive, Mapper, Page, PageTableFlags, Size4KiB},
    PhysAddr, VirtAddr,
};

/// Used to Allocate Physical memory regions. All allocations via this type are guaranteed to be the
/// size of the allocation aligned up to [mem::PAGE_SIZE]
///
/// # Safety
///
/// Using this type is unsafe because it allows access to arbitrary regions of physical memory,
/// which allows aliasing and mutation of read only memory.
///
/// MmioAlloc should **only** be used to access physical memory doing otherwise may lead to UB
///
/// The constructors for this struct are unsafe because [core::alloc::Allocator]'s methods cannot be marked unsafe
#[derive(Copy, Clone)]
pub struct MmioAlloc {
    addr: usize,
}

impl MmioAlloc {
    pub unsafe fn new(phys_addr: usize) -> Self {
        Self { addr: phys_addr }
    }

    pub unsafe fn new_from_phys_addr(phys_addr: PhysAddr) -> Self {
        Self::new(phys_addr.as_u64() as usize)
    }

    pub fn as_phys_addr(&self) -> PhysAddr {
        PhysAddr::new(self.addr as u64)
    }

    /// Returns a pointer to the physical address requested within the given page
    fn offset_addr(&self, ptr: NonNull<[u8]>, len: usize) -> NonNull<[u8]> {
        let offset = self.addr & (mem::PAGE_SIZE - 1); //  returns bytes 11-0

        let req_addr = ptr.cast::<u8>().as_ptr() as usize + offset;

        let new_ptr = unsafe { core::slice::from_raw_parts_mut(req_addr as *mut u8, len) };
        NonNull::new(new_ptr).unwrap() // shouldn't panic
    }

    fn get_page_range<T: ?Sized>(&self, layout: &Layout, ptr: &NonNull<T>) -> PageRangeInclusive {
        let addr = VirtAddr::new(ptr.cast::<u8>().as_ptr() as usize as u64);
        let pages = PageRangeInclusive::<Size4KiB> {
            start: Page::containing_address(addr),
            end: Page::containing_address(addr + layout.size() - 1u64),
        };
        pages
    }

    /// Consumes self and returns a Box containing a `T`
    pub unsafe fn boxed_alloc<T>(self) -> Result<alloc::boxed::Box<T, Self>, AllocError> {
        let ptr = self.allocate(Layout::new::<T>())?.cast::<T>();
        let b = alloc::boxed::Box::from_raw_in(ptr.as_ptr(), self);
        Ok(b)
    }
}

unsafe impl Allocator for MmioAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE; // huge page

        let ptr = super::COMBINED_ALLOCATOR.lock().virt_allocate(layout)?;

        let pages = self.get_page_range(&layout, &ptr);
        let mut phys_frame = x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(
            PhysAddr::new(self.addr as u64),
        );

        for page in pages {
            unsafe {
                mem::SYS_MAPPER
                    .get()
                    .map_to(page, phys_frame, flags, &mut mem::DummyFrameAlloc)
                    .unwrap() // idk debug
                    .ignore(); // not mapped so not cached
            }
            phys_frame = phys_frame + 1;
        }

        Ok(self.offset_addr(ptr, layout.size()))
    }

    /// See [Self::grow_zeroed]
    fn allocate_zeroed(&self, _layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        panic!(
            "Called allocate_zeroed() on {}: Not allowed",
            core::any::type_name::<Self>()
        )
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let pages = self.get_page_range(&layout, &ptr);

        // align down to page boundary
        let mut addr = ptr.as_ptr() as *mut u8 as usize;
        addr &= !(mem::PAGE_SIZE - 1);

        let ptr = NonNull::new(addr as *mut u8).expect("Tried to deallocate illegal address");

        for page in pages {
            mem::SYS_MAPPER
                .get()
                .unmap(page)
                .expect("Tried to deallocate unhandled memory")
                .1
                .flush();
        }

        super::COMBINED_ALLOCATOR
            .lock()
            .virt_deallocate(ptr, layout);
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // This fn does not require copying memory
        self.deallocate(ptr, old_layout);
        self.allocate(new_layout)
    }

    /// This fn will panic because the behaviour expected of this fn is likely to overwrite used memory
    ///
    /// use [Self::grow] instead and manually write zeros
    unsafe fn grow_zeroed(
        &self,
        _ptr: NonNull<u8>,
        _old_layout: Layout,
        _new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        panic!(
            "Called grow_zeroed() on {}: Not allowed",
            core::any::type_name::<Self>()
        )
    }
    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        // This fn does not require copying memory
        self.deallocate(ptr, old_layout);
        self.allocate(new_layout)
    }
}

/// Allocator interface for DMA regions. Allocates contiguous physical memory for a linear region.
/// All allocations will be a minimum size and alignment of 4096.
///
/// This calls both the physical memory allocator and the linear memory allocator. Both will use the
/// size given in `layout` however they may have different alignments. The alignment of the linear
/// allocation is given in `layout` the alignment of the physical allocation is given in the constructor.
///
/// To access regions which are already allocated use [MmioAlloc]
pub struct DmaAlloc {
    region: mem::MemRegion,
    phys_align: usize,
}

impl DmaAlloc {
    /// Initialises self using the given memory region and alignment. `phys_align` must be a power of two
    pub fn new(region: mem::MemRegion, phys_align: usize) -> Self {
        assert!(phys_align.is_power_of_two());
        assert_ne!(phys_align, 0);
        Self { region, phys_align }
    }
}

unsafe impl Allocator for DmaAlloc {
    fn allocate(&self, mut layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // aligns and pads `layout` frame allocator requires size to be aligned to 4k
        // This can only run if align > 4k because align can only be power of 2
        if layout.align() & (mem::PAGE_SIZE - 1) != 0 {
            layout = layout.align_to(mem::PAGE_SIZE).unwrap();
            layout = layout.pad_to_align();
        }

        let addr = super::COMBINED_ALLOCATOR.lock().virt_allocate(layout)?;
        {
            let start = addr.cast::<u8>().as_ptr() as usize as u64;
            let end = unsafe { addr.as_ref().len() as u64 + start } - 1; // sub one to get last byte

            let range = PageRangeInclusive {
                start: Page::<Size4KiB>::containing_address(VirtAddr::new(start)),
                end: Page::<Size4KiB>::containing_address(VirtAddr::new(end)),
            };

            // mem::mem_map cannot be used because DMA areas must be contiguous
            let phys_addr = mem::allocator::COMBINED_ALLOCATOR.lock().phys_alloc()
                .allocate(
                    Layout::from_size_align(layout.size(), self.phys_align).unwrap(),
                    self.region,
                )
                .ok_or(AllocError)?;
            let start = PhysAddr::new(phys_addr as u64);
            let end = PhysAddr::new((phys_addr as u64 + layout.size() as u64) - 1);

            let p_range = x86_64::structures::paging::frame::PhysFrameRangeInclusive {
                start: x86_64::structures::paging::frame::PhysFrame::<Size4KiB>::containing_address(
                    start,
                ),
                end: x86_64::structures::paging::frame::PhysFrame::<Size4KiB>::containing_address(
                    end,
                ),
            };
            let mut lock = mem::SYS_MAPPER.get();
            for (p, f) in core::iter::zip(range, p_range) {
                unsafe {
                    lock.map_to(p, f, mem::mem_map::MMIO_FLAGS, &mut mem::DummyFrameAlloc)
                        .unwrap()
                        .flush()
                };
            }
        }
        Ok(addr)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let start = ptr.as_ptr() as usize;
        let end = start + layout.size() - 1;

        let range = PageRangeInclusive {
            start: Page::<Size4KiB>::containing_address(VirtAddr::new(start as u64)),
            end: Page::containing_address(VirtAddr::new(end as u64)),
        };

        let phys_addr = {
            let l_addr = ptr.as_ptr() as usize as u64;
            let page = Page::<Size4KiB>::containing_address(VirtAddr::new(l_addr));
            mem::SYS_MAPPER
                .get()
                .translate_page(page)
                .expect("Error getting physical address")
        };
        mem::mem_map::unmap_range(range);

        mem::allocator::COMBINED_ALLOCATOR.lock().phys_alloc()
            .dealloc(phys_addr.start_address().as_u64() as usize, layout.size());
        super::COMBINED_ALLOCATOR
            .lock()
            .virt_deallocate(ptr, layout)
    }
}

/// Allocates virtual memory without allocating physical memory to it.
///
/// This implements [Allocator] however only [Allocator::allocate], [Allocator::allocate_zeroed]
/// and [Allocator::by_ref] may be called, all other methods will panic.
/// This is behaviour is because memory allocated by this allocator is inaccessible until it's
/// explicitly mapped to physical memory, these methods are likely to cause page faults, which are
/// harder to debug than a panic.
pub struct VirtAlloc;

unsafe impl Allocator for VirtAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        super::COMBINED_ALLOCATOR.lock().virt_allocate(layout)
    }

    fn allocate_zeroed(&self, _layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        panic!("Not allowed")
    }

    fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        super::COMBINED_ALLOCATOR.lock().virt_deallocate(ptr, layout)
    }

    unsafe fn grow(&self, _ptr: NonNull<u8>, _old_layout: Layout, _new_layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        panic!("Not allowed")
    }

    unsafe fn grow_zeroed(&self, _ptr: NonNull<u8>, _old_layout: Layout, _new_layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        panic!("Not allowed")
    }

    unsafe fn shrink(&self, _ptr: NonNull<u8>, _old_layout: Layout, _new_layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        panic!("Not allowed")
    }
}