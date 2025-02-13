//! This module contains helper functions for mapping virtual memory, and this module should be the
//! preferred methods of doing so.

// todo: consider adding closures as args in these fro handling errors

use super::*;
use crate::mem::buddy_frame_alloc::FrameAllocRef;
use x86_64::structures::paging::mapper::{FlagUpdateError, TranslateError};

/// Flags for Normal data in L1 (4K) pages.
pub const PROGRAM_DATA_FLAGS: PageTableFlags = PageTableFlags::from_bits_truncate((1 << 63) | 0b11);

/// Flags for memory mapped I/O. Sets caching mode to UC uncacheable
pub const MMIO_FLAGS: PageTableFlags = PageTableFlags::from_bits_truncate((1 << 63) | 0b10011);

/// Maps the given pages into memory using frames given by the system frame allocator. This is the
/// preferred method Mapping memory ranges. This fn will flush all the given pages
/// from the tlb
///
/// #Panics
///
/// This fn will panic if a page within range is already mapped
///
/// #Safety
///
/// see [Mapper::map_to]
pub unsafe fn map_range<'a, S: PageSize + core::fmt::Debug, I: Iterator<Item = Page<S>>>(
    pages: I,
    flags: PageTableFlags,
) where
    FrameAllocRef<'a>: FrameAllocator<S>,
    offset_page_table::OffsetPageTable: Mapper<S>,
{
    let b = allocator::COMBINED_ALLOCATOR.lock();

    for page in pages {
        let frame_addr = b.phys_alloc().allocate(alloc::alloc::Layout::from_size_align(S::SIZE as usize, S::SIZE as usize).unwrap(),MemRegion::Mem64)
            .expect("System ran out of memory");
        let frame = PhysFrame::from_start_address(PhysAddr::new(frame_addr as u64)).unwrap();

        match SYS_MAPPER
            .get()
            .map_to(page, frame, flags, &mut DummyFrameAlloc)
        {
            Ok(flush) => flush.flush(),
            Err(err) => {
                panic!("{:?}", err);
            }
        }
    }
}

/// Unmaps pages without deallocating physical frames. Unmapped pages are skipped.
/// Pages will always bee flushed from the tlb.
///
/// # Panics
///
/// This fn will panic a mapped page is not `page<S>` or the frame address is invalid
///
/// # Safety
///
/// This fn is unsafe because it can be used to unmap in use pages that contain in use data.
pub unsafe fn unmap_range<'a, S: PageSize + core::fmt::Debug, I: Iterator<Item = Page<S>>>(pages: I)
where

    offset_page_table::OffsetPageTable: Mapper<S>,
{
    use x86_64::structures::paging::mapper::UnmapError;
    for page in pages {
        match SYS_MAPPER.get().unmap(page) {
            Ok((_, flush)) => flush.flush(),

            Err(UnmapError::PageNotMapped) => continue,

            Err(err) => {
                panic!("{:?}", err)
            }
        }
    }
}

/// Maps a single page of memory, flushing the tlb entry for the given page. This is the preferred
/// method if mapping a single page.
///
/// # Panics
///
/// This fn will panic if a page within range is already mapped
///
/// # Safety
///
/// see [Mapper::map_to]
pub unsafe fn map_page<'a, S: PageSize + core::fmt::Debug>(page: Page<S>, flags: PageTableFlags)
where
    FrameAllocRef<'a>: FrameAllocator<S>,
    offset_page_table::OffsetPageTable: Mapper<S>,
{
    let b = allocator::COMBINED_ALLOCATOR.lock();

    let frame_addr = b.phys_alloc().allocate(alloc::alloc::Layout::from_size_align(S::SIZE as usize, S::SIZE as usize).unwrap(),MemRegion::Mem64)
        .expect("System ran out of memory");
    let frame = PhysFrame::from_start_address(PhysAddr::new(frame_addr as u64)).unwrap();

    match SYS_MAPPER
        .get()
        .map_to(page, frame, flags, &mut DummyFrameAlloc)
    {
        Ok(flush) => flush.flush(),
        Err(err) => {
            panic!("{:?}", err);
        }
    }
}

/// Unmaps the specified page from memory without deallocating the frame. Pages will always be
/// flushed from the tlb. Returns the frame address.
///
/// # Panics
///
/// This fn will panic if an error is encountered while unmapping the page, unlike [unmap_range]
/// this includes unmapped pages.
///
/// # Safety
///
/// See [unmap_range](unmap_range#Safety)
pub unsafe fn unmap_page<'a, S: PageSize + core::fmt::Debug>(page: Page<S>)
where
    offset_page_table::OffsetPageTable: Mapper<S>,
{
    match SYS_MAPPER.get().unmap(page) {
        Ok((_, flush)) => flush.flush(),

        Err(err) => {
            panic!("{:?}", err)
        }
    }
}

