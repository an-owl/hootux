//! This module is for accessing PCI and PCIe devices
//!
//! # PCI Configuration Address Space
//!
//! The PCI Configuration Address Space is separated into multiple classes a 16-bit Segment group
//! address, 8-bit bus address, 6-bit device address and a 3-bit function id. PCI does not implement
//! segment groups, in this case where a segment group is required it may be set to `0`

use crate::alloc_interface::MmioAlloc;
use crate::system::pci::configuration::{register::HeaderType, PciHeader};
use core::{cmp::Ordering, fmt::Formatter};

pub mod capabilities;
mod configuration;
mod scan;

lazy_static::lazy_static! {
    static ref PCI_DEVICES: HwMap<DeviceAddress,alloc::sync::Arc<spin::Mutex<DeviceControl>>> = HwMap::new();
}

/// Attempts to lock a device function returns None is the device does not exist fr is not found.
#[allow(dead_code)] // this will be used at some point
pub(crate) fn get_function(
    addr: DeviceAddress,
) -> Option<alloc::sync::Arc<spin::Mutex<DeviceControl>>> {
    Some(PCI_DEVICES.get(&addr)?.clone())
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub struct DeviceAddress {
    segment_group: u16,
    bus: u8,
    device: u8,
    function: u8,
}

impl DeviceAddress {
    /// Creates a new device address ath the given address
    /// If the Enhanced Configuration Mechanism is not being used the bus group should be set to 0.
    ///
    /// # Panics
    ///
    /// This fn will panic if the `bus > 32` or `function > 8`.
    pub fn new(segment_group: u16, bus: u8, device: u8, function: u8) -> Self {
        assert!(device < 32);
        assert!(function < 8);

        Self {
            segment_group,
            bus,
            device,
            function,
        }
    }

    fn advanced_cfg_addr(&self, mcfg: &acpi::mcfg::PciConfigRegions) -> Option<u64> {
        mcfg.physical_address(self.segment_group, self.bus, self.device, self.function)
    }

    pub fn as_int(&self) -> (u16, u8, u8, u8) {
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
    cfg_region: alloc::boxed::Box<[u8; 4096], MmioAlloc>, // should alloc be generic?
    header: &'static mut dyn PciHeader, // maybe replace this in favour of fetching this when its needed from cfg_region
    bar: alloc::collections::BTreeMap<u8, BarInfo>,
    capabilities:
        alloc::collections::BTreeMap<capabilities::CapabilityId, capabilities::CapabilityPointer>,
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

/*
pub struct DeviceBinding<'a> {
    inner: &'a crate::kernel_structures::mutex::Mutex<DeviceControl>,
}

impl<'a> DeviceBinding<'a> {
    pub fn lock(&self) -> LockedDevice {
        LockedDevice {
            inner: self.inner.lock(),
        }
    }
}

 */

pub struct LockedDevice<'a> {
    inner: crate::kernel_structures::mutex::MutexGuard<'a, DeviceControl>,
}

impl<'a> core::ops::Deref for LockedDevice<'a> {
    type Target = DeviceControl;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl<'a> core::ops::DerefMut for LockedDevice<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut *self.inner
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
        let mut cfg_region = unsafe {
            MmioAlloc::new(cfg_region_addr as usize)
                .boxed_alloc::<[u8; 4096]>()
                .unwrap()
        };

        let header_region =
            unsafe { &mut *(&mut cfg_region[0] as *mut _ as *mut configuration::CommonHeader) };

        if header_region.vendor() == u16::MAX {
            return None;
        }
        let header_type = header_region.header_type();

        // Drop to prevent aliasing
        let header: &mut dyn PciHeader = match header_type {
            HeaderType::Generic => unsafe {
                &mut *(&mut cfg_region[0] as *mut _ as *mut configuration::GenericHeader)
            },
            HeaderType::Bridge => unsafe {
                &mut *(&mut cfg_region[0] as *mut _ as *mut configuration::BridgeHeader)
            },
            HeaderType::CardBusBridge => unsafe {
                &mut *(&mut cfg_region[0] as *mut _ as *mut configuration::CardBusBridge)
            },
        };

        let mut bar = alloc::collections::BTreeMap::new();
        let mut bar_iter = 0..header.bar_count();
        while let Some(i) = bar_iter.next() {
            if let Some(info) = BarInfo::new(&mut *header, i) {
                if let configuration::register::BarType::Qword(_) = info.reg_info {
                    #[allow(unused_must_use)]
                    // the result is not important but the iter needs to be advanced
                    bar_iter.next();
                }
                bar.insert(i, info);
            }
        }

        let mut list = alloc::collections::BTreeMap::new();
        {
            // SAFETY: This is safe because header and cfg_region are for the same device
            let iter = unsafe { capabilities::CapabilityIter::new(header, &*cfg_region) };
            for cap in iter {
                list.insert(cap.id(), cap);
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
            cfg_region,
            capabilities: list,
        }
        .into()
    }

    /// Returns the functions PCI address
    pub fn address(&self) -> DeviceAddress {
        self.address
    }

    /// Returns the functions header type
    pub fn dev_type(&self) -> HeaderType {
        self.dev_type
    }

    fn is_multi_fn(&self) -> bool {
        self.header.is_multi_fn()
    }

    /// Returns the header region of the device
    fn get_config_region(&mut self) -> &dyn PciHeader {
        self.header
    }

    /// Returns the devices class subclass and type id
    pub fn class(&self) -> [u8; 3] {
        self.class
    }

    pub fn get_bar(&self, id: u8) -> Option<BarInfo> {
        Some(self.bar.get(&id)?.clone())
    }

    pub fn capability(
        &mut self,
        id: capabilities::CapabilityId,
    ) -> Option<&mut capabilities::CapabilityPointer> {
        self.capabilities.get_mut(&id)
    }

    pub fn get_cap_structure_mut<'a>(
        &'a mut self,
        id: capabilities::CapabilityId,
    ) -> Option<alloc::boxed::Box<dyn capabilities::Capability + 'a>> {
        match id {
            capabilities::CapabilityId::Null => None, // but why? Null is never stored
            capabilities::CapabilityId::PciPowerManagement => unimplemented!(),
            capabilities::CapabilityId::Agp => unimplemented!(),
            capabilities::CapabilityId::Vpd => unimplemented!(),
            capabilities::CapabilityId::SlotId => unimplemented!(),
            capabilities::CapabilityId::Msi => Some(alloc::boxed::Box::new(
                capabilities::msi::MessageSigInt::try_from(self).ok()?,
            )),
            capabilities::CapabilityId::CompactPciHotSwap => unimplemented!(),
            capabilities::CapabilityId::PciX => unimplemented!(),
            capabilities::CapabilityId::HyperTransport => unimplemented!(),
            capabilities::CapabilityId::VendorSpecific => unimplemented!(),
            capabilities::CapabilityId::DebugPort => unimplemented!(),
            capabilities::CapabilityId::CompactPCi => unimplemented!(),
            capabilities::CapabilityId::PciHotPlug => unimplemented!(),
            capabilities::CapabilityId::PciBridgeSubsystemVendorId => unimplemented!(),
            capabilities::CapabilityId::Agp8x => unimplemented!(),
            capabilities::CapabilityId::SecureDevice => unimplemented!(),
            capabilities::CapabilityId::PciExpress => unimplemented!(),
            capabilities::CapabilityId::MsiX => Some(alloc::boxed::Box::new(
                capabilities::msi::MessageSignaledIntX::try_from(self).ok()?,
            )),
            capabilities::CapabilityId::SataDataIndexConfig => unimplemented!(),
            capabilities::CapabilityId::AdvancedFeatures => unimplemented!(),
            capabilities::CapabilityId::EnhancedAllocation => unimplemented!(),
            capabilities::CapabilityId::FlatteningPortalBridge => unimplemented!(),
            capabilities::CapabilityId::Reserved(_) => unimplemented!(),
        }
    }

    /// This fn configures the PCI function interrupts. Interrupt handlers will be directed to wake
    /// `tid` and push a message onto `queue`. The exact number of interrupt vectors can be set using
    /// `override_count`. `override_count` is intended for driver that know that a function will use
    /// fewer than the requested number of vectors.
    ///
    /// The message pushed to `queue` depends on the mechanism this fn configured the function to use.
    ///  - If the function was configured with MSI the message is the vector number of the interrupt.
    ///  - If the function was configured with MSI-X the message *should* be the vector number. If
    /// the interrupts are coalesced some messages will be repeated. The exact messages will be
    /// returned by the return value.
    ///  - If the function was configured with Legacy Interrupts the message is undefined. The number
    /// of messages is the number of times the interrupt was requested
    ///
    /// When `override_count == Some(_)` this fn will mask ignored interrupts when available
    ///
    /// # Panics
    ///
    /// This fn will panic if the value within `override_count` is greater than 2048
    ///
    /// # Safety
    ///
    /// The caller must ensure that any vector above `override_count` will never be triggered
    pub unsafe fn cfg_interrupts(
        &mut self,
        queue: crate::task::InterruptQueue,
        override_count: Option<u16>,
    ) -> (
        CfgIntResult,
        Option<alloc::vec::Vec<crate::interrupts::InterruptIndex>>,
    ) {
        assert!(override_count.unwrap_or(0) < 2048);

        if let Some(_) = self.capabilities.get(&capabilities::CapabilityId::MsiX) {
            let ret = self.cfg_msi_x(queue.clone(), override_count);
            if ret.0.success() {
                debug_assert!(ret.1.is_some()); // driver did not properly acquire resource
                return ret;
            }
        }

        if let Some(_) = self.capabilities.get(&capabilities::CapabilityId::Msi) {
            // check if override count exceeds max vectors for msi
            let oc = {
                if let Some(n) = override_count {
                    if n > 32 {
                        None
                    } else {
                        Some(n)
                    }
                } else {
                    None
                }
            };

            let ret = self.cfg_msi(queue.clone(), oc);
            if ret.0.success() {
                debug_assert!(ret.1.is_some()); // driver did not properly acquire resource
                return ret;
            }
        }

        self.cfg_legacy_int(queue)
    }

    /// Configures MSI-X for the device function.
    /// If there are not enough free interrupt vectors they will be reallocated to vectors in use by
    /// this function. The expected message for each vector is given in the return value.
    /// When this function raises an interrupt the task given in `tid` will be woken and a message
    /// will be passed onto `queue`.
    ///
    /// When `override_count` is `Some(count)` and the value of `count` is lower than the number of
    /// vectors that this function requests, Then all vectors above `count` will not be allocated.
    /// All vectors that are ignored will be masked
    ///
    /// # Panics
    ///
    /// This fn will panic if `override_count.unwrap_or(0) > 2048`
    fn cfg_msi_x(
        &mut self,
        queue: crate::task::InterruptQueue,
        override_count: Option<u16>,
    ) -> (
        CfgIntResult,
        Option<alloc::vec::Vec<crate::interrupts::InterruptIndex>>,
    ) {
        use capabilities::msi;
        assert!(override_count.unwrap_or(0) < 2048);

        let mut msi = {
            match capabilities::msi::MessageSignaledIntX::try_from(self) {
                Ok(r) => r,
                Err(_) => return (CfgIntResult::Failed, None),
            }
        };

        //todo handle too many vectors
        let vectors = {
            let ts = msi.get_ctl().table_size();
            if let Some(n) = override_count {
                ts.min(n)
            } else {
                ts
            }
        };

        // get vector count
        let ihr = &crate::interrupts::vector_tables::IHR;
        ihr.lock();
        let free_count = ihr.get_free();

        // determine how many IRQ vectors to use
        let irq_count;
        if free_count as u16 > vectors {
            irq_count = vectors;
        } else {
            irq_count = 1;
        }

        // reserve IRQs
        let mut reserved_irq = alloc::vec::Vec::with_capacity(irq_count as usize);
        for _ in 0..irq_count {
            // todo allow different priority
            if let Some(n) = crate::interrupts::reserve_single(0) {
                reserved_irq.push(n);
            } else {
                if reserved_irq.len() == 0 {
                    return (CfgIntResult::Failed, None);
                } else {
                    break;
                }
            }
        }

        // set handlers
        for (i, n) in reserved_irq.iter().enumerate() {
            unsafe {
                crate::interrupts::alloc_irq(*n, queue.clone(), i as u64)
                    .expect("Failed to allocate reserved IRQ");
            }
        }

        let mut ret = alloc::vec::Vec::with_capacity(vectors as usize);
        for (i, e) in msi.get_vec_table().iter_mut().enumerate() {
            let loc_msg = i % reserved_irq.len();
            let i_vec = reserved_irq[loc_msg];
            e.set_entry(
                msi::InterruptAddress::new(msi::get_next_msi_affinity()),
                msi::InterruptMessage::new(i_vec, msi::InterruptDeliveryMode::Fixed, false, false),
            );
            e.mask(false);
            ret.push(crate::task::InterruptMessage(loc_msg as u64));
        }

        if let Some(count) = override_count {
            let t_count = msi.get_ctl().table_size();
            if count < t_count {
                for v in count..t_count {
                    msi.get_vec_table()[v].mask(true)
                }
            }
        }

        (
            CfgIntResult::SetMsiX(ret),
            Some(reserved_irq.iter().map(|n| (*n).into()).collect()),
        )
    }

    /// Configures MSI for the device function.
    /// If enough interrupt vectors cannot be located the device functions interrupts will be coalesced.
    /// Interrupts will cause the task with the id `tid` to be woken and ad a message will be pushed
    /// onto the Interrupt queue.
    /// The caller should ensure that the `queue` contains enough space for all messages that will
    /// be passed to it before being processed.
    /// Messages will contain the device functions vector number.
    ///
    /// If `override_count` is greater than the requested number of interrupts it will be ignored.
    /// If `override_count` number of free IRQs cannot be located they will be coalesced into the
    /// next lowest power of two.
    ///
    /// On success this fn will return [CfgIntResult::SetMsi]. [CfgIntResult::Failed] is returned
    /// when there are no IRQs free.
    ///
    /// # Panics
    ///
    /// This fn will panic if `override_count > Some(32)`
    ///
    /// # Safety
    ///
    /// This function can be considered safe if `override_count == None`.
    /// If `override_count == Some(_)` The caller must ensure that the device function will **never**
    /// raise an interrupt above this value  
    unsafe fn cfg_msi(
        &mut self,
        queue: crate::task::InterruptQueue,
        override_count: Option<u16>,
    ) -> (
        CfgIntResult,
        Option<alloc::vec::Vec<crate::interrupts::InterruptIndex>>,
    ) {
        use capabilities::msi;
        assert!(override_count.unwrap_or(0) < 32);

        let mut msi = match msi::MessageSigInt::try_from(self) {
            Ok(r) => r,
            Err(_) => return (CfgIntResult::Failed, None),
        };

        let req_vec = msi
            .get_control()
            .requested_vectors()
            .min(override_count.unwrap_or(u8::MAX as u16) as u8);

        // locate vectors
        let (irq, count);
        {
            let mut use_vectors = req_vec;
            // Uses a loop because of MP

            loop {
                match crate::interrupts::reserve_irq(0, use_vectors) {
                    Ok(n) => {
                        count = use_vectors;
                        irq = n;
                        break;
                    }
                    Err(n) => {
                        use_vectors = n.next_power_of_two() >> 1;
                        assert!(use_vectors >= 32);
                        if use_vectors == 0 {
                            return (CfgIntResult::Failed, None);
                        }
                    }
                }
            }
        };

        // set handler is IHR
        let mut irqs = alloc::vec::Vec::with_capacity(count as usize);
        for (i, irq) in (irq..irq + count).enumerate() {
            x86_64::instructions::interrupts::without_interrupts(|| {
                crate::interrupts::alloc_irq(irq, queue.clone(), i as u64)
                    .expect("Failed to allocate reserved IRQ")
            });
            irqs.push(irq.into());
        }

        msi.set_interrupt(
            msi::InterruptAddress::new(msi::get_next_msi_affinity()),
            msi::InterruptMessage::new(irq, msi::InterruptDeliveryMode::Fixed, false, false),
        );

        // Mask bits if available
        if let Some(oc) = override_count {
            if let Some(mut mask) = msi.mask {
                let mut add_mask = 0;

                // this can use the set method but this is faster
                for vec in (oc as u8)..count {
                    add_mask |= 1 << vec
                }
                let t = mask.inner.read();
                add_mask |= t;
                mask.inner.write(add_mask);
            }
        }

        (CfgIntResult::SetMsi(count, req_vec), Some(irqs))
    }

    #[allow(unused_variables)]
    unsafe fn cfg_legacy_int(
        &mut self,
        queue: crate::task::InterruptQueue,
    ) -> (
        CfgIntResult,
        Option<alloc::vec::Vec<crate::interrupts::InterruptIndex>>,
    ) {
        unimplemented!()
    }

    /// Runs a self test. If the function does not support self test returns `Err(())`
    pub fn run_bist(&mut self) -> Result<(), ()> {
        self.header.self_test()
    }

    /// Attempts to get the result of a self test.
    /// This fn will return `None` is a test is still running. If no test is running the result code is returned.
    /// If `Ok(_)` is returned the function is working correctly.
    /// If `Err(_)` the device is not working correctly and the caller should take action.
    /// The exact values of the return codes are device specific.
    /// If a test has not ended after 2 seconds the device function can be considered failed.
    ///
    /// note: THe built in self test does not generate an interrupt and this function must therefore
    /// be polled.
    pub fn check_self_test(&self) -> Option<Result<u8, u8>> {
        self.header.check_test()
    }
}

