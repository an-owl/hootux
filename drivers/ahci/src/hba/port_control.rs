use crate::hba::command;
use crate::register::*;

#[derive(Debug)]
#[repr(C)]
pub(crate) struct PortControl {
    /// PxCLB:PxCLBU
    command_list_base: HbaAddr<1028>, // may only be 32bit check first.
    /// PxFB:PxFBU
    /// Although this can be aligned to 256 bytes this is aligned to 4096 to avoid extra checks.
    fis_base_address: HbaAddr<4096>, // may only be 32 bit.
    /// PxIS
    pub(crate) interrupt_status: Register<InterruptStatus, ReadWriteClear<InterruptStatus>>,
    /// PxIE
    interrupt_enable: Register<InterruptEnable>,
    /// PxCMD
    pub(crate) cmd_status: Register<CommStatus>,
    _res0: core::mem::MaybeUninit<u32>,
    /// PxTFD
    pub(crate) task_file_data: Register<TaskFileData, ReadOnly>,
    /// PxSIG
    signature: Register<PortSignature, ReadOnly>, // what dis?
    /// PxSSTS
    sata_status: Register<SataStatus, ReadOnly>,
    /// PxSCTL
    sata_ctl: Register<SataControl>,
    /// PxSERR
    sata_err: Register<SataErr>,
    /// PxSCAT
    sata_active: CmdIssue,
    /// PxCI
    command_issue: CmdIssue,
    /// PxSNTF
    sata_notification: Register<SataNotification, ReadWriteClear<SataNotifyAck>>,
    /// PxFBS
    fis_based_switching_proto: Register<FisSwitchingCtl, Aboe<FisErrClear>>,
    /// PxDEVSLP
    dev_sleep: Register<DevSleep>,
    _res1: core::mem::MaybeUninit<u32>,
    /// PxVS
    vendor_specific: [Register<u32>; 4],
}

impl PortControl {
    /// Returns the state of the device connected to the port. This fn will return [crate::PortState::None]
    /// if the port is not implemented. The caller should query [super::general_control::GeneralControl]
    /// for implemented ports.
    pub fn get_port_state(&self) -> crate::PortState {
        use crate::PortState;
        return if self
            .cmd_status
            .read()
            .contains(CommStatus::COLD_PRESENCE_STATE)
        {
            PortState::Cold
        } else if self.sata_status.read().dev_detect() == DeviceDetection::InComm {
            if self.sata_status.read().power_management() == PowerState::Active {
                PortState::Hot
            } else {
                PortState::Warm
            }
        } else {
            PortState::None
        };
    }

    pub(crate) fn set_cmd_table(&mut self, cmd_list: *const [command::CommandHeader; 32]) {
        // *Should* never panic
        let a = hootux::mem::mem_map::translate(cmd_list as *const _ as usize).unwrap();
        self.command_list_base.set(a);
    }

    /// Executes a command on command slot `cmd`.
    /// For NQC commands use [Self::exec_nqc] instead.
    ///
    /// # Safety
    ///
    /// The caller must ensure that the FIS in the given command slot is valid and that the PRDT has
    /// been configured correctly.
    pub(crate) unsafe fn exec_cmd(&self, cmd: u8) {
        self.command_issue.issue(cmd);
    }

    /// Identical to [Self::exec_cmd] for NQC commands.
    pub(crate) unsafe fn exec_nqc(&self, cmd: u8) {
        self.sata_active.issue(cmd);
        self.command_issue.issue(cmd);
    }

    pub(crate) fn cmd_state(&self) -> u32 {
        self.command_issue.0.get()
    }

    pub(crate) fn tfd_wait(&self) {
        let s = hootux::time::get_sys_time();
        while self.task_file_data.read().status & 0x88 != 0 {
            core::hint::spin_loop()
        }
        log::trace!("tfd_wait: {}ns", hootux::time::get_sys_time() - s);
    }

    pub(crate) fn get_ci(&self) -> u32 {
        self.command_issue.0.get()
    }
}

