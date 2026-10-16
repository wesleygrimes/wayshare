//! Linux glue between the rfb server and the capture, encode and hp modules.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use tracing::{debug, info, warn};

use crate::capture::{CaptureSource, Capturer, monotonic_now};
use crate::encode::{EncoderBackend, EncoderConfig, VideoEncoder};

type Encoder = Box<dyn VideoEncoder<Frame = crate::capture::Frame> + Send>;
use crate::hp::negotiation::{self, AnswerParams, Offer};
use crate::hp::packetizer::{Packetizer, PacketizerConfig, rtp_timestamp};
use crate::hp::ratecontrol::{RateConfig, RateController, RateFeedback};
use crate::hp::rtcp::{self, RtcpEvent};
use crate::hp::srtp::{self, SrtpContext};
use crate::rfb::media::{HpBackend, KEY_LEN, MediaAnswer, MediaPorts, MediaStreamConfig};
use crate::rfb::session::canonical_ip;
use crate::rfb::{CursorSourceFactory, FrameSourceFactory, SessionId};
use crate::source::{CursorImage, CursorSource, Frame, FrameSource};

/// Picks the encoder backend before the capturer exists; the capturer reports the node it uses.
const RENDER_NODE: &str = "/dev/dri/renderD128";

/// The encoder HP sessions will use, or why there is none.
pub fn hp_encoder(cfg: &EncoderConfig) -> Result<EncoderBackend> {
    let node = std::path::Path::new(RENDER_NODE);
    let backend = EncoderBackend::auto(node, cfg);
    let r = backend.check(node);
    if cfg.backend.is_none() && backend == EncoderBackend::X265 {
        r.context("no hardware HEVC encoder, and the libx265 fallback is unusable")?;
    } else {
        r?;
    }
    Ok(backend)
}
const FIRST_RTCP_TIMEOUT: Duration = Duration::from_secs(15);
const RTCP_SILENCE_TIMEOUT: Duration = Duration::from_secs(10);
/// The viewer repeats FIR (one per tile, every ~250 ms) until a refresh decodes; one refresh
/// per window answers them all.
const FIR_DEDUPE: Duration = Duration::from_millis(500);
/// A 4K refresh is several MB sent back to back. Linux caps this at `net.core.wmem_max`.
const SEND_BUFFER: usize = 8 << 20;

/// `CaptureSource` plus its row-diff damage, for Standard mode.
pub struct CaptureFrames(CaptureSource);

impl FrameSource for CaptureFrames {
    fn size(&self) -> (u16, u16) {
        self.0.size()
    }

    fn advance(&mut self) -> bool {
        self.0.advance()
    }

    fn frame(&self) -> &Frame {
        self.0.frame()
    }

    fn damage(&self) -> Vec<(u16, u16, u16, u16)> {
        let clamp = |v: u32| v.min(u16::MAX as u32) as u16;
        self.0
            .dirty_rects()
            .iter()
            .map(|r| (clamp(r.x), clamp(r.y), clamp(r.width), clamp(r.height)))
            .collect()
    }
}

pub fn capture_factory(output: Option<String>) -> FrameSourceFactory {
    Arc::new(move || {
        let src = CaptureSource::new(output.as_deref())?;
        info!(size = ?src.size(), output = ?output, "capturing for Standard mode");
        Ok(Box::new(CaptureFrames(src)) as Box<dyn FrameSource>)
    })
}

/// The cursor theme's pointer, drawn locally by the viewer (it asks for no cursor in the
/// video). Hyprland paints its cursor into captures regardless, so it is hidden while any
/// session holds one of these.
pub fn cursor_factory(output: Option<String>) -> CursorSourceFactory {
    Arc::new(move || {
        let scale = crate::display::hyprland::Hyprland::from_env()
            .and_then(|mut h| {
                use crate::display::DisplayManager;
                h.outputs()
            })
            .ok()
            .and_then(|outs| {
                outs.into_iter()
                    .find(|o| output.as_ref().is_none_or(|n| &o.name == n))
            })
            .map_or(1.0, |o| o.scale);
        let image = match crate::cursor::default_pointer(scale) {
            Ok(img) => Some(img),
            Err(e) => {
                warn!("cursor theme unavailable, viewer shows its own pointer: {e:#}");
                None
            }
        };
        Box::new(ThemeCursor {
            image,
            _hidden: HiddenCursor::acquire(),
        }) as Box<dyn CursorSource>
    })
}

