mod privacy {
    pub trait Sealed {}
}
/// This is a marker trait for checking command compatibility and may not be be implemented outside
/// of this crate
pub trait CheckableCommand: privacy::Sealed {}

impl<T> CheckableCommand for T where T: privacy::Sealed {}

#[repr(u8)]
#[derive(
    Eq, PartialEq, Copy, Clone, Debug, num_enum::TryFromPrimitive, num_enum::IntoPrimitive,
)]
#[allow(non_camel_case_types)]
pub enum AtaCommand {
    // Read
    /// Reads from the device using DMA.
    ///
    /// - LBA contains the first sector to be transferred.
    /// - Count contains the number of sectors to be transferred. A value of 0 is treated as 256.
    /// - Device Bit 6 contains "use LBA" bit, this ust be enabled to use LBA addressing else CHS is used.
    READ_DMA = 0xc8,
    /// See [Self::READ_DMA]
    /// - Count: Size of this field is doubled. A value of 0 is treated as 65,536
    READ_DMA_EXT = 0x25,

    // Write
    /// Writes to the disk using DMA.
    ///
    /// - LBA contains the first sector to be written
    /// - Count contains the number of sectors to be written. A value of 0 is treated as 256 sectors.
    /// - Device Bit 6 contains "use LBA" bit, this ust be enabled to use LBA addressing else CHS is used.
    WRITE_DMA = 0xca,
    /// See [Self::WRITE_DMA]
    /// - Count: Size of this field is doubled. A value of 0 is treated as 65,536
    WRITE_DMA_EXT = 0x35,
    /// Performs a [Self::WRITE_DMA_EXT] returning only after the data has been written to the non
    /// volatile medium.
    WRITE_DMA_FUA_EXT = 0x3d,

    // Diagnostic
    /// Reads an internal buffer on the device. [Self::WRITE_BUFFER] should be called before this.
    /// THe purpose of these commands is for signal checking and does not transfer data to the
    /// physical medium or cache
    READ_BUFFER = 0xe4,
    /// Writes to the devices internal buffer. for more info see [Self::READ_BUFFER]
    WRITE_BUFFER = 0xe8,
    /// See [Self::READ_BUFFER]
    READ_BUFFER_DMA = 0xe9,
    /// See [Self::READ_BUFFER]
    WRITE_BUFFER_DMA = 0xeb,

    // todo sort
    /// On completion this will return an error
    NOP = 0x00,
    CFA_REQUEST_EXT_ERR_CODE = 0x03,
    DATA_SET_MANAGEMENT = 0x06,
    DATA_SET_MANAGEMENT_XL = 0x07,
    REQ_SENSE_DATA_EXT = 0xb,
    GET_PHYSICAL_ELEMENT_STATUS = 0x12,
    READ_SECTORS = 0x20,
    READ_SECTORS_EXT = 0x24,
    READ_STREAM_DMA_EXT = 0x2a,
    READ_STREAM_EXT = 0x2b,

    READ_LOG_EXT = 0x2f,
    WRITE_SECTORS = 0x30,
    WRITE_SECTORS_EXT = 0x34,
    CFA_WRITE_SECTORS_WITHOUT_ERASE = 0x38,

    WRITE_STREAM_DMA_EXT = 0x3a,
    WRITE_STREAM_EXT = 0x3b,
    WRITE_LOG_EXT = 0x3f,
    READ_VERIFY_SECTORS = 0x40,
    READ_VERIFY_SECTORS_EXT = 0x42,
    ZERO_EXT = 0x44,
    WRITE_UNCORRECTABLE_EXT = 0x45,
    READ_LOG_DMA_EXT = 0x47,

    ZAC_MANAGEMENT_IN = 0x4a,
    CONFIG_STREAM = 0x51,
    WRITE_LOG_DMA_EXT = 0x57,
    TRUSTED_NON_DATA = 0x5b,
    TRUSTED_RECEIVE = 0x5c,

