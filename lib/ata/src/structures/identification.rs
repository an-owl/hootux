use core::fmt::{Debug, Formatter};

const _ASSERT: () = {
    assert!(core::mem::size_of::<DeviceIdentity>() == 512);
    assert!(core::mem::size_of::<TransferConfig>() == 14);
};

/// This struct is returned by [crate::command::AtaCommand::IDENTIFY_DEVICE]. It represents the
/// current device configuration most contained values are static but some may be changed.
/// This struct cannot be used configure the device.
// A lot of the fields in this are marked as obsolete by the ACS-4 standard
#[repr(C)]
#[derive(Debug)]
pub struct DeviceIdentity {
    pub general_cfg: GeneralCfg,
    ob0: u16,
    pub specific_cfg: u16,
    ob1: [u16; 4],
    res_cfa: [u16; 2],
    re0: u16,
    pub serial: [u8; 20],
    re1: [u16; 2],
    ob2: u16,
    pub firmware_vers: [u8; 8],
    pub model_num: [u8; 40],
    ob3: u16,
    pub trusted_computing: TrustedComputing,
    pub capabilities: Capabilities,
    pub cap_50: Capabilities50,
    ob4: [u16; 2],
    pub wd_53: u16,
    ob5: [u16; 5],
    pub sanitize_sub_cmd: SanitizeSubcommands,
    // LBAs for 24 bit cmds
    pub lba_28: u32,
    ob6: u16,
    pub multiword_dma: MultiwordDma,
    // word 64
    pub wd_64_70: TransferConfig,
    res_for_command: [u16; 4], // reserved for IDENTIFY_PACKET_DEVICE (ACS-3)
    pub queue_depth: QueueDepth,
    pub sata_cap: SataCap,
    pub sata_cap2: SataCap2,
    pub sata_features: SataFeatures,
    pub sata_features_en: SataFeaturesEnabled,
    pub major_version: MajorVersion, // word 80
    pub minor_version: u16, // see spec 7.13.6.39. if a driver wants to check this then that's it's fault
    pub features: FeaturesSet,
    pub features_copy: FeaturesSet,
    pub ultra_dma: UltraDma,
    pub erase_time: EraseTime,
    pub enhanced_erase_time: EraseTime,
    pub apm_level: AdvancedPowerManagement,
    pub master_passwd_id: u16,
    pub reset_results: u16, // this is a bunch of pata stuff
    ob7: u16,
    pub streaming: Streaming,
    pub logical_sectors: u64,
    pub stream_transfer_pio: u16,
    pub max_sectors_per_sdm: u16,
    pub sector_geom: SectorGeom,
    pub seek_speed_for_sound_test: u16,
    pub world_wide_name: u64,
    re2: [u16; 4],
    ob8: u16,
    pub logical_sector_size: LbaSize, // count in words not bytes
    pub features119: Features119,
    pub features119_copy: Features119,
    re3: [u16; 6],
    ob9: u16,
    pub security_status: SecurityStatus,
    vendor_specific: [u16; 31],
    cfa_res: [u16; 8],
    pub form_factor: FormFactor,
    pub data_management: DataManagement,
    pub additional_product_id: [u8; 8],
    re4: [u16; 2],
    pub current_media_serial: [u8; 40],
    pub current_media_manufacturer: [u8; 20],
    pub sct_command_trans: SCTCommandTransport,
    re5: [u16; 2],
    pub sector_alignment: SectorAlignment,
    pub wrv_mode_3_count: u32,
    pub wrv_mode_2_count: u32,
    oba: [u16; 3],
    pub rpm: RotationRateField,
    re6: u16,
    ob219: u16,
    pub wrv_mode: WriteReadVerifyMode,
    re221: u16,
    pub transport_major_version: TransportMajorVersion,
    pub transport_minor_version: TransportMinorVersionField,
    re7: [u16; 6],
    pub sector_count_ext: SectorCountExt,
    /// Minimum number fo 512 byte blocks required to download microcode
    pub micro_blocks_min: u16,
    /// Maximum number of 512 byte blocks required to download microcode
    pub micro_blocks_max: u16,
    re8: [u16; 19],
    checksum: Integrity,
}

#[repr(C)]
#[derive(Debug)]
pub struct TransferConfig {
    pub pio_mode: PioMode,
    // TODO see ata spec 9.11.9.4.2
    pub min_dma_time: u16,
    pub recommended_min_dma_time: u16,
    pub min_pio_time: u16,
    pub min_pio_time_iordy: u16,
    pub additional_supported: AdditonalSupport,
    _res: u16,
}

#[repr(C)]
#[derive(Debug)]
pub struct FeaturesSet {
    pub features_82: Features82,
    pub features_83: Features83,
    pub features_84: Features84,
}

const DECODE_FAILED: &str = "Failed to decode ata string";