struct ThemeCursor {
    image: Option<CursorImage>,
    _hidden: HiddenCursor,
}

impl CursorSource for ThemeCursor {
    fn image(&self) -> Option<&CursorImage> {
        self.image.as_ref()
    }

    fn position(&self) -> Option<(u16, u16)> {
        None
    }
}

/// Sessions holding the compositor cursor hidden, and its `cursor:invisible` value before.
static HIDDEN: Mutex<(usize, bool)> = Mutex::new((0, false));

struct HiddenCursor;

impl HiddenCursor {
    fn acquire() -> Self {
        let mut st = HIDDEN.lock().unwrap();
        if st.0 == 0 {
            let r = crate::display::hyprland::Hyprland::from_env().and_then(|h| {
                let was = h
                    .request("j/getoption cursor:invisible")?
                    .contains("\"bool\": true");
                h.request("eval hl.config({ cursor = { invisible = true } })")?;
                Ok(was)
            });
            match r {
                Ok(was) => st.1 = was,
                Err(e) => warn!("hiding the compositor cursor: {e:#}"),
            }
        }
        st.0 += 1;
        HiddenCursor
    }
}

impl Drop for HiddenCursor {
    fn drop(&mut self) {
        let mut st = HIDDEN.lock().unwrap();
        st.0 -= 1;
        if st.0 == 0 {
            let cmd = format!("eval hl.config({{ cursor = {{ invisible = {} }} }})", st.1);
            if let Err(e) =
                crate::display::hyprland::Hyprland::from_env().and_then(|h| h.request(&cmd))
            {
                warn!("restoring the compositor cursor: {e:#}");
            }
        }
    }
}

/// High Performance media: capture -> VAAPI HEVC tiles -> RTP/SRTP over UDP.
pub struct LinuxHp {
    output: Option<String>,
    port: u16,
    encoder: EncoderConfig,
    fixed_bitrate: bool,
    audio: bool,
    swap_keys: bool,
    media: Mutex<Option<Media>>,
}

struct Media {
    session: SessionId,
    _audio: Option<crate::audio::sender::AudioSender>,
    peer: IpAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Media {
    fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }
}

impl Drop for Media {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl LinuxHp {
    /// `encoder.backend = None` picks VAAPI when the GPU can, else x265. `audio` streams the
    /// default sink's monitor as AAC-ELD.
    pub fn new(
        output: Option<String>,
        port: u16,
        encoder: EncoderConfig,
        fixed_bitrate: bool,
        audio: bool,
        swap_keys: bool,
    ) -> Self {
        Self {
            output,
            port,
            encoder,
            fixed_bitrate,
            audio,
            swap_keys,
            media: Mutex::new(None),
        }
    }
}

impl LinuxHp {
    fn output_size(&self) -> Result<(u32, u32)> {
        use crate::display::DisplayManager;
        let mut hypr = crate::display::hyprland::Hyprland::from_env()?;
        let name = self.output.as_deref().unwrap_or_default();
        let out = hypr
            .outputs()?
            .into_iter()
            .find(|o| self.output.is_none() || o.name == name)
            .with_context(|| format!("output {name:?} not found"))?;
        Ok((out.width, out.height))
    }
}

impl HpBackend for LinuxHp {
    fn available(&self, peer: IpAddr) -> bool {
        match &*self.media.lock().unwrap() {
            Some(m) => !m.running() || m.peer == peer,
            None => true,
        }
    }

    fn active(&self, session: SessionId) -> bool {
        self.media
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|m| m.session == session && m.running())
    }

    fn stop(&self, session: SessionId) {
        let mut slot = self.media.lock().unwrap();
        if slot.as_ref().is_some_and(|m| m.session == session) {
            slot.take();
            info!(session, "HP media stopped for session end");
        }
    }

