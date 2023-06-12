pub mod frame_information_structure;

use crate::register::*;
use ata::command::constructor::MaybeOpaqueCommand;
use core::alloc::Allocator;
use core::intrinsics::forget;
use core::ops::{Deref, Index, IndexMut};
use frame_information_structure as fis;

const PAGE_SIZE: usize = hootux::mem::PAGE_SIZE;

const _ASSERT: () = {
    let send_fis_size = 64;
    let actual_size = core::mem::size_of::<SendFis>();
    assert!(
        actual_size == send_fis_size,
        "Error: Incorrect size for SendFis.",
    );
};

#[repr(C)]
pub(crate) struct CommandList {
    list: [CommandHeader; 32],
}

#[repr(C)]
pub struct CommandHeader {
    cfg: crate::hba::SyncCap,
    description_info: Register<DescriptionInformation>,
    physical_region_table_len: Register<u16>,
    /// This field contains the physical address of the command table for this entry
    command_table_base_addr: Register<crate::hba::port_control::RegisterAddress<256>>,
    _res: [u32; 4],
}

impl CommandHeader {
    /// Returns a box containing
    pub fn get_com_table(&mut self, cfg: crate::hba::SyncCap) -> CommandTable {
        let region = cfg.get_region();
        let addr = self.command_table_base_addr.read().read_qword();
        // SAFETY: this address is retrieved from the command header and must contain valid data
        let alloc = unsafe { hootux::allocator::alloc_interface::MmioAlloc::new(addr as usize) };

        // SAFETY: this uses literal values which are safe
        let (layout, _) = unsafe {
            core::alloc::Layout::from_size_align_unchecked(0x80, 256)
                .extend(
                    core::alloc::Layout::array::<PhysicalRegionDescription>(
                        self.physical_region_table_len.read() as usize,
                    )
                    .unwrap(),
                )
                .unwrap() // something is seriously wrong if this panics
        };
        let ptr = alloc.allocate(layout).unwrap(); // should never return None

        // this *should* always be valid but it is allowed to be invalid as long as its not read from
        let ptr = core::ptr::slice_from_raw_parts_mut(
            ptr.as_ptr().cast::<u8>(),
            self.physical_region_table_len.read() as usize,
        ) as *mut CommandTableRaw;

        //
        let raw = unsafe { alloc::boxed::Box::from_raw_in(ptr, alloc) };
        let table_len = (if raw.len() == 0x1_0000 { 0 } else { raw.len() }) as u16;

        CommandTable {
            table_size: table_len,
            region,
            parent: self,
            table: raw,
        }
    }
}

bitflags::bitflags! {
    /// DW 0 (RW)
    struct DescriptionInformation: u16 {
        /// C
        ///
        /// When set the HBA shall clear the busy flag in the task file register and the PxCI bit
        /// corresponding to this command slot after the FIS is sent and R_OK is received.
        const CLEAR_BUSY_ON_OK = 1 << 10;
        /// B
        ///
        /// Indicates that the command is for sending a BIST FIS. The HBA shall send the FIS and
        /// enter test mode.
        const BIST = 1 << 9;
        /// R
        ///
        /// Indicates that the command is for a part of a software reset sequence tha manipulates
        /// the SRST bit in the device control register. The HBA must perform a SYNC escape if
        /// necessary to get the device into an idle state before sending the command.
        const RESET = 1 << 8;
        /// P
        ///
        /// only valid when PRDTL is Non-Zero or [Self::ATAPI] is set.
        ///
        /// When PRDTL is non-zero the HBA may prefetch PRDs in anticipation odf a data transfer.
        ///
        /// When [Self::ATAPI] is set the HBA may prefetch the ATAPI command.
        ///
        /// This may not be set when Native command queueing or FIS-based switching is being used.
        // only for FIS-cringe switching
        const PREFETCHABLE = 1 << 7;
        /// W
        ///
        /// Indicates the the directionis a wdevice write. When cleared This is a read. When this bit
        /// and [Self::PREFETCHABLE] is set the HBA may prefetch data.
        const WRITE = 1 << 6;
        /// A
        ///
        /// Indicates that a PIO setup FIS shall be sent by the device indicating a transfer for the
        /// ATAPI command.
        const ATAPI = 1 << 5;
    }
}

impl DescriptionInformation {
    /// PMP
    ///
    /// Sets the port number to use when constructing FISes. Received FISes against this command are
    /// checked against this value.
    ///
    /// # Panics
    ///
    /// This fn will panic if `port` is greater than 15;
    pub fn set_pm_port(&mut self, port: u8) {
        assert!(port < 16);
        let mut t = self.bits();
        t &= !(0xf << 12);
        t |= (port as u16) << 12;
        // this should never panic
        *self = Self::from_bits_retain(t);
    }

