use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::Ordering;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use hid::fstreams::HidIndexFlags;
use hidreport::Report;
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

// todo add support for multiple interfaces
struct DriverState {
    devctl: Option<usb::UsbDevCtl>,

    // devctl owns these
    in_endpoint: Weak<EndpointQueue>,
    out_endpoint: Weak<EndpointQueue>,

    pipe: Arc<HidPipeInner>,

    minor_number: Option<usize>,
    hid_interfaces: Vec<Box<dyn hid_if::HidInterface>>,
}

impl DriverState {
    fn new() -> Self {
        Self {
            devctl: None,
            in_endpoint: Weak::new(),
            out_endpoint: Weak::new(),
            hid_interfaces: Vec::new(),
            pipe: Arc::new(HidPipeInner::new(0)), // We use null here but will change the ID later.
            minor_number: None,
        }
    }

    fn reload_pipe(&mut self) {
        let mut pipe = HidPipeInner::new(0);
        for i in &self.hid_interfaces {
            pipe.enable_interface(i.interface_number())
        }
        self.pipe = Arc::new(pipe);
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

        DevID::new(major, self.minor.load(Ordering::Relaxed))
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
        3
    }

    fn file_list(&self) -> Vec<String> {
        Vec::from([".".to_string(), "..".to_string(), "if0".to_string()])
    }

