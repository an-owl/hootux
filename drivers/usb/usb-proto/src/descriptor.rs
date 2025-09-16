use crate::DescriptorType;
use crate::descriptor::sealed::Sealed;
use bitfield::bitfield;
use bitflags::bitflags;
use core::marker::PhantomData;

/// Contains information about the device capabilities.
///
/// Returned by the [super::RequestCode::GetDescriptor] when the device descriptor is set to
/// request the device descriptor.
#[repr(C, packed)]
pub struct DeviceDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub bcd_usb: BcdUsb,
    pub device_class: u8,
    pub device_sub_class: u8,
    pub device_protocol: u8,
    /// Describes the maximum packet size for a configuration transaction.
    /// Only 8,16,32,64 are valid values for this field.
    /// When a device is high-speed the only valid value for this field is `64`
    pub max_packet_size_0: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub bcd_device: u16,
    pub manufacturer: u8,
    pub product: u8,
    pub serial_number: u8,
    /// Indicates the number of [ConfigurationDescriptor]'s that are available.
    pub num_configurations: u8,
}

impl DeviceDescriptor {
    pub const fn class(&self) -> [u8; 3] {
        [
            self.device_class,
            self.device_sub_class,
            self.device_protocol,
        ]
    }

    /// Before any setup transactions beyond [super::CtlTransfer::set_address] the driver should
    /// identify the maximum packet size of the device's default pipe.
    ///
    /// This will check that the buffer contains the descriptor type for [DescriptorType::Device].
    /// If so it will return the maximum packet size.
    /// The maximum packet size must be set to the returned value or `8`.
    ///
    /// This takes a `[u8;8]` because only the first 8 bytes should've been fetched from the device
    /// descriptor as described in the USB specification 2.0.
    // Why cant we read the whole thing with 8 bye transfers? IDK.
    pub const fn packet_size(buffer: &[u8; 8]) -> Option<u8> {
        if buffer[1] == Self::DESCRIPTOR_TYPE as u8 {
            Some(buffer[core::mem::offset_of!(DeviceDescriptor, max_packet_size_0)])
        } else {
            None
        }
    }
}

/// Contains a USB revision number as a binary coded decimal.
///
/// The BCD format ix `0xMMmr` Where `MM` is the major version, `m` is the minor version and `r` is the revision.
/// Version 2.3.6 is encoded `0x0236`.
#[repr(transparent)]
#[derive(Copy, Clone, Debug)]
pub struct BcdUsb(pub u16);

impl core::fmt::Display for BcdUsb {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let bytes = self.0.to_le_bytes();
        f.write_fmt(format_args!("{}.{}", bytes[0], bytes[1] & 0xf0))?;
        if bytes[1] & 0x0f != 0 {
            f.write_fmt(format_args!(".{}", bytes[1] & 0x0f))?
        };
        Ok(())
    }
}

impl core::fmt::LowerHex for BcdUsb {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

impl core::fmt::UpperHex for BcdUsb {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.0.fmt(f)
    }
}

/// Contains information about configurations when the configured speed is not the current speed.
/// This can ba fetched from the device using [super::CtlTransfer::get_descriptor] where using [DescriptorType::DeviceQualifier]
#[repr(C, packed)]
pub struct DeviceQualifier {
    pub length: u8,
    pub descriptor_type: u8,
    pub bcd_usb: BcdUsb,
    pub device_class: u8,
    pub device_sub_class: u8,
    pub device_protocol: u8,
    pub max_packet_size_0: u8,
    pub num_configurations: u8,
    _reserved: u8,
}