impl DeviceIdentity {
    /// Returns true on a good checksum, otherwise returns false.
    /// This should be called before any other data is read from this struct.
    ///
    /// If this fn returns false this may indicate a hardware failure on the HBA cable or device.
    pub fn checksum(&self) -> bool {
        if !self.checksum.validity == 0xa5 {
            return false;
        }

        let mut sum = 0u8;
        let arr = unsafe { &*(self as *const _ as *const [u8; 511]) };
        for i in arr {
            sum = sum.wrapping_add(*i)
        }

        sum == 0
    }

    /// Returns the general config of the device
    pub fn get_general_config(&self) -> GeneralCfg {
        self.general_cfg
    }

    /// Returns the Specific config of the device indicating weather or not the device requires powering up
    pub fn get_specific_cfg(&self) -> SpecificCfg {
        match self.specific_cfg {
            0x37c8 => SpecificCfg::ReqSpinUpInCom,
            0x738c => SpecificCfg::ReqSpinUpCom,
            0x8c73 => SpecificCfg::NoSpinUpInCom,
            0xc837 => SpecificCfg::NoSpinUpCom,
            r => SpecificCfg::Reserved(r),
        }
    }

    pub fn get_serial(&self) -> &str {
        core::str::from_utf8(&self.serial).unwrap_or(DECODE_FAILED)
    }

    pub fn firmware_revision(&self) -> &str {
        core::str::from_utf8(&self.firmware_vers).unwrap_or(DECODE_FAILED)
    }

    pub fn model_num(&self) -> &str {
        core::str::from_utf8(&self.model_num).unwrap_or(DECODE_FAILED)
    }

    pub fn free_fall_sensitivity(&self) -> u8 {
        (self.wd_53 >> 8) as u8
    }

    fn word_88_valid(&self) -> bool {
        self.wd_53 & (1 << 2) != 0
    }

    fn word_64_70_valid(&self) -> bool {
        self.wd_53 & (1 << 1) != 0
    }

    /// Checks if the transfer config is valid and returns it if it is,
    pub fn get_transfer_cfg(&self) -> Option<&TransferConfig> {
        if self.word_64_70_valid() {
            Some(&self.wd_64_70)
        } else {
            None
        }
    }

    pub fn version_info(&self) -> VersionInfo {
        let maj = if (self.word_88_valid()) && self.major_version.contains_data() {
            Some(self.major_version)
        } else {
            None
        };

        VersionInfo {
            major_vers: maj,
            minor_vers: self.minor_version,
            transport_major: self.transport_major_version.get_version(),
            transport_minor: self.transport_minor_version.get_version(),
        }
    }

    /// This function checks if a command is supported.
    /// This function returns an Option<bool>. When this fn returns `Some(b)` the support of the
    /// command is indicated by `b`. If this fn returns `None` the command has no check implemented for it.
    ///
    /// Ths function can be used to check all command sets defined in [crate::command]   
    // todo support checking features
    pub fn is_supported<C: crate::command::CheckableCommand + Copy + 'static>(
        // would not build without static idk why this is not a ref
        &self,
        cmd: C,
    ) -> Option<bool> {
        use core::any::Any;
        // checks against concrete type and casts to it these checks are optimized out

        // this function is and probably always will be a fucking mess
        if cmd.type_id() == crate::command::AtaCommand::READ_LOG_DMA_EXT.type_id() {
            let cmd = unsafe { *(&cmd as *const _ as *const crate::command::AtaCommand) };
            return self.chk_ata_cmd(cmd);
        } else if cmd.type_id() == crate::command::SanitiseSubcommand::OVERWRITE_EXT.type_id() {
            let cmd = unsafe { *(&cmd as *const _ as *const crate::command::SanitiseSubcommand) };
            return Some(self.sanitize_sub_cmd.is_supported(cmd));
        }

        None
    }

    /// Internal component for [Self::is_supported] for checking [crate::command::AtaCommand]
    fn chk_ata_cmd(&self, cmd: crate::command::AtaCommand) -> Option<bool> {
        if let Some(n) = self.features.features_82.is_supported(cmd) {
            Some(n)
        } else if let Some(n) = self.features.features_83.is_supported(cmd) {
            Some(n)
        } else if let Some(n) = self.features119.is_supported(cmd) {
            Some(n)
        } else if cmd == crate::command::AtaCommand::SANITIZE_DEVICE {
            Some(
                self.sanitize_sub_cmd
                    .is_supported(crate::command::SanitiseSubcommand::SANITIZE_STATUS_EXT),
            )
        } else {
            None
        }
    }

    pub fn world_wide_name(&self) -> Option<u64> {
        if self
            .features
            .features_84
            .contains(Features84::WORLD_WIDE_NAME)
        {
            Some(self.world_wide_name)
        } else {
            None
        }
    }

    pub fn current_power_level(&self) -> ApmLevel {
        self.apm_level.get_mode()
    }

    /// Returns the rotation rate of the device. This may include the actual rotation rate or some
    /// other indicator suck as indicating this is an SSD.
    ///
    /// Returning `None` indicates a hardware error
    pub fn get_dev_rpm(&self) -> Option<RotationRate> {
        self.rpm.get_rate()
    }

    /// Returns the transport version used by the device.
    ///
    /// This fn will return `None` if the major version is not reported correctly or not supported.
    /// If this fn returns `Some(_,None)` this may indicate a minor version that is not recognized.
    pub fn get_transport_version(&self) -> Option<(TransportIf, Option<TransportMinorVersion>)> {
        Some((
            self.transport_major_version.get_version()?,
            self.transport_minor_version.get_version(),
        ))
    }

    /// Returns the number of command slots this device supports (maximum 32)
    pub fn queue_depth(&self) -> u8 {
        self.queue_depth.get_depth()
    }

    pub fn interface_properties(&self) -> InterfaceProperties {
        unimplemented!()
    }

    pub fn get_device_geometry(&self) -> DeviceGeometry {
        // I'm not entirely sure if i should be using logical_sectors or sector_count_ext
        let lba = if self.get_transfer_cfg().map_or(false, |t| {
            t.additional_supported
                .contains(AdditonalSupport::EXTENDED_SECTOR_ADDRESSES)
        }) {
            self.sector_count_ext.read()
        } else if self.lba_28 == 0xfff_ffff {
            self.logical_sectors & 0xffff_ffff_ffff
        } else {
            self.lba_28 as u64
        };

        let logical_sec_size = if self
            .sector_geom
            .contains(SectorGeom::LOGICAL_GREATHER_512_BYTES)
        {
            self.logical_sector_size.read() << 1 // converts from words to bytes
        } else {
            512
        };

        let phys_sec_size = if self
            .sector_geom
            .contains(SectorGeom::MULTIPLE_LOGICAL_PER_PHYS)
        {
            (logical_sec_size as u64) * self.sector_geom.log_sec_per_phys() as u64
        } else {
            logical_sec_size as u64
        };

        DeviceGeometry {
            sector_count: lba,
            logical_sec_size,
            phys_sec_size,

            alignment: self.sector_alignment.get_alignment(),
        }
    }
}

