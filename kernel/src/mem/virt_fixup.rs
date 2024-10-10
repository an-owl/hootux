/// This module contains components for performing page-fixups. A fixup is when a page is fault
/// occurs and is handled by mapping the faulting page into memory.
///
/// This works by storing a list of entries of contiguous memory regions.
/// Entries are located using a binary search algorithm
/// Each entry contains a start address and length, the entry tracks how many fixups have occurred.
/// If all pages have been "fixed-up" then the entry is removed from the list.
/// Some entries are shrinkable, these entries are not removed as they may be used again.
/// All pages which are fixed up will be filed with `0`'s.
///
/// A user which *may* unmap memory from the list must set `shrinkable` to `true`. If this is not
/// done then may result in a kernel panic.
///
/// Fixup events are always handled by the kernel.

use core::cmp::Ordering;
use x86_64::VirtAddr;

static KERNEL_FIXUP_LIST: FixupList = FixupList::new();

/// Inserts a new fixup entry into the fixup list.
///
/// The fixup region will span from `address` to `address + len` in bytes. The Page size field
/// must be a valid page size. Fixups may only use one size of frame. Only 4KiB pages should be
/// used in kernel fixups.
///
/// If the requested region intersects with an existing region this will return `Err(())`
pub fn set_fixup_region<T>(address: *const T, len: usize, page_size: usize, shrinkable: bool) -> Result<(),()> {
    KERNEL_FIXUP_LIST.insert(address,len,page_size,shrinkable)
}

/// Removes a fixup region starting at the `address`
///
/// `address` must be the start starting address of the fixup region.
pub fn remove_fixup_region<T>(address: *const T) -> Result<(),()> {
    KERNEL_FIXUP_LIST.remove(VirtAddr::from_ptr(address))
}

pub(crate) fn query_fixup<'a>() -> Option<CachedFixup<'a>> {
    let addr = x86_64::registers::control::Cr2::read();
    KERNEL_FIXUP_LIST.try_fixup(addr)
}

pub(crate) struct FixupList {
    list: spin::RwLock<alloc::vec::Vec<FixupEntry>>,
}

impl FixupList {
    pub const fn new() -> Self {
        Self {
            list: spin::RwLock::new(alloc::vec::Vec::new()),
        }
    }

    /// See [set_fixup]
    pub fn insert<T>(&self, address: *const T, len: usize, page_size: usize, shrinkable: bool) -> Result<(),()> {

        #[cfg(target_arch = "x86_64")]
        assert!(page_size == 0x1000 || page_size == 0x20_0000 || page_size == 0x40000000, "Invalid Page Size: {page_size:#x}");
        #[cfg(not(target_arch = "x86_64"))]
        compile_error!("Page size must be defined");

        let fixup = FixupEntry {
            address: VirtAddr::from_ptr(address),
            len,
            fixup_remain: atomic::Atomic::new(0),
            page_size,
            clear_when_fixed: !shrinkable,
        };

        let l = self.list.upgradeable_read();

        match l.binary_search(&fixup) {
            Ok(_) => Err(()),
            Err(index) => {
                l.upgrade().insert(index, fixup);
                Ok(())
            }
        }
    }

    /// Resolves `address` to a fixup region and returns that region if it is present
    /// alongside the page size for the region.
    pub(crate)fn try_fixup(&self, address: VirtAddr) -> Option<(CachedFixup)> {
        let l = self.list.upgradeable_read();
        match l.binary_search_by(|e| address.partial_cmp(e).unwrap()) {

            Ok(i) => {
                Some(CachedFixup{
                    lock: l,
                    index: i,
                    address
                })
            },
            Err(_) => None,
        }
    }

    /// See [remove_fixup_region]
    pub fn remove(&self, address: VirtAddr) -> Result<(),()> {
        let l = self.list.upgradeable_read();
        match l.binary_search_by(|e| e.address.cmp(&address)) {
            Err(_) => Err(()),
            Ok(index) => {
                l.upgrade().remove(index);
                Ok(())
            }
        }
    }
}

impl PartialEq<FixupEntry> for VirtAddr {
    fn eq(&self, other: &FixupEntry) -> bool {
        *self >= other.address && *self <= other.address + other.len
    }
}

impl PartialOrd<FixupEntry> for VirtAddr {
    fn partial_cmp(&self, other: &FixupEntry) -> Option<Ordering> {
        if *self >= other.address + other.len {
            Some(Ordering::Greater)
        } else if *self < other.address {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Equal)
        }
    }
}

