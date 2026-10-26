//! RDP input translated into the same session sink used by RFB.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use ironrdp_server::{KeyboardEvent, MouseEvent, RdpServerInputHandler};
use tracing::warn;

use crate::input::{InputFactory, InputSink};

pub(super) struct Input {
    pub factory: Option<InputFactory>,
    pub authenticated: Arc<AtomicBool>,
    pub width: u16,
    pub height: u16,
    pub sink: Option<Box<dyn InputSink>>,
    pub buttons: u8,
    pub x: i32,
    pub y: i32,
    pub surrogate: [Option<u16>; 2],
}

impl Input {
    fn sink(&mut self) -> Result<Option<&mut Box<dyn InputSink>>> {
        if !self.authenticated.load(Ordering::Acquire) {
            return Ok(None);
        }
        if self.sink.is_none()
            && let Some(factory) = &self.factory
        {
            self.sink = Some(factory(self.width.into(), self.height.into())?);
        }
        Ok(self.sink.as_mut())
    }

    fn key(&mut self, event: KeyboardEvent) -> Result<()> {
        match event {
            KeyboardEvent::Pressed { code, extended }
            | KeyboardEvent::Released { code, extended } => {
                let down = matches!(event, KeyboardEvent::Pressed { .. });
                if let Some(code) = scancode(code, extended)
                    && let Some(sink) = self.sink()?
                {
                    sink.physical_key(down, code)?;
                }
            }
            KeyboardEvent::UnicodePressed(unit) | KeyboardEvent::UnicodeReleased(unit) => {
                let down = matches!(event, KeyboardEvent::UnicodePressed(_));
                let pending = &mut self.surrogate[usize::from(down)];
                let scalar = if (0xd800..=0xdbff).contains(&unit) {
                    *pending = Some(unit);
                    return Ok(());
                } else if (0xdc00..=0xdfff).contains(&unit) {
                    pending.take().and_then(|high| {
                        char::from_u32(
                            0x10000 + ((u32::from(high) - 0xd800) << 10) + u32::from(unit) - 0xdc00,
                        )
                    })
                } else {
                    *pending = None;
                    char::from_u32(unit.into())
                };
                if let Some(c) = scalar
                    && let Some(sink) = self.sink()?
                {
                    let keysym = match c {
                        '\r' | '\n' => 0xff0d,
                        '\t' => 0xff09,
                        '\u{8}' => 0xff08,
                        '\u{1b}' => 0xff1b,
                        c if u32::from(c) <= 0xff => c.into(),
                        c => 0x01000000 | u32::from(c),
                    };
                    sink.key(down, keysym, None)?;
                }
            }
            // Toggle flags describe client state, not physical key transitions.
            KeyboardEvent::Synchronize(_) => {}
        }
        Ok(())
    }

    fn pointer(&mut self) -> Result<()> {
        let (x, y, buttons) = (self.x, self.y, self.buttons);
        if let Some(sink) = self.sink()? {
            sink.pointer(x, y, buttons)?;
        }
        Ok(())
    }

    fn button(&mut self, button: u8, down: bool) -> Result<()> {
        if button >= 4 {
            if let Some(sink) = self.sink()? {
                sink.extra_button(button, down)?;
            }
        } else {
            let bit = 1 << (button - 1);
            self.buttons = (self.buttons & !bit) | if down { bit } else { 0 };
            self.pointer()?;
        }
        Ok(())
    }

