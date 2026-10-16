//! Mac virtual keycodes (`kVK_*`, HIToolbox Events.h) -> evdev, by physical position.

use super::evdev::*;
use super::keysym::{ModKey, Modifier};

pub const KVK_FUNCTION: u16 = 0x3f;

pub fn modifier(kvk: u16) -> Option<Modifier> {
    let (key, right) = match kvk {
        0x38 => (ModKey::Shift, false),
        0x3c => (ModKey::Shift, true),
        0x3b => (ModKey::Control, false),
        0x3e => (ModKey::Control, true),
        0x37 => (ModKey::Command, false),
        0x36 => (ModKey::Command, true),
        0x3a => (ModKey::Option, false),
        0x3d => (ModKey::Option, true),
        0x39 => (ModKey::CapsLock, false),
        _ => return None,
    };
    Some(Modifier { key, right })
}

/// Non-modifier keys. `kVK_Function` has no Linux equivalent and maps to `None`.
pub fn to_evdev(kvk: u16) -> Option<u32> {
    Some(match kvk {
        0x00 => KEY_A,
        0x01 => KEY_S,
        0x02 => KEY_D,
        0x03 => KEY_F,
        0x04 => KEY_H,
        0x05 => KEY_G,
        0x06 => KEY_Z,
        0x07 => KEY_X,
        0x08 => KEY_C,
        0x09 => KEY_V,
        0x0a => KEY_102ND,
        0x0b => KEY_B,
        0x0c => KEY_Q,
        0x0d => KEY_W,
        0x0e => KEY_E,
        0x0f => KEY_R,
        0x10 => KEY_Y,
        0x11 => KEY_T,
        0x12 => KEY_1,
        0x13 => KEY_2,
        0x14 => KEY_3,
        0x15 => KEY_4,
        0x16 => KEY_6,
        0x17 => KEY_5,
        0x18 => KEY_EQUAL,
        0x19 => KEY_9,
        0x1a => KEY_7,
        0x1b => KEY_MINUS,
        0x1c => KEY_8,
        0x1d => KEY_0,
        0x1e => KEY_RIGHTBRACE,
        0x1f => KEY_O,
        0x20 => KEY_U,
        0x21 => KEY_LEFTBRACE,
        0x22 => KEY_I,
        0x23 => KEY_P,
        0x24 => KEY_ENTER,
        0x25 => KEY_L,
        0x26 => KEY_J,
        0x27 => KEY_APOSTROPHE,
        0x28 => KEY_K,
        0x29 => KEY_SEMICOLON,
        0x2a => KEY_BACKSLASH,
        0x2b => KEY_COMMA,
        0x2c => KEY_SLASH,
        0x2d => KEY_N,
        0x2e => KEY_M,
        0x2f => KEY_DOT,
        0x30 => KEY_TAB,
        0x31 => KEY_SPACE,
        0x32 => KEY_GRAVE,
        0x33 => KEY_BACKSPACE,
        0x34 => KEY_KPENTER,
        0x35 => KEY_ESC,
        0x40 => KEY_F13 + 4,
        0x41 => KEY_KPDOT,
        0x43 => KEY_KPASTERISK,
        0x45 => KEY_KPPLUS,
        0x47 => KEY_NUMLOCK,
        0x48 => KEY_VOLUMEUP,
        0x49 => KEY_VOLUMEDOWN,
        0x4a => KEY_MUTE,
        0x4b => KEY_KPSLASH,
        0x4c => KEY_KPENTER,
        0x4e => KEY_KPMINUS,
        0x4f => KEY_F13 + 5,
        0x50 => KEY_F13 + 6,
        0x51 => KEY_KPEQUAL,
        0x52 => KEY_KP0,
        0x53 => KEY_KP1,
        0x54 => KEY_KP2,
        0x55 => KEY_KP3,
        0x56 => KEY_KP4,
        0x57 => KEY_KP5,
        0x58 => KEY_KP6,
        0x59 => KEY_KP7,
        0x5a => KEY_F13 + 7,
        0x5b => KEY_KP8,
        0x5c => KEY_KP9,
        0x5d => KEY_YEN,
        0x5e => KEY_RO,
        0x5f => KEY_KPCOMMA,
        0x60 => KEY_F5,
        0x61 => KEY_F6,
        0x62 => KEY_F7,
        0x63 => KEY_F3,
        0x64 => KEY_F8,
        0x65 => KEY_F9,
        0x66 => KEY_HANJA,
        0x67 => KEY_F11,
        0x68 => KEY_HANGEUL,
        0x69 => KEY_F13,
        0x6a => KEY_F13 + 3,
        0x6b => KEY_F13 + 1,
        0x6d => KEY_F10,
        0x6e => KEY_COMPOSE,
        0x6f => KEY_F12,
        0x71 => KEY_F13 + 2,
        0x72 => KEY_INSERT,
        0x73 => KEY_HOME,
        0x74 => KEY_PAGEUP,
        0x75 => KEY_DELETE,
        0x76 => KEY_F4,
        0x77 => KEY_END,
        0x78 => KEY_F2,
        0x79 => KEY_PAGEDOWN,
        0x7a => KEY_F1,
        0x7b => KEY_LEFT,
        0x7c => KEY_RIGHT,
        0x7d => KEY_DOWN,
        0x7e => KEY_UP,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_letters_by_position() {
        let qwerty = [
            (0x0c, KEY_Q),
            (0x0d, KEY_W),
            (0x00, KEY_A),
            (0x06, KEY_Z),
            (0x2e, KEY_M),
        ];
        for (kvk, code) in qwerty {
            assert_eq!(to_evdev(kvk), Some(code), "kVK {kvk:#x}");
        }
    }

    #[test]
    fn function_row_and_navigation() {
        let keys = [
            (0x7a, KEY_F1),
            (0x78, KEY_F2),
            (0x63, KEY_F3),
            (0x76, KEY_F4),
            (0x6d, KEY_F10),
            (0x67, KEY_F11),
            (0x6f, KEY_F12),
            (0x69, KEY_F13),
            (0x40, 187),
            (0x5a, 190),
            (0x33, KEY_BACKSPACE),
            (0x75, KEY_DELETE),
            (0x72, KEY_INSERT),
            (0x7e, KEY_UP),
        ];
        for (kvk, code) in keys {
            assert_eq!(to_evdev(kvk), Some(code), "kVK {kvk:#x}");
        }
    }

    #[test]
    fn modifiers_are_not_plain_keys() {
        for kvk in 0x36..=0x3e {
            assert!(modifier(kvk).is_some());
            assert_eq!(to_evdev(kvk), None);
        }
        assert_eq!(
            modifier(0x37),
            Some(Modifier {
                key: ModKey::Command,
                right: false
            })
        );
        assert_eq!(to_evdev(KVK_FUNCTION), None);
    }

    #[test]
    fn no_duplicate_targets_except_known() {
        let mut seen = std::collections::HashMap::new();
        for kvk in 0..0x80u16 {
            if let Some(code) = to_evdev(kvk)
                && let Some(prev) = seen.insert(code, kvk)
            {
                // kVK 0x34 (old PowerBook Enter) and 0x4c (keypad Enter) share KEY_KPENTER.
                assert_eq!((prev, kvk), (0x34, 0x4c), "evdev {code} used twice");
            }
        }
    }
}
