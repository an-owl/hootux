//! This module handles "Frame attributes".
//!
//! Frame attributes are optional metadata about physical frame entries, this includes how may times
//! a frame used ant what it may be used for.
//! This is required for safely aliasing physical frames, which may be required for DMA operations.
//!
//! The Frame Attribute Table (FAT) is an array located at entry 511 in the highest order page table (on x86).
//! The table is an array of `Option<Box<T>>`'s where `T` is the Frame Attribute Entry (FAE).
//! As a result lookups in the FAT are "O=1".
//! Because other CPUs may add or remove pages when they are no longer used by the FAT before
//! accessing each page the CPU must perform a `invlpg` for each different page accessed.
//! While this isn't perfect it's better than the alternatives.
//!
//! The FAT is initially intended for safely handling DMA operations with otherwise unsound lifetimes.
//! Currently only options for DMA are implemented, due to this all aliased frames must be mapped to
//! read-only pages.
//!
//! ## Future challenges
//!
//! In the future this module shoe be able to handle aliased writable pages, however this prevents
//! usage with DMA. If multiple pages mutably alias a single frame when a DMA operation is requested
//! we cannot set other aliases to read only. Mutably aliased frames must not be allowed to be used
//! for DMA, we need some way to detect this and prevent it.

/// Identifies the physical frame in the page table entry as using a frame attribute entry.
///
/// When a page with this flag set causes a page fault or if this page is unmapped, the handler may
/// need to take action depending on the FAE attributes.
pub const FRAME_ATTR_ENTRY_FLAG: x86_64::structures::paging::page_table::PageTableFlags =   x86_64::structures::paging::page_table::PageTableFlags::BIT_9;

use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use atomic::Atomic;
use x86_64::{PhysAddr, VirtAddr};
use core::alloc::Allocator as _;

// addresses here are to the last entry of the last entry int the highest level page table.
const BITS48_TABLE_ADDRESS: usize = 0xFFFFFF8000000000;
const BITS48_TABLE_SIZE: usize = 0x10_0000_0000;

const BITS57_TABLE_ADDRESS: usize = 0xFFFF000000000000;
const BITS57_TABLE_SIZE: usize = 0x2000_0000_0000;

pub static ATTRIBUTE_TABLE_HEAD: FrameAttributeTable = FrameAttributeTable::new();

pub struct FrameAttributeTable {
    // this can probably avoid spinning using rwlock but accesses should be fast as hell anyway
    // It's likely that if only 2 CPUs access this it will never spin more than once.
    table: spin::Mutex<Option<&'static mut [Option<Arc<FrameAttributesInner>>]>>,
}

impl FrameAttributeTable {
    const fn new() -> Self {
        Self {
            table: spin::Mutex::new(None),
        }
    }

    /// Initialized the FAT using either 57 or 48 bit addressing.
    pub(super) fn init(&self) {
        let is_57_bit = x86_64::registers::control::Cr4::read().contains(x86_64::registers::control::Cr4Flags::from_bits(1<<12).unwrap()); // check la57 bit
        let mut l = self.table.lock();
        let (len,addr) = if is_57_bit {
            (BITS57_TABLE_SIZE, BITS57_TABLE_ADDRESS)
        } else {
            (BITS48_TABLE_SIZE, BITS48_TABLE_ADDRESS)
        };

        // Can only fail if this region is already configured for fixup, which it shouldn't.
        // `len` is in num of entries, multiply by size of entry to get len in bytes
        crate::mem::virt_fixup::set_fixup_region(addr as *const u8,len * core::mem::size_of::<Option<Arc<FrameAttributesInner>>>(),crate::mem::PAGE_SIZE,true).unwrap();
        // SAFETY: Should be safe, I should really write up a memory map to track these things.
        // This memory is not accessible but a fixup will be performed whenever it is accessed which will initialize it to `0`
        *l = Some( unsafe { core::slice::from_raw_parts_mut(addr as *mut _, len) });
    }

    /// Fetches a FAE using a physical address.
    pub(crate) fn lookup(&self, frame: PhysAddr) -> Option<FrameAttributes> {
        let l = self.table.lock();
        let table = l.as_ref().unwrap();

        let index = self.select(frame,table);
        Some(FrameAttributes {inner: table[index].clone()?, addr: frame})
    }