    fn ports(&self) -> MediaPorts {
        MediaPorts {
            flags: 0,
            audio_port: self.port + 2,
            audio_flags: 1,
            video1_port: self.port,
            video1_flags: 1,
            video2_port: 0,
            video2_flags: 0,
        }
    }

    fn configure(
        &self,
        session: SessionId,
        peer: SocketAddr,
        config: MediaStreamConfig,
    ) -> Result<Option<MediaAnswer>> {
        let stream_size = self.output_size()?;
        let video = Offer::parse(&config.video1.offer).context("parsing video offer")?;
        let audio = Offer::parse(&config.audio.offer).context("parsing audio offer")?;
        let v = negotiation::answer(
            &video,
            &AnswerParams {
                ssrc: rand::random::<u32>() & !0xf,
                local: None,
                stream_size,
            },
        )?;
        let a = negotiation::answer(
            &audio,
            &AnswerParams {
                ssrc: rand::random(),
                local: None,
                stream_size,
            },
        )?;
        info!(
            video_mode = video.mode,
            audio_mode = audio.mode,
            ssrc = format_args!("{:#x}", v.ssrc),
            remote_ssrc = format_args!("{:#x}", v.remote_ssrc),
            pt = v.payload_type,
            tiles = v.tiles,
            ?stream_size,
            "HP answers built"
        );
        let (send_key, recv_key) = if self.swap_keys {
            (
                config.video1.key_viewer_to_server,
                config.video1.key_server_to_viewer,
            )
        } else {
            (
                config.video1.key_server_to_viewer,
                config.video1.key_viewer_to_server,
            )
        };
        let params = MediaParams {
            output: self.output.clone(),
            peer: canonical_ip(peer.ip()),
            port: self.port,
            encoder: self.encoder.clone(),
            fixed_bitrate: self.fixed_bitrate,
            ssrc: v.ssrc,
            remote_ssrc: v.remote_ssrc,
            payload_type: v.payload_type,
            stream_size,
            tiles: v.tiles as u8,
            send_key,
            recv_key,
        };

        let mut slot = self.media.lock().unwrap();
        if let Some(m) = slot.as_ref()
            && m.running()
            && m.peer != params.peer
        {
            anyhow::bail!("HP media is in use by another viewer ({})", m.peer);
        }
        let peer_ip = params.peer;
        slot.take();
        let audio = if self.audio {
            let (a_send, a_recv) = if self.swap_keys {
                (
                    config.audio.key_viewer_to_server,
                    config.audio.key_server_to_viewer,
                )
            } else {
                (
                    config.audio.key_server_to_viewer,
                    config.audio.key_viewer_to_server,
                )
            };
            let params = crate::audio::sender::AudioParams {
                peer: SocketAddr::new(peer_ip, self.ports().audio_port),
                local_port: self.port + 2,
                ssrc: a.ssrc,
                remote_ssrc: a.remote_ssrc,
                payload_type: a.payload_type,
                send_key: a_send,
                recv_key: a_recv,
                encoder: crate::audio::fdk::EncoderConfig::default(),
                device: None,
            };
            match crate::audio::sender::AudioSender::start(params) {
                Ok(sender) => {
                    info!(ssrc = format_args!("{:#x}", a.ssrc), "HP audio started");
                    Some(sender)
                }
                Err(e) => {
                    warn!("HP audio unavailable, continuing with video only: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("hp-media".into())
            .spawn(move || {
                if let Err(e) = run_media(params, &thread_stop) {
                    warn!("HP media stopped: {e:#}");
                }
            })?;
        *slot = Some(Media {
            session,
            _audio: audio,
            peer: peer_ip,
            stop,
            thread: Some(thread),
        });

        Ok(Some(MediaAnswer {
            flags: 1,
            audio: a.bytes,
            video1: v.bytes,
            video2: Vec::new(),
        }))
    }
}

struct MediaParams {
    output: Option<String>,
    peer: IpAddr,
    port: u16,
    encoder: EncoderConfig,
    /// hp.bitrate was configured: use it as the rate controller's starting point.
    fixed_bitrate: bool,
    ssrc: u32,
    remote_ssrc: u32,
    payload_type: u8,
    stream_size: (u32, u32),
    tiles: u8,
    send_key: [u8; KEY_LEN],
    recv_key: [u8; KEY_LEN],
}

/// Main-thread counters (capture, RTCP).
#[derive(Default)]
struct Stats {
    frames: u64,
    rtcp: u64,
    firs: u64,
    acks: u64,
    other_rtcp: u64,
    /// Latest receiver feedback (RCTL): bandwidth estimate and one-way relative delay.
    feedback: Option<RateFeedback>,
    last_refresh: Option<Instant>,
    /// Time from the capture being ready to `submit` returning (waiting for the encoder,
    /// diff and conversion).
    submit_ms: Vec<f64>,
    /// Compositor presentation to the captured frame reaching us.
    present_to_ready_ms: Vec<f64>,
    /// Captured frames replaced by a newer one while the encoder was busy.
    dropped: u64,
}

/// Counters kept by the send thread.
#[derive(Default)]
struct SendStats {
    frames_window: u64,
    bytes_window: u64,
    send_errors: u64,
    present_to_sent_ms: Vec<f64>,
    diff_ms: Vec<f64>,
    convert_ms: Vec<f64>,
    encode_ms: Vec<f64>,
    send_ms: Vec<f64>,
    failed: Option<String>,
}

fn pct(v: &mut [f64], q: f64) -> Option<f64> {
    v.sort_by(f64::total_cmp);
    v.get(((v.len() as f64 - 1.0) * q) as usize)
        .map(|x| (x * 10.0).round() / 10.0)
}

struct Sender {
    sock: UdpSocket,
    pk: Packetizer,
    tx: SrtpContext,
    ssrc: u32,
    remote_ssrc: u32,
    tiles: usize,
    start: Instant,
    ts_offset: u32,
    packets: u32,
    octets: u32,
    rtp_ts: u32,
    last_sr: Instant,
    stats: Arc<Mutex<SendStats>>,
    send_errors_logged: u32,
}

impl Sender {
    /// Packetizes, encrypts and sends coded frames as the encoder finishes them, so the next
    /// frame's capture and submission never wait on the network path.
    fn run(mut self, frames: std::sync::mpsc::Receiver<Result<crate::encode::EncodedFrame>>) {
        loop {
            match frames.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(out)) => self.send_frame(out),
                Ok(Err(e)) => {
                    self.stats.lock().unwrap().failed = Some(format!("{e:#}"));
                    return;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
            if self.last_sr.elapsed() >= Duration::from_secs(1) {
                let mut sr = rtcp::sender_report(
                    self.ssrc,
                    self.remote_ssrc,
                    rtcp::ntp_now(),
                    self.rtp_ts,
                    self.packets,
                    self.octets,
                );
                if self.tx.protect_rtcp(&mut sr).is_ok() {
                    let _ = self.sock.send(&sr);
                }
                self.last_sr = Instant::now();
            }
        }
    }

    fn send_frame(&mut self, out: crate::encode::EncodedFrame) {
        let t0 = Instant::now();
        if out.frame_size != self.pk.config().frame_size {
            info!(from = ?self.pk.config().frame_size, to = ?out.frame_size, "HP stream resized");
            self.pk.set_frame_size(out.frame_size);
        }
        let mut tiles: Vec<Option<&[u8]>> = vec![None; self.tiles];
        for t in &out.tiles {
            if let Some(slot) = tiles.get_mut(t.index as usize) {
                *slot = Some(&t.data);
            }
        }
        self.rtp_ts = rtp_timestamp(self.start.elapsed(), self.ts_offset);
        let (mut bytes, mut errors) = (0u64, 0u64);
        for mut pkt in self.pk.packetize(&tiles, self.rtp_ts) {
            if self.tx.protect_rtp(&mut pkt).is_err() {
                errors += 1;
                continue;
            }
            self.packets = self.packets.wrapping_add(1);
            self.octets = self.octets.wrapping_add(pkt.len() as u32);
            bytes += pkt.len() as u64;
            if let Err(e) = self.sock.send(&pkt) {
                errors += 1;
                if errors == 1 && self.send_errors_logged < 10 {
                    self.send_errors_logged += 1;
                    warn!(
                        len = pkt.len(),
                        frame_bytes = out.tiles.iter().map(|t| t.data.len()).sum::<usize>(),
                        "HP video send failed: {e}"
                    );
                }
            }
        }
        let now = monotonic_now();
        let mut st = self.stats.lock().unwrap();
        st.frames_window += 1;
        st.bytes_window += bytes;
        st.send_errors += errors;
        if let Some(p) = out.presented.filter(|&p| now > p) {
            st.present_to_sent_ms.push((now - p).as_secs_f64() * 1e3);
        }
        st.diff_ms.push(out.timings.diff.as_secs_f64() * 1e3);
        st.convert_ms.push(out.timings.convert.as_secs_f64() * 1e3);
        st.encode_ms.push(out.timings.encode.as_secs_f64() * 1e3);
        st.send_ms.push(t0.elapsed().as_secs_f64() * 1e3);
    }
}

fn run_media(p: MediaParams, stop: &AtomicBool) -> Result<()> {
    let sock = UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), p.port))
        .with_context(|| format!("binding UDP {}", p.port))?;
    sock.connect(SocketAddr::new(p.peer, p.port))?;
    sock.set_nonblocking(true)?;
    let sref = socket2::SockRef::from(&sock);
    if let Err(e) = sref.set_send_buffer_size(SEND_BUFFER) {
        warn!("SO_SNDBUF {SEND_BUFFER}: {e}");
    }
    info!(
        peer = %p.peer,
        port = p.port,
        sndbuf = sref.send_buffer_size().ok(),
        "HP media socket ready"
    );

    let backend = EncoderBackend::auto(std::path::Path::new(RENDER_NODE), &p.encoder);
    let mut cap = Capturer::with_options(
        p.output.as_deref(),
        crate::capture::CaptureOptions {
            memory: backend.capture_memory(),
            ..Default::default()
        },
    )?;
    let first = loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if let Some(f) = cap.next_frame(Some(Duration::from_millis(500)))? {
            break f;
        }
    };
    let (w, h) = (first.buffer.width, first.buffer.height);
    ensure!(
        (w, h) == p.stream_size,
        "captured {w}x{h} but the answer advertised {:?}",
        p.stream_size
    );
    let mut enc: Encoder = backend.open(cap.render_node(), w, h, &p.encoder)?;
    info!(
        ?backend,
        skip_unchanged = p.encoder.skip_unchanged,
        "HP encoder backend"
    );
    let mut rate_cfg = RateConfig::default();
    if let Some(bps) = p.encoder.bitrate_bps.filter(|_| p.fixed_bitrate) {
        rate_cfg.start_bps = bps.min(u32::MAX as u64) as u32;
    }
    let mut rc = RateController::new(rate_cfg);
    enc.set_bitrate(rc.target_bps() as u64);
    ensure!(
        u32::from(p.tiles) == crate::encode::TILE_COUNT as u32,
        "answer has {} tiles, encoder produces {}",
        p.tiles,
        crate::encode::TILE_COUNT
    );
    let mut rx = SrtpContext::new(&p.recv_key);
    info!(width = w, height = h, layout = ?enc.layout(), "HP encoder ready");

