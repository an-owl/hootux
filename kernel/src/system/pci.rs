//! This module is for accessing PCI and PCIe devices
//!
//! # PCI Configuration Address Space
//!
//! The PCI Configuration Address Space is separated into multiple classes a 16-bit Segment group
//! address, 8-bit bus address, 6-bit device address and a 3-bit function id. PCI does not implement
//! segment groups, in this case where a segment group is required it may be set to `0`

use crate::allocator::alloc_interface::MmioAlloc;
use crate::system::pci::configuration::register::HeaderType;
use crate::system::pci::configuration::PciHeader;
use core::cmp::Ordering;
use core::fmt::Formatter;

mod configuration;
pub mod meta;
mod scan;

lazy_static::lazy_static! {
    pub static ref PCI_DEVICES: spin::RwLock<alloc::collections::BTreeMap<DeviceAddress,RwLockOrd<DeviceControl>>> =
        spin::RwLock::new(
            alloc::collections::BTreeMap::new()
        );

    static ref PCI_META: spin::RwLock<alloc::vec::Vec<meta::MetaInfo>> = spin::RwLock::new(alloc::vec::Vec::new());
}

/// Iterates over initialized PCI devices after `skip` copying them into `buff`, returning the
/// number of devices copied.
pub fn get_dev_list(buff: &mut [meta::MetaInfo], skip: usize) -> usize {
    let l = PCI_META.read();
    let mut c = 0;
    for (i, (o, b)) in core::iter::zip(l.iter().skip(skip), buff).enumerate() {
        *b = *o;
        c = i
    }

    c
}

pub fn dev_count() -> usize {
    PCI_META.read().len()
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct DeviceAddress {
    segment_group: u16,
    bus: u8,
    device: u8,
    function: u8,
}

/// Used for addressing PCI devices via the I/O bus
#[repr(transparent)]
#[derive(Copy, Clone)]
struct IOAddress {
    addr: u32,
}

impl IOAddress {
    /// Creates a new IOAddress from the given bus information.
    /// `register` is treated as a register number and not a byte address, as such it should be a
    /// value within`0..=64`
    ///
    /// # Panics
    ///
    /// This fn will panic under any of the following conditions
    /// - `device => 32`
    /// - `function => 8`
    /// - `register => 64`
    fn new(bus: u8, device: u8, function: u8, register: u8) -> Self {
        assert!(device < 32);
        assert!(function < 8);
        assert!(register < 64);

        Self {
            addr: ((bus as u32) << 16)
                | ((device as u32) << 11)
                | ((function as u32) << 8)
                | (register as u32) << 2
                | 0x80000000,
        }
    }

    /// Updates the register field to `new_reg`
    fn set_register(&mut self, new_reg: u8) {
        assert!(new_reg < 64);

        self.addr &= !255;
        self.addr |= ((new_reg as u32) << 2);
    }

    /// Sets the enable bit to true
    fn enable(mut self, enable: bool) -> Self {
        let last_bit = 0x80000000;
        if enable {
            self.addr |= last_bit
        } else {
            self.addr &= !last_bit
        }

        self
    }
}

impl core::fmt::Debug for IOAddress {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let bus = (self.addr >> 16) & 255;
        let device = (self.addr >> 11) & 63;
        let function = (self.addr >> 8) & 7;
        let offset = self.addr & 255; // this is capable of not being used for

        let enable = if self.addr & 0x80000000 == 0 {
            false
        } else {
            true
        };

        let mut o = f.debug_struct(core::any::type_name::<Self>());
        o.field("enable", &enable);
        o.field("bus", &bus);
        o.field("device", &device);
        o.field("function", &function);
        o.field("offset", &offset);
        o.finish()
    }
}

impl DeviceAddress {
    /// Creates a new device address ath the given address
    /// If the Enhanced Configuration Mechanism is not being used the bus group should be set to 0.
    ///
    /// # Panics
    ///
    /// This fn will panic if the `bus > 32` or `function > 8`.
    fn new(segment_group: u16, bus: u8, device: u8, function: u8) -> Self {
        assert!(device < 32);
        assert!(function < 8);

        Self {
            segment_group,
            bus,
            device,
            function,
        }
    }

    /// Creates an [IOAddress] from the address within `self`.
    ///
    /// # Panics
    ///
    /// This fn will panic if `self.segment_group` is not 0. This is because segment groups cannot
    /// be accessed using the legacy method and this may cause the wrong device to be accessed.
    fn as_io_addr(&self, register: u8) -> IOAddress {
        assert_eq!(self.segment_group, 0);
        IOAddress::new(self.bus, self.device, self.function, register)
    }

    fn advanced_cfg_addr(&self, mcfg: &acpi::mcfg::PciConfigRegions) -> Option<u64> {
        mcfg.physical_address(self.segment_group, self.bus, self.device, self.function)
    }

    fn as_int(&self) -> (u16, u8, u8, u8) {
        (self.segment_group, self.bus, self.device, self.function)
    }

    /// Creates a new `Self` with a the function number set to `f_num`
    ///
    /// # Panics
    ///
    /// This fn will panic if `F_num > 7`
    fn new_function(&self, f_num: u8) -> Self {
        Self::new(self.segment_group, self.bus, self.device, f_num)
    }
}

impl alloc::fmt::Display for DeviceAddress {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}:{:x}",
            self.segment_group, self.bus, self.device, self.function
        )
    }
}

