//! HEVC encoding of captured frames as four independent horizontal-strip streams (HP mode tiles).

pub mod convert;
pub mod hevc;
pub mod nal;
pub mod plan;
pub mod rate;
#[cfg(target_os = "linux")]
mod tiles;
#[cfg(target_os = "linux")]
pub mod vaapi;
#[cfg(target_os = "linux")]
pub mod x265;

pub const TILE_COUNT: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Frame of `width x height` split into `TILE_COUNT` strips of `tile_height` coded rows each.
/// The last strip is padded below the frame when `height` doesn't divide evenly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileLayout {
    pub width: u32,
    pub height: u32,
    pub tile_height: u32,
}

impl TileLayout {
    pub fn new(width: u32, height: u32) -> Self {
        let tile_height = height.div_ceil(TILE_COUNT as u32).next_multiple_of(16);
        Self {
            width,
            height,
            tile_height,
        }
    }

    pub fn tile_y(&self, tile: usize) -> u32 {
        tile as u32 * self.tile_height
    }

    /// Rows of real frame content in `tile` (less than `tile_height` for a padded last strip).
    pub fn tile_rows(&self, tile: usize) -> u32 {
        self.height
            .saturating_sub(self.tile_y(tile))
            .min(self.tile_height)
    }

    /// Bitmask of tiles intersecting any of `rects`.
    pub fn tiles_touched(&self, rects: &[Rect]) -> u8 {
        let mut mask = 0;
        for r in rects.iter().filter(|r| r.width > 0 && r.height > 0) {
            let first = (r.y / self.tile_height) as usize;
            let last = ((r.y + r.height - 1) / self.tile_height) as usize;
            for t in first..=last.min(TILE_COUNT - 1) {
                mask |= 1 << t;
            }
        }
        mask
    }
}

