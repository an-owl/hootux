//! This module handles UART devices (obviously). It handles it in 2 modes "deaf & dumb" and async.
//!
//! - Deaf & dumb does not attempt to actually check that a UART device exists ad is intended for
//! early startup messages
//! - Async is initialized with other drivers and configures the serial device as a
//! [futures_util::Sink] it also provides sync methods. The async mode also allows the serial device
//! to act as a [futures_util::Stream] although the sink is mutually exclusive.
//!
//! If I ever write serial drivers for other devices (i.e PCI) I need to check if this module owns
//! the device  and claim ownership.  

use alloc::boxed::Box;
use alloc::string::ToString;
use core::pin::Pin;
use core::task::{Context, Poll};
use lazy_static::lazy_static;
use modular_bitfield::{bitfield, BitfieldSpecifier};
use spin::Mutex;
use uart_16550::SerialPort;

mod dispatcher;

static COM_REAL: spin::RwLock<alloc::vec::Vec<alloc::sync::Arc<Serial>>> =
    spin::RwLock::new(alloc::vec::Vec::new());

pub static COM: spin::RwLock<alloc::vec::Vec<dispatcher::SerialDispatcher>> =
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

    if let Some(s) = COM.read().get(0) {
        // drop the result, if something goes wrong dont who a I going to tell?
        drop(s.write_sync(args.to_string().into_bytes().into_boxed_slice()));
    } else {
        x86_64::instructions::interrupts::without_interrupts(|| {
            SP0.lock()
                .write_fmt(args)
                .expect("Printing to serial failed")
        });
    }
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

// qemu uses 8250 (no scratch register)
struct Serial {
    base: u16, // io address

    // cached info, reading this from io bus is slow
    divisor: u16,
    bits: DataBits,
    parity: Parity,
    stop: StopBits,
    modem_state: atomic::Atomic<ModemCtl>,

    // these are inverted, we read from the write buff (so we can write it to the serial line)
    write_buff: Mutex<crate::interrupts::buff::ChonkyBuff<u8>>,
    // is running?
    run: atomic::Atomic<bool>,
    read_buff: Mutex<Option<(usize, Box<[u8]>)>>,
    dispatcher: futures_util::task::AtomicWaker,
}

impl Serial {
    const INT_ENABLE: u16 = 1;
    const INT_ID: u16 = 2;
    const LINE_CTL: u16 = 3;
    const MODEL_CTL: u16 = 4;
    const LINE_STS: u16 = 5;
    const MODEM_STS: u16 = 6;

    #[allow(unused)]
    // not used because it doesn't work on qemu
    const SCRATCH_REG: u16 = 7;

    pub fn new(addr: u16) -> Result<Self, SerialError> {
        let mut s = Self {
            base: addr,

            divisor: 3, // default: 38400Hz
            bits: DataBits::Eight,
            parity: Parity::None,
            stop: StopBits::One,
            modem_state: atomic::Atomic::new(ModemCtl::empty()),

            write_buff: Mutex::new(crate::interrupts::buff::ChonkyBuff::new()),
            run: atomic::Atomic::new(false),

            read_buff: Mutex::new(None),
            dispatcher: futures_util::task::AtomicWaker::new(),
        };

        s.test().map(|_| s)
    }

    pub fn set_divisor(&mut self, divisor: u16) {
        //SAFETY: These are safe because the ports point to registers owned by `self`

        assert_ne!(divisor, 0, "Attempted to set serial divisor to 0"); // div(0) errors here
        let mut line = x86_64::instructions::port::Port::new(self.base + Self::LINE_CTL);
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

        unsafe { x86_64::instructions::port::Port::new(self.base + Self::LINE_CTL).write(b) };
    }

