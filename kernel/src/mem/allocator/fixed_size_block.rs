use crate::mem;
use core::ptr::{null_mut, NonNull};
use core::{
    alloc::{AllocError, Allocator, GlobalAlloc, Layout},
    cell::UnsafeCell,
};
use x86_64::VirtAddr;

use super::{
    combined_allocator::{InferiorAllocator, InteriorAlloc},
    HeapAlloc, Locked,
};
#[cfg(feature = "alloc-debug-serial")]
use crate::serial_println;

/// The block sizes to use.
///
/// The sizes must each be power of 2 because they are also used as
/// the block alignment (alignments must be always powers of 2).
const BLOCK_SIZES: &[usize] = &[8, 16, 32, 64, 128, 256, 512, 1024, 2048];

struct ListNode {
    next: Option<&'static mut ListNode>,
}

pub struct FixedBlockAllocator {
    list_heads: [Option<&'static mut ListNode>; BLOCK_SIZES.len()],
    fallback_allocator: linked_list_allocator::Heap,
}

impl FixedBlockAllocator {
    pub const fn new() -> Self {
        const EMPTY: Option<&'static mut ListNode> = None;
        FixedBlockAllocator {
            list_heads: [EMPTY; BLOCK_SIZES.len()],
            fallback_allocator: linked_list_allocator::Heap::empty(),
        }
    }

    /// Initialize the allocator with the given heap bounds.
    ///
    /// This function is unsafe because the caller must guarantee that the given
    /// heap bounds are valid and that the heap is unused. This method must be
    /// called only once.
    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        self.fallback_allocator.init(heap_start, heap_size)
    }

    fn fallback_alloc(&mut self, layout: Layout) -> *mut u8 {
        match self.fallback_allocator.allocate_first_fit(layout) {
            Ok(ptr) => ptr.as_ptr(),
            Err(_) => null_mut(),
        }
    }

    fn list_index(layout: &Layout) -> Option<usize> {
        let required_block_size = layout.size().max(layout.align());
        BLOCK_SIZES.iter().position(|&s| s >= required_block_size)
    }
}

unsafe impl GlobalAlloc for Locked<FixedBlockAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut allocator = self.lock();
        match FixedBlockAllocator::list_index(&layout) {
            //check if layout fits within blocks
            None => allocator.fallback_alloc(layout),
            Some(index) => {
                // if alloc fits within blocks
                match allocator.list_heads[index].take() {
                    //get address of next block
                    None => {
                        //if no blocks exist create a new one
                        let block_size = BLOCK_SIZES[index];
                        let block_align = block_size;
                        let layout = Layout::from_size_align(block_size, block_align).unwrap();
                        allocator.fallback_alloc(layout)
                    }

                    Some(node) => {
                        // if exists save next block to head
                        allocator.list_heads[index] = node.next.take();
                        node as *mut ListNode as *mut u8
                    }
                }
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut allocator = self.lock();
        match FixedBlockAllocator::list_index(&layout) {
            Some(index) => {
                let new_node = ListNode {
                    next: allocator.list_heads[index].take(),
                };
                assert!(core::mem::size_of::<ListNode>() <= BLOCK_SIZES[index]);
                assert!(core::mem::align_of::<ListNode>() <= BLOCK_SIZES[index]);
                let new_node_ptr = ptr as *mut ListNode;
                new_node_ptr.write(new_node);
                allocator.list_heads[index] = Some(&mut *new_node_ptr);
            }
            None => {
                let ptr = NonNull::new(ptr).unwrap();
                allocator.fallback_allocator.deallocate(ptr, layout)
            }
        }
    }
}

/// Fixed block style allocator with a complexity of O(1)~.
pub(crate) struct NewFixedBlockAllocator {
    inner: UnsafeCell<FixedBlockInner>,
}

