use core::mem::MaybeUninit;

#[allow(dead_code)]
pub struct ReceivedFis {
    dma_setup: MaybeUninit<DmaSetupFis>,
    _res0: [u8; 4],
    pio_setup: MaybeUninit<PioSetupFis>,
    _res1: [u8; 12],
    d2h: MaybeUninit<RegisterDevToHostFis>,
    _res2: [u8; 4],
    set_dev_bits: MaybeUninit<SetDevBitsFis>,
    unknown_fis: [u8; 64],
}

#[repr(transparent)]
#[derive(Copy, Clone, Default)]
pub struct FisCommand {
    low: u8,
}

impl FisCommand {
    #![allow(dead_code)]
    pub fn set_cmd_bit(&mut self, value: bool) {
        if value {
            self.low |= 1 << 7
        } else {
            self.low &= !(1 << 7)
        }
    }

    pub fn get_cmd_bit(&self) -> bool {
        self.low & 1 << 7 != 0
    }

    pub fn set_port(&mut self, port: u8) {
        assert!(port < 16);
        self.low &= !0xf;
        self.low |= port;
    }
}

#[repr(C)]
#[derive(Clone)]
pub struct RegisterHostToDevFis {
    pub(crate) fis_type: FisType,
    pub(crate) cfg: FisCommand,
    pub(crate) command: u8,
    pub(crate) features_low: u8,
    pub(crate) lba_low: [u8; 3],
    pub(crate) dev: u8,
    pub(crate) lba_high: [u8; 3],
    pub(crate) features_high: u8,
    pub(crate) count: u16,
    pub(crate) icc: u8,
    pub(crate) control: u8,
    pub(crate) aux: u32,
}

impl RegisterHostToDevFis {
    pub(crate) fn new(
        cmd: ata::command::AtaCommand,
        port: u8,
        features: u16,
        lba: u64,
        device: u8,
        count: u16,
        icc: u8,
        control: u8,
        aux: u32,
    ) -> Self {
        assert!(lba < 1 << 48);

        // separations
        let (lba_low, lba_high) = {
            let lba_bytes: [u8; 8] = lba.to_le_bytes();
            let mut lba_low = [0u8; 3];
            let mut lba_high = [0u8; 3];
            lba_low[..].copy_from_slice(&lba_bytes[0..3]);
            lba_high[..].copy_from_slice(&lba_bytes[3..6]);
            (lba_low, lba_high)
        };

        let [features_low, features_high] = features.to_le_bytes();
        let mut cfg = FisCommand { low: 0 };
        cfg.set_port(port);

        Self {
            fis_type: FisType::RegisterH2D,
            cfg,
            command: cmd as u8,
            features_low,
            lba_low,
            dev: device,
            lba_high,
            features_high,
            count,
            icc,
            control,
            aux,
        }
    }
}

impl TryFrom<&ata::command::constructor::ComposedCommand> for RegisterHostToDevFis {
    type Error = ();

    fn try_from(value: &ata::command::constructor::ComposedCommand) -> Result<Self, Self::Error> {
        use ata::command::constructor::*;
        if let MaybeOpaqueCommand::Concrete(c) = value.command {
            Ok(Self::new(
                c,
                0,
                value.feature.unwrap_or(0),
                value.lba.unwrap_or(0),
                value.device.unwrap_or(0),
                value.count.unwrap_or(0),
                value.icc.unwrap_or(0),
                0,
                value.aux.unwrap_or(0),
            ))
        } else {
            Err(())
        }
    }
}

#[repr(C)]
pub struct RegisterDevToHostFis {
    fis_type: FisType,
    flags: D2HFlags,
    status: u8,
    err: u8,
    lba_low: [u8; 3],
    dev: u8,
    lba_high: [u8; 3],
    _res0: u8,
    count: u16,
    _res1: u32,
}

#[allow(dead_code)]
pub struct D2HFlags {
    inner: u8,
}

impl D2HFlags {
    #![allow(dead_code)]
    fn get_port(&self) -> u8 {
        self.inner & 0xf
    }

    fn get_int(&self) -> bool {
        self.inner & (1 << 6) != 0
    }
}

/// I think this just updates the PxSERR register
#[repr(C)]
pub struct SetDevBitsFis {
    fis_type: FisType,
    flags: SetDevBitsFlags,
    err: u8,
    high: u32, // protocol specific
}

#[repr(C)]
#[repr(align(1))]
pub struct SetDevBitsFlags {
    low: u8,
    high: u8,
}

impl SetDevBitsFlags {
    // maybe replace get_status_high/low with `get_status` that reruns both
    #![allow(dead_code)]
    fn get_port(&self) -> u8 {
        self.low & 0xf
    }

    fn get_int_flag(&self) -> bool {
        self.low & (1 << 6) != 0
    }

    fn req_attn(&self) -> bool {
        self.low & (1 << 7) != 0
    }

    fn get_status_low(&self) -> u8 {
        self.high & 0x7
    }

    fn get_status_high(&self) -> u8 {
        self.high & (0x7 << 4)
    }
}

#[allow(dead_code)]
pub struct DmaActiveD2H {
    fis_type: FisType,
    port: DmaActivePort,
    _res: u16,
}

#[allow(dead_code)]
pub struct DmaActivePort {
    inner: u8,
}

impl DmaActivePort {
    #![allow(dead_code)]
    fn get_port(&self) -> u8 {
        self.inner & 0xf
    }
}

#[allow(dead_code)]
pub struct DmaSetupFis {
    fis_type: FisType,
    flags: DmaSetupFlags,
    _res0: u16,
    dma_buffer_id: u64, // exact layout is impl specific
    _res1: u32,
    dma_buff_offset: u32,
    dma_transfer_count: u32,
    _res2: u32,
}

