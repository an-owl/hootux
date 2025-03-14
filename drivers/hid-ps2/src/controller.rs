use core::sync::atomic::Ordering;
use core::task::Poll;
use futures_util::FutureExt;
use hootux::task::util::sleep;
use hootux::time::{AbsoluteTime, Duration};

const TIMEOUT_DURATION_MS: u64 = 100;
const PORT_ONE_ISA_IRQ: u8 = 0x01;
const PORT_TWO_ISA_IRQ: u8 = 0x12;

/// Defines the number of retries until we determine the device will not respond correctly.
const RETRIES: usize = 3;

pub(crate) struct Controller {
    ctl_raw: hid_8042::controller::PS2Controller,
    multi_port: bool,
    port_1: alloc::sync::Arc<PortComplex>,
    port_2: alloc::sync::Arc<PortComplex>,
    driver_major: hootux::fs::vfs::MajorNum, // We keep just the major num this is basically the driver root. the portnum is the minor num
}

enum CommandError {
    /// Indicates the controller has timed out. No further commands can be sent.
    ///
    /// The caller can assume when this is returned that the controller is faulty.
    ControllerTimeout,

    /// Indicates that the device attached to port one has timed out.
    ///
    /// The caller can assume the device is faulty or has been unplugged.
    PortTimeout,

    /// Attempted to access port 2 when it is not present.
    SinglePortDevice,
}

