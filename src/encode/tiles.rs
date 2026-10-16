//! Backend-independent plumbing for the tiled sequence: change detection, picture planning,
//! keyframe requests, bitrate coalescing and in-order delivery of submitted frames.

use std::collections::VecDeque;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::plan::{Planner, TilePic};
use super::{EncodeTimings, EncodedFrame, EncoderConfig, TILE_COUNT, TileLayout};
use crate::capture::{Frame, TileDiffer};

pub(super) trait Backend {
    /// Starts coding `pics` (in order) from `frame`. `meta` is the result to complete: the
    /// backend fills in `tiles`, `timings.convert` and `timings.encode`.
    fn submit(&mut self, frame: &Frame, pics: &[TilePic], meta: EncodedFrame) -> Result<()>;
    /// The oldest finished submission.
    fn receive(&mut self, timeout: Option<Duration>) -> Result<Option<EncodedFrame>>;
    /// Delivers finished frames to `tx` directly (from another thread) instead of `receive`.
    /// Returns false if unsupported.
    fn set_output(&mut self, _tx: mpsc::Sender<Result<EncodedFrame>>) -> bool {
        false
    }
    /// Whether `submit` would not block.
    fn can_submit(&mut self) -> bool {
        true
    }
    /// Whether the backend detects changes itself (see `detect`).
    fn detects(&self) -> bool {
        false
    }
    /// Change detection done by the backend (e.g. fused with conversion). `None`: not supported.
    fn detect(&mut self, _frame: &Frame) -> Result<Option<[bool; TILE_COUNT]>> {
        Ok(None)
    }
    /// Makes the next `detect` report every tile as changed.
    fn reset_detection(&mut self) {}
    /// Applies a new total bitrate. Returns true if this restarts the sequence (needs a refresh).
    fn set_bitrate(&mut self, bps: u64) -> bool;
}

const RATE_MIN_CHANGE: f64 = 0.15;

/// Longest gap between coded pictures. The viewer FIRs ("No video displayed timeout") after
/// ~5 s without video, which a static screen would otherwise trigger.
const HEARTBEAT: Duration = Duration::from_secs(1);

struct RateState {
    current: Option<u64>,
    pending: Option<u64>,
    changed_at: Option<Instant>,
    min_interval: Duration,
}

impl RateState {
    fn take_due(&mut self) -> Option<u64> {
        let bps = self.pending?;
        if let Some(cur) = self.current
            && (bps as f64 - cur as f64).abs() < cur as f64 * RATE_MIN_CHANGE
        {
            self.pending = None;
            return None;
        }
        if self
            .changed_at
            .is_some_and(|t| t.elapsed() < self.min_interval)
        {
            return None;
        }
        self.pending = None;
        self.current = Some(bps);
        self.changed_at = Some(Instant::now());
        Some(bps)
    }
}

pub(super) struct Tiles<B> {
    pub(super) backend: B,
    layout: TileLayout,
    differ: Option<TileDiffer>,
    planner: Planner,
    rate: RateState,
    seq: u64,
    /// In submission order: `Some` for frames with nothing to code, `None` while the backend
    /// works on a frame. Unused once an output channel is set.
    pending: VecDeque<Option<EncodedFrame>>,
    /// Output channel for backends that can't deliver directly.
    output: Option<mpsc::Sender<Result<EncodedFrame>>>,
    direct: bool,
    last_coded: Instant,
}

impl<B: Backend> Tiles<B> {
    /// `rate_interval`: minimum spacing of bitrate changes.
    pub(super) fn new(
        backend: B,
        layout: TileLayout,
        cfg: &EncoderConfig,
        per_tile_skip: bool,
        rate_interval: Duration,
    ) -> Result<Self> {
        let differ = if cfg.skip_unchanged && !backend.detects() {
            Some(TileDiffer::new(layout).context("GPU tile diff")?)
        } else {
            None
        };
        Ok(Self {
            backend,
            layout,
            differ,
            planner: Planner::new(per_tile_skip),
            rate: RateState {
                current: cfg.bitrate_bps,
                pending: None,
                changed_at: None,
                min_interval: rate_interval,
            },
            seq: 0,
            pending: VecDeque::new(),
            output: None,
            direct: false,
            last_coded: Instant::now(),
        })
    }