/// Contains information about a specific device configuration.
/// The number of possible configurations is returned by [DeviceDescriptor::num_configurations]
///
/// This can be fetched using [super::CtlTransfer::get_descriptor] using [DescriptorType::Configuration],
///
/// When `DescriptorType::OtherSpeedConfiguration` is used this describes a configuration when the
/// device is using a speed which it is not currently using.
#[repr(C, packed)]
pub struct ConfigurationDescriptor<T: ConfigurationMarker = Normal> {
    pub length: u8,
    pub descriptor_type: u8,
    /// Requesting this descriptor will also return all associated [InterfaceDescriptor]'s and [EndpointDescriptor]'s,
    /// this field indicates the total size of all these fields.
    pub total_length: u16,
    /// Indicates the number of interfaces implemented by this configuration.
    pub num_interfaces: u8,
    /// Value given to [super::CtlTransfer::set_configuration] to assign this configuration to the device.
    pub configuration_value: u8,
    /// Indicates the string index of this descriptor.
    pub configuration_index: u8,
    pub attributes: ConfigurationAttruibutes,
    pub max_power: u8,
    _phantom: PhantomData<T>,
}

impl<T: ConfigurationMarker> ConfigurationDescriptor<T> {
    /// Returns an iterator over interfaces defined by this configuration.
    ///
    /// # Safety
    ///
    /// The caller must ensure that `self` is fully available from `&self..self.length`, and is in
    /// the format as returned by the device. The required format can be described as
    ///
    /// ```ignore
    /// struct FullConfiguration {
    ///     cfg: ConfigurationDescriptor,
    ///     interfaces: [InterfaceDescriptorWithEndpoints]
    /// }
    /// struct InterfaceDescriptorWithEndpoints {
    ///     id: InterfaceDescriptor
    ///     endpoints: [EndpointDescriptor]
    /// }
    /// ```
    ///
    /// In order to guarantee that this is safe to call the base `ConfigurationDescriptor` can be
    /// fetched using [super::CtlTransfer::get_descriptor] where `D` is `Self` first when `len` is
    /// `None` and again where `len` is `ConfigurationDescriptor.length` of the returned configuration descriptor.
    // todo make an example
    pub unsafe fn iter(&self) -> impl Iterator<Item = &InterfaceDescriptor> {
        ConfigurationIterator {
            parent: self,
            // SAFETY: The caller must guarantee this is safe.
            next: Some(unsafe { (self as *const Self).offset(1) }.cast()),
            count: 1,
        }
    }
}

pub struct ConfigurationIterator<'a, T: ConfigurationMarker> {
    parent: &'a ConfigurationDescriptor<T>,
    next: Option<*const InterfaceDescriptor>,
    count: u8,
}

impl<'a, T: ConfigurationMarker> Iterator for ConfigurationIterator<'a, T> {
    type Item = &'a InterfaceDescriptor;

    fn next(&mut self) -> Option<Self::Item> {
        // SAFETY: USB guarantees alignment caller guarantees everything else
        let raw = self.next.take()?;
        let ret = unsafe { &*raw };
        // SAFETY: USB spec guarantees this points to valid data

        if self.count < self.parent.num_interfaces {
            self.next = unsafe { Some(raw.byte_add(ret.length as usize)) };
            self.count += 1;
        }
        Some(ret)
    }
}

macro_rules! protected_trait {
    (@config: $ty:ty) => {
        impl ConfigurationMarker for $ty {}
        impl Sealed for $ty {}
    };
    (@resolv: $ty:ty, $out:expr) => {
        impl RequestableDescriptor for $ty {
            const DESCRIPTOR_TYPE: DescriptorType = $out;
        }
        impl Sealed for $ty {}
    };
}

pub trait ConfigurationMarker: Sealed {}
pub struct Normal;
protected_trait!(@config: Normal);
pub struct AlternateSpeed;
protected_trait!(@config: AlternateSpeed);

bitflags! {
    #[repr(transparent)]
    #[derive(Copy, Clone, Debug)]
    pub struct ConfigurationAttruibutes: u8 {
        const SELF_POWERED = 1 << 6;
        const REMOTE_WAKEUP = 1 << 5;
    }
}

//fn get_descriptor<C: ConfigurationMarker>(raw: &[u8]) -> Option<&C> {}

mod sealed {
    pub trait Sealed {}
}

pub trait RequestableDescriptor: Sealed + 'static {
    const DESCRIPTOR_TYPE: DescriptorType;
}

