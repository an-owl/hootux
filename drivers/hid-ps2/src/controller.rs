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
    port_1: PortComplex,
    port_2: PortComplex,
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
    ($self:ident,$port:ident, $isa_irq:literal, $fail:literal) => {{
        let this = alloc::sync::Arc::downgrade($self);

        let int_handler = move |_, _| {
            // None indicates a driver bug, The interrupt should've been disabled.
            // This will just continue to handle the interrupts without doing anything.
            let Some(ctl) = this.upgrade() else {
                unsafe { hootux::interrupts::eoi() }
                return;
            };

            let mut in_buff = [0; 8];

            // SAFETY: Interrupt indicates that the register is full, first read is safe
            in_buff[0] = unsafe { ctl.ctl_raw.read_unchecked() };
            for i in in_buff.iter_mut().skip(1) {
                match ctl.ctl_raw.read_data() {
                    Some(d) => *i = d,
                    None => break,
                }
            }

            if let Ok(_) = TryInto::<hid_8042::keyboard::Response>::try_into(in_buff[0]) {
                ctl.$port
                    .ack_buffer
                    .borrow_mut()
                    .copy_from_slice(&in_buff[0..4]);
            }

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
                    $this.$port_name.port_status = PortState::Nuisance;
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
                        $this.$port_name.port_status = PortState::Nuisance;
                        return;
                    }
                }
                Err(CommandError::PortTimeout) => {
                    $this.$port_name.port_status = PortState::Down;
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
                    $this.$port_name.port_status = PortState::Nuisance;
                    return;
                }
                Ok([const { Response::Resend as u8 }, ..]) => continue,
                Ok([const { Response::Ack as u8 }, content @ .., _]) if r == 1 => {
                    match hid_8042::DeviceType::try_from(content) {
                        Ok(device_type) => {
                            $this.$port_name.port_status = PortState::Device(device_type);
                            break;
                        }
                        Err(_) => {
                            log::warn!("Unable to identify device: invalid type");
                            $this.$port_name.port_status =
                                PortState::Device(hid_8042::DeviceType::Unknown);
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
                    $this.$port_name.port_status = PortState::Down;
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
                $this.$port_name.dev_meta = DevMeta::ScanCodeSetTwo;
            }

            Ok([const { Response::Resend as u8 }, ..]) => {
                match $this
                    .$port_command(Command::ScanCodeSet(
                        ScanCodeSetSubcommand::SetScancodeSetOne,
                    ))
                    .await
                {
                    Ok([const { Response::Ack as u8 }, ..]) => {
                        $this.$port_name.dev_meta = DevMeta::ScanCodeSetOne;
                    }

                    Err(CommandError::PortTimeout) => {
                        $this.$port_name.port_status = PortState::Down;
                        return;
                    }

                    _ => $this.$port_name.set_state(PortState::Nuisance),
                }
            }
            Err(CommandError::PortTimeout) => {
                $this.$port_name.port_status = PortState::Down;
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
    fn new() -> Self {
        Self {
            ctl_raw: hid_8042::controller::PS2Controller,
            multi_port: true,
            port_1: PortComplex::new(),
            port_2: PortComplex::new(),
        }
    }
    /// Initializes the controller.
    async fn init(mut self) -> Result<Self, ()> {
        static INITIALIZED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);

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
    pub async fn port_1_command(
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

    pub async fn port_2_command(
        &mut self,
        cmd: hid_8042::keyboard::Command,
    ) -> Result<[u8; 4], CommandError> {
        const WRITE_PORT_2_CMD: u8 = 0xd3;

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

    async fn init_port1(&mut self) {
        init_port!(self, port_1, port_1_command, "port 1");
    }

    async fn init_port2(&mut self) {
        init_port!(self, port_2, port_2_command, "port 2")
    }

    fn cfg_interrupt_port1(self: &alloc::sync::Arc<Self>) {
        interrupt_init_body!(
            self,
            port_1,
            1,
            "Failed to allocate interrupt for 8042 port-. Aborting"
        )
    }

    fn cfg_interrupt_port2(self: &alloc::sync::Arc<Self>) {
        interrupt_init_body!(
            self,
            port_2,
            12,
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
}

struct PortComplex {
    port_status: PortState,
    dev_meta: DevMeta,
    ack_buffer: core::cell::RefCell<[u8; 4]>,
    cmd_ack: futures_util::task::AtomicWaker,
}

impl PortComplex {
    const fn new() -> PortComplex {
        Self {
            port_status: PortState::Down,
            dev_meta: DevMeta::ScanCodeSetOne,
            ack_buffer: core::cell::RefCell::new([0; 4]),
            cmd_ack: futures_util::task::AtomicWaker::new(),
        }
    }

    fn set_state(&mut self, state: PortState) {
        log::trace!("set device to {:?}", state);
        self.port_status = state;
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
            todo!()
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
        self.port_status
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
                Poll::Ready(*b)
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
enum DevMeta {
    ScanCodeSetOne,
    ScanCodeSetTwo,
    ScanCodeSetThree,
}

fn controller_timeout_handler() {
    log::error!("Controller timed out: driver should be disabled.");
    // fixme Remove panic and replace with a driver-kill
    panic!("8042 controller timed out");
}
