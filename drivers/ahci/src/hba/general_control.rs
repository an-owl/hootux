use crate::register::{ClearReserved, ReadOnly, Register};

pub(crate) struct GeneralControl {
    /// CAP
    capabilities: Register<HbaCapabilities, ReadOnly>,
    host_ctl: Register<GlobalHbaCtl>,
    interrupt_status: DwordHighClear, // does not use  register because it is W1C
    /// PI
    ///
    /// Ports implemented. Bitfield corresponding to ports implemented by the hba.
    ports: Register<PortImplemented, ReadOnly>,
    version: Register<VersionRegister>,
    ccc_control: Register<CCCControl>,
    ccc_ports: Register<u32>,
    enclosure_management: Register<EnclosureManagement>,
    capabilities_2: Register<HbaCapabilitiesExt>,
    bios_handoff_ctl_status: Register<FirmwareHandoffCtl>,
}

impl GeneralControl {
    pub fn get_capabilities(&self) -> (HbaCapabilities, HbaCapabilitiesExt) {
        (
            self.capabilities.inner().clone(),
            self.capabilities_2.inner().clone(),
        )
    }

    /// Checks whether the `port` is implemented by the HBA
    ///
    /// # Panics
    ///
    /// This fn will panic if `port >= 32`
    pub fn check_port(&self, port: u8) -> bool {
        self.ports.read().implemented(port)
    }
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Copy, Clone, Debug)]
    /// CAP (RO)
    pub(crate) struct HbaCapabilities: u32 {
        /// S64A
        ///
        /// When this is present the device supports 64 bit addressing for all address registers.
        const QWORD_BIT_ADDRESSING = 1 << 31;
        /// SNCQ
        ///
        /// When this is present this device supports native command queuing.
        /// If this is not present NCQ commands must not be issued
        // todo but what if I do?
        const NATIVE_COMMAND_QUEUEING = 1 << 30;
        /// SSNTF
        ///
        /// When this is set the HBA supports the PxSNTF register
        const SNOTIFICATION = 1 << 29;
        /// SMPS
        ///
        /// The device supports a mechanical presence switch for hot plug devices.
        // todo where are the presence bits?
        const PRESENCE_SWITCH = 1 << 28;
        /// SSS
        ///
        /// The device supports supports staggered spin up. This feature helps to prevent large
        /// power spikes on startup.
        // todo link relevant port bits
        const STAGGERED_SPIN_UP = 1 << 27;
        /// SALP
        ///
        /// Aggressive link management allows using timers to automatically reduce power mode of
        /// the connected device.
        const AGGRESSIVE_LINK_POWER_MANGEMENT = 1 << 26;
        /// SAL
        ///
        /// This device supports an activity LED.
        const ACTIVITY_LED = 1 << 25;
        /// SCLO
        ///
        /// The HBA supports overriding the command list to preform a device reset.
        /// When this is present the [super::port_control::CommStatus::COMMAND_LIST_OVERRIDE] but
        /// may be used.
        const COMMAND_LIST_OVERRIDE = 1 << 24;
        /// SAM
        ///
        /// Then this bit is present the device cannot be used as a legacy IDE controller
        // this doesnt really matter anyway because the mode will be set by firmware
        const AHCI_MODE_ONLY = 1 << 18;
        /// SPM
        ///
        /// When present the HBA supports communicating with port multipliers using command based
        /// switching.
        // command based switching is not implemented
        const PORT_MULTIPLIER = 1 << 17;
        /// FBSS
        ///
        /// When this but is present the device supports FIS based switching. This uses the port
        /// field a sent the FIS to identify the target device.
        const FIS_BASED_SWITCHING = 1 << 16;
        /// PMD
        ///
        /// When this bit is present this the HBA supports more than one DRQ block data transfers for the PIO
        /// command protocol.
        // I have no idea what this means
        const PIO_MULTIPLE_DRQ_BLOCK = 1 << 15;
        /// SSC
        ///
        /// When this bit is present the HBA supports transitioning devices to the slumber mode to
        /// reduce power usage.
        const SLUMBER_STATE = 1 << 14;
        /// PSC
        ///
        /// When present the HBA supports transitioning devices into the partial power state
        const PARTIAL_STATE = 1 << 13;
        /// CCCS
        ///
        /// Indicates that the HBA supports the Command Completion Coalescing protocol.
        /// CCC is used to reduce the number of interrupts generated, to allow multiple command
        /// completions to be signalled by a single interrupt.
        const COMMAND_COMPLETION_COALESCING = 1 << 7;
        /// EMS
        ///
        /// Indicates that the HBA supports enclosure management
        const ENCLOSURE_MANAGEMENT = 1 << 6;
        /// SXS
        ///
        /// Indicates that at leas one port uses an eSATA connector.
        const E_SATA = 1 << 5; // lamo as if
    }

    #[repr(transparent)]
    #[derive(Copy, Clone, Debug)]
    /// CAP2 (RO)
    pub(crate) struct HbaCapabilitiesExt: u32 {
        /// `DESO`
        const DEVSLP_FROM_SLUMBER_ONLY = 1 << 5;
        /// `SADM`
        const AGGRESSIVE_DEVICE_SLP_MANAGEMENT = 1 << 4;
        /// `SDS`
        const DEVICE_SLEEP = 1 << 3;
        /// `APST`
        const AUTOMATIC_PARTIAL_TO_SLEEP = 1 << 2;
        /// `NVMP`
        /// When set the NVMHCI table is present
        const NVMHCI = 1 << 1;
        /// `BOH`
        const BIOS_HANDOFF = 1;
    }

    #[repr(transparent)]
    pub(crate) struct GlobalHbaCtl: u32 {
        const AHCI_ENABLE = 31 << 1;
        const MSI_SINGLE_MESSAGE = 2 << 1;
        const INTERRUPT_ENABLE = 1 << 1;
        const HBA_RESET = 1;
    }

    #[repr(transparent)]
    pub(crate) struct EnclosureMgmtCtl: u32 {
        const ACTIVITY_LED = 1 << 26;
        const TRANSMIT_ONLY = 1 << 25;
        const SINGLE_MESSAGE_BUFFER = 1 << 25;
        const SGPIO_MANAGEMENT_MESSAGES = 1 << 19;
        const SES2_MANAGEMENT_MESSAGES = 1 << 18;
        const SAFFE_MANAGEMENT_MESSAGES = 1 << 17;
        const LET_MESSAGE_TYPES = 1 << 16;
        /// Resets enclosure management subsystem. This bit cannot be unset by software.
        const RESET = 1 << 9;
        /// When this is set the message contained in the send buffer will be sent. After the
        /// message is sent this is unset. Software cannot unset this bit
        const TRANSMIT = 1 << 8;
        /// When this this is set a message has been received into the receive buffer. When the
        /// buffer has been read this should be set 1 to clear.
        // TODO = Does this prevent the next message from being written into the
        const MESSAGE_RECIEVED = 1;
    }

    pub(crate) struct FirmwareHandoffCtl: u32 {
        /// Indicates that the firmware is cleaning up for ownership change.
        const FW_BUSY = 1 << 4;
        /// Indicates that the ownership has changed. Write 1 to clear this.
        const OWNERSHIP_CHANGE = 1 << 3;
        /// Sends an SMI when ownership is requested by the OS
        const SMI_ON_OWNERSHIP_CHANGE = 1 << 2;
        /// Used to request ownership from the firmware. When [Self::FW_OWNED_SEMAPHORE] is false
        /// the OS has ownership of the HBA.
        const OS_OWNED_SEMAPHORE = 1 << 1;
        /// Indicates that the firmware has ownership of the HBA. When this bit is clear the OS has ownership of the HBA.
        const FW_OWNED_SEMAPHORE = 1;
    }
}