    /// Returns the port multiplier port targeted by this command header.
    pub fn get_mp_port(&self) -> u8 {
        let mut t = self.bits();
        t >>= 12;
        t &= 0xf;
        t as u8
    }

    /// CFL
    ///
    /// Gets the length of the FIS in dwords.
    pub fn get_command_fis_len(&self) -> u8 {
        (self.bits() & 0xf) as u8
    }

    /// Sets the FIS length in dwords.
    ///
    /// # Panics
    ///
    /// This fn will panic if `len` is 1 or 0 or greater than 15
    pub fn set_command_fis_len(&mut self, len: u8) {
        assert!(len < 16);
        assert!(len > 1);
        let mut t = self.bits();
        t &= !0xf;
        t |= len as u16;
        *self = Self::from_bits_retain(t)
    }
}

/// This struct is a container for a command table. This struct is to enable control of the command
/// table such as relocating and resizing it. The actual size of the table is cached to prevent unnecessary shrinking of the table
///
/// The PRDT has a minimum len of 1 and a maximum of 65536. A table len of `0` will be interpreted in hardware as 65536.
///
/// The command table uses [hootux::allocator::alloc_interface::MmioAlloc] to prevent the struct
/// from being dropped in physical memory. However allocations must be performed using
/// [hootux::allocator::alloc_interface::DmaAlloc] because the physical memory structure cannot be fragmented.
// todo should i even bother making the command header public and use this instead?
pub(crate) struct CommandTable<'a> {
    table_size: u16,
    // this is immutable and here to simplify method signatures
    region: hootux::mem::buddy_frame_alloc::MemRegion,
    parent: &'a mut CommandHeader,
    table: alloc::boxed::Box<CommandTableRaw<'a>, hootux::allocator::alloc_interface::MmioAlloc>,
}

impl CommandTable {
    /// Imports the new command table updating pointers and sizes.
    fn import_table(&mut self, table: BoxedCommandTable, phys_addr: u64) {
        // 1 is min size, this prevents the table from being accessed accidentally.
        self.parent.physical_region_table_len.write(1);

        let mut a = self.parent.command_table_base_addr.read();
        if self.region == hootux::mem::buddy_frame_alloc::MemRegion::Mem64 {
            unsafe { a.qword.set_addr(phys_addr) }
        } else {
            unsafe { a.dword.set_addr(phys_addr as u32) }
        }
        self.parent.command_table_base_addr.write(a);

        self.table = table;
        let len = if table.len() == 0x1_0000 {
            0
        } else {
            table.len()
        };
        self.parent.physical_region_table_len.write(len as u16);
    }

    /// Builds the PRDT for self updating the size if required.
    ///
    /// # Safety
    ///
    /// This fn is unsafe because the caller must ensure the len of `region` is aligned to the
    /// devices LBA size.
    pub(crate) unsafe fn build_prdt<T>(&mut self, region: &[u8]) -> Result<(), CommandError> {
        // increment: this is a separate definition because I may find a reason to change the increments later
        const INC: usize = PAGE_SIZE;
        if arr % 2 != 0 {
            return Err(CommandError::BadAddress);
        }
        let total_incs = region.len().div_ceil(INC);
        // OverFlow: for entries that need to be cached before a new CommandTableRaw is allocated.
        let mut of = alloc::vec::Vec::new();

        /* calculate last segment size.
           the last segment may be smaller than the increment size, and using the increment size
           for the last segment may overrun into uninitialized tata.
           the increment size may also not be a power of two
        */
        let ls = INC % region.len();

        let mut index = 0;
        let mut found: Option<u64> = None;
        let mut incs = 0;

        // flushes the region to the prdt or overflow cache. returns whether the prdt is full
        let flush = |start, count| -> bool {
            let table = PhysicalRegionDescription::new(start, count)
                .expect("Failed to build PRDT: Invalid region layout");
            if let Some(table) = self.table.write_to_index(index, table) {
                of.push(table);
            }

            index += 1;
            // if the index is max then the table has 64k entries this is `0` in the header
            // this does not actually allow the last entry to be used.
            if index == u16::MAX {
                true
            } else {
                false
            }
        };

        for i in 0..total_incs {
            // check that the offset does not exceed region, should not be needed for release versions
            debug_assert!(i * INC <= region.len());
            let ptr = region.as_ptr().offset((i * INC) as isize);

            // this is panic worthy because it means UB has been detected.
            let addr = hootux::mem::mem_map::translate(ptr as usize).expect(&*alloc::format!(
                "Failed to build PRDT: Page not mapped for address {}",
                ptr as usize
            ));

            let addr = if self.region == hootux::mem::buddy_frame_alloc::MemRegion::Mem32 {
                let addr: u32 = addr.try_into().ok().ok_or(CommandError::BadAddress)?;

                addr as u64
            } else {
                addr
            };

            if let Some(n) = found {
                if incs * INC + 1 > 0x40_000 {
                    // region is nearly at max
                    if flush(found.unwrap(), (incs * INC) as u32) {
                        break;
                    }
                    found = Some(addr);
                    count = 1;
                } else if n == addr - (INC * incs) {
                    // page is contiguous
                    incs + 1;
                } else {
                    // page is not contiguous
                    // flush last
                    if flush(found.unwrap(), (incs * INC) as u32) {
                        break;
                    }
                    found = Some(addr);
                    incs = 1;
                }
            } else {
                found = Some(addr);
                incs = 1;
            }
        }

        // creates new command table and populates it with the old prdt and overflow cache
        if of.len() != 0 {
            let (t, new_addr) = CommandTableRaw::new(index, self.region);
            for i in 0..self.table.len() {
                t[i] = self.table[i];
            }
            for i in self.table.len()..index {
                t[i] = of[i]
            }
            self.import_table(t, new_addr);
        }

        Ok(())
    }

