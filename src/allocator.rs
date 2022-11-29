use core::alloc::{AllocError, Layout};
use core::ptr::NonNull;
use x86_64::structures::paging::{
    mapper::MapToError, FrameAllocator,Mapper,Page,PageTableFlags,Size4KiB};
use x86_64::VirtAddr;
use crate::allocator::mmio_bump_alloc::MmioAlloc;

pub mod bump;
pub mod linked_list;
pub mod fixed_size_block;
pub mod page_table_allocator;
pub mod mmio_bump_alloc;

pub struct Locked<A>{
    inner: spin::Mutex<A>
}

impl<A> Locked<A>{
    pub const fn new(inner: A) -> Self{
        Self{
            inner: spin::Mutex::new(inner)
        }
    }

    pub fn lock (&self) -> spin::MutexGuard<A> {
        self.inner.lock()
    }
}

// l4: 257 one l4 above half canonical version remove leading f's for non canonical
pub const HEAP_START: usize = 0xffff808000000000;
pub const HEAP_SIZE: usize = 1024 * 1024;

#[global_allocator]
static ALLOCATOR: Locked<fixed_size_block::FixedBlockAllocator> = Locked::new(fixed_size_block::FixedBlockAllocator::new());

lazy_static::lazy_static! {
    static ref MMIO_HEAP: crate::kernel_structures::Mutex<mmio_bump_alloc::MmioBumpHeap> = {
        use mmio_bump_alloc::MmioBumpHeap;
        let mut mm = mmio_bump_alloc::MmioBumpHeap::new();
        mm.init(VirtAddr::new(MmioBumpHeap::HEAP_START as u64),VirtAddr::new((MmioBumpHeap::HEAP_START + MmioBumpHeap::HEAP_SIZE ) as u64));

        crate::kernel_structures::Mutex::new(mm)
    };
}

pub fn init_heap(
    mapper: &mut impl Mapper<Size4KiB>,
    frame_allocator: &mut impl FrameAllocator<Size4KiB>
) -> Result<(),MapToError<Size4KiB>>{
    let page_range = {
        let heap_start = VirtAddr::new(HEAP_START as u64);
        let heap_end = heap_start + HEAP_SIZE - 1u64;
        let heap_start_page = Page::containing_address(heap_start);
        let heap_end_page = Page::containing_address(heap_end);
        Page::range_inclusive(heap_start_page,heap_end_page)
    };

    for page in page_range{
        let frame = frame_allocator
            .allocate_frame().ok_or(MapToError::FrameAllocationFailed)?;
        let flags = PageTableFlags::PRESENT| PageTableFlags::WRITABLE;
        unsafe {
            mapper.map_to(page, frame, flags, frame_allocator)?.flush()
        };
    }
    unsafe { ALLOCATOR.lock().init(HEAP_START, HEAP_SIZE) };

    Ok(())
}

fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}



pub(crate) enum GenericAlloc {
    Global(alloc::alloc::Global),
    Mmio(mmio_bump_alloc::MmioAlloc)
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

    unsafe fn grow(&self, ptr: NonNull<u8>, old_layout: Layout, new_layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            GenericAlloc::Global(a) => a.grow(ptr, old_layout, new_layout),
            GenericAlloc::Mmio(a) => a.grow(ptr, old_layout, new_layout)
        }
    }
    
    unsafe fn grow_zeroed(&self, ptr: NonNull<u8>, old_layout: Layout, new_layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            GenericAlloc::Global(a) => a.grow_zeroed(ptr, old_layout, new_layout),
            GenericAlloc::Mmio(a) => a.grow_zeroed(ptr, old_layout, new_layout),
        }
    }

    unsafe fn shrink(&self, ptr: NonNull<u8>, old_layout: Layout, new_layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        match self {
            GenericAlloc::Global(a) => a.shrink(ptr, old_layout, new_layout),
            GenericAlloc::Mmio(a) => a.shrink(ptr, old_layout, new_layout)
        }
    }
}

impl From<MmioAlloc> for GenericAlloc {
    fn from(f: MmioAlloc) -> Self {
        Self::Mmio(f)
    }
}