    /// Fetches a single FAE from the FAT. The returned FAE will be for the address pointed to by
    /// `ptr` and not any following addresses.
    pub fn lookup_ptr<T: ?Sized>(&self, ptr: *const T) -> Option<FrameAttributes> {
        let phys = crate::mem::mem_map::translate(ptr.cast::<u8>() as usize)?;
        self.lookup(PhysAddr::new(phys))
    }

    /// Performs an `invlpg` on the required index, returns the index for the address.
    fn select(&self, frame: PhysAddr, bank: &[Option<Arc<FrameAttributesInner>>]) -> usize {
        let index = frame.as_u64() as usize >> 12;

        // This causes all accesses to be a TLB miss, however this *should* be made up for by the reduced lookup complexity
        x86_64::instructions::tlb::flush(VirtAddr::from_ptr(&bank[index]));
        index
    }

    /// Removes an entry for the requested frame.
    ///
    /// This is followed up with a scan of the page to determine if the page can be freed.
    ///
    /// # Safety
    ///
    /// This fn is unsafe because the caller muse ensure that the requested entry is no longer required.
    /// If the alias field is greater than `1` this may cause UB.
    unsafe fn rm_page_entry(&self, frame: PhysAddr) {
        let mut l = self.table.lock();
        let table = l.as_mut().unwrap();

        self.rm_frame_entry_inner(frame,table);
    }

    unsafe fn rm_frame_entry_inner(&self, frame: PhysAddr, table: &mut [Option<Arc<FrameAttributesInner>>]) {
        let i = self.select(frame, table);

        let _ = table[i].take();

        let align = i & !(512 - 1); // 512 is number of entries per page
        // Will this compile to a `repnz scasq`?
        if table[align..align + 512].iter().all(|e| e.is_none()) {
            let addr = VirtAddr::from_ptr(&table[align]);

            // SAFETY: This struct handles TLB synchronization internally
            crate::mem::tlb::without_shootdowns(||
                // SAFETY: This page is checked above for data, this only occurs if not useful data is present in the page.
                crate::mem::mem_map::unmap_page(x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(addr))
            )
        }
    }

