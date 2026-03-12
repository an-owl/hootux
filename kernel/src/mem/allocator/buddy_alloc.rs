use crate::mem::high_order_alloc::{FreeMem, HighOrderAlloc};
use bootstrappable_buddy_allocator::{BuddyAllocator as Backend, Overflow};
use core::alloc::{AllocError, Layout};
use core::ptr::NonNull;

const ORDERS: usize = 18;
pub const MAX_ORDER_SIZE: usize = BackendConfig::ORDER_MAX_SIZE;
const HIGH_ORDER_BLOCK_SIZE: usize = MAX_ORDER_SIZE * 2;

type BackendConfig = Backend<
    ORDERS,
    12,
    Overflow,
    u64,
    memory_addresses::arch::x86_64::VirtAddr,
    alloc::alloc::Global,
>;

pub struct BuddyAllocator {
    inner: crate::util::mutex::ReentrantMutex<BuddyAllocatorInner>,
}

impl BuddyAllocator {
    pub const fn new() -> Self {
        BuddyAllocator {
            inner: crate::util::mutex::ReentrantMutex::new(BuddyAllocatorInner {
                backend: Backend::new(alloc::alloc::Global),
                high_order_alloc: HighOrderAlloc::new(),
                start: None,
                end: 0,
                extend_recurse: core::sync::atomic::AtomicBool::new(false),
            }),
        }
    }
}

struct BuddyAllocatorInner {
    // Allocator will guarantee that `self` is not
    backend: BackendConfig,
    high_order_alloc: HighOrderAlloc<usize, { HIGH_ORDER_BLOCK_SIZE as u32 }>,

    // Start and end addresses for heap
    // Start is immutable. End is not.
    start: Option<core::num::NonZeroUsize>,
    end: usize,

    extend_recurse: core::sync::atomic::AtomicBool,
}

impl BuddyAllocatorInner {
    // not this takes `self` for convenience
    fn layout_high(&self, layout: Layout) -> bool {
        // The maximum buddy alloc size is half the high order block size.
        layout.size().max(layout.align()) >= HIGH_ORDER_BLOCK_SIZE / 2
    }

    /// Extends `self.backend` by bumping the end of the heap.
    fn extend(&mut self) {
        self.backend
            .deallocate(
                MAX_ORDER_SIZE,
                memory_addresses::arch::x86_64::VirtAddr::new(self.end as u64),
            )
            .unwrap();
        self.end += MAX_ORDER_SIZE;
    }

    /// Extends `self.backend` using memory from `self.high_order_alloc`.
    ///
    /// Because `self.backend` cant store the whole block, the higher half of the block's address
    /// will be returned which can be "freed" into `self.backend` after the original allocation is satisfied.
    /// The size of the returned block is [MAX_ORDER_SIZE].
    fn extend_from_high(&mut self) -> Result<memory_addresses::arch::x86_64::VirtAddr, ()> {
        // When running this function memory may be allocated from `self` to satisfy storing its own free list.
        // This checks if this function is recursing. If it is it fails and the caller should call self.extend instead.
        match self.extend_recurse.compare_exchange(
            false,
            true,
            atomic::Ordering::Acquire,
            atomic::Ordering::Relaxed,
        ) {
            Ok(_) => {}
            Err(_) => return Err(()),
        }
        let Some(t) = self
            .high_order_alloc
            .allocate(Layout::from_size_align(HIGH_ORDER_BLOCK_SIZE, 1).unwrap())
        else {
            self.extend_recurse.store(false, atomic::Ordering::Release);
            return Err(());
        };
        let base = memory_addresses::arch::x86_64::VirtAddr::new(t.as_raw().0 as u64);
        let higher = base + MAX_ORDER_SIZE;

        // We use base here because backend will allocate the lowest value blocks, meaning that the
        // start of free memory after the original allocation is satisfied will be contiguous with the higher block.
        self.backend
            .deallocate(HIGH_ORDER_BLOCK_SIZE, base)
            .unwrap();
        self.extend_recurse.store(false, atomic::Ordering::Release);
        Ok(higher)
    }
}

unsafe impl super::HeapAlloc for BuddyAllocator {
    fn virt_allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let mut l = self.inner.lock();

        // `backend` can only return powers of two, this helps sanitize it.
        let size = layout.size().max(layout.align()).next_power_of_two();

        if l.layout_high(layout) {
            let Some(r) = l.high_order_alloc.allocate(layout) else {
                let tgt_addr = l.end;
                l.end += size;

                return Ok(NonNull::slice_from_raw_parts(
                    NonNull::new(tgt_addr as *mut u8).unwrap(),
                    size,
                ));
            };
            return Ok(NonNull::slice_from_raw_parts(
                NonNull::new(r.as_raw().0 as *mut u8).unwrap(),
                r.as_raw().1,
            ));
        }

        if layout.size() == 0 {
            Ok(NonNull::slice_from_raw_parts(NonNull::dangling(), 0))
        } else {
            let ret = match l.backend.allocate(size) {
                Ok(a) => NonNull::new(a.as_mut_ptr()).unwrap(),
                Err(_) => {
                    match l.extend_from_high() {
                        Ok(hold) => {
                            let rc = l.backend.allocate(size)?;
                            // This can never return the buddy.
                            // Part of the buddy is `rc`, this can be guaranteed because
                            // self.high_order_alloc **only** deals is BS aligned blocks.
                            l.backend.deallocate(MAX_ORDER_SIZE, hold).unwrap();
                            NonNull::new(rc.as_mut_ptr()).unwrap()
                        }
                        Err(()) => {
                            l.extend();
                            let t = l.backend.allocate(size)?;
                            NonNull::new(t.as_mut_ptr()).unwrap()
                        }
                    }
                }
            };

            // unwrap: Inserting `0` into self is a logical error.
            Ok(NonNull::slice_from_raw_parts(ret, size))
        }
    }

    fn virt_deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        let mut l = self.inner.lock();

        // `backend` can only return powers of two, this helps sanitize it.
        let size = layout.size().max(layout.align()).next_power_of_two();

        if l.layout_high(layout) {
            l.high_order_alloc
                .free(FreeMem::new(ptr.as_ptr() as usize, size).unwrap())
                .unwrap();
        }

        if layout.size() == 0 {
            return; // Success
        } else {
            match l.backend.deallocate(
                size,
                memory_addresses::arch::x86_64::VirtAddr::from_ptr(ptr.as_ptr()),
            ) {
                Ok(()) => return,
                Err(addr) => l
                    .high_order_alloc
                    .free(
                        FreeMem::new(addr.as_ptr::<u8>() as usize, HIGH_ORDER_BLOCK_SIZE).unwrap(),
                    )
                    .unwrap(),
            }
        }
    }
}

impl super::combined_allocator::SuperiorAllocator for BuddyAllocator {
    unsafe fn init(&mut self, addr: usize) {
        let mut l = self.inner.lock();
        l.start
            .replace(addr.try_into().unwrap())
            .ok_or(())
            .expect_err("Allocator already initialized");
        l.end = addr;
        l.extend();

        let r = l
            .backend
            .allocate(super::super::PAGE_SIZE)
            .map(|p| p.as_ptr::<u8>() as usize);
        assert_eq!(
            r,
            Ok(addr),
            "Allocator startup failed. Expected to pop {addr:#x}, got {:#x?} instead",
            r
        );
    }

    fn allocated_size(layout: Layout) -> usize {
        layout.size().max(layout.align()).next_power_of_two()
    }
}
