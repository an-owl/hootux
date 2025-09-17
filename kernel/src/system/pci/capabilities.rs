pub mod msi;

pub trait Capability<'a> {
    fn id(&self) -> CapabilityId;

    fn boxed(self) -> alloc::boxed::Box<dyn core::any::Any + 'a>;

    fn any_mut(&'a mut self) -> &'a mut (dyn core::any::Any + 'a);
}

#[derive(Debug, Copy, Clone, PartialEq, Ord, PartialOrd, Eq)]
#[repr(u8)]
pub enum CapabilityId {
    Null = 0,
    PciPowerManagement,
    Agp,
    Vpd,
    SlotId,
    Msi,
    CompactPciHotSwap,
    PciX,
    HyperTransport,
    VendorSpecific,
    DebugPort,
    CompactPCi,
    PciHotPlug,
    PciBridgeSubsystemVendorId,
    Agp8x,
    SecureDevice,
    PciExpress,
    MsiX,
    SataDataIndexConfig,
    AdvancedFeatures,
    EnhancedAllocation,
    FlatteningPortalBridge,
    Reserved(u8),
}

impl From<u8> for CapabilityId {
    fn from(value: u8) -> Self {
        match value {
            0x0 => Self::Null,
            0x1 => Self::PciPowerManagement,
            0x2 => Self::Agp,
            0x3 => Self::Vpd,
            0x4 => Self::SlotId,
            0x5 => Self::Msi,
            0x6 => Self::CompactPciHotSwap,
            0x7 => Self::PciX,
            0x8 => Self::HyperTransport,
            0x9 => Self::VendorSpecific,
            0xa => Self::DebugPort,
            0xb => Self::CompactPCi,
            0xc => Self::PciHotPlug,
            0xd => Self::PciBridgeSubsystemVendorId,
            0xe => Self::Agp8x,
            0xf => Self::SecureDevice,
            0x10 => Self::PciExpress,
            0x11 => Self::MsiX,
            0x12 => Self::SataDataIndexConfig,
            0x13 => Self::AdvancedFeatures,
            0x14 => Self::EnhancedAllocation,
            0x15 => Self::FlatteningPortalBridge,
            e => Self::Reserved(e),
        }
    }
}

/// This struct is an iterator over capabilities of a PCI Configuration region.
pub struct CapabilityIter<'a> {
    region: &'a [u8; 4096],
    next: Option<usize>,
}

impl<'a> CapabilityIter<'a> {
    /// Creates a new Capability iter for the given device
    ///
    /// # Safety
    ///
    /// The caller must ensure that `header` and `region` are for the same device
    pub unsafe fn new(
        header: &'a dyn super::configuration::PciHeader,
        region: &'a [u8; 4096],
    ) -> Self {
        let cap = if let Some(n) = header.capabilities() {
            Some(n as usize)
        } else {
            None
        };
        Self {
            region: &region,
            next: cap,
        }
    }
}

impl<'a> Iterator for CapabilityIter<'a> {
    type Item = CapabilityPointer;

    fn next(&mut self) -> Option<Self::Item> {
        let next = self.next?;
        #[allow(invalid_reference_casting)]
        let t = unsafe { &*(&self.region[next] as *const _ as *const CapabilityHeader) };
        let ret = CapabilityPointer {
            id: CapabilityId::from(t.id),
            offset: next as u8,
        };

        if let CapabilityId::Reserved(c) = ret.id {
            log::warn!(r#" \---/  \ | /"#);
            log::warn!(r#"{{\OvO/}}  OO :)"#);
            log::warn!(r#"'/_o_\' / | \"#);
            log::warn!("Mmm, Found a tasty bug: PCI device function has reserved capability {c}");
        }

        if t.next == 0 {
            self.next = None;
        } else {
            self.next = Some(t.next as usize);
        }
        Some(ret)
    }
}

#[derive(Copy, Clone)]
pub struct CapabilityPointer {
    id: CapabilityId,
    offset: u8,
}

impl CapabilityPointer {
    pub fn id(&self) -> CapabilityId {
        self.id
    }
    pub fn offset(&self) -> u8 {
        self.offset
    }
}

#[repr(C)]
struct CapabilityHeader {
    id: u8,
    next: u8,
}
