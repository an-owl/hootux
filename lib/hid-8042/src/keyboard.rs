/// Commands which can be sent to a keyboard, commands must be cast through [CommandBytes] before they are sent.
///
/// All commands return either [Response::Ack] or [Response::Resend] unless documented otherwise.
///
/// Calling [Command::raw_iter] will return an iterator that will return the command as bytes to be sent.
#[derive(Copy, Clone, Debug)]
pub enum Command {
    /// Sets indicator LEDs on the device.
    ///
    /// Returns either [Response::Ack] or [Response::Resend]
    SetLed(KeyboardLed),

    /// Echo, device must respond with [Response::Echo] or [Response::Resend].
    Echo,

    /// Sets or gets the scancode set.
    ///
    /// When the subcommand is [ScanCodeSetSubcommand::GetScanCode] the set is returned as an
    /// integer representing the scan code set.
    /// (`1`, for [ScanCodeSetSubcommand::SetScancodeSetOne],
    /// `2` for [ScanCodeSetSubcommand::SetScancodeSetTwo],
    /// `3` for [ScanCodeSetSubcommand::SetScancodeSetThree] )
    ///
    /// The SetKey* commands contain the scancode of the key that is intended to be set.
    /// The scancode uses a null terminated string, the caller should ensure the scancode is a valid key and the first byte is not `0`
    ScanCodeSet(ScanCodeSetSubcommand),

    /// Returns a [super::DeviceType]
    IdentifyDevice,

    /// Sets repeat delay and repeat rate.
    RepeatCtl(RepeatCtl),

    /// Enables keyboard scanning allowing the keyboard to send scancodes.
    EnableScan,

    /// Disables scanning, preventing the keyboard from sending scancodes.
    DisableScan,

    /// Set default parameters.
    // todo which params? What are the defaults?
    SetDefaultParams,

    /// Set all keys to typematic/autorepeat only (scancode set 3 only)
    SetTypmatic,

    /// Set all keys to make/release (scancode set 3 only)
    MakeRelease,

    /// Set all keys to make only (scancode set 3 only)
    MakeOnly,

    /// Set all keys to typematic/autorepeat/make/release (scancode set 3 only)
    SetTypmaticMakeRelease,

    /// Set specific key to typematic/autorepeat only (scancode set 3 only)
    SetKeyTypmatic([u8; 7]),

    /// Set specific key to make/release (scancode set 3 only)
    SetKeyMakeRelease([u8; 7]),

    /// Set specific key to make only (scancode set 3 only)
    SetKeyMakeOnly([u8; 7]),

    /// Requests device resend last byte
    Resend,

    /// Resets the device and runs Built In Self Test.
    ///
    /// Returns [Response::Resend] or [Response::Ack] followed by [Response::BISTSuccess] or [Response::BISTFailure].
    ResetAndBIST,
}

impl Command {
    /// Returns the raw command byte of the command variant.
    const fn command_byte(&self) -> u8 {
        match self {
            Self::SetLed(_) => 0xed,
            Self::Echo => 0xee,
            Self::ScanCodeSet(_) => 0xf0,
            Self::IdentifyDevice => 0xf2,
            Self::RepeatCtl(_) => 0xf3,
            Self::EnableScan => 0xf4,
            Self::DisableScan => 0xf5,
            Self::SetDefaultParams => 0xf6,
            Self::SetTypmatic => 0xf7,
            Self::MakeRelease => 0xf8,
            Self::MakeOnly => 0xf9,
            Self::SetTypmaticMakeRelease => 0xfa,
            Self::SetKeyTypmatic(_) => 0xfb,
            Self::SetKeyMakeRelease(_) => 0xfc,
            Self::SetKeyMakeOnly(_) => 0xfd,
            Self::Resend => 0xfe,
            Self::ResetAndBIST => 0xff,
        }
    }

    /// Returns a command iter which iterates over each byte in the command.
    pub fn raw_iter(&self) -> CommandIter {
        CommandIter {
            index: 0,
            command: self,
        }
    }
}

bitflags::bitflags! {

    /// Keyboard LEDs for [Command::SetLed].
    ///
    /// These are not restricted to bits 0-2, devices may have more than these 3, however are not specifically defined.
    #[derive(Copy, Clone, Debug)]
    pub struct KeyboardLed: u8 {
        const SCROLL_LOCK = 1;
        const NUM_LOCK = 1 << 1;
        const CAPS_LOCK = 1 << 2;
    }
}

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum ScanCodeSetSubcommand {
    GetScanCode = 0x00,
    SetScancodeSetOne,
    SetScancodeSetTwo,
    SetScancodeSetThree,
}

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum Response {
    Ack = 0xfa,
    Resend = 0xfe,
    Echo = 0xee,
    BISTSuccess = 0xaa,
    BISTFailure = 0xfc,
    Error = 0xff,
}

