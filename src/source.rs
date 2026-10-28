use std::sync::Arc;

pub type FrameSourceFactory = Arc<dyn Fn() -> anyhow::Result<Box<dyn FrameSource>> + Send + Sync>;
pub type CursorSourceFactory = Arc<dyn Fn() -> Box<dyn CursorSource> + Send + Sync>;

/// Pixels are `0x00RRGGBB`, row-major, `width * height` long.
pub struct Frame {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u32>,
}

pub trait FrameSource: Send {
    fn size(&self) -> (u16, u16);
    /// Advance to the next frame. Returns false if nothing changed.
    fn advance(&mut self) -> bool;
    fn frame(&self) -> &Frame;
    /// Regions changed by the last `advance` that returned true, as (x, y, w, h).
    fn damage(&self) -> Vec<(u16, u16, u16, u16)> {
        let (w, h) = self.size();
        vec![(0, 0, w, h)]
    }
}

pub struct TestPattern {
    frame: Frame,
    counter: u64,
}

const BOX: usize = 160;
const DIGIT_SCALE: usize = 12;

// 3x5 glyphs, one row per nibble (bit 2 = leftmost column).
const FONT: [[u8; 5]; 10] = [
    [7, 5, 5, 5, 7],
    [2, 6, 2, 2, 7],
    [7, 1, 7, 4, 7],
    [7, 1, 7, 1, 7],
    [5, 5, 7, 1, 1],
    [7, 4, 7, 1, 7],
    [7, 4, 7, 5, 7],
    [7, 1, 1, 1, 1],
    [7, 5, 7, 5, 7],
    [7, 5, 7, 1, 7],
];

impl TestPattern {
    pub fn new(width: u16, height: u16) -> Self {
        let mut tp = Self {
            frame: Frame {
                width,
                height,
                pixels: vec![0; width as usize * height as usize],
            },
            counter: 0,
        };
        tp.render();
        tp
    }

    fn render(&mut self) {
        let (w, h) = (self.frame.width as usize, self.frame.height as usize);
        let t = self.counter as usize;
        let px = &mut self.frame.pixels;

        for y in 0..h {
            let g = ((y * 255 / h.max(1) + t) & 0xff) as u32;
            let row = &mut px[y * w..(y + 1) * w];
            for (x, p) in row.iter_mut().enumerate() {
                let r = ((x * 255 / w.max(1) + 2 * t) & 0xff) as u32;
                *p = (r << 16) | (g << 8) | 0x60;
            }
        }

        let bx = bounce(t * 7, w.saturating_sub(BOX));
        let by = bounce(t * 5, h.saturating_sub(BOX));
        fill(px, w, bx, by, BOX, BOX, 0xffffff);

        let digits = self.counter.to_string();
        let cell = 4 * DIGIT_SCALE;
        let (ox, oy) = (32, 32);
        fill(
            px,
            w,
            ox - 8,
            oy - 8,
            digits.len() * cell + 12,
            5 * DIGIT_SCALE + 16,
            0xffd000,
        );
        for (i, d) in digits.bytes().enumerate() {
            let glyph = FONT[(d - b'0') as usize];
            for (row, bits) in glyph.iter().enumerate() {
                for col in 0..3 {
                    if bits & (4 >> col) != 0 {
                        fill(
                            px,
                            w,
                            ox + i * cell + col * DIGIT_SCALE,
                            oy + row * DIGIT_SCALE,
                            DIGIT_SCALE,
                            DIGIT_SCALE,
                            0x000000,
                        );
                    }
                }
            }
        }
    }
}

fn bounce(pos: usize, span: usize) -> usize {
    if span == 0 {
        return 0;
    }
    let p = pos % (2 * span);
    if p < span { p } else { 2 * span - p }
}

fn fill(px: &mut [u32], stride: usize, x: usize, y: usize, w: usize, h: usize, colour: u32) {
    let rows = px.len() / stride.max(1);
    for yy in y..(y + h).min(rows) {
        let start = yy * stride + x.min(stride);
        let end = yy * stride + (x + w).min(stride);
        px[start..end].fill(colour);
    }
}

impl FrameSource for TestPattern {
    fn size(&self) -> (u16, u16) {
        (self.frame.width, self.frame.height)
    }

    fn advance(&mut self) -> bool {
        self.counter += 1;
        self.render();
        true
    }

    fn frame(&self) -> &Frame {
        &self.frame
    }
}

/// Cursor pixels are `0xAARRGGBB`, straight (not premultiplied) alpha.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorImage {
    /// Changes whenever the image changes.
    pub serial: u64,
    pub width: u16,
    pub height: u16,
    pub hotspot: (u16, u16),
    pub pixels: Vec<u32>,
}

pub trait CursorSource: Send + Sync {
    fn image(&self) -> Option<&CursorImage>;
    /// Server-driven position, if the server moved the cursor itself.
    fn position(&self) -> Option<(u16, u16)>;
}

/// A black arrow with a white outline.
pub struct TestCursor {
    image: CursorImage,
}

impl Default for TestCursor {
    fn default() -> Self {
        const W: usize = 12;
        const H: usize = 19;
        let mut pixels = vec![0u32; W * H];
        for y in 0..H {
            let len = if y < 12 { y + 1 } else { 0 };
            for x in 0..len.min(W) {
                let edge = x == 0 || x + 1 == len || y == 11;
                pixels[y * W + x] = if edge { 0xffffffff } else { 0xff000000 };
            }
        }
        for y in 12..H {
            let x0 = 4 + (y - 12) / 2;
            for x in x0..(x0 + 3).min(W) {
                let edge = x == x0 || x == x0 + 2 || y + 1 == H;
                pixels[y * W + x] = if edge { 0xffffffff } else { 0xff000000 };
            }
        }
        Self {
            image: CursorImage {
                serial: 1,
                width: W as u16,
                height: H as u16,
                hotspot: (0, 0),
                pixels,
            },
        }
    }
}

impl CursorSource for TestCursor {
    fn image(&self) -> Option<&CursorImage> {
        Some(&self.image)
    }

    fn position(&self) -> Option<(u16, u16)> {
        None
    }
}
