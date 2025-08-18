use crate::ehci::EndpointQueue;
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::any::Any;
use core::pin::pin;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use hootux::fs::file::*;
use hootux::fs::sysfs::{SysfsDirectory, SysfsFile};
use hootux::fs::vfs::MajorNum;
use hootux::fs::{IoError, IoResult};
use hootux::mem::dma::DmaBuff;

#[derive(Clone)]
#[file]
struct UsbDeviceFile {
    major_num: MajorNum,
    address: u8,
    usb_device_accessor: alloc::sync::Weak<UsbDeviceAccessor>,
}

impl File for UsbDeviceFile {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn block_size(&self) -> u64 {
        hootux::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        DevID::new(self.major_num, self.address as usize)
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        self.address as u64
    }

    fn len(&self) -> IoResult<u64> {
        async { Ok(hootux::fs::sysfs::SysfsDirectory::entries(self) as u64) }.boxed()
    }
}

impl SysfsDirectory for UsbDeviceFile {
    fn entries(&self) -> usize {
        let acc = self
            .usb_device_accessor
            .upgrade()
            .expect("USB device disappeared");

        let extra = hootux::block_on!(pin!(acc.extra.lock()));
        extra
            .as_ref()
            .map(|file| SysfsDirectory::entries(&**file))
            .unwrap_or(0)
    }

    fn file_list(&self) -> Vec<String> {
        let acc = self
            .usb_device_accessor
            .upgrade()
            .expect("USB device disappeared"); // fixme this should not result in a panic
        let extra = hootux::block_on!(pin!(acc.extra.lock()));

        let mut v = extra
            .as_ref()
            .map(|file| {
                let file: &dyn SysfsDirectory = &**file;
                SysfsDirectory::file_list(file)
            })
            .unwrap_or(Vec::new());
        v.push("configuration".to_string());
        v.push("class".to_string());
        v
    }

    fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        match name {
            "configuration" => Ok(Box::new(ConfigurationIo {
                major_num: self.major_num,
                address: self.address,
                usb_device_accessor: self.usb_device_accessor.clone(),
            })),
            "class" => Ok(Box::new(ClassFinderFle {
                major_num: self.major_num,
                address: self.address,
                usb_device_accessor: self.usb_device_accessor.clone(),
            })),
            _ => {
                let acc = self
                    .usb_device_accessor
                    .upgrade()
                    .ok_or(IoError::NotPresent)?;
                Ok(hootux::block_on!(pin!(acc.extra.lock()))
                    .as_ref()
                    .map_or(Err(IoError::NotPresent), |file| {
                        SysfsDirectory::get_file(&**file, name)
                    })?)
            }
        }
    }

    fn store(&self, _: &str, _: Box<dyn SysfsFile>) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn remove(&self, _: &str) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

struct UsbDeviceAccessor {
    major_num: MajorNum,
    address: u8,
    ctl_endpoint: alloc::sync::Arc<EndpointQueue>,
    /// Contains all the optional endpoints.
    /// When configuring the device this mutex must be locked.
    endpoints: async_lock::Mutex<[Option<alloc::sync::Arc<EndpointQueue>>; 15]>, // endpoints 1..16
    controller: alloc::sync::Weak<async_lock::Mutex<super::Ehci>>,
    extra: async_lock::Mutex<Option<Box<dyn SysfsDirectory>>>,
}

impl UsbDeviceAccessor {
    pub(super) async unsafe fn new(
        address: u8,
        ctl_endpoint: alloc::sync::Arc<EndpointQueue>,
        controller: alloc::sync::Arc<async_lock::Mutex<super::Ehci>>,
    ) -> Self {
        let l = controller.lock().await;

        let mut this = Self {
            major_num: l.major_num,
            address,
            ctl_endpoint,
            endpoints: async_lock::Mutex::new([const { None }; 15]),
            controller: alloc::sync::Weak::new(),
            extra: async_lock::Mutex::new(None),
        };
        this.controller = alloc::sync::Arc::downgrade(&controller);
        this
    }

    fn get_file(self: &alloc::sync::Arc<Self>) -> Box<dyn File> {
        Box::new(UsbDeviceFile {
            major_num: self.major_num,
            address: self.address,
            usb_device_accessor: alloc::sync::Arc::downgrade(self),
        })
    }
}

