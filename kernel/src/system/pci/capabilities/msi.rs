pub use crate::interrupts::apic::apic_structures::apic_types::InterruptDeliveryMode;
use crate::system::pci::DeviceControl;
use crate::system::pci::capabilities::CapabilityId;
use core::alloc::Allocator;
use core::any::Any;

pub(crate) fn get_next_msi_affinity() -> u8 {
    static MSI_NEXT_AFFINITY: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

    let count = MSI_NEXT_AFFINITY.fetch_add(1, atomic::Ordering::Relaxed);
    let num_cpus = 1; // fixme actually get num of cpus
    (count % (num_cpus.min(u8::MAX) as u64)) as u8 // mod of num cpus or u8::MAX whichever is lower (MSI can only use 8 bits of cpu address on x86)
}

pub struct MessageSigInt<'a> {
    control: &'a mut MsiControlBits,
    addr_low: &'a mut u32,
    addr_high: Option<&'a mut u32>,
    message: &'a mut u16,
    pub mask: Option<MsiMaskBits<'a>>,
    pub pending: Option<&'a mut u32>,
}

#[repr(transparent)]

pub struct MsiMaskBits<'a> {
    pub inner: volatile::Volatile<&'a mut u32>,
}

impl<'a> MsiMaskBits<'a> {
    fn new(refer: &'a mut u32) -> Self {
        Self {
            inner: volatile::Volatile::new(refer),
        }
    }
    /// Returns the value of the interrupt mask for the selected vector
    pub fn get(&self, vector: u8) -> bool {
        let mask = 1 << vector;
        if mask & self.inner.read() == 0 {
            false
        } else {
            true
        }
    }

    /// Sets the mask for the selected vector
    pub fn set(&mut self, vector: u8, value: bool) -> bool {
        let mut t = self.inner.read();
        let mask = 1 << vector;

        if value {
            t |= mask;
        } else {
            t &= !mask;
        }

        self.inner.write(t);
        self.get(vector)
    }
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
        unsafe {
            let mut bits = *self.control;
            bits.set(MsiControlBits::ENABLE, enable);
            core::ptr::write_volatile(self.control, bits)
        }
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
        let low = unsafe { core::ptr::read_volatile(self.addr_low) };
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

impl<'a> super::Capability<'a> for MessageSigInt<'a> {
    fn id(&self) -> CapabilityId {
        CapabilityId::Msi
    }

    fn boxed(self) -> alloc::boxed::Box<(dyn Any + 'a)> {
        alloc::boxed::Box::new(self)
    }

    fn any_mut(&mut self) -> &mut (dyn Any + 'a) {
        self
    }
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Copy, Clone)]
    pub struct MsiControlBits: u16 {
        const ENABLE = 1;
        const BITS_64 = 1 << 7;
        const PER_VEC_MASK = 1 << 8;
    }

    #[repr(transparent)]
    #[derive(Debug, Copy, Clone)]
    pub struct MsiXControlBits: u16 {
        const ENABLE = 1 << 15;
        const MASK = 1 << 14;
    }

    #[repr(transparent)]
    #[derive(Debug, Copy, Clone)]
    pub struct MsiXVectorControl: u32 {
        const MASK = 1;
    }
}

impl MsiControlBits {
    /// Returns the number of requested vectors
    pub fn requested_vectors(&self) -> u8 {
        let mut t = self.bits(); // read only does not need to be volatile

        t >>= 1;
        t &= 7;

        assert!(t < 5); // field is 3 bits values 6,7 are not allowed
        let count = 1u8 << t;
        count
    }

    pub fn allocated_vectors(&self) -> u8 {
        let mut t = unsafe { core::ptr::read_volatile(&self) }.bits(); // does not need to be volatile

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

        let mut cont = unsafe { core::ptr::read_volatile(&self) }.bits();
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

        unsafe { core::ptr::write_volatile(self, Self::from_bits_retain(cont)) };
        self.allocated_vectors()
    }
}

impl MsiXControlBits {
    /// This fn will return the number of vectors that `self` uses.
    /// The returned value will be not larger than 2048
    pub fn table_size(&self) -> u16 {
        (unsafe { core::ptr::read_volatile(self) }.bits() & 0x7f) + 1
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
                Some(MsiMaskBits::new(unsafe {
                    &mut *((base + 0xc + higher_offset) as *mut u32)
                })),
                Some(unsafe { &mut *((base + 0x10 + higher_offset) as *mut u32) }),
            )
        } else {
            (None, None)
        };

        Ok(Self {
            control,
            addr_low,
            addr_high,
            message,
            mask,
            pending,
        })
    }
}

/// MSI-X uses memory allocated within BAR memory to store registers as a result drivers using MSI-X
/// should not access this memory directly. This struct provides methods that will alias these regions.
/// They assume the driver does not access this memory and are not marked as `unsafe`.
pub struct MessageSignaledIntX<'a> {
    parent: &'a mut DeviceControl,
    control: &'a mut MsiXControlBits,
    table_bar: u8,
    table_offset: u32,
    pba_bar: u8,
    pba_offset: u32,
}

impl<'a> super::Capability<'a> for MessageSignaledIntX<'a> {
    fn id(&self) -> CapabilityId {
        CapabilityId::MsiX
    }

    fn boxed(self) -> alloc::boxed::Box<(dyn Any + 'a)> {
        alloc::boxed::Box::new(self)
    }

    fn any_mut(&'a mut self) -> &'a mut (dyn Any + 'a) {
        self
    }
}

