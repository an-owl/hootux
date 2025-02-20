use core::arch::asm;

struct PS2Controller;

impl PS2Controller {
    unsafe fn read_unchecked(&self) -> u8 {
        let ret: u8;
        unsafe {
            asm!(
               "in al, 0x60",
                out("al") ret,
                options(nomem, nostack)
            )
        }
        ret
    }

    /// Read data from data port, if it's currently full.
    fn read_data(&self) -> Option<u8> {
        if self.read_status().contains(StatusFlags::OUTPUT_BUFFER_FULL) {
            let ret: u8;
            unsafe {
                asm!(
                    "in al, 0x60",
                    out("al") ret,
                    options(nomem, preserves_flags)
                )
            }
            Some(ret)
        } else {
            None
        }
    }

    fn read_status(&self) -> StatusFlags {
        let read: u8;
        unsafe {
            asm!(
                "in al, 0x60",
                out("al") read,
                options(nomem, preserves_flags)
            );
        }
        StatusFlags::from_bits_truncate(read)
    }

    /// Attempts to write data to the input register.
    ///
    /// Returns `Err(())` when the data register is empty.
    fn send_data(&self, data: u8) -> Result<(), ()> {
        if !self.read_status().contains(StatusFlags::INPUT_BUFFER_FULL) {
            unsafe {
                asm!(
                    "out 0x60,al",
                    in("al") data,
                    options(nomem, preserves_flags)
                )
            }
            Ok(())
        } else {
            Err(())
        }
    }

    /// Writes the `command` to the controller with the data byte if one is given.
    fn send_command(&self, command: impl Into<RawCommand>) {
        let c = command.into();
        let byte_1 = c.first_byte;
        let byte_2 = c.second_byte.unwrap_or(0);
        let multiple = c.second_byte.is_some() as u16;

        // Writes command byte
        // If multiple bytes are to be written {
        //      move `{sec}` into `al`
        //      send data byte
        // }
        // End
        unsafe {
            asm!(
                "out 0x64,al",
                "jxcz 2f"
                "mov al, {sec}",
                "out 0x60",
                "2:",
                _ = in("al") byte_1,
                sec = in(reg_byte) byte_2,
                do_data = in("cx") multiple,
                options(nomem, preserves_flags)
            )
        }
    }
}

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug)]
    /// Status flags for 8042 controller.
    ///
    /// Note that Output/Inpput terms are from the controllers perspective.
    /// e.g. we read data when [Self::OUTPUT_BUFFER_FULL] is set.
    struct StatusFlags: u8 {
        /// Indicates that data is ready to be read form the controller.
        ///
        /// When this flag is set, the data register may be read.
        const OUTPUT_BUFFER_FULL = 1;

        /// Indicates data is waiting to be sent.
        ///
        /// If software wants to write to the input register this flag mustbe clear.
        const INPUT_BUFFER_FULL = 1 << 1;

        /// Cleared by software on reset and set after completing POST
        // what's the point in that?
        // yes we passed POST, what a shock
        /// Should be set at runtime, however on modern hardware this may not be set.
        const SYSTEM_FLAG = 1 << 2;

        /// Indicates that the controller is expecting a data byte to follow a command.
        ///
        /// This is cleared after writing to the input register.
        const TRANSMIT_COMMAND = 1 << 3;
        const TIME_OUT = 1 << 6;
        const _ = 3 << 4;
        const PARITY_ERROR = 1 << 7;
    }
}

#[repr(u8)]
#[derive(Copy, Clone, Debug)]
enum Command {
    /// Reads byte from internal memory at specified address.
    ///
    /// Address 0 contains controller configuration, the caller should cast this to [ConfigurationByte]
    ///
    /// Values above 31 are disallowed.
    ReadByte(Address),

    /// Writes the second operand into the internal memory into the address specified by the
    /// second operand.
    ///
    /// Address 0 contains controller configuration,
    /// the caller should cast a [ConfigurationByte] to u8 for access.
    ///
    /// Values above 31 are disallowed.
    WriteByte(Address, u8),

    DisablePort2,
    EnablePort2,
    TestPort2,

    /// Performs a Built-In Self Test (BIST)
    ///
    /// * Returns `0x55` if the test passed.
    /// * Returns `0xFC` if the test failed.
    TestController,
    TestPort1,

    /// Dumps diagnostic information
    ///
    /// Outputs 16 bytes of internal memory, the state of the output and output port, the program status word.
    /// All data is sent in scancode format.
    // todo: what is scancode format?
    DiagnosticDump,

    /// Sets command byte:4 to `1`. This forces teh clock signal low, disabling the interface.
    DisablePort1,

    /// Sets command byte:4 to `0`. This enables the clock signal, enabling hte interface
    EnablePort1,

    /// Exports controller input port to data register.
    ReadControllerInput,