unsafe impl ClearReserved for FirmwareHandoffCtl {
    fn clear_reserved(&mut self) {
        *self = Self::from_bits_truncate(self.bits())
    }
}

impl HbaCapabilities {
    pub fn supports_qword_addr(&self) -> bool {
        self.contains(Self::QWORD_BIT_ADDRESSING)
    }
    /// Retrieves the maximum interface speed supported as an integer.
    /// The returned value is the link generation number.
    ///
    /// Gen1: up to 1.5Gib/s
    /// Gen2: up to 3Gib/s
    /// Gen3: up to 6Gib/s
    pub fn get_if_speed(&self) -> u8 {
        let mut t: u32 = self.bits();
        t >>= 20;
        t &= 7;
        t as u8
    }

    /// Returns the maximum number of command slots per port
    pub fn get_command_slots(&self) -> u8 {
        let mut t: u32 = self.bits();
        t >>= 8;
        t as u8 & 0xf
    }

    /// Gets the number of supported ports.
    pub fn port_count(&self) -> u8 {
        let t: u32 = self.bits();
        t as u8 & 0xf
    }
}

/// Contain the current interrupt state of each port.
/// Each bit `n` represents the status of port `n`
#[repr(transparent)]
struct DwordHighClear {
    inner: u32,
}

impl DwordHighClear {
    /// Clears the interrupt for `port`
    pub fn clear(&mut self, port: u8) {
        let mask = 1 << port;
        // SAFETY: This is safe because dst is taken by reference
        unsafe { core::ptr::write_volatile(&mut self.inner, mask) }
    }

    /// Clears vectors where the bits in `mask` are 1
    ///
    /// ```ignore
    /// let mut t = DwordHighClear{inner: 0xffffffff};
    /// t.clear_multiple(0xff);
    ///
    /// assert_eq!(t.read(),0xffffff00)
    ///
    /// ```
    pub fn clear_multiple(&mut self, mask: u32) {
        unsafe { core::ptr::write_volatile(&mut self.inner, mask) }
    }