// todo do i need to distinguish between COW and non-COW fixups?
struct FixupEntry {
    /// Start address of the fixup region
    address: VirtAddr,
    /// Len in bytes
    len: usize,
    /// Number of bytes not allocated to the region
    // It's slightly easier to count in bytes than pages
    fixup_remain: atomic::Atomic<usize>,
    page_size: usize,
    /// Indicates whether the fixup entry will be removed when `fixup_remain == 0`
    clear_when_fixed: bool,
}

impl FixupEntry {
    fn fixup(&self,addr: VirtAddr) -> (VirtAddr,u64) {
        self.fixup_remain.fetch_sub(self.page_size,atomic::Ordering::Relaxed);
        (VirtAddr::new(!(self.page_size as u64) - 1 & addr.as_u64()), self.page_size as u64)
    }
}

impl PartialEq for FixupEntry {
    fn eq(&self, other: &FixupEntry) -> bool {
        self.address <= other.address + other.len && other.address <= self.address + self.len
    }
}

impl PartialOrd for FixupEntry {
    fn partial_cmp(&self, other: &FixupEntry) -> Option<Ordering> {
        if self.address <= other.address + other.len {
            Some(Ordering::Greater)
        } else if other.address <= self.address + self.len {
            Some(Ordering::Less)
        } else {
            Some(Ordering::Equal)
        }
    }
}

impl Eq for FixupEntry {}
impl Ord for FixupEntry {
    fn cmp(&self, other: &FixupEntry) -> Ordering {
        if self.address <= other.address + other.len {
            Ordering::Greater
        } else if other.address <= self.address + self.len {
            Ordering::Less
        } else {
            Ordering::Equal
        }
    }
}

pub(crate) struct CachedFixup<'a> {
    lock: spin::RwLockUpgradableGuard<'a, alloc::vec::Vec<FixupEntry>>,
    index: usize,
    address: VirtAddr
}


impl<'a> CachedFixup<'a> {
    pub fn fixup(mut self) {
        let (addr,size) = self.lock[self.index].fixup(self.address);

        // Force check that this can be acquired safely again.
        // We are just going to panic on a page fault if this fails anyway.
        // This shouldn't panic but if it does we **need** to catch and fix it.
        let l = super::allocator::COMBINED_ALLOCATOR.try_lock_pedantic().expect("Tried to fixup page while mm is in use");

        // SAFETY: map_page() here is safe, this is intended to handle a page fault. The faulting code will expect this data to be accessible this makes it so.
        // from_addr_unchecked() here is safe, `try_fixup` will align the address to the required value.
        #[cfg(target_arch = "x86_64")]
        match size {
            0x1000 => unsafe { super::mem_map::map_page::<x86_64::structures::paging::page::Size4KiB>(x86_64::structures::paging::Page::from_start_address_unchecked(addr), super::mem_map::PROGRAM_DATA_FLAGS) },
            0x20_0000 => unsafe { super::mem_map::map_page::<x86_64::structures::paging::page::Size2MiB>(x86_64::structures::paging::Page::from_start_address_unchecked(addr), super::mem_map::PROGRAM_DATA_FLAGS | x86_64::structures::paging::PageTableFlags::HUGE_PAGE) }
            0x4000_0000 => unsafe { super::mem_map::map_page::<x86_64::structures::paging::page::Size1GiB>(x86_64::structures::paging::Page::from_start_address_unchecked(addr), super::mem_map::PROGRAM_DATA_FLAGS | x86_64::structures::paging::PageTableFlags::HUGE_PAGE) }
            _ => unsafe { core::hint::unreachable_unchecked() } // SAFETY `size` is only defined to be ont of the above values
        }

        drop(l);
        #[cfg(not(target_arch = "x86_64"))] {
            compile_error!("No page sizes configured")
        }

        let index = self.index;
        let entry =  &self.lock[index];
        entry.fixup_remain.fetch_sub(entry.page_size, atomic::Ordering::Relaxed);
        if entry.fixup_remain.load(atomic::Ordering::Relaxed) <= 0 && entry.clear_when_fixed {
            let mut l = self.lock.upgrade();
            l.remove(index);
        }

        /*
        Unfortunately for huge pages we may need to change the cache mode to prevent this filling up the cache.
        Normal pages are fine. Disable cache line fills?
        This shouldn't occupy the entire memory bus, it should be a "Fill with pattern" command
         */
        // SAFETY: We just allocated this
        unsafe { core::slice::from_raw_parts_mut(addr.as_mut_ptr::<u8>(), size as usize) }.fill(0);
    }
}