//! X11 keysym -> evdev for the client's keysym path (msg 4, and 16/23-12 without the keycode flag).

use super::evdev::*;

/// Modifier keys, before `ModifierConfig` decides which Linux modifier they become.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ModKey {
    Shift,
    Control,
    Command,
    Option,
    CapsLock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Modifier {
    pub key: ModKey,
    pub right: bool,
}

pub fn modifier(keysym: u32) -> Option<Modifier> {
    let (key, right) = match keysym {
        0xffe1 => (ModKey::Shift, false),
        0xffe2 => (ModKey::Shift, true),
        0xffe3 => (ModKey::Control, false),
        0xffe4 => (ModKey::Control, true),
        0xffe5 => (ModKey::CapsLock, false),
        // Meta and Super: which of the two the client uses for Command is unconfirmed.
        0xffe7 | 0xffeb => (ModKey::Command, false),
        0xffe8 | 0xffec => (ModKey::Command, true),
        0xffe9 => (ModKey::Option, false),
        0xffea | 0xff7e | 0xfe03 => (ModKey::Option, true),
        _ => return None,
    };
    Some(Modifier { key, right })
}

/// Keys whose evdev position doesn't depend on the keyboard layout.
pub fn fixed(keysym: u32) -> Option<u32> {
    Some(match keysym {
        0xff08 => KEY_BACKSPACE,
        0xff09 | 0xfe20 => KEY_TAB,
        0xff0d => KEY_ENTER,
        0xff13 => KEY_PAUSE,
        0xff14 => KEY_SCROLLLOCK,
        0xff15 => KEY_SYSRQ,
        0xff1b => KEY_ESC,
        0xff50 => KEY_HOME,
        0xff51 => KEY_LEFT,
        0xff52 => KEY_UP,
        0xff53 => KEY_RIGHT,
        0xff54 => KEY_DOWN,
        0xff55 => KEY_PAGEUP,
        0xff56 => KEY_PAGEDOWN,
        0xff57 => KEY_END,
        0xff61 => KEY_SYSRQ,
        0xff63 => KEY_INSERT,
        0xff67 => KEY_COMPOSE,
        0xff6a => KEY_HELP,
        0xff7f => KEY_NUMLOCK,
        0xff8d => KEY_KPENTER,
        0xff95 => KEY_KP7,
        0xff96 => KEY_KP4,
        0xff97 => KEY_KP8,
        0xff98 => KEY_KP6,
        0xff99 => KEY_KP2,
        0xff9a => KEY_KP9,
        0xff9b => KEY_KP3,
        0xff9c => KEY_KP1,
        0xff9d => KEY_KP5,
        0xff9e => KEY_KP0,
        0xff9f => KEY_KPDOT,
        0xffaa => KEY_KPASTERISK,
        0xffab => KEY_KPPLUS,
        0xffac => KEY_KPCOMMA,
        0xffad => KEY_KPMINUS,
        0xffae => KEY_KPDOT,
        0xffaf => KEY_KPSLASH,
        0xffb0 => KEY_KP0,
        0xffb1 => KEY_KP1,
        0xffb2 => KEY_KP2,
        0xffb3 => KEY_KP3,
        0xffb4 => KEY_KP4,
        0xffb5 => KEY_KP5,
        0xffb6 => KEY_KP6,
        0xffb7 => KEY_KP7,
        0xffb8 => KEY_KP8,
        0xffb9 => KEY_KP9,
        0xffbd => KEY_KPEQUAL,
        0xffbe..=0xffc7 => KEY_F1 + (keysym - 0xffbe),
        0xffc8 => KEY_F11,
        0xffc9 => KEY_F12,
        0xffca..=0xffd5 => KEY_F13 + (keysym - 0xffca),
        0xffff => KEY_DELETE,
        0x1008ff02 => KEY_BRIGHTNESSUP,
        0x1008ff03 => KEY_BRIGHTNESSDOWN,
        0x1008ff11 => KEY_VOLUMEDOWN,
        0x1008ff12 => KEY_MUTE,
        0x1008ff13 => KEY_VOLUMEUP,
        0x1008ff14 => KEY_PLAYPAUSE,
        0x1008ff16 => KEY_PREVIOUSSONG,
        0x1008ff17 => KEY_NEXTSONG,
        _ => return None,
    })
}

