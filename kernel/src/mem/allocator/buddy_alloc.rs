use super::combined_allocator::InteriorAlloc;
use crate::mem;
use core::alloc::{AllocError, Layout};
use core::ptr::NonNull;
use x86_64::structures::paging::Mapper;
use x86_64::VirtAddr;

// Shamelessly nicked
// todo: replace when https://github.com/rust-lang/rust/pull/103093 is pulled
mod allocator_linked_list;

pub const ORDERS: usize = 11;
pub const ORDER_ZERO_SIZE: usize = 4096;
pub const ORDER_MAX_SIZE: usize = ORDER_ZERO_SIZE << ORDERS - 1;
const NODE_SIZE: usize = 24; // should be value of sizeof::<alloc::collections::linked_list::Node<usize>>() this cant be asserted at runtime
const DEFAULT_EXTEND_SIZE: usize = 8;
/// ratio of [ORDER_MAX_SIZE] to [HIGH_ORDER_BLOCK_SIZE]
const HIGH_ORDER_BLOCK_RATIO: u32 = 2;
const HIGH_ORDER_BLOCK_SIZE: u32 = (ORDER_MAX_SIZE as u32) * HIGH_ORDER_BLOCK_RATIO;

pub struct BuddyHeap {
    inner: core::cell::UnsafeCell<BuddyHeapInner>,
}

struct BuddyHeapInner {
    start: usize,
    end: usize,
    free_list: [allocator_linked_list::LinkedList<usize, InteriorAlloc>; ORDERS],
    high_order: super::super::high_order_alloc::HighOrderAlloc<usize, HIGH_ORDER_BLOCK_SIZE>,
    mem_cnt: super::memory_counter::MemoryCounter, // all operations on this *should* be safe to unwrap
}

impl BuddyHeap {
    /// Creates an uninitialized instance of `Self`
    pub const fn new() -> Self {
        Self {
            inner: core::cell::UnsafeCell::new(BuddyHeapInner {
                start: 0, // 0 is a theoretically illegal value. using self in this state should cause a page fault
                end: 0,
                // SAFETY: this is safe because.
                free_list: [const {
                    allocator_linked_list::LinkedList::new_in(unsafe { InteriorAlloc::new() })
                }; ORDERS],
                high_order: super::super::high_order_alloc::HighOrderAlloc::new(),
                mem_cnt: super::memory_counter::MemoryCounter::new(0),
            }),
        }
    }
}

impl BuddyHeapInner {
    /// Extends `self` by `ORDER_MAX_SIZE`  order block with a pre-allocated order-0 block at self.end.
    ///
    /// #Panics
    ///
    /// This fn will panic if the inferior allocator returns [core::alloc::AllocError], The
    /// caller should ensure that inferior has enough memory to run `bootstrap`
    fn bootstrap(&mut self) {
        // skip last order
        for i in 0..ORDERS - 1 {
            let block_size = ORDER_ZERO_SIZE << i;
            let block = block_size ^ self.end;
            self.free_list[i].push_front(block);
        }
        self.end += ORDER_MAX_SIZE;
        self.mem_cnt.extend(ORDER_MAX_SIZE);
        self.mem_cnt.reserve(ORDER_ZERO_SIZE).unwrap(); // should never panic
    }

    /// Extends `self` by `len * ORDER_MAX_SIZE` this fn may require an allocation to self, if no
    /// space is available use [Self::stack_extend]
    ///
    /// # Panics
    ///
    /// This fn will panic if [core::alloc::AllocError] is encountered
    fn extend(&mut self, len: usize) {
        if let Some(region) = self
            .high_order
            .allocate(Layout::from_size_align(len, 0x1000).unwrap())
        // this returns a FreeRegion not a pointer so the size is known regardless of the input
        {
            let (ptr, _) = region.as_raw();
            for i in 0..(HIGH_ORDER_BLOCK_RATIO as usize) {
                self.free_list[ORDERS - 1].push_back(i);
            }
            return;
        }

        for i in 0..len {
            let new_block = self.end + (i * ORDER_MAX_SIZE);
            self.free_list[ORDERS - 1].push_back(new_block);
        }

        self.end = self.end + (len * ORDER_MAX_SIZE);
        self.mem_cnt.extend(len * ORDER_MAX_SIZE);
    }

