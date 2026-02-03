//! This module contains public interfaces for memory allocation

use crate::mem::allocator::MEMORY_ALLOCATORS;
use crate::mem::mem_map::{PROGRAM_DATA_FLAGS, map_frame_to_page, map_range};
use crate::{mem, mem::allocator::HeapAlloc};
use core::ptr::null_mut;
use core::{
    alloc::{AllocError, Allocator, Layout},
    ptr::NonNull,
};
use hootux::mem::frame_attribute_table::FatOperation;
use x86_64::{
    PhysAddr, VirtAddr,
    structures::paging::{Page, PageTableFlags, Size4KiB, page::PageRangeInclusive},
};

#[global_allocator]
static KERNEL_ALLOCATOR: KernelAllocator = KernelAllocator;

struct KernelAllocator;

impl KernelAllocator {
    const ALLOC_SIZE_SEP: usize = super::super::PAGE_SIZE / 2;
}

unsafe impl core::alloc::GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size().max(layout.align());
        let l = MEMORY_ALLOCATORS.lock();
        match size {
            ..Self::ALLOC_SIZE_SEP => {
                l.1.allocate(layout)
                    .map(|e| e.as_ptr().cast())
                    .unwrap_or(null_mut())
            }
            Self::ALLOC_SIZE_SEP.. => {
                let Ok(virt_addr) = l.0.virt_allocate(layout) else {
                    return null_mut();
                };
                drop(l);

                map_range(
                    PageRangeInclusive {
                        start: Page::<Size4KiB>::containing_address(VirtAddr::from_ptr(
                            virt_addr.as_ptr(),
                        )),
                        end: Page::<Size4KiB>::containing_address(VirtAddr::new(x86_64::align_up(
                            size as u64,
                            super::super::PAGE_SIZE as u64,
                        ))),
                    },
                    PROGRAM_DATA_FLAGS,
                );
                virt_addr.as_ptr().cast()
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let size = layout.size().max(layout.align());
        match size {
            0..Self::ALLOC_SIZE_SEP => {
                // SAFETY: Safety is upheld by caller.
                unsafe {
                    MEMORY_ALLOCATORS.lock().1.deallocate(
                        NonNull::new(ptr).expect("Attempted to free nullptr"),
                        layout,
                    )
                }
            }

            Self::ALLOC_SIZE_SEP.. => {
                let start = Page::<Size4KiB>::containing_address(VirtAddr::from_ptr(ptr));

                let end = Page::<Size4KiB>::containing_address(VirtAddr::new(
                    (ptr as usize + size) as u64,
                ));
                let iter = PageRangeInclusive { start, end };
                // SAFETY: Caller upholds safety
                let mut iter = unsafe { mem::mem_map::unmap_range(iter) };
                while let Some(entry) = iter.next_page() {
                    let addr = entry.addr();

                    if entry
                        .flags()
                        .contains(mem::frame_attribute_table::FRAME_ATTR_ENTRY_FLAG)
                    {
                        try_free_frame(addr)
                    } else {
                        // SAFETY: Upheld by caller.
                        unsafe {
                            super::PHYS_ALLOCATOR
                                .lock()
                                .dealloc(addr.as_u64() as usize, mem::PAGE_SIZE)
                        }
                    }
                    entry.set_addr(PhysAddr::zero(), PageTableFlags::empty())
                }
            }
        }
    }
}

/// Attempts to free `frame`. If frame is aliased then it will not be freed.
fn try_free_frame(addr: PhysAddr) {
    let will_free = {
        let fae = mem::frame_attribute_table::ATTRIBUTE_TABLE_HEAD
            .do_op_phys(addr, FatOperation::UnAlias);
        if let Some(fae) = fae {
            fae.alias_count() == 0
        } else {
            true
        }
    };

    if will_free {
        // SAFETY: We have checked that the frame is not in use by
        unsafe {
            super::PHYS_ALLOCATOR
                .lock()
                .dealloc(addr.as_u64() as usize, mem::PAGE_SIZE)
        }
    }
}

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
        unsafe { Self::new(phys_addr.as_u64() as usize) }
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
            end: Page::containing_address(addr + layout.size() as u64 - 1u64),
        };
        pages
    }

    /// Consumes self and returns a Box containing a `T`
    pub unsafe fn boxed_alloc<T>(self) -> Result<alloc::boxed::Box<T, Self>, AllocError> {
        unsafe {
            let ptr = self.allocate(Layout::new::<T>())?.cast::<T>();
            let b = alloc::boxed::Box::from_raw_in(ptr.as_ptr(), self);
            Ok(b)
        }
    }
}

