#![no_std]
//! This crate defines commands and return values used by the usb protocol.

pub mod descriptor;

use bitfield::bitfield;

/// This represents a configuration request to a USB device. The operation of this is defined in
/// section 9.2 & 9.3 of the USB bus specification 2.0.
///
/// Configuration transactions may require transmitting data in either direction,
/// if so then the transaction where `CtlTransfer` was sent must be followed by more transactions using the appropriate PID.
/// On a device-to-host transaction the device may not send more then [Self::data_len] bytes, however it may send less.
/// On a host-to-device transaction if the host will send [Self::data_len] bytes.
/// When more bytes are sent to the device than was expected the results are undefined.
///
/// # Errors
///
/// When an illegal request is made to a device it must respond with "Stall".
#[repr(C, packed)]
pub struct CtlTransfer {
    request_type: RequestHeader,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
}

impl CtlTransfer {
    pub fn new(
        recipient: Recipient,
        request_type: RequestType,
        will_receive_data: bool,
        request: u8,
        value: u16,
        index: u16,
        length: u16,
    ) -> Self {
        let mut header = RequestHeader(0);
        header.recipent(recipient);
        header.target_type(request_type);
        header.data_direction(will_receive_data);
        Self {
            request_type: header,
            request,
            value,
            index,
            length,
        }
    }

    pub fn data_len(&self) -> usize {
        self.length as usize
    }
}

impl CtlTransfer {
    /// Clears the feature specified by `feature_selector`.
    /// `feature_selector` should be cast from [FeatureDescriptor] where possible.
    ///
    /// `recipient` may not be [Recipient::Other]
    // fixme double check that
    ///
    /// Attempting to clear a feature which cannot be cleared, does not exist or references an
    /// endpoint that does not exist will return an error
    ///
    /// Note: [FeatureDescriptor::TestMode] cannot be cleared
    // todo separate into endpoint & non-endpoint forms
    pub fn clear_feature(recipient: Recipient, feature_selector: u16) -> Self {
        let mut header = RequestHeader(0);
        header.recipent(recipient);
        Self {
            request_type: header,
            request: RequestCode::ClearFeature as u8,
            value: feature_selector,
            index: 0,
            length: 0,
        }
    }

    /// Returns the current configuration of the device.
    /// If the configuration is `0` then the device is not configured.
    pub fn get_configuration() -> Self {
        let mut header = RequestHeader(0);
        header.data_direction(true);

        Self {
            request_type: header,
            request: RequestCode::GetConfiguration as u8,
            value: 0,
            index: 0,
            length: 1, // Why is this one? It returns 2 bytes. Is it one transaction?
        }
    }

