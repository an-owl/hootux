use alloc::sync::Arc;
use core::sync::atomic::Ordering;
use atomic::Atomic;
use x86_64::{PhysAddr, VirtAddr};

const BITS48_TABLE_ADDRESS: usize = 0xFFFFFF0000000000;
const BITS48_TABLE_SIZE: usize = 0x1000000000;

const BITS57_TABLE_ADDRESS: usize = 0xFFFE000000000000;
const BITS57_TABLE_SIZE: usize = 0x200000000000;

static ATTRIBUTE_TABLE_HEAD: FrameAttributeTable = FrameAttributeTable::new();

struct FrameAttributeTable {
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

    fn lookup<T>(&self, frame: PhysAddr) -> Option<FrameAttributes> {
        let mut l = self.table.lock() ;
        let table = match *l {
            None => {
                crate::mem::virt_fixup::set_fixup_region(BITS48_TABLE_ADDRESS as *const u8,BITS48_TABLE_SIZE * 8,0x1000,true).unwrap();
                // We are using fixups for this region, it will be made valid on being accessed.
                *l = Some( unsafe { core::slice::from_raw_parts_mut(BITS48_TABLE_ADDRESS as *mut _, BITS48_TABLE_SIZE) });
                l.as_ref().unwrap()
            }
            Some(ref t) => {
                t
            }
        };

        let index = self.select(frame,table);
        Some(FrameAttributes {inner: table[index].clone()?, addr: frame})
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
        let mut table = l.as_mut().unwrap();

        let i = self.select(frame, table);

        let _ = table[i].take();

        let align = i & !(512-1); // 512 is number of entries per page
        // Will this compile to a `repnz scasq`?
        if table[align..align+512].iter().all(|e| e.is_none()) {
            let addr = VirtAddr::from_ptr(&table[align]);

            // SAFETY: This struct handles TLB synchronization internally
            crate::mem::tlb::without_shootdowns( ||
                // SAFETY: This page is checked above for data, this only occurs if not useful data is present in the page.
                crate::mem::mem_map::unmap_page(x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(addr))
            )
        }
    }
}

// packed(8) prevents split-locks
#[repr(packed(8))]
struct FrameAttributesInner {
    alias_count: Atomic<usize>, // It's theoretically possible to try to use a frame more than `usize::MAX` times
    flags: Atomic<AttributeFlags>,
}

bitflags::bitflags! {
    struct AttributeFlags: u32 {
        const NO_DROP = 1; // Indicates that this entry shouldn't be dropped when the alias count is >= 1;
    }
}

struct FrameAttributes {
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


