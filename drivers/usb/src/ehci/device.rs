use crate::ehci::{EndpointQueue, TransactionString};
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
use usb_cfg::descriptor::{BaseDescriptor, Descriptor};

#[derive(Clone)]
#[file]
pub struct UsbDeviceFile {
    major_num: MajorNum,
    address: u8,
    usb_device_accessor: alloc::sync::Weak<UsbDeviceAccessor>,
}

impl UsbDeviceFile {
    pub async fn acquire_ctl(
        &self,
        driver: &dyn crate::UsbDeviceDriver,
    ) -> Result<frontend::UsbDevCtl, IoError> {
        let acc = self
            .usb_device_accessor
            .upgrade()
            .ok_or(IoError::NotPresent)?;
        let endpoints = acc.endpoints.try_lock_arc().ok_or(IoError::Busy)?;
        let mut acc_driver = acc.usb_device_driver.lock().await;
        if acc_driver.is_some() {
            log::error!(
                "The fuck? Somehow a driver is registered to {:?} but the endpoints arent acquired",
                self.device()
            );
            return Err(IoError::AlreadyExists);
        }
        *acc_driver = Some(crate::UsbDeviceDriver::clone(driver));
        Ok(frontend::UsbDevCtl {
            acc: alloc::sync::Arc::downgrade(&acc),
            endpoints,
        })
    }
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

    fn len(&self) -> IoResult<'_, u64> {
        async { Ok(hootux::fs::sysfs::SysfsDirectory::entries(self) as u64) }.boxed()
    }
}

impl SysfsDirectory for UsbDeviceFile {
    fn entries(&self) -> usize {
        let acc = self
            .usb_device_accessor
            .upgrade()
            .expect("USB device disappeared");

        let extra = hootux::block_on!(pin!(acc.usb_device_driver.lock()));
        extra
            .as_ref()
            .map(|file| SysfsDirectory::entries(&**file))
            .unwrap_or(0)
            + 2
    }

