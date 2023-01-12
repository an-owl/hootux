use crate::mem;
use core::alloc::{AllocError, Layout};
use core::ptr::NonNull;
use mem::mem_map::*;
use x86_64::structures::paging::{
    mapper::MapToError, FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB,
};
use x86_64::VirtAddr;

pub mod bump;
pub mod fixed_size_block;
pub mod linked_list;
pub mod page_table_allocator;
//pub mod mmio_bump_alloc;
pub mod alloc_interface;
pub(self) mod allocator_linked_list;
mod buddy_alloc;
pub(self) mod combined_allocator;

pub struct Locked<A> {
    inner: spin::Mutex<A>,
}

impl<A> Locked<A> {
    pub const fn new(inner: A) -> Self {
        Self {
            inner: spin::Mutex::new(inner),
        }
    }

    pub fn lock(&self) -> spin::MutexGuard<A> {
        self.inner.lock()
    }
}

// l4: 257 one l4 above half canonical version remove leading f's for non canonical
pub const HEAP_START: usize = 0xffff808000000000;
pub const HEAP_SIZE: usize = 1024 * 1024;

static ALLOCATOR: Locked<fixed_size_block::FixedBlockAllocator> =
    Locked::new(fixed_size_block::FixedBlockAllocator::new());

#[global_allocator]
static COMBINED_ALLOCATOR: crate::kernel_structures::mutex::ReentrantMutex<
    combined_allocator::DualHeap<
        buddy_alloc::BuddyHeap,
        fixed_size_block::NewFixedBlockAllocator
    >
> = crate::kernel_structures::mutex::ReentrantMutex::new(combined_allocator::DualHeap::new(
    buddy_alloc::BuddyHeap::new(),
    fixed_size_block::NewFixedBlockAllocator::new(),
));

pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>,
) -> Result<(), MapToError<Size4KiB>> {
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + HEAP_SIZE - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page, heap_end_page)
    };

    for page in page_range {
        let frame = frame_allocator
            .allocate_frame()
            .ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE;
        unsafe { mapper.map_to(page, frame, flags, frame_allocator)?.flush() };
    }
    unsafe { ALLOCATOR.lock().init(HEAP_START, HEAP_SIZE) };

    Ok(())
}

/// Maps memory to addr and uses it to initialize the allocator
pub unsafe fn init_comb_heap(addr: usize) {
    assert_eq!(addr & (buddy_alloc::ORDER_MAX_SIZE - 1), 0);

    let ptr = addr as *mut u8;
    let mut lock = COMBINED_ALLOCATOR.lock();

    // map mem
    map_page(
        Page::from_start_address(VirtAddr::from_ptr(ptr)).unwrap(),
        PROGRAM_DATA_FLAGS,
    ); // unwrap shouldn't panic
    let ptr = addr as *mut [u8; mem::PAGE_SIZE];
    lock.init(ptr);
}

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

pub(crate) enum GenericAlloc {
    Global(alloc::alloc::Global),
    Mmio(alloc_interface::MmioAlloc),
}

unsafe impl core::alloc::Allocator for GenericAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            GenericAlloc::Global(a) => a.allocate(layout),
            GenericAlloc::Mmio(a) => a.allocate(layout),
        }
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            GenericAlloc::Global(a) => a.allocate_zeroed(layout),
            GenericAlloc::Mmio(a) => a.allocate_zeroed(layout),
        }
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        match self {
            GenericAlloc::Global(a) => a.deallocate(ptr, layout),
            GenericAlloc::Mmio(a) => a.deallocate(ptr, layout),
        }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            GenericAlloc::Global(a) => a.grow(ptr, old_layout, new_layout),
            GenericAlloc::Mmio(a) => a.grow(ptr, old_layout, new_layout),
        }
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            GenericAlloc::Global(a) => a.grow_zeroed(ptr, old_layout, new_layout),
            GenericAlloc::Mmio(a) => a.grow_zeroed(ptr, old_layout, new_layout),
        }
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            GenericAlloc::Global(a) => a.shrink(ptr, old_layout, new_layout),
            GenericAlloc::Mmio(a) => a.shrink(ptr, old_layout, new_layout),
        }
    }
}

impl From<alloc_interface::MmioAlloc> for GenericAlloc {
    fn from(f: alloc_interface::MmioAlloc) -> Self {
        Self::Mmio(f)
    }
}

/// Provides a simple interface for allocating and deallocating virtual memory in a heap. Intended
/// to work the same as [core::alloc::Allocator], however without allocating physical memory.
unsafe trait HeapAlloc {
    /// Returns a pointer to the allocated region. returns [core::alloc:AllocError] on failure
    fn virt_allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError>;

    fn virt_deallocate(&self, ptr: NonNull<u8>, layout: Layout);
}

mod memory_counter {

    /// Struct for recording memory usage
    #[derive(Debug)]
    pub(super) struct MemoryCounter {
        free: usize,
        used: usize,
    }

    impl MemoryCounter {
        /// Creates a new `MemoryCounter` with `size` free bytes
        pub const fn new(size: usize) -> Self {
            Self {
                free: 0,
                used: size,
            }
        }

        /// Adds `count` to the free count and subtracts it from used count returning the new free count.
        /// Returns `Err()` if `count` is greater than `self.used`
        ///
        /// #Panics
        ///
        /// This fn will panic if `self.free + count` overflows
        pub fn free(&mut self, count: usize) -> Result<usize, usize> {
            if self.used >= count {
                self.free += count;
                self.used -= count;
                Ok(self.free)
            } else {
                Err(self.free)
            }
        }

        /// Acts the same as [Self::free] but adding memory to used and removing it from free.
        pub fn reserve(&mut self, count: usize) -> Result<usize, usize> {
            if self.free >= count {
                self.used += count;
                self.free -= count;
                Ok(self.used)
            } else {
                Err(self.used)
            }
        }

        /// Adds `count` to `self.free` ignoring `self.used`
        pub fn extend(&mut self, count: usize) {
            self.free += count
        }

        pub fn get_used(&self) -> usize {
            self.used
        }
        pub fn get_free(&self) -> usize {
            self.free
        }
    }
}
