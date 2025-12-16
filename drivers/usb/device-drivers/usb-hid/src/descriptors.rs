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

use futures_util::future::BoxFuture;
use hid_report::ReportItem;
use hootux::fs::IoError;

/// This component handles parsing of the report descriptor which can be used to generate objects
/// which can retrieve data from a report.
#[derive(Clone, Debug, Default)]
pub(crate) struct StateTable {
    pub report_id: Option<u8>,
    pub usage_page: Option<UsagePage>,
    pub usage: alloc::vec::Vec<u32>,
    pub usage_minimum: Option<u32>,
    pub usage_maximum: Option<u32>,

    pub logical_minimum: Option<u32>,
    pub logical_maximum: Option<u32>,

    pub physical_minimum: Option<u32>,
    pub physical_maximum: Option<u32>,

    pub report_size: Option<u32>,
    pub report_count: Option<u32>,

    pub unit: Option<u32>,
    pub unit_exponent: Option<u32>,

    pub designator_index: Option<u32>,
    pub designator_maximum: Option<u32>,

    pub string_index: Option<u32>,
    pub string_minimum: Option<u32>,
    pub string_maximum: Option<u32>,

    pub delimiter: Option<u32>,
}

#[derive(Clone, Debug, Default)]
struct StateTables {
    local: StateTable,
    global: StateTable,
}

macro_rules! state_init {
    // Init all
    ($self:ident,$tgt:ident) => {
        state_init!($self, $tgt, report_id);
        state_init!($self, $tgt, usage_page);
        state_init!($self, $tgt, usage);
        state_init!($self, $tgt, logical_minimum);
        state_init!($self, $tgt, logical_maximum);
        state_init!($self, $tgt, physical_minimum);
        state_init!($self, $tgt, physical_maximum);
        state_init!($self, $tgt, report_size);
        state_init!($self, $tgt, report_count);
        state_init!($self, $tgt, unit);
        state_init!($self, $tgt, unit_exponent);
        state_init!($self, $tgt, designator_index);
        state_init!($self, $tgt, designator_maximum);
        state_init!($self, $tgt, string_index);
        state_init!($self, $tgt, string_minimum);
        state_init!($self, $tgt, string_maximum);
        state_init!($self, $tgt, delimiter);
    };
    // Init field
    ($self:ident, $tgt:ident, $field:ident) => {
        if let Some(field) = $self.global.$field {
            $tgt.$field.get_or_insert(field);
        }
    };
}

impl StateTables {
    fn compile_state(&self) -> StateTable {
        let mut table = self.local.clone();
        state_init!(self, table);
        table
    }
}

#[repr(u16)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum UsagePage {
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

/// This trait defines an object which can parse report data and forward it to a file object.
trait HidInterface {
    /// Returns which report ID this `HidInterface` can parse. If no report ID's are defined in
    /// the report descriptor then this function *should* not be called, and the return value of this
    /// fn may be undefined.
    ///
    /// [Self::parse_report] will only be called on reports with this report ID.
    fn report_id(&self) -> u8;

    /// Parse the report given in `report` and forward parsed data into `stream`.
    /// `stream` will be given pre-opened in write only mode.
    fn parse_report(
        &self,
        report: &[u8],
        stream: &dyn hootux::fs::device::Fifo<u8>,
    ) -> BoxFuture<Result<(), IoError>>;
}

pub(crate) struct Parser<T: Iterator<Item = ReportItem>> {
    current_state: StateTables,
    stack: alloc::vec::Vec<StateTables>,
    items_stream: T,
}

impl<T: Iterator<Item = hid_report::ReportItem>> Parser<T> {
    /// Pushes the current state tables onto the stack.
    fn push(&mut self) {
        self.stack.push(self.current_state.clone());
    }

    /// Pops the state table from the top of the stack.
    ///
    /// Error indicates that a pop item was found with no corresponding push.
    /// This indicates that the device returned a faulty report descriptor.
    fn pop(&mut self) -> Result<(), ParserPartial> {
        self.current_state = self.stack.pop().ok_or(ParserPartial::PopFail)?;
        Ok(())
    }

    fn step(&mut self) -> Result<ParserPartial, ParserPartial> {
        let Some(item) = self.items_stream.next() else {
            return Ok(ParserPartial::Complete);
        };
        match (item.prefix() >> 2) & 3 {
            // main
            0 => self.handle_main(item),
            // global  | local
            1 | 2 => self.set_field(item),
            // Reserved (HID 1.11)
            3 => panic!("Found reserved HID item field: {:?}", item.as_ref()),
            // SAFETY: int & 3 can never be >3
            _ => unsafe { core::hint::unreachable_unchecked() },
        }
    }

