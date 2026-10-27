//! Constrained Baseline packed headers, matching the VA-API sequence and picture parameters.

use super::hevc::BitWriter;

/// H.264 Annex A limits both macroblock area and each dimension squared to eight
/// times MaxFS. At ten pictures per second these levels also cover the MB/s limit.
pub fn level(width: u32, height: u32) -> Option<u8> {
    if width == 0 || height == 0 || !width.is_multiple_of(16) || !height.is_multiple_of(16) {
        return None;
    }
    let (w, h) = (u64::from(width / 16), u64::from(height / 16));
    [(40, 8192), (50, 22080), (51, 36864)]
        .into_iter()
        .find(|(_, max_fs)| w * h <= *max_fs && w * w <= 8 * max_fs && h * h <= 8 * max_fs)
        .map(|(level, _)| level)
}

pub fn parameter_sets(width: u32, height: u32, level: u8) -> [Vec<u8>; 2] {
    let mut s = BitWriter::default();
    s.u(8, 66); // Baseline profile, with constraint_set0/1 for Constrained Baseline.
    s.u(8, 0xc0);
    s.u(8, u64::from(level));
    s.ue(0);
    s.ue(4); // Eight-bit frame_num, POC type 2, one short-term reference, no B pictures.
    s.ue(2);
    s.ue(1);
    s.flag(false);
    s.ue(width / 16 - 1);
    s.ue(height / 16 - 1);
    s.flag(true);
    s.flag(true);
    // MS-RDPEGFX requires aligned bitstream dimensions and region-mask cropping.
    s.flag(false);
    s.flag(true); // VUI, full-range BT.709 matrix with sRGB transfer.
    s.flag(false);
    s.flag(false);
    s.flag(true);
    s.u(3, 5);
    s.flag(true);
    s.flag(true);
    s.u(8, 1);
    s.u(8, 13);
    s.u(8, 1);
    s.flag(false);
    s.flag(true);
    s.u(32, 1);
    s.u(32, 20); // Ten progressive pictures per second.
    s.flag(true);
    s.flag(false);
    s.flag(false);
    s.flag(false);
    s.flag(true);
    s.flag(true);
    s.ue(0);
    s.ue(0);
    s.ue(16);
    s.ue(16);
    s.ue(0);
    s.ue(1);
    s.trailing();
    let mut p = BitWriter::default();
    p.ue(0);
    p.ue(0);
    p.flag(false); // CAVLC and progressive frames.
    p.flag(false);
    p.ue(0);
    p.ue(0);
    p.ue(0);
    p.flag(false);
    p.u(2, 0);
    p.se(-2); // QP 24.
    p.se(0);
    p.se(0);
    p.flag(true);
    p.flag(false);
    p.flag(false);
    p.trailing();
    [nal(0x67, &s.into_bytes()), nal(0x68, &p.into_bytes())]
}

/// The slice header is not byte-aligned with CAVLC. `bit_length` excludes the zero
/// padding in the last byte, which the driver replaces with encoded slice data.
pub fn slice_header(number: u16, idr: bool) -> (Vec<u8>, u32) {
    let mut w = BitWriter::default();
    w.ue(0);
    w.ue(if idr { 2 } else { 0 });
    w.ue(0);
    w.u(8, u64::from(number));
    if idr {
        w.ue(0);
    } else {
        w.flag(false);
        w.flag(false);
    }
    if idr {
        w.flag(false);
        w.flag(false);
    } else {
        w.flag(false);
    }
    w.se(0);
    w.ue(0);
    w.se(0);
    w.se(0);
    let padding = (8 - w.bit_len() % 8) % 8;
    w.pad_zero();
    let data = nal(if idr { 0x65 } else { 0x41 }, &w.into_bytes());
    let bits = data.len() as u32 * 8 - padding;
    (data, bits)
}

fn nal(header: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 1, header];
    let mut zeros = 0;
    for &b in data {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_covers_phone_sizes_and_the_default_desktop() {
        assert_eq!(level(848, 1584), Some(40));
        assert_eq!(level(1584, 848), Some(40));
        assert_eq!(level(3456, 2160), Some(51));
    }

    #[test]
    fn thin_desktops_respect_dimension_limits_as_well_as_area() {
        assert_eq!(level(4096, 208), Some(40));
        assert_eq!(level(4320, 208), Some(50));
        assert_eq!(level(8192, 208), Some(51));
        assert_eq!(level(208, 8192), Some(51));
    }

    #[test]
    fn unaligned_or_oversized_pictures_have_no_hardware_level() {
        for (w, h) in [(0, 16), (836, 1580), (8192, 8192), (8704, 208)] {
            assert_eq!(level(w, h), None);
        }
    }
}
