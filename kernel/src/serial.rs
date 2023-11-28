use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::SerialPort;

static COM_REAL: spin::RwLock<alloc::vec::Vec<Option<alloc::sync::Arc<Serial>>>> =
    spin::RwLock::new(alloc::vec::Vec::new());

lazy_static! {
    pub static ref SP0: Mutex<SerialPort> = {
        let mut serial_port = unsafe { SerialPort::new(0x3f8) };
        serial_port.init();
        Mutex::new(serial_port)
    };
}

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    use core::fmt::Write;

    x86_64::instructions::interrupts::without_interrupts(|| {
        SP0.lock()
            .write_fmt(args)
            .expect("Printing to serial failed")
    });
}

/// Prints to the host through the serial interface.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => {
        $crate::serial::_print(format_args!($($arg)*))
    };
}

/// Prints to the host through the serial interface, appending a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}

struct Serial {
    base: u16, // io address

    // cached info, reading this from io bus is slow
    divisor: u16,
    bits: DataBits,
    parity: Parity,
    stop: StopBits,
    modem_state: ModemCtl,

    // these are inverted, we read from the write buff (so we can write it to the serial line)
    write_buff: Mutex<crate::interrupts::buff::ChonkyBuff<u8>>,
    // is running?
    run: atomic::Atomic<bool>,

    read_buff: Mutex<Option<(usize, &'static mut [u8])>>,
    stream_lock: core::sync::atomic::AtomicBool,
}

impl Serial {
    const INT_ENABLE: u16 = 1;
    const LINE_CTL: u16 = 3;
    const MODEL_CTL: u16 = 4;
    const LINE_STS: u16 = 5;
    const SCRATCH_REG: u16 = 7;

    pub fn new(addr: u16) -> Result<Self, SerialError> {
        let mut s = Self {
            base: addr,

            divisor: 3, // default: 38400Hz
            bits: DataBits::Eight,
            parity: Parity::None,
            stop: StopBits::One,
            modem_state: ModemCtl::empty(),

            write_buff: Mutex::new(crate::interrupts::buff::ChonkyBuff::new()),
            run: atomic::Atomic::new(false),

            read_buff: Mutex::new(None),
            stream_lock: core::sync::atomic::AtomicBool::new(false),
        };

        let r = s.test();
        if let Err(SerialError::NoLoopback) = &r {
            log::warn!("Serial port at {:#x} may be faulty: Device responded to scratch register, but not loopback test", s.base);
        };
        r.map(|_| s)
    }

    pub fn set_divisor(&mut self, divisor: u16) {
        //SAFETY: These are safe because the ports point to registers owned by `self`

        assert_ne!(divisor, 0, "Attempted to set serial divisor to 0"); // div(0) errors here
        let mut line = x86_64::instructions::port::Port::new(self.base + Self::SCRATCH_REG);
        // set DLAB
        unsafe { line.write(0x80u8) };

        let mut low = x86_64::instructions::port::Port::new(self.base);
        unsafe { low.write(divisor.to_le_bytes()[0]) };

        let mut high = x86_64::instructions::port::Port::new(self.base + 1);
        unsafe { high.write(divisor.to_le_bytes()[1]) };

        // clear DLAB
        unsafe { line.write(0x00u8) };

        self.divisor = divisor;
        self.set_char_mode(self.bits, self.parity, self.stop);
    }

    pub fn set_char_mode(&mut self, data_bits: DataBits, parity: Parity, stop_bits: StopBits) {
        let mut b = data_bits as u8;
        b |= (parity as u8) << 2;
        b |= (stop_bits as u8) << 3;

        self.bits = data_bits;
        self.parity = parity;
        self.stop = stop_bits;
    }

    fn test(&mut self) -> Result<(), SerialError> {
        let test_pattern = 0xaau8;
        let mut p = x86_64::instructions::port::Port::new(self.base + Self::SCRATCH_REG);

        // test that port exists
        unsafe { p.write(test_pattern) };
        if unsafe { p.read() } != 1 {
            return Err(SerialError::NotPresent);
        };

        // disable interrupts
        let mut int = x86_64::instructions::port::Port::new(self.base + Self::INT_ENABLE);
        unsafe { int.write(0u8) };
        self.set_divisor(3);

        self.modem_ctl(ModemCtl::LOOPBACK);

        // the result here is ignored.
        // If the ready bit must be set and receive
        drop(self.try_send(1));
        if let Some(1) = self.receive() {
            self.modem_ctl(ModemCtl::DATA_TERMINAL_READY);
            Ok(())
        } else {
            Err(SerialError::NoLoopback)
        }
    }

