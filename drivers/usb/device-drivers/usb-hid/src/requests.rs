use core::num::NonZeroU8;

/// Represents request types defined by the HID protocol.
#[derive(Debug, Copy, Clone)]
pub enum Request {
    /// Returns the specified report, which would normally be sent via an IN endpoint.
    ///
    /// If no report IDs are used by the endpoint then set `report_id` to None.
    /// The length of the returned report bust be determined by the caller.
    GetReport {
        report_type: ReportType,
        report_id: Option<u8>,
        interface: u8,
        len: u16,
    },

    /// This is the same as [Self::GetReport] but in the opposite direction,
    /// this can be used to control the device.
    SetReport {
        report_type: ReportType,
        report_id: Option<u8>,
        interface: u8,
    },

    /// Returns the current idle rate for a particular input report.
    GetIdle {
        report_id: u8,
        interface: u8,
    },

    /// This limits the reporting frequency of an interrupt endpoint.
    /// This causes the endpoint to NAK any polls on an interrupt endpoint while its state has not changed.
    /// While the device state has not changed the endpoint will NAK polls for a given duration.
    ///
    SetIdle {
        duration: Duration,
        report_id: Option<NonZeroU8>,
        interface: u8,
    },

    GetProtocol {
        interface: u8,
    },

    SetProtocol {
        protocol: Protocol,
        interface: u8,
    },
}

impl Request {
    fn request_type(&self) -> u8 {
        match self {
            Request::GetReport { .. } => 1,
            Request::SetReport { .. } => 2,
            Request::GetIdle { .. } => 3,
            Request::SetIdle { .. } => 9,
            Request::GetProtocol { .. } => 0xb,
            Request::SetProtocol { .. } => 0xa,
        }
    }
}

impl From<Request> for usb_cfg::CtlTransfer {
    fn from(value: Request) -> Self {
        let recipient = usb_cfg::Recipient::Interface;
        let ty = usb_cfg::RequestType::Class;
        match value {
            Request::GetReport {
                report_type,
                report_id,
                interface,
                len,
            } => usb_cfg::CtlTransfer::new(
                recipient,
                ty,
                true,
                value.request_type(),
                u16::from_le_bytes([report_id.unwrap_or(0), report_type as u8]),
                interface as u16,
                len,
            ),
            Request::SetReport {
                report_type,
                report_id,
                interface,
            } => usb_cfg::CtlTransfer::new(
                recipient,
                ty,
                false,
                value.request_type(),
                u16::from_le_bytes([report_id.unwrap_or(0), report_type as u8]),
                interface as u16,
                1,
            ),

            Request::GetIdle {
                report_id,
                interface,
            } => usb_cfg::CtlTransfer::new(
                recipient,
                ty,
                true,
                value.request_type(),
                u16::from_le_bytes([report_id, 0]),
                interface as u16,
                1,
            ),
            Request::SetIdle {
                duration,
                report_id,
                interface,
            } => usb_cfg::CtlTransfer::new(
                recipient,
                ty,
                false,
                value.request_type(),
                u16::from_le_bytes([report_id.map(|e| e.get()).unwrap_or(0), duration.0]),
                interface as u16,
                0,
            ),

            Request::SetProtocol {
                protocol,
                interface,
            } => usb_cfg::CtlTransfer::new(
                recipient,
                ty,
                false,
                value.request_type(),
                protocol as u16,
                interface as u16,
                0,
            ),
            Request::GetProtocol { interface } => usb_cfg::CtlTransfer::new(
                recipient,
                ty,
                true,
                value.request_type(),
                0,
                interface as u16,
                0,
            ),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct Duration(u8);

impl Duration {
    pub const fn infinite() -> Self {
        Self(0)
    }
    pub const fn from_msec(msec: u16) -> Self {
        Self((msec / 4) as u8)
    }

    /// Returns the idle rate of duration.
    /// The idle rate has a resolution of 4 msec, this returns the actual idle rate that will be
    /// passed to the  device.
    pub const fn msec(&self) -> u16 {
        (self.0 as u16) * 4
    }
}

#[derive(Copy, Clone, Debug)]
pub enum ReportType {
    Input = 1,
    Output = 2,
    Feature = 3,
}

#[derive(Copy, Clone, Debug)]
pub enum Protocol {
    Boot = 0,
    Report = 1,
}
