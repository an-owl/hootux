pub use crate::interrupts::apic::apic_structures::apic_types::InterruptDeliveryMode;
use crate::system::pci::capabilities::CapabilityId;
use crate::system::pci::DeviceControl;
use core::any::Any;

pub struct MessageSigInt<'a> {
    parent: &'a DeviceControl,
    control: &'a mut MsiControlBits,
    addr_low: &'a mut u32,
    addr_high: Option<&'a mut u32>,
    message: &'a mut u16,
    mask: Option<&'a mut u32>,
    pending: Option<&'a mut u32>,
}

impl<'a> MessageSigInt<'a> {
    /// Returns the current control bits for self
    pub fn get_control(&self) -> MsiControlBits {
        *self.control // control is non volatile.
    }

    /// Sets the enable bit to `enable`
    ///
    /// # Safety
    ///
    /// The caller must ensure that any interrupts are correctly handled
    pub unsafe fn enable(&mut self, enable: bool) {
        let mut bits = *self.control;
        bits.set(MsiControlBits::ENABLE, enable);
        core::ptr::write_volatile(self.control, bits)
    }

    /// Returns the number of vectors requested by the fn
    pub fn get_vec_count(&self) -> u8 {
        self.control.requested_vectors()
    }

    /// Sets the number of vectors this device can use. Returns the number of vectors set
    ///
    /// # Panics
    ///
    /// This fn will panic if `count` is not a power of 2 or is greater than 32
    pub fn set_vec_count(&mut self, count: u8) -> u8 {
        self.control.set_vectors(count)
    }

    /// Returns the number of vectors currently set
    pub fn get_alloc_vec(&self) -> u8 {
        self.control.allocated_vectors()
    }

    /// Sets the address of `self`. The upper 16 bits of `msg` will be ignored. The Upper 32 bits of
    /// `addr` will only be written if 64 bit addresses are supported.
    pub fn set_interrupt(
        &mut self,
        addr: InterruptAddress,
        msg: InterruptMessage,
    ) -> (InterruptAddress, InterruptMessage) {
        let (high, low) = addr.half();

        unsafe {
            core::ptr::write_volatile(self.addr_low, low);
            if let Some(ad_high) = self.addr_high.as_mut() {
                core::ptr::write_volatile(*ad_high, high)
            }
            core::ptr::write_volatile(self.message, msg.as_raw() as u16)
        }

        (self.addr(), self.msg())
    }

    /// Reads the interrupt Message from memory
    pub fn addr(&self) -> InterruptAddress {
        let mut low = unsafe { core::ptr::read_volatile(self.addr_low) };
        let high = if let Some(ad_high) = self.addr_high.as_ref() {
            unsafe { core::ptr::read_volatile(*ad_high) }
        } else {
            0
        };

        InterruptAddress::from_half(low, high)
    }

    /// Reads the interrupt message from memory
    pub fn msg(&self) -> InterruptMessage {
        InterruptMessage(unsafe { core::ptr::read_volatile(self.message) } as u32)
    }
}

impl<'a> super::Capability for MessageSigInt<'a> {
    fn id(&self) -> CapabilityId {
        CapabilityId::Msi
    }

    fn any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

bitflags::bitflags! {
    #[repr(transparent)]
    pub struct MsiControlBits: u16 {
        const ENABLE = 1;
        const BITS_64 = 1 << 7;
        const PER_VEC_MASK = 1 << 8;
    }

    #[repr(transparent)]
    pub struct MsiXControlBits: u16 {
        const ENABLE = 1 << 15;
        const MASK = 1 << 14;
    }

    #[repr(transparent)]
    pub struct MsiXVectorControl: u32 {
        const MASK = 1;
    }
}

impl MsiControlBits {
    /// Returns the number of requested vectors
    pub fn requested_vectors(&self) -> u8 {
        let mut t = self.bits; // read only does not need to be volatile

        t >>= 1;
        t &= 7;

        assert!(t < 5); // field is 3 bits values 6,7 are not allowed
        let count = 1u8 << t;
        count
    }

    pub fn allocated_vectors(&self) -> u8 {
        let mut t = unsafe { core::ptr::read_volatile(&self.bits) }; // does not need to be volatile

        t >>= 4;
        t &= 7;

        assert!(t < 5); // field is 3 bits values 6,7 are not allowed
        let count = 1u8 << t;
        count
    }