    TRUSTED_RECEIVE_DMA = 0x5d,
    TRUSTED_SEND = 0x5e,
    TRUSTED_SEND_DMA = 0x5f,
    READ_FPDMA_QUEUED = 0x60,
    WRITE_FPDMA_QUEUED = 0x61,

    NCQ_NON_DATA = 0x63,
    SEND_FPDMA_QUEUED = 0x64,
    RECEIVE_FPDMA_QUEUED = 0x65,
    SET_DATE_TIME_EXT = 0x77,
    ACCESSIBLE_MAX_ADDR_CONFIG = 0x78,
    REMOVE_ELEMENT_AND_TRUNCATE = 0x7c,

    RESTORE_ELEMENTS_AND_REBUILD = 0x7d,
    REMOVE_ELEMENT_AND_MODIFY_ZONES = 0x7e,
    CFA_TRANSLATE_SECTOR = 0x87,
    EXECUTE_EDV_DIAGNOSTIC = 0x90,

    DOWNLOAD_MICROCODE = 0x92,
    DOWNLOAD_MICROCODE_DMA = 0x93,
    ZAC_MANAGEMENT_OUT = 0x9f,

    SMART = 0xb0,
    SET_SECTOR_CONFIG_EXT = 0xb2,
    /// This function is used with [SanitiseSubcommand] as teh features field. It is used for the
    /// secure erasure of data from the device. Command args will be detailed in the subcommands.
    SANITIZE_DEVICE = 0xb4,

    CFA_WRITE_MULTIPLE_WITHOUT_ERASE = 0xcd,
    STANDBY_IMMEDIATE = 0xe0,
    IDLE_IMMEDIATE = 0xe1,
    STANDBY = 0xe2,

    IDLE = 0xe3,

    CHECK_POWER_MODE = 0xe5,
    SLEEP = 0xe6,
    /// Writes any cached data to non-volatile media.
    /// Completes when an error occurs or when all data is written.
    /// If the cache is disabled or not present this will complete normally.
    ///
    /// This command may not return errors correctly if errors occur in LBAs above 0xFFFFFF.
    /// For this reason [Self::FLUSH_CACHE_EXT] is preferred.
    FLUSH_CACHE = 0xe7,

    /// Writes any cached data to non-volatile media.
    /// Completes when an error occurs or when all data is written.
    /// If the cache is disabled or not present this will not complete normally.
    FLUSH_CACHE_EXT = 0xea,
    IDENTIFY_DEVICE = 0xec,
    SET_FEATURES = 0xef,

    SECURITY_SET_PASSWORD = 0xf1,
    SECURITY_UNLOCK = 0xf2,
    SECURITY_ERASE_PREPARE = 0xf3,
    SECURITY_ERASE_UNIT = 0xf4,
    SECURITY_FREEZE_LOCK = 0xf5,
    SECURITY_DISABLE_PASSWORD = 0xf6,
}

impl AtaCommand {
    pub fn is_nqc(&self) -> bool {
        use AtaCommand::*;
        match self {
            NCQ_NON_DATA => true,
            READ_FPDMA_QUEUED => true,
            RECEIVE_FPDMA_QUEUED => true,
            SEND_FPDMA_QUEUED => true,
            WRITE_DMA_FUA_EXT => true,
            _ => false,
        }
    }