bitflags::bitflags! {
    /// PxIS
    ///
    /// Represents the port interrupt status register. All but explicitly mentioned flags in this
    /// register are cleared by writing a `1`
    #[derive(Debug)]
    #[repr(transparent)]
    pub(crate) struct InterruptStatus: u32 {
        /// CPDS (R1C)
        ///
        /// A device change was detected by cold presence logic. Only valid if the port supports
        /// cold presence logic.
        const COLD_PORT_DETECT = 1 << 31;
        /// TFES (R1C)
        ///
        /// Set when a device returns an error in a FIS
        const TASK_FILE_ERROR = 1 << 30;
        /// HBFS (R1C)
        ///
        /// Indicates that the HBA encountered a fatal error, such as a bad software pointer.
        /// Will be indicated with a PCI error.
        const HOST_BUS_FATAL = 1 << 29;
        /// HBDS (R1C)
        ///
        /// Indicates that the HBA encountered a data error when accessing system memory.
        const HOST_BUS_DATA_ERR = 1 << 28;
        /// IFS (R1C)
        ///
        /// Indicates the HBA encountered an error that caused a transfer to stop. todo see 6.1.2
        const INTERFACE_FATAL = 1 << 27;
        /// INFS (R1C)
        ///
        /// Indicates the HBA encountered an error on the sata interface but was able to continue
        /// operation.
        const INTERFACE_NON_FATAL = 1 << 26;
        /// OFS (R1C)
        ///
        /// Indicates HBA received more bytes than was expected
        const OVERFLOW = 1 << 24;
        /// IPMS (R1C)
        ///
        /// Indicates that the HBA received an unexpected FIS. This may be set by the enumeration
        /// of a port multiplier. This should be disabled during port enumeration.
        const BAD_PORT_MULTIPLIER_STATUS = 1 << 23;
        /// PRCS (RO)
        ///
        /// This bit is read only and reflects the state of [SataErr::PHY_INTERNAL_ERR]. This bit is
        /// cleared by clearing [SataErr::PHY_INTERNAL_ERR].
        const PHY_RDY_CHANGE = 1 << 22;
        /// DMPS (R1C)
        ///
        /// Indicates the mechanical presence switch associated with this port has been opened or
        /// closed.
        const DEVICE_MECHANICAL_PRESENCE = 1 << 7;
        /// PCS (RO)
        ///
        /// Indicates that the port connection has changed.
        /// This bit reflects the state of [SataErr::EXCHANGED] which mist be cleared to clear this bit.
        const PORT_CONNECT_CHANGE = 1 << 6;
        /// DPS (R1C)
        ///
        /// A PRD with the i bit set has been processed. todo see 5.4.2
        const DESCRIPTOR_PROCESSED = 1 << 5;
        /// UFS (RO)
        ///
        /// This bit is read only. It is set when an unknown FIS is received.
        /// This bit is cleared by clearing [SataErr::UNKNOWN_FIS_TYPE].
        const UNKNOWN_FIS_INT = 1 << 4;
        /// SDBS (R1C)
        ///
        /// A device has sent a Set Bits FIS with a the interrupt flag enabled. todo link to set bits FIS
        const SET_DEV_BITS = 1 << 3;
        /// DSS (R1C)
        ///
        /// A DMA setup fis has been received with the Interrupt flag set.
        const DMA_SETUP_FIS = 1 << 2;
        /// PSS (R1C)
        ///
        /// A PIO setup FIS Has been sent with the Interrupt flag set.
        const PIO_SETUP_FIS = 1 << 1;
        /// DHRS (R1C)
        ///
        /// A D2H Register FIS has been received with the Interrupt flag set
        const DEV_TO_HOST_FIS = 1;
    }

    /// This is used to enable interrupts. When set allows the corresponding bit in [InterruptStatus]
    /// to generate an interrupt.
    #[derive(Debug)]
    #[repr(transparent)]
    struct InterruptEnable: u32 {
        const COLD_PORT_DETECT = 1 << 31;
        const TASK_FILE_ERROR = 1 << 30;
        const HOST_BUS_FATAL = 1 << 29;
        const HOST_BUS_DATA_ERR = 1 << 28;
        const INTERFACE_FATAL = 1 << 27;
        const INTERFACE_NON_FATAL = 1 << 26;
        const OVERFLOW = 1 << 24;
        const BAD_PORT_MULTIPLIER_STATUS = 1 << 23;
        const PHY_RDY_CHANGE = 1 << 22;
        const DEVICE_MECHANICAL_PRESENCE = 1 << 7;
        const PORT_CONNECT_CHANGE = 1 << 6;
        const DESCRIPTOR_PROCESSED = 1 << 5;
        const UNKNOWN_FIS_INT = 1 << 4;
        const SET_DEV_BITS = 1 << 3;
        const DMA_SETUP_FIS = 1 << 2;
        const PIO_SETUP_FIS = 1 << 1;
        const DEV_TO_HOST_FIS = 1;
    }

    #[derive(Debug)]
    pub(crate) struct CommStatus: u32 {
        /// ASP (CD)
        ///
        /// When set and [Self::AGGRESSIVE_LINK_POWER_MAN_ENABLE] is set the device will be be set
        /// to slumber when [DevSleep]s conditions are met. When cleared THe device will be set to
        /// the partial state
        ///
        /// When [super::general_control::HbaCapabilities::AGGRESSIVE_LINK_POWER_MANAGEMENT] is set
        /// this is (RW) otherwise it is (RO)1.
        const AGGRESSIVE_LOW_POWER = 1 << 27;
        /// ALPE (CD)
        ///
        /// When set the device will use aggressive power management features configured by the
        /// [DevSleep] register.
        ///
        /// When [super::general_control::HbaCapabilities::AGGRESSIVE_LINK_POWER_MANAGEMENT] is set
        /// this is (RW) otherwise it is (RO)1.
        const AGGRESSIVE_LINK_POWER_MAN_ENABLE = 1 << 26;
        /// DLAE (RW)
        ///
        /// When set the device will drive the LED pin regardless of [Self::DEVICE_IS_ATAPI].
        const DRIVE_LED_ON_ATAPI = 1 << 25;
        /// ATAPI (RW)
        ///
        /// When set indicates the connected device ATAPI. This is used by the hba to control whether
        /// the drive LED is used by this port
        // todo that can be all this does, right?
        const DEVICE_IS_ATAPI = 1 << 24;
        /// APSTE (RW)
        ///
        /// When set the HBA may perform automatic partial to slumber transactions. Software shall
        /// not set this bit if [super::general_control::HbaCapabilitiesExt::AUTOMATIC_PARTIAL_TO_SLEEP]
        /// is not present.
        const AUTO_PARTIAL_TO_SLUMBER = 1 << 23;
        /// FBSCP (RO)
        ///
        /// Indicates this port supports FIS-based switching
        const FIS_BASED_SWITCHING_CAPABLE = 1 << 22;
        /// ESP (RO)
        ///
        /// Indicates that this port is connected to an external eSATA port. When set this port may
        /// encounter hot plug events regardless of [Self::HOT_PLUG_CAPABLE].
        const ESATA_PORT = 1 << 21;
        /// CPD (RO)
        ///
        /// Indicates that th port supports cold presence detection.
        const COLD_PRESENCE_DETECTION = 1 << 20;
        /// MPSP (RO)
        ///
        /// Indicates that a mechanical presence switch is connected to this port.
        const MECHAINICAL_PRESENCE_SWITCH_ATTACHED = 1 << 19;
        /// HPCP (RO)
        ///
        /// Indicates that this port may experience hot plug events.
        const HOT_PLUG_CAPABLE = 1 << 18;
        /// PMA (CD)
        ///
        /// Indicates that a port multiplier is attached. Software must detect a port multiplier and
        /// set this bit. Software should not set this bit when [Self::START] is present
        const PORT_MULTIPLIER_ATTACHED = 1 << 17;
        /// CPS (RO)
        ///
        /// When set indicates that a device is detected by cold presence detection.
        const COLD_PRESENCE_STATE = 1 << 16;
        /// CR (RO)
        ///
        /// When set indicates the command list DMA engine for this port is running. todo see 5.3.2
        const COMMAND_LIST_RUNNING = 1 << 15;
        /// FR (RO)
        ///
        /// When set the FIS receive DMA engine is running fo this port. todo see 10.3.2
        const FIS_RECIEVE_RUNNING = 1 << 14;
        /// MPSS (RO)
        ///
        /// Reports the state of the mechanical presence switch attached to this port. todo is a present device open or closed
        const MECHANICAL_PRESENCE_SWITCH_STATE = 1 << 13;
        /// FRE (RW)
        ///
        /// When set the HBA will post received FISes into the receive are pointed to by PxFB.
        /// When cleared FISes are not accepted by the HBA except for the first D2H register FIS
        /// after initialization.
        ///
        /// This must not be set this until until PxFB has been set
        // todo link PxFB
        const FIS_RECIEVE_ENABLE = 1 << 4;
        /// CLO (RW1)
        ///
        /// Setting this bit will clear [TaskFileData] BSY and DRQ bits
        const COMMAND_LIST_OVERRIDE = 1 << 3;
        /// POD (CD)
        ///
        /// When [Self::COLD_PRESENCE_STATE] is present this field is (RW) otherwise it is (RO).
        /// When this bit is set by software the HBA enables power to the device.
        // todo it's unclear whether this power is delivered by the HBA.
        const POWER_ON_DEVICE = 1 << 2;
        /// SUD (CD)
        ///
        /// When [super::general_control::HbaCapabilities::STAGGERED_SPIN_UP] is present this bit is (RW).
        /// Otherwise it is (RO) and set to 1. When this bit is set form 0 to 1 the HBA will signal
        /// a COMRESET top the device. todo theres some more bullshit here find out what it means
        const SPIN_UP_DEVICE = 1 << 1;
        /// ST (RW)
        ///
        /// While set the HBA may process the command list. When this bit is set the command list
        /// will be processed from entry 0. When this bit is cleared the PxCI register is cleared.
        /// This shall only be set when [Self::FIS_RECIEVE_ENABLE] is set. todo see 10.3.1
        const START = 1;
    }
}