pub struct DeviceControl {
    address: DeviceAddress,
    vendor: u16,
    device: u16,
    dev_type: HeaderType,
    class: [u8; 3],
    bar: alloc::collections::BinaryHeap<BarInfo>,
    header: alloc::boxed::Box<dyn PciHeader, MmioAlloc>, // should alloc be generic?
}

impl core::fmt::Debug for DeviceControl {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut d = f.debug_struct("DeviceControl");
        d.field("address", &self.address);
        d.field("vendor", &self.vendor);
        d.field("device", &self.device);
        d.finish()
    }
}

impl PartialEq for DeviceControl {
    fn eq(&self, other: &Self) -> bool {
        self.address.eq(&other.address)
    }
}

impl Eq for DeviceControl {}

impl PartialOrd for DeviceControl {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.address.partial_cmp(&other.address)
    }
}

impl Ord for DeviceControl {
    fn cmp(&self, other: &Self) -> Ordering {
        self.address.cmp(&other.address)
    }
}

impl DeviceControl {
    fn new(cfg_region_addr: u64, address: DeviceAddress) -> Option<Self> {
        let header_region = unsafe {
            MmioAlloc::new(cfg_region_addr as usize)
                .boxed_alloc::<configuration::CommonHeader>()
                .unwrap()
        };

        if header_region.vendor() == u16::MAX {
            return None;
        }

        let header_type = header_region.header_type();

        // Drop to prevent aliasing
        drop(header_region);
        let mut header: alloc::boxed::Box<dyn PciHeader, MmioAlloc> = match header_type {
            HeaderType::Generic => unsafe {
                let header = MmioAlloc::new(cfg_region_addr as usize)
                    .boxed_alloc::<configuration::GenericHeader>()
                    .unwrap();
                header
            },
            HeaderType::Bridge => unsafe {
                let header = MmioAlloc::new(cfg_region_addr as usize)
                    .boxed_alloc::<configuration::BridgeHeader>()
                    .unwrap();

                header
            },
            HeaderType::CardBusBridge => unsafe {
                let header = MmioAlloc::new(cfg_region_addr as usize)
                    .boxed_alloc::<configuration::CardBusBridge>()
                    .unwrap();

                header
            },
        };

        let mut bar = alloc::collections::BinaryHeap::new();
        for i in 0..header.bar_count() {
            if let Some(info) = BarInfo::new(&mut *header, i) {
                bar.push(info)
            }
        }

        let mut class = header.class();
        class.reverse();

        Self {
            address,
            vendor: header.vendor(),
            device: header.device(),
            dev_type: header_type,
            class,
            bar,
            header,
        }
        .into()
    }

    fn address(&self) -> DeviceAddress {
        self.address
    }

    fn dev_type(&self) -> HeaderType {
        self.dev_type
    }

    fn is_multi_fn(&self) -> bool {
        self.header.is_multi_fn()
    }

    fn get_config_region(&mut self) -> &mut alloc::boxed::Box<dyn PciHeader, MmioAlloc> {
        &mut self.header
    }
}

/// Caches information about Base Address Registers
struct BarInfo {
    id: u8, // todo may need to be u16
    align: u64,
    reg_info: configuration::register::BarType,
}

impl BarInfo {
    fn new<H: PciHeader + ?Sized>(header: &mut H, id: u8) -> Option<Self> {
        use configuration::register::BarType;
        let bar = header.bar(id)?;
        let reg_info = bar.bar_type();

        match reg_info {
            BarType::DwordIO | BarType::Dword(_) => {
                Self {
                    id,
                    // SAFETY: This is safe because this is fn is only called when the device is initialized into the kernel
                    align: unsafe { bar.alignment() as u64 },
                    reg_info,
                }
                .into()
            }

            BarType::Qword(_) => {
                // SAFETY: this is safe because `bar` is not referenced within this arm
                let bar_long = unsafe { header.bar_long(id)? };
                Self {
                    id,
                    align: unsafe { bar_long.alignment() },
                    reg_info,
                }
                .into()
            }
        }
    }
}

pub struct RwLockOrd<T: Ord> {
    pub lock: spin::RwLock<T>,
}

impl<T: Ord> PartialEq for RwLockOrd<T> {
    fn eq(&self, other: &Self) -> bool {
        self.lock.read().eq(&other.lock.read())
    }
}

impl<T: Ord> Eq for RwLockOrd<T> {}

impl<T: Ord> PartialOrd for RwLockOrd<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.lock.read().partial_cmp(&other.lock.read())
    }
}

impl<T: Ord> Ord for RwLockOrd<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.lock.read().cmp(&other.lock.read())
    }
}

impl<T: Ord> From<T> for RwLockOrd<T> {
    fn from(value: T) -> Self {
        Self {
            lock: spin::RwLock::new(value),
        }
    }
}

impl PartialEq<Self> for BarInfo {
    fn eq(&self, other: &Self) -> bool {
        self.id.eq(&other.id)
    }
}

impl Eq for BarInfo {}

impl PartialOrd for BarInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.id.partial_cmp(&other.id)
    }
}

impl Ord for BarInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
}

pub fn enumerate_devices(pci_regions: &acpi::mcfg::PciConfigRegions) {
    scan::scan_advanced(pci_regions)
}