// Because of how BARS need to be used they are interacted with using DeviceControl not their own
// type. So BAR stuff will get its own impl block
impl DeviceControl {
    /// Sets the given BAR to use the given physical address
    ///
    /// # Panics
    ///
    /// This fn will panic if `region` cannot be translated into a physical address or if `region`
    /// is an incorrect size or alignment for the given bar.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `region`
    /// - is a valid address
    /// - is contiguous in physical memory
    /// - uses an appropriate caching mode as indicated by the BAR (Uncacheable/Write Through/Write Combining)
    /// - must contain data that will not cause device errors
    /// - is within the memory range configured by any bridges between the function and the CPU
    pub unsafe fn configure_bar<T>(
        &mut self,
        id: u8,
        region: core::ptr::NonNull<[T]>,
    ) -> Result<BarInfo, BarError> {
        use configuration::register::BarType;
        use x86_64::structures::paging::Mapper;
        let p_addr = {
            let ptr = x86_64::VirtAddr::from_ptr(region.as_ptr() as *const u8);
            crate::mem::SYS_MAPPER
                .get()
                .translate_page(x86_64::structures::paging::Page::<
                    x86_64::structures::paging::Size4KiB,
                >::containing_address(ptr))
                .expect("Failed to translate page")
                .start_address()
                .as_u64()
        };

        if self.header.bar_count() <= id {
            return Err(BarError::IllegalId);
        }
        let b = *self.bar.get(&id).ok_or(BarError::IdInvalid)?;

        if p_addr & (b.align - 1) != 0 {
            panic!("Addr not aligned");
        }
        if (region.len() * core::mem::size_of::<T>()) as u64 > b.align {
            panic!("Region to large");
        }

        // will not panic erroneous id's are already checked
        let read_back = match b.reg_info {
            BarType::DwordIO => return Err(BarError::BarIsIO),
            BarType::Dword(_) => self.header.bar(id).unwrap().write(p_addr as u32) as u64,
            BarType::Qword(_) => self.header.bar_long(id).unwrap().write(p_addr),
        };

        let bar = self.bar.get_mut(&id).unwrap();
        bar.addr = read_back;
        let saved_region = {
            let ptr = region.as_ptr() as *mut u8;
            let len = region.len() * core::mem::size_of::<T>();
            // will not panic `ptr` is from existing ref
            core::ptr::NonNull::new(core::slice::from_raw_parts_mut(ptr, len)).unwrap()
        };
        bar.virt_region.0 = Some(saved_region);

        Ok(*bar)
    }

