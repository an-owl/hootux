use alloc::boxed::Box;
use core::mem::MaybeUninit;
use core::ops::{Index, IndexMut};
use x86_64::instructions::tlb::flush_all;
use x86_64::structures::paging::{
    Mapper, Page, PageTable, PageTableFlags, PageTableIndex, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};
use PageTableLevel::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PageTableLevel {
    L1,
    L2,
    L3,
    L4,
}

/// Stores virtual addresses of Page Tables for reference
///
/// Addresses contained may be Uninitialized check
/// PageTableFlags::Present flag on relevant page table.
/// Unlike a PageTable this does not contain flags.
///
/// All entries are uninitialized until mapped
#[repr(align(4096))]
struct VirtualPageTable {
    tables: [MaybeUninit<Box<PageTableBranch>>; 512],
}

impl VirtualPageTable {
    /// Initializes a a nwe instance of VirtualPageTable
    fn new() -> Self {
        Self {
            tables: [const { MaybeUninit::uninit() }; 512], //Copy is not for Box so this is used instead
        }
    }

    fn set(&mut self, index: PageTableIndex, addr: Box<PageTableBranch>) {
        let index = usize::from(index);
        self.tables[index].write(addr);
    }
}

impl Index<PageTableIndex> for VirtualPageTable {
    type Output = MaybeUninit<Box<PageTableBranch>>;

    /// Returns an &MaybeUninit<PageTableBranch>
    /// that be initialized later

    fn index(&self, index: PageTableIndex) -> &Self::Output {
        let t = &self.tables[usize::from(index)];
        t
    }
}

impl IndexMut<PageTableIndex> for VirtualPageTable {
    fn index_mut(&mut self, index: PageTableIndex) -> &mut Self::Output {
        let t = &mut self.tables[usize::from(index)];
        t
    }
}

/// Contains PageTable metadata and virtual addresses of mapped pages.
/// The contained PageTable and its mapped virtual addresses are
/// stored within Box's.
///
/// PageTableBranches with `level: PageTableLevel::L1` does not
/// contain its mapped virtual addresses
// editors note HUGE pages do not require a mapped virtual address
// however they are already there taking space so use them?
pub struct PageTableBranch {
    level: PageTableLevel,
    page: Box<PageTable>,
    virt_table: Option<Box<VirtualPageTable>>, // parent_entry: Option<PageTableIndex>?
                                               // todo fast_drop: bool,
                                               // fast drop will be set on children when the
                                               // parent is dropped to skip unbinding steps
}

impl PageTableBranch {
    /// Creates a new PageTableBranch for level
    pub fn new(level: PageTableLevel) -> Self {
        match level {
            L1 => Self {
                level,
                page: Box::new(PageTable::new()),
                virt_table: None,
            },
            _ => Self {
                level,
                page: Box::new(PageTable::new()),
                virt_table: Some(Box::new(VirtualPageTable::new())),
            },
        }
    }

    pub fn level(&self) -> PageTableLevel {
        self.level
    }

    /// finds the first free index in self
    // TODO make one that searches within range
    pub fn find_free(&self) -> Option<PageTableIndex> {
        for (i, entry) in self.page.iter().enumerate() {
            if entry.is_unused() {
                return Some(PageTableIndex::new(i as u16));
            }
        }
        None
    }

    /// Creates a child and binds it to index
    ///
    /// This function will panic if called on a PageTableBranch marked L1
    pub fn child(
        &mut self,
        index: PageTableIndex,
        mapper: &impl Mapper<Size4KiB>,
    ) -> &mut PageTableBranch {
        let level = match self.level {
            L1 => panic!("Tried to spawn child from L1 page table"),
            L2 => L1,
            L3 => L2,
            L4 => L3,
        };

        // im pretty sure maybe uninit is needed here
        let new_child = Box::new(Self::new(level));

        // assign to virt_table
        self.virt_table.as_mut().unwrap().set(index, new_child);
        flush_all();

        // get PhysAddr of child and map to self
        // child is now stored in self.virt_table
        let phys = unsafe { &*self.virt_table.as_mut().as_ref().unwrap()[index].as_ptr() }
            .page_phy_addr(mapper);

        self.page[index].set_addr(
            phys,
            PageTableFlags::PRESENT | PageTableFlags::NO_EXECUTE | PageTableFlags::WRITABLE,
        );

        unsafe { &mut *self.virt_table.as_mut().unwrap()[index].as_mut_ptr() }
    }

    /// Returns a reference to the contained PageTable
    pub fn get_page(&self) -> &PageTable {
        &*self.page
    }

    /// Returns the given page table as mutable
    pub fn get_page_mut(&mut self) -> &mut PageTable {
        &mut self.page
    }

    /// address of &self.page
    pub fn page_phy_addr(&self, mapper: &impl Mapper<Size4KiB>) -> PhysAddr {
        let virt_addr = VirtAddr::from_ptr(&*self.page);
        assert!(
            virt_addr.is_aligned(4096u64),
            "PageTable at {:#?} misaligned",
            virt_addr
        );

        mapper
            .translate_page(Page::containing_address(virt_addr))
            .unwrap()
            .start_address()
    }

    /// preforms a recursive [Prolicide](https://en.wikipedia.org/wiki/List_of_types_of_killing#Killing_of_family)
    /// on the child at `index`
    ///
    /// This function can be used to deallocate pages from L1 page tables
    ///
    /// This function is potentially expensive to call as it
    /// must drop all grandchildren
    pub fn drop_child(&mut self, index: PageTableIndex) {
        let curr_flags = self.page[index].flags();
        self.page[index].set_flags(curr_flags & !PageTableFlags::PRESENT);

        // drops given child if virt_table exists
        // virt table exists opn all but L1 page tables
        if let Some(vpt) = self.virt_table.as_mut() {
            unsafe { vpt[index].assume_init_drop() };
        }
    }

    /// Allocates a physical frame to the given page table index
    ///
    /// This function will panic if self.level is NOT L1
    pub fn allocate_frame(&mut self, index: PageTableIndex, addr: PhysAddr) {
        assert_eq!(self.level, L1);

        self.page[index].set_addr(
            addr,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
        )
    }

    /// Returns a mutable refrence to the child at `index`
    pub fn get_child_mut(&mut self, index: PageTableIndex) -> Option<&mut Self> {
        if let Some(vpt) = &mut self.virt_table {
            if self.page[index].is_unused() {
                return None;
            }
            let r = unsafe { &mut **vpt[index].as_mut_ptr() };
            return Some(r);
        } else {
            None
        }
    }
}

impl Drop for PageTableBranch {
    /// dropping should only be done form the parent
    /// because `drop()` cannot unmap itself from its parent
    fn drop(&mut self) {
        if self.level == L4 {
            // just in case
            // though
            panic!("tried to drop l4 page table")
        }
        let mut flags: [PageTableFlags; 512] = [PageTableFlags::empty(); 512];
        for (i, e) in self.page.iter().enumerate() {
            flags[i] = e.flags();
        }

        for (i, e) in flags.iter().enumerate() {
            if !e.is_empty() {
                self.drop_child(PageTableIndex::new(i as u16));
            }
        }
    }
}