    fn file_list(&self) -> Vec<String> {
        let acc = self
            .usb_device_accessor
            .upgrade()
            .expect("USB device disappeared"); // fixme this should not result in a panic
        let extra = hootux::block_on!(pin!(acc.usb_device_driver.lock()));

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
            "." => Ok(Box::new(self.clone())),
            ".." => {
                let usb = SysfsDirectory::get_file(&hootux::fs::sysfs::SysFsRoot::new().bus, "usb")
                    .unwrap()
                    .into_sysfs_dir()
                    .unwrap();
                SysfsDirectory::get_file(&*usb, &alloc::format!("usb{}", self.device().as_int().0))
            }
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
                Ok(hootux::block_on!(pin!(acc.usb_device_driver.lock()))
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

pub(super) struct UsbDeviceAccessor {
    major_num: MajorNum,
    pub address: crate::DeviceAddress,
    ctl_endpoint: alloc::sync::Arc<EndpointQueue>,
    /// Contains all the optional endpoints.
    /// When configuring the device this mutex must be locked.
    endpoints: alloc::sync::Arc<async_lock::Mutex<[Option<alloc::sync::Arc<EndpointQueue>>; 15]>>, // endpoints 1..16
    controller: alloc::sync::Weak<async_lock::Mutex<super::Ehci>>,
    usb_device_driver: async_lock::Mutex<Option<Box<dyn crate::UsbDeviceDriver>>>,

    configurations: u8,
}

impl UsbDeviceAccessor {
    pub(super) async unsafe fn insert_into_controller(
        address: crate::DeviceAddress,
        ctl_endpoint: alloc::sync::Arc<EndpointQueue>,
        controller: alloc::sync::Arc<async_lock::Mutex<super::Ehci>>,
        portnum: u8,
    ) {
        // Get head of Device descriptor so we can set the transaction size
        let data_buff = hootux::mem::dma::DmaBuff::from(alloc::vec![0u8; 8]);
        let ts = TransactionString::setup_transaction(
            {
                let descriptor = usb_cfg::CtlTransfer::get_descriptor::<
                    usb_cfg::descriptor::DeviceDescriptor,
                >(0, Some(8));
                log::debug!("{descriptor:?} : {:?}", descriptor.to_bytes());
                DmaBuff::from(Vec::from(descriptor.to_bytes()))
            },
            Some((data_buff, crate::PidCode::In)),
        );

        let (buffer, 8, Ok(_)) = ctl_endpoint.append_cmd_string(ts).await.complete() else {
            panic!("USB device threw error on initialisation")
        };
        ctl_endpoint
            .update_transaction_len(
                usb_cfg::descriptor::DeviceDescriptor::packet_size((&*buffer).try_into().unwrap())
                    .unwrap(),
            )
            .unwrap(); // Will never panic

        let data_buff = hootux::mem::dma::DmaBuff::from(
            alloc::vec![0u8; size_of::<usb_cfg::descriptor::DeviceDescriptor>()],
        );
        let ts = TransactionString::setup_transaction(
            hootux::mem::dma::DmaBuff::from(Vec::from(
                usb_cfg::CtlTransfer::get_descriptor::<usb_cfg::descriptor::DeviceDescriptor>(
                    0,
                    Some(size_of::<usb_cfg::descriptor::DeviceDescriptor>() as u16),
                )
                .to_bytes(),
            )),
            Some((data_buff, crate::PidCode::In)),
        );
        let (raw_buffer, _, Ok(_)) = ctl_endpoint.append_cmd_string(ts).await.complete() else {
            panic!("USB device threw error on initialisation")
        };
        let buff = usb_cfg::descriptor::DeviceDescriptor::from_raw(&*raw_buffer).unwrap();
        let mut l = controller.lock().await;

        let mut this = Self {
            major_num: l.major_num,
            address,
            ctl_endpoint,
            endpoints: alloc::sync::Arc::new(async_lock::Mutex::new([const { None }; 15])),
            controller: alloc::sync::Weak::new(),
            usb_device_driver: async_lock::Mutex::new(None),
            configurations: buff.num_configurations,
        };
        this.controller = alloc::sync::Arc::downgrade(&controller);
        l.port_files.insert(portnum, alloc::sync::Arc::new(this));
        hootux::fs::sysfs::SysFsRoot::new()
            .bus
            .event_file()
            .notify_event()
    }

    pub(super) fn get_file(self: &alloc::sync::Arc<Self>) -> Box<UsbDeviceFile> {
        Box::new(UsbDeviceFile {
            major_num: self.major_num,
            address: self.address.into(),
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

            fn len(&self) -> IoResult<'_, u64> {
                async { Ok(8) }.boxed()
            }
        }

        impl NormalFile for $name {
            fn len_chars(&self) -> IoResult<'_, u64> {
                self.len()
            }

            fn file_lock<'a>(
                self: Box<Self>,
            ) -> BoxFuture<'a, Result<LockedFile<u8>, (IoError, Box<dyn NormalFile<u8>>)>> {
                async { Err((IoError::NotSupported, self as Box<dyn NormalFile<u8>>)) }.boxed()
            }

            unsafe fn unlock_unsafe(&self) -> IoResult<'_, ()> {
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
    /// the first 3 bytes of `pos` (le) indicate the class to be searched for. The lower bits of the
    /// fourth byte contains a bitmap indicating the descriptors at which this a match can be found.
    /// Bits 6..7 of the fourth byte indicate which class fields must be matched.
    ///
    /// - A value of `0` will match class subclass and protocol
    /// - A value of `1` will match class and subclass.
    /// - A value of `2` will match only match the class.
    ///
    /// All other values in the fourth byte are reserved and will return [IoError::EndOfFile].
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
    /// where the class is first found. The results will be returned in the first two
    /// bytes of the data buffer.
    ///
    /// * The first byte will indicate which configuration the class was found in (0 if device).
    /// * The second byte indicates what descriptor the class was found in.
    /// * If the class is not found, no data will be written to the buffer and the returned data length will be `0`
    fn read(
        &self,
        pos: u64,
        mut buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async move {
            let (class, descriptors) = match pos.to_le_bytes() {
                [_, _, _, descriptors_bitmap, ..]
                if DescriptorBitmap::from_bits(descriptors_bitmap).unwrap_or(DescriptorBitmap::empty()).is_empty() =>
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
                        let buffer = hootux::mem::dma::DmaBuff::from(alloc::vec![0u8;255]);

                        let command = usb_cfg::CtlTransfer::get_descriptor::<$descriptor>($index,Some(255));
                        let cmd_raw = DmaBuff::from(Vec::from(command.to_bytes()));

                        let ts = TransactionString::setup_transaction(cmd_raw,Some((buffer,crate::PidCode::In)));

                        let ctl: &alloc::sync::Arc<EndpointQueue> = &acc.ctl_endpoint;
                        let rc = ctl.append_cmd_string(ts).await;
                        let (ret_buff,len,Ok(_)) = rc.complete() else {
                            return Err((IoError::MediaError,buff,0))
                        };
                        let mut g: Vec<_> = ret_buff.try_into().unwrap();
                        g.truncate(len);
                        g
                    }
                };
            }

            let dev_descriptor_buffer = get_descriptor_full!(usb_cfg::descriptor::DeviceDescriptor,0);
            let Some(dev_descriptor): Option<&usb_cfg::descriptor::DeviceDescriptor> = usb_cfg::descriptor::Descriptor::from_raw(&dev_descriptor_buffer) else {
                let len = buff.len(); // todo this is the wrong len
                return Err((IoError::MediaError, buff, len));
            };
            if dev_descriptor.class()[descriptors.class_len()] == class[descriptors.class_len()] && descriptors.contains(DescriptorBitmap::DEVICE) {
                // SAFETY: This is safe because `DmaTarget::data_ptr()` must be writable.
                (&mut *buff)[0..2].copy_from_slice(&[0, 1]);
                return Ok((buff, 2));
            }

            // If only device is present
            if !descriptors.contains(DescriptorBitmap::DEVICE.complement()) {
                return Ok((buff, 0));
            }

            for configuration in 0..dev_descriptor.num_configurations {
                let cfg_raw = get_descriptor_full!(usb_cfg::descriptor::ConfigurationDescriptor,configuration);
                let Some(cfg_descriptor): Option<&usb_cfg::descriptor::ConfigurationDescriptor> = Descriptor::from_raw(&cfg_raw) else { return Err((IoError::MediaError, buff, 0)) };

                // SAFETY: We fetch the max size for a descriptor, so we definitely have the whole thing & it has not been modified.
                // `cast` is guaranteed by iter
                for interface_descriptor in unsafe { cfg_descriptor.iter().filter(|h| h.cast::<usb_cfg::descriptor::DeviceDescriptor>().is_some()) } {
                    let Some(BaseDescriptor::Interface(interface_descriptor)) = interface_descriptor.base_descriptor() else {unreachable!()};
                    if descriptors.contains(DescriptorBitmap::INTERFACE) && interface_descriptor.class()[descriptors.class_len()] == class[descriptors.class_len()] {
                        (&mut *buff)[0..2].copy_from_slice(&[configuration, DescriptorBitmap::INTERFACE.bits()]);
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
    fn write(
        &self,
        _pos: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async { Err((IoError::ReadOnly, buff, 0)) }.boxed()
    }
}

bitflags::bitflags! {
    pub struct DescriptorBitmap: u8 {
        const DEVICE = 1;
        const INTERFACE = 1 << 1;
        const MATCH_CLASS = 1 << 7;
        const MATCH_SUBCLASS = 1 << 6;
    }
}

impl DescriptorBitmap {
    const fn class_len(&self) -> core::ops::Range<usize> {
        if !self.contains(DescriptorBitmap::MATCH_CLASS)
            && !self.contains(DescriptorBitmap::MATCH_SUBCLASS)
        {
            0..3
        } else if !self.contains(DescriptorBitmap::MATCH_CLASS)
            && self.contains(DescriptorBitmap::MATCH_SUBCLASS)
        {
            0..2
        } else if self.contains(DescriptorBitmap::MATCH_CLASS)
            && !self.contains(DescriptorBitmap::MATCH_SUBCLASS)
        {
            0..1
        } else {
            0..0 // invalid, will always cause EOF
        }
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
    fn read(
        &self,
        pos: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async move {
            let Some(acc) = self.usb_device_accessor.upgrade() else {
                return Err((IoError::NotPresent, buff, 0));
            };
            const DEVICE_DESCRIPTOR_ID: u8 = usb_cfg::DescriptorType::Device.0;
            const DEVICE_QUALIFIER_ID: u8 = usb_cfg::DescriptorType::DeviceQualifier.0;
            const CONFIGURATION_DESCRIPTOR_ID: u8 = usb_cfg::DescriptorType::Configuration.0;
            const OTHER_SPEED_CONFIGURATION: u8 =
                usb_cfg::DescriptorType::OtherSpeedConfiguration.0;

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
                [index, CONFIGURATION_DESCRIPTOR_ID, ..] if index < acc.configurations => {
                    Vec::from(
                        usb_cfg::CtlTransfer::get_descriptor::<
                            usb_cfg::descriptor::ConfigurationDescriptor,
                        >(index, Some(buff.len().try_into().unwrap_or(255)))
                        .to_bytes(),
                    )
                }
                [index, OTHER_SPEED_CONFIGURATION, ..] if index < acc.configurations => Vec::from(
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

            let command = hootux::mem::dma::DmaBuff::from(command);
            // SAFETY: Unsafe
            // todo: I must change DmaBuffer to be `+ 'static`
            let ts = TransactionString::setup_transaction(
                command,
                Some((unsafe { core::mem::transmute(buff) }, crate::PidCode::In)),
            );

            // We need to actually get completion information from the ts rather than just assuming
            // it completed properly and completely
            let (buff, len, result) = acc.ctl_endpoint.append_cmd_string(ts).await.complete();
            if let Err(_) = result {
                return Err((IoError::MediaError, buff, len));
            }
            // If we are fetching the current configuration then the len is always `1`
            if pos != 0 {
                Ok((buff, len))
            } else {
                Ok((buff, 1))
            }
        }
        .boxed()
    }
}

impl Write<u8> for ConfigurationIo {
    fn write(
        &self,
        _: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async { Err((IoError::ReadOnly, buff, 0)) }.boxed()
    }
}

pub mod frontend {
    use super::*;
    use crate::ehci::PeriodicEndpointQueue;
    use alloc::sync::Arc;
    use core::task::Poll;

    pub struct UsbDevCtl {
        pub(super) acc: alloc::sync::Weak<UsbDeviceAccessor>,
        pub(super) endpoints: async_lock::MutexGuardArc<[Option<Arc<EndpointQueue>>; 15]>,
    }

    impl UsbDevCtl {
        /// Requests a particular descriptor with a given index. If `T` is a
        /// [usb_cfg::descriptor::DeviceDescriptor] or [usb_cfg::descriptor::DeviceQualifier] then
        /// `index` is ignored.
        ///
        /// This may issue 2 transactions.
        pub async fn get_descriptor<T: usb_cfg::descriptor::RequestableDescriptor>(
            &mut self,
            index: u8,
        ) -> Result<Vec<u8>, IoError> {
            let base_len = size_of::<T>();
            let buffer = alloc::vec![0u8; base_len];
            let command = DmaBuff::from(Vec::from(
                usb_cfg::CtlTransfer::get_descriptor::<T>(index, Some(buffer.len() as u16))
                    .to_bytes(),
            ));

            let acc = self.acc.upgrade().ok_or(IoError::NotPresent)?;
            let ts = TransactionString::setup_transaction(
                command,
                Some((DmaBuff::from(buffer), crate::PidCode::In)),
            );
            let (mut b, _, Ok(_)) = acc.ctl_endpoint.append_cmd_string(ts).await.complete() else {
                return Err(IoError::MediaError);
            };

            let op_len = u16::from_le_bytes((&b[2..=3]).try_into().unwrap());

            let mut buffer: Vec<u8> = b.try_into().unwrap(); // Uses global, wont panic
            buffer.resize(op_len as usize, 0);
            let buffer = DmaBuff::from(buffer);
            let ts = TransactionString::setup_transaction(
                DmaBuff::from(Vec::from(
                    usb_cfg::CtlTransfer::get_descriptor::<T>(index, Some(op_len)).to_bytes(),
                )),
                Some((buffer, crate::PidCode::In)),
            );
            let (rc, _, Ok(_)) = acc.ctl_endpoint.append_cmd_string(ts).await.complete() else {
                return Err(IoError::MediaError);
            };
            b = rc;

            if b[1] == T::DESCRIPTOR_TYPE.0 {
                let mut v: Vec<u8> = b.try_into().unwrap();
                v.shrink_to_fit();
                Ok(v)
            } else {
                Err(IoError::MediaError)
            }
        }

        /// Sets the device's configuration to the `configuration`
        pub async fn set_configuration(&mut self, configuration: u8) -> Result<(), IoError> {
            let acc = self.acc.upgrade().ok_or(IoError::NotPresent)?;
            if configuration > acc.configurations {
                return Err(IoError::InvalidData);
            }
            let command =
                Vec::from(usb_cfg::CtlTransfer::set_configuration(configuration).to_bytes());
            acc.ctl_endpoint
                .append_cmd_string(TransactionString::setup_transaction(
                    DmaBuff::from(command),
                    None,
                ))
                .await
                .complete()
                .2
                .map_err(|_| IoError::MediaError)?;
            Ok(())
        }

        /// Configures an endpoint for asynchronous or interrupt transactions.
        ///
        /// # Safety
        ///
        /// The caller must ensure the endpoint is actually configured.
        pub async unsafe fn setup_endpoint(
            &mut self,
            descriptor: &usb_cfg::descriptor::EndpointDescriptor,
        ) -> Result<Arc<EndpointQueue>, IoError> {
            enum EndpointWrapper {
                Async(Arc<EndpointQueue>),
                Interrupt(PeriodicEndpointQueue),
            }

            impl EndpointWrapper {
                fn as_endpoint(&self) -> &Arc<EndpointQueue> {
                    match self {
                        EndpointWrapper::Async(inner) => inner,
                        EndpointWrapper::Interrupt(int) => &int.endpoint,
                    }
                }
            }

            let acc = self.acc.upgrade().ok_or(IoError::NotPresent)?;
            let pid = if descriptor.attributes.transfer_type()
                == usb_cfg::descriptor::EndpointTransferType::Control
            {
                crate::PidCode::Control
            } else {
                if descriptor.endpoint_address.in_endpoint() {
                    crate::PidCode::In
                } else {
                    crate::PidCode::Out
                }
            };
            let dt_ctl = pid == crate::PidCode::Control;

            let ep = match descriptor.attributes.transfer_type() {
                usb_cfg::descriptor::EndpointTransferType::Bulk
                | usb_cfg::descriptor::EndpointTransferType::Control => {
                    let qh = EndpointQueue::new_async(
                        crate::Target {
                            dev: acc.address,
                            endpoint: descriptor.into(),
                        },
                        pid,
                        descriptor.max_packet_size.max_packet_size() as u32,
                        dt_ctl,
                    );
                    let qh = Arc::new(qh);
                    EndpointWrapper::Async(qh)
                }
                usb_cfg::descriptor::EndpointTransferType::Interrupt => {
                    let ep = EndpointQueue::new_periodic(
                        crate::Target {
                            dev: acc.address,
                            endpoint: descriptor.into(),
                        },
                        pid,
                        descriptor.max_packet_size.max_packet_size() as u32,
                        1 << (descriptor.interval - 1),
                    );

                    EndpointWrapper::Interrupt(ep)
                }
                usb_cfg::descriptor::EndpointTransferType::Isochronous => {
                    log::error!(
                        "Called {}::{}::setup_endpoint() on Isochronous endpoint",
                        module_path!(),
                        core::any::type_name::<Self>()
                    );
                    return Err(IoError::InvalidData);
                }
            };

            match self.endpoints[descriptor.endpoint_address.endpoint() as usize]
                .replace(ep.as_endpoint().clone())
            {
                None => {}
                Some(old_head) => {
                    self.endpoints[descriptor.endpoint_address.endpoint() as usize] =
                        Some(old_head);
                    log::error!("Attempted to configure endpoint that was already configured");
                    return Err(IoError::AlreadyExists);
                }
            }

            let mut ctl = acc
                .controller
                .upgrade()
                .ok_or(IoError::NotPresent)?
                .lock_arc()
                .await;

            match ep {
                EndpointWrapper::Async(ref ep) => ctl.insert_into_async(ep.clone()),
                EndpointWrapper::Interrupt(ref per) => {
                    ctl.insert_into_periodic([per.clone()].into_iter())
                        .await
                        .map_err(|_| IoError::MediaError)?;
                }
            }

            Ok(ep.as_endpoint().clone())
        }

        pub fn endpoint(&self, ep: impl Into<crate::Endpoint>) -> Option<Arc<EndpointQueue>> {
            Some(
                self.endpoints[Into::<u8>::into(ep.into()) as usize]
                    .as_ref()?
                    .clone(),
            )
        }

        /// Deconfigures the device and performs cleanup at the device driver level.
        /// This should be called instead of [Drop::drop]
        pub async fn shutdown(mut self) {
            self.shutdown_inner().await;

            for i in self.endpoints.iter_mut() {
                i.take();
            }

            self.acc = alloc::sync::Weak::new();
            // SAFETY: This is safe because this coerced from a reference and the remnants are forgotten.
            unsafe { core::ptr::drop_in_place(&mut self.endpoints) };
            core::mem::forget(self);
        }

        async fn shutdown_inner(&mut self) {
            let Some(acc) = self.acc.upgrade() else {
                return;
            }; // If `acc` is not present then we can just drop everything dumbly.
            let command = DmaBuff::from(Vec::from(
                usb_cfg::CtlTransfer::set_configuration(0).to_bytes(),
            ));
            let ts = TransactionString::setup_transaction(command, None);
            // fixme: This should check for errors.
            let _ = acc.ctl_endpoint.append_cmd_string(ts).await; // deconfigure, this returns no useful information

            let Some(ctl) = acc.controller.upgrade() else {
                return;
            };
            let mut ctl = ctl.lock().await;

            ctl.drop_endpoints(
                acc.endpoints
                    .lock_arc()
                    .await
                    .iter()
                    .flatten()
                    .map(|arc| &**arc),
            )
            .await
        }

        /// Sends a generic command via the control pipe.
        ///
        /// # Safety
        ///
        /// Command that are defined in the USB serial bus specifications or any commands that
        /// affect the generic device state may not be sent using this function.
        pub async unsafe fn send_command(
            &self,
            command: usb_cfg::CtlTransfer,
            buffer: Option<DmaBuff>,
        ) -> (Option<DmaBuff>, Result<usize, IoError>) {
            let Some(cmd_ep) = self
                .acc
                .upgrade()
                .and_then(|e| Some(e.ctl_endpoint.clone()))
            else {
                return (buffer, Err(IoError::NotPresent));
            };
            let buffer_present = buffer.is_some();
            if (command.data_len() == 0) == buffer_present {
                log::debug!("Attempted to send command with invalid buffer");
                return (buffer, Err(IoError::InvalidData));
            } else if command.data_len() > buffer.as_ref().and_then(|b| Some(b.len())).unwrap_or(0)
            {
                log::debug!("Buffer was smaller than request size... Ignoring");
            };

            let pid = if command.is_rx_command() {
                crate::PidCode::In
            } else {
                crate::PidCode::Out
            };

            let b = if let Some(buff) = buffer {
                Some((buff, pid))
            } else {
                None
            };

            let cmd_ts =
                TransactionString::setup_transaction(Vec::from(command.to_bytes()).into(), b);
            let completion = cmd_ep.append_cmd_string(cmd_ts).await;
            let (buffer, len, status) = completion.complete();
            let status = match status {
                Ok(crate::UsbError::RecoveredError) => {
                    log::trace!("USB corrected error");
                    Ok(len)
                }
                Ok(crate::UsbError::Ok) => Ok(len),
                Err(()) => {
                    log::error!("Device error: Halted");
                    Err(IoError::MediaError)
                }
                Ok(crate::UsbError::Halted) => unreachable!(), // This is never returned by completion, Err(()) is used instead.
            };
            if buffer_present {
                (Some(buffer), status)
            } else {
                (None, status) // Buffer is bogus, so this is fine to drop.
            }
        }

        /// Returns a file object for self.
        pub fn to_file(&self) -> Result<UsbDeviceFile, IoError> {
            let accessor = self.acc.upgrade().ok_or(IoError::NotPresent)?;
            Ok(UsbDeviceFile {
                major_num: accessor.major_num,
                address: accessor.address.into(),
                usb_device_accessor: Arc::downgrade(&accessor),
            })
        }
    }

    impl Drop for UsbDevCtl {
        #[track_caller]
        fn drop(&mut self) {
            let Some(acc) = self.acc.upgrade() else {
                log::trace!(
                    "{} Dropped blocking but acc was not present",
                    core::any::type_name::<Self>()
                );
                return; // Default-style drop if acc is not present
            };
            let mut shutdown = pin!(self.shutdown_inner());

            // We do this nonsense reduce the amount of time we are blocked.
            let mut poll;
            poll = shutdown.poll_unpin(&mut core::task::Context::from_waker(
                core::task::Waker::noop(),
            ));
            log::error!("UsbDevCtl dropped at:{}", core::panic::Location::caller());
            if Poll::Pending == poll {
                poll = shutdown.poll_unpin(&mut core::task::Context::from_waker(
                    core::task::Waker::noop(),
                ));
            }
            log::info!(
                "Blocking on shutdown for usb device {:?}",
                DevID::new(acc.major_num, Into::<u8>::into(acc.address) as usize)
            );
            if Poll::Pending == poll {
                hootux::block_on!(shutdown);
            }
        }
    }
}