    fn test(&mut self) -> Result<(), SerialError> {
        // disable interrupts
        let mut int = x86_64::instructions::port::Port::new(self.base + Self::INT_ENABLE);
        unsafe { int.write(0u8) };
        self.set_divisor(3);

        self.modem_ctl(ModemCtl::LOOPBACK);

        // the result here is ignored.
        // This should never fail on a working device.
        // I dont care if the send failed.
        #[allow(dropping_copy_types)]
        drop(self.try_send(1));
        if let Some(1) = self.receive() {
            self.modem_ctl(ModemCtl::DATA_TERMINAL_READY);
            Ok(())
        } else {
            Err(SerialError::NoLoopback)
        }
    }

    pub fn modem_ctl(&self, state: ModemCtl) {
        self.modem_state.store(state, atomic::Ordering::Relaxed);
        let mut p = x86_64::instructions::port::Port::new(self.base + Self::MODEL_CTL);
        unsafe { p.write(state.bits) }
    }

    /// Outputs the byte over the serial line.
    ///
    /// Most significant bits will be truncated if self is not using [DataBits::Eight]
    // todo should this be &mut self?

    pub fn try_send(&self, data: u8) -> Result<(), u8> {
        if self.can_send() {
            let mut reg = x86_64::instructions::port::Port::new(self.base);
            unsafe { reg.write(data) }
            Ok(())
        } else {
            Err(data)
        }
    }

    fn can_send(&self) -> bool {
        // SAFETY: Extra bits are allowed, this port addr is owned by there are no read effects
        let line: LineStatus = unsafe {
            LineStatus::from_bits_unchecked(
                x86_64::instructions::port::Port::new(self.base + Self::LINE_STS).read(),
            )
        };
        line.contains(LineStatus::EMPTY_TRANSMIT_REG)
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
    pub fn queue_send<T: Into<Box<[u8]>>>(&self, buff: T) {
        x86_64::instructions::interrupts::without_interrupts(|| {
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
        })
    }

    /// Returns the interrupt vector for this port.
    ///
    /// Note: These are routed to IOAPIC's not directly to the CPU's interrupt vectors
    fn irq(&self) -> Option<u8> {
        match self.base {
            0x3f8 | 0x3e8 => Some(4), /* SERIAL_ADDR[0]*/
            0x2f8 | 0x2e8 => Some(3),
            _ => None,
        }
    }

    fn int_id(&self) -> IntIdentification {
        // SAFETY: This address is owned by this port
        let r: u8 =
            unsafe { x86_64::instructions::port::Port::new(self.base + Self::INT_ID).read() };
        r.into()
    }

    /// Returns an interrupt handler.
    ///
    /// This fn takes `Self` as a [alloc::sync::Arc] and cannot de-register itself if the device is
    /// removed. If `self` is removed from [COM_REAL] for any reason the interrupt handler needs to
    /// be removed manually.
    fn int_handler(self: alloc::sync::Arc<Self>) -> Box<dyn Fn()> {
        let h = move || {
            let id = self.int_id();
            if id.pending() {
                // pending bit is assert low
                return;
            }

            match id.reason() {
                IntReason::ModemStatus => panic!("Serial modem status change"), // This is not configured to raise an interrupt
                IntReason::TransmitterEmpty => {
                    while self.can_send() {
                        // fill the fifo
                        if let Some(b) = self.write_buff.lock().pop() {
                            self.try_send(b).unwrap() // checks if sending is allowed beforehand
                        } else {
                            break;
                        }
                    }
                }
                IntReason::DataAvailable => {
                    if let Some((ref mut i, ref mut b)) = *self.read_buff.lock() {
                        // always drain fifo
                        while let Some(n) = self.receive() {
                            // only inserts if buffer is not full
                            if *i < b.len() {
                                b[*i] = n;
                                *i += 1;
                            }
                        }
                    } else {
                        // FIFO must be drained
                        while let Some(_) = self.receive() {}
                    }
                }
                IntReason::LineStatus => panic!("Serial line status change"), // This is not configured to raise an interrupt
                IntReason::FifoTimeOut => {
                    if let Some((ref mut i, ref mut b)) = *self.read_buff.lock() {
                        // always drain fifo
                        while let Some(n) = self.receive() {
                            // only inserts if buffer is not full
                            if *i < b.len() {
                                b[*i] = n;
                                *i += 1;
                            }
                        }
                    } else {
                        // FIFO must be drained
                        while let Some(_) = self.receive() {}
                    }
                }
            }

            self.dispatcher.wake()
        };

        Box::new(h)
    }

    /// Sets the interrupt enable register.
    ///
    /// Setting [ModemCtl::AUX2] is also required on most systems to enable interrupts.
    ///
    /// # Safety
    ///
    /// This fn is unsafe because it modifies interrupt behaviour. The caller must ensure that
    /// interrupts are correctly handled
    unsafe fn set_int_enable(&self, mode: InterruptEnable) {
        log::debug!("serial int enable: {mode:#x}");
        x86_64::instructions::port::Port::new(self.base + Self::INT_ENABLE).write(mode.bits());
    }

    /// Returns the modem status register
    fn get_modem_state(&self) -> ModemStatus {
        // SAFETY: This is safe
        unsafe {
            ModemStatus::from_bits_unchecked(
                x86_64::instructions::port::Port::new(self.base + Self::MODEM_STS).read(),
            )
        }
    }

    // todo do properly
    /// # Safety
    ///
    /// This is safe because illegal writes are discarded
    fn set_fifo(&self, data: u8) {
        unsafe { x86_64::instructions::port::Port::new(self.base + Self::INT_ID).write(data) };
    }
}

/// This implementation acts only to wake the dispatcher all of the important processing is
/// handled by the interrupt handler
impl futures_util::Future for &Serial {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.dispatcher.register(cx.waker());
        Poll::Pending
    }
}