    /// Attempts to validate the information stored within the given BAR id.
    /// Checks are done in the following order
    /// 1. Checks that `id` is legal
    /// 2. Checks the validity if the BAR id
    /// 3. Checks that it is not I/O mapped
    /// 4. Checks that the physical addresses match
    /// 5. Checks that the Virt address is mapped to the found physical address
    ///
    /// see [BarError] for more information
    pub fn validate_bar(&mut self, id: u8) -> Result<(), BarError> {
        use configuration::register::BarType;
        use x86_64::structures::paging::Mapper;
        if id > self.header.bar_count() {
            return Err(BarError::IllegalId);
        }
        let bar = *self.bar.get(&id).ok_or(BarError::IdInvalid)?;
        // panic sources and unsafe blocks are checked by match
        let p_addr_reg = match bar.reg_info {
            BarType::DwordIO => return Err(BarError::BarIsIO),
            BarType::Dword(_) => self.header.bar(id).unwrap().read() as u64,
            BarType::Qword(_) => unsafe { self.header.bar_long(id).unwrap().read() },
        };

        if bar.addr != p_addr_reg {
            return Err(BarError::PhysAddrMismatch);
        }

        let found_addr = {
            // SAFETY: this is safe because this does not dereference the address
            let addr = unsafe { bar.region_start() }.ok_or(BarError::VirtAddressMismatch)?;
            let page = x86_64::structures::paging::Page::<x86_64::structures::paging::Size4KiB>::containing_address(x86_64::VirtAddr::from_ptr(addr));
            match crate::mem::SYS_MAPPER.get().translate_page(page) {
                Ok(a) => a.start_address().as_u64(),
                Err(_) => return Err(BarError::VirtAddressMismatch), // todo create better translate fn
            }
        };

        if found_addr == p_addr_reg {
            Ok(())
        } else {
            Err(BarError::VirtAddressMismatch)
        }
    }

