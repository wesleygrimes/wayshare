//! Client msg 29 (SetDisplayConfiguration) and the DisplayInfo (1101) / DisplayInfo2 (1105)
//! pseudo-encoding payloads.

use anyhow::{Result, bail, ensure};

const HEADER: usize = 12;
const DESCRIPTOR: usize = 0x9c;
const MODE: usize = 0x1c;
const NAME_LEN: usize = 120;

pub const DISPLAY_FLAG_DYNAMIC_RESOLUTION: u32 = 1;
pub const DISPLAY_FLAG_FIXED_REFRESH: u32 = 2;
pub const MODE_FLAG_HDR: u32 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct RequestedMode {
    pub pixel_width: u32,
    pub pixel_height: u32,
    pub point_width: u32,
    pub point_height: u32,
    pub refresh_hz: f64,
    pub flags: u32,
}

impl RequestedMode {
    /// Retina factor (2.0 on a HiDPI Mac).
    pub fn scale(&self) -> f64 {
        if self.point_width == 0 {
            1.0
        } else {
            self.pixel_width as f64 / self.point_width as f64
        }
    }

    /// Output mode for the compositor: pixel size at the client's Retina scale.
    pub fn display_mode(&self) -> super::DisplayMode {
        super::fit_mode(
            self.pixel_width,
            self.pixel_height,
            self.refresh_hz,
            self.scale(),
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RequestedDisplay {
    pub name: String,
    pub flags: u32,
    pub reserved: u32,
    pub width_mm: f32,
    pub height_mm: f32,
    pub max_pixel_width: u32,
    pub max_pixel_height: u32,
    pub native_mode: u16,
    pub preferred_mode: u16,
    pub rotations: u32,
    pub modes: Vec<RequestedMode>,
}

impl RequestedDisplay {
    pub fn preferred(&self) -> Option<&RequestedMode> {
        self.modes.get(self.preferred_mode as usize)
    }

    pub fn dynamic_resolution(&self) -> bool {
        self.flags & DISPLAY_FLAG_DYNAMIC_RESOLUTION != 0
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayRequest {
    pub version: u16,
    pub flags: u32,
    pub displays: Vec<RequestedDisplay>,
}

/// Parses a whole msg 29, starting at the type byte.
pub fn parse_display_configuration(msg: &[u8]) -> Result<DisplayRequest> {
    ensure!(
        msg.len() >= HEADER && msg[0] == 29,
        "not a SetDisplayConfiguration message"
    );
    let u16_at = |o: usize| u16::from_be_bytes([msg[o], msg[o + 1]]);
    let u32_at = |o: usize| u32::from_be_bytes(msg[o..o + 4].try_into().unwrap());
    let size = u16_at(2) as usize;
    ensure!(
        size > 0xbf,
        "msg 29 size {size} below the server's minimum 192"
    );
    ensure!(
        msg.len() >= size + 4,
        "msg 29 truncated: {} < {}",
        msg.len(),
        size + 4
    );
    let count = u16_at(6);
    ensure!(count > 0, "msg 29 display count is zero");
    let end = size + 4;
    let mut off = HEADER;
    let mut displays = Vec::new();
    for i in 0..count.min(2) {
        ensure!(
            off + DESCRIPTOR <= end,
            "display {i} descriptor beyond end of message"
        );
        let d = &msg[off..];
        let du16 = |o: usize| u16::from_be_bytes([d[o], d[o + 1]]);
        let du32 = |o: usize| u32::from_be_bytes(d[o..o + 4].try_into().unwrap());
        let elem = du16(0) as usize;
        let n_modes = du16(0x9a) as usize;
        ensure!(
            elem >= DESCRIPTOR + n_modes * MODE,
            "display {i}: element size {elem} < {} modes",
            n_modes
        );
        ensure!(off + elem <= end, "display {i} goes beyond end of message");
        let name = &d[2..2 + NAME_LEN];
        let name =
            String::from_utf8_lossy(&name[..name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN)])
                .into_owned();
        let native_mode = du16(0x92);
        let preferred_mode = du16(0x94);
        if n_modes > 0 && (native_mode as usize >= n_modes || preferred_mode as usize >= n_modes) {
            bail!(
                "display {i}: mode index out of range ({native_mode}/{preferred_mode} of {n_modes})"
            );
        }
        let modes = (0..n_modes)
            .map(|m| {
                let o = DESCRIPTOR + m * MODE;
                RequestedMode {
                    pixel_width: du32(o),
                    pixel_height: du32(o + 4),
                    point_width: du32(o + 8),
                    point_height: du32(o + 12),
                    refresh_hz: f64::from_be_bytes(d[o + 16..o + 24].try_into().unwrap()),
                    flags: du32(o + 24),
                }
            })
            .collect();
        displays.push(RequestedDisplay {
            name,
            flags: du32(0x7a),
            reserved: du32(0x7e),
            width_mm: f32::from_bits(du32(0x82)),
            height_mm: f32::from_bits(du32(0x86)),
            max_pixel_width: du32(0x8a),
            max_pixel_height: du32(0x8e),
            native_mode,
            preferred_mode,
            rotations: du32(0x96),
            modes,
        });
        off += elem;
    }
    Ok(DisplayRequest {
        version: u16_at(4),
        flags: u32_at(8),
        displays,
    })
}

/// RFB PixelFormat as the agent fills it for 32-bit displays.
const PIXEL_FORMAT_32: [u8; 16] = [32, 32, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0];

/// Header flag bits shared by DisplayInfo and DisplayInfo2 (names from agent log context).
pub const INFO_FLAG_OBSCURED: u32 = 0x01;
pub const INFO_FLAG_SCREEN_LOCKED: u32 = 0x02;
pub const INFO_FLAG_ON_CONSOLE: u32 = 0x04;

/// One display in a DisplayInfo (1101) rect.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInfoEntry {
    pub display_id: u32,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

/// Payload after the 1101 rect header (rect w/h = framebuffer pixels).
pub fn encode_display_info(
    point_width: u16,
    point_height: u16,
    flags: u32,
    entries: &[DisplayInfoEntry],
) -> Vec<u8> {
    let mut v = Vec::with_capacity(10 + 28 * entries.len());
    v.extend_from_slice(&point_width.to_be_bytes());
    v.extend_from_slice(&point_height.to_be_bytes());
    v.extend_from_slice(&flags.to_be_bytes());
    v.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for e in entries {
        v.extend_from_slice(&e.display_id.to_be_bytes());
        v.extend_from_slice(&e.pixel_width.to_be_bytes());
        v.extend_from_slice(&e.pixel_height.to_be_bytes());
        v.extend_from_slice(&0u32.to_be_bytes());
        v.extend_from_slice(&PIXEL_FORMAT_32);
    }
    v
}

/// One display in a DisplayInfo2 (1105) rect. Rects are (top, left, bottom, right).
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInfo2Entry {
    pub backing_scale: f64,
    pub scale_factor: f64,
    pub display_id: u32,
    pub points: [i16; 4],
    pub framebuffer: [i16; 4],
    /// bit0 main display, bit1 mirrored, bit2 dynamic resolution active.
    pub flags: u32,
}

pub const SCREEN_FLAG_MAIN: u32 = 0x1;
/// The client exits dynamic resolution mode if this is clear while it is in it.
pub const SCREEN_FLAG_DYNAMIC_RESOLUTION: u32 = 0x4;
/// DisplayInfo2 header flags. The client only offers dynamic resolution with `VIRTUAL` set.
pub const DISPLAY_INFO2_VIRTUAL: u32 = 0x1;
pub const DISPLAY_INFO2_ON_CONSOLE: u32 = 0x4;

/// `u16 size` + payload after the 1105 rect header (rect w/h = framebuffer pixels).
/// `display_id` is the shared display, or `u32::MAX` when all displays are combined.
pub fn encode_display_info2(
    point_size: (u16, u16),
    pixel_size: (u16, u16),
    display_id: u32,
    flags: u32,
    entries: &[DisplayInfo2Entry],
) -> Vec<u8> {
    let size = 20 + 56 * entries.len();
    let mut v = Vec::with_capacity(2 + size);
    v.extend_from_slice(&(size as u16).to_be_bytes());
    v.extend_from_slice(&5u16.to_be_bytes());
    for x in [point_size.0, point_size.1, pixel_size.0, pixel_size.1] {
        v.extend_from_slice(&x.to_be_bytes());
    }
    v.extend_from_slice(&display_id.to_be_bytes());
    v.extend_from_slice(&flags.to_be_bytes());
    v.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for e in entries {
        v.extend_from_slice(&e.backing_scale.to_be_bytes());
        v.extend_from_slice(&e.scale_factor.to_be_bytes());
        v.extend_from_slice(&e.display_id.to_be_bytes());
        for x in e.points.iter().chain(&e.framebuffer) {
            v.extend_from_slice(&x.to_be_bytes());
        }
        v.extend_from_slice(&e.flags.to_be_bytes());
        v.extend_from_slice(&PIXEL_FORMAT_32);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPTURE: &[u8] = include_bytes!("../../tests/fixtures/msg29-0.bin");

    #[test]
    fn parses_capture() {
        let r = parse_display_configuration(CAPTURE).unwrap();
        assert_eq!((r.version, r.flags, r.displays.len()), (1, 0, 1));
        let d = &r.displays[0];
        assert_eq!(d.name, "Screen Sharing Virtual Display");
        assert_eq!((d.flags, d.reserved, d.rotations), (0, 0, 7));
        assert!(!d.dynamic_resolution());
        assert_eq!((d.max_pixel_width, d.max_pixel_height), (3840, 2160));
        // Client derives mm from the max pixel size at 264 ppi.
        assert!((d.width_mm - 3840.0 * 25.4 / 264.0).abs() < 0.01);
        assert!((d.height_mm - 2160.0 * 25.4 / 264.0).abs() < 0.01);
        assert_eq!((d.native_mode, d.preferred_mode), (0, 0));
        let sizes: Vec<_> = d
            .modes
            .iter()
            .map(|m| (m.pixel_width, m.pixel_height, m.point_width, m.point_height))
            .collect();
        assert_eq!(
            sizes,
            [
                (3840, 2160, 1920, 1080),
                (2880, 1800, 1440, 900),
                (3840, 2160, 1920, 1080),
                (2880, 1620, 1440, 810),
                (2624, 1696, 1312, 848)
            ]
        );
        assert!(
            d.modes
                .iter()
                .all(|m| m.refresh_hz == 60.0 && m.flags == 0 && m.scale() == 2.0)
        );
        let p = d.preferred().unwrap();
        assert_eq!(
            (p.pixel_width, p.pixel_height, p.scale()),
            (3840, 2160, 2.0)
        );
        assert_eq!(
            p.display_mode(),
            crate::display::DisplayMode {
                width: 3840,
                height: 2160,
                refresh_hz: 60.0,
                scale: 2.0
            }
        );
    }

    #[test]
    fn rejects_bad_messages() {
        assert!(parse_display_configuration(&CAPTURE[..200]).is_err());
        let mut m = CAPTURE.to_vec();
        m[7] = 0;
        assert!(parse_display_configuration(&m).is_err());
        let mut m = CAPTURE.to_vec();
        m[12] = 0;
        m[13] = 0x9c; // element too small for 5 modes
        assert!(parse_display_configuration(&m).is_err());
        let mut m = CAPTURE.to_vec();
        m[12 + 0x95] = 9; // preferred index out of range
        assert!(parse_display_configuration(&m).is_err());
    }

    #[test]
    fn display_info_layout() {
        let v = encode_display_info(
            1920,
            1080,
            0,
            &[DisplayInfoEntry {
                display_id: 1,
                pixel_width: 3840,
                pixel_height: 2160,
            }],
        );
        assert_eq!(v.len(), 10 + 28);
        assert_eq!(&v[..10], &[0x07, 0x80, 0x04, 0x38, 0, 0, 0, 0, 0, 1]);
        assert_eq!(&v[10..18], &[0, 0, 0, 1, 0x0f, 0x00, 0x08, 0x70]);
        assert_eq!(&v[22..26], &[32, 32, 0, 1]);
    }

    #[test]
    fn display_info2_layout() {
        let e = DisplayInfo2Entry {
            backing_scale: 2.0,
            scale_factor: 1.0,
            display_id: 1,
            points: [0, 0, 1080, 1920],
            framebuffer: [0, 0, 2160, 3840],
            flags: SCREEN_FLAG_MAIN,
        };
        let v = encode_display_info2((1920, 1080), (3840, 2160), 1, INFO_FLAG_ON_CONSOLE, &[e]);
        // Client checks: size > 19, count 1..25, size >= (20 + 56 * count) & !3.
        assert_eq!(u16::from_be_bytes([v[0], v[1]]) as usize, 20 + 56);
        assert_eq!(v.len(), 2 + 20 + 56);
        let p = &v[2..];
        assert_eq!(&p[..2], &[0, 5]);
        assert_eq!(&p[18..20], &[0, 1]);
        assert_eq!(f64::from_be_bytes(p[20..28].try_into().unwrap()), 2.0);
        assert_eq!(&p[36..40], &[0, 0, 0, 1]);
        assert_eq!(&p[44..48], &[0x04, 0x38, 0x07, 0x80]);
        assert_eq!(&p[56..60], &[0, 0, 0, 1]);
        assert_eq!(&p[60..64], &[32, 32, 0, 1]);
    }
}