#[derive(Debug, Eq, PartialEq, Copy, Clone)]
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

const SERIAL_ADDR: [u16; 8] = [0x3f8, 0x2f8, 0x3e8, 0x2e8, 0x5f8, 0x4f8, 0x5e8, 0x4e8];

pub fn init_rt_serial() {
    let mut com = alloc::vec::Vec::new();
    let mut dispatchers = alloc::vec::Vec::new();
    for a in SERIAL_ADDR.iter() {
        match Serial::new(*a) {
            Ok(p) => {
                log::info!("Found UART device on {a:#x}");

                p.set_fifo(6); // Clears & disables FIFO

                // Handle pending interrupts by ignoring them.
                loop {
                    let id = p.int_id();
                    if !id.pending() {
                        match id.reason() {
                            IntReason::ModemStatus => {
                                p.get_modem_state();
                            }
                            IntReason::TransmitterEmpty => {} // reading this handled this
                            IntReason::DataAvailable => {
                                p.receive();
                            }
                            IntReason::LineStatus => {
                                p.line_sate();
                            }
                            IntReason::FifoTimeOut => {
                                p.receive();
                            }
                        }
                    } else {
                        break;
                    }
                }

                // SAFETY: The interrupt handler installed later handles these & interrupts are not
                // enabled until after the handler is installed
                unsafe {
                    p.set_int_enable(
                        InterruptEnable::TRANSMIT_HOLDING_REGISTER_EMPTY
                            | InterruptEnable::DATA_RECEIVED,
                    )
                }
                let p = alloc::sync::Arc::new(p);
                let d = dispatcher::SerialDispatcher::new(&p);
                com.push(p);

                dispatchers.push(d.clone());
                crate::task::run_task(Box::pin(d.run()))
            }
            Err(_) => {}
        }
    }

    let mut irq_map = alloc::collections::BTreeMap::new();
    for p in &com {
        if let Some(irq) = p.irq() {
            let l = irq_map.entry(irq).or_insert(alloc::vec::Vec::new());
            l.push(p.clone().int_handler());
        }
    }

    for (isa_irq, handler) in irq_map {
        let t = crate::interrupts::reserve_irq(0, 1).expect("Failed to allocate IRQ for serial"); //todo handle Err()
        let irq = crate::interrupts::InterruptIndex::Generic(t);

        let (mut gsi, ov) = irq.get_isa(isa_irq);

        // if these are `Some(t)` `gsi.field` will already be `t`
        gsi.trigger_mode = ov
            .as_ref()
            .map_or(None, |s| s.trigger_mode)
            .unwrap_or(crate::interrupts::apic::ioapic::TriggerMode::LevelTriggered);
        gsi.polarity = ov
            .as_ref()
            .map_or(None, |s| s.polarity)
            .unwrap_or(crate::interrupts::apic::ioapic::PinPolarity::AssertHigh);
        gsi.mask = false;

        // SAFETY: This is safe because `handler` contains interrupt handlers for the serial ports.
        unsafe {
            irq.set(
                crate::interrupts::vector_tables::InterruptHandleContainer::HighPerfCascading(
                    handler,
                ),
            );
            gsi.set().expect("Failed to set GSI");
        }
    }

    for i in &com {
        if let Some(_) = i.irq() {
            i.modem_ctl(ModemCtl::AUX2 | ModemCtl::DATA_TERMINAL_READY);
        }
    }

    *COM_REAL.write() = com;
    *COM.write() = dispatchers;
}