pub(crate) unsafe fn unmap_and_free(addr: VirtAddr) -> Result<(),()>{

    let page = Page::<Size4KiB>::containing_address(addr);

    let free = |entry: x86_64::structures::paging::page_table::PageTableEntry,len| {
        if entry.flags().contains(frame_attribute_table::FRAME_ATTR_ENTRY_FLAG) {
            let op = frame_attribute_table::FatOperation::UnAlias;
            let fae = frame_attribute_table::ATTRIBUTE_TABLE_HEAD.do_op_phys(entry.addr(),op);

            let free = if let Some(ref fae) = fae {
                // if not aliased
                fae.alias_count() <= 1
            } else {
                // No aliases are present if no FAE is present
                true
            };

            // if no fae is present
            if free {
                let l = allocator::COMBINED_ALLOCATOR.lock();
                l.phys_alloc().dealloc(entry.addr().as_u64() as usize, len)
            }
        };

    };

    // We need to determine the size of the frame before we free it.
    match get_entry(page) {
        Ok(e) => {
            // Only necessary for 4k pages higher ones will return NotMapped
            if !e.flags().contains(PageTableFlags::PRESENT) {
                return Err(())
            }
            unmap_page(Page::<Size4KiB>::containing_address(addr));
            free(e,0x1000);
        },
        Err(GetEntryErr::NotMapped) => return Err(()),
        Err(GetEntryErr::ParentHugePage) => {
            let page = Page::<Size2MiB>::containing_address(addr);

            match get_entry(page) {
                Ok(e) => {
                    unmap_page(Page::<Size2MiB>::containing_address(addr));
                    free(e,0x200000);

                },
                // SAFETY: The 4K NotMapped arm will be taken not this one.
                Err(GetEntryErr::NotMapped) => unsafe { core::hint::unreachable_unchecked() },
                Err(GetEntryErr::ParentHugePage) => {
                    let page = Page::<Size1GiB>::containing_address(addr);
                    // No parent huge pages are possible. This would've returned unmapped on the 4k check. no errors are possible here.
                    unmap_page(Page::<Size1GiB>::containing_address(addr));
                    free(get_entry(page).unwrap(), 0x40000000);
                }
            }
        }
    }
    Ok(())
}

pub fn translate(addr: usize) -> Option<u64> {
    let addr = VirtAddr::new(addr as u64);

    {
        // 4k
        let page = Page::<Size4KiB>::containing_address(addr);
        let offset = addr.as_u64() & (0x1000 - 1);
        let ret = match SYS_MAPPER.get().translate_page(page) {
            Err(TranslateError::ParentEntryHugePage) => {
                let page = Page::<Size2MiB>::containing_address(addr);
                let offset = addr.as_u64() & (0x200000 - 1);
                match SYS_MAPPER.get().translate_page(page) {
                    Err(TranslateError::ParentEntryHugePage) => {
                        let page = Page::<Size1GiB>::containing_address(addr);
                        let offset = addr.as_u64() & (0x40000000 - 1);
                        SYS_MAPPER
                            .get()
                            .translate_page(page)
                            .ok()?
                            .start_address()
                            .as_u64()
                            + offset
                    }

                    r => r.ok()?.start_address().as_u64() + offset,
                }
            }
            r => r.ok()?.start_address().as_u64() + offset,
        };

        Some(ret)
    }
}

/// This fn gets the physical address of a linear pointer.
///
/// This fn is a wrapper for [translate]
// ptr is never dereferenced so this will only be a single impl
pub fn translate_ptr<T>(ptr: *const T) -> Option<u64> {
    translate(ptr as usize)
}

/// Updates the page flags of the given page
/// Only supports 4k pages atm
pub(crate) fn set_flags<S>(page: Page<S>, flags: PageTableFlags) -> Result<(), UpdateFlagsErr>
    where
        Page<S>: Copy,
        S: PageSize,
        offset_page_table::OffsetPageTable: Mapper<S>,

{
    let r = unsafe {
        SYS_MAPPER.get().update_flags(
            Page::<S>::from_start_address(VirtAddr::new(page.start_address().as_u64()))
                .map_err(|_| UpdateFlagsErr::InvalidAddress)?,
            flags,
        )
    };
    let p = page.start_address();
    r.map_err(|e| match e {
        FlagUpdateError::PageNotMapped => UpdateFlagsErr::PageNotMapped(p.as_u64()),
        FlagUpdateError::ParentEntryHugePage => UpdateFlagsErr::ParentHugePage(p.as_u64()),
    })?
    .flush();
    Ok(())
}