    /// Copies the low nibble of the input port to status bits 4-7.
    CopyLowNibbleToStatus,

    /// Copies the high nibble of the input port to status bits 4-7.
    CopyHighNibbleToStatus,

    /// Returns [ControllerOutput], this should only be used when [StatusFlags::OUTPUT_BUFFER_FULL] is clear.
    ReadControllerOutput,

    /// Writes payload to controller output port.
    WriteControllerOutput(ControllerOutput),

    /// Treats the payload if it was received by the first port.
    WriteByteToFirstOutput(u8),

    /// Treats the payload as if it was written to the second port.
    WriteByteToSecondOutput(u8),

    /// Writes the payload to the second port input buffer.
    WriteToSecondInput(u8),

    /// Lowers the indicated pins.
    ///
    /// The duration of the pulse differs depending on the hardware.
    /// On the original i8042 this was 6ms,
    /// for a SCH311x SuperIO chip this is 500ns.
    ///
    /// Please read [ControllerOutput::SYSTEM_RESET].
    PulseOutputPins(ControllerOutput),
}

bitflags::bitflags! {
    /// Configuration byte contained in address `0` in internal memory.
    struct ConfigurationByte: u8 {
        const PORT_ONE_INT = 1;
        const PORT_TWO_INT = 1 << 1;
        const SYSTEM_FLAG = 1 << 2;
        const PORT_ONE_CLOCK = 1 << 4;
        const PORT_TWO_CLOCK = 1 << 5;
        const PORT_TRANSLATION = 1 << 6;
    }
}

#[derive(Copy, Clone, Debug)]
struct Address {
    data: u8,
}

impl Address {
    const fn new(address: u8) -> Option<Self> {
        if address > 32 {
            None
        } else {
            Some(Self { data: address })
        }
    }
}

bitflags::bitflags! {
    #[derive(Copy, Clone, Debug)]
    struct ControllerOutput: u8 {
        /// Reset pin status.
        ///
        /// <div class="warning"> This is attached to the CPU and must not be cleared </div>
        const SYSTEM_RESET = 1;

        /// CPU A20 gate, or'd with the address-20 bit on the CPU. When disabled the CPU can only access odd MiB of memory.
        const A20_GATE = 1 << 1;
        const SECOND_PORT_CLOCK = 1 << 2;
        const SECOND_PORT_DATA = 1 << 3;
        const FIRST_PORT_IRQ = 1 << 4;
        const SECOND_PORT_IRQ = 1 << 5;
        const FIRST_PORT_CLOCK = 1 << 6;
        const FIRST_PORT_DATA = 1 << 7;
    }
}

struct RawCommand {
    first_byte: u8,
    second_byte: Option<u8>,
}

macro_rules! n_command {
    ($first:expr) => {
        RawCommand {
            first_byte: $first,
            second_byte: None,
        }
    };

    ($first:expr, $second:expr) => {
        RawCommand {
            first_byte: $first,
            second_byte: Some($second),
        }
    };
}

impl From<Command> for RawCommand {
    fn from(value: Command) -> Self {
        match value {
            Command::ReadByte(addr) => n_command!(0x20 + addr.data),
            Command::WriteByte(addr, data) => n_command!(0x60 + addr.data, data),
            Command::DisablePort2 => n_command!(0xa7),
            Command::EnablePort2 => n_command!(0xa8),
            Command::TestPort2 => n_command!(0xa9),
            Command::TestController => n_command!(0xaa),
            Command::TestPort1 => n_command!(0xab),
            Command::DiagnosticDump => n_command!(0xac),
            Command::DisablePort1 => n_command!(0xad),
            Command::EnablePort1 => n_command!(0xae),
            Command::ReadControllerInput => n_command!(0xc0),
            Command::CopyLowNibbleToStatus => n_command!(0xc1),
            Command::CopyHighNibbleToStatus => n_command!(0xc2),
            Command::ReadControllerOutput => n_command!(0xd0),
            Command::WriteControllerOutput(payload) => n_command!(0xd1, payload.bits()),
            Command::WriteByteToFirstOutput(payload) => n_command!(0xd2, payload),
            Command::WriteByteToSecondOutput(payload) => n_command!(0xd3, payload),
            Command::WriteToSecondInput(payload) => n_command!(0xd4, payload),
            Command::PulseOutputPins(payload) => n_command!(0xf0 + payload.bits() & 8),
        }
    }
}

impl core::fmt::Debug for RawCommand {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let mut dt = f.debug_tuple("RawCommand");

        match self.second_byte {
            Some(second_byte) => {
                dt.field(&format!("{:?}", &[self.first_byte, second_byte]));
            }
            None => {
                dt.field(&format!("{:?}", &[self.first_byte]));
            }
        }
        dt.finish()
    }
}