    fn get_file(&self, name: &str) -> Result<Box<dyn SysfsFile>, IoError> {
        match name {
            "." => Ok(Box::new(Clone::clone(self))),
            ".." => {
                let Some(ref t) =
                    hootux::task::util::block_on!(core::pin::pin!(self.inner.read())).devctl
                else {
                    unreachable!()
                };
                t.to_file().map(|e| Box::new(e) as Box<dyn SysfsFile>)
            }
            "if1" => {
                let pi = hootux::task::util::block_on!(core::pin::pin!(self.inner.read()))
                    .pipe
                    .clone();
                Ok(Box::new(pi.to_file(0)))
            }
            _ => Err(IoError::NotPresent),
        }
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
    pub(crate) async fn run(mut self) -> hootux::task::TaskResult {
        let Ok((input_size, output_size, _feature_size)) =
            self.configure_device().await.inspect_err(|e| {
                log::error!("Failed to initialize USB HID device {}: {:?}", self.id, e)
            })
        else {
            return hootux::task::TaskResult::Error;
        };
        log::info!("Initialized USB HID device");

        let mut l = self.inner.write().await;
        let Some(ref mut devctl) = l.devctl else {
            unreachable!()
        };
        let command = crate::requests::Request::SetProtocol {
            interface: 0,
            protocol: crate::requests::Protocol::Boot,
        };
        // This is a class specific request, so is always safe.
        unsafe { devctl.send_command(command.into(), None).await };
        drop(l);

        let rc = match self.mainloop(input_size, output_size).await {
            Ok(_) => hootux::task::TaskResult::ExitedNormally,
            Err(IoError::NotPresent) => hootux::task::TaskResult::StoppedExternally, // Device is no longer attached/present
            Err(e) => {
                log::error!("HID device stopped: Reason {e:?}");
                hootux::task::TaskResult::Error
            }
        }
    }

    async fn mainloop(&mut self, input_size: usize, _output_size: usize) -> Result<(), IoError> {
        let mut input_buffer: DmaBuff = {
            let mut t = Vec::new();
            t.resize(input_size, 0);
            t.into()
        };
        let Some(input_endpoint) = self.inner.read().await.in_endpoint.upgrade() else {
            return Err(IoError::NotPresent);
        };
        // todo: maybe add second buffer so we can alternate between them so one is pending while we parse the new data.
        loop {
            let (buff, len, status) = input_endpoint
                .new_string(input_buffer, usb::ehci::StringInterruptConfiguration::End)
                .await
                .complete();

            match status {
                Ok(_) => {}
                Err(()) => {
                    log::error!("Encountered error: Device halted");
                    return Err(IoError::MediaError);
                }
            }
            input_buffer = buff;

            // This must expect to errors, especially when Usage Pages aare not supported.
            // Errors should be handled downstream, but we still need to know about them.
            let _ = self
                .handle_input(&input_buffer[..len])
                .await
                .inspect_err(|e| log::trace!("Got error {e:?} while parsing report"));
        }
    }

    async fn configure_device(&self) -> Result<(usize, usize, usize), IoError> {
        let mut driver_state = self.inner.write().await;
        let mut report_sizes = None;

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
                                    crate::descriptors::HidDescriptorRequestType::Report,
                                    interface.interface_number,
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

                                let report_descriptor =
                                    match hidreport::ReportDescriptor::try_from(&buffer) {
                                        Ok(report) => report,
                                        Err(e) => {
                                            log::error!(
                                                "Failed to parse report descriptor: {e:?}\n{:?}",
                                                &buffer[..len]
                                            );
                                            return Err(IoError::MediaError);
                                        }
                                    };

                                driver_state.hid_interfaces =
                                    hid_if::resolve_interfaces(&report_descriptor)?;
                                let None = report_sizes
                                    .replace(Self::resolve_interface_sizes(&report_descriptor))
                                else {
                                    log::error!("Got multiple report descriptors.");
                                    return Err(IoError::AlreadyExists);
                                };
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

        driver_state.reload_pipe();
        let rs = report_sizes.ok_or(IoError::NotPresent);
        let minor_num = ID_DISPENSER.alloc_id().as_int().1;
        driver_state.minor_number = Some(minor_num);

        rs
    }

    /// Passes `data` to all interfaces until one successfully handles it.
    async fn handle_input(&self, data: &[u8]) -> Result<(), IoError> {
        let mut lr = self.inner.write().await;
        let pipe = lr.pipe.clone();

        for i in lr.hid_interfaces.iter_mut() {
            match i.parse_report(data, &pipe).await {
                Ok(_) => return Ok(()),
                Err(_) => {}
            }
        }
        log::trace!("No interface for report");
        Err(IoError::NotSupported)
    }

    /// Returns maximum size of (input, output, feature) descriptors.
    fn resolve_interface_sizes(report: &hidreport::ReportDescriptor) -> (usize, usize, usize) {
        let mut input = 0;
        for i in report.input_reports() {
            input = input.max(i.size_in_bytes());
        }
        let mut output = 0;
        for i in report.output_reports() {
            output = output.max(i.size_in_bytes());
        }
        let mut feature = 0;
        for i in report.feature_reports() {
            feature = feature.max(i.size_in_bytes());
        }
        (input, output, feature)
    }
}

/// The HID pipe is a character device that returns a single relevant character to indicate a HID device state change.
/// The packet formats are defined in [hid]. `pos` in [Read::read] and [Write::write] are a [hid::fstreams::HidIndexFlags].
///
/// This file can return different characters and so IOs to this file do not need to conform to
/// [File::block_size] like other character devices.
///
/// This file tracks which interfaces are supported by the HID pipe, if none of the requested
/// interfaces are supported by the HID device then the operation will return [IoError::NotSupported].
///
/// When an interface option is not recognised or pass in an invalid way then this will return [IoError::InvalidData].
/// When multiple interfaces are requested the first byte will indicate the interface that returned data.
/// Note that this byte will only be 0..31 because only these interfaces can be requested at the same time.
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
    pending: async_lock::Mutex<
        Vec<(
            DmaBuff,
            HidIndexFlags,
            hootux::task::util::MessageFuture<hootux::task::util::Sender, (DmaBuff, usize)>,
        )>,
    >,

    /// Counts the number of file objects that have opened this file for reading.
    open_read: core::sync::atomic::AtomicUsize,

    /// We queue all operations, operations must be given a serial number so each file can track
    /// which Op's need to be fetched.
    serial: core::sync::atomic::AtomicUsize,
    queued: spin::Mutex<alloc::collections::VecDeque<QueuedOp>>,

    /// Contains interfaces supported by this file.
    interfaces: alloc::collections::BTreeSet<u32>,