    /// Automatically locate the physical region and set the pointer internally. This fn may result
    /// in memory leaks if the memory region was allocated by the system.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the physical region is located within Reserved memory
    pub unsafe fn locate_region(&mut self, id: u8) -> Result<BarInfo, BarError> {
        use core::alloc::Allocator;
        self.locate_bar(id)?;

        // locate_bar will return Err if this call will fail
        let b = self.bar.get_mut(&id).unwrap();
        let alloc = MmioAlloc::new(b.addr as usize);

        // layout will not panic, allocate might
        let region = alloc
            .allocate(core::alloc::Layout::from_size_align(b.align as usize, 1).unwrap())
            .expect("System ran out of memory");
        b.virt_region.0 = Some(region);
        Ok(*b)
    }

    /// Validates that the given bar exists
    /// This should called before other bar functions to check that the bar is available
    pub fn locate_bar(&self, id: u8) -> Result<(), BarError> {
        if id > self.header.bar_count() {
            return Err(BarError::IllegalId);
        }
        self.bar.get(&id).ok_or(BarError::IdInvalid)?;
        Ok(())
    }

    pub fn fetch_region(&mut self, id: u8) -> Result<*mut [u8], BarError> {
        self.validate_bar(id)?;
        let bar = self.bar.get(&id).unwrap().virt_region.0.unwrap().as_ptr();

        Ok(bar)
    }
}