#[allow(unused)]
const SHUTUPSHUTUPSHUTUPSHUTUP: () = {
    let t = IntIdentification::new();
    t.into_bytes();
};

#[bitfield]
#[derive(Debug)]
struct IntIdentification {
    pending: bool, // invert this, this bit is assert low.
    reason: IntReason,
    #[skip]
    _reserved: modular_bitfield::specifiers::B1,
    #[allow(dead_code)]
    fifo_64_bytes: bool,
    #[allow(dead_code)]
    fifo: FifoEnabled,
}

#[derive(BitfieldSpecifier, Debug, Copy, Clone, Eq, PartialEq)]
#[bits = 3]
#[repr(C)]
enum IntReason {
    ModemStatus = 0,
    TransmitterEmpty,
    DataAvailable,
    LineStatus,
    FifoTimeOut = 6,
}

#[derive(BitfieldSpecifier, Debug, Copy, Clone, Eq, PartialEq)]
#[bits = 2]
#[repr(C)]
enum FifoEnabled {
    No = 0,
    Busted = 2,
    Working,
}

impl From<u8> for IntIdentification {
    fn from(value: u8) -> Self {
        IntIdentification::from_bytes([value])
    }
}

bitflags::bitflags! {
    struct ModemStatus: u8 {
        const DELTA_CLEAR_TO_SEND = 1;
        const DELTA_DATA_SET_READY = 1 << 1;
        const TRAILING_EDGE_RING_DETECTOR = 1 << 2;
        const TRAILING_DEGE_RING_DETECTOR =  1 << 3;
        const CLEAR_TO_SEND = 1 << 4;
        const DATA_SET_READY = 1 << 5;
        const RING_INDICATOR = 1 << 6;
        const CARRIER_DETECT = 1 << 7;
    }

    struct InterruptEnable: u8 {
        /// Interrupt when data has been received
        const DATA_RECEIVED = 1;
        /// Interrupt when all data has been transmitted
        const TRANSMIT_HOLDING_REGISTER_EMPTY = 1 << 1;
        /// Interrupt when something in the line status register is updated. This is usually an error.
        const RECIEVER_LIE_STATUS = 1 << 2;
        /// Interrupt when something in the modem status register is updated.
        ///
        /// This indicates something on the physical connection has changed.
        const MODEM_STATUS_INTERRUPT = 1 << 3;
        const SLEEP_MODE = 1 << 4;
        const LOW_POWER_MODE = 1 << 5;
    }
}
