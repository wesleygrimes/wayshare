//! Zero-copy capture of a Wayland output into GPU dmabufs.
//!
//! Uses ext-image-copy-capture-v1 when the compositor offers it, else wlr-screencopy-v1 (v3,
//! linux_dmabuf). Frames are delivered only when the output changed; damage is reported per frame.

mod alloc;
mod cpu_source;
mod diff;
mod gl;
mod mapping;

use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_buffer::WlBuffer, wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self as ext_frame, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{self as ext_mgr, ExtImageCopyCaptureManagerV1},
    ext_image_copy_capture_session_v1::{self as ext_session, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self as dmabuf_params, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self as wlr_frame, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

use crate::encode::Rect;
use alloc::Allocator;
pub use alloc::Memory;
pub use cpu_source::CaptureSource;
pub use diff::{Nv12Target, TileDiffer};
pub use gl::PlaneDesc;
use mapping::Mapping;
pub use mapping::ReadGuard;

const MAX_BUFFERS: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    ExtImageCopyCapture,
    WlrScreencopy,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CaptureOptions {
    /// `None` prefers ext-image-copy-capture.
    pub protocol: Option<Protocol>,
    pub memory: Memory,
}

pub struct Plane {
    pub fd: OwnedFd,
    pub offset: u32,
    pub stride: u32,
}

/// A GPU buffer the compositor copies into. `id` is unique per allocation, so importers can
/// cache per-buffer state.
pub struct DmabufBuffer {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    /// DRM fourcc.
    pub fourcc: u32,
    pub modifier: u64,
    pub planes: Vec<Plane>,
    wl_buffer: WlBuffer,
    mapping: std::sync::OnceLock<Mapping>,
}

impl DmabufBuffer {
    /// CPU view of plane 0 (mapped once, synced per call). Fast only with `Memory::System`.
    pub fn read(&self) -> Result<ReadGuard<'_>> {
        let p = &self.planes[0];
        let len = p.offset as usize + p.stride as usize * self.height as usize;
        let map = match self.mapping.get() {
            Some(m) => m,
            None => {
                let m = Mapping::new(&p.fd, len)?;
                self.mapping.get_or_init(|| m)
            }
        };
        ReadGuard::new(map, &p.fd, p.offset as usize, p.stride as usize)
    }
}

impl Drop for DmabufBuffer {
    fn drop(&mut self) {
        self.wl_buffer.destroy();
    }
}

/// A captured frame. The compositor won't write into `buffer` until this (and every clone of
/// the `Arc`) is dropped.
pub struct Frame {
    pub seq: u64,
    pub buffer: Arc<DmabufBuffer>,
    /// Regions changed since the previous frame. The first frame is fully damaged.
    pub damage: Vec<Rect>,
    /// Compositor presentation time on CLOCK_MONOTONIC, if reported.
    pub presented: Option<Duration>,
    pub requested_at: Instant,
    pub ready_at: Instant,
}

/// CLOCK_MONOTONIC, comparable with `Frame::presented`.
pub fn monotonic_now() -> Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable timespec for the call.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

pub struct Capturer {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
}

impl Capturer {
    /// Captures the output named `output` (e.g. "HEADLESS-1"), or the first output.
    pub fn new(output: Option<&str>) -> Result<Self> {
        Self::with_options(output, CaptureOptions::default())
    }

