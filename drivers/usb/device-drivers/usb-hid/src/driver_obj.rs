use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::any::Any;
use hootux::fs::file::*;
use hootux::fs::sysfs::{SysfsDirectory, SysfsFile};
use hootux::fs::vfs::DeviceIdDistributer;
use hootux::fs::{IoError, IoResult};
use usb::ehci::EndpointQueue;
use usb_cfg::descriptor::{Descriptor, DeviceDescriptor, EndpointTransferType};

static ID_DISPENSER: DeviceIdDistributer = DeviceIdDistributer::new();

struct DriverState {
    devctl: Option<usb::UsbDevCtl>,

    // devctl owns these
    in_endpoint: Weak<EndpointQueue>,
    out_endpoint: Weak<EndpointQueue>,
}

impl DriverState {
    const fn new() -> Self {
        Self {
            devctl: None,
            in_endpoint: Weak::new(),
            out_endpoint: Weak::new(),
        }
    }
}

#[file]
pub(crate) struct DriverStateFile {
    inner: Arc<async_lock::RwLock<DriverState>>,
    minor: core::sync::atomic::AtomicUsize,
}

impl DriverStateFile {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(async_lock::RwLock::new(DriverState::new())),
            minor: core::sync::atomic::AtomicUsize::new(usize::MAX),
        }
    }

    pub(crate) async fn install_ctl(&self, ctl: usb::UsbDevCtl) {
        self.inner.write().await.devctl = Some(ctl);
    }

    pub(crate) fn runtime(&self) -> DriverRuntime {
        DriverRuntime {
            inner: self.inner.clone(),
            id: self.device(),
        }
    }
}

impl Clone for DriverStateFile {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            minor: core::sync::atomic::AtomicUsize::new(
                self.minor.load(core::sync::atomic::Ordering::Relaxed),
            ),
        }
    }
}

impl File for DriverStateFile {
    fn file_type(&self) -> FileType {
        FileType::Directory
    }

    fn block_size(&self) -> u64 {
        hootux::mem::PAGE_SIZE as u64
    }

    fn device(&self) -> DevID {
        let major = ID_DISPENSER.major();

        // Minor needs to be lazily initialised, because we need self to attempt to bind using self.
        // Failing to bind would otherwise allocate a minor number.
        let t = self.minor.load(core::sync::atomic::Ordering::Relaxed);
        let minor = if t == usize::MAX {
            ID_DISPENSER.alloc_id().as_int().1
        } else {
            t
        };

        DevID::new(major, minor)
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(Clone::clone(self))
    }

    fn id(&self) -> u64 {
        0
    }

    fn len(&self) -> IoResult<'_, u64> {
        Box::pin(async { Ok(SysfsDirectory::entries(self) as u64) })
    }
}

impl SysfsDirectory for DriverStateFile {
    fn entries(&self) -> usize {
        todo!()
    }

    fn file_list(&self) -> Vec<String> {
        todo!()
    }

    fn get_file(&self, _name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        todo!()
    }

    fn store(&self, _name: &str, _file: Box<dyn SysfsFile>) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn remove(&self, _name: &str) -> Result<(), IoError> {
        Err(IoError::NotSupported)
    }

    fn as_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl usb::UsbDeviceDriver for DriverStateFile {
    fn clone(&self) -> Box<dyn usb::UsbDeviceDriver> {
        Box::new(Clone::clone(self))
    }
}

pub(crate) struct DriverRuntime {
    inner: Arc<async_lock::RwLock<DriverState>>,
    id: DevID,
}

impl DriverRuntime {
    pub(crate) async fn run(self) -> hootux::task::TaskResult {
        let Ok(_) = self.configure_device().await.inspect_err(|e| {
            log::error!("Failed to initialize USB HID device {}: {:?}", self.id, e)
        }) else {
            return hootux::task::TaskResult::Error;
        };
        loop {
            return hootux::task::TaskResult::ExitedNormally;
        }
    }

    async fn configure_device(&self) -> Result<(), IoError> {
        let mut driver_state = self.inner.write().await;

        macro_rules! devctl {
            () => {{
                let Some(ref mut ctl) = driver_state.devctl else {
                    unreachable!()
                };
                ctl
            }};
        }

        log::trace!("Attempting to configure device {} as HID", self.id);

        let dev_descriptor_buff = devctl!().get_descriptor::<DeviceDescriptor>(0).await?;
        // SAFETY: `get_descriptor<T>()` is guaranteed to return T
        let dev_descriptor =
            unsafe { DeviceDescriptor::from_raw(&dev_descriptor_buff).unwrap_unchecked() };

        // Iterate over configurations to locate our
        'cfg: for configuration_no in 0..dev_descriptor.num_configurations {
            let descriptor_buff = devctl!()
                .get_descriptor::<usb_cfg::descriptor::ConfigurationDescriptor>(configuration_no)
                .await?;
            let descriptor = unsafe {
                usb_cfg::descriptor::ConfigurationDescriptor::<usb_cfg::descriptor::Normal>::from_raw(&descriptor_buff)
                    .unwrap_unchecked()
            };

            // SAFETY: get_descriptor always returns all the descriptor data.
            for mut interface_group in unsafe {
                descriptor
                    .iter()
                    .fold_on(usb_cfg::DescriptorType::Interface)
            } {
                let interface = interface_group
                    .next()
                    .unwrap()
                    .cast::<usb_cfg::descriptor::InterfaceDescriptor>()
                    .unwrap(); // First element is always fold_on type.

                if interface.class()[0] == crate::USB_HID_CLASS {
                    devctl!()
                        .set_configuration(configuration_no)
                        .await
                        .inspect_err(|e| {
                            log::error!("Failed to configure {}: {e:?}, aborting", self.id)
                        })?;

                    use usb_cfg::descriptor::BaseDescriptor;
                    // SAFETY: get_descriptor always returns all the descriptor data.
                    for descriptor in interface_group {
                        match descriptor.base_descriptor() {
                            Some(BaseDescriptor::Endpoint(endpoint)) => {
                                if let EndpointTransferType::Interrupt =
                                    endpoint.attributes.transfer_type()
                                {
                                    match endpoint.endpoint_address.in_endpoint() {
                                        true => {
                                            let Some(ref mut ctl) = driver_state.devctl else {
                                                unreachable!()
                                            };
                                            // SAFETY: The device was configured above.
                                            driver_state.in_endpoint = Arc::downgrade(&unsafe {
                                                ctl.setup_endpoint(endpoint).await
                                            }?)
                                        }
                                        false => {
                                            // SAFETY: The device was configured above.
                                            driver_state.out_endpoint = Arc::downgrade(&unsafe {
                                                devctl!().setup_endpoint(endpoint).await?
                                            });
                                        }
                                    }

                                    if driver_state.in_endpoint.upgrade().is_some()
                                        && driver_state.out_endpoint.upgrade().is_some()
                                    {
                                        break 'cfg;
                                    }
                                }
                            }

                            None => {
                                log::debug!("Unknown descriptor type: {:x?}", descriptor.id().0);
                                log::debug!("{:x?}", descriptor.as_raw());
                            }
                            _ => {} // Shouldn't happen, ignore
                        }
                    }
                    break 'cfg;
                }
            }
        }
        Ok(())
    }
}