unsafe impl Acknowledge<InterruptStatus> for InterruptStatus {
    /// Read only bits should not be changed
    fn ack(self) -> InterruptStatus
    where
        Self: Sized,
    {
        Self::from_bits_truncate(self.bits())
    }
}

#[repr(u8)]
#[derive(Eq, PartialEq, Debug, Default)]
pub enum PowerState {
    /// When this value is read the interface is ready to accept a new command, even if the
    /// previous operation has not been completed.
    #[default]
    Idle = 0,
    /// Requests that the interface transition into an active state.
    // todo which does?
    Active,
    /// Requests the interface transition into a a partial state.
    ///
    /// This request may be rejected and ignored.
    // todo: this does? when will it reject this?
    Partial,
    /// Requests the interface transition to the slumber state.
    ///
    /// This request may be rejected and ignored.
    // todo: once again idk what this does.
    Slumber = 6,
    /// Asserts the `DEVSLP` signal. The HBA will ignore the timeout value specified by [todo].
    /// This may only be asserted when the interface is idle PxCI & PxSCAT is clear.
    /// - If [super::general_control::HbaCapabilitiesExt::DEVICE_SLEEP] in the associated
    /// [super::general_control::GeneralControl] struct is not present no action will be taken and
    /// the interface will remain in its current state.
    /// - If [super::general_control::HbaCapabilitiesExt::DEVICE_SLEEP] is present and
    /// [super::general_control::HbaCapabilitiesExt::DEVSLP_FROM_SLUMBER_ONLY] is present and
    /// [SataStatus::power_management] is not 6 the signal will not be sent and the interface will
    /// remain in its current state.
    DevSleep = 8,
}

