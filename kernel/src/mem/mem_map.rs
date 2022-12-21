//! This module contains helper functions for mapping virtual memory, and this module should be the
//! preferred methods of doing so.

// todo: consider adding closures as args in these fro handling errors

use super::*;
use x86_64::structures::paging::{Mapper, PageTableFlags};

/// Flags for Normal data in L1 (4K) pages.
pub const PROGRAM_DATA_FLAGS: PageTableFlags = PageTableFlags::from_bits_truncate((1 << 63) | 0b11);

/// Flags for memory mapped I/O. Sets caching mode to UC uncacheable
pub const MMIO_FLAGS: PageTableFlags = PageTableFlags::from_bits_truncate((1 << 63) | 0b11011);

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
    page_table_tree::PageTableTree: Mapper<S>,
    BootInfoFrameAllocator: FrameAllocator<S>,
    offset_page_table::OffsetPageTable: Mapper<S>,
{
    for page in pages {
        let frame = FrameAllocator::<S>::allocate_frame(&mut *SYS_FRAME_ALLOCATOR.get())
            .expect("System ran out of memory");

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
/// #Panics
///
/// This fn will panic a mapped page is not `page<S>` or the frame address is invalid
///
/// #Safety
///
/// This fn is unsafe because it can be used to unmap in use pages that contain in use data.
pub unsafe fn unmap_range<'a, S: PageSize + core::fmt::Debug, I: Iterator<Item = Page<S>>>(pages: I)
where
    page_table_tree::PageTableTree: Mapper<S>,
    BootInfoFrameAllocator: FrameAllocator<S>,
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
/// #Panics
///
/// This fn will panic if a page within range is already mapped
///
/// #Safety
///
/// see [Mapper::map_to]
pub unsafe fn map_page<'a, S: PageSize + core::fmt::Debug>(page: Page<S>, flags: PageTableFlags)
where
    page_table_tree::PageTableTree: Mapper<S>,
    BootInfoFrameAllocator: FrameAllocator<S>,
    offset_page_table::OffsetPageTable: Mapper<S>,
{
    let frame = FrameAllocator::<S>::allocate_frame(&mut *SYS_FRAME_ALLOCATOR.get())
        .expect("System ran out of memory");

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
/// flushed from the tlb.
///
/// #Panics
///
/// This fn will panic if an error is encountered while unmapping the page, unlike [unmap_range]
/// this includes unmapped pages.
///
/// #Safety
///
/// See [unmap_range]\#Safety
pub unsafe fn unmap_page<'a, S: PageSize + core::fmt::Debug>(page: Page<S>)
where
    page_table_tree::PageTableTree: Mapper<S>,
    BootInfoFrameAllocator: FrameAllocator<S>,
    offset_page_table::OffsetPageTable: Mapper<S>,
{
    match SYS_MAPPER.get().unmap(page) {
        Ok((_, flush)) => flush.flush(),

        Err(err) => {
            panic!("{:?}", err)
        }
    }
}