    pub(super) fn layout(&self) -> TileLayout {
        self.layout
    }

    pub(super) fn request_keyframe(&mut self, tile: Option<usize>) {
        self.planner.request_keyframe(tile);
    }

    pub(super) fn set_bitrate(&mut self, bps: u64) {
        self.rate.pending = Some(bps);
    }

    pub(super) fn wants_frame(&self) -> bool {
        self.planner.keyframe_pending() || self.last_coded.elapsed() >= HEARTBEAT
    }

    pub(super) fn pending(&self) -> usize {
        self.pending.len()
    }

    pub(super) fn can_submit(&mut self) -> bool {
        self.backend.can_submit()
    }

    /// Sends coded frames to `tx` as soon as they finish; frames with nothing coded are
    /// dropped. `receive` then returns nothing.
    pub(super) fn set_output(&mut self, tx: mpsc::Sender<Result<EncodedFrame>>) {
        self.direct = self.backend.set_output(tx.clone());
        if !self.direct {
            self.output = Some(tx);
        }
    }

    pub(super) fn submit(&mut self, frame: &Frame) -> Result<()> {
        let b = &frame.buffer;
        if (b.width, b.height) != (self.layout.width, self.layout.height) {
            bail!(
                "frame {}x{} doesn't match encoder {:?}",
                b.width,
                b.height,
                self.layout
            );
        }
        let mut timings = EncodeTimings::default();
        let t0 = Instant::now();
        let detected = match self.backend.detect(frame) {
            Ok(d) => d,
            Err(e) => {
                self.resync();
                return Err(e);
            }
        };
        let changed = match (detected, &mut self.differ) {
            (Some(c), _) => (0..TILE_COUNT)
                .filter(|&t| c[t])
                .fold(0u8, |m, t| m | 1 << t),
            (None, Some(d)) => {
                let c = d.diff(b)?;
                (0..TILE_COUNT)
                    .filter(|&t| c[t])
                    .fold(0u8, |m, t| m | 1 << t)
            }
            (None, None) => self.layout.tiles_touched(&frame.damage),
        };
        timings.diff = t0.elapsed();
        if let Some(bps) = self.rate.take_due()
            && self.backend.set_bitrate(bps)
        {
            self.planner.request_keyframe(None);
        }
        let seq = self.seq;
        self.seq += 1;
        let changed_tiles = std::array::from_fn(|t| changed & (1 << t) != 0);
        if self.last_coded.elapsed() >= HEARTBEAT {
            self.planner.request_heartbeat();
        }
        let pics = self.planner.plan(changed);
        let frame_size = (self.layout.width as u16, self.layout.height as u16);
        let meta = EncodedFrame {
            seq,
            changed_tiles,
            tiles: Vec::new(),
            timings,
            presented: frame.presented,
            frame_size,
        };
        let streaming = self.direct || self.output.is_some();
        if pics.is_empty() {
            if !streaming {
                self.pending.push_back(Some(meta));
            }
            return Ok(());
        }
        if let Err(e) = self.backend.submit(frame, &pics, meta) {
            self.resync();
            return Err(e);
        }
        self.last_coded = Instant::now();
        if let Some(tx) = &self.output {
            while let Some(f) = self.backend.receive(Some(Duration::ZERO))? {
                let _ = tx.send(Ok(f));
            }
        } else if !self.direct {
            self.pending.push_back(None);
        }
        Ok(())
    }

    pub(super) fn receive(&mut self, timeout: Option<Duration>) -> Result<Option<EncodedFrame>> {
        let Some(front) = self.pending.front_mut() else {
            return Ok(None);
        };
        if let Some(f) = front.take() {
            self.pending.pop_front();
            return Ok(Some(f));
        }
        match self.backend.receive(timeout) {
            Ok(Some(f)) => {
                self.pending.pop_front();
                Ok(Some(f))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                self.pending.pop_front();
                self.resync();
                Err(e)
            }
        }
    }

    // References and the diff reference may be out of step after an error; restart cleanly.
    fn resync(&mut self) {
        self.planner.request_keyframe(None);
        self.backend.reset_detection();
        if let Some(d) = &mut self.differ {
            d.reset();
        }
    }
}