pub(crate) use offset_page_table::GetEntryErr;

/// Retrieves the page table entry for the given page.
/// This can be used to update the entries flags.
pub(crate) fn get_entry<S: PageSize + core::fmt::Debug + 'static>(page: Page<S>) -> Result<x86_64::structures::paging::page_table::PageTableEntry, GetEntryErr> {
    let l = allocator::COMBINED_ALLOCATOR.lock();

    // This can be completely removed by the compiler.
    let level = if core::any::TypeId::of::<S>() == core::any::TypeId::of::<Size4KiB>() {
        PageTableLevel::L1
    } else if core::any::TypeId::of::<S>() == core::any::TypeId::of::<Size2MiB>() {
        PageTableLevel::L2
    } else if core::any::TypeId::of::<S>() == core::any::TypeId::of::<Size1GiB>() {
        PageTableLevel::L3
    } else {
        // SAFETY: These are the only 3 possible variants.
        unsafe { core::hint::unreachable_unchecked() };
    };
    l.mapper().get_entry(level, page.start_address())
}

/// Maps the specified frame to the specified page, with the given flags.
///
/// The page-address is fetched from `addr` and will be treated as a `S` sized page.
/// `addr` will be automatically aligned.
///
/// An `invdpg` is not run on the updated page. This is because if `page` is present this will return an error.
pub(crate) fn map_frame_to_page<S: PageSize + core::fmt::Debug>(page: VirtAddr, frame: PhysFrame<S>, flags: PageTableFlags) -> Result<(),x86_64::structures::paging::mapper::MapToError<S>> where offset_page_table::OffsetPageTable: Mapper<S> {
    let page = Page::<S>::containing_address(page);
    let mut l = allocator::COMBINED_ALLOCATOR.lock();
    unsafe { l.mapper_mut().map_to(page,frame, flags, &mut DummyFrameAlloc) }.map(|f| f.ignore())
}

/// Updates page table flags regardless of page size.
/// This can be used to update one page or an iterator of pages.
/// This has a single page form and a range form.
///
/// * `($flags,$addr)` When a single page is used `$addr` must be a [Page] in order to determine which type of page.
/// * `($flags,$start_addr,$end_addr)` When using the range form the addresses must be [VirtAddr]
/// to so the page size can be determined automatically
///
/// Note: The second form may be used with the same `$start_addr` & `$end_addr` to modify a
/// single page while automatically resolving the size
#[macro_export]
macro_rules! update_flags {
    ($flags:expr, $addr:expr) => {
        $crate::mem::mem_map::set_flags($addr.into(), $flags)
    };
    ($flags:expr, $start_addr:expr, $end_addr:expr) => {

        match $crate::mem::mem_map::set_flags_iter(
            x86_64::structures::paging::page::PageRangeInclusive::<x86_64::structures::paging::Size4KiB>{
                start: x86_64::structures::paging::page::Page::containing_address($start_addr),
                end: x86_64::structures::paging::page::Page::containing_address($end_addr)},
            $flags)
        {
            Err($crate::mem::mem_map::UpdateFlagsErr::ParentHugePage(p)) if $start_addr.as_u64() == p => {

                match $crate::mem::mem_map::set_flags_iter(
                    x86_64::structures::paging::page::PageRangeInclusive::<x86_64::structures::paging::Size2MiB>{
                        start: x86_64::structures::paging::page::Page::containing_address($start_addr),
                        end: x86_64::structures::paging::page::Page::containing_address($end_addr)},
                    $flags) {
                    Err($crate::mem::mem_map::UpdateFlagsErr::ParentHugePage(p)) if $start_addr.as_u64() == p => {


                        $crate::mem::mem_map::set_flags_iter(
                            x86_64::structures::paging::page::PageRangeInclusive::<x86_64::structures::paging::Size1GiB>{
                                start: x86_64::structures::paging::page::Page::containing_address($start_addr),
                                end: x86_64::structures::paging::page::Page::containing_address($end_addr)},
                            $flags)

                    }
                    e => e,
                }
            }
            e => e,
        }
    }
}
pub use update_flags;

/// Iterates over `iter` updating each page.
/// If an error is encountered while this is running then all pages before the one returned will have updated flags.
pub(crate) fn set_flags_iter<P: PageSize, T: Iterator<Item = Page<P>>>(
    iter: T,
    flags: PageTableFlags,
) -> Result<(), UpdateFlagsErr>
    where offset_page_table::OffsetPageTable: Mapper<P> {
    for p in iter {
        set_flags(p, flags)?
    }
    Ok(())
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]

pub enum UpdateFlagsErr {
    PageNotMapped(u64),
    ParentHugePage(u64),
    InvalidAddress,
}