macro_rules! impl_file_for_util {
    ($name:ty) => {
        impl File for $name {
            fn file_type(&self) -> FileType {
                FileType::NormalFile
            }
            fn block_size(&self) -> u64 {
                hootux::mem::PAGE_SIZE as u64
            }

            fn device(&self) -> DevID {
                DevID::new(self.major_num, self.address as usize)
            }

            fn clone_file(&self) -> Box<dyn File> {
                Box::new(self.clone())
            }

            fn id(&self) -> u64 {
                self.address as u64
            }

            fn len(&self) -> IoResult<u64> {
                async { Ok(8) }.boxed()
            }
        }

        impl NormalFile for $name {
            fn len_chars(&self) -> IoResult<u64> {
                self.len()
            }

            fn file_lock<'a>(
                self: Box<Self>,
            ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
                async { Err((IoError::NotSupported, self as Box<dyn NormalFile<u8>>)) }.boxed()
            }

            unsafe fn unlock_unsafe(&self) -> IoResult<()> {
                async { Err(IoError::Exclusive) }.boxed() // indicates the file is not locked
            }
        }
    };
}

#[file]
#[derive(Clone)]
struct ClassFinderFle {
    major_num: MajorNum,
    address: u8,
    usb_device_accessor: alloc::sync::Weak<UsbDeviceAccessor>,
}

impl_file_for_util!(ClassFinderFle);
impl SysfsFile for ClassFinderFle {}

impl Read<u8> for ClassFinderFle {
    /// This implementation allows the caller to determine whether the device implements a certain class.
    /// the first 3 bytes of `pos` (le) indicate the class to be searched for. The fourth byte
    /// contains a bitmap indicating the descriptors at which this a match can be found.
    /// All other bits in the fourth byte are reserved and will return [IoError::EndOfFile].
    ///
    /// ``` ignore
    /// # use hootux::fs::file::NormalFile;
    ///
    /// async fn demo(file: &dyn NormalFile, buffer: hootux::mem::dma::DmaBuff) {
    ///     let mut descriptor = 0; // Invalid always returns IoError::InvalidData
    ///     descriptor |= 1;         // Device descriptor
    ///     descriptor |= 1 << 1;    // Interface descriptor
    ///
    ///     /*
    ///     Searches for a USB keyboard
    ///         * Class:    3
    ///         * Subclass: 0
    ///         * Protocol: 1
    ///     */
    ///     let pos = u32::from_be_bytes([3,0,1,descriptor]) as u64;
    ///     file.read(pos,buffer).await;
    /// }
    /// ```
    ///
    /// This will perform a breadth-first-search, this will return the value and descriptor type
    /// where the class is first found. The result. The results will be returned in the first two
    /// bytes of the data buffer.
    ///
    /// * The first byte will indicate which configuration the class was found in (0 if device).
    /// * The second byte indicates what descriptor the class was found in.
    /// * If the class is not found, no data will be written to the buffer and the returned data length will be `0`
    fn read<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        pos: u64,
        mut buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async move {
            let (class, descriptors) = match pos.to_le_bytes() {
                [_, _, _, descriptors_bitmap, ..]
                if !DescriptorBitmap::from_bits(descriptors_bitmap).is_none() =>
                    {
                        return Err((IoError::InvalidData, buff, 0));
                    }
                [class @ .., descriptors_bitmap, 0, 0, 0, 0] => {
                    (
                        class,
                        DescriptorBitmap::from_bits(descriptors_bitmap).unwrap(),
                    ) // unwrap will never fail, checked above
                }
                _ => {
                    // Can only trigger if upper half is not `0`
                    return Err((IoError::EndOfFile, buff, 0));
                }
            };

            let Some(acc) = self.usb_device_accessor.upgrade() else {
                return Err((IoError::NotPresent, buff, 0));
            };

            macro_rules! get_descriptor_full {

                ($descriptor:ty,$index:expr) => {
                    {
                        let mut ts = super::TransactionString::empty();
                        let mut guarded = hootux::mem::dma::DmaGuard::new(alloc::vec![0u8;255]);
                        let command = usb_cfg::CtlTransfer::get_descriptor::<$descriptor>($index,Some(255));

                        let cmd_raw = Vec::from(command.to_bytes());

                        let mut command = hootux::mem::dma::DmaGuard::new(cmd_raw);
                        // SAFETY: Buffers are stack allocated
                        unsafe {
                            ts.append_qtd(&mut command,crate::PidCode::Control);
                            ts.append_qtd(&mut guarded,crate::PidCode::Out);
                        }

                        let ctl: &alloc::sync::Arc<EndpointQueue> = &acc.ctl_endpoint;
                        ctl.append_cmd_string(ts).wait().await;
                        guarded.unwrap()
                    }
                };
            }

            let dev_descriptor_buffer = get_descriptor_full!(usb_cfg::descriptor::DeviceDescriptor,0);
            let Some(dev_descriptor): Option<&usb_cfg::descriptor::DeviceDescriptor> = usb_cfg::descriptor::Descriptor::from_raw(&dev_descriptor_buffer) else {
                let len = buff.len(); // todo this is the wrong len
                return Err((IoError::MediaError, buff, len));
            };
            if dev_descriptor.class() == class && descriptors.contains(DescriptorBitmap::DEVICE) {
                // SAFETY: This is safe because `DmaTarget::data_ptr()` must be writable.
                unsafe { (&mut *buff.data_ptr())[0..2].copy_from_slice(&[0,1]) }
                return Ok((buff, 2));
            }

            // If only device is present
            if !descriptors.contains(DescriptorBitmap::DEVICE.complement()) {
                return Ok((buff, 0));
            }

            for configuration in 0..dev_descriptor.num_configurations {
                let cfg_raw = get_descriptor_full!(usb_cfg::descriptor::ConfigurationDescriptor,configuration);
                let Some(cfg_descriptor): Option<&usb_cfg::descriptor::ConfigurationDescriptor> = usb_cfg::descriptor::Descriptor::from_raw(&cfg_raw) else { return Err((IoError::MediaError,buff,0)) }; // fixme get actual length
                // SAFETY: We fetch the max size for a descriptor, so we definitely have the whole thing & it has not been modified.
                for interface_desc in unsafe { cfg_descriptor.iter() } {
                    if descriptors.contains(DescriptorBitmap::INTERFACE) && interface_desc.class() == class {
                        unsafe { (&mut *buff.data_ptr())[0..2].copy_from_slice(&[configuration, DescriptorBitmap::INTERFACE.bits()])};
                        return Ok((buff, 2));
                    }
                }
            }

            Ok((buff, 0))
        }
            .boxed()
    }
}