    fn handle_main(&mut self, item: ReportItem) -> Result<ParserPartial, ParserPartial> {
        let state = self.current_state.compile_state();

        // We need to clear the local state when finding a main item.
        core::mem::take(&mut self.current_state.local);
        match item {
            r @ ReportItem::Input(_) | r @ ReportItem::Output(_) | r @ ReportItem::Feature(_) => {
                Ok(ParserPartial::Yield(Report {
                    report_item: r,
                    state_table: state,
                }))
            }

            // Just return incomplete. I'm pretty sure that everything that needs to be done
            // is done already within or outside the collection.
            ReportItem::EndCollection(_) | ReportItem::Collection(_) => {
                Ok(ParserPartial::Incomplete)
            }

            _ => panic!("Passed non-main item to handle_main()"),
        }
    }

    fn set_field(&mut self, report_item: ReportItem) -> Result<ParserPartial, ParserPartial> {
        let state = match report_item.prefix() & 3 << 2 {
            1 => &mut self.current_state.global,
            2 => &mut self.current_state.local,
            _ => panic!("Incorrect prefix for set_field()"),
        };

        match report_item {
            ReportItem::UsagePage(p) => {
                let partial = p.data().try_into().unwrap_or([p.data()[0], 0]);
                let int: u16 = u16::from_le_bytes(partial);
                state.usage_page = Some(UsagePage::from(int));
            }
            ReportItem::LogicalMinimum(min) => {
                state.logical_minimum = Some(slice_to_dword(min.data()))
            }
            ReportItem::LogicalMaximum(max) => {
                state.logical_maximum = Some(slice_to_dword(max.data()))
            }
            ReportItem::PhysicalMinimum(min) => {
                state.physical_minimum = Some(slice_to_dword(min.data()))
            }
            ReportItem::PhysicalMaximum(max) => {
                state.physical_maximum = Some(slice_to_dword(max.data()))
            }
            ReportItem::UnitExponent(exponent) => {
                state.unit_exponent = Some(slice_to_dword(exponent.data()))
            }
            ReportItem::Unit(unit) => state.unit = Some(slice_to_dword(unit.data())),
            ReportItem::ReportSize(size) => state.report_size = Some(slice_to_dword(size.data())),
            ReportItem::ReportId(id) => state.report_id = Some(id.data()[0]),
            ReportItem::ReportCount(count) => {
                state.report_count = Some(slice_to_dword(count.data()))
            }
            ReportItem::Push(_) => self.push(),
            ReportItem::Pop(_) => self.pop()?,
            ReportItem::Usage(usage) => state.usage.push(slice_to_dword(usage.data())),
            ReportItem::UsageMinimum(min) => state.usage_minimum = Some(slice_to_dword(min.data())),
            ReportItem::UsageMaximum(max) => state.usage_maximum = Some(slice_to_dword(max.data())),
            ReportItem::DesignatorIndex(index) => {
                state.designator_index = Some(slice_to_dword(index.data()))
            }
            ReportItem::DesignatorMinimum(min) => {
                state.designator_maximum = Some(slice_to_dword(min.data()))
            }
            ReportItem::DesignatorMaximum(max) => {
                state.designator_maximum = Some(slice_to_dword(max.data()))
            }
            ReportItem::StringIndex(index) => {
                state.string_index = Some(slice_to_dword(index.data()))
            }
            ReportItem::StringMinimum(max) => {
                state.string_minimum = Some(slice_to_dword(max.data()))
            }
            ReportItem::StringMaximum(min) => {
                state.string_maximum = Some(slice_to_dword(min.data()))
            }
            ReportItem::Delimiter(delimiter) => {
                state.delimiter = Some(slice_to_dword(delimiter.data()))
            }
            ReportItem::Reserved(res) => {
                log::error!("Got reserved item while parsing {:?}", res.as_ref())
            }

            _ => unreachable!(), // Main items will panic in first panic statement.
        }

        Ok(ParserPartial::Incomplete)
    }
}

impl<T: Iterator<Item = ReportItem>> Iterator for Parser<T> {
    type Item = Result<Report, ()>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.step() {
                Ok(ParserPartial::Complete) => return None,
                Ok(ParserPartial::Incomplete) => continue,
                Ok(ParserPartial::Yield(report)) => return Some(Ok(report)),
                Err(ParserPartial::PopFail) => return Some(Err(())),
                _ => unreachable!("Bad result from {}.step()", core::any::type_name::<Self>()),
            }
        }
    }
}

fn slice_to_dword(slice: &[u8]) -> u32 {
    let mut accum = [0, 0, 0, 0];
    accum[..slice.len()].copy_from_slice(&slice);
    u32::from_le_bytes(accum)
}

enum ParserPartial {
    Complete,
    Yield(Report),
    Incomplete,
    PopFail,
}

pub(crate) struct Report {
    pub report_item: ReportItem,
    pub state_table: StateTable,
}