macro_rules! interrupt_init_body {
    ($self:ident,$port:ident, $isa_irq:expr, $fail:literal) => {{
        let this = alloc::sync::Arc::downgrade(&$self.$port);

        let int_handler = move |_, _| {
            // None indicates a driver bug, The interrupt should've been disabled.
            // This will just continue to handle the interrupts without doing anything.
            let Some(port) = this.upgrade() else {
                unsafe { hootux::interrupts::eoi() }
                return;
            };

            let ctl_raw = hid_8042::controller::PS2Controller;

            let mut in_buff = [0; 8];
            let mut in_len = 0;

            // SAFETY: Interrupt indicates that the register is full, first read is safe
            in_buff[0] = unsafe { ctl_raw.read_unchecked() };
            for i in in_buff.iter_mut().skip(1) {
                match ctl_raw.read_data() {
                    Some(d) => *i = d,
                    None => break,
                }
                in_len += 1;
            }

            port.new_data(&in_buff[..in_len]);

            unsafe { hootux::interrupts::eoi() } // declare EOI
        };

        let Some(mut irq) = hootux::interrupts::alloc_interrupt() else {
            log::warn!($fail);
            return;
        };

        // SAFETY: Trigger mode is "edge" and EOI is called.
        unsafe { irq.set_high_perf(alloc::boxed::Box::new(int_handler)) }

        let (mut gsi, io) = irq.get_isa($isa_irq);
        if let None = io {
            gsi.polarity = hootux::interrupts::apic::ioapic::PinPolarity::AssertHigh;
            gsi.trigger_mode = hootux::interrupts::apic::ioapic::TriggerMode::EdgeTriggered;
        }
        gsi.mask = false;
        gsi.delivery_mode = hootux::interrupts::apic::ioapic::DeliveryMode::Fixed;
        gsi.target = hootux::interrupts::apic::ioapic::Target::Logical(0);

        // SAFETY: Interrupt handler is set above.
        unsafe {
            gsi.set().expect("???");
        }
    }};
}
macro_rules! init_port {
    ($this:ident,$port_name:ident,$port_command:ident,$port_lit:literal) => {{
        use hid_8042::keyboard::*;

        log::info!("Initializing {port}", port = $port_lit);

        for r in 0..RETRIES {
            match $this.$port_command(Command::ResetAndBIST).await {
                Ok([const { Response::Resend as u8 }, ..]) if r == RETRIES - 1 => {
                    log::warn!(
                        "Unable to initialize {port}: exceeded retries",
                        port = $port_lit
                    );
                    $this.$port_name.set_state(PortState::Nuisance);
                    return;
                }
                Ok([const { Response::Resend as u8 }, ..]) => continue,
                Ok(
                    [
                        const { Response::Ack as u8 },
                        const { Response::BISTSuccess as u8 },
                        ..,
                    ],
                ) => break,
                Ok(
                    [
                        const { Response::Ack as u8 },
                        const { Response::BISTFailure as u8 },
                        ..,
                    ],
                ) => {
                    log::error!("Device: BIST failed");
                    return;
                }
                Ok(_) => {
                    log::error!("Unknown BIST test response");
                    if r == 2 {
                        $this.$port_name.set_state(PortState::Nuisance);
                        return;
                    }
                }
                Err(CommandError::PortTimeout) => {
                    $this.$port_name.set_state(PortState::Down);
                    return; // no device, dont retry
                }
                // SAFETY: Port 1 commands cannot return controller timeout or SinglePortDevice
                Err(CommandError::SinglePortDevice) => {
                    log::error!("Attempted to initialize port 2 when not present");
                    return;
                },
                Err(CommandError::ControllerTimeout) => {
                    controller_timeout_handler();
                    return;
                }
            }
        }

        log::trace!("test success");

        for r in 0..RETRIES {
            match $this.$port_command(Command::IdentifyDevice).await {
                Ok([const { Response::Resend as u8 }, ..]) if r == RETRIES - 1 => {
                    log::warn!("Unable to identify device: exceeded retries");
                    $this.$port_name.set_state(PortState::Nuisance);
                    return;
                }
                Ok([const { Response::Resend as u8 }, ..]) => continue,
                Ok([const { Response::Ack as u8 }, content @ .., _]) if r == 1 => {
                    match hid_8042::DeviceType::try_from(content) {
                        Ok(device_type) => {
                            $this.$port_name.set_state(PortState::Device(device_type));
                            break;
                        }
                        Err(_) => {
                            log::warn!("Unable to identify device: invalid type");
                            $this.$port_name.set_state(PortState::Device(hid_8042::DeviceType::Unknown));
                            return;
                        }
                    }
                }
                Ok(_) => {
                    log::error!("Unknown bad response");
                    $this.$port_name.set_state(PortState::Nuisance);
                }
                Err(CommandError::PortTimeout) => {
                    log::error!("Timeout fetching device type");
                    $this.$port_name.set_state(PortState::Down);
                    return;
                }

                Err(CommandError::ControllerTimeout) => {
                    controller_timeout_handler()
                }
                Err(CommandError::SinglePortDevice) => {
                    // SAFETY: We cannot hit this because it will be returned above when reset and BIST is sent.
                    unsafe { core::hint::unreachable_unchecked() }
                }
            }
        }

        // set scancode set
        match $this
            .$port_command(Command::ScanCodeSet(
                ScanCodeSetSubcommand::SetScancodeSetTwo,
            ))
            .await
        {
            Ok([const { Response::Ack as u8 }, ..]) => {
                $this.$port_name.set_meta(DevMeta::ScanCodeSetTwo);
            }

            Ok([const { Response::Resend as u8 }, ..]) => {
                match $this
                    .$port_command(Command::ScanCodeSet(
                        ScanCodeSetSubcommand::SetScancodeSetOne,
                    ))
                    .await
                {
                    Ok([const { Response::Ack as u8 }, ..]) => {
                        $this.$port_name.set_meta(DevMeta::ScanCodeSetOne);
                    }

                    Err(CommandError::PortTimeout) => {
                        $this.$port_name.set_state(PortState::Down);
                        return;
                    }

                    _ => $this.$port_name.set_state(PortState::Nuisance),
                }
            }
            Err(CommandError::PortTimeout) => {
                $this.$port_name.set_state(PortState::Down);
                return;
            }
            Err(CommandError::SinglePortDevice) => {
                core::unreachable!()
            }
            _ => {
                log::warn!("Bad response when querying device type");
                $this.$port_name.set_state(PortState::Nuisance);
            },
        }
    }};
}

