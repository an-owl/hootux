use super::Locked;
use crate::mem;
use crate::mem::{DummyFrameAlloc, PageTableLevel};
use core::alloc::{AllocError, Allocator, Layout};
use core::fmt::{Debug, Formatter};
use core::ptr::NonNull;
use x86_64::structures::paging::page::PageRangeInclusive;
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB};
use x86_64::VirtAddr;

const PAGE_SIZE: usize = 4096;

pub static PT_ALLOC: Locked<PageTableAllocator> = Locked::new(PageTableAllocator::new());

/// This is a wrapper for PageTableAllocator it forwards all calls to PT_ALLOC
/// this is required because Copy is required for allocators however this is not reasonable
/// for PageTableAllocator
#[derive(Copy, Clone)]
pub struct PtAlloc;

unsafe impl Allocator for PtAlloc {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        PT_ALLOC.allocate(layout)
    }

    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        PT_ALLOC.allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        PT_ALLOC.deallocate(ptr, layout)
    }

    unsafe fn grow(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        PT_ALLOC.grow(ptr, old_layout, new_layout)
    }

    unsafe fn grow_zeroed(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        PT_ALLOC.grow_zeroed(ptr, old_layout, new_layout)
    }

    unsafe fn shrink(
        &self,
        ptr: NonNull<u8>,
        old_layout: Layout,
        new_layout: Layout,
    ) -> Result<NonNull<[u8]>, AllocError> {
        PT_ALLOC.shrink(ptr, old_layout, new_layout)
    }
}

//TODO change this to some sort of heap trait
unsafe impl Allocator for Locked<PageTableAllocator> {
    // todo optimize by remapping frames for realloc()
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        assert_eq!(
            layout.align(),
            PAGE_SIZE,
            "PageTableAllocator Layout not 4096"
        );
        assert_eq!(
            layout.size(),
            PAGE_SIZE,
            "PageTableAllocator layout.size not 4096"
        );

        let mut alloc = self.inner.lock();

        // basically `alloc.head.take()?`
        let head = {
            if let Some(head) = alloc.head.take() {
                head
            } else {
                // alloc is at absolute max size
                return Err(AllocError);
            }
        };

        // check head.next's contents
        // if none automatically generate new head
        // if some place it in alloc.head

        return match head.next.take() {
            Some(next) => {
                alloc.head = Some(next);
                Ok(NonNull::new(unsafe { Node::allocate(head) }).unwrap())
            }

            None => {
                //println!("head.next_addr(): {:x}, alloc.end_addr: {:x}",head.next_addr(),alloc.end_addr.as_u64());
                if (head.next_addr() < (alloc.end_addr.as_u64() as usize - (PAGE_SIZE * 3)))
                    || alloc.extend_self
                {
                    // check if there is 3 free pages or extend mode is active
                    alloc.head = Some(unsafe { head.autogen_next() })
                } else {
                    if let Err(_) = alloc.extend() {
                        return Err(AllocError);
                    }
                    alloc.head = Some(unsafe { head.autogen_next() })
                }
                Ok(NonNull::new(unsafe { Node::allocate(head) }).unwrap())
            }
        };
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        assert_eq!(layout.size(), PAGE_SIZE);
        assert_eq!(layout.align(), PAGE_SIZE);
        let mut alloc = self.inner.lock();

        let new = Node::new(VirtAddr::from_ptr(ptr.as_ptr()));

        match alloc.head.take() {
            None => alloc.head = Some(new),
            Some(old) => {
                // move old onto new.next and new into alloc.head
                new.next = Some(old);
                alloc.head = Some(new)
            }
        }
    }
}

/// This contains information about the next usable location on the heap
/// align is 4096 because it should only be used for 4096 byte Pages
#[repr(align(4096))]
struct Node {
    next: Option<&'static mut Self>,
}

/// Node can't derive Debug because then it would print all the following nodes
impl Debug for Node {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        return match self.next {
            None => write!(f, "Node {{ None }}"),
            Some(_) => write!(f, "Node {{ Some }}"),
        };
    }
}

impl Node {
    /// Creates a new node at addr
    ///
    /// This function is unsafe because the caller must ensure that addr is valid
    unsafe fn new(addr: VirtAddr) -> &'static mut Self {
        addr.as_mut_ptr::<Self>().write(Self { next: None });
        &mut *addr.as_mut_ptr::<Self>()
    }

    /// Converts Self into `*mut \[u8;4096] starting at self
    ///
    /// This function is unsafe because it violates memory safety by
    /// potentially allocating already allocated space
    unsafe fn allocate(s_elf: *mut Self) -> *mut [u8] {
        let addr = s_elf as *mut u8;
        core::slice::from_raw_parts_mut(addr, 4096)
    }

    /// Creates a Node 4096 bytes after self effectively referencing the next region of memory
    ///
    /// this function is unsafe because the caller must guarantee that
    /// `&const self + 4096[4096]` is a valid address space
    unsafe fn autogen_next(&self) -> &'static mut Self {
        let mut self_addr = VirtAddr::from_ptr(self);
        self_addr += PAGE_SIZE;
        Node::new(self_addr)
    }

    fn next_addr(&self) -> usize {
        let mut self_addr = VirtAddr::from_ptr(self);
        self_addr += PAGE_SIZE;
        self_addr.as_u64() as usize
    }
}

