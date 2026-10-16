//! Pointer state machine: client pointer/scroll events -> virtual pointer requests.

use super::ScrollEvent;
use super::evdev::{BTN_LEFT, BTN_MIDDLE, BTN_RIGHT};

/// libinput's default angle per wheel click, which clients treat as one step.
const WHEEL_STEP: f64 = 15.0;

const SCROLL_PHASE_ENDED: u32 = 4;
const SCROLL_PHASE_CANCELLED: u32 = 8;
const FLAG_CONTINUOUS: u32 = 2;

/// Bit order of buttons 1 and 2 in the client's mask. Apple's server swaps RFB order into mac
/// order unless the viewer speaks RFB 3.888/3.889, so Screen Sharing.app sends `Mac`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonOrder {
    /// bit1 = right, bit2 = middle.
    Mac,
    /// bit1 = middle, bit2 = right.
    Rfb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Vertical,
    Horizontal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisSource {
    Wheel,
    Finger,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PtrAction {
    Motion {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Button {
        code: u32,
        pressed: bool,
    },
    AxisSource(AxisSource),
    Axis {
        axis: Axis,
        value: f64,
    },
    AxisDiscrete {
        axis: Axis,
        value: f64,
        steps: i32,
    },
    AxisStop(Axis),
    Frame,
}

#[derive(Clone, Debug)]
pub struct PointerConfig {
    pub buttons: ButtonOrder,
    /// Forward the client's momentum (inertia) scroll events. Off by default: Linux toolkits run
    /// their own kinetic scrolling after `axis_stop`, so forwarding would double it.
    pub momentum: bool,
    /// Flip both scroll axes.
    pub invert_scroll: bool,
}

impl Default for PointerConfig {
    fn default() -> Self {
        Self {
            buttons: ButtonOrder::Mac,
            momentum: false,
            invert_scroll: false,
        }
    }
}

pub struct PointerEngine {
    cfg: PointerConfig,
    width: u32,
    height: u32,
    pos: Option<(u32, u32)>,
    /// bit0 left, bit1 right, bit2 middle.
    buttons: u8,
}

impl PointerEngine {
    pub fn new(cfg: PointerConfig, width: u32, height: u32) -> Self {
        Self {
            cfg,
            width: width.max(1),
            height: height.max(1),
            pos: None,
            buttons: 0,
        }
    }

    pub fn set_extent(&mut self, width: u32, height: u32) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.pos = None;
    }

    /// `mask`: bits 0-2 buttons (see `ButtonOrder`), 3 wheel up, 4 down, 5 left, 6 right.
    pub fn pointer(&mut self, x: i32, y: i32, mask: u8) -> Vec<PtrAction> {
        let mut out = Vec::new();
        let x = x.clamp(0, self.width as i32 - 1) as u32;
        let y = y.clamp(0, self.height as i32 - 1) as u32;
        if self.pos != Some((x, y)) {
            self.pos = Some((x, y));
            out.push(PtrAction::Motion {
                x,
                y,
                width: self.width,
                height: self.height,
            });
        }
        let want = match self.cfg.buttons {
            ButtonOrder::Mac => mask & 7,
            ButtonOrder::Rfb => (mask & 1) | (mask >> 1 & 2) | (mask << 1 & 4),
        };
        for (bit, code) in [(1u8, BTN_LEFT), (2, BTN_RIGHT), (4, BTN_MIDDLE)] {
            if (want ^ self.buttons) & bit != 0 {
                out.push(PtrAction::Button {
                    code,
                    pressed: want & bit != 0,
                });
            }
        }
        self.buttons = want;
        let wheel = [
            (8u8, Axis::Vertical, -1),
            (16, Axis::Vertical, 1),
            (32, Axis::Horizontal, -1),
            (64, Axis::Horizontal, 1),
        ];
        let mut sourced = false;
        for (bit, axis, steps) in wheel {
            if mask & bit != 0 {
                if !sourced {
                    out.push(PtrAction::AxisSource(AxisSource::Wheel));
                    sourced = true;
                }
                let steps = self.sign(steps);
                out.push(PtrAction::AxisDiscrete {
                    axis,
                    value: steps as f64 * WHEEL_STEP,
                    steps,
                });
            }
        }
        if !out.is_empty() {
            out.push(PtrAction::Frame);
        }
        out
    }

    /// EventMessage2 subtype 11. CG deltas are positive for up/left; Wayland's for down/right.
    pub fn scroll(&mut self, ev: &ScrollEvent) -> Vec<PtrAction> {
        if ev.momentum_phase != 0 && !self.cfg.momentum {
            return vec![];
        }
        let mut out = Vec::new();
        let continuous = ev.flags & FLAG_CONTINUOUS != 0;
        let axes = [
            (Axis::Vertical, ev.lines_y, ev.point_y),
            (Axis::Horizontal, ev.lines_x, ev.point_x),
        ];
        if continuous {
            out.push(PtrAction::AxisSource(AxisSource::Finger));
            for (axis, _, px) in axes {
                if px != 0 {
                    out.push(PtrAction::Axis {
                        axis,
                        value: self.sign(-px) as f64,
                    });
                }
            }
            if ev.momentum_phase == 0
                && ev.scroll_phase & (SCROLL_PHASE_ENDED | SCROLL_PHASE_CANCELLED) != 0
            {
                out.push(PtrAction::AxisStop(Axis::Vertical));
                out.push(PtrAction::AxisStop(Axis::Horizontal));
            }
        } else {
            out.push(PtrAction::AxisSource(AxisSource::Wheel));
            for (axis, lines, px) in axes {
                if lines != 0 {
                    let steps = self.sign(-(lines as i32));
                    out.push(PtrAction::AxisDiscrete {
                        axis,
                        value: steps as f64 * WHEEL_STEP,
                        steps,
                    });
                } else if px != 0 {
                    out.push(PtrAction::Axis {
                        axis,
                        value: self.sign(-px) as f64,
                    });
                }
            }
        }
        if out.len() == 1 {
            return vec![];
        }
        out.push(PtrAction::Frame);
        out
    }

    pub fn release_all(&mut self) -> Vec<PtrAction> {
        let mut out: Vec<PtrAction> = [(1u8, BTN_LEFT), (2, BTN_RIGHT), (4, BTN_MIDDLE)]
            .into_iter()
            .filter(|(bit, _)| self.buttons & bit != 0)
            .map(|(_, code)| PtrAction::Button {
                code,
                pressed: false,
            })
            .collect();
        self.buttons = 0;
        if !out.is_empty() {
            out.push(PtrAction::Frame);
        }
        out
    }

    fn sign(&self, v: i32) -> i32 {
        if self.cfg.invert_scroll { -v } else { v }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use PtrAction as P;

    fn eng() -> PointerEngine {
        PointerEngine::new(PointerConfig::default(), 3456, 2160)
    }

    #[test]
    fn motion_clamps_and_dedups() {
        let mut p = eng();
        assert_eq!(
            p.pointer(5000, -3, 0),
            vec![
                P::Motion {
                    x: 3455,
                    y: 0,
                    width: 3456,
                    height: 2160
                },
                P::Frame
            ]
        );
        assert!(p.pointer(5000, -3, 0).is_empty());
    }

    #[test]
    fn mac_button_order() {
        let mut p = eng();
        p.pointer(1, 1, 0);
        assert_eq!(
            p.pointer(1, 1, 2),
            vec![
                P::Button {
                    code: BTN_RIGHT,
                    pressed: true
                },
                P::Frame
            ]
        );
        assert_eq!(
            p.pointer(1, 1, 4),
            vec![
                P::Button {
                    code: BTN_RIGHT,
                    pressed: false
                },
                P::Button {
                    code: BTN_MIDDLE,
                    pressed: true
                },
                P::Frame
            ]
        );
    }

    #[test]
    fn rfb_button_order() {
        let mut p = PointerEngine::new(
            PointerConfig {
                buttons: ButtonOrder::Rfb,
                ..Default::default()
            },
            10,
            10,
        );
        p.pointer(1, 1, 0);
        assert_eq!(
            p.pointer(1, 1, 2),
            vec![
                P::Button {
                    code: BTN_MIDDLE,
                    pressed: true
                },
                P::Frame
            ]
        );
        assert_eq!(
            p.pointer(1, 1, 4),
            vec![
                P::Button {
                    code: BTN_RIGHT,
                    pressed: true
                },
                P::Button {
                    code: BTN_MIDDLE,
                    pressed: false
                },
                P::Frame
            ]
        );
    }

    #[test]
    fn wheel_bits() {
        let mut p = eng();
        p.pointer(1, 1, 0);
        assert_eq!(
            p.pointer(1, 1, 8),
            vec![
                P::AxisSource(AxisSource::Wheel),
                P::AxisDiscrete {
                    axis: Axis::Vertical,
                    value: -15.0,
                    steps: -1
                },
                P::Frame
            ]
        );
        assert_eq!(
            p.pointer(1, 1, 64)[1],
            P::AxisDiscrete {
                axis: Axis::Horizontal,
                value: 15.0,
                steps: 1
            }
        );
    }

    fn scroll(flags: u32, lines_y: i16, point_y: i32, phase: u32, momentum: u32) -> ScrollEvent {
        ScrollEvent {
            lines_y,
            point_y,
            flags,
            scroll_phase: phase,
            momentum_phase: momentum,
            ..Default::default()
        }
    }

    #[test]
    fn trackpad_scroll_is_finger_with_stop() {
        let mut p = eng();
        assert_eq!(
            p.scroll(&scroll(2, 0, 12, 2, 0)),
            vec![
                P::AxisSource(AxisSource::Finger),
                P::Axis {
                    axis: Axis::Vertical,
                    value: -12.0
                },
                P::Frame
            ]
        );
        let end = p.scroll(&scroll(2, 0, 0, 4, 0));
        assert!(end.contains(&P::AxisStop(Axis::Vertical)));
        assert!(p.scroll(&scroll(2, 0, 30, 0, 2)).is_empty());
    }

    #[test]
    fn wheel_scroll_is_discrete() {
        let mut p = eng();
        assert_eq!(
            p.scroll(&scroll(0, -2, -20, 0, 0)),
            vec![
                P::AxisSource(AxisSource::Wheel),
                P::AxisDiscrete {
                    axis: Axis::Vertical,
                    value: 30.0,
                    steps: 2
                },
                P::Frame
            ]
        );
    }
}