/// This struct contains device version and interface information. Some fields are optional because
/// they may or may not be reported by hardware.
///
/// - `major_vers`: This field may or may not be present if not it will be set to `None`.
/// This field contains information about the command sets supported by the device.
/// (currently this library only supports "ACS-4")
/// - `minor_vers`: This field contains the minor version of the device. A driver may check this
/// field but its potential values are not specified in this crate.
/// - `transport_major`: This is only `None` if the value is not supported or faulty. Contains the
/// transport interface nad version it is using.
/// - `transport_minor`: This is only `None` if the value is not supported or faulty. This contains
/// the minor version of transport interface.
pub struct VersionInfo {
    pub major_vers: Option<MajorVersion>,
    pub minor_vers: u16,
    pub transport_major: Option<TransportIf>,
    pub transport_minor: Option<TransportMinorVersion>,
}

#[repr(C)]
#[derive(Debug)]
pub struct Streaming {
    /// Number of sectors that provides optimum performance in streaming environments.
    /// Starting LBAs for streaming commands should be divisable by this value.
    min_req_size: u16,

    // is this in msec?
    access_time: u16,

    access_latency: u16,

    // this field has predefined values but i cant find what they are
    // this is supposed to be a u32 but it is misaligned in the DeviceIdentity
    perf_granularity_low: u16,
    perf_granularity_high: u16,
}

impl Streaming {
    pub fn perf_granularity(&self) -> u32 {
        let t = (self.perf_granularity_high as u32) << 16;
        t | self.perf_granularity_low as u32
    }
}

/// Contains the device geometry.
///
/// - Logical sectors are the blocks which are addressed by software.
/// - Physical sectors are the blocks accessed by hardware.
///
/// When `self.multiple_lbas_per_sector == true` software should align operations to physical sector
/// boundaries to optimize performance.
#[derive(Debug)]
pub struct DeviceGeometry {
    sector_count: u64,
    /// This field contains the size of each LBA
    pub logical_sec_size: u32,
    /// This field contains the physical size of each sector in the device.
    /// Operations should be aligned to physical sector boundaries for optimal performance.
    pub phys_sec_size: u64,

    alignment: u16,
}

impl DeviceGeometry {
    /// Gets the number of logical sectors on the device.
    pub fn lba_count(&self) -> u64 {
        self.sector_count
    }

    /// Gets size in bytes of the logical sectors on the device.
    pub fn logical_sec_size(&self) -> u32 {
        self.logical_sec_size
    }

    /// Gets the size in bytes of the physical sectors the device.
    pub fn phys_sec_size(&self) -> u64 {
        self.phys_sec_size
    }

    /// Returns the offset of the first lba into the first physical sector.
    ///
    /// For example a device with 4K physical sectors where this value is
    ///
    /// - 0: LBA 0 is the start of the first physical sector.
    /// - 1: LBA 0 starts at byte 512 if the first physical sector.
    /// - 2: LBA 0 starts at byte 1024 if the first physical sector.
    pub fn get_alignment(&self) -> u16 {
        self.alignment
    }
}

#[repr(transparent)]
#[derive(Copy, Clone, Debug)]
pub struct GeneralCfg(u16);

impl GeneralCfg {
    pub fn is_ata(&self) -> bool {
        self.0 & (1 << 15) == 0
    }