impl<'a> TryFrom<&'a mut DeviceControl> for MessageSignaledIntX<'a> {
    type Error = ();

    fn try_from(dev: &'a mut DeviceControl) -> Result<Self, Self::Error> {
        let cap = dev.capabilities.get(&CapabilityId::MsiX).ok_or(())?.clone();

        #[allow(invalid_reference_casting)]
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

pub struct MsiXVectorTable<'a> {
    _phantom: core::marker::PhantomData<&'a ()>,
    table: alloc::boxed::Box<[MsiXVectorEntry], crate::alloc_interface::MmioAlloc>,
}

impl<'a> MsiXVectorTable<'a> {
    pub fn iter(&self) -> core::slice::Iter<MsiXVectorEntry> {
        self.table.iter()
    }

    pub fn iter_mut(&mut self) -> core::slice::IterMut<MsiXVectorEntry> {
        self.table.iter_mut()
    }
}

impl<'a> core::ops::Index<u16> for MsiXVectorTable<'a> {
    type Output = MsiXVectorEntry;

    fn index(&self, index: u16) -> &Self::Output {
        &self.table[index as usize]
    }
}

impl<'a> core::ops::IndexMut<u16> for MsiXVectorTable<'a> {
    fn index_mut(&mut self, index: u16) -> &mut Self::Output {
        &mut self.table[index as usize]
    }
}

impl<'a> MessageSignaledIntX<'a> {
    pub fn get_ctl(&self) -> MsiXControlBits {
        self.control.clone()
    }

    /// Returns a slice to the functions MSI-X Vector Table
    pub fn get_vec_table(&self) -> MsiXVectorTable {
        let t = self.parent.bar[self.table_bar as usize]
            .as_ref()
            .expect("PCI device did not implement BAR specified for MSI-X table");
        let size = core::mem::size_of::<MsiXVectorEntry>() * self.control.table_size() as usize;
        let addr = t.addr + self.table_offset as u64;

        // SAFETY: It's not really safe but it's documented
        let alloc = unsafe { crate::alloc_interface::MmioAlloc::new(addr as usize) };
        // Layout shouldn't panic, alloc may if liner memory is depleted
        let ptr = alloc
            .allocate(
                core::alloc::Layout::from_size_align(
                    size,
                    core::mem::align_of::<MsiXVectorEntry>(),
                )
                .unwrap(),
            )
            .expect("System ran out of memory")
            .cast()
            .as_ptr();
        // SAFETY: This is safe, ptr will always point to the existing vector table. The size is read directly from the hardware
        let s = unsafe { core::slice::from_raw_parts_mut(ptr, self.control.table_size() as usize) };

        // SAFETY: this is safe because it uses a reference not a pointer
        MsiXVectorTable {
            _phantom: core::marker::PhantomData,
            table: unsafe { alloc::boxed::Box::from_raw_in(s, alloc) },
        }
    }

    /// Returns a reference to the Pending Bit Array
    ///
    /// # Safety
    ///
    /// Technically this fn is unsafe because it breaks rusts aliasing rules however the aliasing
    /// rules are broken by the PBA regardless of whether this fn is called or not.  
    pub fn get_pba(&self) -> PendingBitArray {
        let size = self.control.table_size();

        let bar = self.parent.bar[self.pba_bar as usize]
            .as_ref()
            .expect("PCI device did not implement BAR specified for MSI-X PBA");

        // SAFETY: This aliases memory but this is documented.
        let alloc = unsafe {
            crate::alloc_interface::MmioAlloc::new((bar.addr + self.pba_offset as u64) as usize)
        };

        let ptr = alloc
            .allocate(
                core::alloc::Layout::from_size_align(
                    size.div_ceil(core::mem::size_of::<u64>() as u16) as usize,
                    core::mem::align_of::<u64>(),
                )
                .unwrap(),
            )
            .unwrap()
            .cast()
            .as_ptr();

        // SAFETY: This is safe the memory that is uninitialized is not accessible by PendingBitArray
        let s = unsafe {
            core::slice::from_raw_parts_mut(
                ptr,
                (size as usize).div_ceil(core::mem::size_of::<u64>()),
            )
        };
        // SAFETY: The memory is initialized and only boxed once.
        let b = unsafe { alloc::boxed::Box::from_raw_in(s, alloc) };

        PendingBitArray {
            _phantom: core::marker::PhantomData,
            len: size,
            array: volatile::Volatile::new(b),
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

impl MsiXVectorEntry {
    /// Sets the entries address and message.
    ///
    /// This fn does not conform to the normal read back method other functions use. This is because
    /// MsiVector Entries exist in system memory not MMIO memory  
    pub fn set_entry(&mut self, addr: InterruptAddress, message: InterruptMessage) {
        unsafe {
            core::ptr::write_volatile(&mut self.address, addr);
            core::ptr::write_volatile(&mut self.message, message);
        }
    }

    /// Sets the mask for `Self` to `value`
    pub fn mask(&mut self, value: bool) {
        let mut c = unsafe { core::ptr::read_volatile(&self.control) };
        c.set(MsiXVectorControl::MASK, value);
        unsafe { core::ptr::write_volatile(&mut self.control, c) }
    }
}

pub struct PendingBitArray<'a> {
    _phantom: core::marker::PhantomData<&'a ()>,
    len: u16,
    array: volatile::Volatile<alloc::boxed::Box<[u64], crate::alloc_interface::MmioAlloc>>,
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
    pub fn new(
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