impl Write<u8> for ClassFinderFle {
    fn write<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        _pos: u64,
        buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async { Err((IoError::ReadOnly, buff, 0)) }.boxed()
    }
}

bitflags::bitflags! {
    pub struct DescriptorBitmap: u8 {
        const DEVICE = 1;
        const INTERFACE = 1 << 1;
    }
}

#[file]
#[derive(Clone)]
struct ConfigurationIo {
    major_num: MajorNum,
    address: u8,
    usb_device_accessor: alloc::sync::Weak<UsbDeviceAccessor>,
}

impl_file_for_util!(ConfigurationIo);
impl SysfsFile for ConfigurationIo {}

impl Read<u8> for ConfigurationIo {
    /// Fetches a descriptor indicated by the `pos` argument.
    /// The format for `pos` is defined as the "wValue" field in the "GET_DESCRIPTOR" command in
    /// the USB specification 2.0 section 9.4.3.
    /// This fn will attempt to read `buffer.len().max(255)` bytes.
    fn read<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        pos: u64,
        mut buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async move {
            let Some(acc) = self.usb_device_accessor.upgrade() else {
                return Err((IoError::NotPresent, buff, 0));
            };
            const DEVICE_DESCRIPTOR_ID: u8 = usb_cfg::DescriptorType::Device as u8;
            const DEVICE_QUALIFIER_ID: u8 = usb_cfg::DescriptorType::DeviceQualifier as u8;
            const CONFIGURATION_DESCRIPTOR_ID: u8 = usb_cfg::DescriptorType::Configuration as u8;
            const OTHER_SPEED_CONFIGURATION: u8 =
                usb_cfg::DescriptorType::OtherSpeedConfiguration as u8;

            let mut ts = super::TransactionString::empty();

            let command = match pos.to_le_bytes() {
                [0, DEVICE_DESCRIPTOR_ID, ..] => Vec::from(
                    usb_cfg::CtlTransfer::get_descriptor::<usb_cfg::descriptor::DeviceDescriptor>(
                        0,
                        Some(buff.len().try_into().unwrap_or(255)),
                    )
                    .to_bytes(),
                ),
                [0, DEVICE_QUALIFIER_ID, ..] => Vec::from(
                    usb_cfg::CtlTransfer::get_descriptor::<usb_cfg::descriptor::DeviceQualifier>(
                        0,
                        Some(buff.len().try_into().unwrap_or(255)),
                    )
                    .to_bytes(),
                ),
                [index, CONFIGURATION_DESCRIPTOR_ID, ..] => Vec::from(
                    usb_cfg::CtlTransfer::get_descriptor::<
                        usb_cfg::descriptor::ConfigurationDescriptor,
                    >(index, Some(buff.len().try_into().unwrap_or(255)))
                    .to_bytes(),
                ),
                [index, OTHER_SPEED_CONFIGURATION, ..] => Vec::from(
                    usb_cfg::CtlTransfer::get_descriptor::<
                        usb_cfg::descriptor::ConfigurationDescriptor<
                            usb_cfg::descriptor::AlternateSpeed,
                        >,
                    >(index, Some(buff.len().try_into().unwrap_or(255)))
                    .to_bytes(),
                ),
                [0, 0, 0, 0, 0, 0, 0, 0] => {
                    let cmd = Vec::from(usb_cfg::CtlTransfer::get_configuration().to_bytes());
                    if buff.len() == 0 {
                        return Ok((buff, 0));
                    }
                    cmd
                }
                _ => return Err((IoError::EndOfFile, buff, 0)),
            };

            let mut dma = hootux::mem::dma::DmaGuard::new(command);

            // SAFETY: `dma` is heap allocated and ts will be completed.
            unsafe {
                ts.append_qtd(&mut dma, crate::PidCode::Control);
                ts.append_qtd(&mut *buff, crate::PidCode::In);
            };

            // We need to actually get completion information from the ts rather than just assuming
            // it completed properly and completely
            let _: () = acc.ctl_endpoint.append_cmd_string(ts).wait().await;
            let l = buff.len();
            // If we are fetching the current configuration then the len is always `1`
            if pos != 0 {
                Ok((buff, l))
            } else {
                Ok((buff, 1))
            }
        }
        .boxed()
    }
}