    pub fn modem_ctl(&mut self, state: ModemCtl) {
        self.modem_state = state;
        let mut p = x86_64::instructions::port::Port::new(self.base + Self::MODEL_CTL);
        unsafe { p.write(state.bits) }
    }

    /// Outputs the byte over the serial line.
    ///
    /// Most significant bits will be truncated if self is not using [DataBits::Eight]
    // todo should this be &mut self?

    pub fn try_send(&self, data: u8) -> Result<(), u8> {
        // SAFETY: Extra bits are allowed, this port addr is owned by there are no read effects
        let line: LineStatus = unsafe {
            LineStatus::from_bits_unchecked(
                x86_64::instructions::port::Port::new(self.base + Self::LINE_STS).read(),
            )
        };

        if line.contains(LineStatus::EMPTY_DATA_REG) {
            let mut reg = x86_64::instructions::port::Port::new(self.base);
            unsafe { reg.write(data) }
            Ok(())
        } else {
            Err(data)
        }
    }

    fn line_sate(&self) -> LineStatus {
        // SAFETY: Extra bits are allowed
        unsafe {
            LineStatus::from_bits_unchecked(
                x86_64::instructions::port::Port::new(self.base + Self::LINE_STS).read(),
            )
        }
    }

    /// Reads the data from the serial port if there is any to be read.
    pub fn receive(&self) -> Option<u8> {
        if self.line_sate().contains(LineStatus::DATA_READY) {
            Some(unsafe { x86_64::instructions::port::Port::new(self.base).read() })
        } else {
            None
        }
    }

    /// Queues the buffer to be sent
    pub fn queue_send<T: Into<alloc::boxed::Box<[u8]>>>(&self, buff: T) {
        let mut l = self.write_buff.lock();
        l.push(buff);

        // try to start sending if it isn't already
        if !self.run.load(atomic::Ordering::Relaxed) {
            if let Some(b) = l.pop() {
                if self.try_send(b).is_ok() {
                    self.run.store(true, atomic::Ordering::Relaxed);
                }
            }
        }
    }
}

pub enum SerialError {
    NotPresent,
    NoLoopback,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub enum Parity {
    /// Disables parity bit
    None = 0,
    /// Adds together the value of the bits of the data. If the value is odd parity is set to `1`
    Odd,
    /// Adds together the value of the bits of the data. If the value is even parity is set to `1`
    Even,
    /// Sets the parity bit to `1`
    Mark,
    /// Sets the parity bit to `0`
    Space,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub enum StopBits {
    One = 0,
    /// Is a actually 1.5 if bits is [DataBits::Five]
    Two,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub enum DataBits {
    /// When set with [StopBits::Two] the number of stop bits will be 1.5
    Five = 0,
    Six,
    Seven,
    Eight,
}

bitflags::bitflags! {
    struct ModemCtl: u8 {
        const DATA_TERMINAL_READY = 1;
        const REQUEST_TO_SEND = 1 << 1;
        const AUX1 = 1 << 2;
        const AUX2 = 1 << 3;
        const LOOPBACK = 1 << 4;
        /// Only available on 16750 devices and newer
        const AUTOFLOW_CTL_ENABLE = 1 << 5;
    }
}

bitflags::bitflags! {
    /// Represents the state of the serial port.
    /// Errors here should be forwarded to application software to handle.
    struct LineStatus: u8 {
        /// Data is ready to be read.
        const DATA_READY = 1;
        /// Internal data buffers were overrun. If this error occurs and the the serial device
        /// currently in use, this should be considered a software error where the handler is not fast enough.
        const OVERRUN_ERR = 1 << 1;
        /// A parity error has occurred.
        const PARITY_ERR = 1 << 2;
        /// Stop bit was a logical `0`, stop bits should be logical `1` if this occurs the data line
        /// might be faulty or the timing might be incorrect.
        const FRAMING_ERR = 1 << 3;
        /// If logical `0`'s were received for an entire word this bit is set.
        const BREAK_INT = 1 << 4;
        /// Transmission hardware has no data to send and is idle.
        const EMPTY_TRANSMIT_REG = 1 << 5;
        /// Device can send more data.
        const EMPTY_DATA_REG = 1 << 6;
        /// If this is set, then the FIFO needs to be dumped because the data may be unreliable.
        const ERR_IN_FIFO = 1 << 7;
    }
}