    pub fn is_complete(&self) -> bool {
        self.0 & (1 << 2) == 0
    }
}

// TODO see ata spec 4.17
#[derive(Debug, Eq, PartialEq, Copy, Clone)]
pub enum SpecificCfg {
    /// Device requires spin up, identification data is incomplete.
    ReqSpinUpInCom,
    /// Device Requires spin up and identification data is complete.
    ReqSpinUpCom,
    /// Device does not require spin up, identification data is incomplete.
    NoSpinUpInCom,
    /// Device does not require spin up and identification data is complete.
    NoSpinUpCom,
    Reserved(u16),
}

// TODO see ata spec 7.12.6.16
#[repr(transparent)]
#[derive(Debug)]
pub struct TrustedComputing(pub u16);

impl TrustedComputing {
    pub fn is_supported(&self) -> bool {
        self.0 & 1 != 0
    }
}

pub struct InterfaceProperties {}

bitflags::bitflags! {
    // TODO see ata spec 7.12.6.17
    #[repr(transparent)]
    #[derive(Debug)]
    pub struct Capabilities: u16 {
        /// When clear the standby timer values are vendor specific
        const STANDARD_STANDBY_TIMER = 1 << 13;
        /// When clear IORDY may be supported
        // todo idk what IORDY is
        const IORDY_SUPPORTED = 1 << 11;
        // how is this helpful?
        // bit values are not specified im assuming this is for 1
        const IORDY_MAYBE_DISABLED = 1 << 10;
        const LBA_SUPPORTED = 1 << 9;
        const DMA_SUPPORTED = 1 << 8;
    }

    #[derive(Debug)]
    pub struct Capabilities50: u16 {
        const MIN_STANDBY_TIME = 1;
    }
}