    /// Indicates if the command will write data to the device.
    /// This fn can be used to determine whether the writable bit needs to be set in a AHCI command HEader
    pub fn is_write(&self) -> bool {
        match self {
            AtaCommand::WRITE_DMA => true,
            AtaCommand::WRITE_DMA_EXT => true,
            AtaCommand::WRITE_DMA_FUA_EXT => true,
            AtaCommand::WRITE_BUFFER => true,
            AtaCommand::WRITE_BUFFER_DMA => true,
            AtaCommand::DATA_SET_MANAGEMENT => true,
            AtaCommand::DATA_SET_MANAGEMENT_XL => true,
            AtaCommand::WRITE_SECTORS => true,
            AtaCommand::WRITE_SECTORS_EXT => true,
            AtaCommand::CFA_WRITE_SECTORS_WITHOUT_ERASE => true,
            AtaCommand::WRITE_STREAM_DMA_EXT => true,
            AtaCommand::WRITE_STREAM_EXT => true,
            AtaCommand::WRITE_LOG_EXT => true,
            AtaCommand::WRITE_UNCORRECTABLE_EXT => true,
            AtaCommand::ZAC_MANAGEMENT_IN => true,
            AtaCommand::WRITE_LOG_DMA_EXT => true,
            AtaCommand::TRUSTED_SEND => true,
            AtaCommand::TRUSTED_SEND_DMA => true,
            AtaCommand::WRITE_FPDMA_QUEUED => true,
            AtaCommand::SEND_FPDMA_QUEUED => true,
            AtaCommand::ZAC_MANAGEMENT_OUT => true,
            AtaCommand::CFA_WRITE_MULTIPLE_WITHOUT_ERASE => true,
            AtaCommand::SECURITY_SET_PASSWORD => true,
            AtaCommand::SECURITY_UNLOCK => true,
            AtaCommand::SECURITY_ERASE_UNIT => true,
            AtaCommand::SECURITY_DISABLE_PASSWORD => true,
            _ => false,
        }
    }
}

impl privacy::Sealed for AtaCommand {}

/// Used with the [AtaCommand::SANITIZE_DEVICE] command.
#[repr(u16)]
#[allow(non_camel_case_types)]
#[derive(Copy, Clone, Debug, num_enum::TryFromPrimitive, num_enum::IntoPrimitive)]
pub enum SanitiseSubcommand {
    SANITIZE_STATUS_EXT = 0,
    CRYPTO_SCRAMBLE_EXT = 0x11,
    BLOCK_ERASE_EXT = 0x12,
    OVERWRITE_EXT = 0x14,
    SANITIZE_FREEZE_LOCK_EXT = 0x20,
    SANITIZE_ANTIFREEZE_LOCK_EXT = 0x40,
}

impl privacy::Sealed for SanitiseSubcommand {}

pub mod constructor {
    use crate::command::AtaCommand;

    /// A composed command contains all the fields required to issue a command.
    /// A field containing none is not defined at the ATA level and must be set by the driver.
    /// A field containing `Some(_)` may be modified, but this may result in errors from the device.
    ///
    /// Some unused fields may be left as None when the command is Opaque. In this case see
    /// [AtaCommand] for their usage.
    #[derive(Copy, Clone, Debug)]
    pub struct ComposedCommand {
        pub command: MaybeOpaqueCommand,
        pub feature: Option<u16>,
        pub count: Option<u16>,
        pub lba: Option<u64>,
        pub device: Option<u8>,
        pub icc: Option<u8>,
        pub aux: Option<u32>,
    }

    impl ComposedCommand {
        fn zeroed(cmd: MaybeOpaqueCommand) -> Self {
            Self {
                command: cmd,
                feature: Some(0),
                count: Some(0),
                lba: Some(0),
                device: Some(0),
                icc: Some(0),
                aux: Some(0),
            }
        }

        fn empty(cmd: MaybeOpaqueCommand) -> Self {
            Self {
                command: cmd,
                feature: None,
                count: None,
                lba: None,
                device: None,
                icc: None,
                aux: None,
            }
        }
        /// Returns true if the command is a 48bit command of if the command is a concrete command,
        /// otherwise returns false.
        ///
        /// if false the feature, and count can be treated as a u8 and lba can be treated asa u32
        /// where only the first 24 bits are valid.
        pub fn is_48_bit(&self) -> bool {
            if let MaybeOpaqueCommand::Concrete(_) = self.command {
                return true;
            }

            if let Some(n) = self.feature.as_ref() {
                if *n > u8::MAX as u16 {
                    return true;
                }
            } else if let Some(n) = self.count.as_ref() {
                if *n > u8::MAX as u16 {
                    return true;
                }
            } else if let Some(n) = self.lba.as_ref() {
                if *n > 0xfff_ffff {
                    return true;
                }
            }

            false
        }
    }

