use core::task::{Context, Poll};
use hid_8042::keyboard::Response;
use hootux::task::util::sleep;
use hootux::time::{AbsoluteTime, Duration};

const TIMEOUT_DURATION_MS: u64 = 100;
const PORT_ONE_ISA_IRQ: u8 = 0x01;
const PORT_TWO_ISA_IRQ: u8 = 0x12;

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
    ($port:ident, $isa_irq:literal, $fail:literal) => {
        let this = alloc::sync::Arc::downgrade(self);

        let int_handler = || {
            // None indicates a driver bug, The interrupt should've been disabled.
            // This will just continue to handle the interrupts without doing anything.
            let Some(ctl) = this.upgrade() else {
                unsafe { hootux::interrupts::eoi() }
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

            if let Ok(_) = TryFrom::<hid_8042::keyboard::Response>::try_from(*in_buff[0]) {
                self.$port
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
    };
}

impl Controller {
    /// Initializes the controller.
    const fn new() -> Option<Self> {
        static INITIALIZED: core::sync::atomic::AtomicBool =
            core::sync::atomic::AtomicBool::new(false);
        let mut this = Self {
            ctl_raw: hid_8042::controller::PS2Controller,
            multi_port: true,
            port_1: PortComplex::new(),
            port_2: PortComplex::new(),
        };

        log::info!("Initializing controller...");

        use hid_8042::controller::*;
        // disable devices

        this.set_operation(false);
        // clear buffer
        while let Some(_) = this.ctl_raw.read_data() {}

        this.ctl_raw
            .send_command(Command::ReadByte(0.try_into().unwrap()));

        let mut cfg_byte = ConfigurationByte::from_bits_retain(loop {
            match this.ctl_raw.read_data() {
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

        this.ctl_raw
            .send_command(Command::WriteByte(0.try_into().unwrap(), cfg_byte.bits()));
        this.ctl_raw.send_command(Command::TestController);

        // Perform controller self test.
        match this.read_data_with_timeout(Duration::millis(TIMEOUT_DURATION_MS)) {
            Some(0x55) => {}
            Some(e) => {
                log::error!("8042 Initialization failed: BIST returned {e:#x}");
                return None;
            }
            None => {
                log::error!("8042 Initialization failed: BIST timed out");
                return None;
            }
        }

        // check if this is multi-port
        this.ctl_raw.send_command(Command::EnablePort2);
        this.ctl_raw.send_command(Command::ReadByte(0.into()));
        let t = ConfigurationByte::from_bits_retain(
            this.read_data_with_timeout(Duration::millis(TIMEOUT_DURATION_MS))?,
        );
        this.multi_port = !t.contains(ConfigurationByte::PORT_TWO_CLOCK_DISABLED);
        if this.multi_port {
            this.ctl_raw.send_command(Command::DisablePort2);
        }

        if this.multi_port {
            log::info!("8042 is mult-port")
        } else {
            log::info!("8042 is not mult-port");
        }

        // test ports
        {
            this.ctl_raw.send_command(Command::TestPort1);
            let r = PortTestResult::from_bits_retain(
                this.read_data_with_timeout(Duration::millis(TIMEOUT_DURATION_MS))?,
            );
            if !r.is_good() {
                log::warn!("8042 Port one test failure: {r}"); // I think this can indicate that nothing is connected.
            }
            this.ctl_raw.send_command(Command::TestPort1);
            let r = PortTestResult::from_bits_retain(
                this.read_data_with_timeout(Duration::millis(TIMEOUT_DURATION_MS))?,
            );
            if !r.is_good() {
                log::warn!("8042 Port two test failure: {r}")
            }
        }

        // we are done initializing the controller, enable ports.
        this.ctl_raw.send_command(Command::EnablePort1);
        this.ctl_raw.send_command(Command::EnablePort2);

        log::trace!("initialization completed");

        Some(this)
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

        let mut timeout = sleep(TIMEOUT_DURATION_MS);
        let mut cmd_ret = self.port_1.cmd_wait();
        let ret = futures_util::select_biased! {
            _ = timeout => return Err(CommandError::PortTimeout),
            ret = cmd_ret => ret,
        };

        Ok(ret)
    }

    pub async fn port_2_command(
        &mut self,
        cmd: hid_8042::keyboard::Command,
    ) -> Result<(), CommandError> {
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
        Ok(())
    }

    fn cfg_interrupt_port1(self: &alloc::sync::Arc<Self>) {
        interrupt_init_body!(
            port_1,
            1,
            "Failed to allocate interrupt for 8042 port-. Aborting"
        )
    }

    fn cfg_interrupt_port2(self: &alloc::sync::Arc<Self>) {
        interrupt_init_body!(
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
    dev_type: spin::Mutex<Option<hid_8042::DeviceType>>,
    ack_buffer: core::cell::RefCell<[u8; 4]>,
    cmd_ack: futures_util::task::AtomicWaker,
}

impl PortComplex {
    const fn new() -> PortComplex {
        Self {
            dev_type: spin::Mutex::new(None),
            ack_buffer: core::cell::RefCell::new([0; 4]),
            cmd_ack: Default::default(),
        }
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

    fn set_device(&mut self, dev_type: Option<hid_8042::DeviceType>) {}
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
