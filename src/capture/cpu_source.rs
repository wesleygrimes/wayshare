//! CPU frames for Standard (non-HP) mode. Uses system memory for linear offers and GBM
//! for tiled offers. Converts to `source::Frame` pixels while finding changed rows.

use std::time::Duration;

use anyhow::{Result, bail};

use super::{CaptureOptions, Capturer, Memory};
use crate::encode::Rect;
use crate::source::{Frame, FrameSource};

pub struct CaptureSource {
    cap: Capturer,
    frame: Frame,
    dirty: Vec<Rect>,
    poll: Duration,
}

impl CaptureSource {
    /// Blocks (up to 5 s) for the first frame so `size` is known.
    pub fn new(output: Option<&str>) -> Result<Self> {
        let cap = Capturer::with_options(
            output,
            CaptureOptions {
                memory: Memory::System,
                ..Default::default()
            },
        )?;
        let mut s = Self {
            cap,
            frame: Frame {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            },
            dirty: Vec::new(),
            poll: Duration::from_millis(1),
        };
        for _ in 0..50 {
            if s.try_advance()? {
                return Ok(s);
            }
        }
        bail!("no frame from compositor within 5 s")
    }

    /// How long `advance` waits for a new frame before reporting no change.
    pub fn set_poll_timeout(&mut self, poll: Duration) {
        self.poll = poll;
    }

    /// Regions that changed in the last `advance` that returned true.
    pub fn dirty_rects(&self) -> &[Rect] {
        &self.dirty
    }

    fn try_advance(&mut self) -> Result<bool> {
        let timeout = if self.frame.width == 0 {
            Duration::from_millis(100)
        } else {
            self.poll
        };
        let Some(captured) = self.cap.next_frame(Some(timeout))? else {
            return Ok(false);
        };
        let buf = &captured.buffer;
        let (w, h) = (buf.width as usize, buf.height as usize);
        let resized = (w, h) != (self.frame.width as usize, self.frame.height as usize);
        if resized {
            let (Ok(w16), Ok(h16)) = (u16::try_from(w), u16::try_from(h)) else {
                bail!("{w}x{h} exceeds the RFB frame size limit");
            };
            self.frame = Frame {
                width: w16,
                height: h16,
                pixels: vec![0; w * h],
            };
        }

        buf.with_read(|guard| copy_frame(&mut self.frame, &mut self.dirty, guard, resized))?;
        Ok(!self.dirty.is_empty())
    }
}

fn copy_frame(
    frame: &mut Frame,
    dirty: &mut Vec<Rect>,
    guard: &super::ReadGuard<'_>,
    resized: bool,
) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    dirty.clear();
    let mut run: Option<Rect> = None;
    for y in 0..h {
        let src = guard.row(y, w * 4).as_chunks::<4>().0;
        let dst = &mut frame.pixels[y * w..(y + 1) * w];
        let span = copy_row(dst, src, resized);
        match (span, &mut run) {
            (Some((x0, x1)), Some(r)) => {
                let right = (r.x + r.width).max(x1 as u32);
                r.x = r.x.min(x0 as u32);
                r.width = right - r.x;
                r.height += 1;
            }
            (Some((x0, x1)), None) => {
                run = Some(Rect {
                    x: x0 as u32,
                    y: y as u32,
                    width: (x1 - x0) as u32,
                    height: 1,
                });
            }
            (None, _) => dirty.extend(run.take()),
        }
    }
    dirty.extend(run);
}

/// Converts XRGB8888 into `0x00RRGGBB`, returning the changed column range `[x0, x1)`.
fn copy_row(dst: &mut [u32], src: &[[u8; 4]], force: bool) -> Option<(usize, usize)> {
    let px = |s: &[u8; 4]| u32::from_le_bytes(*s) & 0x00ff_ffff;
    let x0 = if force {
        0
    } else {
        dst.iter().zip(src).position(|(d, s)| *d != px(s))?
    };
    let x1 = if force {
        dst.len()
    } else {
        dst.iter().zip(src).rposition(|(d, s)| *d != px(s))? + 1
    };
    for (d, s) in dst[x0..x1].iter_mut().zip(&src[x0..x1]) {
        *d = px(s);
    }
    Some((x0, x1))
}

impl FrameSource for CaptureSource {
    fn size(&self) -> (u16, u16) {
        (self.frame.width, self.frame.height)
    }

    fn advance(&mut self) -> bool {
        self.try_advance().unwrap_or_else(|e| {
            tracing::error!("capture: {e:#}");
            false
        })
    }

    fn frame(&self) -> &Frame {
        &self.frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padded_stripe_frame_converts_exact_pixels() {
        let (width, height, stride) = (7, 6, 64);
        let colors = [0x00123456u32, 0x00abcdef, 0x00ff0080];
        let mut bytes = vec![0xe7; stride * height];
        let mut expected = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let pixel = colors[(x + y) % colors.len()];
                let offset = y * stride + x * 4;
                bytes[offset..offset + 4].copy_from_slice(&(pixel | 0xff000000).to_le_bytes());
                expected.push(pixel);
            }
        }
        let guard = super::super::ReadGuard::borrowed(&bytes, stride);
        let mut frame = Frame {
            width: width as u16,
            height: height as u16,
            pixels: vec![0; width * height],
        };
        let mut dirty = Vec::new();
        copy_frame(&mut frame, &mut dirty, &guard, true);
        assert_eq!(frame.pixels, expected);
        assert_eq!(dirty.len(), 1);
        assert_eq!(
            (dirty[0].x, dirty[0].y, dirty[0].width, dirty[0].height),
            (0, 0, width as u32, height as u32)
        );
        copy_frame(&mut frame, &mut dirty, &guard, false);
        assert!(dirty.is_empty());
        assert_eq!(frame.pixels, expected);
    }
}
