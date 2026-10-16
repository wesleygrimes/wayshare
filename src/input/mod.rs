//! Input injection: client key/pointer/scroll messages -> virtual keyboard and pointer.

pub mod evdev;
pub mod keyboard;
pub mod keysym;
pub mod kvk;
pub mod pointer;
#[cfg(target_os = "linux")]
pub mod wayland;

use anyhow::{Result, ensure};
use tracing::debug;

pub use keyboard::{KeyboardConfig, ModTarget};
pub use pointer::{ButtonOrder, PointerConfig};

/// What the rfb session calls for input. Coordinates are framebuffer pixels.
pub trait InputSink: Send {
    /// `mac_keycode` is `Some` only when the client set the use-keycode flag.
    fn key(&mut self, down: bool, keysym: u32, mac_keycode: Option<u16>) -> Result<()>;
    /// `buttons` as on the wire: bits 0-2 buttons, 3/4 wheel up/down, 5/6 wheel left/right.
    fn pointer(&mut self, x: i32, y: i32, buttons: u8) -> Result<()>;
    fn scroll(&mut self, ev: &ScrollEvent) -> Result<()>;
    /// Pointer coordinate space; call when the framebuffer is resized.
    fn set_extent(&mut self, width: u32, height: u32);
    /// Client msg 26. Unknown sources are ignored.
    fn set_input_source(&mut self, id: &str) -> Result<()>;
    /// Lift every held key and button (disconnect, loss of control).
    fn release_all(&mut self) -> Result<()>;
}

/// EventMessage2 subtype 11 (CG scroll wheel event). `_y` = CG axis 1, `_x` = axis 2;
/// positive is up/left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollEvent {
    pub lines_y: i16,
    pub lines_x: i16,
    pub fixed_y: i32,
    pub fixed_x: i32,
    pub point_y: i32,
    pub point_x: i32,
    pub scroll_phase: u32,
    pub momentum_phase: u32,
    pub scroll_count: i32,
    pub flags: u32,
    /// Desktop points, not framebuffer pixels.
    pub x: u16,
    pub y: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Begin,
    End,
    Magnify,
    SmartZoom,
    Rotate,
    Swipe,
    Lookup,
    GestureScroll,
    LegacyScroll,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event2 {
    Scroll(ScrollEvent),
    /// Subtype 12: `block` is still AES-ECB encrypted.
    KeyMouse {
        flags: u8,
        block: [u8; 16],
        timestamp: Option<u64>,
    },
    /// No Wayland equivalent the virtual pointer can express; dropped.
    Gesture(Gesture),
    Other(u16),
}

impl Event2 {
    pub fn parse(hdr_flags: u8, body: &[u8]) -> Result<Self> {
        let ts = if hdr_flags & 1 != 0 { 8 } else { 0 };
        ensure!(
            body.len() >= 4,
            "EventMessage2 body too short ({})",
            body.len()
        );
        let kind = u16::from_be_bytes([body[2], body[3]]);
        let extra = match kind {
            1 | 2 => 8,
            3 | 5 => 12,
            4 | 7 => 4,
            6 => 44,
            8 => 28,
            9 => 32,
            11 => 50,
            12 => 18,
            _ => 0,
        };
        ensure!(
            body.len() >= 4 + extra + ts,
            "EventMessage2 subtype {kind}: body {} < {}",
            body.len(),
            4 + extra + ts
        );
        let i16_at = |o: usize| i16::from_be_bytes([body[o], body[o + 1]]);
        let i32_at = |o: usize| i32::from_be_bytes(body[o..o + 4].try_into().unwrap());
        let u16_at = |o: usize| u16::from_be_bytes([body[o], body[o + 1]]);
        Ok(match kind {
            11 => Event2::Scroll(ScrollEvent {
                lines_x: i16_at(4),
                lines_y: i16_at(6),
                fixed_x: i32_at(10),
                fixed_y: i32_at(14),
                point_x: i32_at(22),
                point_y: i32_at(26),
                scroll_phase: i32_at(34) as u32,
                momentum_phase: i32_at(38) as u32,
                scroll_count: i32_at(42),
                flags: i32_at(46) as u32,
                x: u16_at(50),
                y: u16_at(52),
            }),
            12 => Event2::KeyMouse {
                flags: body[5],
                block: body[6..22].try_into().unwrap(),
                timestamp: (ts != 0).then(|| u64::from_be_bytes(body[22..30].try_into().unwrap())),
            },
            1 => Event2::Gesture(Gesture::Begin),
            2 => Event2::Gesture(Gesture::End),
            3 => Event2::Gesture(Gesture::Magnify),
            4 => Event2::Gesture(Gesture::SmartZoom),
            5 => Event2::Gesture(Gesture::Rotate),
            6 => Event2::Gesture(Gesture::Swipe),
            7 => Event2::Gesture(Gesture::Lookup),
            8 => Event2::Gesture(Gesture::GestureScroll),
            9 => Event2::Gesture(Gesture::LegacyScroll),
            k => Event2::Other(k),
        })
    }
}

/// Applies a decrypted 16-byte key/mouse block (msgs 16 and 23/12). `flags` is the outer
/// flags byte; `keycode_needs_bit1` is true when the client's version is above 10.
pub fn apply_block<S: InputSink + ?Sized>(
    sink: &mut S,
    flags: u8,
    b: &[u8; 16],
    keycode_needs_bit1: bool,
) -> Result<()> {
    ensure!(
        matches!(b[0], 0 | 0xff) && matches!(b[10], 0 | 0xff),
        "decryption error: {b:02x?}"
    );
    let has_key = b[0] == 0xff;
    let has_mouse = b[10] == 0xff;
    if has_key {
        let keysym = u32::from_be_bytes(b[2..6].try_into().unwrap());
        let use_kvk = !has_mouse && flags & 1 != 0 && (!keycode_needs_bit1 || flags & 2 != 0);
        let kvk = use_kvk.then(|| u16::from_be_bytes([b[14], b[15]]));
        sink.key(b[1] != 0, keysym, kvk)?;
    }
    if has_mouse {
        let x = i16::from_be_bytes([b[12], b[13]]) as i32;
        let y = i16::from_be_bytes([b[14], b[15]]) as i32;
        sink.pointer(x, y, b[11])?;
    }
    Ok(())
}