unsafe impl ClearReserved for InterruptEnable {
    /// This relies on unavailable features not being set.
    fn clear_reserved(&mut self) {
        Self::from_bits_truncate(self.bits());
    }
}

impl Into<u32> for PowerState {
    /// Returns the value of self that can be ORd into the register
    fn into(self) -> u32 {
        (self as u8 as u32) << 28
    }
}

impl From<u32> for PowerState {
    /// Returns a variant of `Self` from a raw register value
    ///
    /// # Panics
    ///
    /// This fn will panic if value is not a variant of `Self`
    fn from(mut value: u32) -> Self {
        value = (value >> 28) & 0xf;
        match value {
            0 => Self::Idle,
            1 => Self::Active,
            3 => Self::Partial,
            6 => Self::Slumber,
            8 => Self::DevSleep,
            e => panic!(
                "Unable to convert {} into {}",
                e,
                core::any::type_name::<Self>()
            ),
        }
    }
}

impl From<u8> for PowerState {
    /// Retrieves a variant from the raw value using bits 0..4
    ///
    /// # Panics
    ///
    /// This fn will panic if value is not a variant of `Self`
    fn from(value: u8) -> Self {
        match value {
            0 => Self::Idle,
            1 => Self::Active,
            3 => Self::Partial,
            6 => Self::Slumber,
            8 => Self::DevSleep,
            e => panic!(
                "Unable to convert {} into {}",
                e,
                core::any::type_name::<Self>()
            ),
        }
    }
}

impl CommStatus {
    #![allow(dead_code)]
    /// ICC (RW)
    ///
    /// Controls the power management states of the interface if the link layer is currently in the
    /// `L_IDLE` or `L_NoCommPower` state this will transition to the requested power state,
    /// otherwise this field has no effect.
    ///
    /// If the interface is in a low power state the interface may need to be made
    /// [PowerState::Active] before changing into a different low power state. If
    /// [super::general_control::HbaCapabilitiesExt::DEVSLP_FROM_SLUMBER_ONLY] is present
    /// [PowerState::DevSleep] may only be set when the device is already in [PowerState::Slumber]
    ///
    /// # Safety
    ///
    /// This fn is unsafe because calling this fn while `self.get_power_state() != CommPowerState::Idle`
    // todo: how do i get L_IDLE and L_NoCommPower? What interface link layer?
    pub unsafe fn set_power_state(&mut self, command: PowerState) {
        let mut t: u32 = self.bits();
        t &= !(0xf << 28);
        t |= Into::<u32>::into(command);
        *self = Self::from_bits_retain(t);
    }