#[derive(Debug)]
pub enum BarError {
    /// Returned When a requested id is higher than the function contains.
    IllegalId,
    /// The bar is unusable. Either it is used as the high half of a 64 bit bar or is unused by the
    /// function.
    IdInvalid,
    /// Returned when an action is requested but cannot (or should not) be preformed because the bar
    /// is mapped to the I/O bus
    BarIsIO,
    /// Returned when the address in the BAR and its associated `BarInfo` contain differing addresses
    PhysAddrMismatch,
    /// Returned when the virtual address within the `BarInfo` and the physical address do not match
    /// or when it is not set.
    VirtAddressMismatch,
}

/// Caches information about Base Address Registers
/// - `id` is the numerical value of the BAR within the header.
/// - `align` is both the size and alignment of the the register and all allocations used for this BAR
/// should use this as both size and alignment
/// - `reg_info` stores info about the type of bar this is. Memory variants contain a bool
/// identifying whether the BAR is "Prefetchable". When the region is prefetchable the region's
/// cache mode should be either "Write Through" or "Write Combining" which is preferred
/// - `virt_Region` This is a pointer (when known) to the virtual address that the bar is mapped to.
#[derive(Debug, Copy, Clone)]
pub struct BarInfo {
    id: u8,
    align: u64,
    reg_info: configuration::register::BarType,
    addr: u64,
    virt_region: BarPtr,
}