    pub fn with_options(output: Option<&str>, opts: CaptureOptions) -> Result<Self> {
        let protocol = opts.protocol;
        let conn = crate::display::connect_wayland().context("connect to Wayland")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();

        let mut state = State::new(&globals, &qh)?;
        state.alloc.memory = opts.memory;
        queue.roundtrip(&mut state)?;
        let wl_output = match output {
            Some(name) => state
                .outputs
                .iter()
                .find(|(_, n)| n.as_deref() == Some(name)),
            None => state.outputs.first(),
        }
        .map(|(o, _)| o.clone())
        .ok_or_else(|| anyhow!("output {output:?} not found"))?;

        if protocol == Some(Protocol::WlrScreencopy) {
            state.ext_mgr = None;
        }
        if let (Some(src_mgr), Some(mgr)) = (&state.ext_src_mgr, &state.ext_mgr) {
            let source = src_mgr.create_source(&wl_output, &qh, ());
            let session = mgr.create_session(&source, ext_mgr::Options::empty(), &qh, ());
            state.ext = Some((source, session));
            while state.constraints.is_none() && !state.stopped {
                queue.blocking_dispatch(&mut state)?;
            }
            if state.stopped {
                bail!("capture session stopped during setup");
            }
        } else if protocol == Some(Protocol::ExtImageCopyCapture) {
            bail!("compositor doesn't offer ext-image-copy-capture");
        } else if state.wlr_mgr.is_none() {
            bail!("compositor offers neither ext-image-copy-capture nor wlr-screencopy");
        }
        state.output = Some(wl_output);
        Ok(Self { conn, queue, state })
    }

    pub fn protocol(&self) -> Protocol {
        if self.state.ext.is_some() {
            Protocol::ExtImageCopyCapture
        } else {
            Protocol::WlrScreencopy
        }
    }

    /// DRM render node of the GPU the buffers live on.
    pub fn render_node(&self) -> &PathBuf {
        self.state.alloc.render_node()
    }

    /// Output size in buffer pixels, once known (always known for ext; after the first frame
    /// for wlr).
    pub fn size(&self) -> Option<(u32, u32)> {
        self.state.constraints.as_ref().map(|c| (c.width, c.height))
    }