    /// Performs the operation specified by `op` on `tgt`. See [FatOperation] for more information.
    ///
    /// A pointer will be returned depending on the value of `op`. If `op` is constant then the
    /// caller may safely `unwrap()` the returned value.
    /// This fn may return a `*const T`, the caller may choose to cast this to a `*mut T`.
    /// This is not done automatically because this fn does not know whether the returned pointer can be mutable.
    ///
    /// # Safety
    ///
    /// See [FatOperation] for panic and safety information.
    // This panics because returning Err(_) requires us to undo all changes from the op, which where not preserved.
    // having `tgt` partially updated can result in UB if other CPUs access `tgt`. This also locks
    // the inner mutex longer than I'd like.
    pub unsafe fn do_op<T: ?Sized>(&self, tgt: *const T, op: FatOperation) -> Option<*const T> {
        // SAFETY: This is kind of safe. If the size >isize then the slice will overflow into a non-canonical address range.
        // The kernel should not allow slices like these to exist by normal means.
        // If `tgt` does not do this that is a problem with whatever constructed `tgt`, not this.
        let len = unsafe { core::mem::size_of_val_raw(tgt) };
        // we use usize_max because it's an impossible index
        let mut last_page = usize::MAX;
        let mut l = self.table.lock();
        let table = l.as_mut().unwrap();

        // Returned pointer when required by `op`
        let virt = if op.req_virt() {
            // I'm pretty sure this is safe.
            Some(crate::alloc_interface::VirtAlloc.allocate(core::alloc::Layout::from_size_align(len, unsafe { core::mem::align_of_val_raw(tgt) }).unwrap()).unwrap())
        } else {
            None
        };

        for offset in (0..len).step_by(4096) {

            // Fetch page table entry, we ensure that if it uses memory fixups that it is fixed up.
            let addr = VirtAddr::new((tgt.cast::<u8>() as usize + offset) as u64);
            let (entry, size) = match Self::get_entry(addr) {
                None => {
                    if let Some(c) = crate::mem::virt_fixup::query_fixup() {
                        c.fixup();
                        Self::get_entry(addr).unwrap()
                    } else {
                        panic!("Failed to perform operation {addr:?} not mapped");
                    }
                }
                Some(r) => r,
            };


            if let FatOperation::NewFae{..} | FatOperation::NewAlias {..} = op {
                use x86_64::structures::paging::Page;
                use x86_64::structures::paging::{Size4KiB, Size2MiB, Size1GiB};

                let mut new_flags = entry.flags() | FRAME_ATTR_ENTRY_FLAG;

                new_flags.set(x86_64::structures::paging::PageTableFlags::WRITABLE, op.initial_virt_writable() );
                // unwrap because there is no reason these can throw an error
                match size {
                    0x1000 => crate::mem::mem_map::set_flags(Page::<Size4KiB>::containing_address(addr), new_flags).unwrap(),
                    0x200000 => crate::mem::mem_map::set_flags(Page::<Size2MiB>::containing_address(addr), new_flags).unwrap(),
                    0x40000000 => crate::mem::mem_map::set_flags(Page::<Size1GiB>::containing_address(addr), new_flags).unwrap(),
                    _ => panic!("Illegal page size"),
                }
            };

            // try to reduce TLB misses
            let this_pg = addr.as_u64() as usize >> (12 + 9); // >> 12 gives us the entry index, >> 9 gives us the entries page number.
            if this_pg != last_page {
                last_page = this_pg;
                self.select(entry.addr(), table);
            };
            // Fetch the FAE and operate on it.
            let index = entry.addr().as_u64() as usize >> 12;
            if op.operate(&mut table[index], entry.addr()) {
                // SAFETY: This is safe, operate() determines if this should be called.
                self.rm_frame_entry_inner(entry.addr(), table);
            }

            // if the op require mapping a region then that's what we do.
            if let Some(ref v) = virt {
                // These unwrap() because errors in this function must panic on errors.
                let mut set_flags = entry.flags();
                set_flags.remove(x86_64::structures::paging::PageTableFlags::WRITABLE);
                match size {
                    0x1000 => {
                        crate::mem::mem_map::map_frame_to_page(
                            VirtAddr::from_ptr( unsafe { v.as_ptr().byte_add(offset) }),
                            unsafe { x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size4KiB>::from_start_address_unchecked(entry.addr()) },
                            set_flags | FRAME_ATTR_ENTRY_FLAG
                        ).unwrap()
                    },
                    0x200000 => {
                        crate::mem::mem_map::map_frame_to_page(
                            VirtAddr::from_ptr(unsafe { v.as_ptr().byte_add(offset) }),
                            unsafe { x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size2MiB>::from_start_address_unchecked(entry.addr()) },
                            set_flags | FRAME_ATTR_ENTRY_FLAG,
                        ).unwrap()
                    }
                    0x40000000 => {
                        crate::mem::mem_map::map_frame_to_page(
                            VirtAddr::from_ptr(unsafe { v.as_ptr().byte_add(offset) }),
                            unsafe { x86_64::structures::paging::PhysFrame::<x86_64::structures::paging::Size2MiB>::from_start_address_unchecked(entry.addr()) },
                            set_flags | FRAME_ATTR_ENTRY_FLAG,
                        ).unwrap()
                    }
                    #[cfg(target_arch = "x86_64")]
                    _ => unreachable!(),
                    #[cfg(not(target_arch = "x86_64"))]
                    _ => panic!("Unchecked page size")
                };
            }
        }

        // Because T is ?Sized, this copies the metadata of `tgt` onto `addr`
        virt.map(|addr| {
            unsafe {
                addr.as_ptr().with_metadata_of(tgt).cast_const()
            }
        })
    }

    /// Performs the given operation on the `addr` FAE. This will return the FAE for `addr` is one is present after the operation.
    ///
    /// This cannot be used to perform aliasing operations because it will not return an aliased pointer.
    ///
    /// See [FatOperation] for more details.
    pub fn do_op_phys(&self, addr: PhysAddr, op: FatOperation) -> Option<FrameAttributes> {
        let mut l = self.table.lock();
        let table = l.as_mut().unwrap();
        let index = self.select(addr, table);

        op.operate(&mut table[index], addr);

        let fae = table[index].as_ref().and_then(|e| { Some(e.clone()) });
        Some(FrameAttributes { inner: fae?, addr })
    }