/// Printable keysyms on the `us` layout: (evdev code, needs Shift).
pub fn us(keysym: u32) -> Option<(u32, bool)> {
    const LETTERS: [u32; 26] = [
        KEY_A, KEY_B, KEY_C, KEY_D, KEY_E, KEY_F, KEY_G, KEY_H, KEY_I, KEY_J, KEY_K, KEY_L, KEY_M,
        KEY_N, KEY_O, KEY_P, KEY_Q, KEY_R, KEY_S, KEY_T, KEY_U, KEY_V, KEY_W, KEY_X, KEY_Y, KEY_Z,
    ];
    const DIGITS: [u32; 10] = [
        KEY_0, KEY_1, KEY_2, KEY_3, KEY_4, KEY_5, KEY_6, KEY_7, KEY_8, KEY_9,
    ];
    let c = char::from_u32(keysym)?;
    Some(match c {
        'a'..='z' => (LETTERS[c as usize - 'a' as usize], false),
        'A'..='Z' => (LETTERS[c as usize - 'A' as usize], true),
        '0'..='9' => (DIGITS[c as usize - '0' as usize], false),
        ' ' => (KEY_SPACE, false),
        '-' => (KEY_MINUS, false),
        '=' => (KEY_EQUAL, false),
        '[' => (KEY_LEFTBRACE, false),
        ']' => (KEY_RIGHTBRACE, false),
        '\\' => (KEY_BACKSLASH, false),
        ';' => (KEY_SEMICOLON, false),
        '\'' => (KEY_APOSTROPHE, false),
        '`' => (KEY_GRAVE, false),
        ',' => (KEY_COMMA, false),
        '.' => (KEY_DOT, false),
        '/' => (KEY_SLASH, false),
        '!' => (KEY_1, true),
        '@' => (KEY_2, true),
        '#' => (KEY_3, true),
        '$' => (KEY_4, true),
        '%' => (KEY_5, true),
        '^' => (KEY_6, true),
        '&' => (KEY_7, true),
        '*' => (KEY_8, true),
        '(' => (KEY_9, true),
        ')' => (KEY_0, true),
        '_' => (KEY_MINUS, true),
        '+' => (KEY_EQUAL, true),
        '{' => (KEY_LEFTBRACE, true),
        '}' => (KEY_RIGHTBRACE, true),
        '|' => (KEY_BACKSLASH, true),
        ':' => (KEY_SEMICOLON, true),
        '"' => (KEY_APOSTROPHE, true),
        '~' => (KEY_GRAVE, true),
        '<' => (KEY_COMMA, true),
        '>' => (KEY_DOT, true),
        '?' => (KEY_SLASH, true),
        _ => return None,
    })
}

/// Case-folded key identity, so a release with the other case still finds the pressed key.
pub fn fold(keysym: u32) -> u32 {
    match char::from_u32(keysym) {
        Some(c @ 'A'..='Z') => c.to_ascii_lowercase() as u32,
        _ => keysym,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_ascii_is_complete() {
        for ks in 0x20..0x7f {
            assert!(us(ks).is_some(), "missing {:?}", char::from_u32(ks));
        }
    }

    #[test]
    fn letters_and_shifted_symbols() {
        assert_eq!(us('a' as u32), Some((KEY_A, false)));
        assert_eq!(us('Q' as u32), Some((KEY_Q, true)));
        assert_eq!(us('!' as u32), Some((KEY_1, true)));
        assert_eq!(us('?' as u32), Some((KEY_SLASH, true)));
        assert_eq!(us('`' as u32), Some((KEY_GRAVE, false)));
        assert_eq!(us(0xe9), None);
    }

    #[test]
    fn function_keys() {
        assert_eq!(fixed(0xff0d), Some(KEY_ENTER));
        assert_eq!(fixed(0xffbe), Some(KEY_F1));
        assert_eq!(fixed(0xffc7), Some(KEY_F10));
        assert_eq!(fixed(0xffc8), Some(KEY_F11));
        assert_eq!(fixed(0xffca), Some(KEY_F13));
        assert_eq!(fixed(0xffd5), Some(KEY_F24));
        assert_eq!(fixed(0xffb5), Some(KEY_KP5));
        assert_eq!(fixed(0xffff), Some(KEY_DELETE));
        assert_eq!(fixed('a' as u32), None);
    }

    #[test]
    fn modifiers() {
        assert_eq!(
            modifier(0xffe1),
            Some(Modifier {
                key: ModKey::Shift,
                right: false
            })
        );
        assert_eq!(modifier(0xffe7).unwrap().key, ModKey::Command);
        assert_eq!(
            modifier(0xffec),
            Some(Modifier {
                key: ModKey::Command,
                right: true
            })
        );
        assert_eq!(modifier(0xffe9).unwrap().key, ModKey::Option);
        assert_eq!(modifier('a' as u32), None);
    }
}