impl Write<u8> for ConfigurationIo {
    fn write<'f, 'a: 'f, 'b: 'f>(
        &'a self,
        _: u64,
        mut buff: DmaBuff<'b>,
    ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
        async {
            use hootux::mem::dma::DmaClaimable as _;

            let Some(acc) = self.usb_device_accessor.upgrade() else {
                return Err((IoError::NotPresent, buff, 0));
            };
            // Prevents operations being inserted out of order
            let ports = acc.endpoints.lock().await;

            // If we are configuring the device then we must ensure the device isn't already configured.
            // Deconfiguring can be done without checking
            // SAFETY: buff.data_ptr() must be accessible
            let target_config: u8 = unsafe { (&*buff.data_ptr())[0] };
            match target_config {
                0 => {} // no check required to clear configuration
                _ => {
                    // Determine if the device is already configured
                    let dma = hootux::mem::dma::DmaGuard::new(alloc::vec![0u8; 2]);
                    let Some((base, borrow)) = dma.claim() else {
                        unreachable!()
                    };
                    let (_, _) = self.read(0, borrow).await?;
                    let current_cfg = base.unwrap().unwrap().unwrap();
                    match current_cfg[0..=1] {
                        [0, 0] => {}
                        _ => return Err((IoError::Busy, buff, 1)),
                    }
                }
            }

            let mut ts = super::TransactionString::empty();
            let mut command = hootux::mem::dma::DmaGuard::new(Vec::from(
                usb_cfg::CtlTransfer::set_configuration(target_config).to_bytes(),
            ));
            // SAFETY: `command` is stack allocated and will not be dropped before the operation is completed.
            unsafe { ts.append_qtd(&mut command, crate::PidCode::Control) };
            // no data stage here
            let _: () = acc.ctl_endpoint.append_cmd_string(ts).wait().await;

            drop(ports);
            Ok((buff, 0))
        }
        .boxed()
    }
}