#[derive(Debug, Copy, Clone)]
struct BarPtr(Option<core::ptr::NonNull<[u8]>>);

unsafe impl Sync for BarPtr {}
unsafe impl Send for BarPtr {}
impl Default for BarPtr {
    fn default() -> Self {
        Self(None)
    }
}

impl BarInfo {
    /// Creates a new `Self` from a pci header
    fn new<H: PciHeader + ?Sized>(header: &mut H, id: u8) -> Option<Self> {
        use configuration::register::BarType;
        // SAFETY: This is safe because this will disable the functions ability to respond to
        // memory/io space interactions and is restored before exiting the fn
        unsafe {
            header.update_control(
                configuration::register::CommandRegister::MEMORY_SPACE
                    | configuration::register::CommandRegister::IO_SPACE,
                false,
            )
        };
        let bar = header.bar(id)?;
        let reg_info = bar.bar_type();

        match reg_info {
            BarType::DwordIO | BarType::Dword(_) => {
                let align = unsafe { bar.alignment() as u64 };
                if align == 0 {
                    unsafe {
                        header.update_control(
                            configuration::register::CommandRegister::MEMORY_SPACE
                                | configuration::register::CommandRegister::IO_SPACE,
                            true,
                        )
                    };
                    return None;
                }
                let b = Self {
                    id,
                    // SAFETY: This is safe because this is fn is only called when the device is initialized into the kernel
                    align,
                    reg_info,
                    addr: bar.read() as u64,
                    virt_region: BarPtr::default(),
                }
                .into();

                unsafe {
                    header.update_control(
                        configuration::register::CommandRegister::MEMORY_SPACE
                            | configuration::register::CommandRegister::IO_SPACE,
                        true,
                    )
                };
                b
            }

            BarType::Qword(_) => {
                // SAFETY: this is safe because `bar` is not referenced within this arm
                let bar_long = unsafe { header.bar_long(id)? };
                let align = unsafe { bar_long.alignment() } as u64;
                if align == 0 {
                    unsafe {
                        header.update_control(
                            configuration::register::CommandRegister::MEMORY_SPACE
                                | configuration::register::CommandRegister::IO_SPACE,
                            true,
                        )
                    };
                    return None;
                }
                let b = Self {
                    id,
                    align,
                    reg_info,
                    addr: bar_long.read(),
                    virt_region: BarPtr::default(),
                }
                .into();

                unsafe {
                    header.update_control(
                        configuration::register::CommandRegister::MEMORY_SPACE
                            | configuration::register::CommandRegister::IO_SPACE,
                        true,
                    )
                };
                b
            }
        }
    }

