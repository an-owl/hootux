//! The Keyboard format uses a delta style formatting, where a key code is sent and extra data
//! indicating its position.
//! All key definitions are based on the key positions in an EN-global keyboard.
//!
//! The packet format for this stream shall be a [KeyGroup] followed by the key state.
//! The key state may be any format and should represent the key position as closely to the keyboards output.
//! When the state format is not [crate::query::DataType::Boolean] the state should be expanded to
//! the full data range of the format, the value should not be normalized.

#[repr(u8)]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum KeyState {
    Released = 0,
    Pressed = 1,
}

#[repr(u8)]
#[derive(Ord, PartialOrd, Eq, PartialEq, Copy, Clone, Debug)]
pub enum KeyGroup {
    /// Keys which have a Unicode representation when not modified and not marked as belonging to the keypad.
    /// Only unmodified keys may be returned via this type e.g. the 'c' key will never output 'C'.
    MainBody(char),

    /// Keys which do not have a Unicode representation must use this type.
    ControlKey(ControlKey),

    /// Keypad keys are represented as "modified", "1/"End" will be given as "1".
    ///
    /// Non-unicode keys are represented as a [ControlKey] offset by `0x110000`.
    /// So the value `0x110001` is left arrow key.
    ///
    /// Keypad includes hexadecimal keys, these should be upper case.
    Keypad(u32),

    /// Followed by nonnull-integer to indicate key.
    FunctionKey(core::num::NonZeroU32),

    /// Modifier keys as defined in the USB HID usage tables section.
    ///
    /// Codes are integers 0-7 from left to right.
    Modifier(ModifierKey),

    /// Extra keys undefined in this specification.
    // todo: How to resolve a purpose or name for these keys.
    Extra(u32),
}

#[repr(u32)]
#[derive(Ord, PartialOrd, Eq, PartialEq, Copy, Clone, Debug)]
pub enum ModifierKey {
    LeftControl,
    LeftShift,
    LeftAlt,
    LeftSuper,

    RightControl,
    AltRight,
    RightSuper,
    RightShift,
}

#[repr(u32)]
#[derive(Ord, PartialOrd, Eq, PartialEq, Copy, Clone, Debug)]
pub enum ControlKey {
    Escape,

    Left,
    Right,
    Up,
    Down,

    Insert,
    PageUp,
    PageDown,
    Home,
    End,

    PrintScreen,
    Pause,

    NumLock,
    ScrollLock,
    CapsLock,

    Execute,
    Help,
    Menu,
    Select,
    Stop,
    Again,
    Undo,
    Cut,
    Copy,
    Paste,
    Find,

    Mute,
    VolumeUp,
    VolumeDown,

    International1,
    International2,
    International3,
    International4,
    International5,
    International6,
    International7,
    International8,
    International9,
    Lang1,
    Lang2,
    Lang3,
    Lang4,
    Lang5,
    Lang6,
    Lang7,
    Lang8,
    Lang9,

    AlternateErase,

    // Keypad only.
    // Note that I'm Going down the list of USB HID usages
    // XOR is here, But XOR should be ⊻ \u{22bb}
    MemoryStore = 0x1_0000,
    MemoryRecall,
    MemoryClear,
    MemoryAdd,
    MemorySub,
    MemoryMultiply,
    MemoryDivide,

    Clear,
    ClearEntry,

    Binary,
    Octal,
    Decimal,
    Hexadecimal,
}

impl ControlKey {
    pub const KEYPAD_OFFSET: u32 = 0x11_0000;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_modifier_repr() {
        assert_eq!(0, ModifierKey::LeftControl as u32);
        assert_eq!(7, ModifierKey::RightShift as u32);
    }

    #[test]
    fn check_unicode_decode() {
        let key = KeyGroup::MainBody('a');
        let encoded = unsafe { *(&raw const key).cast::<[u8; size_of::<KeyGroup>()]>() };
        assert_eq!([0, 0, 0, 0, b"a"[0], 0, 0, 0], encoded);
    }

    #[test]
    fn decode_keypad_special() {
        let key = KeyGroup::Keypad(ControlKey::MemoryAdd as u32 + ControlKey::KEYPAD_OFFSET);
        let encoded = unsafe { *(&raw const key).cast::<[u8; size_of::<KeyGroup>()]>() };
        assert_eq!(2, u8::from(encoded[0]));
        assert_eq!(
            ControlKey::MemoryAdd as u32 + ControlKey::KEYPAD_OFFSET,
            u32::from_le_bytes(encoded[4..8].try_into().unwrap())
        );
    }
}
