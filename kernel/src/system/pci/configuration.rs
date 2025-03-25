pub(super) mod register;

use core::any::Any;
use register::*;

pub trait PciHeader: Any + Sync + Send {
    /// Returns the header type
    fn header_type(&self) -> HeaderType;

    /// Returns the device Vendor id
    fn vendor(&self) -> u16;

    /// Returns the device id
    fn device(&self) -> u16;

    /// Returns the Number of BARs in this header
    fn bar_count(&self) -> u8;

    /// Returns a reference to a BarRegister as long as it exists
    fn bar(&mut self, bar_num: u8) -> Option<&mut BaseAddressRegister>;

    /// Returns a reference to a bar register as long as it exists and is 64bit
    ///
    /// # Safety
    ///
    /// This fn is unsafe because it allows mutable aliasing of memory. The caller should ensure
    /// that `self.bar(bar_num + 1)` is not referenced.
    unsafe fn bar_long(&mut self, bar_num: u8) -> Option<&mut BarRegisterLong> {
        unsafe {
            let bar = self.bar(bar_num)?;
            if let BarType::Qword(_) = bar.bar_type() {
                // Cast is allowed because register is type qword
                Some(&mut *(bar as *mut BaseAddressRegister as *mut BarRegisterLong))
            } else {
                None
            }
        }
    }

    /// Returns the Class,Subclass and ProfIf of the device.
    /// The layout is the original layout as read from memory
    fn class(&self) -> [u8; 3];

    fn is_multi_fn(&self) -> bool;

    fn capabilities(&self) -> Option<u8> {
        None
    }

    /// Sets all flags given in `flags` to `state`.
    ///
    /// # Safety
    ///
    /// This fn is unsafe because if modifies a functions response to some actions. The caller must
    /// ensure that these actions will not cause UB
    unsafe fn update_control(&mut self, flags: CommandRegister, value: bool) -> CommandRegister;

    /// Runs the built in self test for the device. If BIST is not available returns `Err(())`.  
    fn self_test(&mut self) -> Result<(), ()>;

    /// Checks the result of a self test
    fn check_test(&self) -> Option<Result<u8, u8>>;

    fn as_any(&self) -> &dyn Any;
}

/// Common header shared between all PCI header types.
/// A common header may be safely cast into the type indicated in the `header_type` register.
#[repr(C)]
pub struct CommonHeader {
    _pin: core::marker::PhantomPinned,
    vendor_id: u16,
    device_id: u16,
    command: CommandRegister,
    status: StatusRegister,
    revision: u8,
    class: [u8; 3],
    cache_line_size: u8,
    latency_timer: u8,
    header_type: HeaderTypeRegister,
    self_test: SelfTestRegister,
}

impl PciHeader for CommonHeader {
    fn header_type(&self) -> HeaderType {
        self.header_type.get_type()
    }

    fn vendor(&self) -> u16 {
        self.vendor_id
    }

    fn device(&self) -> u16 {
        self.device_id
    }

    fn bar_count(&self) -> u8 {
        0
    }

    /// This header does not contain nay BARs
    fn bar(&mut self, _bar_num: u8) -> Option<&mut BaseAddressRegister> {
        None
    }

    fn class(&self) -> [u8; 3] {
        self.class.clone()
    }

    fn is_multi_fn(&self) -> bool {
        self.header_type.is_multiple_functions()
    }

    unsafe fn update_control(&mut self, flags: CommandRegister, value: bool) -> CommandRegister {
        unsafe {
            let mut c = core::ptr::read_volatile(&self.command);
            c.set(flags, value);
            core::ptr::write_volatile(&mut self.command, c);
            core::ptr::read_volatile(&self.command)
        }
    }

    fn self_test(&mut self) -> Result<(), ()> {
        self.self_test.run_test()
    }