    // file metadata
    minor_num: usize,
}

struct QueuedOp {
    payload: Box<[u8]>,
    serial: usize,
    pending: core::sync::atomic::AtomicUsize,
    interface: u32,
}

impl HidPipeInner {
    const fn new(minor_num: usize) -> Self {
        Self {
            pending: async_lock::Mutex::new(Vec::new()),
            open_read: core::sync::atomic::AtomicUsize::new(0),
            serial: core::sync::atomic::AtomicUsize::new(0),
            queued: spin::Mutex::new(alloc::collections::VecDeque::new()),
            interfaces: alloc::collections::BTreeSet::new(),
            minor_num,
        }
    }

    /// Sends the character to all file objects. If the character cannot be written into all file
    /// objects immediately it will be queued until it is received by all open file objects.
    async fn send_op(&self, payload: &[u8], interface_index: u32) {
        let mut count = self.serial.load(Ordering::Relaxed);
        let mut queue = self.pending.lock().await;
        self.serial.fetch_add(1, Ordering::Relaxed);

        for (mut buffer, select, tx) in queue.extract_if(.., |e| e.1.is_interface(interface_index))
        {
            count -= 1;

            let mut tgt = &mut buffer[..];
            if select.is_multiple() {
                // prepend interface number for multiple interfaces
                tgt[0] = interface_index as u8;
                tgt = &mut tgt[1..];
            }
            let len = tgt.len().min(payload.len());
            tgt[0..len].copy_from_slice(&payload[..len]);
            tx.complete((buffer, len));
        }

        if count > 0 {
            self.pend_op(payload, interface_index, count)
        }
    }

    /// Send a character to the file, the character will wait until all file objects have read the character.
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

    /// Removes all queued messages that are no longer pending.
    fn clean_queue(&self) {
        let mut l = self.queued.lock();
        while let Some(op) = l.get(0) {
            if op.pending.load(Ordering::Relaxed) == 0 {
                l.pop_front();
            } else {
                break;
            }
        }
    }

    /// Decrements all queued messages beginning from `serial` until the end of the queue.
    ///
    /// This must be called by file objects when they are closed in order to clean the queued messages from the queue.
    fn close_from_serial(&self, serial: usize) {
        let mut pending = self.queued.lock();
        if self.open_read.fetch_sub(1, Ordering::Relaxed) == 1 {
            pending.clear();
            return;
        }
        if let Ok(start) = pending.binary_search_by(|e| e.serial.cmp(&serial)) {
            for i in pending.iter().skip(start) {
                i.pending.fetch_sub(1, Ordering::Relaxed);
            }
            drop(pending);
            self.clean_queue();
        }
    }

    /// Receives a character from the file queue. Only characters matching `interface` will be returned.
    /// On success returns the number of bytes written into `buffer`.
    ///
    /// This will always return the new serial of the file object.
    async fn receive_queued(
        &self,
        buffer: &mut DmaBuff,
        interface: HidIndexFlags,
        serial: usize,
    ) -> (Result<usize, ()>, usize) {
        let l = self.queued.lock();
        let mut dirty = false;
        let (Ok(start) | Err(start)) = l.binary_search_by(|e| serial.cmp(&e.serial));
        for i in l.iter().skip(start) {
            if i.pending.fetch_sub(1, Ordering::Relaxed) == 1 {
                dirty = true;
            }
            if interface.is_interface(i.interface) {
                let len = i.payload.len().min(buffer.len());
                buffer[..len].copy_from_slice(&i.payload[..len]);
                if dirty {
                    self.clean_queue();
                }
                return (Ok(len), i.serial);
            }
        }
        if dirty {
            self.close_from_serial(serial);
        }
        (Err(()), self.serial.load(Ordering::Relaxed))
    }

    fn enable_interface(&mut self, interface: u32) {
        self.interfaces.insert(interface);
    }