    fn mouse_event(&mut self, event: MouseEvent) -> Result<()> {
        match event {
            MouseEvent::Move { x, y } => {
                self.x = x.into();
                self.y = y.into();
                self.pointer()?;
            }
            MouseEvent::RelMove { x, y } => {
                self.x = self.x.saturating_add(x).clamp(0, i32::from(self.width) - 1);
                self.y = self
                    .y
                    .saturating_add(y)
                    .clamp(0, i32::from(self.height) - 1);
                self.pointer()?;
            }
            MouseEvent::Buttons {
                x,
                y,
                buttons,
                pressed,
            } => {
                self.x = x.into();
                self.y = y.into();
                self.pointer()?;
                for i in 0..5 {
                    if buttons & (1 << i) != 0 {
                        self.button(i + 1, pressed)?;
                    }
                }
            }
            MouseEvent::LeftPressed => self.button(1, true)?,
            MouseEvent::LeftReleased => self.button(1, false)?,
            MouseEvent::RightPressed => self.button(2, true)?,
            MouseEvent::RightReleased => self.button(2, false)?,
            MouseEvent::MiddlePressed => self.button(3, true)?,
            MouseEvent::MiddleReleased => self.button(3, false)?,
            MouseEvent::Button4Pressed => self.button(4, true)?,
            MouseEvent::Button4Released => self.button(4, false)?,
            MouseEvent::Button5Pressed => self.button(5, true)?,
            MouseEvent::Button5Released => self.button(5, false)?,
            MouseEvent::VerticalScroll { value } => self.wheel(0, value.into())?,
            MouseEvent::Scroll { x, y } => self.wheel(x, y)?,
        }
        Ok(())
    }

    fn wheel(&mut self, x: i32, y: i32) -> Result<()> {
        if let Some(sink) = self.sink()? {
            sink.wheel(x, y)?;
        }
        Ok(())
    }
}

impl RdpServerInputHandler for Input {
    fn keyboard(&mut self, event: KeyboardEvent) {
        if !self.authenticated.load(Ordering::Acquire) {
            return;
        }
        if let Err(error) = self.key(event) {
            warn!(%error, "RDP keyboard injection failed");
        }
    }
    fn mouse(&mut self, event: MouseEvent) {
        if !self.authenticated.load(Ordering::Acquire) {
            return;
        }
        if let Err(error) = self.mouse_event(event) {
            warn!(%error, "RDP pointer injection failed");
        }
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        if let Some(sink) = &mut self.sink {
            let _ = sink.release_all();
        }
    }
}

