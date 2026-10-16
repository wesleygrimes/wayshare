//! The High Performance media pipeline: capture, encode, packetize, SRTP, and RTCP handling.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use tracing::{debug, info, warn};

use super::RENDER_NODE;
use crate::capture::{Capturer, monotonic_now};
use crate::encode::{EncoderBackend, EncoderConfig, VideoEncoder};
use crate::hp::packetizer::{Packetizer, PacketizerConfig, rtp_timestamp};
use crate::hp::ratecontrol::{RateConfig, RateController, RateFeedback};
use crate::hp::rtcp::{self, RtcpEvent};
use crate::hp::srtp::{self, SrtpContext};
use crate::rfb::media::KEY_LEN;

type Encoder = Box<dyn VideoEncoder<Frame = crate::capture::Frame> + Send>;

const FIRST_RTCP_TIMEOUT: Duration = Duration::from_secs(15);
const RTCP_SILENCE_TIMEOUT: Duration = Duration::from_secs(10);
/// The viewer repeats FIR (one per tile, every ~250 ms) until a refresh decodes; one refresh
/// per window answers them all.
const FIR_DEDUPE: Duration = Duration::from_millis(500);
/// A 4K refresh is several MB sent back to back. Linux caps this at `net.core.wmem_max`.
const SEND_BUFFER: usize = 8 << 20;

pub(super) struct MediaParams {
    pub(super) output: Option<String>,
    pub(super) peer: IpAddr,
    pub(super) port: u16,
    pub(super) encoder: EncoderConfig,
    /// hp.bitrate was configured: use it as the rate controller's starting point.
    pub(super) fixed_bitrate: bool,
    pub(super) ssrc: u32,
    pub(super) remote_ssrc: u32,
    pub(super) payload_type: u8,
    pub(super) stream_size: (u32, u32),
    pub(super) tiles: u8,
    pub(super) send_key: [u8; KEY_LEN],
    pub(super) recv_key: [u8; KEY_LEN],
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

pub(super) fn run_media(p: MediaParams, stop: &AtomicBool) -> Result<()> {
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