    /// Waits for the next changed frame. `None` on timeout; the capture stays in flight and
    /// the next call resumes waiting for it.
    pub fn next_frame(&mut self, timeout: Option<Duration>) -> Result<Option<Frame>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let qh = self.queue.handle();
        loop {
            if self.state.stopped {
                bail!("capture session stopped");
            }
            match self.state.inflight.as_ref().map(|f| f.status) {
                None => self.state.start_frame(&qh)?,
                Some(Status::Ready) => {
                    let frame = self.state.finish_frame();
                    // Queue the next capture now so it overlaps the caller's encode. On error
                    // (e.g. no free buffer) the next call retries and reports it.
                    if self.state.start_frame(&qh).is_ok() {
                        self.conn.flush()?;
                    }
                    return Ok(Some(frame));
                }
                Some(Status::Failed(reason)) => {
                    self.state.abort_frame();
                    if reason != Some(ext_frame::FailureReason::BufferConstraints) {
                        bail!("capture failed: {reason:?}");
                    }
                }
                Some(Status::Pending) => {
                    if !self.dispatch_until(deadline)? {
                        return Ok(None);
                    }
                }
            }
        }
    }

    /// Dispatches events once; false if `deadline` passed first.
    fn dispatch_until(&mut self, deadline: Option<Instant>) -> Result<bool> {
        if self.queue.dispatch_pending(&mut self.state)? > 0 {
            return Ok(true);
        }
        self.conn.flush()?;
        let Some(guard) = self.queue.prepare_read() else {
            self.queue.dispatch_pending(&mut self.state)?;
            return Ok(true);
        };
        let timeout_ms = match deadline {
            None => -1,
            Some(d) => {
                let left = d.saturating_duration_since(Instant::now());
                left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
            }
        };
        let mut pfd = libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(&guard.connection_fd()),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is a valid pollfd and the count is 1.
        let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(err.into());
            }
            return Ok(true);
        }
        if n == 0 {
            return Ok(false);
        }
        match guard.read() {
            Ok(_) => {}
            Err(wayland_client::backend::WaylandError::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
        self.queue.dispatch_pending(&mut self.state)?;
        Ok(true)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Constraints {
    width: u32,
    height: u32,
    fourcc: u32,
    /// Empty: let the allocator pick (wlr-screencopy carries no modifier list).
    modifiers: Vec<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Status {
    Pending,
    Ready,
    /// `None` for wlr-screencopy, which gives no reason.
    Failed(Option<ext_frame::FailureReason>),
}

enum CaptureFrame {
    Ext(ExtImageCopyCaptureFrameV1),
    Wlr(ZwlrScreencopyFrameV1),
}

struct Inflight {
    frame: CaptureFrame,
    /// Not yet attached for wlr until `buffer_done`.
    buffer: Option<Arc<DmabufBuffer>>,
    status: Status,
    damage: Vec<Rect>,
    presented: Option<Duration>,
    requested_at: Instant,
}

// DRM fourccs, in order of preference.
const PREFERRED_FORMATS: [u32; 4] = [
    u32::from_le_bytes(*b"XR24"),
    u32::from_le_bytes(*b"AR24"),
    u32::from_le_bytes(*b"XB24"),
    u32::from_le_bytes(*b"AB24"),
];

struct State {
    outputs: Vec<(wl_output::WlOutput, Option<String>)>,
    output: Option<wl_output::WlOutput>,
    dmabuf: ZwpLinuxDmabufV1,
    ext_mgr: Option<ExtImageCopyCaptureManagerV1>,
    ext_src_mgr: Option<ExtOutputImageCaptureSourceManagerV1>,
    wlr_mgr: Option<ZwlrScreencopyManagerV1>,
    ext: Option<(ExtImageCaptureSourceV1, ExtImageCopyCaptureSessionV1)>,

    /// Constraints being collected from session events until `done`.
    pending: Vec<(u32, Vec<u64>)>,
    pending_size: (u32, u32),
    pending_device: Option<u64>,
    constraints: Option<Constraints>,
    stopped: bool,

    alloc: Allocator,
    pool: Vec<Arc<DmabufBuffer>>,
    inflight: Option<Inflight>,
    seq: u64,
}

impl State {
    fn new(globals: &GlobalList, qh: &QueueHandle<Self>) -> Result<Self> {
        let outputs = globals.contents().with_list(|list| {
            list.iter()
                .filter(|g| g.interface == "wl_output")
                .map(|g| {
                    let o = globals.registry().bind::<wl_output::WlOutput, _, _>(
                        g.name,
                        g.version.min(4),
                        qh,
                        (),
                    );
                    (o, None)
                })
                .collect()
        });
        Ok(Self {
            outputs,
            output: None,
            dmabuf: globals.bind(qh, 3..=5, ()).context("zwp_linux_dmabuf_v1")?,
            ext_mgr: globals.bind(qh, 1..=1, ()).ok(),
            ext_src_mgr: globals.bind(qh, 1..=1, ()).ok(),
            wlr_mgr: globals.bind(qh, 3..=3, ()).ok(),
            ext: None,
            pending: Vec::new(),
            pending_size: (0, 0),
            pending_device: None,
            constraints: None,
            stopped: false,
            alloc: Allocator::new(None)?,
            pool: Vec::new(),
            inflight: None,
            seq: 0,
        })
    }

    fn set_constraints(&mut self, c: Constraints) {
        if self.constraints.as_ref() != Some(&c) {
            self.pool.clear();
            self.constraints = Some(c);
        }
    }

    fn take_buffer(&mut self, qh: &QueueHandle<Self>) -> Result<Arc<DmabufBuffer>> {
        if let Some(b) = self.pool.iter().find(|b| Arc::strong_count(b) == 1) {
            return Ok(b.clone());
        }
        if self.pool.len() >= MAX_BUFFERS {
            bail!("all {MAX_BUFFERS} capture buffers are still held by consumers");
        }
        let c = self.constraints.as_ref().context("no buffer constraints")?;
        let alloc = self
            .alloc
            .allocate(c.width, c.height, c.fourcc, &c.modifiers)?;

        let params = self.dmabuf.create_params(qh, ());
        for (i, p) in alloc.planes.iter().enumerate() {
            params.add(
                p.fd.as_fd(),
                i as u32,
                p.offset,
                p.stride,
                (alloc.modifier >> 32) as u32,
                alloc.modifier as u32,
            );
        }
        let wl_buffer = params.create_immed(
            c.width as i32,
            c.height as i32,
            c.fourcc,
            dmabuf_params::Flags::empty(),
            qh,
            (),
        );
        params.destroy();
        let buf = Arc::new(DmabufBuffer {
            id: alloc.id,
            width: c.width,
            height: c.height,
            fourcc: c.fourcc,
            modifier: alloc.modifier,
            planes: alloc.planes,
            wl_buffer,
            mapping: Default::default(),
        });
        self.pool.push(buf.clone());
        Ok(buf)
    }

    fn start_frame(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        let requested_at = Instant::now();
        let (frame, buffer) = if let Some((_, session)) = &self.ext {
            let session = session.clone();
            let buffer = self.take_buffer(qh)?;
            let f = session.create_frame(qh, ());
            f.attach_buffer(&buffer.wl_buffer);
            f.damage_buffer(0, 0, buffer.width as i32, buffer.height as i32);
            f.capture();
            (CaptureFrame::Ext(f), Some(buffer))
        } else {
            let mgr = self.wlr_mgr.as_ref().unwrap();
            let f = mgr.capture_output(0, self.output.as_ref().unwrap(), qh, ());
            (CaptureFrame::Wlr(f), None)
        };
        self.inflight = Some(Inflight {
            frame,
            buffer,
            status: Status::Pending,
            damage: Vec::new(),
            presented: None,
            requested_at,
        });
        Ok(())
    }

    fn finish_frame(&mut self) -> Frame {
        let f = self.inflight.take().unwrap();
        f.frame.destroy();
        let buffer = f.buffer.unwrap();
        let mut damage = f.damage;
        tracing::trace!(?damage, "compositor damage");
        if self.seq == 0 || damage.is_empty() {
            damage = vec![Rect {
                x: 0,
                y: 0,
                width: buffer.width,
                height: buffer.height,
            }];
        }
        self.seq += 1;
        Frame {
            seq: self.seq,
            buffer,
            damage,
            presented: f.presented,
            requested_at: f.requested_at,
            ready_at: Instant::now(),
        }
    }

    fn abort_frame(&mut self) {
        if let Some(f) = self.inflight.take() {
            f.frame.destroy();
        }
    }

    fn inflight_mut(&mut self) -> Option<&mut Inflight> {
        self.inflight.as_mut()
    }
}

impl CaptureFrame {
    fn destroy(&self) {
        match self {
            CaptureFrame::Ext(f) => f.destroy(),
            CaptureFrame::Wlr(f) => f.destroy(),
        }
    }
}

fn timestamp(hi: u32, lo: u32, nsec: u32) -> Duration {
    Duration::new(((hi as u64) << 32) | lo as u64, nsec)
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(entry) = state.outputs.iter_mut().find(|(o, _)| o == output)
        {
            entry.1 = Some(name);
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_session::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session::Event::BufferSize { width, height } => {
                state.pending_size = (width, height)
            }
            ext_session::Event::DmabufDevice { device } => {
                state.pending_device = device.try_into().ok().map(u64::from_ne_bytes);
            }
            ext_session::Event::DmabufFormat { format, modifiers } => {
                let mods = modifiers
                    .as_chunks::<8>()
                    .0
                    .iter()
                    .map(|c| u64::from_ne_bytes(*c))
                    .collect();
                state.pending.push((format, mods));
            }
            ext_session::Event::Done => {
                let formats = std::mem::take(&mut state.pending);
                let picked = PREFERRED_FORMATS
                    .iter()
                    .find_map(|f| formats.iter().find(|(g, _)| g == f));
                let Some((fourcc, modifiers)) = picked.cloned() else {
                    tracing::error!(?formats, "no supported dmabuf format offered");
                    state.stopped = true;
                    return;
                };
                if let Some(dev) = state.pending_device.take()
                    && let Err(e) = state.alloc.use_device(dev)
                {
                    tracing::warn!("dmabuf device {dev:#x}: {e:#}; keeping current node");
                }
                let (width, height) = state.pending_size;
                tracing::debug!(width, height, fourcc, ?modifiers, "ext capture constraints");
                state.set_constraints(Constraints {
                    width,
                    height,
                    fourcc,
                    modifiers,
                });
            }
            ext_session::Event::Stopped => state.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_frame::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(f) = state.inflight_mut() else {
            return;
        };
        match event {
            ext_frame::Event::Damage {
                x,
                y,
                width,
                height,
            } => f.damage.push(Rect {
                x: x.max(0) as u32,
                y: y.max(0) as u32,
                width: width.max(0) as u32,
                height: height.max(0) as u32,
            }),
            ext_frame::Event::PresentationTime {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                f.presented = Some(timestamp(tv_sec_hi, tv_sec_lo, tv_nsec));
            }
            ext_frame::Event::Ready => f.status = Status::Ready,
            ext_frame::Event::Failed { reason } => {
                f.status = Status::Failed(match reason {
                    WEnum::Value(r) => Some(r),
                    WEnum::Unknown(_) => Some(ext_frame::FailureReason::Unknown),
                });
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for State {
    fn event(
        state: &mut Self,
        frame: &ZwlrScreencopyFrameV1,
        event: wlr_frame::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wlr_frame::Event::LinuxDmabuf {
                format,
                width,
                height,
            } => {
                state.pending.push((format, Vec::new()));
                state.pending_size = (width, height);
            }
            wlr_frame::Event::BufferDone => {
                let formats = std::mem::take(&mut state.pending);
                let Some(&fourcc) = PREFERRED_FORMATS
                    .iter()
                    .find(|f| formats.iter().any(|(g, _)| g == *f))
                else {
                    tracing::error!(?formats, "no supported dmabuf format offered");
                    state.stopped = true;
                    return;
                };
                let (width, height) = state.pending_size;
                state.set_constraints(Constraints {
                    width,
                    height,
                    fourcc,
                    modifiers: Vec::new(),
                });
                match state.take_buffer(qh) {
                    Ok(buffer) => {
                        frame.copy_with_damage(&buffer.wl_buffer);
                        if let Some(f) = state.inflight_mut() {
                            f.buffer = Some(buffer);
                        }
                    }
                    Err(e) => {
                        tracing::error!("capture buffer: {e:#}");
                        if let Some(f) = state.inflight_mut() {
                            f.status = Status::Failed(None);
                        }
                    }
                }
            }
            wlr_frame::Event::Damage {
                x,
                y,
                width,
                height,
            } => {
                if let Some(f) = state.inflight_mut() {
                    f.damage.push(Rect {
                        x,
                        y,
                        width,
                        height,
                    });
                }
            }
            wlr_frame::Event::Ready {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
            } => {
                if let Some(f) = state.inflight_mut() {
                    f.presented = Some(timestamp(tv_sec_hi, tv_sec_lo, tv_nsec));
                    f.status = Status::Ready;
                }
            }
            wlr_frame::Event::Failed => {
                if let Some(f) = state.inflight_mut() {
                    f.status = Status::Failed(None);
                }
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore ZwpLinuxDmabufV1);
delegate_noop!(State: ignore ZwpLinuxBufferParamsV1);
delegate_noop!(State: ignore WlBuffer);
delegate_noop!(State: ExtImageCopyCaptureManagerV1);
delegate_noop!(State: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(State: ExtImageCaptureSourceV1);
delegate_noop!(State: ZwlrScreencopyManagerV1);
