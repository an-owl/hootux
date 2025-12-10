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