    /// ICC
    ///
    /// Return the current power state
    pub fn get_power_state(&self) -> PowerState {
        self.bits().into()
    }

    /// CCS (RO)
    ///
    /// This field is valid when [Self::START] is set and is set to the the slot of the current
    /// command being issued. When [Self::START] is cleared this field is set to `0`. When [Self::START]
    /// is set the highest priority command is command `0`
    pub fn get_current_command(&self) -> u8 {
        let t: u32 = self.bits();
        (t >> 8) as u8 & 0xf
    }
}

// SAFETY: this does not contain reserved bits
unsafe impl ClearReserved for CommStatus {
    fn clear_reserved(&mut self) {}
}

/// PxTFD (RO)
///
/// This register copied specific fields of the task file when FISes are received.
///
/// The FISes that contain this information are
/// - D2H Register FIS
/// - PIO setup FIS
/// - Set device bits FIS
#[derive(Debug, Copy, Clone)]
#[repr(C)]
pub(crate) struct TaskFileData {
    status: u8,
    err: u8,
    _reserved: u16,
}

impl TaskFileData {
    #![allow(dead_code)]
    /// Returns the latest copy of the task file error register.
    pub(crate) fn get_err(&self) -> u8 {
        self.err
    }

    /// Returns the status
    pub(crate) fn get_status(&self) -> u8 {
        self.status
    }
}

/// PxSIG (RO)
///
/// This register is set when the first D2H FIS is received from the device.
// idk what any of this does
#[repr(C)]
#[derive(Debug)]
struct PortSignature {
    sector_count: u8,
    lba_low: u8,
    lba_mid: u8,
    lba_high: u8,
}

impl PortSignature {
    #![allow(dead_code)]
    pub fn sector_count(&self) -> u8 {
        self.sector_count
    }

    pub fn lba_low(&self) -> u8 {
        self.lba_low
    }

    pub fn lba_mid(&self) -> u8 {
        self.lba_mid
    }

    pub fn lba_high(&self) -> u8 {
        self.lba_high
    }
}

/// PxSSTS (RO)
#[repr(transparent)]
#[derive(Debug)]
struct SataStatus {
    inner: u32,
}

#[repr(u8)]
#[derive(Copy, Clone, Eq, PartialEq)]
enum DeviceDetection {
    /// No device is connected to the port.
    NoDevice = 0,
    /// A device is connected to the but Physical communication is not established
    NoComm,
    /// A device is connected and communication has been established
    InComm = 3,
    /// The port is disabled.
    Offline,
}

impl SataStatus {
    /// IPM
    ///
    /// This fn returns the current power state of the device.
    /// The return value of this fn reflects the current state of the device not an active transition
    #[allow(dead_code)]
    pub fn power_management(&self) -> PowerState {
        ((self.inner >> 8) as u8 & 0xf).into()
    }

    /// SPD
    /// Returns the interface speed if the device is enabled
    #[allow(dead_code)]
    pub fn interface_speed(&self) -> Option<super::SataGeneration> {
        use super::SataGeneration;
        let t = (self.inner >> 4) & 0xf;
        match t {
            0 => None,
            1 => Some(SataGeneration::Gen1),
            2 => Some(SataGeneration::Gen2),
            3 => Some(SataGeneration::Gen3),
            _ => unreachable!(),
        }
    }

    /// DET
    pub fn dev_detect(&self) -> DeviceDetection {
        self.inner.into()
    }
}

impl From<u32> for DeviceDetection {
    fn from(value: u32) -> Self {
        match value & 7 {
            0 => Self::NoDevice,
            1 => Self::NoComm,
            3 => Self::InComm,
            4 => Self::Offline,
            e => panic!(
                "Unable to convert {} into {}",
                e,
                core::any::type_name::<Self>()
            ),
        }
    }
}

/// SCR2
#[derive(Debug)]
struct SataControl {
    inner: u32,
}

bitflags::bitflags! {
    pub struct AllowedPowerMode: u32 {
        const PARTIAL_DISABLE = 1 << 8;
        const SLUMBER_DISABLE = 1 << 9;
        const DEVSLP_DISABLE = 1 << 10;
        const _RESERVED = 1 << 11;
    }
}

