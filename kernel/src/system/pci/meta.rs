use crate::system::pci::DeviceAddress;
use core::cmp::Ordering;
use core::fmt::Formatter;

/// A struct containing metadata bout PCI devices that can then be used for lookups.
#[derive(Debug, Copy, Clone)]
pub struct MetaInfo {
    pub addr: DeviceAddress,
    pub device: [u16; 2],
    pub class: [u8; 3],
}

impl MetaInfo {
    /// Creates a `Self` using an illegal device / vendor number, distinguishing it from properly
    /// initialized instances of `Self`
    pub fn new() -> Self {
        Self {
            addr: DeviceAddress::new(0, 0, 0, 0),
            device: [u16::MAX; 2], // reading device/vendor cannot be #FFFF so this value can indicate self in uninitialized
            class: [u8::MAX; 3],
        }
    }
}

impl core::fmt::Display for MetaInfo {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} [{},{}]\n", self.addr, self.device[0], self.device[1]).expect("?");
        write!(
            f,
            "class: {}, subclass: {} interface: {} ",
            self.class[2], self.class[1], self.class[0]
        )
    }
}

impl From<&super::DeviceControl> for MetaInfo {
    fn from(value: &super::DeviceControl) -> Self {
        Self {
            addr: value.address(),
            device: [value.vendor, value.device],
            class: value.class,
        }
    }
}

impl Eq for MetaInfo {}

impl PartialEq<Self> for MetaInfo {
    fn eq(&self, other: &Self) -> bool {
        self.addr.eq(&other.addr)
    }
}

impl PartialOrd<Self> for MetaInfo {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.addr.partial_cmp(&other.addr)
    }
}

impl Ord for MetaInfo {
    fn cmp(&self, other: &Self) -> Ordering {
        self.addr.cmp(&other.addr)
    }
}