    /// Returns the raw value of the register.
    pub fn read(&self) -> u32 {
        unsafe { core::ptr::read_volatile(&self.inner) }
    }
}

#[repr(transparent)]
struct PortImplemented(u32);

impl PortImplemented {
    /// Checks whether the `port` is implemented by the HBA
    ///
    /// # Panics
    ///
    /// This fn will panic if `port >= 32`
    pub fn implemented(&self, port: u8) -> bool {
        assert!(port < 32);

        self.0 & (1 << port) != 0
    }
}

#[repr(transparent)]
struct VersionRegister {
    inner: u32,
}

impl VersionRegister {
    pub fn maj_min(&self) -> (u16, u16) {
        // Im pretty sure these do not need to be volatile
        let low = self.inner as u16;
        let high = (self.inner >> 16) as u16;
        (high, low)
    }

    pub fn get_version(&self) -> Option<HbaVersion> {
        HbaVersion::try_from(self.maj_min()).ok()
    }
}

#[non_exhaustive]
pub enum HbaVersion {
    Vers0_95,
    Vers1,
    Vers1_1,
    Vers1_2,
    Vers1_3,
    Vers1_3_1,
}

impl TryFrom<(u16, u16)> for HbaVersion {
    type Error = ();

    fn try_from(value: (u16, u16)) -> Result<Self, Self::Error> {
        match value {
            (0, 0x0905) => Ok(Self::Vers0_95),
            (1, 0) => Ok(Self::Vers1),
            (1, 0x0100) => Ok(Self::Vers1_1),
            (1, 0x0200) => Ok(Self::Vers1_2),
            (1, 0x0300) => Ok(Self::Vers1_3),
            (1, 0x0301) => Ok(Self::Vers1_3_1),
            _ => Err(()),
        }
    }
}

#[repr(transparent)]
pub(crate) struct CCCControl {
    inner: u32,
}

impl CCCControl {
    /// Sets timeout to `milis` milliseconds. Timer is used when CCC commands are in progress. When
    /// the timer reaches 0 a CCC interrupt will will be raised. The timer is reset when CCC
    /// interrupt is asserted.
    ///
    /// See AHCI specification 1.3.1 section 11 for more information
    ///
    /// # Panics
    ///
    /// This fn will panic if `milis` is `0`
    ///
    /// # Safety
    ///
    /// The caller must ensure that CCC is disabled before calling this fn
    unsafe fn set_timeout(&mut self, milis: u16) {
        assert_ne!(milis, 0);

        let mut t = self.inner;
        t &= 0xfffff;
        t |= (milis as u32) << 16;
        self.inner = t;
    }

    /// Sets the number of command completions until a CCC interrupt is asserted.
    /// When a port has a command completion a counter is incremented when it reaches `completions`
    /// a CCC interrupt is asserted and the counter is reset to `0`.
    /// A value of 0 will prevent interrupts occurring,
    ///
    /// # Safety
    ///
    /// The caller must ensure that CCC is disabled before calling this fn
    pub unsafe fn set_completions(&mut self, completions: u8) {
        let mut t = self.inner;
        t &= !(0xff << 8);
        t |= (completions as u32) << 8;
        self.inner = t
    }

    /// Sets the port used for a CCC interrupt. The port must be unused.
    pub fn set_vector(&mut self, vector: u8) {
        assert!(vector < 32);
        let mut t = self.inner;
        t &= !(31 << 3);
        t |= (vector as u32) << 3;
        self.inner = t;
    }

    /// Enables or disables CCC.
    pub fn enable(&mut self, state: bool) {
        let mut t = self.inner;
        if state {
            t |= 1;
        } else {
            t &= !1;
        }
        self.inner = t;
    }
}

#[repr(transparent)]
struct EnclosureManagement {
    inner: u32,
}

impl EnclosureManagement {
    /// Returns the offset of the message buffer from the beginning of ABAR
    // TODO: wtf is ABAR and how do i get it?
    pub fn offset(&self) -> isize {
        (self.inner >> 16) as isize * 4
    }

    /// Returns the size of the transmit buffer.
    /// If both transmit and receive buffers are supported then the transmit buffer begins at
    /// `self.offset()` and the receive buffer begins at `self.offset() + self.size()`.
    /// Both Transmit and receive buffers are the same size.
    pub fn size(&self) -> usize {
        (self.inner & (0xffff << 16)) as usize
    }
}

impl Register<FirmwareHandoffCtl> {
    /// Claims the HBA from the Firmware blocking until the Firmware releases it.
    pub fn claim_blocking(&mut self) {
        self.update(|mut t| {
            t.set(FirmwareHandoffCtl::OS_OWNED_SEMAPHORE, true);
            t
        });
        while self.read().contains(FirmwareHandoffCtl::FW_OWNED_SEMAPHORE) {
            core::hint::spin_loop();
        }
    }
}
