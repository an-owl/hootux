use super::HeapAlloc;
use crate::mem;
use crate::mem::{buddy_frame_alloc, PAGE_SIZE};
use core::alloc::{AllocError, Layout};
use core::ptr::NonNull;

/// Provides a struct capable of holding allocators together providing full control of memory
pub(crate) struct DualHeap<S, I>
where
    S: SuperiorAllocator,
    I: InferiorAllocator,
{
    superior: S,
    inferior: I,
    phys_alloc: buddy_frame_alloc::BuddyFrameAlloc,
    mapper: Option<super::super::offset_page_table::OffsetPageTable>
}

impl<S: SuperiorAllocator, I: InferiorAllocator> DualHeap<S, I> {
    pub const fn new(superior: S, inferior: I) -> Self {
        Self {
            superior,
            inferior,
            phys_alloc: buddy_frame_alloc::BuddyFrameAlloc::new(),
            mapper: None,
        }
    }

    /// Initializes self
    ///
    /// #Safety
    ///
    /// This fn is unsafe because ptr must point to valid mapped writable memory. Memory above `ptr` should be unused
    // todo replace PAGE_SIZE with I::INIT_BYTES see https://github.com/rust-lang/rust/issues/76560
    pub unsafe fn init(&mut self, ptr: *mut [u8; PAGE_SIZE]) {
        unsafe {
            self.inferior.init(ptr);
            self.superior.init(ptr as usize);
        }
    }

    pub fn mapper_mut(&mut self) -> &mut super::super::offset_page_table::OffsetPageTable {
        self.mapper.as_mut().expect("Mapper not initialized")
    }
    pub fn mapper(&self) -> &super::super::offset_page_table::OffsetPageTable {
        self.mapper.as_ref().expect("Mapper not initialized")
    }

    pub fn cfg_mapper(&mut self, mapper: super::super::offset_page_table::OffsetPageTable) {
        self.mapper = Some(mapper)
    }

    pub(crate) fn phys_alloc(&self) -> &buddy_frame_alloc::BuddyFrameAlloc {
        &self.phys_alloc
    }
}

unsafe impl<S: SuperiorAllocator, I: InferiorAllocator> core::alloc::GlobalAlloc
    for crate::util::mutex::ReentrantMutex<DualHeap<S, I>>
{
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        use x86_64::structures::paging::{
            page::{Page, PageRangeInclusive, Size4KiB},
            PageTableFlags,
        };
        use x86_64::VirtAddr;

        let alloc = self.lock();

        let cmp = layout.size().max(layout.align());

        return if cmp < 2048 {
            // inferior max size
            let t = alloc.inferior.allocate(layout).unwrap().cast().as_ptr(); // panic is expected on fail
            t
        } else {
            let ret = alloc
                .superior
                .virt_allocate(layout)
                .unwrap()
                .cast()
                .as_ptr();

            let pages = {
                let start = VirtAddr::from_ptr(ret);
                PageRangeInclusive::<Size4KiB> {
                    start: Page::containing_address(start),
                    end: Page::containing_address(start + S::allocated_size(layout) - 1usize),
                }
            };

            let flags = PageTableFlags::WRITABLE | PageTableFlags::PRESENT;

            mem::mem_map::map_range(pages, flags);

            ret
        };
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        use x86_64::structures::paging::page::{Page, PageRangeInclusive, Size4KiB};
        use x86_64::VirtAddr;

        let cmp = layout.size().max(layout.align());
        if cmp < 2048 {
            self.lock()
                .inferior
                .deallocate(NonNull::new(ptr).unwrap(), layout) // shouldn't panic
        } else {
            let pages = {
                let start = VirtAddr::from_ptr(ptr);
                PageRangeInclusive::<Size4KiB> {
                    start: Page::containing_address(start),
                    end: Page::containing_address(start + S::allocated_size(layout) - 1usize),
                }
            };

            for i in pages.map(|p| p.start_address()) {
                super::super::mem_map::unmap_and_free(i).expect("Failed to free memory");
            }

            // TODO: deallocate frames

            self.lock()
                .superior
                .virt_deallocate(NonNull::new(ptr).unwrap(), layout);
        }
    }
}