    /// Sets the len of the PRDT within the command header.
    /// if `len` is greater than `self.table_size` this will cause the
    /// table to be reallocated.
    pub(crate) fn set_len(&mut self, len: u16) {
        if self.table_size < len {
            self.resize(len);
            self.table_size = len;
        } else {
            self.parent.physical_region_table_len.write(len);
        }
    }

    // todo should this be dyn?
    fn issue_command(&mut self, command: frame_information_structure::RegisterHostToDevFis) {
        let n_fis = command.into();
        self.table.command_fis.send_cmd(&n_fis);
    }
}

enum CommandError {
    /// Attempted to use a 64bit address on a device does not support 64bit addressing.
    BadAddress,
    /// Given buffer is too long, contained value contains the index of the first unused byte in the buffer.
    /// This indicates a partial success, but not all data has been processed.
    /// Functions that return this should detail how to handle this result
    BuffTooLong(usize),
    /// A buffer likely has some requirements to it to be usable, any functions that may return
    /// this should define a usable buffer.
    BadBuffer,
}

/// This struct is the Command Table defined in the AHCI specification.
///
/// The command table is a DST, it's size is defined by the relevant [CommandHeader].
#[repr(C, align(256))]
struct CommandTableRaw<'a> {
    _pin: core::marker::PhantomData<&'a mut CommandHeader>,
    /// This contains the frame information structure that will be sent to the device.
    // todo i think this is just RegD2H
    command_fis: CommandBuff,
    /// This is reserved for ATAPI commands
    _atapi_cmd: [u8; 16],
    _res: [u8; 0x30],
    /// This table contains the Physical Region Descriptor Table
    prdt: [PhysicalRegionDescription],
}

type BoxedCommandTable<'a> =
    alloc::boxed::Box<CommandTableRaw<'a>, hootux::allocator::alloc_interface::MmioAlloc>;
impl CommandTableRaw {
    /// Creates a new instance of self.
    /// If `len`. If `len == 0` a length of 65536 will be used.
    /// This fn will return the physical address of the returned `Self`
    fn new(
        len: u16,
        region: hootux::mem::buddy_frame_alloc::MemRegion,
    ) -> (BoxedCommandTable, u64) {
        use core::alloc::Layout;
        use hootux::allocator::alloc_interface;

        let len = if len == 0 { 65536 } else { len.into() };
        let alloc = alloc_interface::DmaAlloc::new(region, core::mem::align_of::<Self>());
        // This should never panic (i think)
        let layout = Layout::new::<Self>()
            .extend(Layout::array::<PhysicalRegionDescription>(len).unwrap())
            .unwrap()
            .0;

        let ptr = alloc
            .allocate(layout)
            .expect("System ran out of memory")
            .as_ptr()
            .cast::<u8>();
        // SAFETY: The region can hold
        let table = unsafe { core::slice::from_raw_parts_mut(ptr, len) as &mut Self };

        let addr = hootux::mem::mem_map::translate(table as *mut _ as usize).expect("Found a bug");
        // SAFETY: This is safe because the region was allocated from DmaAlloc.
        let alloc = unsafe { alloc_interface::MmioAlloc::new(addr as usize) };
        // SAFETY: This region was allocated above and the pointer is never re-used
        unsafe { (alloc::boxed::Box::from_raw_in(table, alloc), addr) }
    }

    /// Writes `region` into the given index, returns `Some(region)` if the entry does not exist
    fn write_to_index(
        &mut self,
        index: u16,
        region: PhysicalRegionDescription,
    ) -> Option<PhysicalRegionDescription> {
        if self.prdt.len() < index as usize {
            Some(region)
        } else {
            self[index] = region
        }
    }