    /// Returns the page table entry for the given address, alongside the page size.
    fn get_entry(ptr: VirtAddr) -> Option<(x86_64::structures::paging::page_table::PageTableEntry, usize)> {
        let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(VirtAddr::new(ptr.as_u64()));

        match crate::mem::mem_map::get_entry(page) {
            Ok(e) => Some((e,0x1000)),
            Err(crate::mem::mem_map::GetEntryErr::NotMapped) => None,
            Err(crate::mem::mem_map::GetEntryErr::ParentHugePage) => {
                let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size2MiB>::containing_address(VirtAddr::new(ptr.as_u64()));

                match crate::mem::mem_map::get_entry(page) {
                    Ok(e) => Some((e,0x200000)),
                    Err(crate::mem::mem_map::GetEntryErr::NotMapped) => None,
                    Err(crate::mem::mem_map::GetEntryErr::ParentHugePage) => {
                        let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size1GiB>::containing_address(VirtAddr::new(ptr.as_u64()));
                        // No parent huge pages are possible. This would've returned unmapped on the 4k check. no errors are possible here.
                        Some((crate::mem::mem_map::get_entry(page).unwrap(), 0x40000000))
                    }
                }
            }
        }
    }

    pub(crate) fn fixup(&self, addr: VirtAddr) -> Result<(),()> {
        let (pte,size) = Self::get_entry(addr).ok_or(())?;
        // Check that FAE bit is set.
        pte.flags().contains(FRAME_ATTR_ENTRY_FLAG).then(||()).ok_or(())?;

        let mut l = self.table.lock();
        let table = l.as_mut().unwrap();
        let index = self.select(pte.addr(), table);

        let fae = table[index].as_ref().ok_or(())?.clone();
        drop(l);
        let flags = fae.flags.load(Ordering::Relaxed);

        let map_to = |virt,dst,len, flags| {
            let _ = unsafe { crate::mem::mem_map::unmap_and_free(virt) }; // don't care if this fails
            match len {
                0x1000 => crate::mem::mem_map::map_frame_to_page(virt, x86_64::structures::paging::frame::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(dst), flags),
                0x200000 => crate::mem::mem_map::map_frame_to_page(virt,x86_64::structures::paging::frame::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(dst),flags),
                0x40000000 => crate::mem::mem_map::map_frame_to_page(virt,x86_64::structures::paging::frame::PhysFrame::<x86_64::structures::paging::Size4KiB>::containing_address(dst),flags),
                _ => panic!("Invalid page size")
            }
        };

        // If page is not present and fixups are enabled then copy the frame and update the entry
        if flags.contains(AttributeFlags::COPY_ON_FAULT) {
            let frame = crate::mem::allocator::COMBINED_ALLOCATOR.lock().phys_alloc().allocate(core::alloc::Layout::from_size_align(size,size).unwrap(),flags.get_mem_region()).expect("System ran out of memory");
            let dst = PhysAddr::new(frame as u64);
            // SAFETY: `ptr` is to be copied, `dst` is given by the memory allocator. `size` is given as the page size by the mapper.
            unsafe { Self::copy_frame(pte.addr(), dst, size) };
            let mut pt_flags = pte.flags();
            pt_flags.set(x86_64::structures::paging::PageTableFlags::WRITABLE,true);
            pt_flags.remove(FRAME_ATTR_ENTRY_FLAG);
            map_to(addr,dst,size,pt_flags).expect("Mapping failed");
            Ok(())
        } else {
            Err(())
        }
    }


    /// Copies from the physical address `origin` to `dst` for `len` bytes.
    /// `len` should be a valid page size.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `origin` and `dst` point to valid physical regions of `len` bytes.
    /// The caller must ensure that `dst` is not aliased.
    unsafe fn copy_frame(origin: PhysAddr, dst: PhysAddr, len: usize) {
        // SAFETY: Box is immutable, and memory is not expected to be present in memory.
        let mut b = alloc::vec::Vec::<u8,_>::new_in(unsafe { crate::alloc_interface::MmioAlloc::new_from_phys_addr(origin) });
        b.reserve(len);
        // SAFETY: This is safe because the size is set above.
        unsafe { b.set_len(len) }
        let mut dst = alloc::vec::Vec::new_in(unsafe { crate::alloc_interface::MmioAlloc::new_from_phys_addr(dst) });
        dst.reserve(len);
        // SAFETY: See above
        dst.set_len(len);
        dst.copy_from_slice(&b)
    }
}

pub enum FatOperation {
    /// Adds an alias the existing frame, if the frame has no associated FAE then one will be
    /// constructed using `attributes`. If a FAE already exists the attributes will set to the ones given.
    /// The caller must ensure that the new attribute configuration is compatible with the previous one.
    ///
    /// If the FAE already exists the alias count will be incremented if it doesn't it will be set to `2`.
    ///
    /// To enable COW set the [AttributeFlags::COPY_ON_WRITE] bit.
    ///
    /// This operation will return a pointer to the memory alias.
    NewAlias {
        attributes: AttributeFlags,

        /// When this is true any pages this operation is used on this must have its writable bit
        /// set to this value.
        ///
        /// This must be set to `true` to use as copy on write.
        initial_writable: bool,
    },