protected_trait!(@resolv: DeviceDescriptor, DescriptorType::Device);
protected_trait!(@resolv: DeviceQualifier,DescriptorType::DeviceQualifier);
protected_trait!(@resolv: ConfigurationDescriptor, DescriptorType::Configuration);
protected_trait!(@resolv: ConfigurationDescriptor<AlternateSpeed>, DescriptorType::OtherSpeedConfiguration);

#[repr(C, packed)]
pub struct InterfaceDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub interface_number: u8,
    pub alternate_setting: u8,
    pub num_endpoints: u8,
    pub interface_class: u8,
    pub interface_sub_class: u8,
    pub interface_protocol: u8,
    pub interface_index: u8,
}

impl InterfaceDescriptor {
    /// Returns an iterator over [EndpointDescriptor]'s defined by this interface.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that `self` is in the format returned by the device as specified
    /// by the USB specification. see [ConfigurationDescriptor::iter] for more information.
    pub unsafe fn iter(&self) -> impl Iterator<Item = &EndpointDescriptor> {
        if self.num_endpoints == 0 {
            InterfaceIter {
                parent: self,
                next: None,
                count: 0,
            }
        } else {
            InterfaceIter {
                parent: self,
                // Caller must guarantee that this is safe.
                next: unsafe { Some((self as *const Self).add(1).cast()) },
                count: 0,
            }
        }
    }
}

pub struct InterfaceIter<'a> {
    parent: &'a InterfaceDescriptor,
    next: Option<*const EndpointDescriptor>,
    count: u8,
}

impl<'a> Iterator for InterfaceIter<'a> {
    type Item = &'a EndpointDescriptor;
    fn next(&mut self) -> Option<Self::Item> {
        let raw = self.next.take()?;
        let ret = unsafe { &*raw };
        if self.count < self.parent.num_endpoints {
            // SAFETY: We perform a bounds check to ensure this pointer is valid
            self.next = unsafe { Some(raw.add(1)) };
            self.count += 1;
        }
        Some(ret)
    }
}

impl InterfaceDescriptor {
    pub fn class(&self) -> [u8; 3] {
        [
            self.interface_class,
            self.interface_sub_class,
            self.interface_protocol,
        ]
    }
}

#[repr(C)]
pub struct EndpointDescriptor {
    pub length: u8,
    pub descriptor_type: u8,
    pub endpoint_address: EndpointAddress,
    pub attributes: EndpointAttributes,
    pub max_packet_size: MaxPacketSize,
    /// Polling rate for periodic devices.
    ///
    /// For high/full speed isochronous endpoints and high-speed interrupt endpoints this value
    /// must be `1..=16`. and is an exponential (2^num).
    /// A value of 4 indicates a period of 4.
    ///
    /// For full/low speed interrupt endpoints this value may be between `1..=255`
    ///
    /// For full/low speed devices this number must be multiplied
    /// by 8 to convert frame-times into microframes.
    ///
    /// For high speed bulk OUT endpoints this indicates the maximum NAK rate of the endpoint.
    /// A value of `0` indicates this endpoint will never return NAK.
    /// Other values indicate at most 1 NAK each `interval` of microframes.
    pub interval: u8,
}

bitfield! {
    #[repr(transparent)]
    #[derive(Copy, Clone)]
    pub struct EndpointAddress(u8);
    impl Debug;
    pub endpoint, _: 3,0;
    pub in_endpoint, _: 7;
}