fn scancode(code: u8, extended: bool) -> Option<u32> {
    if !extended {
        return match code {
            0x01..=0x53 | 0x56..=0x58 => Some(code.into()),
            0x64..=0x6f => Some(u32::from(code) - 0x64 + 183),
            0x70 => Some(90),
            0x73 => Some(89),
            0x79 => Some(92),
            0x7b => Some(94),
            0x7d => Some(124),
            0x7e => Some(121),
            _ => None,
        };
    }
    Some(match code {
        0x10 => 165,
        0x19 => 163,
        0x1c => 96,
        0x1d => 97,
        0x20 => 113,
        0x21 => 140,
        0x22 => 164,
        0x24 => 166,
        0x2e => 114,
        0x30 => 115,
        0x32 => 150,
        0x35 => 98,
        0x37 => 99,
        0x38 => 100,
        0x45 | 0x46 => 119,
        0x47 => 102,
        0x48 => 103,
        0x49 => 104,
        0x4b => 105,
        0x4d => 106,
        0x4f => 107,
        0x50 => 108,
        0x51 => 109,
        0x52 => 110,
        0x53 => 111,
        0x5b => 125,
        0x5c => 126,
        0x5d => 127,
        0x5e => 116,
        0x5f => 142,
        0x63 => 143,
        // Ignore fake E0 shift transitions surrounding PrintScreen.
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::keyboard::{KbAction, KeyboardEngine};
    use crate::input::pointer::{Axis, PointerConfig, PointerEngine, PtrAction};
    use crate::input::{KeyboardConfig, ModTarget, ScrollEvent};
    use ironrdp_pdu::input::mouse::PointerFlags;
    use ironrdp_pdu::input::mouse_x::PointerXFlags;
    use ironrdp_pdu::input::{MousePdu, MouseXPdu};
    use std::sync::Mutex;

    struct Sink(Arc<Mutex<Vec<String>>>);
    impl InputSink for Sink {
        fn key(&mut self, down: bool, sym: u32, _: Option<u16>) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("unicode {down} {sym:x}"));
            Ok(())
        }
        fn physical_key(&mut self, down: bool, code: u32) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("physical {down} {code}"));
            Ok(())
        }
        fn pointer(&mut self, x: i32, y: i32, buttons: u8) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("pointer {x} {y} {buttons}"));
            Ok(())
        }
        fn extra_button(&mut self, button: u8, down: bool) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("button {button} {down}"));
            Ok(())
        }
        fn wheel(&mut self, x: i32, y: i32) -> Result<()> {
            self.0.lock().unwrap().push(format!("wheel {x} {y}"));
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
            self.0.lock().unwrap().push("released".into());
            Ok(())
        }
    }

    #[test]
    fn authenticated_input_maps_wire_events_and_disconnect_releases_everything() {
        let events = Arc::new(Mutex::new(vec![]));
        let recorder = events.clone();
        let authenticated = Arc::new(AtomicBool::new(false));
        let mut input = Input {
            factory: Some(Arc::new(move |_, _| Ok(Box::new(Sink(recorder.clone()))))),
            authenticated: authenticated.clone(),
            width: 800,
            height: 600,
            sink: None,
            buttons: 0,
            x: 0,
            y: 0,
            surrogate: [None; 2],
        };
        input.keyboard(KeyboardEvent::Pressed {
            code: 0x1e,
            extended: false,
        });
        input.mouse(MouseEvent::LeftPressed);
        assert!(input.sink.is_none());
        authenticated.store(true, Ordering::Release);
        input.keyboard(KeyboardEvent::Pressed {
            code: 0x1d,
            extended: true,
        });
        input.keyboard(KeyboardEvent::Released {
            code: 0x1d,
            extended: true,
        });
        for down in [true, false] {
            for unit in [0xd83d, 0xde00] {
                input.keyboard(if down {
                    KeyboardEvent::UnicodePressed(unit)
                } else {
                    KeyboardEvent::UnicodeReleased(unit)
                });
            }
        }
        input.mouse(
            MousePdu {
                flags: PointerFlags::MIDDLE_BUTTON_OR_WHEEL | PointerFlags::DOWN,
                x_position: 100,
                y_position: 200,
                number_of_wheel_rotation_units: 0,
            }
            .into(),
        );
        input.mouse(
            MousePdu {
                flags: PointerFlags::HORIZONTAL_WHEEL,
                x_position: 0,
                y_position: 0,
                number_of_wheel_rotation_units: -60,
            }
            .into(),
        );
        input.mouse(
            MouseXPdu {
                flags: PointerXFlags::BUTTON1 | PointerXFlags::DOWN,
                x_position: 101,
                y_position: 201,
            }
            .into(),
        );
        drop(input);
        assert_eq!(
            *events.lock().unwrap(),
            [
                "physical true 97",
                "physical false 97",
                "unicode true 101f600",
                "unicode false 101f600",
                "pointer 100 200 0",
                "pointer 100 200 4",
                "wheel -60 0",
                "pointer 101 201 4",
                "button 4 true",
                "released"
            ]
        );
    }

    #[test]
    fn pc_modifiers_bypass_mac_remapping_and_release_with_buttons() {
        let mut kb = KeyboardEngine::new(KeyboardConfig {
            command: ModTarget::Ctrl,
            option: ModTarget::Super,
            ..Default::default()
        });
        assert!(
            kb.physical_key(true, scancode(0x38, false).unwrap())
                .contains(&KbAction::Key {
                    code: 56,
                    down: true
                })
        );
        assert!(
            kb.physical_key(true, scancode(0x5b, true).unwrap())
                .contains(&KbAction::Key {
                    code: 125,
                    down: true
                })
        );
        let released = kb.release_all();
        for code in [56, 125] {
            assert!(released.contains(&KbAction::Key { code, down: false }));
        }
        let mut pointer = PointerEngine::new(PointerConfig::default(), 800, 600);
        pointer.pointer(10, 20, 7);
        pointer.extra_button(4, true);
        pointer.extra_button(5, true);
        pointer.pointer(30, 40, 7);
        let released = pointer.release_all();
        for code in 0x110..=0x114 {
            assert!(released.contains(&PtrAction::Button {
                code,
                pressed: false
            }));
        }
        assert!(pointer.wheel(60, 120).contains(&PtrAction::Axis {
            axis: Axis::Horizontal,
            value: 7.5
        }));
        assert!(pointer.wheel(60, 120).contains(&PtrAction::AxisDiscrete {
            axis: Axis::Vertical,
            value: -15.0,
            steps: -1
        }));
        assert_eq!(scancode(0x2a, true), None);
        assert_eq!(scancode(0xff, false), None);
    }
}