impl Capabilities {
    pub fn long_sec_err_reporting(&self) -> u8 {
        (self.bits() & 3) as u8
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct SanitizeSubcommands(pub u16);

impl SanitizeSubcommands {
    /// Returns whether or not the sanitize subcommand is supported.
    /// Subcommands that return None are not checked by this field.
    ///
    /// [crate::command::SanitiseSubcommand::SANITIZE_STATUS_EXT] and [crate::command::SanitiseSubcommand::SANITIZE_FREEZE_LOCK_EXT]
    /// return the same value as [crate::command::AtaCommand::SANITIZE_DEVICE]. This should
    /// be used to check whether the sanitize command set is available by [DeviceIdentity::is_supported]
    fn is_supported(&self, cmd: super::super::command::SanitiseSubcommand) -> bool {
        use super::super::command::SanitiseSubcommand;
        if self.0 & (1 << 12) != 0 {
            match cmd {
                SanitiseSubcommand::CRYPTO_SCRAMBLE_EXT => self.0 & (1 << 13) != 0,
                SanitiseSubcommand::BLOCK_ERASE_EXT => self.0 & (1 << 15) != 0,
                SanitiseSubcommand::OVERWRITE_EXT => self.0 & (1 << 14) != 0,
                SanitiseSubcommand::SANITIZE_ANTIFREEZE_LOCK_EXT => self.0 & (1 << 10) != 0,
                _ => true,
            }
        } else {
            false
        }
    }

    /// Sanitize commands conform to the ACS-2 standard other it conforms to the ACS-4 standard
    pub fn is_acs2(&self) -> bool {
        self.0 & (1 << 11) == 0
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct MultiwordDma(u16);

impl MultiwordDma {
    pub fn max_supported_mode(&self) -> Option<MultiwordDmaMode> {
        let t = self.0 & 7;
        if t == 0 {
            return None;
        }

        if t > 4 {
            Some(MultiwordDmaMode::Mode2)
        } else if t > 2 {
            Some(MultiwordDmaMode::Mode1)
        } else if 1 == 1 {
            Some(MultiwordDmaMode::Mode0)
        } else {
            None // this arm is a hardware error
        }
    }

    pub fn is_selected(&self, mode: MultiwordDmaMode) -> bool {
        match mode {
            MultiwordDmaMode::Mode2 => self.0 & (1 << 10) != 0,
            MultiwordDmaMode::Mode1 => self.0 & (1 << 9) != 0,
            MultiwordDmaMode::Mode0 => self.0 & (1 << 8) != 0,
        }
    }
}

pub enum MultiwordDmaMode {
    Mode0,
    Mode1,
    Mode2,
}

/// For SATA mode 3 & 4 are supported
#[repr(transparent)]
#[derive(Debug)]
pub struct PioMode(u16);

impl PioMode {
    pub fn mode_4_supported(&self) -> bool {
        self.0 & (1 << 1) != 0
    }

    pub fn mode_3_supported(&self) -> bool {
        self.0 & (1 << 1) != 0
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct AdditonalSupport: u16 {
        const DETERMINISTIC_DATA_IN_TRIMM_LBA = 1 << 14;
        const LONG_PHYS_ALIGN_ERR_RORTING = 1 << 13;
        const READ_BUFF_DMA = 1 << 11;
        const WRITE_BUFF_DMA = 1 << 10;
        const DOWNLOAD_MICRO_DMA = 1 << 8;
        const NO_OPTIONAL_28_BIT_COMMANDS = 1 << 6;
        const TRIMMED_LBA_RETURNS_ZEROS = 1 << 5;
        const DEVICE_ENCRYPTS_ALL_DATA = 1 << 4;
        const EXTENDED_SECTOR_ADDRESSES = 1 << 3;
        const NON_VOLATILE_CACHE = 1 << 2;
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct QueueDepth(u16);

impl QueueDepth {
    /// Returns the maximum queue depth (maximum commands that may be active at one time)
    fn get_depth(&self) -> u8 {
        let t = (self.0 & 0x1f) as u8;
        t + 1
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct SataCap: u16 {
        /// [super::command::AtaCommand::READ_LOG_DMA_EXT] is equivalent to [super::command::AtaCommand::READ_LOG_DMA_EXT].
        // does this mean the DMA command uses PIO?
        const READ_LOG_DMA_EXT_IS_READ = 1 << 15;
        const DEVICE_AUTO_PARTIAL_TO_SLUMBER = 1 << 14;
        const HOST_AUTO_PARTIAL_TO_SLUMBER = 1 << 13;
        const SUPPORTS_NQC_PRIORIITY = 1 << 12;
        const SUPPORTS_UNLOAD_WHILE_NQC = 1 << 11;
        const SATA_PHY_EVENT_COUNTERS_LOG = 1 << 10;
        const SUPPORTS_HOST_PM_REQUESTS = 1 << 9;
        const SUPPORTS_NQC = 1 << 8;

        const SUPPORTS_SATA_GEN3 = 1 << 3;
        const SUPPORTS_SATA_GEN2 = 1 << 2;
        const SUPPORTS_SATA_GEN1 = 1 << 1;
    }

    #[derive(Debug)]
    #[repr(transparent)]
    pub struct SataCap2: u16 {
        const POWER_DISABLE_ALWAYS_ENABLED = 1 << 8;
        const SUPPORTS_DEVSLP_TO_REDUCED_PWR_STATE = 1 << 7;
        /// Device supports [super::command::AtaCommand::RECEIVE_FPDMA_QUEUED] and
        /// [super::command::AtaCommand::SEND_FPDMA_QUEUED] commands.
        const FPDMA_COMMANDS = 1 << 6;
        const SUPPORTS_NCQ_NON_DATA = 1 << 5;
        const SUPPORTS_NCQ_STREAMING = 1 << 4;
    }

    #[derive(Debug)]
    #[repr(transparent)]
    pub struct SataFeatures: u16 {
        const POWER_DISABLE = 1 << 12;
        const REBUILD_ASSIST_SET = 1 << 11;
        const HYBRID_INFORMATION = 1 << 10;
        const DEVICE_SLEEP = 1 << 8;
        const NQC_AUTOSENSE = 1 << 7;
        const SOFTWARE_SETTINGS_PRESERVATION = 1 << 6;
        const HARDWARE_FEATURE_CTL = 1 << 5;
        const IN_ORDER_DATA_DELIVERY = 1 << 4;
        const INIT_POWER_MANAGEMENT = 1 << 3;
        const DMA_SAETUP_AUTO_ACTIVATION = 1 << 2;
        const NON_ZERO_BUFF_OFFSETS = 1 << 1;
    }

    #[derive(Debug)]
    #[repr(transparent)]
    pub struct SataFeaturesEnabled: u16 {
        const REBUILD_ASSIST = 1 << 11;
        const POWER_DISABLE = 1 << 10;
        const HYBRID_INFO = 1 << 9;
        const DEVICE_SLEEP = 1 << 8;
        const AUTO_PARTIAL_TO_SLUMBER = 1 << 7;
        const SETTINGS_PRESERVATION = 1 << 6;
        const HARDWARE_FEATURE_CONTROL = 1 << 5;
        const IN_ORDER_DELIVERY = 1 << 4;
        const DEVICE_POWER_MANAGEMENT = 1 << 3;
        const DMA_SETUP_ATUO_ACTIVATION = 1 << 2;
        const NON_ZERO_BUFF_OFFSETS = 1 << 1;
    }
}

impl SataCap2 {
    // spec gives wrong section it's actually 9.11.10.3.1
    // todo add to InterfaceProperties
    pub fn get_sata_gen(&self) -> u8 {
        let t = (self.bits() & 7) as u8;
        assert!(t < 4, "Invalid SATA speed reported");
        t
    }
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Copy, Clone, Debug)]
    pub struct MajorVersion: u16 {
        const ACS_4 = 1 << 11;
        const ACS_3 = 1 << 10;
        const ACS_2 = 1 << 9;
        const ATA_8 = 1 << 8;
        const ATA_7 = 1 << 7;
        const ATA_6 = 1 << 6;
        const ATA_5 = 1 << 5;
    }
}

impl MajorVersion {
    fn contains_data(&self) -> bool {
        !(self.bits() == u16::MAX || self.bits() == 0)
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct Features82: u16 {
        const NOP = 1 << 14;
        const READ_BUFFER = 1 << 13;
        const WRITE_BUFFER = 1 << 12;
        const DEVICE_RESET = 1 << 9;
        const LOOK_AHEAD = 1 << 6;
        const VOLATILE_WRITE_CACHE = 1 << 5;
        const PACKET_FEATURES = 1 << 4;
        const POWER_MANAGEMENT_FEATURES = 1 << 3;
        const SECUTITY_FEATURES = 1 << 1;
        const SMART = 1;
    }
}

impl Features82 {
    /// Checks if the given command is supported.
    /// Returns `Some(true)` if the command is supported and `Some(false)` if it is not.
    /// If this fn returns `None` the command cannot be checked against this.
    fn is_supported(&self, cmd: super::super::command::AtaCommand) -> Option<bool> {
        use super::super::command::AtaCommand;
        match cmd {
            AtaCommand::NOP => return self.contains(Self::NOP).into(),
            AtaCommand::READ_BUFFER => return self.contains(Self::READ_BUFFER).into(),
            AtaCommand::WRITE_BUFFER => return self.contains(Self::WRITE_BUFFER).into(),
            AtaCommand::SMART => return self.contains(Self::SMART).into(),
            _ => None,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct Features83: u16 {
        const FLUSH_CACHE_EXT = 1 << 13;
        const FLUSH_CACHE = 1 << 12;
        const LBA_48 = 1 << 10;
        /// Set features is required to spin up the disk
        const SET_FEATURES_REQUIRED = 1 << 6;
        const PUIS = 1 << 5;
        const APM = 1 << 3;
        const DOWNLOAD_MICROCODE = 1;
    }
}

impl Features83 {
    fn is_supported(&self, cmd: super::super::command::AtaCommand) -> Option<bool> {
        use super::super::command::AtaCommand;
        match cmd {
            AtaCommand::FLUSH_CACHE_EXT => return self.contains(Self::FLUSH_CACHE_EXT).into(),
            AtaCommand::FLUSH_CACHE => return self.contains(Self::FLUSH_CACHE).into(),
            AtaCommand::DOWNLOAD_MICROCODE => {
                return self.contains(Self::DOWNLOAD_MICROCODE).into()
            }
            _ => None,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct Features84: u16 {
        const IDLE_IMMEDIATE_WITH_UNLOAD = 1 << 13;
        const WORLD_WIDE_NAME = 1 << 8;
        const WRITE_DMA_FUA_EXT = 1 << 6;
        const GPL_FEATURES = 1 << 5;
        const STREAMING = 1 << 4;
        const SMART_SELF_TEST = 1 << 1;
        const SMAART_ERR_LOGGING = 1;
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct UltraDma {
    selected: u8,
    supported: u8,
}

impl UltraDma {
    pub fn current(&self) -> Option<UltraDmaMode> {
        let t = self.selected;
        if t == 0 {
            return None;
        };

        match t {
            1 => Some(UltraDmaMode::Mode0),
            2 => Some(UltraDmaMode::Mode1),
            4 => Some(UltraDmaMode::Mode2),
            8 => Some(UltraDmaMode::Mode3),
            16 => Some(UltraDmaMode::Mode4),
            32 => Some(UltraDmaMode::Mode5),
            64 => Some(UltraDmaMode::Mode6),
            _ => None,
        }
    }

    pub fn is_supported(&self, mode: UltraDmaMode) -> bool {
        match mode {
            UltraDmaMode::Mode0 => self.supported & 1 != 0,
            UltraDmaMode::Mode1 => self.supported & 2 != 0,
            UltraDmaMode::Mode2 => self.supported & 4 != 0,
            UltraDmaMode::Mode3 => self.supported & 8 != 0,
            UltraDmaMode::Mode4 => self.supported & 16 != 0,
            UltraDmaMode::Mode5 => self.supported & 32 != 0,
            UltraDmaMode::Mode6 => self.supported & 64 != 0,
        }
    }
}

#[derive(Debug)]
pub enum UltraDmaMode {
    Mode0 = 1,
    Mode1 = 1 << 1,
    Mode2 = 1 << 2,
    Mode3 = 1 << 3,
    Mode4 = 1 << 4,
    Mode5 = 1 << 5,
    Mode6 = 1 << 6,
}

#[repr(transparent)]
#[derive(Debug)]
pub struct EraseTime(u16);

impl EraseTime {
    pub fn get_erase_time(&self) -> u16 {
        if self.0 & (1 << 15) != 0 {
            self.0 & !(1 << 15)
        } else {
            self.0 & 0xff
        }
    }
}

#[repr(C)]
#[derive(Debug)]
pub struct AdvancedPowerManagement {
    level: u8,
    _res: u8,
}

#[derive(Debug)]
pub enum ApmLevel {
    MinimumStandby,
    IntermediateStandby(u8),
    Min,
    Intermediate(u8),
    MaxPerformance,
    Reserved(u8),
}

impl AdvancedPowerManagement {
    pub fn get_mode(&self) -> ApmLevel {
        match self.level {
            1 => ApmLevel::MinimumStandby,
            n if n > 1 && n < 0x80 => ApmLevel::IntermediateStandby(n),
            0x80 => ApmLevel::MinimumStandby,
            n if n > 0x80 && n < 0xfe => ApmLevel::Intermediate(n),
            0xfe => ApmLevel::MaxPerformance,
            e => ApmLevel::Reserved(e),
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct SectorGeom: u16 {
        const MULTIPLE_LOGICAL_PER_PHYS = 1 << 13;
        const LOGICAL_GREATHER_512_BYTES = 1 << 12;
    }
}

impl SectorGeom {
    // todo use in DeviceGeometry
    pub fn log_sec_per_phys(&self) -> u16 {
        let shift = self.bits() & 0xf;
        1 << shift
    }
}

// required for misaligned read of u32

pub struct LbaSize {
    lba_low: u16,
    lba_high: u16,
}

impl Debug for LbaSize {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut fmt = Formatter::debug_struct(f, core::any::type_name::<Self>());
        fmt.field("len", &self.read());
        fmt.finish()
    }
}

impl LbaSize {
    fn read(&self) -> u32 {
        let t = self.lba_low as u32;
        t | (self.lba_high as u32) << 16
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct Features119: u16 {
        const DSN = 1 << 9;
        const MAX_ADDR_CFG = 1 << 8;
        const EPC = 1 << 7;
        const SENSE_DATA_REPORTING = 1 << 6;
        const FREE_FALL_CTL = 1 << 5;
        const DOWNLOAD_MICRO_MODE_3 = 1 << 4;
        const LOG_DMA_EXT = 1 << 3;
        const WRITE_UNCORRECTABLE = 1 << 2;
        const READ_WRITE_VERIFY = 1 << 1;
    }
}

impl Features119 {
    fn is_supported(&self, cmd: super::super::command::AtaCommand) -> Option<bool> {
        use super::super::command::AtaCommand;

        match cmd {
            AtaCommand::READ_LOG_DMA_EXT => Some(self.contains(Self::LOG_DMA_EXT)),
            AtaCommand::WRITE_LOG_DMA_EXT => Some(self.contains(Self::LOG_DMA_EXT)),
            AtaCommand::WRITE_UNCORRECTABLE_EXT => Some(self.contains(Self::WRITE_UNCORRECTABLE)),
            _ => None,
        }
    }
}

bitflags::bitflags! {
    #[derive(Debug)]
    #[repr(transparent)]
    pub struct SecurityStatus: u16 {
        const MASTER_PASSWORD_CAPABILITY_MAX = 1 << 8;
        const ENHANCED_SECURE_ERASE = 1 << 5;
        const COUNT_EXPIRED = 1 << 4;
        const FROZEN = 1 << 3;
        const LOCKED = 1 << 2;
        const ENABLED = 1 << 1;
        const SUPPORTED = 1;
    }
}

#[repr(transparent)]
pub struct FormFactor(u16);

impl Debug for FormFactor {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut t = f.debug_struct(core::any::type_name::<Self>());
        t.field("form_factor", &self.form());
        t.finish()
    }
}

#[allow(non_camel_case_types)]
#[derive(Debug)]
pub enum DeviceFromFactor {
    NotReported = 0,
    Size5_25, // 5.25 inch
    Size3_5,  // 3.5 inch
    Size2_5,  // 2.5 inch
    Size1_8,  // 1.8 inch
    SmallerThan1_8,
    mSATA,
    M_2,
    MicroSSD,
    CFast,
}

impl FormFactor {
    /// None indicates device firmware error
    pub fn form(&self) -> Option<DeviceFromFactor> {
        match self.0 & 0xf {
            0 => Some(DeviceFromFactor::NotReported),
            1 => Some(DeviceFromFactor::Size5_25),
            2 => Some(DeviceFromFactor::Size3_5),
            3 => Some(DeviceFromFactor::Size2_5),
            4 => Some(DeviceFromFactor::Size1_8),
            5 => Some(DeviceFromFactor::SmallerThan1_8),
            6 => Some(DeviceFromFactor::MicroSSD),
            7 => Some(DeviceFromFactor::M_2),
            8 => Some(DeviceFromFactor::MicroSSD),
            9 => Some(DeviceFromFactor::CFast),
            _ => None,
        }
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct DataManagement(u16);

impl DataManagement {
    pub fn trim_support(&self) -> bool {
        self.0 & 1 != 0
    }
}

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug)]
    pub struct SCTCommandTransport: u16 {
        const BIT_7 = 1 << 7;
        const DATA_TABLES = 1 << 5;
        const SCT_FEATURE_CTL = 1 << 4;
        const SCT_ERR_RECOVERY = 1 << 3;
        const SCT_WRITE_SAME = 1 << 2;
        const SCT_COMMAND_TRANSPORT = 1;
    }
}

impl SCTCommandTransport {
    pub fn get_vendor(&self) -> u8 {
        ((self.bits() >> 12) & 0xf) as u8
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct SectorAlignment(u16);

impl SectorAlignment {
    /// This value is the number of logical sectors between the beginning of physical sector 0 and LBA 0
    fn get_alignment(&self) -> u16 {
        self.0 & 0x3fff
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct RotationRateField(u16);

pub enum RotationRate {
    NotReported,
    SolidState,
    Rpm(u16),
}

impl RotationRateField {
    fn get_rate(&self) -> Option<RotationRate> {
        match self.0 {
            rpm if rpm > 0x400 && rpm < 0xffff => Some(RotationRate::Rpm(rpm)),
            0 => Some(RotationRate::NotReported),
            1 => Some(RotationRate::SolidState),
            _ => None,
        }
    }
}

#[repr(transparent)]
#[derive(Debug)]
pub struct WriteReadVerifyMode(u16);

pub enum WriteReadVerify {
    /// Always write read verify regardless of command,
    Always,
    /// Uses write read verify on the first 64K logical sectors written.
    Check64K,
    /// Vendor specific definition
    VendorSpecific,
    /// Checks first number of logical sectors defined at runtime.
    /// The the number of sectors checked is `count * 1024`
    // todo defined how?
    CheckFromCounter,
}

impl WriteReadVerifyMode {
    pub fn get_mode(&self) -> Option<WriteReadVerify> {
        match self.0 {
            0 => Some(WriteReadVerify::Always),
            1 => Some(WriteReadVerify::Check64K),
            2 => Some(WriteReadVerify::VendorSpecific),
            3 => Some(WriteReadVerify::CheckFromCounter),
            _ => None,
        }
    }
}

pub struct TransportMajorVersion(u16);

impl Debug for TransportMajorVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut t = f.debug_struct(core::any::type_name::<Self>());
        if let Some(tif) = self.get_version() {
            t.field("if", &tif);
        } else {
            t.field("if", &"Unknown");
        };

        t.finish()
    }
}

#[derive(Debug, Copy, Clone)]
pub enum TransportIf {
    Parallel(ParallelVersion),
    Serial(SerialVersion),
    Pcie,
}

#[derive(Copy, Clone, Debug)]
pub enum ParallelVersion {
    Ata7,
    Ata8,
}

impl TryFrom<u16> for ParallelVersion {
    type Error = u16;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        let t = value & 0xfff;
        match t {
            0 => Ok(Self::Ata8),
            1 => Ok(Self::Ata7),
            e => Err(e),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum SerialVersion {
    Sata3_2,
    Sata3_1,
    Sata3_0,
    Sata2_6,
    Sata2_5,
    Sata2,
    Sata1,
    Ata8,
}

impl TryFrom<u16> for SerialVersion {
    type Error = u16;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        let t = value & 0xfff;

        match t {
            0 => Ok(Self::Ata8),
            1 => Ok(Self::Sata1),
            2 => Ok(Self::Sata2),
            3 => Ok(Self::Sata2_5),
            4 => Ok(Self::Sata2_6),
            5 => Ok(Self::Sata3_0),
            6 => Ok(Self::Sata3_1),
            7 => Ok(Self::Sata3_2),
            e => Err(e),
        }
    }
}

impl TransportMajorVersion {
    fn get_version(&self) -> Option<TransportIf> {
        let t = (self.0 >> 12) & 0xf;
        match t {
            0 => Some(TransportIf::Parallel(self.0.try_into().ok()?)),
            1 => Some(TransportIf::Serial(self.0.try_into().ok()?)),
            0xe => Some(TransportIf::Pcie),
            _ => None,
        }
    }
}

pub struct TransportMinorVersionField(u16);

impl Debug for TransportMinorVersionField {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut t = f.debug_struct(core::any::type_name::<Self>());
        if let Some(v) = self.get_version() {
            t.field("version", &v);
        } else {
            t.field("version", &"Unknown");
        }

        t.finish()
    }
}

#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone)]
pub enum TransportMinorVersion {
    NotReported,
    Ata8_AstT13ProjectD1697V0b,
    Ata_AstT13ProjectD1697V1,
}

impl TransportMinorVersionField {
    fn get_version(&self) -> Option<TransportMinorVersion> {
        match self.0 {
            0 | u16::MAX => Some(TransportMinorVersion::NotReported),
            0x21 => Some(TransportMinorVersion::Ata8_AstT13ProjectD1697V0b),
            0x51 => Some(TransportMinorVersion::Ata_AstT13ProjectD1697V1),
            _ => None,
        }
    }
}

#[repr(C, align(4))]
pub struct SectorCountExt {
    low: u32,
    high: u32,
}

impl SectorCountExt {
    // todo DeviceGeometry
    fn read(&self) -> u64 {
        unsafe { core::ptr::read_unaligned(self as *const _ as *const u64) }
    }
}

impl Debug for SectorCountExt {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        let mut t = f.debug_struct(core::any::type_name::<Self>());
        t.field("inner", &self.read());

        t.finish()
    }
}

#[derive(Debug)]
#[repr(C)]
struct Integrity {
    validity: u8,
    checksum: u8,
}
