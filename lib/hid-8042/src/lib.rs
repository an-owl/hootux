#![cfg_attr(not(test), no_std)]
extern crate alloc;

pub mod controller;
pub mod keyboard;

pub enum DeviceType {
    /// Very old keyboard which cannot identify itself.
    ///
    // The actual code is not 0xfe, this is the resend byte which is otherwise unused.
    AncientIBMKeyboard,

    /// Two button mouse.
    StandardMouse,

    /// Mouse with 2 buttons and scroll wheel
    MouseWithScrollWheel,

    Mouse5Button,
    MF2Keyboard,

    /// Think pad, IBM spacesaver, other short keyboards.
    //
    // I looked them up, they seem pretty big to me.
    ShortKeyboard,

    // They seem kinda similar in terms of layout.
    Ncd97Or122HostConnectKeyboard,

    /// 122 Key keyboard
    Key122,

    JapaneseGKeyboard,
    JapanesePKeyboard,
    JapaneseAKeyboard,

    NCDSunKeyboard,

    Unknown,
}

impl DeviceType {
    /// Returns whether we believe this device is a keyboard.
    // We require is_kb and is_rodent because Self::Unknown is neither.
    pub const fn is_keyboard(&self) -> bool {
        match self {
            Self::StandardMouse
            | Self::MouseWithScrollWheel
            | Self::Mouse5Button
            | Self::Unknown => false,
            _ => true,
        }
    }

    /// Returns whether we believe this device is a rodent.
    ///
    /// We do not have enough information to determine if this is a mouse or a hamster.
    /// However, this is a PS/2 device so it is almost definitely a mouse we do not want to assume,
    /// but it should be fine to treat this as a mouse regardless.
    ///
    /// If the caller can determine that the device is their mother then it is a hamster.
    pub const fn is_rodent(&self) -> bool {
        match self {
            Self::StandardMouse | Self::MouseWithScrollWheel | Self::Mouse5Button => true,
            _ => false,
        }
    }
}

impl TryFrom<[u8; 2]> for DeviceType {
    type Error = DeviceType;

    fn try_from(value: [u8; 2]) -> Result<Self, Self::Error> {
        match value {
            [0, _] => Ok(DeviceType::StandardMouse),
            [3, _] => Ok(DeviceType::MouseWithScrollWheel),
            [4, _] => Ok(DeviceType::Mouse5Button),
            [0xab, 0xa3] => Ok(DeviceType::MF2Keyboard),
            [0xab, 0xc1] => Ok(DeviceType::MF2Keyboard),
            [0xab, 0x84] => Ok(DeviceType::ShortKeyboard),
            [0xab, 0x85] => Ok(DeviceType::Ncd97Or122HostConnectKeyboard),
            [0xab, 0x86] => Ok(DeviceType::Key122),
            [0xab, 0x90] => Ok(DeviceType::JapaneseGKeyboard),
            [0xab, 0x91] => Ok(DeviceType::JapanesePKeyboard),
            [0xab, 0x92] => Ok(DeviceType::JapaneseAKeyboard),
            [0xac, 0xa1] => Ok(DeviceType::NCDSunKeyboard),
            _ => Err(DeviceType::Unknown),
        }
    }
}
