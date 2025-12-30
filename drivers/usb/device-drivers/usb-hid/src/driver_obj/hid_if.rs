use crate::descriptors::UsagePage;
use crate::driver_obj::HidPipeInner;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ops::BitXor;
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use hid::keyboard::{ControlKey, KeyGroup, KeyState, ModifierKey};
use hidreport::Report;
use hidreport::{Field, ReportDescriptor};
use hootux::fs::IoError;

pub(crate) fn resolve_interfaces(
    descriptor: ReportDescriptor,
) -> Result<BTreeMap<UsagePage, Box<dyn HidInterface>>, IoError> {
    let mut collection = BTreeMap::new();

    if let Some(interface) = BaseKeyboardIf::new(&descriptor) {
        let None = collection.insert(
            interface.usage_page(),
            Box::new(interface) as Box<dyn HidInterface>,
        ) else {
            unreachable!()
        };
    }

    Ok(collection)
}

/// This trait defines an object which can parse report data and forward it to a file object.
pub(crate) trait HidInterface {
    fn usage_page(&self) -> UsagePage;

    fn interface_number(&self) -> u32;

    /// Parse the report given in `report` and forward parsed data into `stream`.
    /// `stream` will be given pre-opened in write only mode.
    fn parse_report<'a>(
        &'a mut self,
        report: &'a [u8],
        stream: &'a HidPipeInner,
    ) -> BoxFuture<'a, Result<(), IoError>>;
}

/// Handles key input.
struct BaseKeyboardIf {
    modifier_keys: Vec<hidreport::VariableField>,
    main_keys: hidreport::ArrayField,

    state_machine: KeyboardStateMachine,
}

impl BaseKeyboardIf {
    fn new(report: &ReportDescriptor) -> Option<Self> {
        let mut variable_fields = Vec::new();
        let mut array_field = None;

        for reports in report.input_reports() {
            for field in reports.fields() {
                match field {
                    Field::Variable(field_distinct)
                        if Into::<u16>::into(field_distinct.usage.usage_page)
                            == Into::<u16>::into(UsagePage::KeyboardKeypad) =>
                    {
                        variable_fields.push(field_distinct.clone());
                    }

                    Field::Array(field_distinct) => {
                        if Into::<u16>::into(field_distinct.usage_range().minimum().usage_page())
                            == Into::<u16>::into(UsagePage::KeyboardKeypad)
                        {
                            array_field = Some(field_distinct.clone())
                        }
                    }

                    _ => log::trace!("Unhandled field {:?}", field),
                }
            }
        }

        if let Some(array_field) = array_field
            && !variable_fields.is_empty()
        {
            log::info!("Found keyboard/keypad report for device.");
            log::trace!("Variable fields: {:?}", variable_fields.len());
            Some(Self {
                modifier_keys: variable_fields,
                main_keys: array_field,
                state_machine: KeyboardStateMachine::empty(),
            })
        } else {
            None
        }
    }
}

impl HidInterface for BaseKeyboardIf {
    fn usage_page(&self) -> UsagePage {
        UsagePage::KeyboardKeypad
    }

    fn interface_number(&self) -> u32 {
        hid::KEYBOARD
    }

    fn parse_report<'a>(
        &'a mut self,
        report: &'a [u8],
        stream: &'a HidPipeInner,
    ) -> BoxFuture<'a, Result<(), IoError>> {
        async {
            let mut curr_state = KeyboardStateMachine::empty();
            for field in &self.modifier_keys {
                let Ok(data) = field.extract(report) else {
                    continue;
                };
                let data: u32 = data.into();
                let state = data != 1;
                if state {
                    curr_state.set_bit(Into::<u16>::into(field.usage.usage_id) as u8);
                }
            }

            for i in 0..self.main_keys.report_count.into() {
                let Ok(t) = self.main_keys.extract_one(report, i) else {
                    break;
                };
                let key: u32 = t.into();
                curr_state.set_bit(key as u8);
            }

            let prev_state = core::mem::replace(&mut self.state_machine, curr_state);

            for (pos, key) in prev_state.into_update_iter(&self.state_machine) {
                let keycode = match usb_2_hid(key as u16) {
                    Ok(keycode) => keycode,
                    Err(e) => {
                        log::error!("Got {e:?} from keyboard");
                        return Err(e);
                    }
                };
                let char = hid::keyboard::KeyChar {
                    key_group: keycode,
                    state: if pos {
                        KeyState::Pressed
                    } else {
                        KeyState::Released
                    },
                };
                stream
                    .send_op(&char.into_bytes(), self.interface_number())
                    .await;
            }
            Ok(())
        }
        .boxed()
    }
}

