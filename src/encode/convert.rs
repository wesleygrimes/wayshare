//! BGRX (DRM XRGB8888) to planar YUV with BT.709 coefficients, for software encoders.

use super::Chroma;

#[derive(Clone, Copy, Debug)]
pub struct Coeffs {
    y: [i32; 3],
    u: [i32; 3],
    v: [i32; 3],
    y_off: i32,
}

const SHIFT: u32 = 16;

impl Coeffs {
    pub fn bt709(full_range: bool) -> Self {
        let (ys, cs, y_off) = if full_range {
            (1.0, 1.0, 0)
        } else {
            (219.0 / 255.0, 224.0 / 255.0, 16)
        };
        let f = |x: f64| (x * (1 << SHIFT) as f64).round() as i32;
        let (kr, kb) = (0.2126, 0.0722);
        let kg = 1.0 - kr - kb;
        Self {
            y: [f(kr * ys), f(kg * ys), f(kb * ys)],
            u: [
                f(-kr / (2.0 * (1.0 - kb)) * cs),
                f(-kg / (2.0 * (1.0 - kb)) * cs),
                f(0.5 * cs),
            ],
            v: [
                f(0.5 * cs),
                f(-kg / (2.0 * (1.0 - kr)) * cs),
                f(-kb / (2.0 * (1.0 - kr)) * cs),
            ],
            y_off,
        }
    }

    #[inline(always)]
    fn luma(&self, r: i32, g: i32, b: i32) -> u8 {
        (((self.y[0] * r + self.y[1] * g + self.y[2] * b + (1 << (SHIFT - 1))) >> SHIFT)
            + self.y_off) as u8
    }

    #[inline(always)]
    fn chroma(&self, r: i32, g: i32, b: i32) -> (u8, u8) {
        let c = |k: [i32; 3]| {
            (((k[0] * r + k[1] * g + k[2] * b + (1 << (SHIFT - 1))) >> SHIFT) + 128).clamp(0, 255)
                as u8
        };
        (c(self.u), c(self.v))
    }
}

pub struct Planes<'a> {
    pub y: &'a mut [u8],
    pub u: &'a mut [u8],
    pub v: &'a mut [u8],
    /// Luma and chroma row strides in bytes.
    pub y_stride: usize,
    pub c_stride: usize,
}

/// Converts `rows` rows of `width` pixels. For 4:2:0, `rows` must be even except at the end
/// of the picture, where the last row is reused.
pub fn bgrx_to_yuv(
    src: &[u8],
    src_stride: usize,
    width: usize,
    rows: usize,
    chroma: Chroma,
    k: &Coeffs,
    out: Planes<'_>,
) {
    let px = |row: &[u8], x: usize| {
        (
            row[4 * x + 2] as i32,
            row[4 * x + 1] as i32,
            row[4 * x] as i32,
        )
    };
    for y in 0..rows {
        let s = &src[y * src_stride..y * src_stride + width * 4];
        let d = &mut out.y[y * out.y_stride..y * out.y_stride + width];
        for (x, o) in d.iter_mut().enumerate() {
            let (r, g, b) = px(s, x);
            *o = k.luma(r, g, b);
        }
    }
    match chroma {
        Chroma::Yuv444 => {
            for y in 0..rows {
                let s = &src[y * src_stride..y * src_stride + width * 4];
                let (u, v) = (
                    &mut out.u[y * out.c_stride..y * out.c_stride + width],
                    &mut out.v[y * out.c_stride..y * out.c_stride + width],
                );
                for x in 0..width {
                    let (r, g, b) = px(s, x);
                    (u[x], v[x]) = k.chroma(r, g, b);
                }
            }
        }
        Chroma::Yuv420 => {
            let cw = width.div_ceil(2);
            for cy in 0..rows.div_ceil(2) {
                let r0 = 2 * cy;
                let r1 = (r0 + 1).min(rows - 1);
                let (s0, s1) = (&src[r0 * src_stride..], &src[r1 * src_stride..]);
                for cx in 0..cw {
                    let (x0, x1) = (2 * cx, (2 * cx + 1).min(width - 1));
                    let (mut r, mut g, mut b) = (0, 0, 0);
                    for (row, x) in [(s0, x0), (s0, x1), (s1, x0), (s1, x1)] {
                        let p = px(row, x);
                        r += p.0;
                        g += p.1;
                        b += p.2;
                    }
                    let (u, v) = k.chroma((r + 2) >> 2, (g + 2) >> 2, (b + 2) >> 2);
                    out.u[cy * out.c_stride + cx] = u;
                    out.v[cy * out.c_stride + cx] = v;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(bgrx: [u8; 4], full: bool) -> (u8, u8, u8) {
        let (mut y, mut u, mut v) = ([0u8; 1], [0u8; 1], [0u8; 1]);
        let k = Coeffs::bt709(full);
        bgrx_to_yuv(
            &bgrx,
            4,
            1,
            1,
            Chroma::Yuv444,
            &k,
            Planes {
                y: &mut y,
                u: &mut u,
                v: &mut v,
                y_stride: 1,
                c_stride: 1,
            },
        );
        (y[0], u[0], v[0])
    }

    #[test]
    fn reference_colours() {
        assert_eq!(one([255, 255, 255, 0], true), (255, 128, 128));
        assert_eq!(one([0, 0, 0, 0], true), (0, 128, 128));
        assert_eq!(one([0, 0, 255, 0], true), (54, 99, 255)); // red
        assert_eq!(one([255, 255, 255, 0], false), (235, 128, 128));
        assert_eq!(one([0, 0, 0, 0], false), (16, 128, 128));
    }

    #[test]
    fn subsampling_averages() {
        let k = Coeffs::bt709(true);
        // 2x2 of red and blue columns.
        let src = [0, 0, 255, 0, 255, 0, 0, 0, 0, 0, 255, 0, 255, 0, 0, 0];
        let (mut y, mut u, mut v) = ([0u8; 4], [0u8; 1], [0u8; 1]);
        bgrx_to_yuv(
            &src,
            8,
            2,
            2,
            Chroma::Yuv420,
            &k,
            Planes {
                y: &mut y,
                u: &mut u,
                v: &mut v,
                y_stride: 2,
                c_stride: 1,
            },
        );
        let (eu, ev) = k.chroma(128, 0, 128);
        assert_eq!((u[0], v[0]), (eu, ev));
    }
}
