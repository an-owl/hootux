use usb_cfg::DescriptorType;

/// HID descriptor defined by the USB HID specification 1.11
///
/// This contains the HID version number country code and the type and size of optional & report
/// descriptors descriptors.
///
/// # Safety
///
/// This may not be constructed, references to self must point to data where `self..self + self.length`
/// is returned by the USB device with the tail intact.
#[repr(C, packed)]
#[derive(Debug)]
pub struct HidDescriptor {
    length: u8,
    class: DescriptorType,
    bcd_hid: u16,
    country_code: u8,
    num_descriptors: u8,
}

impl HidDescriptor {
    /// Returns a slice containing the optional descriptors
    pub fn optionals(&self) -> &[DescriptorDescriber] {
        let tail_len_bytes = self.length as usize - size_of::<Self>();
        let elements = tail_len_bytes / size_of::<DescriptorDescriber>();
        let tail_start: *const Self = &raw const *self;
        let tail_start = tail_start.cast();

        // SAFETY: Requirements must be upheld when creating the pointer to `self`
        // &self..&self.length is required to be owned by self.
        unsafe { core::slice::from_raw_parts(tail_start, elements) }
    }
}

impl usb_cfg::descriptor::Descriptor for HidDescriptor {
    fn length(&self) -> u8 {
        self.length
    }

    fn descriptor_type() -> DescriptorType {
        DescriptorType(0x21)
    }
}

#[repr(C, packed)]
pub struct DescriptorDescriber {
    pub id: u8,
    pub length: u16,
}

#[repr(u16)]
pub enum HidDescriptorRequestType {
    Hid = 0x21,
    Report = 0x22,
    /// Requests a physical descriptor, the inner value indicates which descriptor.
    /// An interface may have multiple physical descriptors, for different usages.
    /// When this is set to a non-existent descriptor the last physical descriptor from the endpoint will be returned.
    /// When set to `0` the request returns a special descriptor
    /// identifying the number of descriptor sets and their sizes.
    Physical(u8) = 0x23,
}

impl HidDescriptorRequestType {
    pub const SPECIAL_REQUEST: Self = Self::Physical(0);

    // Note: This is what core::mem::discriminant describes
    pub fn discriminant(&self) -> u8 {
        // SAFETY: Because `Self` is marked `repr(u8)`, its layout is a `repr(C)` `union`
        // between `repr(C)` structs, each of which has the `u8` discriminant as its first
        // field, so we can read the discriminant without offsetting the pointer.
        unsafe { *(&raw const *self).cast() }
    }
}

/// Returns a [usb_cfg::CtlTransfer] to fetch a HID descriptor from `endpoint`.
pub(crate) fn request_descriptor_command(
    request: HidDescriptorRequestType,
    interface: u8,
    length: u16,
) -> usb_cfg::CtlTransfer {
    use usb_cfg::*;
    let value_low = if let HidDescriptorRequestType::Physical(value) = request {
        value
    } else {
        0
    };

    CtlTransfer::new(
        Recipient::Interface,
        RequestType::Standard,
        true,
        RequestCode::GetDescriptor as u8,
        u16::from_le_bytes([value_low, request.discriminant()]),
        interface as u16,
        length,
    )
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum UsagePage {
    Undefined = 0,
    GenericDesktop = 1,
    SimulationControls = 2,
    VrControls = 3,
    SportsControls = 4,
    GameControls = 5,
    GenericDeviceControls = 6,
    KeyboardKeypad = 7,
    LedPage = 8,
    ButtonPage = 9,
    Ordinal = 0xa,
    Telephony = 0xb,
    Consumer = 0xc,
    Digitizers = 0xd,
    Haptics = 0xe,
    PhysicalInputDevice = 0xf,
    Unicode = 0x10,
    SoC = 0x11,
    EyeAndHeadTrackers = 0x12,
    AuxiliaryDisplay = 0x14,
    Sensors = 0x20,
    MedicalInstrument = 0x40,
    BrailleDisplay = 0x41,
    LightingAndIllumination = 0x59,
    MonitorPage = 0x80,
    MonitorEnumerated = 0x81,
    PowerPage = 0x84,
    BatterySystem = 0x85,
    BarcodeScanner = 0x8c,
    Scales = 0x8d,
    MagneticTapeReader = 0x8e,
    CameraControlPage = 0x90,
    ArcadePage = 0x91,
    GamingDevice = 0x92,
    FidoAlliance = 0xf1d0,
    Other(u16),
}

impl From<UsagePage> for u16 {
    fn from(value: UsagePage) -> Self {
        match value {
            UsagePage::Other(val) => val,
            p => {
                // core::mem::discriminant defines this.
                unsafe { *(&raw const p).cast() }
            }
        }
    }
}

impl From<u16> for UsagePage {
    fn from(value: u16) -> Self {
        match value {
            0 => UsagePage::Undefined,
            1 => UsagePage::GenericDesktop,
            2 => UsagePage::SimulationControls,
            3 => UsagePage::VrControls,
            4 => UsagePage::SportsControls,
            5 => UsagePage::GameControls,
            6 => UsagePage::GenericDeviceControls,
            7 => UsagePage::KeyboardKeypad,
            8 => UsagePage::LedPage,
            9 => UsagePage::ButtonPage,
            0xa => UsagePage::Ordinal,
            0xb => UsagePage::Telephony,
            0xc => UsagePage::Consumer,
            0xd => UsagePage::Digitizers,
            0xe => UsagePage::Haptics,
            0xf => UsagePage::PhysicalInputDevice,
            0x10 => UsagePage::Unicode,
            0x11 => UsagePage::SoC,
            0x12 => UsagePage::EyeAndHeadTrackers,
            0x14 => UsagePage::AuxiliaryDisplay,
            0x20 => UsagePage::Sensors,
            0x40 => UsagePage::MedicalInstrument,
            0x41 => UsagePage::BrailleDisplay,
            0x59 => UsagePage::LightingAndIllumination,
            0x80 => UsagePage::MonitorPage,
            0x81 => UsagePage::MonitorEnumerated,
            0x84 => UsagePage::PowerPage,
            0x85 => UsagePage::BatterySystem,
            0x8c => UsagePage::BarcodeScanner,
            0x8d => UsagePage::Scales,
            0x8e => UsagePage::MagneticTapeReader,
            0x8f => UsagePage::CameraControlPage,
            0x91 => UsagePage::ArcadePage,
            0x94 => UsagePage::GamingDevice,
            0xf1d0 => UsagePage::FidoAlliance,
            o => UsagePage::Other(o),
        }
    }
}

impl From<UsagePage> for hidreport::UsagePage {
    fn from(value: UsagePage) -> Self {
        let n: u16 = value.into();
        hidreport::UsagePage::from(n)
    }
}