unsafe impl<S: SuperiorAllocator, I: InferiorAllocator> HeapAlloc for DualHeap<S, I> {
    fn virt_allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        self.superior.virt_allocate(layout)
    }

    fn virt_deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        self.superior.virt_deallocate(ptr, layout)
    }
}

#[doc(hidden)]
struct Hidden;

/// Provides an unsafe interface to allow an inferior allocator to call the superior allocator.
///
/// #Saftey
///
/// This struct unsafe to use because its trait implementations are subject to race conditions.
/// This struct should only be used within HeapController, It is intended to bypass its mutex to
/// allow each field to call the other while locked.
// todo: using ReentrantMutex may have made this redundant, investigate removing
pub(crate) struct InteriorAlloc {
    _inner: Hidden,
}

impl InteriorAlloc {
    pub const unsafe fn new() -> Self {
        Self { _inner: Hidden }
    }
}

unsafe impl HeapAlloc for InteriorAlloc {
    fn virt_allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        super::COMBINED_ALLOCATOR
            .lock()
            .superior
            .virt_allocate(layout)
    }

    fn virt_deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        super::COMBINED_ALLOCATOR
            .lock()
            .superior
            .virt_deallocate(ptr, layout)
    }
}

unsafe impl core::alloc::Allocator for InteriorAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        super::COMBINED_ALLOCATOR.lock().inferior.allocate(layout)
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        super::COMBINED_ALLOCATOR
            .lock()
            .inferior
            .allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe {
            super::COMBINED_ALLOCATOR
                .lock()
                .inferior
                .deallocate(ptr, layout)
        }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe {
            super::COMBINED_ALLOCATOR
                .lock()
                .inferior
                .grow(ptr, old_layout, new_layout)
        }
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe {
            super::COMBINED_ALLOCATOR
                .lock()
                .inferior
                .grow(ptr, old_layout, new_layout)
        }
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe {
            super::COMBINED_ALLOCATOR
                .lock()
                .inferior
                .shrink(ptr, old_layout, new_layout)
        }
    }
}

impl SuperiorAllocator for InteriorAlloc {
    unsafe fn init(&mut self, _addr: usize) {
        panic!("unable to call init on InteriorAlloc")
    }

    fn allocated_size(_layout: Layout) -> usize {
        panic!("unable to call init on InteriorAlloc")
    }
}

impl InferiorAllocator for InteriorAlloc {
    const INIT_BYTES: usize = 0;

    unsafe fn init(&self, _ptr: *mut [u8; PAGE_SIZE]) {
        panic!("unable to call init on InteriorAlloc")
    }

    unsafe fn force_dealloc(&self, ptr: NonNull<u8>, layout: Layout) {
        super::COMBINED_ALLOCATOR
            .lock()
            .inferior
            .force_dealloc(ptr, layout)
    }
}

pub(crate) trait SuperiorAllocator: HeapAlloc {
    /// Initialize self at `*addr`.
    unsafe fn init(&mut self, addr: usize);

    fn allocated_size(layout: Layout) -> usize;
}

pub trait InferiorAllocator: core::alloc::Allocator {
    const INIT_BYTES: usize;

    /// Initializes allocator using memory at `ptr`
    ///
    /// #Safety
    ///
    /// This fn is unsafe because ptr must be ptr must be valid writable memory.
    unsafe fn init(&self, ptr: *mut [u8; PAGE_SIZE]);

    /// Forcibly deallocate memory. This is intended to add regions of memory into `self`.
    /// deallocate may defer to another allocator force_dealloc may not for any reason.
    /// Implementations of this fn should find any way to store the given memory although it is not
    /// always possible, calls to this should consider these limitations
    ///
    /// #Panics
    ///
    /// This fn may panic if implementations are unable to store all memory given to them.
    ///
    /// #Saftey
    ///
    /// This fn is unsafe because it wraps [core::alloc::Allocator]
    unsafe fn force_dealloc(&self, ptr: NonNull<u8>, layout: Layout);
}