    /// This will construct a request that will request the descriptor `D`.
    /// When either [descriptor::DeviceDescriptor] or [descriptor::DeviceQualifier] is requested
    /// `index` will be silently driven to 0.
    ///
    /// When requesting [descriptor::ConfigurationDescriptor] the device will attempt to send all
    /// associated [descriptor::InterfaceDescriptor]'s and [descriptor::EndpointDescriptor]'s.
    /// To determine the buffer length required to fetch all associated data the driver should
    /// request the required `ConfigurationDescriptor` with `len` set to `None`, the returned
    /// descriptor will contain the total length of the configuration.
    ///
    /// If `len` is `None` it will be set to `core::mem::size_of::<D>()`.
    pub fn get_descriptor<D: descriptor::RequestableDescriptor + 'static>(
        mut index: u8,
        len: Option<u16>,
    ) -> Self {
        if core::any::TypeId::of::<D>() == core::any::TypeId::of::<descriptor::DeviceDescriptor>()
            || core::any::TypeId::of::<D>()
                == core::any::TypeId::of::<descriptor::DeviceQualifier>()
        {
            index = 0;
        }

        let mut header = RequestHeader(0);
        header.data_direction(true);
        Self {
            request_type: header,
            request: RequestCode::GetDescriptor as u8,
            value: u16::from_le_bytes([index, D::DESCRIPTOR_TYPE as u8]),
            index: 0,
            length: len.unwrap_or(size_of::<D>() as u16),
        }
    }

    /// Some devices have configurations with interfaces that have mutually exclusive settings.
    /// This request will return the currently selected alternate setting.
    pub fn get_interface(interface: u16) -> Self {
        let mut header = RequestHeader(0);
        header.data_direction(true);
        Self {
            request_type: header,
            request: RequestCode::GetInterface as u8,
            value: 0,
            index: interface,
            length: 1,
        }
    }

    /// This returns a status value indicating hte state of the recipient.
    ///
    /// When `recipient` is [Recipient::Device] this the return value contains two bitflags.
    /// Bit 0 is set when the device is self powered. Bit 1 is set when the device supports remote wakeup.
    /// Remote wakeup will be clear unless the feature was explicitly enabled by the host.
    ///
    /// When `recipient` is [Recipient::Interface] the returned status is reserved.
    ///
    /// When `recipient` is [Recipient::Endpoint] bit 0 indicates whether the endpoint is currently halted.
    ///
    /// # Panics
    ///
    /// This fn will panic if `recipient` is [Recipient::Other] or if
    pub fn get_status(recipient: Recipient, interface: u16) -> Self {
        let mut header = RequestHeader(0);
        header.data_direction(true);
        header.recipent(recipient);

        assert_ne!(recipient, Recipient::Other);
        if recipient == Recipient::Device && interface != 0 {
            panic!("interface must be `0` when recipient is Device");
        }

        Self {
            request_type: header,
            request: RequestCode::GetStatus as u8,
            value: 0,
            index: interface,
            length: 2,
        }
    }

    /// Assigns a new address to the device. If the device has not been assigned an address and the
    /// address is non-zero this will update it to the "address" state. All successive transactions
    /// must be to the new address.
    ///
    /// # Panics
    ///
    /// This fn will panic if `address` is greater than `127`.
    pub fn set_address(address: u8) -> Self {
        assert!(address < 128u8);
        Self {
            request_type: RequestHeader(0),
            request: RequestCode::SetAddress as u8,
            value: address as u16,
            index: 0,
            length: 0,
        }
    }

    /// This request will set the device to use the configuration with the index `configuration_value`.
    /// If `configuration_value` is `0` the device will be deconfigured and placed in the address state.
    pub fn set_configuration(configuration_value: u8) -> Self {
        Self {
            request_type: RequestHeader(0),
            request: RequestCode::SetConfiguration as u8,
            value: configuration_value as u16,
            index: 0,
            length: 0,
        }
    }

    // no set_descriptor

    /// Sets `feature_selector` for the recipient. The caller muse ensure that `feature_selector`
    /// is appropriate for `recipient`.
    ///
    /// If the [FeatureDescriptor::TestMode] feature is selected `recipient` must be [Recipient::Device] and `target`
    /// must contain the test mode for the device.
    ///
    /// When either [Recipient::Interface] or [Recipient::Endpoint] is selected `target` describes
    /// the interface or endpoint.
    pub fn set_feature(recipient: Recipient, feature_selector: u16, target: u16) -> Self {
        let mut header = RequestHeader(0);
        header.recipent(recipient);
        assert_ne!(recipient, Recipient::Other);

        Self {
            request_type: header,
            request: RequestCode::SetFeature as u8,
            value: feature_selector,
            index: target,
            length: 0,
        }
    }

    /// This allows setting the device to an alternate interface. This request amy only be sent if
    /// the device supports alternate interfaces.
    /// This is not used to change the set of configured interfaces. see [Self::set_configuration]
    /// for initial configuration.
    pub fn set_interface(interface: u16, alternate: u16) -> Self {
        let mut header = RequestHeader(0);
        header.recipent(Recipient::Interface);
        Self {
            request_type: header,
            request: RequestCode::SetInterface as u8,
            value: alternate,
            index: interface,
            length: 0,
        }
    }

    /// Some isochronous transfer sizes may vary on a per-frame basis according to a pattern.
    /// The host and endpoint must agree on which frame the pattern begins.
    /// The frame where the pattern begins will be returned to the host.
    pub fn synch_frame(endpoint: u16) -> Self {
        let mut header = RequestHeader(0);
        header.data_direction(true);
        header.recipent(Recipient::Interface);
        Self {
            request_type: header,
            request: RequestCode::SynchFrame as u8,
            value: 0,
            index: endpoint,
            length: 2,
        }
    }
}

bitfield! {
    #[derive(Copy, Clone)]
    struct RequestHeader(u8);
    impl Debug;

    from Recipient, _, recipent: 4,0;
    from RequestType, _, target_type: 6,5;
    from DataDirection, _, data_direction: 7;

}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum RequestType {
    #[default]
    Standard = 0,
    Interface = 1,
    Vendor = 2,
}

impl From<RequestType> for u8 {
    fn from(value: RequestType) -> Self {
        value as u8
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Recipient {
    Device = 0,
    Interface = 1,
    Endpoint = 2,
    Other = 3,
}

impl From<Recipient> for u8 {
    fn from(value: Recipient) -> Self {
        value as u8
    }
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum RequestCode {
    GetStatus = 0,
    ClearFeature = 1,
    SetFeature = 3,
    SetAddress = 5,
    GetDescriptor = 6,
    SetDescriptor = 7,
    GetConfiguration = 8,
    SetConfiguration = 9,
    GetInterface = 10,
    SetInterface = 11,
    SynchFrame = 12,
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum DescriptorType {
    Device = 1,
    Configuration = 2,
    String = 3,
    Interface = 4,
    Endpoint = 5,
    DeviceQualifier = 6,
    OtherSpeedConfiguration = 7,
    InterfacePower = 8,
}

#[non_exhaustive]
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum FeatureDescriptor {
    EndpointHalt = 0,
    DeviceRemoteWakeup = 1,
    TestMode = 2,
}

/// Test modes defined by the USB specification 2.0
///
/// Values of `0xc0..=0xff` are are allowed for vendor specific test modes.
#[non_exhaustive]
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum TestMode {
    TestJ = 1,
    TestK,
    TestSe0Nak,
    TestPacket,
    TestForceEnable,
}