#[derive(Debug, Copy, Clone)]
struct KeyboardStateMachine([u128; 2]);

impl KeyboardStateMachine {
    const fn empty() -> Self {
        Self([0; 2])
    }

    /// USB does not pass state changes like PS/2, it gives us the whole state each time.
    /// So we do not have an "update" fn. Instead, we just rebuild the whole state, and compare
    /// the old and new states to send it down the pipe.
    fn from_iter(iter: impl IntoIterator<Item = u8>) -> Self {
        let mut this = Self::empty();

        for mut char in iter {
            let word = if char > 128 {
                char -= 128;
                &mut this.0[1]
            } else {
                &mut this.0[0]
            };

            *word |= 1 << char;
        }

        this
    }

    /// Sets the first one bit to zero, returns its position.
    fn flip_first_bit(&mut self) -> Option<u8> {
        let pos = self.0[0].trailing_zeros();
        if pos == u128::BITS {
            let pos = self.0[1].trailing_zeros();
            if pos == u128::BITS {
                None
            } else {
                self.0[1] ^= 1 << pos;
                Some((pos + u128::BITS) as u8)
            }
        } else {
            self.0[0] ^= 1 << pos;
            Some(pos as u8)
        }
    }

    /// Returns the value of the bit.
    fn get_bit(&self, mut index: u8) -> bool {
        let word = if index > 128 {
            index -= 128;
            self.0[1]
        } else {
            self.0[0]
        };

        word & (1 << index) != 0
    }

    fn set_bit(&mut self, mut index: u8) {
        let word = if index > 128 {
            index -= 128;
            &mut self.0[1]
        } else {
            &mut self.0[0]
        };

        *word |= 1 << index;
    }

    fn into_update_iter(self, new_state: &Self) -> KeyboardStateIter {
        let diff = self ^ *new_state;
        KeyboardStateIter {
            diff,
            state: *new_state,
        }
    }
}

impl BitXor for KeyboardStateMachine {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self {
        Self([self.0[0] ^ rhs.0[0], self.0[1] ^ rhs.0[1]])
    }
}

struct KeyboardStateIter {
    diff: KeyboardStateMachine,
    state: KeyboardStateMachine,
}

impl Iterator for KeyboardStateIter {
    type Item = (bool, u8);
    fn next(&mut self) -> Option<Self::Item> {
        let pos = self.diff.flip_first_bit()?;
        Some((self.state.get_bit(pos), pos))
    }
}