impl Controller {
    pub fn new() -> Self {
        Self {
            ctl_raw: hid_8042::controller::PS2Controller,
            multi_port: true,
            port_1: alloc::sync::Arc::new(PortComplex::new()),
            port_2: alloc::sync::Arc::new(PortComplex::new()),
            driver_major: hootux::fs::vfs::MajorNum::new(),
        }
    }
    /// Initializes the controller.
    pub async fn init(mut self) -> Result<Self, ()> {
        #[cfg(debug_assertions)]
        static INITIALIZED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);

        #[cfg(debug_assertions)]
        if INITIALIZED.load(Ordering::Relaxed) {
            panic!("Attempted to initialize i8042 twice")
        } else {
            INITIALIZED.store(true, Ordering::Relaxed);
        }

        log::info!("Initializing controller...");

        use hid_8042::controller::*;
        // disable devices

        self.set_operation(false);
        // clear buffer
        while let Some(_) = self.ctl_raw.read_data() {}

        self.ctl_raw
            .send_command(Command::ReadByte(0.try_into().unwrap()));

        let mut cfg_byte = ConfigurationByte::from_bits_retain(loop {
            match self.ctl_raw.read_data() {
                Some(r) => break r,
                None => {}
            }
        });

        // Disable translation.
        cfg_byte.set(ConfigurationByte::PORT_TRANSLATION, false);
        // Enable interrupts.
        cfg_byte.set(
            ConfigurationByte::PORT_ONE_INT | ConfigurationByte::PORT_TWO_INT,
            true,
        );

        self.ctl_raw
            .send_command(Command::WriteByte(0.try_into().unwrap(), cfg_byte.bits()));
        self.ctl_raw.send_command(Command::TestController);

        // Perform controller self test.
        match self
            .read_data_with_timeout(Duration::millis(TIMEOUT_DURATION_MS))
            .await
        {
            Some(0x55) => {}
            Some(e) => {
                log::error!("8042 Initialization failed: BIST returned {e:#x}");
                return Err(());
            }
            None => {
                log::error!("8042 Initialization failed: BIST timed out");
                return Err(());
            }
        }

        // check if self is multi-port
        self.ctl_raw.send_command(Command::EnablePort2);
        self.ctl_raw
            .send_command(Command::ReadByte(0u8.try_into().unwrap()));
        let t = ConfigurationByte::from_bits_retain(
            self.read_data_with_timeout(Duration::millis(TIMEOUT_DURATION_MS))
                .await
                .ok_or(())?,
        );
        self.multi_port = !t.contains(ConfigurationByte::PORT_TWO_CLOCK_DISABLED);
        if self.multi_port {
            self.ctl_raw.send_command(Command::DisablePort2);
        }

        if self.multi_port {
            log::info!("8042 is mult-port")
        } else {
            log::info!("8042 is not mult-port");
        }

        // test ports
        {
            self.ctl_raw.send_command(Command::TestPort1);
            let r = PortTestResult::from_bits_retain(
                self.read_data_with_timeout(Duration::millis(TIMEOUT_DURATION_MS))
                    .await
                    .ok_or(())?,
            );
            if !r.is_good() {
                log::warn!("8042 Port one test failure: {r:?}"); // I think self can indicate that nothing is connected.
            }
            self.ctl_raw.send_command(Command::TestPort1);
            let r = PortTestResult::from_bits_retain(
                self.read_data_with_timeout(Duration::millis(TIMEOUT_DURATION_MS))
                    .await
                    .ok_or(())?,
            );
            if !r.is_good() {
                log::warn!("8042 Port two test failure: {r:?}")
            }
        }

        // we are done initializing the controller, enable ports.
        self.ctl_raw.send_command(Command::EnablePort1);
        self.ctl_raw.send_command(Command::EnablePort2);

        log::trace!("initialization completed");