    /// Extends self by moving the end address by `len * ORDER_MAX_SIZE` returning region extended
    #[allow(dead_code)]
    // im not sure why I added this if I wasn't going to use it. It's probably here for something but IDK what.
    fn extend_allocate(&mut self, len: usize) -> NonNull<[u8]> {
        let ret = self.end;
        self.end += len * ORDER_MAX_SIZE;

        self.mem_cnt.extend(len * ORDER_MAX_SIZE);
        // shouldn't panic
        self.mem_cnt.reserve(len * ORDER_MAX_SIZE).unwrap();
        // SAFETY: This is safe because ret is guaranteed to not be unused.
        // new will never return None
        NonNull::new(unsafe {
            core::slice::from_raw_parts_mut(ret as *mut u8, len * ORDER_MAX_SIZE)
        })
        .unwrap()
    }

    /// Manually extends self by [ORDER_MAX_SIZE] without requiring free space in within `self`.
    /// This is done by mapping a new page at the ned of the managed heap region, adding it to the
    /// inferior allocator, then performing buddy calculations to allocate the used page into `self`
    ///
    /// #Safety
    /// This fn will map memory to `*self.end`
    unsafe fn stack_extend(&mut self) {
        use super::combined_allocator::InferiorAllocator;
        use x86_64::structures::paging::{page_table::PageTableFlags, FrameAllocator};

        // map new region
        let end_page = x86_64::structures::paging::Page::
        <x86_64::structures::paging::Size4KiB>::from_start_address(
            VirtAddr::new(self.end as u64)
        ).expect("BuddyAlloc has become misaligned");
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::HUGE_PAGE;
        let frame = mem::allocator::COMBINED_ALLOCATOR
            .lock()
            .phys_alloc()
            .get()
            .allocate_frame()
            .expect("System ran out of memory");

        mem::SYS_MAPPER
            .get()
            .map_to(end_page, frame, flags, &mut mem::DummyFrameAlloc)
            .expect("Failed to map memory for System Allocator")
            .flush();

        // deallocate new region to inferior
        InteriorAlloc::new().force_dealloc(
            NonNull::new(end_page.start_address().as_mut_ptr()).unwrap(),
            Layout::from_size_align(1, 4096).unwrap(),
        ); // these should never panic

        // append new blocks

        self.bootstrap()
    }

    /// Calculates the size of block form its order
    const fn block_size(order: usize) -> usize {
        assert!(order <= ORDERS, "Order above allowed scope");
        ORDER_ZERO_SIZE << order
    }

    /// Returns a region by calling [Self::extend_allocate].
    ///
    fn alloc_huge(&mut self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // ensures that size is aligned to HIGH_ORDER_BLOCK_SIZE
        let l = if layout.align() < HIGH_ORDER_BLOCK_SIZE as usize {
            layout
                .align_to(HIGH_ORDER_BLOCK_SIZE as usize)
                .unwrap()
                .pad_to_align()
        } else {
            layout
                .align_to(HIGH_ORDER_BLOCK_SIZE as usize)
                .unwrap()
                .pad_to_align()
                .align_to(layout.align())
                .unwrap()
        };

        let t = if let Some(p) = self.high_order.allocate(l) {
            p
        } else {
            let mut ne = self.end + layout.size();
            if ne & !(layout.align() - 1) != 0 {
                ne += layout.align();
            }

            self.extend(ne - self.end - 1); // end points to bit after last so sub 1
            self.high_order.allocate(l).ok_or(AllocError)?
        };
        // SAFETY: ptr comes from allocator, it is safe.
        Ok(unsafe { NonNull::new(t.into_ptr()).unwrap() })
    }

    /// Retrieves a block of size `order`, splitting and extending the heap as necessary
    fn fetch(&mut self, order: usize) -> usize {
        if let Some(block) = self.free_list[order].pop_front() {
            block
        } else {
            self.split(order);
            let ret = self.free_list[order]
                .pop_front()
                .expect("BuddyAlloc split() failed");
            ret
        }
    }

    /// Breaks unused blocks to create at least one free block at the target order.
    fn split(&mut self, order: usize) {
        for i in order..ORDERS {
            if let Some(block) = self.free_list[i].pop_front() {
                for j in (order..i).rev() {
                    let buddy = block ^ Self::block_size(j);
                    self.free_list[j].push_front(buddy);
                }

                self.free_list[order].push_front(block);
                return;
            }
        }

        if self.mem_cnt.get_free() >= NODE_SIZE * ORDERS {
            self.extend(DEFAULT_EXTEND_SIZE);
        } else {
            // SAFETY: This is probably safe.
            unsafe { self.stack_extend() };
            self.extend(DEFAULT_EXTEND_SIZE - 1);
        }

        self.split(order); // in theory this will only recurse once
    }

