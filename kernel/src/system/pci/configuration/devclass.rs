#[repr(u8)]
pub enum PciClass {
    Unclassified(UnclassifiedSubclass) = 0,
    MassStorageController(MassStorageSubclass),
    NetworkController(NetworkControllerSubclass),
    DisplayController(DisplayControllerSubclass),
    MultimediaController(MultimediaControllerSubclass),
    MemoryController(MemControllerSubclass),
    Bridge(BridgeSubclass),
    SimpleCommunicationController(SimpleCommunicationSubclass),
    BaseSystemPeripheral(BaseSystemPeripheralSubclass),
    InputDeviceController(InputDeviceSubclass),
    DockingStation(DockingStnSubclass),
    Processor(ProcessorSubclass),
    SerialBusController(SerialBusSubclass),
    WirelessController(WirelessController),
    IntelligentController(IntelligentController),
    SatelliteCommunicationController(SatelliteCommunicationSubclass),
    EncryptionController(EncryptionController),
    SignalProcessingController(SignalProcessingSubclass),
    ProcessingAccelerator,
    NonEssentialInstrumentation,
    CoProcessor = 0x40,
    UnassignedClass = 0xff,
}

#[repr(u8)]
pub enum UnclassifiedSubclass {
    NonVgaCompatible = 0,
    VgaCompatible,
}

#[repr(u8)]
pub enum MassStorageSubclass {
    ScsiBusController = 0,
    IdeController(IdeInterface),
    FloppyDiskController,
    IpiBusController,
    RaidController,
    AtaController(AtaInterface),
    SerialAtaController(SataInterface),
    SasController(SasInterface),     // Serial attached SCSI
    NvMemController(NvMemInterface), // Non-Volatile Memory controller
    Other = 0x80,
}

#[repr(u8)]
pub enum IdeInterface {
    /// This interface only allows ISA mode
    IsaNative = 0,
    /// This interface only allows PCI mode
    PciNative = 0x5,
    /// This device runs in ISA mode with PCI compatibility
    IsaPci = 0xa,
    /// This device in PCI mode with ISA compatibility
    PciIsa = 0xf,
    /// [Self::IsaNative] with bus mastering
    IsaOnlyBm = 0x80,
    /// [Self::PciNative] with bus mastering
    PciOnlyBm = 0x85,
    /// [Self::IsaPci] with bus mastering
    IsaPciBm = 0x8a,
    /// [Self::PciIsa] with bus mastering
    PciIsaBm = 0x8f,
}

#[repr(u8)]
pub enum AtaInterface {
    SingleDma = 0x20,
    ChainedDma = 0x30,
}

#[repr(u8)]
pub enum SataInterface {
    VendorSpecific = 0,
    Ahci, // AHCI 1.0
    SerialStorageBus,
}

#[repr(u8)]
pub enum SasInterface {
    Sas = 0,
    SerialStorageBus, // @PCI-SIG thous should be 2
}

#[repr(u8)]
pub enum NvMemInterface {
    NvmHci,
    NvmExpress,
}

#[repr(u8)]
pub enum NetworkControllerSubclass {
    EtherNetController = 0,
    TokenRingController,
    FddiController,
    AtmController,
    IsdnController,
    WorldFlipController,
    Pcimg214MultiComputingController,
    InfinibandController,
    FabricController,
    Other = 0x80,
}

#[repr(u8)]
pub enum DisplayControllerSubclass {
    VgaCompatible(VgaInterface) = 0,
    XgaController,
    NonVga3dController,
    Other = 0x80,
}

#[repr(u8)]
pub enum VgaInterface {
    VgaController = 0,
    Compat8514Controller,
}

#[repr(u8)]
pub enum MultimediaControllerSubclass {
    VideoController = 0,
    AudioController,
    TelephonyDevice,
    AudioDevice,
    Other = 0x80,
}

#[repr(u8)]
pub enum MemControllerSubclass {
    RamController = 0,
    FlashController,
    Other = 0x80,
}

#[repr(u8)]
pub enum BridgeSubclass {
    Host = 0,
    Isa,
    Eisa,
    Mcia,
    PciPci(PciPciInterface),
    CardBus,
    RaceWay(RaceWayInterface),
    PciPciTwoBusBoogaloo(PciPciIfTwo),
    InfinibandPciHost,
    Other = 0x80,
}

#[repr(u8)]
pub enum PciPciInterface {
    NormalMode = 0,
    SubtractiveMode,
}

#[repr(u8)]
pub enum RaceWayInterface {
    Transparent = 0,
    EndpointMode,
}

#[repr(u8)]
pub enum PciPciIfTwo {
    PrimaryBus,
    SecondaryBus,
}