        Ok(self)
    }

    /// For receiving data after sending a command to the controller.
    async fn read_data_with_timeout(&self, timeout: Duration) -> Option<u8> {
        let r: AbsoluteTime = timeout.into();
        while !r.is_future() {
            match self.ctl_raw.read_data() {
                Some(d) => return Some(d),
                None => hootux::suspend!(),
            }
        }
        None
    }

    /// Sends a command to the device on port 1.
    ///
    /// Returns the raw command response as a null terminated `[u8;4]`.
    async fn port_1_command(
        &mut self,
        cmd: hid_8042::keyboard::Command,
    ) -> Result<[u8; 4], CommandError> {
        for payload in cmd.raw_iter() {
            let timeout: AbsoluteTime = Duration::millis(TIMEOUT_DURATION_MS).into();

            while self.ctl_raw.send_data(payload).is_err() {
                if timeout.is_future() {
                    return Err(CommandError::ControllerTimeout);
                }
                hootux::suspend!()
            }
        }

        let timeout = sleep(TIMEOUT_DURATION_MS);
        let cmd_ret = self.port_1.cmd_wait();
        let ret = futures_util::select_biased! {
            _ = timeout.fuse() => return Err(CommandError::PortTimeout),
            ret = cmd_ret.fuse() => ret,
        };

        Ok(ret)
    }

    async fn port_2_command(
        &mut self,
        cmd: hid_8042::keyboard::Command,
    ) -> Result<[u8; 4], CommandError> {
        const WRITE_PORT_2_CMD: u8 = 0xd3;

        if !self.is_multi_port() {
            return Err(CommandError::SinglePortDevice);
        }

        for payload in cmd.raw_iter() {
            self.ctl_raw
                .send_command(hid_8042::controller::RawCommand::new(
                    WRITE_PORT_2_CMD,
                    None,
                ));

            // busy-wait until timeout expires, then throw error
            let timeout: AbsoluteTime = Duration::millis(TIMEOUT_DURATION_MS).into();
            // This doesn't send data unless it returns Ok(())
            while self.ctl_raw.send_data(payload).is_err() {
                if timeout.is_future() {
                    return Err(CommandError::PortTimeout);
                }
                hootux::suspend!()
            }
        }

        let timeout = sleep(TIMEOUT_DURATION_MS);
        let cmd_ret = self.port_1.cmd_wait();
        let ret = futures_util::select_biased! {
            _ = timeout.fuse() => return Err(CommandError::PortTimeout),
            ret = cmd_ret.fuse() => ret,
        };

        Ok(ret)
    }

    pub(crate) async fn init_port1(&mut self) {
        init_port!(self, port_1, port_1_command, "port 1");
    }

    pub(crate) async fn init_port2(&mut self) {
        init_port!(self, port_2, port_2_command, "port 2")
    }

    pub(crate) async fn cfg_interrupt_port1(&mut self) {
        interrupt_init_body!(
            self,
            port_1,
            PORT_ONE_ISA_IRQ,
            "Failed to allocate interrupt for 8042 port-. Aborting"
        )
    }

    pub(crate) async fn cfg_interrupt_port2(&mut self) {
        interrupt_init_body!(
            self,
            port_2,
            PORT_TWO_ISA_IRQ,
            "Failed to allocate interrupt for 8042 port-2. Aborting"
        )
    }

    /// Disables both ports
    fn set_operation(&self, enabled: bool) {
        if enabled {
            self.ctl_raw
                .send_command(hid_8042::controller::Command::DisablePort1);
            if self.multi_port {
                self.ctl_raw
                    .send_command(hid_8042::controller::Command::DisablePort2);
            }
        } else {
            self.ctl_raw
                .send_command(hid_8042::controller::Command::EnablePort1);
            if self.multi_port {
                self.ctl_raw
                    .send_command(hid_8042::controller::Command::EnablePort2);
            }
        }
    }

    pub(crate) fn major(&self) -> hootux::fs::vfs::MajorNum {
        self.driver_major
    }

    pub(crate) fn is_multi_port(&self) -> bool {
        self.multi_port
    }
}

