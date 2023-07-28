pub mod frame_information_structure;

use crate::register::*;
use core::alloc::Allocator;
use core::ops::{Index, IndexMut};
use frame_information_structure as fis;

#[repr(C)]
pub struct CommandHeader {
    description_info: Register<DescriptionInformation>,
    pub(crate) physical_region_table_len: Register<u16>,
    /// This field contains the physical address of the command table for this entry
    pub(crate) command_table_base_addr: HbaAddr<256>,
    _res: [u32; 4],
}

unsafe impl ClearReserved for u16 {
    #[inline]
    fn clear_reserved(&mut self) {}
}

impl CommandHeader {
    /// Assigns a new [CommandTableRaw] to self.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the `addr` contains the physical address of a valid command
    /// table, which has at least `len` PRDT entries.
    pub(crate) unsafe fn set_table(&mut self, addr: u64, len: u16) {
        // believe it or not this is the most effective way to do this operation.
        // this prevents accidental operations if the new table is shorter than the old one.
        self.physical_region_table_len.write(1);

        self.command_table_base_addr.set(addr);
        self.physical_region_table_len.write(len);
    }

    /// Sets the fis len. Len may only be a select few values, see below for details.
    ///
    /// # Panics
    ///
    /// This fn will panic if `len < 2 || len > 16
    pub(crate) fn set_fis_len(&mut self, len: u8) {
        assert!(len > 1, "Illegal FIS Size: {} ", len);
        assert!(len <= 16, "Illegal FIS Size: {}", len);
        self.description_info.update(|d| d.set_command_fis_len(len))
    }

    pub(crate) fn set_prefetch(&mut self, value: bool) {
        self.description_info
            .update(|d| d.set(DescriptionInformation::PREFETCHABLE, value))
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
        /// When PRDTL is non-zero the HBA may prefetch PRDs in anticipation of a data transfer.
        ///
        /// When [Self::ATAPI] is set the HBA may prefetch the ATAPI command.
        ///
        /// This may not be set when Native command queueing or FIS-based switching is being used.
        // only for FIS-cringe switching
        const PREFETCHABLE = 1 << 7;
        /// W
        ///
        /// Indicates the the HBA will send fis data to the device. When cleared this is a read. When this bit
        /// and [Self::PREFETCHABLE] is set the HBA may prefetch data.
        const WRITE = 1 << 6;
        /// A
        ///
        /// Indicates that a PIO setup FIS shall be sent by the device indicating a transfer for the
        /// ATAPI command.
        const ATAPI = 1 << 5;
    }
}

unsafe impl ClearReserved for DescriptionInformation {
    fn clear_reserved(&mut self) {
        let t = self.bits();
        *self = Self::from_bits_truncate(t);
    }
}

impl DescriptionInformation {
    #![allow(dead_code)]
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

/// This struct is the Command Table defined in the AHCI specification.
///
/// The command table is a DST, it's size is defined by the relevant [CommandHeader].
#[repr(C, align(256))]
pub(crate) struct CommandTableRaw {
    /// This contains the frame information structure that will be sent to the device.
    // todo i think this is just RegD2H
    pub(crate) command_fis: CommandBuff,
    /// This is reserved for ATAPI commands
    _atapi_cmd: [u8; 16],
    _res: [u8; 0x30],
    /// This table contains the Physical Region Descriptor Table
    prdt: [PhysicalRegionDescription],
}

type BoxedCommandTable =
    alloc::boxed::Box<CommandTableRaw, hootux::allocator::alloc_interface::MmioAlloc>;

impl CommandTableRaw {
    /// Creates a new instance of self.
    /// If `len`. If `len == 0` a length of 65536 will be used.
    /// This fn will return the physical address of the returned `Self`
    pub(crate) fn new(
        len: u16,
        region: hootux::mem::buddy_frame_alloc::MemRegion,
    ) -> (BoxedCommandTable, u64) {
        use core::alloc::Layout;
        use hootux::allocator::alloc_interface;

        let len = len.into();
        let alloc = alloc_interface::DmaAlloc::new(region, 256);
        // This should never panic (i think)
        let layout = Layout::from_size_align(32, 256)
            .unwrap()
            .extend(Layout::array::<PhysicalRegionDescription>(len).unwrap())
            .unwrap()
            .0;

        let ptr = alloc
            .allocate(layout)
            .expect("System ran out of memory")
            .as_ptr();
        // SAFETY: ptr is allocated and unused
        unsafe {
            (*ptr).fill(0);
        }
        let ptr = ptr.cast::<u8>();

        // SAFETY: The allocated region can hold table, the calculation is done to construct `layout`
        let table: *mut Self =
            unsafe { core::mem::transmute(core::slice::from_raw_parts_mut(ptr, len)) };

        let addr = hootux::mem::mem_map::translate(table as *mut u8 as usize).expect("Found a bug");
        // SAFETY: This is safe because the region was allocated from DmaAlloc.
        let alloc = unsafe { alloc_interface::MmioAlloc::new(addr as usize) };
        // SAFETY: This region was allocated above and the pointer is never re-used
        let n = unsafe { (alloc::boxed::Box::from_raw_in(table, alloc), addr) };
        n
    }

    /// Returns the length of the PRDT
    pub(crate) fn len(&self) -> usize {
        self.prdt.len()
    }
}

impl Index<u16> for CommandTableRaw {
    type Output = PhysicalRegionDescription;

    fn index(&self, index: u16) -> &Self::Output {
        &self.prdt[index as usize]
    }
}

impl IndexMut<u16> for CommandTableRaw {
    fn index_mut(&mut self, index: u16) -> &mut Self::Output {
        &mut self.prdt[index as usize]
    }
}

pub(crate) struct CommandBuff {
    buff: [u8; 64],
}

impl CommandBuff {
    pub(crate) fn send_cmd(&mut self, fis: &frame_information_structure::RegisterHostToDevFis) {
        let b = unsafe {
            &mut *(&mut self.buff as *mut _
                as *mut frame_information_structure::RegisterHostToDevFis)
        };
        b.fis_type = fis.fis_type;
        b.cfg.set_port(0);
        b.cfg.set_cmd_bit(true);
        b.command = fis.command;
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

        // prevents writes to b being optimized out
    }
}

#[repr(C)]
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct PhysicalRegionDescription {
    base_addr: u64,
    _res: u32,
    /// byte 0 should always be `1`. The actual value is one less than buffer size in bytes
    data_count: PhysicalDataCount,
}

impl PhysicalRegionDescription {
    /// Attempts to create a new Self using the start address and the size in bytes of the region
    pub(crate) fn new(start: u64, len: u32) -> Option<Self> {
        if len > 0x40_000 {
            return None;
        } else {
            Some(Self {
                base_addr: start,
                _res: 0,
                data_count: PhysicalDataCount::new(len)?,
            })
        }
    }
}

#[repr(transparent)]
#[derive(Copy, Clone, Debug, Default)]
struct PhysicalDataCount {
    inner: u32,
}

impl PhysicalDataCount {
    /// Creates a new instance of Self. `count` must be less than or equal to 4MiB and must be
    /// an even number.
    fn new(count: u32) -> Option<Self> {
        if count > (1 << 21) || (count & 1 != 0) {
            return None;
        } else {
            let mut t = Self { inner: 0 };
            t.set_data_count(count);
            Some(t)
        }
    }

    // not used atm
    #[allow(dead_code)]
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
// unused primitive at this point. May not actually use this.
#[allow(dead_code)]
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