struct FixedBlockInner {
    list_heads: [super::allocator_linked_list::RawLinkedList; BLOCK_SIZES.len()],
    start: *mut u8,
    alloc_count: usize,
}

unsafe impl Send for FixedBlockInner {}

impl NewFixedBlockAllocator {
    /// Notes the minimum size that `Self` may be initialized with. This will always be a multiple
    /// of the largest block size.

    pub const fn new() -> Self {
        Self {
            inner: UnsafeCell::new(FixedBlockInner {
                list_heads: [const { super::allocator_linked_list::RawLinkedList::new() };
                    BLOCK_SIZES.len()],
                start: null_mut(), // this is never actually dereferenced so this is quite safe
                alloc_count: 0,
            }),
        }
    }

    /// Returns the block index capable of holding the given layout. Returns `None` when the given
    /// layout cannot be stored
    fn get_block_index(layout: Layout) -> Option<usize> {
        let find_size = layout.size().max(layout.align());
        BLOCK_SIZES.iter().position(|&s| s >= find_size)
    }

    /// Extends self mapping a page from the superior allocator and adding it to the free list.
    fn extend(&self) {
        use x86_64::structures::paging::page_table::PageTableFlags;

        // SAFETY this is safe because self's parent is interior alloc
        let alloc = unsafe { InteriorAlloc::new() };
        let layout = Layout::from_size_align(mem::PAGE_SIZE, mem::PAGE_SIZE).unwrap(); // will not panic
        let ptr =
            HeapAlloc::virt_allocate(&alloc, layout).expect("System encountered a memory error");

        let region_start = VirtAddr::from_ptr(ptr.cast::<u8>().as_ptr());

        // map region
        // NO_EXECUTE is fine here because this allocator should only be used for data.
        // The superior allocator should be used for allocating executable memory.
        let flags = PageTableFlags::PRESENT | PageTableFlags::NO_EXECUTE | PageTableFlags::WRITABLE;

        unsafe {
            // SAFETY: this is safe because the page was given by the system allocator

            mem::mem_map::map_page(
                x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(region_start),
                flags,
            );
            // This is safe because
            self.force_dealloc(ptr.cast(), layout)
        }
    }
}

impl FixedBlockInner {
    /// Searches for the next largest block above `target` and cracks it into as many `target`
    /// blocks as will fit into it. Returns `Ok(())` on success. Will always return `Err(())` When
    /// target is set to the largest block.
    ///
    /// #Panics
    ///
    /// This fn will panic if `target` is equal or greater than `BLOCK_SIZES.len()`
    fn crack(&mut self, target: usize) -> Result<(), ()> {
        assert!(target < BLOCK_SIZES.len());

        let mut node = &mut super::allocator_linked_list::RawLinkedList::new();
        let mut t_large = 0;

        for (i, l) in self.list_heads.iter_mut().skip(target + 1).enumerate() {
            if l.is_empty() {
                continue;
            } else {
                node = l;
                t_large = BLOCK_SIZES[i + target + 1];
                break;
            }
        }

        if t_large == 0 {
            // t_large is only 0 if no free nodes are found
            return Err(());
        }

        // SAFETY: This is safe `*node.fetch()` is guaranteed to be unused for `t_large` bytes
        let region = unsafe { core::slice::from_raw_parts_mut(node.fetch().unwrap(), t_large) }; // never none
        let t_target = BLOCK_SIZES[target];
        for block in 0..(t_large / t_target) {
            let new = &mut region[t_target * block] as *mut u8 as *mut u64;
            // SAFETY: This is safe because new is a legal address;
            unsafe { self.list_heads[target].add(new) }
        }
        Ok(())
    }
}

unsafe impl Allocator for NewFixedBlockAllocator {
    // The contained serial statements are feature gated because they cause a huge amount
    // of overhead, but are too useful to remove.
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let alloc = unsafe { &mut *self.inner.get() };
        alloc.alloc_count += 1;
        // return dangling if len == 0
        if layout.size() == 0 {
            let slice =
                unsafe { core::slice::from_raw_parts_mut(NonNull::<u8>::dangling().as_ptr(), 0) };
            return Ok(NonNull::new(slice).unwrap());
        }