struct PortComplex {
    port_status: atomic::Atomic<PortState>,
    dev_meta: atomic::Atomic<DevMeta>,
    open: core::sync::atomic::AtomicBool,

    ack_buffer: core::cell::RefCell<[u8; 4]>,
    cmd_ack: futures_util::task::AtomicWaker,
    // The *mut [u8] must be constructed from DmaTarget::as_mut()
    out_buff: spin::Mutex<Option<alloc::sync::Arc<(*mut [u8], core::sync::atomic::AtomicUsize)>>>,
    o_waker: futures_util::task::AtomicWaker,
}

// SAFETY: This is required because of the *mut[u8] in `self.tgt`, which is behind a mutex.
unsafe impl Send for PortComplex {}
unsafe impl Sync for PortComplex {}

impl PortComplex {
    fn new() -> PortComplex {
        Self {
            port_status: atomic::Atomic::new(PortState::Down),
            dev_meta: atomic::Atomic::new(DevMeta::ScanCodeSetOne),
            open: core::sync::atomic::AtomicBool::new(false),
            ack_buffer: core::cell::RefCell::new([0; 4]),
            cmd_ack: futures_util::task::AtomicWaker::new(),
            out_buff: spin::Mutex::new(None),
            o_waker: futures_util::task::AtomicWaker::new(),
        }
    }

    fn set_state(&self, state: PortState) {
        log::trace!("set device to {:?}", state);
        self.port_status.store(state, Ordering::Relaxed);
    }

    fn set_meta(&self, meta: DevMeta) {
        self.dev_meta.store(meta, Ordering::Relaxed);
    }

    fn meta(&self) -> DevMeta {
        self.dev_meta.load(Ordering::Relaxed)
    }

    /// Clear command buffer
    fn cmd_clear(&self) {
        self.ack_buffer.borrow_mut().fill(0);
    }

    /// Wait for command response.
    fn cmd_wait(&self) -> impl Future<Output = [u8; 4]> {
        CmdFut { port: self }
    }

    /// Indicate to the handler that new data has become available.
    ///
    /// This fn will determine whether `data` is a command response or a scancode.
    ///
    /// - If `data` is a command response, then the contents will be cached and `self.cmd_ack` will be woken.
    /// - If `data` is a scancode then the code will be forwarded to any buffers currently waiting for them.
    fn new_data(&self, data: &[u8]) {
        if let Ok(_) = TryInto::<hid_8042::keyboard::Response>::try_into(data[0]) {
            self.ack(&data[..4]);
        } else {
            let mut l = self.out_buff.lock();
            if let Some(tgt) = l.take() {
                // Drop lock ASAP
                drop(l);
                // SAFETY: DmaTarget pointer is provided by a DmaTarget which data may be cast to reference as long as it is not mutably aliased.
                let dma_tgt = unsafe { &mut *tgt.0 };
                dma_tgt[..data.len()].copy_from_slice(data);
                tgt.1.fetch_add(data.len(), Ordering::Relaxed);
                self.o_waker.wake();
            }
        }
    }

    /// Called by [Self::new_data] when command response data has been located.
    fn ack(&self, buff: &[u8]) {
        assert_eq!(buff.len(), 4);
        let mut b = self.ack_buffer.borrow_mut();
        b.fill(0);
        b.copy_from_slice(buff);
        self.cmd_ack.wake()
    }

    pub fn state(&self) -> PortState {
        self.port_status.load(Ordering::Relaxed)
    }
}

struct CmdFut<'a> {
    port: &'a PortComplex,
}

