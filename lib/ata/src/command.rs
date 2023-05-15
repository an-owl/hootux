#[repr(u8)]
#[derive(Eq, PartialEq, Copy, Clone)]
#[allow(non_camel_case_types)]
pub enum AtaCommand {
    // Read
    /// Reads from the device using DMA.
    ///
    /// - LBA contains the first sector to be transferred.
    /// - Count contains the number of sectors to be transferred. A value of 0 is treated as 256
    READ_DMA = 0xc8,
    /// See [Self::READ_DMA]
    /// - Count: Size of this field is doubled. A value of 0 is treated as 65,536
    READ_DMA_EXT = 0x25,

    // Write
    /// Writes to the disk using DMA.
    ///
    /// - LBA contains the first sector to be written
    /// - Count contains the number of sectors to be written. A value of 0 is treated as 256 sectors
    WRITE_DMA = 0xca,
    /// See [Self::WRITE_DMA]
    /// - Count: Size of this field is doubled. A value of 0 is treated as 65,536
    WRITE_DMA_EXT = 0x35,
    /// Performs an uncached [Self::WRITE_DMA_EXT] regardless of the current caching policy.
    WRITE_DMA_FUA_EXT = 0x3d,

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
    MUTATE_EXT = 0x96,
    ZAC_MANAGEMENT_OUT = 0x9f,
    SMART = 0xb0,

    SET_SECTOR_CONFIG = 0xb2,
    SANITIZE_DEVICE = 0xb4,
    CFA_WRITE_MULTIPLE_WITHOUT_ERASE = 0xcd,

    STANDBY_IMMEDIATE = 0xe0,
    IDLE_IMMEDIATE = 0xe1,
    STANDBY = 0xe2,
    IDLE = 0xe3,
    READ_BUFFER = 0xe4,
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
    WRITE_BUFFER = 0xe8,

    READ_BUFFER_DMA = 0xe9,
    WRITE_BUFFER_DMA = 0xeb,
    IDENTIFY_DEVICE = 0xec,
    SET_FEATURES = 0xef,

    SECURITY_SET_PASSWORD = 0xf1,
    SECURITY_UNLOCK = 0xf2,
    SECURITY_ERASE_PREPARE = 0xf3,
    SECURITY_ERASE_UNIT = 0xf4,
    SECURITY_FREEZE_LOCK = 0xf5,
    SECURITY_DISABLE_PASSWORD = 0xf6,
}

/// Used with the [AtaCommand::SANITIZE_DEVICE] command.
#[repr(u16)]
#[allow(non_camel_case_types)]
pub enum SanitiseSubcommand {
    SANITIZE_STATUS_EXT = 0,
    CRYPTO_SCRAMBLE_EXT = 0x11,
    BLOCK_ERASE_EXT = 0x12,
    OVERWRITE_EXT = 0x14,
    SANITIZE_FREEZE_LOCK_EXT = 0x20,
    SANITIZE_ANTIFREEZE_LOCK_EXT = 0x40,
}