    /// Contains either an [AtaCommand] or an [OpaqueCommand]. This allows [ComposedCommand]s to not need
    /// to specify an exact command when a driver may want to specify the exact behaviour of a command
    /// i.e. a driver can choose to use a NCQ read instead of a regular read.
    #[derive(Copy, Clone, Debug)]
    pub enum MaybeOpaqueCommand {
        Concrete(AtaCommand),
        Opaque(OpaqueCommand),
    }

    impl TryInto<u8> for MaybeOpaqueCommand {
        type Error = Self;

        fn try_into(self) -> Result<u8, Self::Error> {
            match self {
                MaybeOpaqueCommand::Concrete(c) => Ok(c.into()),
                MaybeOpaqueCommand::Opaque(_) => Err(self),
            }
        }
    }

    impl Into<MaybeOpaqueCommand> for AtaCommand {
        fn into(self) -> MaybeOpaqueCommand {
            MaybeOpaqueCommand::Concrete(self)
        }
    }

    /// Opaque commands are commands have multiple potential command that the driver may want fine
    /// control of.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub enum OpaqueCommand {
        Read,
        Write,
    }

    pub trait CommandConstructor {
        fn compose(self) -> ComposedCommand;
    }

    /// A struct to construct simple command that spans over a region of LBAs such as reading or writing
    pub struct SpanningCmd {
        cmd: SpanningCmdType,
        lba: u64,
        count: u16,
    }

    impl SpanningCmd {
        pub fn new(cmd_type: SpanningCmdType, lba: u64, count: u16) -> Option<Self> {
            if (lba & !0xffff_ffff_ffff) != 0 {
                return None;
            }
            Some(SpanningCmd {
                cmd: cmd_type,
                lba,
                count,
            })
        }
    }

    impl CommandConstructor for SpanningCmd {
        fn compose(self) -> ComposedCommand {
            let mut cmd = ComposedCommand::empty(self.cmd.into());
            match self.cmd {
                SpanningCmdType::Read => {
                    cmd.lba = Some(self.lba);
                    cmd.count = Some(self.count);
                }
                SpanningCmdType::Write => {
                    cmd.lba = Some(self.lba);
                    cmd.count = Some(self.count);
                }
            }
            cmd
        }
    }

    #[derive(Copy, Clone, Debug)]
    #[non_exhaustive]
    pub enum SpanningCmdType {
        Read,
        Write,
    }

    impl Into<MaybeOpaqueCommand> for SpanningCmdType {
        fn into(self) -> MaybeOpaqueCommand {
            match self {
                SpanningCmdType::Read => MaybeOpaqueCommand::Opaque(OpaqueCommand::Read),
                SpanningCmdType::Write => MaybeOpaqueCommand::Opaque(OpaqueCommand::Write),
            }
        }
    }

    /// This enum contains command variants for commands that do not contain any arguments.
    #[derive(Copy, Clone, Debug)]
    #[non_exhaustive]
    pub enum NoArgCmd {
        IdentifyDevice,
    }

    impl Into<MaybeOpaqueCommand> for NoArgCmd {
        fn into(self) -> MaybeOpaqueCommand {
            match self {
                NoArgCmd::IdentifyDevice => AtaCommand::IDENTIFY_DEVICE.into(),
            }
        }
    }

    impl CommandConstructor for NoArgCmd {
        fn compose(self) -> ComposedCommand {
            ComposedCommand::zeroed(self.into())
        }
    }
}
