use ironrdp_ainput as ainput;
use ironrdp_pdu::input::fast_path::{self, SynchronizeFlags};
use ironrdp_pdu::input::mouse::PointerFlags;
use ironrdp_pdu::input::mouse_rel::PointerRelFlags;
use ironrdp_pdu::input::mouse_x::PointerXFlags;
use ironrdp_pdu::input::sync::SyncToggleFlags;
use ironrdp_pdu::input::{MousePdu, MouseRelPdu, MouseXPdu, scan_code, unicode};

/// Keyboard Event
///
/// Describes a keyboard event received from the client
///
#[derive(Debug)]
pub enum KeyboardEvent {
    Pressed { code: u8, extended: bool },
    Released { code: u8, extended: bool },
    UnicodePressed(u16),
    UnicodeReleased(u16),
    Synchronize(SynchronizeFlags),
}

/// Mouse Event
///
/// Describes a mouse event received from the client
///
#[derive(Debug)]
pub enum MouseEvent {
    Buttons { x: u16, y: u16, buttons: u8, pressed: bool },
    Move { x: u16, y: u16 },
    RightPressed,
    RightReleased,
    LeftPressed,
    LeftReleased,
    MiddlePressed,
    MiddleReleased,
    Button4Pressed,
    Button4Released,
    Button5Pressed,
    Button5Released,
    VerticalScroll { value: i16 },
    Scroll { x: i32, y: i32 },
    RelMove { x: i32, y: i32 },
}

/// Input Event Handler for an RDP server
///
/// Whenever the RDP server will receive an input event from a client, the relevant callback from
/// this handler will be called
///
/// # Example
///
/// ```
/// use ironrdp_server::{KeyboardEvent, MouseEvent, RdpServerInputHandler};
///
/// pub struct InputHandler;
///
/// impl RdpServerInputHandler for InputHandler {
///     fn keyboard(&mut self, event: KeyboardEvent) {
///         match event {
///             KeyboardEvent::Pressed { code, .. } => println!("Pressed {}", code),
///             KeyboardEvent::Released { code, .. } => println!("Released {}", code),
///             other => println!("unhandled event: {:?}", other),
///         };
///     }
///
///     fn mouse(&mut self, event: MouseEvent) {
///         let result = match event {
///             MouseEvent::Move { x, y } => println!("Moved mouse to {} {}", x, y),
///             other => println!("unhandled event: {:?}", other),
///         };
///     }
/// }
/// ```
pub trait RdpServerInputHandler: Send {
    fn keyboard(&mut self, event: KeyboardEvent);
    fn mouse(&mut self, event: MouseEvent);
}

impl From<(u8, fast_path::KeyboardFlags)> for KeyboardEvent {
    fn from((key, flags): (u8, fast_path::KeyboardFlags)) -> Self {
        let pause = flags.contains(fast_path::KeyboardFlags::EXTENDED1);
        let key = if pause { 0x45 } else { key };
        let extended = pause || flags.contains(fast_path::KeyboardFlags::EXTENDED);
        if flags.contains(fast_path::KeyboardFlags::RELEASE) {
            KeyboardEvent::Released { code: key, extended }
        } else {
            KeyboardEvent::Pressed { code: key, extended }
        }
    }
}

impl From<(u16, fast_path::KeyboardFlags)> for KeyboardEvent {
    fn from((key, flags): (u16, fast_path::KeyboardFlags)) -> Self {
        if flags.contains(fast_path::KeyboardFlags::RELEASE) {
            KeyboardEvent::UnicodeReleased(key)
        } else {
            KeyboardEvent::UnicodePressed(key)
        }
    }
}

impl From<(u16, scan_code::KeyboardFlags)> for KeyboardEvent {
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "we are truncating the value on purpose"
    )]
    fn from((key, flags): (u16, scan_code::KeyboardFlags)) -> Self {
        let pause = flags.contains(scan_code::KeyboardFlags::EXTENDED_1);
        let key = if pause { 0x45 } else { key };
        let extended = pause || flags.contains(scan_code::KeyboardFlags::EXTENDED);

        if flags.contains(scan_code::KeyboardFlags::RELEASE) {
            KeyboardEvent::Released {
                code: key as u8,
                extended,
            }
        } else {
            KeyboardEvent::Pressed {
                code: key as u8,
                extended,
            }
        }
    }
}

impl From<(u16, unicode::KeyboardFlags)> for KeyboardEvent {
    fn from((key, flags): (u16, unicode::KeyboardFlags)) -> Self {
        if flags.contains(unicode::KeyboardFlags::RELEASE) {
            KeyboardEvent::UnicodeReleased(key)
        } else {
            KeyboardEvent::UnicodePressed(key)
        }
    }
}