    fn check_test(&self) -> Option<Result<u8, u8>> {
        self.self_test.get_err()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[repr(C)]
pub struct GenericHeader {
    common: CommonHeader,
    pub bar_0: BaseAddressRegister,
    pub bar_1: BaseAddressRegister,
    pub bar_2: BaseAddressRegister,
    pub bar_3: BaseAddressRegister,
    pub bar_4: BaseAddressRegister,
    pub bar_5: BaseAddressRegister,
    card_bus_info_ptr: u32,
    subsystem_vendor_id: u16,
    subsystem_id: u16,
    expansion_rom_base_addr: u32,
    capabilities_ptr: u32, // low byte only the rest is reserved
    _res: u32,
    int_line: u8,
    int_pin: InterruptPin,
    min_grant: u8,
    max_latency: u8,
}

impl GenericHeader {
    const HEADER_TYPE: HeaderType = HeaderType::Generic;
}

impl TryFrom<&mut CommonHeader> for &mut GenericHeader {
    type Error = HeaderType;

    fn try_from(value: &mut CommonHeader) -> Result<Self, Self::Error> {
        let header_type = value.header_type.get_type();

        if header_type == GenericHeader::HEADER_TYPE {
            Ok(unsafe { &mut *(value as *mut CommonHeader).cast() })
        } else {
            Err(header_type)
        }
    }
}

impl PciHeader for GenericHeader {
    fn header_type(&self) -> HeaderType {
        self.common.header_type()
    }

    fn vendor(&self) -> u16 {
        self.common.vendor()
    }

    fn device(&self) -> u16 {
        self.common.device()
    }

    fn bar_count(&self) -> u8 {
        6
    }

    fn bar(&mut self, bar_num: u8) -> Option<&mut BaseAddressRegister> {
        match bar_num {
            0 => Some(&mut self.bar_0),
            1 => Some(&mut self.bar_1),
            2 => Some(&mut self.bar_2),
            3 => Some(&mut self.bar_3),
            4 => Some(&mut self.bar_4),
            5 => Some(&mut self.bar_5),
            _ => None,
        }
    }

    fn class(&self) -> [u8; 3] {
        self.common.class()
    }

    fn is_multi_fn(&self) -> bool {
        self.common.is_multi_fn()
    }

    fn capabilities(&self) -> Option<u8> {
        if self
            .common
            .status
            .contains(StatusRegister::CAPABILITES_LIST)
        {
            Some(self.capabilities_ptr as u8)
        } else {
            None
        }
    }

    unsafe fn update_control(&mut self, flags: CommandRegister, value: bool) -> CommandRegister {
        unsafe { self.common.update_control(flags, value) }
    }

    fn self_test(&mut self) -> Result<(), ()> {
        self.common.self_test()
    }

    fn check_test(&self) -> Option<Result<u8, u8>> {
        self.common.check_test()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[repr(C)]
pub struct BridgeHeader {
    common: CommonHeader,
    pub bar_0: BaseAddressRegister,
    pub bar_1: BaseAddressRegister,
    primary_bus_number: u8,
    secondary_bus_number: u8,
    subordinate_bus_number: u8,
    secondary_latency_timer: u8,
    io_base: u8,
    io_limit: u8,
    secondary_status: u16,
    mem_base: u16,
    mem_limit: u16,
    prefetch_mem_base: u16,
    prefetch_mem_limit: u16,
    prefetch_base_upper: u32,
    prefetch_limit_upper: u32,
    io_base_upper: u16,
    io_limit_upper: u16,
    capabilities_ptr: u32, // low byte only the rest is reserved
    expansion_rom_base_addr: u32,
    int_line: u8,
    int_pin: InterruptPin,
    bridge_ctl: u16,
}

impl BridgeHeader {
    const HEADER_TYPE: HeaderType = HeaderType::Bridge;

    pub fn get_secondary_bus(&self) -> u8 {
        self.secondary_bus_number
    }
}

impl TryFrom<&mut CommonHeader> for &mut BridgeHeader {
    type Error = HeaderType;

    fn try_from(value: &mut CommonHeader) -> Result<Self, Self::Error> {
        let h = value.header_type();
        if h == BridgeHeader::HEADER_TYPE {
            Ok(unsafe { &mut *(value as *mut CommonHeader).cast() })
        } else {
            Err(h)
        }
    }
}

impl PciHeader for BridgeHeader {
    fn header_type(&self) -> HeaderType {
        self.common.header_type()
    }

    fn vendor(&self) -> u16 {
        self.common.vendor()
    }

    fn device(&self) -> u16 {
        self.common.device()
    }

    fn bar_count(&self) -> u8 {
        2
    }

    fn bar(&mut self, bar_num: u8) -> Option<&mut BaseAddressRegister> {
        match bar_num {
            0 => Some(&mut self.bar_0),
            1 => Some(&mut self.bar_1),
            _ => None,
        }
    }

    fn class(&self) -> [u8; 3] {
        self.common.class()
    }

    fn is_multi_fn(&self) -> bool {
        self.common.is_multi_fn()
    }

    fn capabilities(&self) -> Option<u8> {
        if self
            .common
            .status
            .contains(StatusRegister::CAPABILITES_LIST)
        {
            Some(self.capabilities_ptr as u8)
        } else {
            None
        }
    }

    unsafe fn update_control(&mut self, flags: CommandRegister, value: bool) -> CommandRegister {
        unsafe { self.common.update_control(flags, value) }
    }

    fn self_test(&mut self) -> Result<(), ()> {
        self.common.self_test()
    }

    fn check_test(&self) -> Option<Result<u8, u8>> {
        self.common.check_test()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[repr(C)]
pub struct CardBusBridge {
    common: CommonHeader,
    card_bus_base_address: u32,
    secondary_status: u16,
    _res0: u8,
    capabilities_ptr: u8,
    card_bus_latency_timer: u8,
    subordinate_bus: u8,
    card_bus: u8,
    pci_bus: u8,
    mem_base_addr_0: u32,
    mem_limit_0: u32,
    mem_base_addr_1: u32,
    mem_limit_1: u32,
    io_base_addr_0: u32,
    io_limit_0: u32,
    io_base_addr_1: u32,
    io_limit_1: u32,
    bridge_ctl: u16,
    int_pin: InterruptPin,
    int_line: u8,
    subsystem_vendor_id: u16,
    subsystem_device_id: u16,
    legacy_mode_base_address: u32, // 16 bit?
}

impl CardBusBridge {
    const HEADER_TYPE: HeaderType = HeaderType::CardBusBridge;
}

impl TryFrom<&mut CommonHeader> for &mut CardBusBridge {
    type Error = HeaderType;

    fn try_from(value: &mut CommonHeader) -> Result<Self, Self::Error> {
        let h = value.header_type();
        if h == CardBusBridge::HEADER_TYPE {
            Ok(unsafe { &mut *(value as *mut CommonHeader).cast() })
        } else {
            Err(h)
        }
    }
}

impl PciHeader for CardBusBridge {
    fn header_type(&self) -> HeaderType {
        self.common.header_type()
    }

    fn vendor(&self) -> u16 {
        self.common.vendor()
    }

    fn device(&self) -> u16 {
        self.common.device()
    }

    fn bar_count(&self) -> u8 {
        0
    }

    /// This header does nto contain any BARs
    fn bar(&mut self, _bar_num: u8) -> Option<&mut BaseAddressRegister> {
        None
    }

    fn class(&self) -> [u8; 3] {
        self.common.class()
    }

    fn is_multi_fn(&self) -> bool {
        self.common.is_multi_fn()
    }

    unsafe fn update_control(&mut self, flags: CommandRegister, value: bool) -> CommandRegister {
        unsafe { self.common.update_control(flags, value) }
    }

    fn self_test(&mut self) -> Result<(), ()> {
        self.common.self_test()
    }

    fn check_test(&self) -> Option<Result<u8, u8>> {
        self.common.check_test()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