    /// Returns the length of the PRDT
    fn len(&self) -> usize {
        self.prdt.len()
    }

    /// Returns whether the size of the PRDT should be 0 in the command header.
    fn is_size_0(&self) -> bool {
        self.len() == 0x1_0000
    }
}

impl Index<usize> for CommandTableRaw {
    type Output = PhysicalRegionDescription;

    fn index(&self, index: PhysicalRegionDescription) -> &Self::Output {
        &self.prdt[index as usize]
    }
}

impl IndexMut<usize> for CommandTableRaw {
    fn index_mut(&mut self, index: u16) -> &mut Self::Output {
        &mut self.command_fis[index as usize]
    }
}

struct CommandBuff {
    buff: [u8; 64],
}

impl CommandBuff {
    fn send_cmd(&mut self, fis: &frame_information_structure::RegisterHostToDevFis) {
        let b = unsafe {
            &mut *(&mut self.buff as *mut _
                as *mut frame_information_structure::RegisterHostToDevFis)
        };
        b.fis_type = fis.fis_type;
        b.cfg = {
            // a command will be sent if the command bit is toggled so it must be preserved
            let mut cfg = fis.cfg.clone();
            cfg.set_cmd_bit(b.cfg.get_cmd_bit());
            cfg
        };
        b.features_low = fis.features_low;
        b.lba_low = fis.lba_low;
        b.aux = fis.aux;
        b.dev = fis.dev;
        b.lba_high = fis.lba_high;
        b.features_high = fis.features_high;
        b.count = fis.count;
        b.icc = fis.icc;
        b.control = fis.control;
        b.aux = fis.aux;

        // writing to command send the FIS so it must be done last
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        b.command = fis.command;

        // prevents writes to b being optimized out
        core::hint::black_box(b);
    }

    fn reissue_cmd(&mut self) {
        self.buff[1] ^= 1 << 7;
        core::hint::black_box(self);
    }
}

union SendFis {
    /// byte 0 for all structs in this union is a [fis::FisType] and can be used to identify the actual type this should be dereference as
    id: fis::FisType,
    reg_h2d: core::mem::ManuallyDrop<fis::RegisterHostToDevFis>,
    dma_setup: core::mem::ManuallyDrop<fis::DmaSetupFis>,
    self_test: core::mem::ManuallyDrop<fis::BistActivateFis>,
    vendor_spec: [u8; 64],
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
struct PhysicalRegionDescription {
    base_addr: u64,
    _res: u32,
    data_count: PhysicalDataCount,
}

impl PhysicalRegionDescription {
    /// Attempts to create a new Self using the
    fn new(start: u64, len: u32) -> Option<Self> {
        if len > 0x40_000 {
            return None;
        } else {
            Some(Self {
                base_addr: start,
                _res: 0,
                data_count: PhysicalDataCount::new(count)?,
            })
        }
    }
}

#[repr(transparent)]
struct PhysicalDataCount {
    inner: u32,
}

impl PhysicalDataCount {
    /// Creates a new instance of Self. `count` must be less than or equal to `0x40000` and must be
    /// an even number.
    fn new(count: u32) -> Option<Self> {
        if len > 0x40_000 || (len & 1 != 0) {
            return None;
        } else {
            let mut t = Self { inner: 0 };
            t.set_data_count(count);
            Some(t)
        }
    }
    fn set_interrupt(&mut self, value: bool) {
        if value {
            self.inner |= 1 << 31;
        } else {
            self.inner &= !(1 << 31);
        }
    }

    /// Sets the length of the physical region to `count`. The maximum length is 4MiB
    ///
    /// The HBA adds one to the actual value in this field and so is subtracted by one before writing.
    ///
    /// # Panics
    ///
    /// This fn will panic if `count > 4MiB` or if count is `0`
    fn set_data_count(&mut self, mut count: u32) {
        assert!(count < (1 << 22));
        assert_ne!(count, 0);

        count -= 1;
        self.inner &= (1 << 22) - 1;

        self.inner |= count;
    }
}

/// This struct contains the last received FISes from the device.
#[repr(C)]
struct ReceivedFisTable {
    dma_setup: Register<fis::DmaSetupFis, ReadOnly>,
    _res0: core::mem::MaybeUninit<[u8; 4]>,
    pio_setup: Register<fis::PioSetupFis, ReadOnly>,
    _res1: core::mem::MaybeUninit<[u8; 12]>,
    register: Register<fis::RegisterDevToHostFis, ReadOnly>,
    _res2: core::mem::MaybeUninit<[u8; 4]>,
    se_bits: Register<fis::SetDevBitsFis, ReadOnly>,
    unknown_fis: Register<[u8; 64], ReadOnly>,
    // this struct is 256 bytes long
    _res3: core::mem::MaybeUninit<[u8; 96]>,
}