bitfield! {
    #[repr(transparent)]
    #[derive(Copy, Clone)]
    pub struct EndpointAttributes(u8);
    impl Debug;

    /// Indicates the endpoint type, [Self::endpoint_synchronisation] and [Self::endpoint_usage]
    /// are only valid if this is [EndpointTransferType::Isochronous].
    pub into EndpointTransferType, transfer_type,_: 1,0;
    pub into EndpointSynchronisationType, endpoint_synchronisation,_:  3,2;
    pub into EndpointUsageType, endpoint_usage, _: 5,4
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum EndpointTransferType {
    Control = 0,
    Isochronous,
    Bulk,
    Interrupt,
}

impl From<u8> for EndpointTransferType {
    fn from(value: u8) -> Self {
        match value {
            0 => EndpointTransferType::Control,
            1 => EndpointTransferType::Isochronous,
            2 => EndpointTransferType::Bulk,
            3 => EndpointTransferType::Interrupt,
            _ => panic!("invalid endpoint transfer type {value}"),
        }
    }
}

impl From<EndpointTransferType> for u8 {
    fn from(value: EndpointTransferType) -> Self {
        value as u8
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum EndpointSynchronisationType {
    NoSynchronisation = 0,
    /// An asynchronous-isochronous endpoint derives its sample rate from an external source,
    /// such as a crystal and is not synchronised to the USB bus.
    /// Explicit feedback information must be provided so the device driver can adjust data rates.
    Asynchronous = 1,
    /// Adaptive endpoints may sink or send data at any rate within their operating range.
    /// The consumer must provide feedback to the producer to adjust the data rate.
    Adaptive = 2,
    /// An adaptive-isochronous endpoint derives its sample rate from SOF, the sample rate is
    /// deterministic and periodic.
    Synchronous = 3,
}

impl From<u8> for EndpointSynchronisationType {
    fn from(value: u8) -> Self {
        match value {
            0 => EndpointSynchronisationType::NoSynchronisation,
            1 => EndpointSynchronisationType::Adaptive,
            2 => EndpointSynchronisationType::Synchronous,
            3 => EndpointSynchronisationType::Adaptive,
            _ => panic!("invalid endpoint synchronisation {value}"),
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum EndpointUsageType {
    Data = 0,
    Feedback = 1,
    ImplicitFeedbackData = 2,
}

impl From<u8> for EndpointUsageType {
    fn from(value: u8) -> Self {
        match value {
            0 => EndpointUsageType::Data,
            1 => EndpointUsageType::Feedback,
            2 => EndpointUsageType::ImplicitFeedbackData,
            _ => panic!("invalid endpoint usage type {value}"),
        }
    }
}

bitfield! {
    pub struct MaxPacketSize(u16);
    impl Debug;
    pub max_packet_size, _: 10,0;
    /// Indicates the number of extra transaction opportunities per microframe.
    /// `self.isochronous_mult() + 1` indicates number of possible transactions per microframe.
    pub isochronus_mult, _: 12,11;
}

pub trait Descriptor: Sized + Sealed {
    fn length(&self) -> u8;
    fn descriptor_type() -> DescriptorType;

    fn from_raw(raw: &[u8]) -> Option<&Self> {
        if raw.len() < size_of::<Self>() {
            return None;
        }
        if *raw.get(1).unwrap() != Self::descriptor_type() as u8 {
            return None;
        }
        // SAFETY: All invariants of self muse be valid, we assert the descriptor type of self
        Some(unsafe { &*(&raw[0] as *const u8).cast::<Self>() })
    }
}

macro_rules! impl_descriptor {
    ($ty:ty,$id_byte:expr) => {
        impl Descriptor for $ty {
            fn length(&self) -> u8 {
                self.length
            }
            fn descriptor_type() -> DescriptorType {
                $id_byte
            }
        }
    };
}

impl_descriptor!(DeviceDescriptor, DescriptorType::Device);
impl_descriptor!(
    ConfigurationDescriptor<Normal>,
    DescriptorType::Configuration
);
impl_descriptor!(
    ConfigurationDescriptor<AlternateSpeed>,
    DescriptorType::OtherSpeedConfiguration
);
impl_descriptor!(InterfaceDescriptor, DescriptorType::Interface);
impl Sealed for InterfaceDescriptor {}
impl_descriptor!(EndpointDescriptor, DescriptorType::Endpoint);
impl Sealed for EndpointDescriptor {}

impl_descriptor!(DeviceQualifier, DescriptorType::DeviceQualifier);
