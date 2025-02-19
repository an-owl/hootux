#[no_std]
extern crate alloc;

mod controller;
mod keyboard;

enum DeviceType {
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

#[derive(Copy, Clone, Debug)]
struct BadBuffer;

impl TryFrom<&[u8]> for DeviceType {
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
