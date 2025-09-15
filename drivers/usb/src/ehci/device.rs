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
use hootux::mem::dma::{DmaBuff, DmaClaimable};
use usb_cfg::descriptor::Descriptor;

#[derive(Clone)]
#[file]
pub(super) struct UsbDeviceFile {
    major_num: MajorNum,
    address: u8,
    usb_device_accessor: alloc::sync::Weak<UsbDeviceAccessor>,
}

impl UsbDeviceFile {
    async fn acquire_ctl(
        &self,
        driver: &Box<dyn UsbDeviceDriver>,
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
        *acc_driver = Some(UsbDeviceDriver::clone(&**driver));
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

    fn len(&self) -> IoResult<u64> {
        async { Ok(hootux::fs::sysfs::SysfsDirectory::entries(self) as u64) }.boxed()
    }

    fn method_call<'f, 'a: 'f, 'b: 'f>(
        &'b self,
        method: &str,
        arguments: &'a (dyn Any + Send + Sync + 'a),
    ) -> IoResult<'f, MethodRc> {
        impl_method_call!(method,arguments => acquire_ctl(Box<UsbDeviceDriver>))
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
    address: crate::DeviceAddress,
    ctl_endpoint: alloc::sync::Arc<EndpointQueue>,
    /// Contains all the optional endpoints.
    /// When configuring the device this mutex must be locked.
    endpoints: alloc::sync::Arc<async_lock::Mutex<[Option<alloc::sync::Arc<EndpointQueue>>; 15]>>, // endpoints 1..16
    controller: alloc::sync::Weak<async_lock::Mutex<super::Ehci>>,
    usb_device_driver: async_lock::Mutex<Option<Box<dyn UsbDeviceDriver>>>,

    configurations: u8,
}

trait UsbDeviceDriver: SysfsDirectory + 'static {
    fn clone(&self) -> Box<dyn UsbDeviceDriver>;
}