#[repr(C)]
enum DeviceDetectionInit {
    /// Performs no action
    NoAction = 0,
    /// Performs an initialization of the interface. This is essentially a reset of the interface.
    /// When set it should be cleared after a minimum of 1ms
    ResetInterface,
    /// Disable the interface and put Phy in offline mode
    DisableInterface = 4,
}

impl From<u32> for DeviceDetectionInit {
    fn from(mut value: u32) -> Self {
        value &= 0xf;
        match value {
            0 => Self::NoAction,
            1 => Self::ResetInterface,
            4 => Self::DisableInterface,
            _ => unreachable!(),
        }
    }
}

impl SataControl {
    #![allow(dead_code)]
    /// IPM (RW) non-volatile
    ///
    /// Gets the power modes the HBA is allowed to transition to.
    /// If a transition to a disabled power state is attempted the HBA must PMNAKp any request form the device.
    // todo wtf is PMNAKp?
    pub fn get_power_man_trans_allow(&self) -> AllowedPowerMode {
        AllowedPowerMode::from_bits_truncate(self.inner)
    }

    /// See [Self::set_power_man_trans_allow]
    pub fn set_power_man_trans_allow(&mut self, mode: AllowedPowerMode) {
        self.inner &= !AllowedPowerMode::all().bits();
        self.inner |= mode.bits();
    }

    /// SPD (RW)
    ///
    /// Indicates the highest allowed speed by the interface.
    pub fn get_allowed_limit(&self) -> Option<super::SataGeneration> {
        use super::SataGeneration;
        let t = (self.inner >> 4) & 0xf;
        match t {
            0 => None,
            1 => Some(SataGeneration::Gen1),
            2 => Some(SataGeneration::Gen2),
            3 => Some(SataGeneration::Gen3),
            _ => unreachable!(),
        }
    }

    /// See [Self::get_allowed_limit]
    pub fn set_speed_limit(&mut self, limit: Option<super::SataGeneration>) {
        self.inner &= !0xf0;
        self.inner |= limit.map_or(0, |g| g as u32);
    }

    /// DET (RW)
    ///
    /// Controls the device detection and initialization
    pub fn get_dev_detect_init(&self) -> DeviceDetectionInit {
        self.inner.into()
    }

    pub fn set_dev_detect_init(&mut self, action: DeviceDetectionInit) {
        self.inner &= !0xf;
        self.inner |= action as u32;
    }
}

// SAFETY: reserved bits are 20..31
unsafe impl ClearReserved for SataControl {
    fn clear_reserved(&mut self) {
        self.inner &= 0xf_ffff
    }
}

bitflags::bitflags! {
    /// DIAG (RWC)
    #[derive(Debug)]
    #[repr(transparent)]
    struct SataErr: u32 {
        /// DIAG.X
        ///
        /// Indicates a change in device presence.
        const EXCHANGED =  1 << 26;
        /// DIAG.F
        ///
        /// Indicates that a FIS was received with a good CRC but an unknown field.
        const UNKNOWN_FIS_TYPE =  1 << 25;
        /// DIAG.T
        ///
        /// Indicates that a state transition within the transport layer has encountered an error.
        const TRANSPORT_STATE_TRANSITION_ERR =  1 << 24;
        /// DIAG.S
        ///
        /// Indicates that the link layer state machine has encountered an error.
        /// The link layer state machine defines the condition under which the link layer detect an
        /// erroneous transition.
        const LINK_SEQUENCE_ERR =  1 << 23;
        /// DIAG.H
        ///
        /// Indicates that one or more R_ERR (received error) responses was returned from a frame
        /// transition.
        ///
        /// This may be triggered by a CRC error or 8b/10b decoding error was detected by the recipient.
        const HANDSHAKE_ERR =  1 << 22;
        /// DIAG.C
        ///
        /// Indicates that a CRC error was encountered by the link layer.
        const CRC_ERR =  1 << 21;
        /// DIAG.D
        ///
        /// This is not used by AHCI.
        const DISPARITY_ERR =  1 << 20;
        /// DIAG.B
        ///
        /// Indicates that a 10b/8b decoding error was encountered.
        const DECODE_ERROR =  1 << 19;
        /// DIAG.W
        ///
        /// Indicates that a Comm Wake signal was encountered.
        const COMM_ERR =  1 << 18;
        /// DIAG.I
        ///
        /// Indicates that the phy detected an internalerror.
        const PHY_INTERNAL_ERR =  1 << 17;
        /// DIAG.N
        ///
        /// Indicates that the PhyRdy Signal changed state.
        const PHY_RDY_CHANGE =  1 << 16;

        /// ERR.E
        ///
        /// The HBA encountered an error that may include a master or target abort from the PCI
        /// interface or another internal condition. The type of error *should* be indicated by the
        /// [InterruptStatus] register.
        const INTERNAL_ERR = 1 << 11;
        /// ERR.P
        ///
        /// A SATA protocol violation was detected
        const PROTOCOL_ERR = 1 << 10;
        /// ERR.C
        ///
        /// An unrecoverable communication occurred and is expected to be persistent. This may
        /// indicate a hardware error.
        const PERSISTANT_COMM_DATA_ERR = 1 << 9;
        /// ERR.T
        ///
        /// A data integrety error was encountered that was not recovered by the interface
        const TRANSIENT_DATA_ERR = 1 << 8;
        /// ERR.M
        ///
        /// Communication between the the device and host was temporarily lost and re-established.
        const RECOVERED_COMM_ERR = 1 << 1;
        /// ERR.I
        ///
        /// A data integrity error was encountered and recovered thorough a retry
        const RECOVERED_DATA_ERR = 1;
    }
}