    fn to_file(self: &Arc<Self>, interface: u8) -> HidPipe {
        assert!(interface < 16);
        HidPipe {
            inner: Arc::downgrade(self),
            id: DevID::new(ID_DISPENSER.major(), self.minor_num),
            interface: interface as u64,
            open_mode: OpenMode::Locked,
            serial: core::sync::atomic::AtomicUsize::new(self.serial.load(Ordering::Relaxed)),
        }
    }
}

impl HidPipe {
    /// Receives normal data.
    /// Does not support options.
    async fn recv_data(
        &self,
        mut buffer: DmaBuff,
        interface: HidIndexFlags,
    ) -> Result<(DmaBuff, usize), (DmaBuff, IoError)> {
        // Only called from Self::read() which upgraded inner already.
        let Some(fa) = self.inner.upgrade() else {
            unreachable!()
        };
        let mut queue = fa.pending.lock().await;

        if self.serial.load(Ordering::Relaxed) != fa.serial.load(Ordering::Relaxed) {
            let (rc, serial) = fa
                .receive_queued(&mut buffer, interface, self.serial.load(Ordering::Relaxed))
                .await;
            self.serial.store(serial, Ordering::Relaxed);
            match rc {
                Ok(n) => return Ok((buffer, n)),
                Err(()) => {} // fall through
            }
        }

        let (tx, rx) = hootux::task::util::new_message();
        queue.push((buffer, interface, tx));

        // Explicit drop, prevents deadlock on rx.await
        drop(queue);

        Ok(rx.await)
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

impl SysfsFile for HidPipe {}

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
        async { Ok(hootux::mem::PAGE_SIZE as u64) }.boxed()
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
        if self.open_mode != OpenMode::Locked {
            let Some(fa) = self.inner.upgrade() else {
                return Err(IoError::NotPresent);
            };
            fa.close_from_serial(self.serial.load(Ordering::Relaxed));
        } else {
            return Err(IoError::Busy);
        }
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
        async move {
            // Prevents error where receiving data may panic when `pos` is multiple.
            // Char size is set to `8` anyway.
            if buff.len() <= 1 {
                log::debug!("Attempted to read HidPipe with short buffer");
                return Err((IoError::InvalidData, buff, 0));
            } else if !self.open_mode.is_read() {
                return Err((IoError::NotReady, buff, 0));
            };

            let hid_index = HidIndexFlags::from_bits_retain(pos);
            let Some(fa) = self.inner.upgrade() else {
                return Err((IoError::NotPresent, buff, 0));
            };

            // Perform sanity check to ensure that interfaces are actually supported.
            // Otherwise
            {
                if hid_index.contains(HidIndexFlags::NO_BITFIELD) {
                    let mut flags = hid_index.bits() as u32;
                    let mut no_if = true;

                    while flags != 0 {
                        let bit = flags.trailing_zeros();
                        // Clear bit
                        flags &= !(1 << bit);
                        if fa.interfaces.contains(&bit) {
                            no_if = false;
                        }
                    }
                    if no_if {
                        return Err((IoError::NotSupported, buff, 0));
                    }
                } else {
                    if !fa.interfaces.contains(&(hid_index.bits() as u32)) {
                        return Err((IoError::NotSupported, buff, 0));
                    }
                }
            }

            // We need to own self.inner to make sure its present.
            let Some(_inner) = self.inner.upgrade() else {
                return Err((IoError::NotPresent, buff, 0));
            };

            return match self.recv_data(buff, hid_index).await {
                Err((buff, e)) => Err((e, buff, 0)),
                Ok(r) => Ok(r),
            };
        }
        .boxed()
    }
}

impl Write<u8> for HidPipe {
    fn write(
        &self,
        _pos: u64,
        buff: DmaBuff,
    ) -> BoxFuture<'_, Result<(DmaBuff, usize), (IoError, DmaBuff, usize)>> {
        async { Err((IoError::NotSupported, buff, 0)) }.boxed()
    }
}

impl Drop for HidPipe {
    fn drop(&mut self) {
        // Must clean all queued retained for self in self.inner
        if self.open_mode != OpenMode::Locked {
            let _ = self.close();
        }
    }
}