    let start = Instant::now();
    let send_stats = Arc::new(Mutex::new(SendStats::default()));
    let (frame_tx, frame_rx) = std::sync::mpsc::channel();
    enc.set_output(frame_tx.clone());
    let sender = Sender {
        sock: sock.try_clone()?,
        pk: Packetizer::new(PacketizerConfig::new(
            p.ssrc,
            p.payload_type,
            p.tiles,
            (w as u16, h as u16),
        )),
        tx: SrtpContext::new(&p.send_key),
        ssrc: p.ssrc,
        remote_ssrc: p.remote_ssrc,
        tiles: p.tiles as usize,
        start,
        ts_offset: rand::random(),
        packets: 0,
        octets: 0,
        rtp_ts: 0,
        last_sr: Instant::now(),
        stats: send_stats.clone(),
        send_errors_logged: 0,
    };
    let send_thread = std::thread::Builder::new()
        .name("hp-send".into())
        .spawn(move || sender.run(frame_rx))?;

    let render_node = cap.render_node().clone();
    let mut reopen = |w: u32, h: u32| -> Result<Encoder> {
        let mut e = backend.open(&render_node, w, h, &p.encoder)?;
        e.set_output(frame_tx.clone());
        Ok(e)
    };
    let r = media_loop(
        stop,
        &sock,
        &mut cap,
        &mut enc,
        &mut reopen,
        &mut rc,
        &mut rx,
        first,
        start,
        &send_stats,
    );
    // Dropping the encoder and the last sender closes the frame channel and ends the send thread.
    drop(frame_tx);
    drop(enc);
    let _ = send_thread.join();
    r
}