/// VUI colour description. The matrix is always BT.709 (the GPU conversion uses it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorTags {
    pub primaries: Primaries,
    pub transfer: Transfer,
    pub full_range: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Primaries {
    Bt709,
    DisplayP3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transfer {
    Bt709,
    /// IEC 61966-2-1, what Hyprland outputs and Apple tags.
    Srgb,
}

impl Default for ColorTags {
    fn default() -> Self {
        Self {
            primaries: Primaries::Bt709,
            transfer: Transfer::Srgb,
            full_range: true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chroma {
    Yuv420,
    /// x265 only (Main 4:4:4 8-bit).
    Yuv444,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderBackend {
    Vaapi,
    /// Not implemented yet; `auto` never picks it.
    Nvenc,
    X265,
}

#[derive(Clone, Debug)]
pub struct X265Options {
    pub preset: String,
    /// Worker threads per tile encoder; `None` = available cores / 4.
    pub threads_per_tile: Option<u32>,
}

impl Default for X265Options {
    fn default() -> Self {
        Self {
            preset: "ultrafast".into(),
            threads_per_tile: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    /// `None` = `EncoderBackend::auto`.
    pub backend: Option<EncoderBackend>,
    pub chroma: Chroma,
    pub x265: X265Options,
    pub fps: u32,
    /// Total across all tiles. `None` selects constant QP.
    pub bitrate_bps: Option<u64>,
    pub qp: u8,
    pub color: ColorTags,
    /// Detect unchanged tiles and leave them out of `EncodedFrame::tiles`.
    pub skip_unchanged: bool,
    /// Sample adaptive offset. Off by default: on radeonsi (VCN 2-4) SAO forces the slower
    /// "balance" preset instead of "speed".
    pub sao: bool,
    /// Frames allowed on the GPU encoder at once (1-2). 2 overlaps the next frame's
    /// diff/convert with this frame's encode; on the 680M under compositor load the overlap
    /// slows both, so 1 gives lower latency.
    pub max_in_flight: u8,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            backend: None,
            chroma: Chroma::Yuv420,
            x265: X265Options::default(),
            fps: 60,
            bitrate_bps: Some(40_000_000),
            qp: 24,
            color: ColorTags::default(),
            skip_unchanged: true,
            sao: false,
            max_in_flight: 1,
        }
    }
}

#[derive(Clone, Debug)]
pub struct EncodedTile {
    pub index: u8,
    pub keyframe: bool,
    /// Annex B access unit (start-code prefixed NALs, SEI removed). IDRs carry VPS/SPS/PPS.
    pub data: Vec<u8>,
}

/// Where an `encode` call spent its time.
#[derive(Clone, Copy, Debug, Default)]
pub struct EncodeTimings {
    /// Change detection.
    pub diff: std::time::Duration,
    /// Colour conversion / crop into encoder input.
    pub convert: std::time::Duration,
    /// Submitting pictures until the last coded picture is read back.
    pub encode: std::time::Duration,
}

/// One captured frame's output. Tiles whose content didn't change are absent from `tiles`
/// unless a keyframe was requested.
#[derive(Clone, Debug)]
pub struct EncodedFrame {
    pub seq: u64,
    pub changed_tiles: [bool; TILE_COUNT],
    pub tiles: Vec<EncodedTile>,
    pub timings: EncodeTimings,
    /// The source frame's compositor presentation time (CLOCK_MONOTONIC).
    pub presented: Option<std::time::Duration>,
    /// Picture size (unpadded) the tiles were coded at.
    pub frame_size: (u16, u16),
}

pub trait VideoEncoder {
    /// Backend-specific input (e.g. a dmabuf capture frame for VAAPI).
    type Frame;

    fn layout(&self) -> TileLayout;
    /// Force an IDR on the next encode for one tile, or all tiles with `None` (FIR).
    fn request_keyframe(&mut self, tile: Option<usize>);
    /// Target total bitrate (RTCP rate control). Takes effect within a frame or two.
    fn set_bitrate(&mut self, bps: u64);
    /// Starts encoding `frame`. The frame's buffer is no longer needed when this returns.
    /// May block while the maximum number of frames is in flight.
    fn submit(&mut self, frame: &Self::Frame) -> anyhow::Result<()>;
    /// The next finished frame, in submission order. `timeout` `None` blocks; `Some(ZERO)` polls.
    fn receive(
        &mut self,
        timeout: Option<std::time::Duration>,
    ) -> anyhow::Result<Option<EncodedFrame>>;
    /// Frames submitted and not yet received.
    fn pending(&self) -> usize;
    /// Whether `submit` would start right away instead of waiting for the encoder. Callers that
    /// care about latency hold only the newest frame until this is true (skipped frames lose
    /// nothing: change detection compares against the last submitted frame).
    fn can_submit(&mut self) -> bool;
    /// Whether to submit again even without a new capture (resubmit the last frame): a
    /// keyframe request is pending, or the stream is due a heartbeat picture.
    fn wants_frame(&self) -> bool;
    /// Streams coded frames to `tx` as they finish (possibly from another thread), instead of
    /// `receive`. Frames with nothing to code are not sent.
    fn set_output(&mut self, tx: std::sync::mpsc::Sender<anyhow::Result<EncodedFrame>>);

    /// Synchronous `submit` + `receive`, for callers that keep nothing in flight.
    fn encode(&mut self, frame: &Self::Frame) -> anyhow::Result<EncodedFrame> {
        debug_assert_eq!(self.pending(), 0);
        self.submit(frame)?;
        loop {
            if let Some(f) = self.receive(None)? {
                return Ok(f);
            }
        }
    }
}

#[cfg(target_os = "linux")]
impl EncoderBackend {
    /// VAAPI if the GPU encodes HEVC (and 4:2:0 is wanted), then NVENC once implemented,
    /// else x265. `cfg.backend` overrides.
    pub fn auto(render_node: &std::path::Path, cfg: &EncoderConfig) -> Self {
        if let Some(b) = cfg.backend {
            return b;
        }
        if cfg.chroma == Chroma::Yuv420 && vaapi::hevc_encode_available(render_node) {
            Self::Vaapi
        } else if cfg.chroma == Chroma::Yuv420 && nvenc_available() {
            Self::Nvenc
        } else {
            Self::X265
        }
    }

    pub fn check(self, render_node: &std::path::Path) -> anyhow::Result<()> {
        match self {
            Self::Vaapi => anyhow::ensure!(
                vaapi::hevc_encode_available(render_node),
                "no VAAPI HEVC encoder on {}",
                render_node.display()
            ),
            Self::X265 => x265::check().map_err(anyhow::Error::msg)?,
            Self::Nvenc => anyhow::bail!("NVENC backend not implemented"),
        }
        Ok(())
    }

    /// Capture buffer placement this backend reads efficiently.
    pub fn capture_memory(self) -> crate::capture::Memory {
        match self {
            Self::X265 => crate::capture::Memory::System,
            Self::Vaapi | Self::Nvenc => crate::capture::Memory::Gpu,
        }
    }

    pub fn open(
        self,
        render_node: &std::path::Path,
        width: u32,
        height: u32,
        cfg: &EncoderConfig,
    ) -> anyhow::Result<Box<dyn VideoEncoder<Frame = crate::capture::Frame> + Send>> {
        Ok(match self {
            Self::Vaapi => Box::new(vaapi::VaapiEncoder::new(render_node, width, height, cfg)?),
            Self::X265 => Box::new(x265::X265Encoder::new(width, height, cfg)?),
            Self::Nvenc => anyhow::bail!("NVENC backend not implemented"),
        })
    }
}

/// Hook for the NVENC backend.
#[cfg(target_os = "linux")]
fn nvenc_available() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_3456x2160() {
        let l = TileLayout::new(3456, 2160);
        assert_eq!(l.tile_height, 544);
        assert_eq!(l.tile_rows(3), 528);
        assert_eq!(l.tile_rows(0), 544);
    }

    #[test]
    fn layout_1080p() {
        let l = TileLayout::new(1920, 1080);
        assert_eq!(l.tile_height, 272);
        assert_eq!(l.tile_rows(3), 264);
    }

    #[test]
    fn damage_mask() {
        let l = TileLayout::new(3456, 2160);
        let r = |y, h| Rect {
            x: 0,
            y,
            width: 10,
            height: h,
        };
        assert_eq!(l.tiles_touched(&[]), 0);
        assert_eq!(l.tiles_touched(&[r(0, 1)]), 0b0001);
        assert_eq!(l.tiles_touched(&[r(543, 2)]), 0b0011);
        assert_eq!(l.tiles_touched(&[r(2000, 160)]), 0b1000);
        assert_eq!(l.tiles_touched(&[r(0, 2160)]), 0b1111);
    }
}
