use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::Ordering;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use hootux::fs::device::{Fifo, OpenMode};
use hootux::fs::file::*;
use hootux::fs::sysfs::{SysfsDirectory, SysfsFile};
use hootux::fs::vfs::DeviceIdDistributer;
use hootux::fs::{IoError, IoResult};
use hootux::mem::dma::DmaBuff;
use usb::ehci::EndpointQueue;
use usb_cfg::descriptor::{Descriptor, DeviceDescriptor, EndpointTransferType};

mod hid_if;

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
            for (i, mut interface_group) in unsafe {
                descriptor
                    .iter()
                    .fold_on(usb_cfg::DescriptorType::Interface)
                    .enumerate()
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

                            None if descriptor.id()
                                == crate::descriptors::HidDescriptor::descriptor_type() =>
                            {
                                let Some(hid_descriptor) =
                                    descriptor.cast::<crate::descriptors::HidDescriptor>()
                                else {
                                    unreachable!()
                                };

                                let Some(report_descriptor) =
                                    hid_descriptor.optionals().iter().find(|o| {
                                        o.id == crate::descriptors::HidDescriptorRequestType::Report
                                            .discriminant()
                                    })
                                else {
                                    unreachable!() // The report descriptor entry is guaranteed to be contained in the HID descriptor
                                };

                                let Some(ref devctl) = driver_state.devctl else {
                                    unreachable!()
                                };

                                let command = crate::descriptors::request_descriptor_command(
                                    crate::descriptors::HidDescriptorRequestType::Hid,
                                    i as u8,
                                    report_descriptor.length,
                                );
                                let buffer = vec![0u8; report_descriptor.length as usize];
                                // SAFETY: The request is targeted at the interface and is not defined in the base specification.
                                let rc =
                                    unsafe { devctl.send_command(command, Some(buffer.into())) }
                                        .await;

                                // Buffer is always Some(_)
                                let (Some(buffer), len) = (rc.0, rc.1?) else {
                                    unreachable!()
                                };

                                let mut buffer: Vec<u8> = buffer.try_into().unwrap();
                                if buffer.len() < len {
                                    log::warn!("Received buffer was smaller than expected");
                                    buffer.truncate(len);
                                }
                                todo!()
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

#[file]
struct HidPipe {
    inner: Weak<HidPipeInner>,
    id: DevID,
    interface: u64,
    open_mode: OpenMode,
    serial: core::sync::atomic::AtomicUsize,
}

struct HidPipeInner {
    /// File objects may only pend operations when it's serial and `self.serial` is equal.
    pending: async_lock::Mutex<Vec<(DmaBuff, hid::fstreams::HidIndexFlags, core::task::Waker)>>,
    driver_state: Arc<DriverState>,

    /// Counts the number of file objects that have opened this file for reading.
    open_read: core::sync::atomic::AtomicUsize,

    /// We queue all operations, operations must be given a serial number so each file can track
    /// which Op's need to be fetched.
    serial: core::sync::atomic::AtomicUsize,
    queued: spin::Mutex<alloc::collections::VecDeque<QueuedOp>>,
}

struct QueuedOp {
    payload: Box<[u8]>,
    serial: usize,
    pending: core::sync::atomic::AtomicUsize,
    interface: u32,
}

impl HidPipeInner {
    async fn send_op(&self, payload: &[u8], interface_index: u32) {
        let mut count = self.serial.load(Ordering::Relaxed);
        let mut queue = self.pending.lock().await;
        self.serial.fetch_add(1, Ordering::Relaxed);

        for (buffer, select, waker) in queue.extract_if(.., |e| e.1.is_interface(interface_index)) {
            count -= 1;

            let mut tgt = &mut buffer[..];
            if select.is_multiple() {
                // prepend interface number for multiple interfaces
                tgt[0] = interface_index as u8;
                tgt = &mut tgt[1..];
            }
            let len = tgt.len().min(payload.len());
            tgt[0..len].copy_from_slice(&payload[..len]);
            waker.wake()
        }

        if count > 0 {
            self.pend_op(payload, interface_index, count)
        }
    }

    fn pend_op(&self, payload: &[u8], interface: u32, count: usize) {
        let mut l = self.queued.lock();
        let qd = QueuedOp {
            payload: Box::from(payload),
            serial: self.serial.load(Ordering::Relaxed),
            pending: core::sync::atomic::AtomicUsize::new(count),
            interface,
        };
        l.push_back(qd)
    }
}

impl Clone for HidPipe {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            id: self.id,
            interface: self.interface,
            open_mode: OpenMode::Locked,
            serial: core::sync::atomic::AtomicUsize::new(0), // set when open() is called
        }
    }
}

impl File for HidPipe {
    fn file_type(&self) -> FileType {
        FileType::CharDev
    }

    fn block_size(&self) -> u64 {
        8
    }

    fn device(&self) -> DevID {
        self.id
    }

    fn clone_file(&self) -> Box<dyn File> {
        Box::new(self.clone())
    }

    fn id(&self) -> u64 {
        self.interface << 8
    }

    fn len(&self) -> IoResult<'_, u64> {
        async { hootux::mem::PAGE_SIZE }.boxed()
    }
}

impl Fifo<u8> for HidPipe {
    fn open(&mut self, mode: OpenMode) -> Result<(), IoError> {
        let Some(fa) = self.inner.upgrade() else {
            return Err(IoError::NotPresent);
        };
        if self.open_mode == OpenMode::Locked {
            // Self can be opened an unlimited number of times.
            self.open_mode = mode;
            if mode.is_read() {
                fa.open_read.fetch_add(1, Ordering::Acquire);
                self.serial
                    .store(fa.serial.load(Ordering::Relaxed), Ordering::Release);
            }

            Ok(())
        } else {
            Err(IoError::Busy)
        }
    }

    fn close(&mut self) -> Result<(), IoError> {
        self.open_mode = OpenMode::Locked;
        Ok(())
    }

    fn locks_remain(&self, _: OpenMode) -> usize {
        usize::MAX
    }

    fn is_master(&self) -> Option<usize> {
        None
    }
}

impl Read<u8> for HidPipe {
    fn read(
        &self,
        pos: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async {
            if buff.len() <= 1 {
                log::debug!("Attempted to read HidPipe with short buffer");
                return Err((IoError::EndOfFile, buff, 0));
            }
            todo!()
        }
    }
}

impl Write<u8> for HidPipe {
    fn write(
        &self,
        pos: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        todo!()
    }
}