    /// Sets the number of interrupt vectors this function will use
    /// The returned value is the value read back from the register after writing
    ///
    /// # Panics
    ///
    /// This fn will panic if `count` is not a power of 2 or is greater than 32.
    pub fn set_vectors(&mut self, count: u8) -> u8 {
        assert!(count.is_power_of_two());
        assert!(count <= 32);

        let mut cont = unsafe { core::ptr::read_volatile(&self.bits) };
        cont &= !(0x70);

        match count {
            0 => {}
            2 => cont |= 0x10,
            4 => cont |= 0x20,
            8 => cont |= 0x30,
            16 => cont |= 0x40,
            32 => cont |= 0x50,
            _ => unreachable!(),
        }

        unsafe { core::ptr::write_volatile(&mut self.bits, cont) };
        self.allocated_vectors()
    }
}

impl MsiXControlBits {
    /// This fn will return the number of vectors that `self` uses.
    /// The returned value will be not larger than 2048
    pub fn table_size(&self) -> u16 {
        (unsafe { core::ptr::read_volatile(&self.bits) } & 0x7f) + 1
    }
}

impl<'a> TryFrom<&'a mut DeviceControl> for MessageSigInt<'a> {
    type Error = ();

    fn try_from(dev: &'a mut DeviceControl) -> Result<Self, Self::Error> {
        // SAFETY: casts within this fn are safe they are calculated from an address found within
        // the device header and are guaranteed to point to valid data
        let ptr = dev.capabilities.get(&CapabilityId::Msi).ok_or(())?.clone();
        let base = &dev.cfg_region[ptr.offset as usize] as *const _ as usize;

        let control = unsafe { &mut *((base + 0x2) as *mut MsiControlBits) };
        let addr_low = unsafe { &mut *((base + 0x4) as *mut u32) };

        let (addr_high, higher_offset) = if control.contains(MsiControlBits::BITS_64) {
            (Some(unsafe { &mut *((base + 0x8) as *mut u32) }), 4)
        } else {
            (None, 0)
        };

        let message = unsafe { &mut *((base + 0x8 + higher_offset) as *mut u16) };
        let (mask, pending) = if control.contains(MsiControlBits::PER_VEC_MASK) {
            (
                Some(unsafe { &mut *((base + 0xc + higher_offset) as *mut u32) }),
                Some(unsafe { &mut *((base + 0x10 + higher_offset) as *mut u32) }),
            )
        } else {
            (None, None)
        };

        Ok(Self {
            parent: dev,
            control,
            addr_low,
            addr_high,
            message,
            mask,
            pending,
        })
    }
}

pub struct MessageSignaledIntX<'a> {
    parent: &'a mut DeviceControl,
    control: &'a mut MsiXControlBits,
    table_bar: u8,
    table_offset: u32,
    pba_bar: u8,
    pba_offset: u32,
}

impl<'a> super::Capability for MessageSignaledIntX<'a> {
    fn id(&self) -> CapabilityId {
        CapabilityId::MsiX
    }

    fn any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl<'a> TryFrom<&'a mut DeviceControl> for MessageSignaledIntX<'a> {
    type Error = ();

    fn try_from(dev: &'a mut DeviceControl) -> Result<Self, Self::Error> {
        let cap = dev.capabilities.get(&CapabilityId::MsiX).ok_or(())?.clone();
        let base =
            unsafe { &mut *(&mut dev.cfg_region[cap.offset as usize] as *mut _ as *mut RawMsiX) };

        let table_bar = (base.table_entry & 0x3) as u8;
        let table_offset = base.table_entry & !0x3;
        let pba_bar = (base.pba_entry & 0x3) as u8;
        let pba_offset = base.pba_entry & !0x3;

        Ok(Self {
            parent: dev,
            control: &mut base.ctl,
            table_bar,
            table_offset,
            pba_bar,
            pba_offset,
        })
    }
}

impl<'a> MessageSignaledIntX<'a> {
    pub fn get_ctl(&self) -> MsiXControlBits {
        self.control.clone()
    }

    /// Returns a slice to the functions MSI-X Vector Table
    pub fn get_vec_table(&self) -> &[MsiXVectorEntry] {
        let id = self.table_bar;
        let size = self.control.table_size() as usize;
        let offset = self.table_offset;

        // validate_bar() will ensure that these do nto panic
        let bar = self.parent.bar.get(&id).unwrap();
        // SAFETY: validate_bar() will check that this address is correct.
        let raw = unsafe { bar.virt_region.0.unwrap().as_ref() };
        // SAFETY: This is safe because the offset is given by hardware and is guaranteed to be
        // a valid safe address
        let ptr = unsafe { raw.as_ptr().offset(offset as isize).cast() };
        // SAFETY: Hardware guarantees that this is valid
        unsafe { core::slice::from_raw_parts(ptr, size) }
    }