#[allow(clippy::too_many_arguments)]
fn media_loop(
    stop: &AtomicBool,
    sock: &UdpSocket,
    cap: &mut Capturer,
    enc: &mut Encoder,
    reopen: &mut dyn FnMut(u32, u32) -> Result<Encoder>,
    rc: &mut RateController,
    rx: &mut SrtpContext,
    first: crate::capture::Frame,
    start: Instant,
    send_stats: &Mutex<SendStats>,
) -> Result<()> {
    let mut stats = Stats::default();
    let mut last_log = Instant::now();
    let mut last_forced_idr: Option<Instant> = None;
    let mut last_rtcp: Option<Instant> = None;
    // Latest-frame policy: while the encoder is busy only the newest capture is kept, so a
    // frame is never encoded after a newer one exists. Nothing is lost by dropping frames:
    // change detection compares against the last submitted frame.
    let mut held = Some(first);
    // A static screen produces no captures, but keyframe requests and heartbeats still need a
    // source, so the last submitted frame is kept for resubmission.
    let mut last: Option<crate::capture::Frame> = None;

    while !stop.load(Ordering::Relaxed) {
        if held.is_none() && enc.can_submit() && enc.wants_frame() {
            held = last.as_ref().map(repeat_frame);
        }
        if held.is_some() && enc.can_submit() {
            let frame = held.take().unwrap();
            let l = enc.layout();
            let size = (frame.buffer.width, frame.buffer.height);
            if size != (l.width, l.height) {
                // The viewer accepts new dimensions in-band from a refresh (a new encoder starts
                // with one); the sender switches the packetizer when it sees the new size.
                info!(from = ?(l.width, l.height), to = ?size, "HP encoder resizing");
                *enc = reopen(size.0, size.1)?;
                enc.set_bitrate(rc.target_bps() as u64);
            }
            if last_rtcp.is_none()
                && last_forced_idr.is_none_or(|t| t.elapsed() >= Duration::from_secs(1))
            {
                enc.request_keyframe(None);
                last_forced_idr = Some(Instant::now());
            }
            enc.submit(&frame)?;
            stats
                .submit_ms
                .push(frame.ready_at.elapsed().as_secs_f64() * 1e3);
            stats.frames += 1;
            last = Some(frame);
        }
        if let Some(e) = send_stats.lock().unwrap().failed.take() {
            anyhow::bail!("encode failed: {e}");
        }

        drain_rtcp(sock, rx, enc, rc, start, &mut stats, &mut last_rtcp);
        match last_rtcp {
            None if start.elapsed() > FIRST_RTCP_TIMEOUT => {
                anyhow::bail!("no RTCP from the viewer within {FIRST_RTCP_TIMEOUT:?}")
            }
            Some(t) if t.elapsed() > RTCP_SILENCE_TIMEOUT => {
                anyhow::bail!("viewer RTCP silent for {RTCP_SILENCE_TIMEOUT:?}")
            }
            _ => {}
        }

        if last_log.elapsed() >= Duration::from_secs(5) {
            let secs = last_log.elapsed().as_secs_f64();
            let mut st = std::mem::take(&mut *send_stats.lock().unwrap());
            info!(
                fps = format_args!("{:.1}", st.frames_window as f64 / secs),
                mbps = format_args!("{:.1}", st.bytes_window as f64 * 8.0 / secs / 1e6),
                frames = stats.frames,
                rtcp = stats.rtcp,
                don_acks = stats.acks,
                firs = stats.firs,
                target_mbps = format_args!("{:.1}", rc.target_bps() as f64 / 1e6),
                bwe_mbps = ?stats.feedback.as_ref().map(|f| f.bwe_bps as f64 / 1e6),
                owrd_ms = ?stats.feedback.as_ref().map(|f| (f.owrd * 1000.0).round()),
                other_rtcp = stats.other_rtcp,
                send_errors = st.send_errors,
                present_to_sent_p50_ms = ?pct(&mut st.present_to_sent_ms, 0.5),
                present_to_sent_p95_ms = ?pct(&mut st.present_to_sent_ms, 0.95),
                ready_to_submitted_p50_ms = ?pct(&mut stats.submit_ms, 0.5),
                ready_to_submitted_p95_ms = ?pct(&mut stats.submit_ms, 0.95),
                present_to_ready_p50_ms = ?pct(&mut stats.present_to_ready_ms, 0.5),
                present_to_ready_p95_ms = ?pct(&mut stats.present_to_ready_ms, 0.95),
                dropped = stats.dropped,
                diff_p50_ms = ?pct(&mut st.diff_ms, 0.5),
                convert_p50_ms = ?pct(&mut st.convert_ms, 0.5),
                encode_p50_ms = ?pct(&mut st.encode_ms, 0.5),
                encode_p95_ms = ?pct(&mut st.encode_ms, 0.95),
                send_p50_ms = ?pct(&mut st.send_ms, 0.5),
                "HP stats"
            );
            stats.submit_ms.clear();
            stats.present_to_ready_ms.clear();
            last_log = Instant::now();
        }

        // Poll quickly while a frame waits for the encoder.
        let wait = if held.is_some() { 2 } else { 20 };
        if let Some(f) = cap.next_frame(Some(Duration::from_millis(wait)))? {
            if let Some(p) = f.presented {
                let ready = monotonic_now().saturating_sub(f.ready_at.elapsed());
                stats
                    .present_to_ready_ms
                    .push(ready.saturating_sub(p).as_secs_f64() * 1e3);
            }
            if held.replace(f).is_some() {
                stats.dropped += 1;
            }
        }
    }
    info!(frames = stats.frames, "HP media stopping");
    Ok(())
}