        // Calculate best size.
        let index = if let Some(block_index) = Self::get_block_index(layout) {
            block_index
        } else {
            return Err(AllocError);
        };

        #[cfg(feature = "alloc-debug-serial")]
        serial_println!("Doing {}", index);

        // try get node
        if alloc.list_heads[index].is_empty() {
            #[cfg(feature = "alloc-debug-serial")]
            serial_println!("cracking");
            if let Err(()) = alloc.crack(index) {
                // on fail req new space from superior.

                #[cfg(feature = "alloc-debug-serial")]
                alloc.list_heads[index].dump();

                #[cfg(feature = "alloc-debug-serial")]
                serial_println!("extending");

                self.extend();

                #[cfg(feature = "alloc-debug-serial")]
                alloc.list_heads[index].dump();

                // map memory from superior
                if index != (BLOCK_SIZES.len() - 1) {
                    alloc.crack(index).unwrap(); // error should have occurred before here
                }
            }
        }

        #[cfg(feature = "alloc-debug-serial")]
        alloc.list_heads[index].dump();

        // SAFETY this is safe because fetch will return an unused section of `BLOCK_SIZES[index]` length;
        // every check has been made to ensure that fetch does not return `None`
        let slice = unsafe {
            core::slice::from_raw_parts_mut::<u8>(
                alloc.list_heads[index].fetch().unwrap(),
                BLOCK_SIZES[index],
            )
        };

        let ret = NonNull::new(slice).unwrap(); // slice is never null
        unsafe {
            let t = &mut *ret.as_ptr();
            let m = (&t[0]) as *const u8;
            m.read_volatile();
        }

        #[cfg(feature = "alloc-debug-serial")]
        serial_println!("Done doing {}", index);

        Ok(ret)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // determine layout
        if layout.size() == 0 {
            return;
        }

        let index = Self::get_block_index(layout).unwrap();

        #[cfg(feature = "alloc-debug-serial")]
        serial_println!("dealloc: Ptr {:#x}, index {}", ptr.as_ptr() as usize, index);

        // add ptr
        (*self.inner.get()).list_heads[index].add(ptr.cast::<u64>().as_ptr());
    }
}

impl InferiorAllocator for NewFixedBlockAllocator {
    const INIT_BYTES: usize = 4096;

    unsafe fn init(&self, ptr: *mut [u8; Self::INIT_BYTES]) {
        let alloc = unsafe { &mut *self.inner.get() };
        let initial_block_size = *BLOCK_SIZES.last().unwrap();
        let count = Self::INIT_BYTES / BLOCK_SIZES.last().unwrap(); // will never panic
        alloc.start = ptr as *mut u8;

        for i in 0..count {
            let location = ptr as usize + (i * initial_block_size);
            alloc
                .list_heads
                .last_mut()
                .unwrap()
                .add(location as *mut u64)
        }
    }

    unsafe fn force_dealloc(&self, ptr: NonNull<u8>, layout: Layout) {
        let mut complete = 0usize;
        let addr = ptr.as_ptr() as usize;
        assert_eq!(
            (addr as *const u64).align_offset(8),
            0,
            "FixedBlockAllocator cannot handle regions smaller than 8 bytes"
        );

        for i in BLOCK_SIZES.iter().rev() {
            // biggest first
            let num = *i;

            while (layout.size() - complete) >= num {
                let layout = Layout::from_size_align(num, 1).unwrap(); // size is never 0
                let ptr = NonNull::new((addr + complete) as *mut u8).unwrap(); // never None
                unsafe { self.deallocate(ptr, layout) }
                complete += num;
            }
            if complete == layout.size() {
                break;
            }
        }
    }
}