/// Client msg 26 payload after the u16 size field: version, id length, id.
pub fn parse_input_source(payload: &[u8]) -> Option<String> {
    let len = u16::from_be_bytes(payload.get(2..4)?.try_into().ok()?) as usize;
    let id = payload.get(4..4 + len)?;
    Some(
        String::from_utf8_lossy(id)
            .trim_end_matches('\0')
            .to_string(),
    )
}

/// Drops everything; for platforms without a backend.
pub struct LoggingSink;

impl InputSink for LoggingSink {
    fn key(&mut self, down: bool, keysym: u32, mac_keycode: Option<u16>) -> Result<()> {
        debug!(
            down,
            keysym = format_args!("{keysym:#x}"),
            ?mac_keycode,
            "key"
        );
        Ok(())
    }
    fn pointer(&mut self, x: i32, y: i32, buttons: u8) -> Result<()> {
        debug!(x, y, buttons, "pointer");
        Ok(())
    }
    fn scroll(&mut self, ev: &ScrollEvent) -> Result<()> {
        debug!(?ev, "scroll");
        Ok(())
    }
    fn set_extent(&mut self, _: u32, _: u32) {}
    fn set_input_source(&mut self, id: &str) -> Result<()> {
        debug!(id, "input source");
        Ok(())
    }
    fn release_all(&mut self) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Rec(Vec<String>);

    impl InputSink for Rec {
        fn key(&mut self, down: bool, keysym: u32, kvk: Option<u16>) -> Result<()> {
            self.0.push(format!("key {down} {keysym:#x} {kvk:?}"));
            Ok(())
        }
        fn pointer(&mut self, x: i32, y: i32, b: u8) -> Result<()> {
            self.0.push(format!("ptr {x} {y} {b}"));
            Ok(())
        }
        fn scroll(&mut self, _: &ScrollEvent) -> Result<()> {
            Ok(())
        }
        fn set_extent(&mut self, _: u32, _: u32) {}
        fn set_input_source(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        fn release_all(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn block_key_with_keycode_flag() {
        let b = [0xff, 1, 0, 0, 0, 0x61, 0, 0, 0, 0, 0, 0, 0, 40, 0, 0x00];
        let mut r = Rec::default();
        apply_block(&mut r, 1, &b, false).unwrap();
        apply_block(&mut r, 1, &b, true).unwrap();
        apply_block(&mut r, 3, &b, true).unwrap();
        assert_eq!(
            r.0,
            [
                "key true 0x61 Some(0)",
                "key true 0x61 None",
                "key true 0x61 Some(0)"
            ]
        );
    }

    #[test]
    fn block_mouse_signed_coords() {
        let b = [
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 1, 0xff, 0xff, 0x01, 0x00,
        ];
        let mut r = Rec::default();
        apply_block(&mut r, 0, &b, false).unwrap();
        assert_eq!(r.0, ["ptr -1 256 1"]);
        let bad = [7u8; 16];
        assert!(apply_block(&mut r, 0, &bad, false).is_err());
    }

    #[test]
    fn event2_scroll() {
        let mut body = vec![0u8; 54];
        body[3] = 11;
        body[6..8].copy_from_slice(&(-1i16).to_be_bytes());
        body[26..30].copy_from_slice(&(-10i32).to_be_bytes());
        body[34..38].copy_from_slice(&2u32.to_be_bytes());
        body[46..50].copy_from_slice(&2u32.to_be_bytes());
        body[50..52].copy_from_slice(&100u16.to_be_bytes());
        let Event2::Scroll(s) = Event2::parse(0, &body).unwrap() else {
            panic!()
        };
        assert_eq!(
            (s.lines_y, s.point_y, s.scroll_phase, s.flags, s.x),
            (-1, -10, 2, 2, 100)
        );
        assert!(Event2::parse(1, &body).is_err());
    }

    #[test]
    fn event2_keymouse_and_gestures() {
        let mut body = vec![0u8; 30];
        body[3] = 12;
        body[5] = 3;
        body[6] = 0xaa;
        body[29] = 9;
        assert_eq!(
            Event2::parse(1, &body).unwrap(),
            Event2::KeyMouse {
                flags: 3,
                block: {
                    let mut b = [0; 16];
                    b[0] = 0xaa;
                    b
                },
                timestamp: Some(9)
            }
        );
        let mut g = vec![0u8; 16];
        g[3] = 3;
        assert_eq!(
            Event2::parse(0, &g).unwrap(),
            Event2::Gesture(Gesture::Magnify)
        );
        g[3] = 10;
        assert_eq!(Event2::parse(0, &g).unwrap(), Event2::Other(10));
    }

    #[test]
    fn input_source_payload() {
        let id = b"com.apple.keylayout.US";
        let mut p = vec![0, 1, 0, id.len() as u8];
        p.extend_from_slice(id);
        p.push(0);
        assert_eq!(
            parse_input_source(&p).as_deref(),
            Some("com.apple.keylayout.US")
        );
        assert_eq!(parse_input_source(&[0, 1, 0, 9, b'x']), None);
    }
}