fn repeat_frame(f: &crate::capture::Frame) -> crate::capture::Frame {
    let now = Instant::now();
    crate::capture::Frame {
        seq: f.seq,
        buffer: f.buffer.clone(),
        damage: Vec::new(),
        presented: None,
        requested_at: now,
        ready_at: now,
    }
}

fn drain_rtcp(
    sock: &UdpSocket,
    rx: &mut SrtpContext,
    enc: &mut Encoder,
    rc: &mut RateController,
    start: Instant,
    stats: &mut Stats,
    last_rtcp: &mut Option<Instant>,
) {
    let mut buf = vec![0u8; 2048];
    while let Ok(n) = sock.recv(&mut buf) {
        let mut pkt = buf[..n].to_vec();
        if !srtp::is_rtcp(&pkt) {
            debug!(len = n, "ignoring non-RTCP packet on the video port");
            continue;
        }
        if let Err(e) = rx.unprotect_rtcp(&mut pkt) {
            warn!(len = n, "SRTCP unprotect failed: {e:#}");
            continue;
        }
        if last_rtcp.is_none() {
            info!("first RTCP from viewer");
        }
        *last_rtcp = Some(Instant::now());
        stats.rtcp += 1;
        match rtcp::parse(&pkt) {
            Ok(events) => {
                for ev in events {
                    if let Some(bps) = rc.on_event(&ev, start.elapsed()) {
                        debug!(bps, "rate control: new target");
                        enc.set_bitrate(bps as u64);
                    }
                    match ev {
                        RtcpEvent::KeyframeRequest { .. } => {
                            stats.firs += 1;
                            if stats.last_refresh.is_none_or(|t| t.elapsed() >= FIR_DEDUPE) {
                                info!("viewer requested a keyframe");
                                enc.request_keyframe(None);
                                stats.last_refresh = Some(Instant::now());
                            }
                        }
                        RtcpEvent::DonAck { .. } => stats.acks += 1,
                        RtcpEvent::RateControl { ref data, .. } => {
                            if let Some(fb) = RateFeedback::parse(data) {
                                stats.feedback = Some(fb);
                            }
                        }
                        RtcpEvent::ReceiverReport { .. } | RtcpEvent::Sdes => {}
                        other => {
                            stats.other_rtcp += 1;
                            if stats.other_rtcp <= 20 {
                                info!(?other, "unhandled RTCP");
                            }
                        }
                    }
                }
            }
            Err(e) => warn!("RTCP parse failed: {e:#}"),
        }
    }
}