    /// Returns a pointer to the start fo the mapped region
    ///
    /// # Safety
    ///
    /// This should not be dereferenced without a lock to its associated [DeviceControl]
    pub unsafe fn region_start(&self) -> Option<*const u8> {
        let t = self.virt_region.0?;
        Some(t.as_ptr() as *const u8)
    }

    pub fn align(&self) -> u64 {
        self.align
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

// todo: Should this be in another module? if so probably system
struct HwMap<K: Ord, V> {
    lock: crate::kernel_structures::Mutex<()>,
    map: core::cell::UnsafeCell<alloc::collections::BTreeMap<K, V>>,
}

impl<K: Ord, V> HwMap<K, V> {
    // SAFETY: *self.map.get() is safe as long as a self.lock.lock() exists above it.
    // This prevents synchronisation errors. derefs are safe because the inner value is initialized
    fn new() -> Self {
        Self {
            lock: crate::kernel_structures::Mutex::new(()),
            map: core::cell::UnsafeCell::new(alloc::collections::BTreeMap::new()),
        }
    }

    fn insert(&self, key: K, value: V) -> Option<V> {
        self.lock.lock();
        unsafe { &mut *self.map.get() }.insert(key, value)
    }

    fn get(&self, key: &K) -> Option<&V> {
        self.lock.lock();
        unsafe { &*self.map.get() }.get(key)
    }
}

unsafe impl<K: Sync + Ord, V: Sync> Sync for HwMap<K, V> {}

pub enum CfgIntResult {
    /// Indicates the function was configured with MSI
    /// Returns the number of vectors allocated and number of vectors requested by the function `(alloc,req)`.
    /// A driver can use the values in this to know what messages to expect from interrupts.
    SetMsi(u8, u8),

    /// Indicates the function was configured with MSI-X.
    /// The contained Vec contains message for each interrupt vector.
    /// the index into the array is the interrupt vector as present in the function. the contained
    /// value is the message bound to it.
    /// Coalesced interrupts will have repeated messages. the driver must handle these appropriately
    SetMsiX(alloc::vec::Vec<crate::task::InterruptMessage>),

    /// Indicates that the device is configured with legacy interrupts.
    /// In this mode the Interrupt messages only indicate mow many times this function has requested
    /// an interrupt.
    /// All messages will be 1.
    SetLegacy,

    /// Indicates that the kernel failed to configure the function.
    Failed,
}

impl CfgIntResult {
    pub fn success(&self) -> bool {
        if let Self::Failed = self {
            false
        } else {
            true
        }
    }
}

impl crate::task::int_message_queue::MessageCfg for CfgIntResult {
    fn count(&self) -> usize {
        match self {
            CfgIntResult::SetMsi(a, _) => *a as usize,
            CfgIntResult::SetMsiX(v) => v.len(),
            CfgIntResult::SetLegacy => 1,
            CfgIntResult::Failed => panic!(
                "Called MsgConfig::count() on {}::Failed",
                core::any::type_name::<Self>()
            ),
        }
    }

    #[track_caller]
    fn message(&self, vector: usize) -> crate::task::InterruptMessage {
        if vector < self.count() {
            match self {
                CfgIntResult::SetMsi(_, _) => crate::task::InterruptMessage(vector as u64),
                CfgIntResult::SetMsiX(a) => a[vector],
                CfgIntResult::SetLegacy => crate::task::InterruptMessage(1),
                CfgIntResult::Failed => panic!(
                    "Called MsgConfig::message() on {}::Failed",
                    core::any::type_name::<Self>()
                ),
            }
        } else {
            panic!(
                "Attempted to request message for vector: {vector:}, at {}",
                core::panic::Location::caller()
            )
        }
    }
}

#[derive(Clone, Debug)]
pub struct PciResourceContainer {
    addr: DeviceAddress,
    device: alloc::sync::Arc<spin::Mutex<DeviceControl>>,
}

impl PciResourceContainer {
    fn new(addr: DeviceAddress, device: alloc::sync::Arc<spin::Mutex<DeviceControl>>) -> Self {
        Self { addr, device }
    }

    /// Returns an [alloc::sync::Arc] containing the [DeviceControl]
    pub fn dev(&self) -> alloc::sync::Arc<spin::Mutex<DeviceControl>> {
        self.device.clone()
    }

    /// Returns the bus address of the function
    pub fn addr(&self) -> DeviceAddress {
        self.addr
    }

    /// Returns the function class
    pub fn class(&self) -> u32 {
        // todo change PCI class to u32 from [u8;3]
        let mut c = [0u8; 4];
        c[0..3].copy_from_slice(&self.device.lock().header.class()[..]);

        u32::from_le_bytes(c)
    }

    /// Returns the vendor and device id's.
    /// Note the order of id's are reversed from how they appear in the PCI specification
    pub fn dev_id(&self) -> (u16, u16) {
        let l = self.device.lock();
        (l.header.vendor(), l.header.device())
    }

    /// Returns the [HeaderType] of the device. Most drivers will want [HeaderType::Generic]
    pub fn header_type(&self) -> HeaderType {
        self.device.lock().header.header_type()
    }
}

impl super::driver_if::ResourceId for PciResourceContainer {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }

    fn bus_name(&self) -> &str {
        "pci"
    }
}

impl core::fmt::Display for PciResourceContainer {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} bus addr: ", core::any::type_name::<Self>()).unwrap();
        core::fmt::Display::fmt(&self.addr, f)
    }
}