/// Translates a key defined in the USB HID usages and description into a [hid::keyboard::KeyState].
/// Some errors given must be handled internally others must be retuned via the pipe.
///
/// - [IoError::InvalidData]: Is returned when a reserved character is passed to `usb_key`.
/// - [IoError::Busy]: Is returned when "ErrorUndefined" is given. The state machine should be
/// rolled back to before this character was received.
/// - [IoError::MediaError]: Is returned when the "POSTFail" scancode is received. This should be forwarded through the pipe.
/// Specification doesn't say, but presumably we would no longer receive any data.
///
fn usb_2_hid(usb_key: u16) -> Result<KeyGroup, IoError> {
    match usb_key {
        n @ 0 => {
            log::error!("usb_2_hid() was called with a reserved scancode {}", n);
            Err(IoError::InvalidData)
        }
        1 => {
            log::warn!("GotErrorRollover");
            Err(IoError::Busy)
        }
        2 => {
            log::error!("Got POSTFail");
            Err(IoError::MediaError)
        }
        char_key @ 4..=0x1d => {
            const CHAR_OFFSET: u16 = ('A' as u16) - 4;
            Ok(KeyGroup::MainBody((CHAR_OFFSET + char_key) as u8 as char))
        }
        num_key @ 0x1e..=0x26 => {
            const NUM_OFFSET: u16 = ('1' as u16) - 0x1e;
            Ok(KeyGroup::MainBody((NUM_OFFSET + num_key) as u8 as char))
        }
        0x27 => Ok(KeyGroup::MainBody('0')),
        0x28 => Ok(KeyGroup::MainBody('\n')),
        0x29 => Ok(KeyGroup::ControlKey(ControlKey::Escape)),
        0x2a => Ok(KeyGroup::MainBody('\x08')),
        0x2b => Ok(KeyGroup::MainBody('\t')),
        0x2c => Ok(KeyGroup::MainBody(' ')),
        0x2d => Ok(KeyGroup::MainBody('-')),
        0x2e => Ok(KeyGroup::MainBody('=')),
        0x2f => Ok(KeyGroup::MainBody('[')),
        0x30 => Ok(KeyGroup::MainBody(']')),
        0x31 => Ok(KeyGroup::MainBody('\\')),
        0x32 => Ok(KeyGroup::MainBody('#')), // Note this is not present on EN keyboards, This definition is using a german keyboard.
        0x33 => Ok(KeyGroup::MainBody(';')),
        0x34 => Ok(KeyGroup::MainBody('\'')),
        0x35 => Ok(KeyGroup::MainBody('`')),
        0x36 => Ok(KeyGroup::MainBody(',')),
        0x37 => Ok(KeyGroup::MainBody('.')),
        0x38 => Ok(KeyGroup::MainBody('/')),
        0x39 => Ok(KeyGroup::ControlKey(ControlKey::CapsLock)),
        n @ 0x3a..=0x45 => {
            let n = n - 0x39; // 0x3a = F1
            // SAFETY: n can never be 0
            unsafe {
                Ok(KeyGroup::FunctionKey(
                    (n as u32).try_into().unwrap_unchecked(),
                ))
            }
        }
        0x46 => Ok(KeyGroup::ControlKey(ControlKey::PrintScreen)),
        0x47 => Ok(KeyGroup::ControlKey(ControlKey::ScrollLock)),
        0x48 => Ok(KeyGroup::ControlKey(ControlKey::Pause)),
        0x49 => Ok(KeyGroup::ControlKey(ControlKey::Insert)),
        0x4a => Ok(KeyGroup::ControlKey(ControlKey::Home)),
        0x4b => Ok(KeyGroup::ControlKey(ControlKey::PageUp)),
        0x4c => Ok(KeyGroup::MainBody('\u{177}')), // ASCII delete
        0x4d => Ok(KeyGroup::ControlKey(ControlKey::End)),
        0x4e => Ok(KeyGroup::ControlKey(ControlKey::PageDown)),
        0x4f => Ok(KeyGroup::ControlKey(ControlKey::Right)),
        0x50 => Ok(KeyGroup::ControlKey(ControlKey::Left)),
        0x51 => Ok(KeyGroup::ControlKey(ControlKey::Down)),
        0x52 => Ok(KeyGroup::ControlKey(ControlKey::Up)),
        0x53 => Ok(KeyGroup::ControlKey(ControlKey::NumLock)),
        0x54 => Ok(KeyGroup::Keypad('/' as u32)),
        0x55 => Ok(KeyGroup::Keypad('*' as u32)),
        0x56 => Ok(KeyGroup::Keypad('-' as u32)),
        0x57 => Ok(KeyGroup::Keypad('+' as u32)),
        0x58 => Ok(KeyGroup::Keypad('\n' as u32)),
        n @ 0x59..=0x61 => {
            let n = n - 0x58; // 0x59 == '1'
            Ok(KeyGroup::Keypad(n as u32))
        }
        0x62 => Ok(KeyGroup::Keypad('0' as u32)),
        0x63 => Ok(KeyGroup::Keypad('.' as u32)),
        0x64 => Ok(KeyGroup::MainBody('∖')), // This is a different backslash from '\u{92}' this is '\u{8726}'
        0x65 => Ok(KeyGroup::ControlKey(ControlKey::Application)),
        0x66 => Ok(KeyGroup::ControlKey(ControlKey::Power)),
        0x67 => Ok(KeyGroup::Keypad('=' as u32)),
        n @ 0x68..=0x73 => {
            let n = n - (0x68 + 13);
            // SAFETY: This is safe, this can never be `0`
            unsafe {
                Ok(KeyGroup::FunctionKey(
                    (n as u32).try_into().unwrap_unchecked(),
                ))
            }
        }
        0x74 => Ok(KeyGroup::ControlKey(ControlKey::Execute)),
        0x75 => Ok(KeyGroup::ControlKey(ControlKey::Help)),
        0x76 => Ok(KeyGroup::ControlKey(ControlKey::Menu)),
        0x77 => Ok(KeyGroup::ControlKey(ControlKey::Select)),
        0x78 => Ok(KeyGroup::ControlKey(ControlKey::Stop)),
        0x79 => Ok(KeyGroup::ControlKey(ControlKey::Again)),
        0x7a => Ok(KeyGroup::ControlKey(ControlKey::Undo)),
        0x7b => Ok(KeyGroup::ControlKey(ControlKey::Cut)),
        0x7c => Ok(KeyGroup::ControlKey(ControlKey::Copy)),
        0x7d => Ok(KeyGroup::ControlKey(ControlKey::Paste)),
        0x7e => Ok(KeyGroup::ControlKey(ControlKey::Find)),
        0x7f => Ok(KeyGroup::ControlKey(ControlKey::Mute)),
        0x80 => Ok(KeyGroup::ControlKey(ControlKey::VolumeUp)),
        0x81 => Ok(KeyGroup::ControlKey(ControlKey::VolumeDown)),
        0x82 => Ok(KeyGroup::ControlKey(ControlKey::LockingCapsLock)),
        0x83 => Ok(KeyGroup::ControlKey(ControlKey::LockingNumLock)),
        0x84 => Ok(KeyGroup::ControlKey(ControlKey::LockingScrollLock)),
        0x85 => Ok(KeyGroup::Keypad(',' as u32)),
        0x86 => Ok(KeyGroup::Keypad('=' as u32)),
        0x87 => Ok(KeyGroup::ControlKey(ControlKey::International1)),
        0x88 => Ok(KeyGroup::ControlKey(ControlKey::International2)),
        0x89 => Ok(KeyGroup::ControlKey(ControlKey::International3)),
        0x8a => Ok(KeyGroup::ControlKey(ControlKey::International4)),
        0x8b => Ok(KeyGroup::ControlKey(ControlKey::International5)),
        0x8c => Ok(KeyGroup::ControlKey(ControlKey::International6)),
        0x8d => Ok(KeyGroup::ControlKey(ControlKey::International7)),
        0x8e => Ok(KeyGroup::ControlKey(ControlKey::International8)),
        0x8f => Ok(KeyGroup::ControlKey(ControlKey::International9)),
        0x90 => Ok(KeyGroup::ControlKey(ControlKey::Lang1)),
        0x91 => Ok(KeyGroup::ControlKey(ControlKey::Lang2)),
        0x92 => Ok(KeyGroup::ControlKey(ControlKey::Lang3)),
        0x93 => Ok(KeyGroup::ControlKey(ControlKey::Lang4)),
        0x94 => Ok(KeyGroup::ControlKey(ControlKey::Lang5)),
        0x95 => Ok(KeyGroup::ControlKey(ControlKey::Lang6)),
        0x96 => Ok(KeyGroup::ControlKey(ControlKey::Lang7)),
        0x97 => Ok(KeyGroup::ControlKey(ControlKey::Lang8)),
        0x98 => Ok(KeyGroup::ControlKey(ControlKey::Lang9)),
        0x99 => Ok(KeyGroup::ControlKey(ControlKey::AlternateErase)),
        0x9a => Ok(KeyGroup::ControlKey(ControlKey::SysReq)),
        0x9b => Ok(KeyGroup::ControlKey(ControlKey::Cancel)),
        0x9c => Ok(KeyGroup::ControlKey(ControlKey::Clear)),
        0x9d => Ok(KeyGroup::ControlKey(ControlKey::Prior)),
        0x9e => Ok(KeyGroup::ControlKey(ControlKey::Return)),
        0x9f => Ok(KeyGroup::ControlKey(ControlKey::Seperator)),
        0xa0 => Ok(KeyGroup::ControlKey(ControlKey::Out)),
        0xa1 => Ok(KeyGroup::ControlKey(ControlKey::Oper)),
        0xa2 => Ok(KeyGroup::ControlKey(ControlKey::Clear)),
        0xa3 => Ok(KeyGroup::ControlKey(ControlKey::CrSel)),
        0xa4 => Ok(KeyGroup::ControlKey(ControlKey::ExSel)),
        0xa5..=0xaf => Err(IoError::InvalidData),
        0xb0 => Ok(KeyGroup::ControlKey(ControlKey::Hundreds)),
        0xb1 => Ok(KeyGroup::ControlKey(ControlKey::Thousands)),
        0xb2 => Ok(KeyGroup::ControlKey(ControlKey::ThousandsSeperator)),
        0xb3 => Ok(KeyGroup::ControlKey(ControlKey::DecimalSeperator)),
        0xb4 => Ok(KeyGroup::ControlKey(ControlKey::CurrencyUnit)),
        0xb5 => Ok(KeyGroup::ControlKey(ControlKey::CurrencySubUnit)),
        0xb6 => Ok(KeyGroup::Keypad('(' as u32)),
        0xb7 => Ok(KeyGroup::Keypad(')' as u32)),
        0xb8 => Ok(KeyGroup::Keypad('{' as u32)),
        0xb9 => Ok(KeyGroup::Keypad('}' as u32)),
        0xba => Ok(KeyGroup::Keypad('\t' as u32)),
        0xbb => Ok(KeyGroup::Keypad('\u{8}' as u32)),
        n @ 0xbc..=0xc1 => {
            let n = (n as u8) - 0xbd + 'A' as u8;
            Ok(KeyGroup::Keypad(n as u32))
        }
        0xc2 => Ok(KeyGroup::Keypad('⊕' as u32)),
        0xc3 => Ok(KeyGroup::Keypad('^' as u32)),
        0xc4 => Ok(KeyGroup::Keypad('%' as u32)),
        0xc5 => Ok(KeyGroup::Keypad('<' as u32)),
        0xc6 => Ok(KeyGroup::Keypad('>' as u32)),
        0xc7 => Ok(KeyGroup::Keypad('&' as u32)),
        0xc8 => Ok(KeyGroup::Keypad('∧' as u32)), // Logical and "&&" in the spec.
        0xc9 => Ok(KeyGroup::Keypad('|' as u32)),
        0xca => Ok(KeyGroup::Keypad('∨' as u32)),
        0xcb => Ok(KeyGroup::Keypad(':' as u32)),
        0xcc => Ok(KeyGroup::Keypad('#' as u32)),
        0xcd => Ok(KeyGroup::Keypad(' ' as u32)), // Why does the keypad need a ' '?
        0xce => Ok(KeyGroup::Keypad('@' as u32)),
        0xcf => Ok(KeyGroup::Keypad('!' as u32)),
        0xd0 => Ok(KeyGroup::Keypad(
            ControlKey::MemoryStore as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xd1 => Ok(KeyGroup::Keypad(
            ControlKey::MemoryRecall as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xd2 => Ok(KeyGroup::Keypad(
            ControlKey::MemoryClear as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xd3 => Ok(KeyGroup::Keypad(
            ControlKey::MemoryAdd as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xd4 => Ok(KeyGroup::Keypad(
            ControlKey::MemorySub as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xd5 => Ok(KeyGroup::Keypad(
            ControlKey::MemoryMultiply as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xd6 => Ok(KeyGroup::Keypad(
            ControlKey::MemoryDivide as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xd7 => Ok(KeyGroup::Keypad('±' as u32)),
        0xd8 => Ok(KeyGroup::Keypad(
            ControlKey::Clear as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xd9 => Ok(KeyGroup::Keypad(
            ControlKey::ClearEntry as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xda => Ok(KeyGroup::Keypad(
            ControlKey::Binary as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xdb => Ok(KeyGroup::Keypad(
            ControlKey::Octal as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xdc => Ok(KeyGroup::Keypad(
            ControlKey::Decimal as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xdd => Ok(KeyGroup::Keypad(
            ControlKey::Hexadecimal as u32 + ControlKey::KEYPAD_OFFSET,
        )),
        0xde..=0xdf => Err(IoError::InvalidData),
        0xe0 => Ok(KeyGroup::Modifier(ModifierKey::LeftControl)),
        0xe1 => Ok(KeyGroup::Modifier(ModifierKey::LeftShift)),
        0xe2 => Ok(KeyGroup::Modifier(ModifierKey::LeftAlt)),
        0xe3 => Ok(KeyGroup::Modifier(ModifierKey::LeftSuper)),
        0xe4 => Ok(KeyGroup::Modifier(ModifierKey::RightControl)),
        0xe5 => Ok(KeyGroup::Modifier(ModifierKey::RightShift)),
        0xe6 => Ok(KeyGroup::Modifier(ModifierKey::AltRight)),
        0xe7 => Ok(KeyGroup::Modifier(ModifierKey::RightSuper)),
        0xe8..=0xffff | 3 => Err(IoError::InvalidData),
    }
}