    /// Returns the order that should be used to store the given layout. Will choose a larger block
    /// for larger alignments.
    #[inline]
    fn order_from_layout(layout: Layout) -> Option<usize> {
        let mut use_order = None;
        let try_num = layout.size().max(layout.align());

        for i in 0..ORDERS {
            let order_size = ORDER_ZERO_SIZE << i;
            if try_num <= order_size {
                use_order = Some(i);
                break;
            }
        }

        use_order
    }

    /// Rejoins the described block into the free list, joining as many buddies as possible.
    fn rejoin(&mut self, addr: usize, order: usize) {
        let mut use_ord = ORDERS - 1;
        for i in order..ORDERS {
            let buddy = addr ^ Self::block_size(i);

            // just removing the buddy is fine enough

            let mut filter = self.free_list[i].drain_filter(|n| *n == buddy);

            if let None = filter.next() {
                // still calls Box::new
                if i != ORDERS - 1 {
                    drop(filter);
                } else {
                    // I could make this locate any adjacent memory regions and free those but this is just convenient

                    // gets the address of the lower buddy regardless of whether this is the higher one or not.
                    let ba = addr & !((HIGH_ORDER_BLOCK_SIZE as usize) - 1);

                    // Will never return None
                    let f = super::super::high_order_alloc::FreeMem::new(
                        ba,
                        HIGH_ORDER_BLOCK_SIZE as usize,
                    )
                    .unwrap();
                    self.high_order
                        .free(f)
                        .expect("Failed to move memory into HighOrderAlloc");
                }
                use_ord = i;
                break;
            }
        }
        self.free_list[use_ord].push_front(addr)
    }
}

unsafe impl super::HeapAlloc for BuddyHeap {
    /// This implementation will handle all sizes
    fn virt_allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        // calculate order from size.
        // SAFETY: This is safe because inner is only used within self
        // what idiot write this ^^, its incorrect. this is only safe within a mutex
        let alloc = unsafe { &mut *self.inner.get() };

        let use_order = if let Some(n) = BuddyHeapInner::order_from_layout(layout) {
            n
        } else {
            return if layout.size() > ORDER_MAX_SIZE {
                // if alloc size is huge
                alloc.alloc_huge(layout)
            } else {
                // unknown err
                Err(AllocError)
            };
        };

        let block = alloc.fetch(use_order);
        let slice = unsafe {
            NonNull::new(core::slice::from_raw_parts_mut(
                block as *mut u8,
                BuddyHeapInner::block_size(use_order),
            ))
            .unwrap()
        }; // Should never be None
        alloc
            .mem_cnt
            .reserve(BuddyHeapInner::block_size(use_order))
            .unwrap();

        Ok(slice)
    }

    fn virt_deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: This is safe because inner is only used within self
        let alloc = unsafe { &mut *self.inner.get() };

        if let Some(order) = BuddyHeapInner::order_from_layout(layout) {
            let ptr = {
                let mut ptr = ptr.as_ptr() as usize;
                ptr &= !((ORDER_ZERO_SIZE << order) - 1);
                NonNull::new(ptr as *mut u8).expect("Tried to deallocate illegal pointer")
            };

            // Ensure that ptr is within self's scope
            assert!(ptr.as_ptr() as usize >= alloc.start);
            assert!(ptr.as_ptr() as usize <= alloc.end);

            alloc.rejoin(ptr.as_ptr() as usize, order);

            alloc
                .mem_cnt
                .free(BuddyHeapInner::block_size(order))
                .unwrap();
        } else {
            // huge deallocation
            assert_eq!(
                layout.size() % ORDER_ZERO_SIZE,
                0,
                "Misaligned huge deallocation"
            );
            let count = layout.size() / ORDER_MAX_SIZE;
            let addr = ptr.as_ptr() as usize;
            for block in 0..count {
                let use_addr = (block * ORDER_MAX_SIZE) + addr;
                alloc.rejoin(use_addr, ORDERS - 1) // always max order;
            }
        };
    }
}

impl super::combined_allocator::SuperiorAllocator for BuddyHeap {
    unsafe fn init(&mut self, addr: usize) {
        // Assert aligned.
        let alloc = &mut *self.inner.get();
        assert_eq!(addr & (ORDER_MAX_SIZE - 1), 0);

        alloc.start = addr;
        alloc.end = addr;

        alloc.bootstrap();
    }

    fn allocated_size(layout: Layout) -> usize {
        BuddyHeapInner::block_size(BuddyHeapInner::order_from_layout(layout).unwrap())
        // panics if layout > ORDER_MAX_SIZE
    }
}