    /// Returns a mutable slice to the functions Vector table
    pub fn get_vec_table_mut(&mut self) -> &mut [MsiXVectorEntry] {
        let id = self.table_bar;
        let size = self.control.table_size() as usize;
        let offset = self.table_offset;
        self.parent.validate_bar(id).unwrap(); // this will print a good enough message right?

        // validate_bar() will ensure that these do nto panic
        let bar = self.parent.bar.get_mut(&id).unwrap();
        // SAFETY: validate_bar() will check that this address is correct.
        let raw = unsafe { bar.virt_region.0.unwrap().as_mut() };
        // SAFETY: This is safe because the offset is given by hardware and is guaranteed to be
        // a valid safe address
        let ptr = unsafe { raw.as_mut_ptr().offset(offset as isize).cast() };
        // SAFETY: Hardware guarantees that this is valid
        unsafe { core::slice::from_raw_parts_mut(ptr, size) }
    }

    pub fn get_pba(&self) -> PendingBitArray {
        let id = self.pba_bar;
        let size = self.control.table_size();
        let offset = self.table_offset;

        let bar = self.parent.bar.get(&id).unwrap();
        let slice = unsafe {
            let raw = bar.virt_region.0.unwrap().as_mut();
            let len = size.div_ceil(64);
            let ptr = raw.as_ptr().offset(offset as isize).cast::<u64>();
            core::slice::from_raw_parts(ptr, len as usize)
        };

        PendingBitArray {
            len: size,
            array: volatile::Volatile::new(slice),
        }
    }
}

#[repr(C)]
struct RawMsiX {
    header: super::CapabilityHeader,
    ctl: MsiXControlBits,
    table_entry: u32,
    pba_entry: u32,
}

#[repr(C)]
pub struct MsiXVectorEntry {
    address: InterruptAddress,
    message: InterruptMessage,
    control: MsiXVectorControl,
}

pub struct PendingBitArray<'a> {
    len: u16,
    array: volatile::Volatile<&'a [u64]>,
}

impl<'a> PendingBitArray<'a> {
    /// Retrieves the status of the given bit. Returns none if the requested bit is higher than the
    /// number of vectors
    pub fn get_bit(&self, bit: u16) -> Option<bool> {
        if bit > self.len {
            return None;
        }
        let index = (bit >> 6) as usize;
        let mask = 1u64 << (bit & 0x7f);

        let t = self.array.index(index).read();

        if t & mask != 0 {
            Some(true)
        } else {
            Some(false)
        }
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct InterruptAddress(u64);

impl InterruptAddress {
    /// Creates a new InterruptAddress which corresponds to an address in physical memory.
    ///
    /// `cpu` is the target cpu for which will handle the interrupt
    ///
    /// There are 2 other fields defined "redirection hint indication" and "destination mode"
    /// but I dont fully understand what these do.
    ///
    /// Note: 0xfee00000..=0xfeefffff is reserved as I/O memory because of this
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    pub fn new(cpu: u8) -> Self {
        let n: u32 = 0xfee00000 | ((cpu as u32) << 12);

        Self(n as u64)
    }

    /// Splits self into (high: u32, low: u32)
    fn half(self) -> (u32, u32) {
        let high = (self.0 >> 32) as u32;
        let low = self.0 as u32;

        (high, low)
    }

    /// Returns self from 2 halves.
    fn from_half(low: u32, high: u32) -> Self {
        Self(low as u64 + ((high as u64) << 32))
    }

    pub(crate) fn raw(&self) -> u64 {
        self.0
    }
}

impl Into<u64> for InterruptAddress {
    fn into(self) -> u64 {
        self.0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct InterruptMessage(u32);

impl InterruptMessage {
    /// Creates a new InterruptDeliveryMessage for x86 platforms.
    ///
    /// - `vector` sets the vector that this message will trigger.
    /// - `mode` is the interrupt mode that this message will use
    /// - While `level_trigger` is `true` the interrupt will  use a level based trigger otherwise the
    /// interrupt will be edge based
    /// - `trigger_high` will cause the interrupt to be triggered when the interrupt is asserted.
    /// Otherwise it will be while the interrupt is deasserted. This is ignored while `level_trigger` is `false`
    ///
    /// The recommendation is to set `mode` to `InterruptDeliveryMode::Fixed` and `level_trigger` to `false`
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    fn new(
        vector: u8,
        mode: InterruptDeliveryMode,
        level_trigger: bool,
        trigger_high: bool,
    ) -> Self {
        let mut n: u32 = vector as u32 | ((mode as u32) << 8);

        if level_trigger {
            n |= 1 << 15;
            if trigger_high {
                n |= 1 << 14;
            }
        }

        Self(n)
    }

    pub(crate) fn as_raw(&self) -> u32 {
        self.0
    }
}