unsafe impl Allocator for MmioAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::NO_CACHE; // huge page

        let ptr = MEMORY_ALLOCATORS.lock().0.virt_allocate(layout)?;

        let pages = self.get_page_range(&layout, &ptr);
        let phys_frame = x86_64::structures::paging::PhysFrame::<Size4KiB>::containing_address(
            PhysAddr::new(self.addr as u64),
        );

        for (i, page) in pages.enumerate() {
            map_frame_to_page(page.start_address(), phys_frame + i as u64, flags)
                .expect("Failed to map frame");
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
        let mut addr = ptr.as_ptr() as usize;
        addr &= !(mem::PAGE_SIZE - 1);

        // SAFETY: Upheld by caller.
        unsafe { mem::mem_map::unmap_range(pages) }; // Note that the returned iter is dropped immediately.

        let ptr = NonNull::new(addr as *mut u8).expect("Tried to deallocate illegal address");

        // SAFETY: Upheld by caller.
        MEMORY_ALLOCATORS.lock().0.virt_deallocate(ptr, layout);
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe {
            // This fn does not require copying memory
            self.deallocate(ptr, old_layout);
            self.allocate(new_layout)
        }
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
        unsafe {
            // This fn does not require copying memory
            self.deallocate(ptr, old_layout);
            self.allocate(new_layout)
        }
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
#[derive(Copy, Clone)]
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

        let addr = MEMORY_ALLOCATORS.lock().0.virt_allocate(layout)?;
        {
            let start = addr.cast::<u8>().as_ptr() as usize as u64;
            let end = unsafe { addr.as_ref().len() as u64 + start } - 1; // sub one to get last byte

            let range = PageRangeInclusive {
                start: Page::<Size4KiB>::containing_address(VirtAddr::new(start)),
                end: Page::<Size4KiB>::containing_address(VirtAddr::new(end)),
            };

            // mem::mem_map cannot be used because DMA areas must be contiguous
            let phys_addr = mem::allocator::PHYS_ALLOCATOR
                .lock()
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

            for (p, f) in core::iter::zip(range, p_range) {
                map_frame_to_page(p.start_address(), f, mem::mem_map::MMIO_FLAGS).unwrap();
            }
        }
        Ok(addr)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe {
            let start = ptr.as_ptr() as usize;
            let end = start + layout.size() - 1;

            let range = PageRangeInclusive {
                start: Page::<Size4KiB>::containing_address(VirtAddr::new(start as u64)),
                end: Page::containing_address(VirtAddr::new(end as u64)),
            };

            let mut iter = mem::mem_map::unmap_range(range);

            let mut free_separate = false;
            while let Some(entry) = iter.next_page() {
                if entry
                    .flags()
                    .contains(mem::frame_attribute_table::FRAME_ATTR_ENTRY_FLAG)
                {
                    free_separate = true;
                    break;
                }
            }
            iter.reload();
            if free_separate {
                while let Some(entry) = iter.next_page() {
                    try_free_frame(entry.addr())
                }
            } else {
                let Some(entry) = iter.next_page() else {
                    return;
                };
                let start_address = entry.addr();
                super::PHYS_ALLOCATOR.lock().dealloc(
                    start_address.as_u64() as usize,
                    layout.size().max(layout.align()),
                );
            }

            MEMORY_ALLOCATORS.lock().0.virt_deallocate(ptr, layout);
        }
    }
}

/// Allocates virtual memory without allocating physical memory to it.
///
/// This allocator will always return page aligned addresses. When [Allocator::deallocate] is called
/// the given pointer will be aligned down to [mem::PAGE_SIZE], and all pages within the described
/// region will be [mem::mem_map::unmap_and_free]'d.
///
/// This implements [Allocator] however only [Allocator::allocate], [Allocator::allocate_zeroed]
/// and [Allocator::by_ref] may be called, all other methods will panic.
/// This is behaviour is because memory allocated by this allocator is inaccessible until it's
/// explicitly mapped to physical memory, these methods are likely to cause page faults, which are
/// harder to debug than a panic.
pub struct VirtAlloc;

impl VirtAlloc {
    pub fn alloc_boxed<T>() -> Result<alloc::boxed::Box<core::mem::MaybeUninit<T>, Self>, AllocError>
    {
        let ptr = Self.allocate(Layout::new::<T>())?;
        Ok(unsafe { alloc::boxed::Box::from_raw_in(ptr.as_ptr().cast(), Self) })
    }
}

unsafe impl Allocator for VirtAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        MEMORY_ALLOCATORS.lock().0.virt_allocate(layout)
    }

    fn allocate_zeroed(&self, _layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        panic!("Not allowed")
    }

    unsafe fn deallocate(&self, mut ptr: NonNull<u8>, layout: Layout) {
        unsafe {
            // pointer may not be aligned so we align it down
            let off = ptr.as_ptr() as usize & (mem::PAGE_SIZE - 1);
            ptr = ptr.byte_sub(off);

            MEMORY_ALLOCATORS.lock().0.virt_deallocate(ptr, layout);
            for i in ptr.as_ptr() as usize..ptr.as_ptr() as usize + layout.size() {
                let addr = VirtAddr::from_ptr(i as *const u8);
                let _ = mem::mem_map::unmap_and_free(addr); // this is likely to fail, we just want to ensure that the memory isn't mapped
            }
        }
    }

    unsafe fn grow(
        &self,
        _ptr: NonNull<u8>,
        _old_layout: Layout,
        _new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        panic!("Not allowed")
    }

    unsafe fn grow_zeroed(
        &self,
        _ptr: NonNull<u8>,
        _old_layout: Layout,
        _new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        panic!("Not allowed")
    }

    unsafe fn shrink(
        &self,
        _ptr: NonNull<u8>,
        _old_layout: Layout,
        _new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        panic!("Not allowed")
    }
}
