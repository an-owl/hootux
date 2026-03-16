use super::HeapAlloc;
use crate::mem::PAGE_SIZE;
use core::alloc::{AllocError, Layout};
use core::ptr::NonNull;

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
        super::MEMORY_ALLOCATORS.lock().0.virt_allocate(layout)
    }

    fn virt_deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        super::MEMORY_ALLOCATORS
            .lock()
            .0
            .virt_deallocate(ptr, layout)
    }
}

unsafe impl core::alloc::Allocator for InteriorAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        super::MEMORY_ALLOCATORS.lock().1.allocate(layout)
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        super::MEMORY_ALLOCATORS.lock().1.allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe { super::MEMORY_ALLOCATORS.lock().1.deallocate(ptr, layout) }
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        unsafe {
            super::MEMORY_ALLOCATORS
                .lock()
                .1
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
            super::MEMORY_ALLOCATORS
                .lock()
                .1
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
            super::MEMORY_ALLOCATORS
                .lock()
                .1
                .shrink(ptr, old_layout, new_layout)
        }
    }
}

impl SuperiorAllocator for InteriorAlloc {
    unsafe fn init(&mut self, _addr: usize) {
        panic!("unable to call init on InteriorAlloc")
    }
}

impl InferiorAllocator for InteriorAlloc {
    const INIT_BYTES: usize = 0;

    unsafe fn init(&self, _ptr: *mut [u8; PAGE_SIZE]) {
        panic!("unable to call init on InteriorAlloc")
    }

    unsafe fn force_dealloc(&self, ptr: NonNull<u8>, layout: Layout) {
        unsafe { super::MEMORY_ALLOCATORS.lock().1.force_dealloc(ptr, layout) }
    }
}

pub(crate) trait SuperiorAllocator: HeapAlloc {
    /// Initialize self at `*addr`.
    unsafe fn init(&mut self, addr: usize);
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