#[derive(Debug)]
#[repr(C)]
struct SataNotification {
    notification: u16,
    _reserved: u16,
}

impl SataNotification {
    #![allow(dead_code)]
    pub fn get_bits(&self) -> u16 {
        self.notification
    }

    pub fn get_bit(&self, bit: u8) -> bool {
        assert!(bit < 32);

        if (self.notification | 1 << bit) != 0 {
            true
        } else {
            false
        }
    }

    pub fn ack_all(self) -> SataNotifyAck {
        SataNotifyAck::new(self.notification)
    }
}

#[derive(Default, Copy, Clone)]
struct SataNotifyAck {
    ack: u16,
}

impl SataNotifyAck {
    #![allow(dead_code)]
    /// Creates a new acknowledgement using the mask `bits`.
    ///
    /// To create an empty acknowledgement use `Default::default()` instead
    pub fn new(bits: u16) -> Self {
        Self { ack: bits }
    }

    /// Sets the given bit in the mask.
    pub fn set(&mut self, bit: u8) {
        self.ack |= 1 << bit;
    }
}

unsafe impl Acknowledge<SataNotification> for SataNotifyAck {
    fn ack(self) -> SataNotification {
        SataNotification {
            notification: self.ack,
            _reserved: 0,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    struct FisSwitchingCtl: u32 {
        /// SDE (RO)
        const SINGLE_DEVICE_ERROR = 1 << 2;
        /// DEC (R1C)
        const DEVICE_ERROR_CLEAR = 1 << 1;
        /// EN
        const ENABLE = 1;
    }
}

impl FisSwitchingCtl {
    #![allow(dead_code)]
    /// DWE (RO)
    fn get_dev_err(&self) -> u8 {
        let t: u32 = self.bits();
        ((t >> 16) & !0xf) as u8
    }

    /// ADO (RO)
    ///
    /// Exposes the number of devices the implementation is optimized for. When there are more
    /// devices attached than indicated in this field performance degradation may occur.
    ///
    /// The value returned by this will be `>= 2`.
    fn get_dev_optimization(&self) -> u8 {
        ((self.bits() >> 12) & 0x7) as u8
    }

    /// DEV (RW)
    ///
    /// Set by software to the port value of the next command to issue.
    /// This allows the hardware to know the port the command is issued to without checking the
    /// command header.
    ///
    /// Software must not issue commands to multiple port multipliers on the same write of the
    /// [CmdIssue] register.  
    fn set_dev_to_issue(&mut self, id: u8) {
        let id = id as u32;
        assert!(id < 16);

        let mut t = self.bits();
        t &= !(0xf << 8);
        t |= id << 8;
        *self = Self::from_bits_retain(t);
    }

    fn get_dev_to_issue(&mut self) -> u8 {
        let t = self.bits();
        ((t >> 8) & 0xf) as u8
    }

    fn clear_dec(self) -> FisErrClear {
        FisErrClear { binding: self }
    }
}

unsafe impl ClearReserved for FisSwitchingCtl {
    fn clear_reserved(&mut self) {
        self.remove(Self::DEVICE_ERROR_CLEAR);
        let t = self.bits() & 0xfff07;
        *self = Self::from_bits_retain(t);
    }
}

struct FisErrClear {
    binding: FisSwitchingCtl,
}

unsafe impl Acknowledge<FisSwitchingCtl> for FisErrClear {
    fn ack(self) -> FisSwitchingCtl
    where
        Self: Sized,
    {
        let t = self.binding.bits() & 0xfff07;
        FisSwitchingCtl::from_bits_truncate(t)
    }
}

/// PxDEVSLP
///
/// Some fields in this register are RO/RW these fields will be marked as (CD1).
/// These fields are Writable when [super::general_control::HbaCapabilitiesExt::DEVICE_SLEEP]
/// and [DevSleep::sleep_present] are both set. Otherwise they should be treated as reserved.
#[derive(Debug)]
struct DevSleep {
    inner: u32,
}

impl DevSleep {
    // power states currently not used.
    #![allow(dead_code)]

    /// DM (RO)
    ///
    /// Multiplies the value returned by [Self::get_dev_idle_timeout].
    fn get_dito_mul(&self) -> u8 {
        (((self.inner >> 25) & 0xf) as u8) + 1
    }

    /// DITO (CD)
    fn get_dev_idle_timeout_raw(&self) -> u16 {
        ((self.inner >> 15) & 0x3f) as u16
    }

    /// Sets the device idle sleep timeout. When the device has idle for `timeout` milliseconds the
    /// device will be set to sleep. The maximum allowed value is `1023`
    ///
    /// This register writable when [super::general_control::HbaCapabilitiesExt::DEVICE_SLEEP]
    /// and [super::general_control::HbaCapabilitiesExt::AGGRESSIVE_DEVICE_SLP_MANAGEMENT] and
    /// [Self::sleep_present] are set.
    ///
    /// This field shall not be changed when [CommStatus::START] or [Self::is_enabled] are both cleared.  
    fn set_dev_idle_timeout_raw(&mut self, timeout: u16) {
        assert!(timeout < 1023);

        self.inner &= !(0x3ff << 15);
        self.inner |= (timeout as u32) << 15;
    }

    /// Returns the idle timeout in milliseconds.
    fn get_idle_timeout_adjusted(&self) -> u16 {
        let t = self.get_dev_idle_timeout_raw();
        t * (self.get_dito_mul() as u16)
    }

    /// MDAT (CD1)
    ///
    /// Specifies the amount of time that DEVSLP signal will be asserted.
    fn get_min_assertion_time(&self) -> u8 {
        ((self.inner >> 10) & 0x1f) as u8
    }

    /// Sets the minimum device sleep assertion time to `duration` milliseconds.
    /// The maximim allowed value is `31`
    ///
    /// # Safety
    ///
    /// This should not be called while [CommStatus::START] or [Self::is_enabled] are set, and
    /// should be set before calling `CommStatus::set_power_state(PowerState::DevSlp)`.
    unsafe fn set_min_assertion_time(&mut self, duration: u8) {
        assert!(duration < 32);

        self.inner &= !(0x1f << 10);
        self.inner |= (duration as u32) << 10;
    }

    /// DETO (CD1)
    ///
    /// Gets the maximum amount of time as milliseconds that DEVSLP will be deasserted until the
    /// device is ready to accept OOB.
    fn get_dev_exit_timeout(&self) -> u8 {
        (self.inner >> 2) as u8
    }

    /// See [Self::get_dev_exit_timeout].
    ///
    /// # Safety
    ///
    /// This should not be called while [CommStatus::START] or [Self::is_enabled] are set, and
    /// should be set before calling `CommStatus::set_power_state(PowerState::DevSlp)`.
    unsafe fn set_device_exit_timeout(&mut self, duration: u8) {
        self.inner &= 0xff << 2;
        self.inner |= (duration as u32) << 2
    }

    /// DSP (RO)
    fn sleep_present(&self) -> bool {
        if self.inner & 2 != 0 {
            true
        } else {
            false
        }
    }

    /// ADSE (CD1)
    ///
    /// When enabled, when the device has been idle for [Self::get_idle_timeout_adjusted] it asset
    /// DEVSLP for the specified minimum assertion time and the interface is in slumber.  
    fn set_enable(&mut self, state: bool) {
        if state {
            self.inner |= 1;
        } else {
            self.inner &= !1;
        }
    }

    fn is_enabled(&self) -> bool {
        if self.inner & 1 != 0 {
            true
        } else {
            false
        }
    }
}

#[repr(transparent)]
#[derive(Debug)]
struct CmdIssue(core::cell::Cell<u32>);

impl CmdIssue {
    fn issue(&self, cmd: u8) {
        atomic::fence(atomic::Ordering::SeqCst);
        if self.0.get() & 1 << cmd != 0 {
            panic!("Command in progress")
        }
        self.0.set(1 << cmd);
        core::hint::black_box(self);
        atomic::fence(atomic::Ordering::SeqCst);
    }
}