    /// Constructs a new FAE with the given attributes.
    NewFae {
        attributes: AttributeFlags,
        /// When this is set, if the requested frame has a FAE present it will panic. If this is
        /// clear then the FAE will be updated with the new flags.
        fallible: bool,

        /// The frame may not actually be mapped yet. This field is given to allow the caller to
        /// specify the number of times this frame is used. This field is ignored when this
        /// operation is used to update FAE flags.
        alias_count: usize,

        /// When this is true any pages this operation is used on this must have its writable bit
        /// set to this value.
        ///
        /// This must be set to `true` to use as copy on write.
        initial_writable: bool,
    },

    /// Increments the alias count by one.
    ///
    /// This operation will return a pointer to the memory alias
    ///
    /// # Panics
    ///
    /// This operation will panic if no FAE is present.
    Alias,

    /// Decrements the alias count. If the alias count is `0` and [AttributeFlags::NO_DROP] is
    /// clear then the FAE will be dropped. This is intended to use by the kernel, see [Safety](Self::UnAlias#Safety)
    ///
    /// # Panics
    ///
    /// This operation will panic if no FAE is present.
    ///
    /// # Safety
    ///
    /// Using this operation may cause a double free if the FA bit is set in the page table entry.
    /// The caller must ensure this bit is cleared.
    UnAlias,

    /// Attempts to claim the caller as sole owner of the target. Removing any FAEs that may be
    /// present for this region. This operation can only complete of the alias count is `<=1`
    /// otherwise this will fail.
    ///
    /// This operation is fallible, if a frame cannot be reclaimed then it will be skipped.
    Reclaim,
}

impl FatOperation {

    /// Predefined op to configure memory as Copy-On-Write
    // SAFETY: 6 is COPY_ON_FAULT | FIXUP_WRITABLE
    pub const INIT_COW: Self = Self::NewAlias { attributes: unsafe { AttributeFlags::from_bits_unchecked(6) }, initial_writable: false };

    /// Performs the requested operation for the given address.
    ///
    /// Returns whether the caller should attempt to remove the entry.
    fn operate(&self, fae: &mut Option<Arc<FrameAttributesInner>>, phys_addr: PhysAddr) -> bool {
        match self {
            FatOperation::NewAlias { attributes, .. } => {
                match fae.as_mut() {
                    Some(d) => {
                        d.flags.store(*attributes, Ordering::Relaxed);
                        d.alias_count.fetch_add(1, Ordering::Relaxed);
                    }
                    None => *fae = Some(Arc::new(FrameAttributesInner { alias_count: Atomic::new(2), flags: Atomic::new(*attributes) })),
                }
            }

            FatOperation::NewFae { attributes, fallible, alias_count, .. } => {
                match fae.as_mut() {
                    Some(d) if *fallible => d.flags.store(*attributes, Ordering::Relaxed),
                    Some(_) if !*fallible => panic!("New-Frame-Attribute-Entry operation failed entry for {phys_addr:?} is present operation is infallible"),
                    None => *fae = Some(Arc::new(FrameAttributesInner { alias_count: Atomic::new(*alias_count), flags: Atomic::new(*attributes) })),
                    // SAFETY: `if true | false` seems pretty exhaustive to me.
                    _ => unsafe { core::hint::unreachable_unchecked() }
                }
            }

            FatOperation::Alias => {
                fae.as_mut().expect("No FAE present for Alias operation").alias_count.fetch_add(1, Ordering::Relaxed);
            }
            FatOperation::UnAlias => {
                let fae_ref = fae.as_mut().expect("No FAE present for UnAlias operation");
                if !fae_ref.flags.load(Ordering::Relaxed).contains(AttributeFlags::NO_DROP) && fae_ref.alias_count.fetch_sub(1, Ordering::Relaxed) == 1 { // 1 - 1 == 0
                    FrameAttributes { inner: fae_ref.clone(), addr: phys_addr };
                    return true
                }
            }
            FatOperation::Reclaim =>  {
                if let Some(fae_ref) = fae {
                    if fae_ref.alias_count.load(Ordering::Relaxed) <= 1 {
                        fae.take();
                    }
                }
            }
        }
        false
    }