impl<'a> Future for CmdFut<'a> {
    type Output = [u8; 4];
    fn poll(
        self: core::pin::Pin<&mut Self>,
        cx: &mut core::task::Context<'_>,
    ) -> Poll<Self::Output> {
        x86_64::instructions::interrupts::without_interrupts(|| {
            let b = self.port.ack_buffer.borrow();
            if b[0] != 0 {
                let buff = b.clone();
                drop(b);
                self.port.cmd_clear();
                Poll::Ready(buff)
            } else {
                self.port.cmd_ack.register(cx.waker());
                Poll::Pending
            }
        })
    }
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum PortState {
    /// Link state is down.
    Down,
    /// A device is detected but is acting improperly. Don't talk to it, pretend it isn't there
    Nuisance,
    /// Device is detected and working correctly.
    Device(hid_8042::DeviceType),
}

/// Contains device operation metadata.
#[non_exhaustive]
#[derive(Copy, Clone, Debug)]
pub enum DevMeta {
    ScanCodeSetOne,
    ScanCodeSetTwo,
    #[allow(dead_code)]
    ScanCodeSetThree,
}

fn controller_timeout_handler() {
    log::error!("Controller timed out: driver should be disabled.");
    // fixme Remove panic and replace with a driver-kill
    panic!("8042 controller timed out");
}

pub(crate) mod file {
    use crate::controller::Controller;
    use alloc::boxed::Box;
    use core::any::Any;
    use core::fmt::{Arguments, Formatter, Write as _};
    use core::pin::{Pin, pin};
    use core::task::{Context, Poll};
    use futures_util::FutureExt;
    use futures_util::future::BoxFuture;
    use hootux::WriteableBuffer;
    use hootux::fs::sysfs::SysfsFile;
    use hootux::fs::vfs::MajorNum;
    use hootux::fs::{IoError, IoResult, device::*, file::*};
    use hootux::mem::dma::{DmaBuff, DmaTarget};

    #[derive(Debug, Copy, Clone)]
    pub enum PortNum {
        One,
        Two,
    }

    impl core::fmt::Display for PortNum {
        fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
            match self {
                PortNum::One => write!(f, "1"),
                PortNum::Two => write!(f, "2"),
            }
        }
    }

    #[file]
    pub struct PortFileObject {
        port: PortNum,
        ctl: alloc::sync::Arc<async_lock::Mutex<super::Controller>>,
        dev: DevID,
        mode: OpenMode,
    }

    impl PortFileObject {
        pub(crate) fn new(
            controller: alloc::sync::Arc<async_lock::Mutex<Controller>>,
            port: PortNum,
            major_num: MajorNum,
        ) -> Self {
            let id = DevID::new(major_num, match port {
                PortNum::One => 1,
                PortNum::Two => 2,
            });

            Self {
                port,
                ctl: controller,
                dev: id,
                mode: OpenMode::Locked,
            }
        }
    }

    impl Clone for PortFileObject {
        fn clone(&self) -> Self {
            Self {
                port: self.port,
                ctl: self.ctl.clone(),
                dev: self.dev.clone(),
                mode: OpenMode::Locked,
            }
        }
    }

    impl File for PortFileObject {
        fn file_type(&self) -> FileType {
            FileType::CharDev
        }

        fn block_size(&self) -> u64 {
            8
        }

        fn device(&self) -> DevID {
            self.dev
        }

        fn clone_file(&self) -> Box<dyn File> {
            Box::new(self.clone())
        }

        fn id(&self) -> u64 {
            match self.port {
                PortNum::One => 1,
                PortNum::Two => 2,
            }
        }

        fn len(&self) -> IoResult<u64> {
            async { Ok(8) }.boxed()
        }
    }

    impl DeviceFile for PortFileObject {}
    impl SysfsFile for PortFileObject {}

    impl Fifo<u8> for PortFileObject {
        fn open(&mut self, mode: OpenMode) -> Result<(), IoError> {
            let t = async {
                let mut ctl = self.ctl.lock().await;
                let port = match self.port {
                    PortNum::One => &mut ctl.port_1,
                    PortNum::Two => &mut ctl.port_2,
                };

                if let Ok(_) = &mut port.open.compare_exchange(
                    true,
                    false,
                    atomic::Ordering::Acquire,
                    atomic::Ordering::Relaxed,
                ) {
                    return Err(IoError::Busy);
                }

                self.mode = mode;
                Ok(())
            };

            // todo, this shouldn't block
            hootux::block_on!(pin![t])
        }