impl From<SynchronizeFlags> for KeyboardEvent {
    fn from(value: SynchronizeFlags) -> Self {
        KeyboardEvent::Synchronize(value)
    }
}

impl From<SyncToggleFlags> for KeyboardEvent {
    #[expect(
        clippy::as_conversions,
        clippy::cast_possible_truncation,
        reason = "we are truncating the value on purpose"
    )]
    fn from(value: SyncToggleFlags) -> Self {
        KeyboardEvent::Synchronize(SynchronizeFlags::from_bits_retain(value.bits() as u8))
    }
}

impl From<MousePdu> for MouseEvent {
    fn from(value: MousePdu) -> Self {
        let mut buttons = 0;
        for (flag, bit) in [
            (PointerFlags::LEFT_BUTTON, 1),
            (PointerFlags::RIGHT_BUTTON, 2),
            (PointerFlags::MIDDLE_BUTTON_OR_WHEEL, 4),
        ] {
            if value.flags.contains(flag) {
                buttons |= bit;
            }
        }
        if buttons != 0 {
            Self::Buttons {
                x: value.x_position,
                y: value.y_position,
                buttons,
                pressed: value.flags.contains(PointerFlags::DOWN),
            }
        } else if value.flags.contains(PointerFlags::HORIZONTAL_WHEEL) {
            Self::Scroll {
                x: value.number_of_wheel_rotation_units.into(),
                y: 0,
            }
        } else if value.flags.contains(PointerFlags::VERTICAL_WHEEL) {
            Self::VerticalScroll {
                value: value.number_of_wheel_rotation_units,
            }
        } else {
            Self::Move {
                x: value.x_position,
                y: value.y_position,
            }
        }
    }
}

impl From<MouseXPdu> for MouseEvent {
    fn from(value: MouseXPdu) -> Self {
        let buttons = u8::from(value.flags.contains(PointerXFlags::BUTTON1)) * 8
            | u8::from(value.flags.contains(PointerXFlags::BUTTON2)) * 16;
        if buttons != 0 {
            Self::Buttons {
                x: value.x_position,
                y: value.y_position,
                buttons,
                pressed: value.flags.contains(PointerXFlags::DOWN),
            }
        } else {
            Self::Move {
                x: value.x_position,
                y: value.y_position,
            }
        }
    }
}

impl From<MouseRelPdu> for MouseEvent {
    fn from(value: MouseRelPdu) -> Self {
        if value.flags.contains(PointerRelFlags::BUTTON1) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::LeftPressed
            } else {
                MouseEvent::LeftReleased
            }
        } else if value.flags.contains(PointerRelFlags::BUTTON2) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::RightPressed
            } else {
                MouseEvent::RightReleased
            }
        } else if value.flags.contains(PointerRelFlags::BUTTON3) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::MiddlePressed
            } else {
                MouseEvent::MiddleReleased
            }
        } else if value.flags.contains(PointerRelFlags::XBUTTON1) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::Button4Pressed
            } else {
                MouseEvent::Button4Released
            }
        } else if value.flags.contains(PointerRelFlags::XBUTTON2) {
            if value.flags.contains(PointerRelFlags::DOWN) {
                MouseEvent::Button5Pressed
            } else {
                MouseEvent::Button5Released
            }
        } else {
            MouseEvent::RelMove {
                x: value.x_delta.into(),
                y: value.y_delta.into(),
            }
        }
    }
}

impl From<ainput::MousePdu> for MouseEvent {
    fn from(value: ainput::MousePdu) -> Self {
        use ainput::MouseEventFlags;

        if value.flags.contains(MouseEventFlags::BUTTON1) {
            if value.flags.contains(MouseEventFlags::DOWN) {
                MouseEvent::LeftPressed
            } else {
                MouseEvent::LeftReleased
            }
        } else if value.flags.contains(MouseEventFlags::BUTTON2) {
            if value.flags.contains(MouseEventFlags::DOWN) {
                MouseEvent::RightPressed
            } else {
                MouseEvent::RightReleased
            }
        } else if value.flags.contains(MouseEventFlags::BUTTON3) {
            if value.flags.contains(MouseEventFlags::DOWN) {
                MouseEvent::MiddlePressed
            } else {
                MouseEvent::MiddleReleased
            }
        } else if value.flags.contains(MouseEventFlags::WHEEL) {
            MouseEvent::Scroll { x: value.x, y: value.y }
        } else if value.flags.contains(MouseEventFlags::REL) {
            MouseEvent::RelMove { x: value.x, y: value.y }
        } else if value.flags.contains(MouseEventFlags::MOVE) {
            // assume moves are 0 <= u16::MAX
            MouseEvent::Move {
                x: value.x.try_into().unwrap_or(0),
                y: value.y.try_into().unwrap_or(0),
            }
        } else {
            MouseEvent::Move { x: 0, y: 0 }
        }
    }
}