impl UsbDeviceAccessor {
    pub(super) async unsafe fn insert_into_controller(
        address: crate::DeviceAddress,
        ctl_endpoint: alloc::sync::Arc<EndpointQueue>,
        controller: alloc::sync::Arc<async_lock::Mutex<super::Ehci>>,
    ) {
        let mut l = controller.lock().await;

        let data_buff = hootux::mem::dma::DmaGuard::new(alloc::vec![0u8; 8]);
        let (guarded, borrowed) = data_buff.claim().unwrap();
        let ts = TransactionString::setup_transaction(
            {
                let descriptor = usb_cfg::CtlTransfer::get_descriptor::<
                    usb_cfg::descriptor::DeviceDescriptor,
                >(0, Some(8));
                log::debug!("{descriptor:?} : {:?}", descriptor.to_bytes());
                Box::new(hootux::mem::dma::DmaGuard::new(Vec::from(
                    descriptor.to_bytes(),
                )))
            },
            Some((borrowed, crate::PidCode::In)),
        );

        let (_, 8, Ok(_)) = ctl_endpoint.append_cmd_string(ts).await.complete() else {
            panic!("USB device threw error on initialisation")
        };
        let buffer = guarded.unwrap().unwrap().unwrap();
        ctl_endpoint
            .update_transaction_len(
                usb_cfg::descriptor::DeviceDescriptor::packet_size((&*buffer).try_into().unwrap())
                    .unwrap(),
            )
            .unwrap(); // Will never panic

        let data_buff = hootux::mem::dma::DmaGuard::new(
            alloc::vec![0u8; size_of::<usb_cfg::descriptor::DeviceDescriptor>()],
        );
        let (guarded, borrowed) = data_buff.claim().unwrap();
        let ts = TransactionString::setup_transaction(
            Box::new(hootux::mem::dma::DmaGuard::new(Vec::from(
                usb_cfg::CtlTransfer::get_descriptor::<usb_cfg::descriptor::DeviceDescriptor>(
                    0,
                    Some(size_of::<usb_cfg::descriptor::DeviceDescriptor>() as u16),
                )
                .to_bytes(),
            ))),
            Some((borrowed, crate::PidCode::In)),
        );
        let (_, _, Ok(_)) = ctl_endpoint.append_cmd_string(ts).await.complete() else {
            panic!("USB device threw error on initialisation")
        };
        let raw_buffer = guarded.unwrap().unwrap().unwrap();
        let buff = usb_cfg::descriptor::DeviceDescriptor::from_raw(&*raw_buffer).unwrap();

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
        l.port_files
            .insert(this.address.into(), alloc::sync::Arc::new(this));
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
    /// where the class is first found. The results will be returned in the first two
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
                        let guarded = hootux::mem::dma::DmaGuard::new(alloc::vec![0u8;255]);
                        let (guarded,borrow) = guarded.claim().unwrap();
                        let command = usb_cfg::CtlTransfer::get_descriptor::<$descriptor>($index,Some(255));
                        let cmd_raw = Vec::from(command.to_bytes());

                        let command = hootux::mem::dma::DmaGuard::new(cmd_raw);
                        let ts = TransactionString::setup_transaction(Box::new(command),Some((borrow,crate::PidCode::In)));

                        let ctl: &alloc::sync::Arc<EndpointQueue> = &acc.ctl_endpoint;
                        let rc = ctl.append_cmd_string(ts).await;
                        let (_,len,Ok(_)) = rc.complete() else {
                            return Err((IoError::MediaError,buff,0))
                        };
                        let mut g = guarded.unwrap().unwrap().unwrap();
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
            if dev_descriptor.class() == class && descriptors.contains(DescriptorBitmap::DEVICE) {
                // SAFETY: This is safe because `DmaTarget::data_ptr()` must be writable.
                unsafe { (&mut *buff.data_ptr())[0..2].copy_from_slice(&[0, 1]) }
                return Ok((buff, 2));
            }

            // If only device is present
            if !descriptors.contains(DescriptorBitmap::DEVICE.complement()) {
                return Ok((buff, 0));
            }

            for configuration in 0..dev_descriptor.num_configurations {
                let cfg_raw = get_descriptor_full!(usb_cfg::descriptor::ConfigurationDescriptor,configuration);
                let Some(cfg_descriptor): Option<&usb_cfg::descriptor::ConfigurationDescriptor> = usb_cfg::descriptor::Descriptor::from_raw(&cfg_raw) else { return Err((IoError::MediaError, buff, 0)) };
                // SAFETY: We fetch the max size for a descriptor, so we definitely have the whole thing & it has not been modified.
                for interface_desc in unsafe { cfg_descriptor.iter() } {
                    if descriptors.contains(DescriptorBitmap::INTERFACE) && interface_desc.class() == class {
                        unsafe { (&mut *buff.data_ptr())[0..2].copy_from_slice(&[configuration, DescriptorBitmap::INTERFACE.bits()]) };
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

            let command = hootux::mem::dma::DmaGuard::new(command);
            // SAFETY: Unsafe
            // todo: I must change DmaBuffer to be `+ 'static`
            let ts = TransactionString::setup_transaction(
                Box::new(command),
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

            let command = hootux::mem::dma::DmaGuard::new(Vec::from(
                usb_cfg::CtlTransfer::set_configuration(target_config).to_bytes(),
            ));
            // SAFETY: `command` is stack allocated and will not be dropped before the operation is completed.
            let ts = TransactionString::setup_transaction(Box::new(command), None);
            // no data stage here
            let sc: super::StringCompletion = acc.ctl_endpoint.append_cmd_string(ts).await;

            drop(ports);
            let (buff, len, ok) = sc.complete();
            match ok {
                Ok(_) => Ok((buff, len)),
                Err(_) => Err((IoError::MediaError, buff, len)),
            }
        }
        .boxed()
    }
}

pub mod frontend {
    use super::*;
    use alloc::sync::Arc;
    use core::task::Poll;
    use hootux::mem::dma::DmaGuard;

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
            mut index: u8,
        ) -> Result<Box<T>, IoError> {
            let is_device = if core::any::TypeId::of::<T>()
                == core::any::TypeId::of::<usb_cfg::descriptor::DeviceDescriptor>()
                || core::any::TypeId::of::<T>()
                    == core::any::TypeId::of::<usb_cfg::descriptor::DeviceQualifier>()
            {
                index = 0;
                true
            } else {
                false
            };
            let base_len = size_of::<T>();
            let buffer = alloc::vec![0u8; base_len];
            let command = Box::new(DmaGuard::new(Vec::from(
                usb_cfg::CtlTransfer::get_descriptor::<T>(index, Some(buffer.len() as u16))
                    .to_bytes(),
            )));

            let (guarded, borrow) = DmaGuard::new(buffer).claim().unwrap();
            let acc = self.acc.upgrade().ok_or(IoError::NotPresent)?;
            let ts =
                TransactionString::setup_transaction(command, Some((borrow, crate::PidCode::In)));
            let (_, _, Ok(_)) = acc.ctl_endpoint.append_cmd_string(ts).await.complete() else {
                return Err(IoError::MediaError);
            };

            let mut b = guarded.unwrap().unwrap().unwrap();
            if is_device {
                let op_len = u16::from_le_bytes((&b[2..3]).try_into().unwrap());
                b.resize(op_len as usize, 0);
                let (guarded, borrow) = DmaGuard::new(b).claim().unwrap();
                let ts = TransactionString::setup_transaction(
                    Box::new(DmaGuard::new(Vec::from(
                        usb_cfg::CtlTransfer::get_descriptor::<T>(index, Some(op_len)).to_bytes(),
                    ))),
                    Some((borrow, crate::PidCode::In)),
                );
                let (_, _, Ok(_)) = acc.ctl_endpoint.append_cmd_string(ts).await.complete() else {
                    return Err(IoError::MediaError);
                };
                b = guarded.unwrap().unwrap().unwrap();
            }

            if b[1] == T::DESCRIPTOR_TYPE as u8 {
                b.shrink_to_fit();
                let ptr = (&raw mut *b.leak()).cast::<T>();
                // SAFETY: We have checked that this is T and that it is the correct size for T
                Ok(unsafe { Box::from_raw(ptr) })
            } else {
                Err(IoError::MediaError)
            }
        }

        /// Sets the device's configuration to the `configuration`
        async fn set_configuration(&mut self, configuration: u8) -> Result<(), IoError> {
            let acc = self.acc.upgrade().ok_or(IoError::NotPresent)?;
            if acc.configurations >= configuration {
                return Err(IoError::InvalidData);
            }
            let command =
                Vec::from(usb_cfg::CtlTransfer::set_configuration(configuration).to_bytes());
            acc.ctl_endpoint
                .append_cmd_string(TransactionString::setup_transaction(
                    Box::new(DmaGuard::new(command)),
                    None,
                ))
                .await
                .complete()
                .2
                .map_err(|_| IoError::MediaError)?;
            Ok(())
        }

        /// Configures an endpoint in the async list for the device.
        ///
        /// # Safety
        ///
        /// The caller must ensure the endpoint is actually configured.
        async unsafe fn setup_endpoint(
            &mut self,
            descriptor: &usb_cfg::descriptor::EndpointDescriptor,
        ) -> Result<Arc<EndpointQueue>, IoError> {
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
            match self.endpoints[descriptor.endpoint_address.endpoint() as usize]
                .replace(qh.clone())
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
            ctl.insert_into_async(qh.clone());
            Ok(qh)
        }

        fn endpoint(&self, ep: impl Into<crate::Endpoint>) -> Option<Arc<EndpointQueue>> {
            Some(
                self.endpoints[Into::<u8>::into(ep.into()) as usize]
                    .as_ref()?
                    .clone(),
            )
        }

        async fn shutdown(mut self) {
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
            let command = Box::new(DmaGuard::new(Vec::from(
                usb_cfg::CtlTransfer::set_configuration(0).to_bytes(),
            )));
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