impl TryFrom<u8> for Response {
    type Error = UnknownResponse;
    fn try_from(value: u8) -> Result<Self, UnknownResponse> {
        match value {
            0x00 | 0xff => Ok(Response::Error),
            0xfa => Ok(Response::Ack),
            0xfe => Ok(Response::Resend),
            0xee => Ok(Response::Echo),
            0xaa => Ok(Response::BISTSuccess),
            0xfc | 0xfd => Ok(Response::BISTFailure),
            _ => Err(UnknownResponse),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct UnknownResponse;

#[derive(Copy, Clone)]
pub struct RepeatCtl(u8);

impl RepeatCtl {
    /// Data for [Command::RepeatCtl] command.
    ///
    /// The rate value does not allow values above `31` (`0x1f`).
    /// The rate is a gradient from 30Hz (`0`) to 2Hz (`31`).
    /// Rate values are as follows.
    ///
    /// | value | rate |
    /// |-------|------|
    /// |   0   | 30.3 |
    /// |   1   | 27.0 |
    /// |   2   | 23.8 |
    /// |   3   | 21.7 |
    /// |   4   | 20.0 |
    /// |   5   | 18.5 |
    /// |   6   | 17.2 |
    /// |   7   | 15.9 |
    /// |   8   | 14.9 |
    /// |   9   | 13.3 |
    /// |   10  | 12.0 |
    /// |   11  | 10.9 |
    /// |   12  | 10.0 |
    /// |   13  |  9.2 |
    /// |   14  |  8.6 |
    /// |   15  |  8.0 |
    /// |   16  |  7.5 |
    /// |   17  |  6.7 |
    /// |   18  |  6.0 |
    /// |   19  |  5.5 |
    /// |   20  |  5.0 |
    /// |   21  |  4.6 |
    /// |   22  |  4.3 |
    /// |   23  |  4.0 |
    /// |   24  |  3.7 |
    /// |   25  |  3.3 |
    /// |   26  |  3.0 |
    /// |   27  |  2.7 |
    /// |   28  |  2.5 |
    /// |   29  |  2.3 |
    /// |   30  |  2.1 |
    /// |   31  |  2.0 |
    ///
    /// # Panics
    ///
    /// This fn will panic if `rate` is greater than 31.
    pub const fn new(rate: u8, delay: RepeatDelay) -> Self {
        assert!(rate & 0xe0 == 0, "Repeat rate must be below 32");

        Self((delay as u8) << 5 | rate)
    }
}

impl core::fmt::Debug for RepeatCtl {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let reprate = match self.0 >> 4 {
            0 => RepeatDelay::Milis250,
            1 => RepeatDelay::Milis500,
            2 => RepeatDelay::Milis750,
            3 => RepeatDelay::Milis1000,
            _ => panic!(
                "{} had invalid value {}",
                core::any::type_name::<Self>(),
                self.0
            ),
        };
        f.debug_struct("RepeatCtl")
            .field("rate", &(self.0 & 0x1f))
            .field("delay", &reprate)
            .finish()
    }
}

/// Repeat delay in milliseconds.
#[derive(Copy, Clone, Debug)]
pub enum RepeatDelay {
    Milis250 = 0,
    Milis500 = 1,
    Milis750 = 2,
    Milis1000 = 3,
}

pub struct CommandIter<'a> {
    index: usize,
    command: &'a Command,
}

impl<'a> Iterator for CommandIter<'a> {
    type Item = u8;
    fn next(&mut self) -> Option<Self::Item> {
        let index = self.index;
        self.index += 1;

        if index == 0 {
            return Some(self.command.command_byte());
        }

        match self.command {
            Command::SetLed(p) if index == 1 => Some(p.bits()),
            Command::RepeatCtl(p) if index == 1 => Some(p.0),
            Command::ScanCodeSet(p) if index == 1 => Some(*p as u8),
            Command::SetKeyMakeOnly(p)
            | Command::SetKeyTypmatic(p)
            | Command::SetKeyMakeRelease(p)
                if p[index - 1] != 0 =>
            {
                p.get(index - 1).map(|i| *i)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_ctl_raw_ok() {
        assert_eq!(RepeatCtl::new(0, RepeatDelay::Milis250).0, 0);
        assert_eq!(RepeatCtl::new(1, RepeatDelay::Milis250).0, 1);
        assert_eq!(RepeatCtl::new(31, RepeatDelay::Milis250).0, 31);

        assert_eq!(RepeatCtl::new(0, RepeatDelay::Milis500).0, 0x20);
        assert_eq!(RepeatCtl::new(0, RepeatDelay::Milis1000).0, 0x60);
        assert_eq!(RepeatCtl::new(1, RepeatDelay::Milis500).0, 0x21);
        assert_eq!(RepeatCtl::new(31, RepeatDelay::Milis1000).0, 0x7f);
    }

    #[test]
    #[should_panic]
    fn bad_repeat_ctl() {
        assert_eq!(RepeatCtl::new(32, RepeatDelay::Milis250).0, 0);
    }

    #[test]
    fn raw_command_byte() {
        let cmd = Command::Echo;
        let v: Vec<_> = cmd.raw_iter().collect();

        assert_eq!(v, [0xee]);
    }

    #[test]
    fn raw_command_full_str_ok() {
        let cmd = Command::SetKeyTypmatic([1, 2, 3, 4, 5, 6, 7]);

        let v: Vec<_> = cmd.raw_iter().collect();

        assert_eq!(v, [0xfb, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn not_full_scancode() {
        let cmd = Command::SetKeyTypmatic([1, 2, 3, 0, 0, 0, 0]);
        let len = cmd.raw_iter().count();
        assert_eq!(len, 4);
    }

    #[test]
    fn raw_cmd_with_byte_payload() {
        let led = Command::SetLed(
            KeyboardLed::CAPS_LOCK | KeyboardLed::SCROLL_LOCK | KeyboardLed::NUM_LOCK,
        );
        let led_vec: Vec<_> = led.raw_iter().collect();
        assert_eq!(led_vec, [0xed, 7]);

        let scan_set = Command::ScanCodeSet(ScanCodeSetSubcommand::SetScancodeSetThree);
        let scan_set_vec: Vec<_> = scan_set.raw_iter().collect();
        assert_eq!(scan_set_vec, [0xf0, 3]);

        let rep = Command::RepeatCtl(RepeatCtl::new(1, RepeatDelay::Milis500));
        let rep_vec: Vec<_> = rep.raw_iter().collect();
        assert_eq!(rep_vec, [0xf3, 0x21]);
    }
}