/// Applies msg 29 to a Hyprland output and restores the original mode afterwards.
pub struct HyprDisplay {
    output: String,
    /// Session that changed the mode, and the mode to restore.
    owner: Mutex<Option<(SessionId, crate::display::OutputInfo)>>,
}

impl HyprDisplay {
    pub fn new(output: String) -> Self {
        Self {
            output,
            owner: Mutex::new(None),
        }
    }
}

impl crate::rfb::DisplayControl for HyprDisplay {
    fn configure(
        &self,
        session: SessionId,
        request: &crate::display::apple::DisplayRequest,
    ) -> Result<Option<crate::rfb::AppliedDisplay>> {
        use crate::display::DisplayManager;
        let Some(pref) = request.displays.first().and_then(|d| d.preferred()) else {
            return Ok(None);
        };
        let mode = pref.display_mode();
        let mut hypr = crate::display::hyprland::Hyprland::from_env()?;
        {
            let mut owner = self.owner.lock().unwrap();
            match &*owner {
                Some((other, _)) if *other != session => {
                    info!(
                        owner = other,
                        "output mode owned by another session; not resizing"
                    );
                    return Ok(None);
                }
                Some(_) => {}
                None => {
                    let current = hypr
                        .outputs()?
                        .into_iter()
                        .find(|o| o.name == self.output)
                        .with_context(|| format!("output {} not found", self.output))?;
                    *owner = Some((session, current));
                }
            }
        }
        let actual = hypr.set_mode(&self.output, &mode)?;
        info!(output = %self.output, requested = ?mode, ?actual, "output mode set");
        let px = |v: u32| v.min(u16::MAX as u32) as u16;
        let points = |v: u32| px((v as f64 / actual.scale).round() as u32);
        Ok(Some(crate::rfb::AppliedDisplay {
            pixels: (px(actual.width), px(actual.height)),
            points: (points(actual.width), points(actual.height)),
            scale: actual.scale,
        }))
    }

    fn restore(&self, session: SessionId) -> Result<()> {
        use crate::display::DisplayManager;
        let original = {
            let mut owner = self.owner.lock().unwrap();
            match owner.take() {
                Some((s, info)) if s == session => info,
                other => {
                    *owner = other;
                    return Ok(());
                }
            }
        };
        let mut hypr = crate::display::hyprland::Hyprland::from_env()?;
        let actual = hypr.set_mode(&self.output, &original.mode())?;
        info!(output = %self.output, ?actual, "output mode restored");
        Ok(())
    }
}

/// Virtual keyboard and pointer bound to `output`.
pub fn input_factory(
    output: String,
    keyboard: crate::input::KeyboardConfig,
) -> crate::rfb::InputFactory {
    Arc::new(move |w, h| {
        let sink = crate::input::wayland::WaylandInput::connect(
            Some(&output),
            w,
            h,
            keyboard.clone(),
            crate::input::PointerConfig::default(),
        )?;
        info!(output = %output, width = w, height = h, "input injection enabled");
        Ok(Box::new(sink) as Box<dyn crate::input::InputSink>)
    })
}