/// heap manager used exclusively for Page Tables
///
/// `end_addr` refers to end of controlled area. end addr may be changed to extend or shrink the heap
/// `head` contains the next page to be allocated. When `alloc()` is called head is checked against `end_addr`
/// to check how much space is remaining. 3 pages should remain free so self can be extended.
/// The last Node in head should always be the highest in memory unless `at_absolute_max` is true.
/// When `at_absolute_max` is true `extend_self` should also be true
#[derive(Debug)]
pub struct PageTableAllocator {
    start_addr: VirtAddr,
    end_addr: VirtAddr,
    head: Option<&'static mut Node>,
    at_absolute_max: bool,
    extend_self: bool,
}

impl PageTableAllocator {
    const ALIGNMENT_INDEX: PageTableLevel = PageTableLevel::L4;
    // this is exclusive Self may not reach this size
    const ABSOLUTE_MAX: usize = mem::addr_from_indices(1, 0, 0, 0);

    /// Create an uninitialized instance of PageTableAllocator
    const fn new() -> Self {
        Self {
            start_addr: VirtAddr::new_truncate(0),
            end_addr: VirtAddr::new_truncate(0),
            head: None,
            at_absolute_max: false,
            extend_self: false,
        }
    }

    /// Initializes self. this is required because the construct for an impl Allocator must be `const`
    ///
    /// This function is unsafe because the caller must ensure that `start_addr`..`end_adr` is
    /// mapped and writable inclusively
    pub unsafe fn init(&mut self, start_addr: VirtAddr, end_addr: VirtAddr) {
        self.start_addr = start_addr;
        self.end_addr = end_addr + 0x1000u64; // end addr should be exclusive, end_addr is the first byte not mapped

        // this drops an initial node as start_addr so alloc may be called.
        // self.head should always be Some otherwise it has reached the end
        // of its address limits.
        // if the node contained within head is none and &head.next + 4096 < end
        // that address may become head.next

        core::ptr::write(start_addr.as_mut_ptr(), Node { next: None });

        self.head = Some(&mut *start_addr.as_mut_ptr());
    }

    fn extend(&mut self) -> Result<(), ()> {
        const EXTEND_SIZE: usize = 32; // defines the number of pages allocated per extend()
        self.extend_self = true;

        let flags = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::NO_EXECUTE
            | PageTableFlags::GLOBAL
            | PageTableFlags::HUGE_PAGE; // todo make const

        let new_end_addr = self.end_addr + EXTEND_SIZE * PAGE_SIZE;

        match self.check_advance_end(new_end_addr.as_u64()) {
            Ok(_) => {}
            Err(_) => {
                self.at_absolute_max = true;
                return Err(()); // todo check for new extend size and use that
            }
        };

        let range = PageRangeInclusive::<Size4KiB> {
            start: Page::containing_address(self.end_addr),
            end: Page::containing_address(new_end_addr),
        };

        // map new memory

        unsafe {
            let b = mem::allocator::COMBINED_ALLOCATOR.lock();
            for page in range {
                let frame = b.phys_alloc().get().allocate_frame().unwrap();
                mem::SYS_MAPPER
                    .get()
                    .map_to(page, frame, flags, &mut DummyFrameAlloc)
                    .unwrap()
                    .flush();
            }
        }
        self.end_addr = new_end_addr;
        self.extend_self = false;

        Ok(())
    }

    /// calculates whether self can advance the given amount of space.
    /// returns reason on error
    ///
    /// on `Err(_)` the caller should set `self.at_absolute_max" to `true`
    /// Err(AdvanceError::Overflow) contains how far the new address overflowed
    // todo calculate amount of space to extend into instead
    #[inline]
    fn check_advance_end(&self, increase_by: u64) -> Result<(), AdvanceError> {
        const MAX_ADDR: u64 = VirtAddr::new_truncate(u64::MAX).as_u64();

        if self.at_absolute_max {
            return Err(AdvanceError::AlreadyAtMaximum);
        }

        return match VirtAddr::try_new(increase_by + self.end_addr.as_u64()) {
            Ok(new_addr) => {
                if self.end_addr > VirtAddr::new(increase_by) {
                    return Err(AdvanceError::TriedToDecrease);
                }

                if Self::ALIGNMENT_INDEX
                    .get_index(Page::<Size4KiB>::containing_address(self.end_addr))
                    == Self::ALIGNMENT_INDEX
                        .get_index(Page::<Size4KiB>::containing_address(new_addr))
                {
                    let overflow = new_addr.as_u64() % Self::ABSOLUTE_MAX as u64;
                    return Err(AdvanceError::OverFlow(overflow));
                }

                Ok(())
            }
            Err(err) => {
                let overflow = MAX_ADDR - err.0;
                return Err(AdvanceError::OverFlow(overflow));
            }
        };
    }
}

#[derive(Debug)]
enum AdvanceError {
    OverFlow(u64),
    TriedToDecrease,
    AlreadyAtMaximum,
}