        fn close(&mut self) -> Result<(), IoError> {
            if self.mode == OpenMode::Locked {
                return Err(IoError::DeviceError);
            };
            let mut ctl = hootux::block_on!(self.ctl.lock().boxed());
            let port = match self.port {
                PortNum::One => &mut ctl.port_1,
                PortNum::Two => &mut ctl.port_2,
            };
            port.open.store(false, atomic::Ordering::Release);

            self.mode = OpenMode::Locked;
            Ok(())
        }

        fn locks_remain(&self, _mode: OpenMode) -> usize {
            let mut ctl = hootux::block_on!(pin![self.ctl.lock()]);
            let port = match self.port {
                PortNum::One => &mut ctl.port_1,
                PortNum::Two => &mut ctl.port_2,
            };

            if port.open.load(atomic::Ordering::Relaxed) == false {
                1
            } else {
                0
            }
        }

        fn is_master(&self) -> Option<usize> {
            None
        }
    }

    impl hootux::fs::file::Write<u8> for PortFileObject {
        fn write<'f, 'a: 'f, 'b: 'f>(
            &'a self,
            _pos: u64,
            buff: DmaBuff<'b>,
        ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
            async { Err((IoError::NotSupported, buff, 0)) }.boxed()
        }
    }

    impl hootux::fs::file::Read<u8> for PortFileObject {
        fn read<'f, 'a: 'f, 'b: 'f>(
            &'a self,
            pos: u64,
            mut buff: DmaBuff<'b>,
        ) -> BoxFuture<'f, Result<(DmaBuff<'b>, usize), (IoError, DmaBuff<'b>, usize)>> {
            async move {
                if !self.mode.is_read() {
                    return Err((IoError::DeviceError, buff, 0));
                }

                match pos {
                    0 => {
                        let mut xfer_buff: [u8; 16] = [0; 16];
                        let mut xfer_buff_writable =
                            hootux::ToWritableBuffer::writable(&mut xfer_buff[..]);
                        let lock = self.ctl.lock().await;
                        let meta = match self.port {
                            PortNum::One => lock.port_1.meta(),
                            PortNum::Two => lock.port_2.meta(),
                        };

                        let (mut b, len) = IoFuture::new(lock, buff, self.port).await;
                        let b_ref = unsafe { &mut *DmaTarget::as_mut(&mut *b) };

                        let mut decoder = pc_keyboard::EventDecoder::new(
                            pc_keyboard::layouts::Us104Key,
                            pc_keyboard::HandleControl::Ignore,
                        );
                        let mut set1 = pc_keyboard::ScancodeSet1::new();
                        let mut set2 = pc_keyboard::ScancodeSet2::new();

                        let use_set: &mut dyn pc_keyboard::ScancodeSet = match meta {
                            super::DevMeta::ScanCodeSetOne => &mut set1,
                            super::DevMeta::ScanCodeSetTwo => &mut set2,
                            _ => return Err((IoError::MediaError, b, len)),
                        };

                        for i in &b_ref[0..len] {
                            match use_set.advance_state(*i) {
                                Ok(None) => continue,
                                Ok(Some(e)) => {
                                    let Some(pc_keyboard::DecodedKey::Unicode(ch)) =
                                        decoder.process_keyevent(e)
                                    else {
                                        continue;
                                    };
                                    if xfer_buff_writable.write_char(ch).is_err() {
                                        break;
                                    };
                                }
                                Err(e) => {
                                    log::warn!("failed to decode scancode {e:?}");
                                    return Err((IoError::MediaError, b, len));
                                }
                            }
                        }

                        let xfer_len = xfer_buff_writable.cursor();
                        drop(xfer_buff_writable);
                        b_ref[..xfer_len].copy_from_slice(&xfer_buff[..xfer_len]);

                        return Ok((b, xfer_len));
                    }

                    // Raw output
                    1 => {
                        let r = IoFuture::new(self.ctl.lock().await, buff, self.port).await;
                        Ok(r)
                    }
                    2 => {
                        let ctl = self.ctl.lock().await;
                        let t = DmaTarget::as_mut(&mut *buff);
                        let mut b = unsafe { hootux::ToWritableBuffer::writable(&mut *t) };

                        let port = match self.port {
                            PortNum::One => &ctl.port_1,
                            PortNum::Two => &ctl.port_2,
                        };

                        match write!(b, "{:?}", port.port_status) {
                            Ok(()) => Ok((buff, b.cursor())),
                            Err(_) => Err((IoError::EndOfFile, buff, b.cursor())),
                        }
                    }
                    _ => Err((IoError::EndOfFile, buff, 0)),
                }
            }
            .boxed()
        }
    }

    struct IoFuture<'a, 'b> {
        ctl: Option<async_lock::MutexGuard<'a, super::Controller>>,
        buff: Option<Box<dyn DmaTarget + 'b>>,
        tgt: alloc::sync::Arc<(*mut [u8], core::sync::atomic::AtomicUsize)>,
        port_num: PortNum,
    }

    // SAFETY: Required because IoFuture contains *mut [u8]
    // The pointer points to data which IoFuture contains exclusive access to.
    unsafe impl Sync for IoFuture<'_, '_> {}
    unsafe impl Send for IoFuture<'_, '_> {}

    impl<'a, 'b> IoFuture<'a, 'b> {
        fn new(
            ctl: async_lock::MutexGuard<'a, super::Controller>,
            mut buff: Box<dyn DmaTarget + 'b>,
            port_num: PortNum,
        ) -> Self {
            let tgt = alloc::sync::Arc::new((
                DmaTarget::as_mut(&mut *buff),
                core::sync::atomic::AtomicUsize::new(0),
            ));

            Self {
                ctl: Some(ctl),
                buff: Some(buff),
                tgt,
                port_num,
            }
        }
    }
    impl<'a, 'b> Future for IoFuture<'a, 'b> {
        type Output = (Box<dyn DmaTarget + 'b>, usize);

        /// Inserts `self.tgt` into the [super::PortComplex], and waits for data to be present.
        ///
        /// The [super::PortComplex] will drop its end ot the [alloc::sync::Arc] reducing the strong count indicating the data is ready.
        ///
        /// The caller must block interrupts prior to polling this initially and re-enable them after this is called once.
        /// Not blocking interrupts may cause a data-race. Not enabling interrupts may cause this future to never return [Poll::Ready]
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            // We drop this immediately to free the mutex.
            let Some(mut ctl) = self.ctl.take() else {
                return if alloc::sync::Arc::strong_count(&self.tgt) > 1 {
                    Poll::Pending
                } else {
                    Poll::Ready((
                        self.buff
                            .take()
                            .expect("Called Future::poll after returning Poll::Ready(_)"),
                        self.tgt.1.load(core::sync::atomic::Ordering::Relaxed),
                    ))
                };
            };

            let port = match self.port_num {
                PortNum::One => &mut ctl.port_1,
                PortNum::Two => &mut ctl.port_2,
            };

            *port.out_buff.lock() = Some(self.tgt.clone());
            port.cmd_ack.register(cx.waker());

            Poll::Pending
        }
    }

    impl hootux::fs::sysfs::bus::BusDeviceFile for PortFileObject {
        fn bus(&self) -> &'static str {
            "i8042"
        }

        fn id(&self) -> Arguments {
            match self.port {
                PortNum::One => format_args!("port-1"),
                PortNum::Two => format_args!("port-2"),
            }
        }

        fn as_any(self: Box<Self>) -> Box<dyn Any> {
            self
        }
    }
}