impl DmaSetupFis {
    #![allow(dead_code)]
    /// Sets offset of the byte buffer in bits.
    ///
    /// # Panics
    ///
    /// `value` must be aligned to 4
    fn set_offset(&mut self, value: u32) {
        assert_eq!(value & 3, 0);

        self.dma_buff_offset = value;
    }
}

#[allow(dead_code)]
#[repr(transparent)]
pub struct DmaSetupFlags {
    inner: u8,
}

impl DmaSetupFlags {
    #![allow(dead_code)]
    fn get_port(&self) -> u8 {
        self.inner & 0xf
    }

    /// Sets the port number for a port multiplier.
    ///
    /// # Panics
    ///
    /// `port` must be les than `16`
    fn set_port(&mut self, port: u8) {
        assert!(port < 16);

        self.inner &= !0xf;
        self.inner |= port
    }

    /// Returns whether the sender is going to send data or the receiver.
    fn is_sender_sending(&self) -> bool {
        self.inner & (1 << 5) != 0
    }

    /// Sets whether an interrupt should eb generated when the DMA transfer is completed
    fn set_interrupt_bit(&mut self, value: bool) {
        if value {
            self.inner |= 1 << 6
        } else {
            self.inner &= !(1 << 6)
        }
    }

    fn set_auto(&mut self, value: bool) {
        if value {
            self.inner |= 1 << 7
        } else {
            self.inner &= !(1 << 7)
        }
    }

    fn get_auto(&self) -> bool {
        self.inner & (1 << 7) != 0
    }
}

#[allow(dead_code)]
pub struct BistActivateFis {
    fis_type: FisType,
    port: u8, // 0..15 only
    flags: BistFlags,
    _res: u8,
    data_1: u32,
    data_2: u32,
}

bitflags::bitflags! {
    pub struct BistFlags: u8 {
        /// T:
        const FAR_END_TRANSMIT = 1 << 7;
        /// A: Only use with [Self::FAR_END_TRANSMIT]
        const ALIGN_BYPASS = 1 << 6;
        /// S: Only use with [Self::FAR_END_TRANSMIT]
        const BYPASS_SCRAMBLING = 1 << 5;
        /// L:
        /// Transmitter will insert ALIGNp primitives
        const FAR_END_RETIMED_LOOPBACK = 1 << 4;
        /// F
        ///
        /// When the raw data is received it is retransmitted back. Used to verify connectivity.
        /// Hardware support is optional
        const FAR_END_ANALOGUE = 1 << 3;
        /// P: Only use with [Self::FAR_END_TRANSMIT]
        const PRIMATIVE_BIT = 1 << 2;
        /// V: Causes other bits to be ignored
        const VENDOR_SPECIFIC = 1;
    }
}

#[repr(C)]
pub struct PioSetupFis {
    fis_type: FisType,
    flags: PioSetupFlags,
    status: u8, // copied into PxSERR
    error: u8,  // copied into PxSERR
    lba_low: [u8; 3],
    device: u8,
    lba_high: [u8; 3],
    _res0: u8,
    count: u16,
    _res1: u8,
    e_status: u8,
    transfer_count: u8,
    _res2: u16,
}

impl PioSetupFis {
    #![allow(dead_code)]

    /// Returns the lba given by the device
    fn get_lba(&self) -> u64 {
        let mut arr_le = [0u8; 8];
        arr_le[0..3].copy_from_slice(&self.lba_low);
        arr_le[3..6].copy_from_slice(&self.lba_high);

        u64::from_le_bytes(arr_le)
    }
}

#[allow(dead_code)]
#[repr(transparent)]
pub struct PioSetupFlags {
    inner: u8,
}

impl PioSetupFlags {
    #![allow(dead_code)]
    /// Sets the Port multiplier port
    ///
    /// # Panics
    ///
    /// `port` must be lower than 16.
    fn set_port(&mut self, port: u8) {
        assert!(port < 16);
        self.inner &= !0xf;
        self.inner |= port;
    }

    /// Sets the receive bit. When set the host is expecting to receive data, when clear the host
    /// expects to send data.
    fn is_receive(&self) -> bool {
        self.inner & (1 << 5) != 0
    }

    fn is_interrupt(&self) -> bool {
        self.inner & (1 << 6) != 0
    }
}
// All variants exist here but not all are used. Some of this will probably always remain dead
#[allow(dead_code)]
#[repr(u8)]
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq)]
pub enum FisType {
    RegisterH2D = 0x27,
    RegisterD2H = 0x34,
    DmaActiveD2H = 0x39,
    DmaSetupBiDir = 0x41,
    DataFisBiDir = 0x46,
    BistActivateBiDir = 0x58,
    PioSetupD2H = 0x5f,
    SetDevBitsD2H = 0xa1,
    _Res0 = 0xa6,
    _Res1 = 0xb8,
    _Res3 = 0xbf,
    Vendor0 = 0xc7,
    Vendor1 = 0xd4,
    _Res4 = 0xd9,
}

impl FisType {
    #![allow(dead_code)]
    /// Returns whether or not the FIS type is host to device.
    ///
    /// Reserved and vendor specific types always return false
    fn is_host_to_dev(&self) -> bool {
        match self {
            FisType::RegisterH2D => true,
            FisType::RegisterD2H => false,
            FisType::DmaActiveD2H => false,
            FisType::DmaSetupBiDir => true,
            FisType::DataFisBiDir => true,
            FisType::BistActivateBiDir => true,
            FisType::PioSetupD2H => false,
            FisType::SetDevBitsD2H => false,
            FisType::_Res0 => false,
            FisType::_Res1 => false,
            FisType::_Res3 => false,
            FisType::Vendor0 => false,
            FisType::Vendor1 => false,
            FisType::_Res4 => false,
        }
    }
}