#[repr(u8)]
pub enum SimpleCommunicationSubclass {
    Serial(SerialInterface) = 0,
    Parallel(ParallelInterface),
    MultiPortSerial,
    Modem(ModemInterface),
    Ieee488AndAHalf,
    SmartCard,
    Other = 0x80,
}

#[repr(u8)]
pub enum SerialInterface {
    Compat8250 = 0,
    Compat16450,
    Compat16550,
    Compat16650,
    Compat16750,
    Compat16850,
    Compat16950,
}

#[repr(u8)]
pub enum ParallelInterface {
    Standard = 0,
    BiDirectional,
    Ecp1xCompliant,
    Ieee1284Controller,
    Ieee1284Target,
}

#[repr(u8)]
pub enum ModemInterface {
    Generic = 0,
    Hayes16450Compat,
    Hayes16550Compat,
    Hayes16650Compat,
    Hayes16750Compat,
}

#[repr(u8)]
pub enum BaseSystemPeripheralSubclass {
    Pic(BaseSystemPeripheralInterface) = 0,
    // please end me
    DmaController(BaseSystemPeripheralInterface),
    Timer(BaseSystemPeripheralInterface),
    RtcController(BaseSystemPeripheralInterface),
    PciHotPlugController,
    SdHostController, // shouldnt this be isn mas storage?
    Iommu,
    Other = 0x80,
}

pub enum BaseSystemPeripheralInterface {
    /// This variant is available for all subclasses
    /// Represents multiple device interfaces depending on the subclass
    /// - PIC: 8259 compatible
    /// - DMA: 8237 compatible
    /// - Timer: 8254 compatible,
    /// - RTC: Generic RTC
    Generic = 0,
    /// This variant is available for all subclasses
    IsaCompat,
    /// Not available for RTC controller subclass
    EisaCompat,
    /// Only for Timer Subclass
    Hpet,
    /// Only for PIC subclass
    IoApicIntController = 0x10,
    /// Only for PIC subclass
    IoxApicIntController = 0x20,
}

#[repr(u8)]
pub enum InputDeviceSubclass {
    KeyboardController,
    DigitizerPen,
    MouseController,
    ScannerController,
    GamePortController(GamePortInterface),
    Other = 0x80,
}

#[repr(u8)]
pub enum GamePortInterface {
    Generic = 0,
    Extended = 0x10,
}

#[repr(u8)]
pub enum DockingStnSubclass {
    Generic = 0,
    Other = 0x80,
}

#[repr(u8)]
#[allow(non_camel_case_types)]
pub enum ProcessorSubclass {
    i386 = 0,
    i486,
    Pentium,
    PentiumPro,
    Alpha = 0x10,
    PowerPc = 0x20,
    Mips = 0x30,
    CoProcessor = 0x40,
    Other = 80,
}

#[repr(u8)]
pub enum SerialBusSubclass {
    FireWire(FirewireInterface) = 0,
    AccessBusController,
    Ssa,
    Usb(UsbInterface),
    FibreChannel,
    SmBus,
    Infiniband,
    IpmiInterface(IpmiIf),
    SercosInterface,
    CanBus,
    Other = 0x80,
}

#[repr(u8)]
pub enum FirewireInterface {
    Generic = 0,
    Ohci = 0x10,
}

#[repr(u8)]
pub enum UsbInterface {
    UhciController = 0x0,
    OhciController = 0x10,
    EhciController = 0x20, // usb2
    XhciController = 0x30, // usb3
    Unspecified = 0x80,
    UsbDevice = 0xfe, // Device not controller
}

#[repr(u8)]
pub enum IpmiIf {
    Smic = 0,
    KeyboardControllerStyle,
    BlockTransfer,
}

#[repr(u8)]
pub enum WirelessController {
    #[allow(non_camel_case_types)]
    iRDACompatible = 0,
    ConsumerIrController,
    RfController = 0x10,
    BluetoothController,
    BroadbandController,
    EthernetController8021a = 0x20,
    EthernetController8021b,
    Other = 0x80,
}

#[repr(u8)]
pub enum IntelligentController {
    I2O = 0x0, // why did i even to this one?
}

#[repr(u8)]
pub enum SatelliteCommunicationSubclass {
    TvController = 1,
    AudioController,
    VoiceController,
    DataController,
}

#[repr(u8)]
pub enum EncryptionController {
    NetworkAndComputing = 0,
    Entertainment = 10,
    Other = 0x80,
}

#[repr(u8)]
pub enum SignalProcessingSubclass {
    DpioModules = 0,
    PerformanceCounters,
    ComSynchronizer = 0x10,
    SignalProcessingManagement = 0x80,
}