    /// Performs the op defined by `self` on the targeted FAE.
    ///
    /// # Safety
    ///
    /// See safety sections for [Self] variants.
    pub unsafe fn do_op(&self, fae: FrameAttributes ) {
        self.operate(&mut Some(fae.inner),fae.addr);
    }

    /// Does this operation require returning a pointer.
    fn req_virt(&self) -> bool {
        match self {
            FatOperation::NewAlias { .. } | FatOperation::Alias => true,
            _ => false
        }
    }

    /// Indicates the updated value of the initial page-entry writable bit.
    ///
    /// # Panics
    ///
    /// This will panic if called when `self` is no [Self::NewAlias] or [Self::NewFae]
    fn initial_virt_writable(&self) -> bool {
        match self {
            FatOperation::NewAlias { initial_writable, ..} | FatOperation::NewFae { initial_writable, .. } => {
                *initial_writable
            }
            _ => panic!(),
        }
    }
}

struct FrameAttributesInner {
    /// Number of times this frame is mapped into virtual memory
    alias_count: Atomic<usize>, // It's theoretically possible to try to use a frame more than `usize::MAX` times
    flags: Atomic<AttributeFlags>,
}

bitflags::bitflags! {

    pub struct AttributeFlags: u32 {
        /// Indicates that this entry shouldn't be dropped when the alias count is >= 1.
        /// When this flag is set this frame will never be freed. It is the responsibility of the
        /// caller to free this frame.
        ///
        /// # Panics
        ///
        /// If the owner of this frame does not explicitly remove the FAE then this *may* cause
        /// panics in the future.
        ///
        /// Users should consider this state UB although this does not
        /// explicitly break rusts safety rules.
        const NO_DROP = 1;

        /// Indicates that this frame will be copied and fixed up if a page fault occurs.
        /// The fixed up frame will always be writable.
        ///
        /// If this bit is clear then this FAE is used for explicit operations and not implicit fixup operations.
        const COPY_ON_FAULT = 1 << 1;

        // Last 4 bits used to select DMA region

        /// Forces allocator to select [crate::mem::MemRegion::Mem16]
        const DMA_USE_MEM16 = 1 << 28;
        /// Forces allocator to select [crate::mem::MemRegion::Mem32]
        const DMA_USE_MEM32 = 2 << 28;
        /// Forces allocator to select [crate::mem::MemRegion::Mem64]
        const DMA_USE_MEM64 = 3 << 28;
    }
}

impl AttributeFlags {

    /// Fetches the [crate::mem::MemRegion] indicated by `self`.
    ///
    /// If no region is specified then the default variant will be used.
    fn get_mem_region(&self) -> crate::mem::MemRegion {
        if self.contains(Self::DMA_USE_MEM16) {
            crate::mem::MemRegion::Mem16
        } else if self.contains(Self::DMA_USE_MEM32) {
            crate::mem::MemRegion::Mem32
        } else if self.contains(Self::DMA_USE_MEM64) {
            crate::mem::MemRegion::Mem64
        } else {
            Default::default()
        }
    }
}

pub struct FrameAttributes {
    inner: Arc<FrameAttributesInner>,
    addr: PhysAddr,
}

impl FrameAttributes {

    /// Removes the frame attribute entry from the frame attribute table.
    ///
    /// # Safety
    ///
    /// This is unsafe because the caller must ensure this frame is not aliased or has attributes
    /// that may modify behaviour.
    pub unsafe fn clear(self) {
        ATTRIBUTE_TABLE_HEAD.rm_page_entry(self.addr);
    }

    /// Indicates the number of pages this frame is used by.
    /// This includes all contexts, not just the current one.
    pub fn alias_count(&self) -> usize {
        self.inner.alias_count.load(Ordering::Relaxed)
    }

    fn alias(&self) {
        self.inner.alias_count.fetch_add(1,Ordering::Relaxed);
    }

    fn de_alias(&self) {
        self.inner.alias_count.fetch_sub(1,Ordering::Relaxed);
    }

    /// Sets the flags for the physical frame.
    ///
    /// # Safety
    ///
    /// The caller must ensure that all page-aliases expect and handle the new behaviour correctly.
    pub unsafe fn set_flags(&self, flags: AttributeFlags) {
        self.inner.flags.store(flags,Ordering::Relaxed);
    }

    pub fn get_flags(&self) -> AttributeFlags {
        self.inner.flags.load(Ordering::Relaxed)
    }
}