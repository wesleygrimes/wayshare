use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use super::Config;
use super::clipboard::ClipboardSync;
use super::crypt::{self, RecordReader, RecordWriter};
use super::dh::ecb_decrypt;
use super::encode::{ENC_RAW, ENC_ZLIB, Encoder};
use super::handshake::{self, MSG_SET_ENCRYPTION};
use super::media::{self, ENC_MEDIA_STREAM, MediaStreamConfig};
use super::messages::{self, ClientMessage, EventBlock};
use super::pixel::PixelFormat;
use super::pseudo::{
    self, ENC_CURSOR_ALPHA, ENC_CURSOR_POSITION, ENC_DESKTOP_SIZE, ENC_DEVICE_INFO,
};
use super::{AppliedDisplay, SessionId};
use crate::clipboard::proto as clip_proto;
use crate::display::apple::{
    self, DISPLAY_INFO2_ON_CONSOLE, DISPLAY_INFO2_VIRTUAL, DisplayInfo2Entry,
    SCREEN_FLAG_DYNAMIC_RESOLUTION, SCREEN_FLAG_MAIN,
};
use crate::input::{self, Event2, InputSink, LoggingSink};
use crate::source::{CursorSource, FrameSource};

const FRAME_INTERVAL: Duration = Duration::from_millis(33);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const ENCRYPTION_METHOD: u32 = 1;
const MAX_RECTS: usize = 64;
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

type Rect = (u16, u16, u16, u16);
type Inbound = mpsc::Sender<Result<ClientMessage>>;
type StreamKeys = Arc<Mutex<Option<([u8; 16], [u8; 16])>>>;

pub async fn run(mut stream: TcpStream, cfg: Arc<Config>, id: SessionId) -> Result<()> {
    stream.set_nodelay(true)?;
    set_keepalive(&stream);
    let peer = stream.peer_addr()?;
    let source = blocking(|| (cfg.frame_source)()).context("creating frame source")?;
    let (w, h) = source.size();
    let cmds = cfg.server_cmds(canonical_ip(peer.ip()));

    let handshake = handshake::run(
        &mut stream,
        &cfg.auth,
        w,
        h,
        &PixelFormat::NATIVE,
        &cfg.name,
        &cmds,
    );
    let Some(hs) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
        .await
        .map_err(|_| anyhow!("handshake timed out after {HANDSHAKE_TIMEOUT:?}"))??
    else {
        return Ok(());
    };
    info!(user = %hs.username, flags = format_args!("{:#04x}", hs.client_flags), ?cmds, "session started");

    let (rd, wr) = stream.into_split();
    let (tx, mut rx) = mpsc::channel(256);
    let keys: StreamKeys = Arc::default();
    tokio::spawn(reader(rd, tx, keys.clone()));

    let cursor: Box<dyn CursorSource> = blocking(|| (cfg.cursor_source)());
    let input: Box<dyn InputSink> = match &cfg.input {
        Some(make) => blocking(|| make(w as u32, h as u32)).context("creating input sink")?,
        None => Box::new(LoggingSink),
    };
    let mut s = Session {
        input_stats: InputStats::default(),
        id,
        shared_clipboard: false,
        clipboard_serial: 0,
        input,
        keycode_needs_bit1: false,
        display_applied: None,
        dynamic_resolution: false,
        stats: RfbStats::new(),
        source,
        media_active: false,
        cfg: cfg.clone(),
        peer,
        auth_key: hs.session_key,
        out: Output { wr, enc: None },
        keys,
        pf: PixelFormat::NATIVE,
        encodings: Vec::new(),
        encoding: ENC_RAW,
        encoder: Encoder::default(),
        pending: None,
        auto: None,
        fb_size: (w, h),
        cursor_ids: HashMap::new(),
        next_cursor_id: pseudo::FIRST_CURSOR_ID,
        cursor_serial: None,
        cursor_pos: None,
        device_info_sent: false,
        hp_offered: false,
        hp_restarting: false,
        clip: ClipboardSync::default(),
    };

    let mut tick = tokio::time::interval(FRAME_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = async {
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(Ok(m)) => s.handle(m).await?,
                    Some(Err(e)) if is_eof(&e) => {
                        info!("client disconnected");
                        return Ok(());
                    }
                    Some(Err(e)) => return Err(e.context("reading client message")),
                    None => return Ok(()),
                },
                _ = tick.tick() => s.tick(cursor.as_ref()).await?,
            }
        }
    }
    .await;
    drop(s);
    result
}

struct Output {
    wr: OwnedWriteHalf,
    enc: Option<RecordWriter>,
}

impl Output {
    async fn send(&mut self, msg: &[u8]) -> Result<()> {
        let write = async {
            match &mut self.enc {
                Some(enc) => {
                    let mut sealed = Vec::with_capacity(msg.len() + msg.len() / 512 + 64);
                    enc.seal(msg, &mut sealed);
                    self.wr.write_all(&sealed).await
                }
                None => self.wr.write_all(msg).await,
            }
        };
        tokio::time::timeout(WRITE_TIMEOUT, write)
            .await
            .map_err(|_| anyhow!("client stopped reading for {WRITE_TIMEOUT:?}"))?
            .context("writing to client")
    }
}

struct RfbStats {
    since: std::time::Instant,
    updates: u64,
    bytes: u64,
    rects: u64,
    encode: Duration,
}

impl RfbStats {
    fn new() -> Self {
        Self {
            since: std::time::Instant::now(),
            updates: 0,
            bytes: 0,
            rects: 0,
            encode: Duration::ZERO,
        }
    }
}

/// Mouse event (byte 10 = 0xFF) with any wheel bit (byte 11 bits 3-6) set.
fn is_wheel_block(block: &[u8; 16]) -> bool {
    block[10] == 0xFF && block[11] & 0x78 != 0
}

/// Counts only: key contents are never logged above trace level.
#[derive(Default)]
struct InputStats {
    keys: u64,
    pointer: u64,
    scroll: u64,
    encrypted: u64,
    enc_keys: u64,
    since: Option<std::time::Instant>,
}

struct Session {
    input_stats: InputStats,
    id: SessionId,
    /// The viewer turned on "Use Shared Clipboard" (msg 21 cmd 1).
    shared_clipboard: bool,
    clipboard_serial: u64,
    input: Box<dyn InputSink>,
    /// Viewer version > 10: the mac keycode in event blocks also needs flags bit 1.
    keycode_needs_bit1: bool,
    /// Set after a msg 29 resize; sent as DisplayInfo2/DisplayInfo once the new size is live.
    display_applied: Option<AppliedDisplay>,
    /// The viewer's last msg 29 asked for dynamic resolution.
    dynamic_resolution: bool,
    stats: RfbStats,
    source: Box<dyn FrameSource>,
    /// HP media is streaming, so RFB framebuffer updates are paused.
    media_active: bool,
    cfg: Arc<Config>,
    peer: SocketAddr,
    auth_key: [u8; 16],
    out: Output,
    keys: StreamKeys,
    pf: PixelFormat,
    encodings: Vec<i32>,
    encoding: i32,
    encoder: Encoder,
    pending: Option<Rect>,
    auto: Option<Rect>,
    fb_size: (u16, u16),
    cursor_ids: HashMap<u64, u32>,
    next_cursor_id: u32,
    cursor_serial: Option<u64>,
    cursor_pos: Option<(u16, u16)>,
    device_info_sent: bool,
    hp_offered: bool,
    /// HP media was stopped on purpose for a display change; a new msg 28 is expected.
    hp_restarting: bool,
    clip: ClipboardSync,
}

impl Session {
    fn supports(&self, encoding: i32) -> bool {
        self.encodings.contains(&encoding)
    }

    async fn handle(&mut self, msg: ClientMessage) -> Result<()> {
        match msg {
            ClientMessage::SetPixelFormat(new) => {
                debug!(?new, "SetPixelFormat");
                new.validate()?;
                if new != self.pf {
                    self.cursor_ids.clear();
                    self.cursor_serial = None;
                }
                self.pf = new;
            }
            ClientMessage::SetEncodings(list) => {
                info!(?list, "SetEncodings");
                self.encoding = list
                    .iter()
                    .copied()
                    .find(|e| *e == ENC_ZLIB || *e == ENC_RAW)
                    .unwrap_or(ENC_RAW);
                self.encodings = list;
                debug!(encoding = self.encoding, "selected encoding");
                self.offer_media_stream().await?;
            }
            ClientMessage::FramebufferUpdateRequest {
                incremental,
                x,
                y,
                w,
                h,
            } => {
                debug!(incremental, x, y, w, h, "FramebufferUpdateRequest");
                let rect = clamp((x, y, w, h), self.fb_size);
                if incremental {
                    self.pending = Some(rect);
                } else {
                    self.pending = None;
                    self.send_frame(&[rect]).await?;
                }
            }
            ClientMessage::AutoFramebufferUpdate { value, x, y, w, h } => {
                let enabled = value != u32::MAX;
                info!(enabled, value, x, y, w, h, "AutoFramebufferUpdate");
                self.auto = enabled.then(|| clamp((x, y, w, h), self.fb_size));
            }
            ClientMessage::KeyEvent { down, keysym } => {
                tracing::trace!(down, keysym = format_args!("{keysym:#x}"), "KeyEvent");
                self.input_stats.keys += 1;
                input_result("key", self.input.key(down, keysym, None));
            }
            ClientMessage::PointerEvent { buttons, x, y } => {
                tracing::trace!(buttons, x, y, "PointerEvent");
                self.input_stats.pointer += 1;
                input_result("pointer", self.input.pointer(x as i32, y as i32, buttons));
            }
            ClientMessage::EncryptedEvent { flags, mut block } => {
                ecb_decrypt(&self.auth_key, &mut block);
                log_event_block("EncryptedEvent", flags, &block);
                let r =
                    input::apply_block(self.input.as_mut(), flags, &block, self.keycode_needs_bit1);
                input_result("encrypted event", r);
                self.input_stats.encrypted += 1;
                self.input_stats.scroll += u64::from(is_wheel_block(&block));
                self.input_stats.enc_keys += u64::from(block[0] == 0xFF);
            }
            ClientMessage::EventMessage2 { hdr_flags, body } => {
                match Event2::parse(hdr_flags, &body) {
                    Ok(Event2::Scroll(ev)) => {
                        self.input_stats.scroll += 1;
                        input_result("scroll", self.input.scroll(&ev))
                    }
                    Ok(Event2::KeyMouse {
                        flags, mut block, ..
                    }) => {
                        ecb_decrypt(&self.auth_key, &mut block);
                        log_event_block("EventMessage2/V2", flags, &block);
                        let r = input::apply_block(
                            self.input.as_mut(),
                            flags,
                            &block,
                            self.keycode_needs_bit1,
                        );
                        input_result("event2 key/mouse", r);
                        self.input_stats.encrypted += 1;
                        self.input_stats.scroll += u64::from(is_wheel_block(&block));
                        self.input_stats.enc_keys += u64::from(block[0] == 0xFF);
                    }
                    Ok(other) => debug!(?other, "EventMessage2 dropped"),
                    Err(e) => warn!("EventMessage2: {e:#}"),
                }
            }
            ClientMessage::SetKeyboardInputSource(payload) => {
                match input::parse_input_source(&payload) {
                    Some(id) => {
                        info!(%id, "SetKeyboardInputSource");
                        input_result("input source", self.input.set_input_source(&id));
                    }
                    None => warn!(len = payload.len(), "SetKeyboardInputSource not decoded"),
                }
            }
            ClientMessage::ClientCutText(text) => {
                let text = String::from_utf8_lossy(&text).into_owned();
                info!(len = text.len(), "ClientCutText");
                if let Some(cb) = &self.cfg.clipboard {
                    self.clip.from_client(cb.as_ref(), text);
                }
            }
            ClientMessage::ViewerInfo(v) => {
                self.keycode_needs_bit1 = v.app_version[0] > 10;
                info!(
                    version = v.version,
                    app_id = v.app_id,
                    app_version = ?v.app_version,
                    os_version = ?v.os_version,
                    features = ?v.feature_bits(),
                    "ViewerInfo"
                );
            }
            ClientMessage::SetEncryption {
                cmd,
                param,
                methods,
            } => {
                info!(cmd, param, ?methods, "SetEncryption");
                self.set_encryption(cmd, &methods).await?;
            }
            ClientMessage::MediaStreamConfig(raw) => self.media_stream_config(&raw).await?,
            ClientMessage::SetDisplayConfiguration(raw) => {
                match apple::parse_display_configuration(&raw) {
                    Ok(req) => {
                        let pref = req.displays.first().and_then(|d| d.preferred());
                        info!(
                            displays = req.displays.len(),
                            pixels = ?pref.map(|m| (m.pixel_width, m.pixel_height)),
                            points = ?pref.map(|m| (m.point_width, m.point_height)),
                            scale = ?pref.map(|m| m.scale()),
                            hz = ?pref.map(|m| m.refresh_hz),
                            "SetDisplayConfiguration"
                        );
                        self.dynamic_resolution =
                            req.displays.first().is_some_and(|d| d.dynamic_resolution());
                        // Like Apple's host: stop HP media, resize, then offer a new stream. The
                        // viewer only ends its resize transition on a new stream's first frame.
                        let restart_hp = self.media_active.then(|| self.cfg.hp.clone()).flatten();
                        if let Some(hp) = &restart_hp {
                            blocking(|| hp.stop(self.id));
                            self.hp_restarting = true;
                            info!("HP media stopped for display change");
                        }
                        if let Some(display) = self.cfg.display.clone() {
                            match blocking(|| display.configure(self.id, &req)) {
                                Ok(Some(applied)) => {
                                    info!(
                                        ?applied,
                                        dynamic = self.dynamic_resolution,
                                        "output resized for viewer"
                                    );
                                    // The viewer waits for display info after every msg 29,
                                    // even when the size is unchanged.
                                    if applied.pixels == self.fb_size {
                                        self.send_display_info(applied).await?;
                                    } else {
                                        self.display_applied = Some(applied);
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => warn!("display resize failed: {e:#}"),
                            }
                        }
                    }
                    Err(e) => warn!(
                        len = raw.len(),
                        "SetDisplayConfiguration not decoded: {e:#}"
                    ),
                }
                if let Some(hp) = &self.cfg.hp {
                    hp.set_display_configuration(&raw)?;
                }
                if self.media_active {
                    self.sync_size().await?;
                    self.hp_offered = false;
                    self.offer_media_stream().await?;
                }
            }
            ClientMessage::ClipboardRequest { flags, opaque } => {
                debug!(flags, opaque, "ViewerClipboardRequest");
                self.clipboard_request(opaque).await?;
            }
            ClientMessage::AutoPasteboard { cmd } => {
                let on = cmd == clip_proto::AUTO_PASTEBOARD_ON;
                info!(cmd, on, "AutoPasteboardCommand");
                self.set_shared_clipboard(on).await?;
            }
            ClientMessage::ClipboardSend {
                flags,
                session,
                uncompressed_len,
                data,
            } => {
                debug!(
                    flags,
                    session,
                    uncompressed_len,
                    len = data.len(),
                    "ViewerClipboardSend"
                );
                self.clipboard_send(flags, uncompressed_len, &data);
            }
            ClientMessage::Ignored { kind, len } => debug!(kind, len, "ignored client message"),
        }
        Ok(())
    }

    async fn tick(&mut self, cursor: &dyn CursorSource) -> Result<()> {
        self.log_input_stats();
        self.check_media().await?;
        let changed = !self.media_active && blocking(|| self.source.advance());
        self.sync_size().await?;
        self.sync_cursor(cursor).await?;
        self.sync_device_info().await?;
        self.sync_shared_clipboard().await?;
        if let Some(cb) = self.cfg.clipboard.clone()
            && let Some(text) = self.clip.pending(cb.as_ref())
            && !self.shared_clipboard
        {
            self.out
                .send(&pseudo::server_cut_text(text.as_bytes()))
                .await?;
            debug!(len = text.len(), "sent ServerCutText");
        }
        if changed && (self.pending.is_some() || self.auto.is_some()) {
            self.pending = None;
            let rects = damage_rects(self.source.damage(), self.fb_size);
            if !rects.is_empty() {
                self.send_frame(&rects).await?;
            }
        }
        Ok(())
    }

    async fn send_frame(&mut self, rects: &[Rect]) -> Result<()> {
        // With HP active, fb_size follows the applied display mode while the paused source
        // still holds a frame at the old size.
        let f = self.source.frame();
        let (fw, fh) = (f.width, f.height);
        let rects: Vec<Rect> = rects
            .iter()
            .filter(|&&(x, y, _, _)| x < fw && y < fh)
            .map(|&(x, y, w, h)| (x, y, w.min(fw - x), h.min(fh - y)))
            .filter(|&(_, _, w, h)| w > 0 && h > 0)
            .collect();
        if rects.is_empty() {
            return Ok(());
        }
        let rects = &rects[..];
        let t0 = std::time::Instant::now();
        let msg = self
            .encoder
            .update_rects(self.source.frame(), &self.pf, self.encoding, rects)?;
        self.stats.encode += t0.elapsed();
        self.out.send(&msg).await?;
        self.stats.updates += 1;
        self.stats.bytes += msg.len() as u64;
        self.stats.rects += rects.len() as u64;
        let secs = self.stats.since.elapsed().as_secs_f64();
        if secs >= 5.0 {
            let s = &self.stats;
            info!(
                fps = format_args!("{:.1}", s.updates as f64 / secs),
                mbps = format_args!("{:.1}", s.bytes as f64 * 8.0 / secs / 1e6),
                rects_per_update = format_args!("{:.1}", s.rects as f64 / s.updates as f64),
                encode_ms_avg =
                    format_args!("{:.1}", s.encode.as_secs_f64() * 1000.0 / s.updates as f64),
                "RFB stats"
            );
            self.stats = RfbStats::new();
        }
        debug!(
            bytes = msg.len(),
            encoding = self.encoding,
            rects = rects.len(),
            first = ?rects.first(),
            "sent FramebufferUpdate"
        );
        Ok(())
    }

    async fn sync_size(&mut self) -> Result<()> {
        // The RFB source stops advancing once HP media is active, so its size can predate the
        // viewer's resize; the applied display mode is the real size then.
        let size = if self.media_active {
            match &self.display_applied {
                Some(a) => a.pixels,
                None => return Ok(()),
            }
        } else {
            self.source.size()
        };
        if size == self.fb_size {
            return Ok(());
        }
        if !self.supports(ENC_DESKTOP_SIZE) {
            warn!(
                ?size,
                "framebuffer size changed but the client lacks DesktopSize"
            );
            return Ok(());
        }
        self.out.send(&pseudo::desktop_size(size.0, size.1)).await?;
        info!(?size, "sent DesktopSize");
        self.input.set_extent(size.0 as u32, size.1 as u32);
        if let Some(applied) = self.display_applied.take()
            && applied.pixels == size
        {
            self.send_display_info(applied).await?;
        }
        self.fb_size = size;
        self.pending = Some((0, 0, size.0, size.1));
        if self.auto.is_some() {
            self.auto = Some((0, 0, size.0, size.1));
        }
        Ok(())
    }

    async fn send_display_info(&mut self, d: AppliedDisplay) -> Result<()> {
        const DISPLAY_ID: u32 = 1;
        let (w, h) = d.pixels;
        let msg = if self.supports(pseudo::ENC_DISPLAY_INFO2) {
            let entry = DisplayInfo2Entry {
                backing_scale: d.scale,
                scale_factor: 1.0,
                display_id: DISPLAY_ID,
                points: [0, 0, d.points.1 as i16, d.points.0 as i16],
                framebuffer: [0, 0, h as i16, w as i16],
                flags: if self.dynamic_resolution {
                    SCREEN_FLAG_MAIN | SCREEN_FLAG_DYNAMIC_RESOLUTION
                } else {
                    SCREEN_FLAG_MAIN
                },
            };
            let payload = apple::encode_display_info2(
                d.points,
                d.pixels,
                DISPLAY_ID,
                DISPLAY_INFO2_VIRTUAL | DISPLAY_INFO2_ON_CONSOLE,
                &[entry],
            );
            pseudo::single_rect(0, 0, w, h, pseudo::ENC_DISPLAY_INFO2, &payload)
        } else if self.supports(pseudo::ENC_DISPLAY_INFO) {
            let entry = apple::DisplayInfoEntry {
                display_id: DISPLAY_ID,
                pixel_width: w,
                pixel_height: h,
            };
            let payload = apple::encode_display_info(d.points.0, d.points.1, 0, &[entry]);
            pseudo::single_rect(0, 0, w, h, pseudo::ENC_DISPLAY_INFO, &payload)
        } else {
            return Ok(());
        };
        self.out.send(&msg).await?;
        info!(
            pixels = ?d.pixels,
            points = ?d.points,
            scale = d.scale,
            dynamic = self.dynamic_resolution,
            "sent display info"
        );
        Ok(())
    }

    async fn sync_cursor(&mut self, cursor: &dyn CursorSource) -> Result<()> {
        if let Some(img) = cursor.image()
            && self.cursor_serial != Some(img.serial)
        {
            let msg = if self.supports(ENC_CURSOR_ALPHA) && self.pf.bits_per_pixel == 32 {
                let (id, cached) = match self.cursor_ids.get(&img.serial) {
                    Some(&id) => (id, true),
                    None => {
                        let id = self.next_cursor_id;
                        self.next_cursor_id += 1;
                        self.cursor_ids.insert(img.serial, id);
                        (id, false)
                    }
                };
                Some(pseudo::cursor_alpha(img, id, &self.pf, cached)?)
            } else if self.supports(pseudo::ENC_RICH_CURSOR) {
                Some(pseudo::rich_cursor(img, &self.pf))
            } else {
                None
            };
            if let Some(msg) = msg {
                self.out.send(&msg).await?;
                debug!(serial = img.serial, bytes = msg.len(), "sent cursor image");
                self.cursor_serial = Some(img.serial);
            }
        }
        if let Some(pos) = cursor.position()
            && self.cursor_pos != Some(pos)
            && self.supports(ENC_CURSOR_POSITION)
            && pos.0 < self.fb_size.0
            && pos.1 < self.fb_size.1
        {
            self.out
                .send(&pseudo::cursor_position(pos.0, pos.1))
                .await?;
            self.cursor_pos = Some(pos);
        }
        Ok(())
    }

    async fn sync_device_info(&mut self) -> Result<()> {
        if self.device_info_sent || !self.supports(ENC_DEVICE_INFO) {
            return Ok(());
        }
        if let Some(d) = &self.cfg.device_info {
            let msg = pseudo::device_info(&d.identifier, &d.color, &d.enclosure_color);
            self.out.send(&msg).await?;
            info!(identifier = %d.identifier, "sent DeviceInfo");
        }
        self.device_info_sent = true;
        Ok(())
    }

    async fn offer_media_stream(&mut self) -> Result<()> {
        let Some(hp) = &self.cfg.hp else {
            return Ok(());
        };
        if self.hp_offered || !self.supports(ENC_MEDIA_STREAM) {
            return Ok(());
        }
        let ports = hp.ports();
        self.out.send(&media::ports_update(&ports)).await?;
        info!(?ports, "sent media stream ports (1010 type 1)");
        self.hp_offered = true;
        Ok(())
    }

    async fn media_stream_config(&mut self, raw: &[u8]) -> Result<()> {
        let Some(hp) = self.cfg.hp.clone() else {
            warn!(
                len = raw.len(),
                "MediaStreamConfiguration without an HP backend"
            );
            return Ok(());
        };
        let reply = match MediaStreamConfig::parse(raw) {
            Ok(config) => {
                info!(
                    version = config.version,
                    flags = config.flags,
                    video2 = config.video2.is_some(),
                    "MediaStreamConfiguration"
                );
                match blocking(|| hp.configure(self.id, self.peer, config)) {
                    Ok(Some(answer)) => {
                        self.media_active = true;
                        self.hp_restarting = false;
                        info!("HP media active; pausing RFB framebuffer updates");
                        // The viewer blanks the session if the new size arrives after the answer.
                        self.sync_size().await?;
                        media::answer_update(&answer)
                    }
                    Ok(None) => return Ok(()),
                    Err(e) => {
                        warn!("HP backend failed: {e:#}");
                        let resume = self.media_active;
                        self.media_active = false;
                        self.hp_restarting = false;
                        self.out.send(&media::error_update(2, 0)).await?;
                        if !resume {
                            return Ok(());
                        }
                        let full = (0, 0, self.fb_size.0, self.fb_size.1);
                        self.pending = Some(full);
                        return self.send_frame(&[full]).await;
                    }
                }
            }
            Err(e) => return Err(e.context("parsing MediaStreamConfiguration")),
        };
        self.out.send(&reply).await
    }

    async fn set_encryption(&mut self, cmd: u16, methods: &[u32]) -> Result<()> {
        if !self.cfg.stream_encryption {
            warn!(
                cmd,
                "SetEncryption received but stream encryption is disabled"
            );
            return Ok(());
        }
        if cmd != 1 || self.out.enc.is_some() {
            return Ok(());
        }
        if !methods.contains(&ENCRYPTION_METHOD) {
            warn!(?methods, "client offered no supported encryption method");
            return Ok(());
        }
        let (mut key, mut iv) = ([0u8; 16], [0u8; 16]);
        rand::fill(&mut key[..]);
        rand::fill(&mut iv[..]);
        self.out
            .send(&crypt::encryption_update(&self.auth_key, &key, &iv))
            .await?;
        self.out.enc = Some(RecordWriter::new(&key, &iv));
        *self.keys.lock().unwrap() = Some((key, iv));
        info!("stream encryption on (server to client)");
        Ok(())
    }
}

async fn reader(mut rd: OwnedReadHalf, tx: Inbound, keys: StreamKeys) {
    loop {
        let msg = messages::read(&mut rd).await;
        let switch = matches!(
            msg,
            Ok(ClientMessage::SetEncryption {
                cmd: 2,
                param: 1,
                ..
            })
        );
        let done = msg.is_err();
        if tx.send(msg).await.is_err() || done {
            return;
        }
        if switch {
            break;
        }
    }
    let Some((key, iv)) = *keys.lock().unwrap() else {
        let _ = tx
            .send(Err(anyhow!(
                "client turned on stream decryption before {MSG_SET_ENCRYPTION} keys were sent"
            )))
            .await;
        return;
    };
    info!("stream encryption on (client to server)");
    let (mut plain_rd, mut plain_wr) = tokio::io::duplex(1 << 16);
    let pump_tx = tx.clone();
    tokio::spawn(async move {
        let mut records = RecordReader::new(&key, &iv);
        let result: Result<()> = async {
            loop {
                let t = rd.read_u16().await? as usize;
                let mut record = vec![0u8; t];
                rd.read_exact(&mut record).await?;
                plain_wr.write_all(&records.open(&record)?).await?;
            }
        }
        .await;
        if let Err(e) = result
            && !is_eof(&e)
        {
            let _ = pump_tx
                .send(Err(e.context("decrypting client stream")))
                .await;
        }
    });
    loop {
        let msg = messages::read(&mut plain_rd).await;
        let done = msg.is_err();
        if tx.send(msg).await.is_err() || done {
            return;
        }
    }
}

/// Runs blocking work (capture, encoder setup) without stalling other tasks on the runtime.
fn blocking<R>(f: impl FnOnce() -> R) -> R {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(h) if h.runtime_flavor() == RuntimeFlavor::MultiThread => tokio::task::block_in_place(f),
        _ => f(),
    }
}

/// Clips damage to the framebuffer and falls back to one bounding box when there are many rects.
fn damage_rects(damage: Vec<Rect>, (fw, fh): (u16, u16)) -> Vec<Rect> {
    let clipped: Vec<Rect> = damage
        .into_iter()
        .filter_map(|(x, y, w, h)| {
            let (x1, y1) = (
                (x as u32 + w as u32).min(fw as u32),
                (y as u32 + h as u32).min(fh as u32),
            );
            (x < fw && y < fh && x1 > x as u32 && y1 > y as u32)
                .then(|| (x, y, (x1 - x as u32) as u16, (y1 - y as u32) as u16))
        })
        .collect();
    if clipped.len() <= MAX_RECTS {
        return clipped;
    }
    let x0 = clipped.iter().map(|r| r.0).min().unwrap();
    let y0 = clipped.iter().map(|r| r.1).min().unwrap();
    let x1 = clipped.iter().map(|r| r.0 + r.2).max().unwrap();
    let y1 = clipped.iter().map(|r| r.1 + r.3).max().unwrap();
    vec![(x0, y0, x1 - x0, y1 - y0)]
}

fn clamp((x, y, w, h): Rect, (fw, fh): (u16, u16)) -> Rect {
    let x = x.min(fw);
    let y = y.min(fh);
    let w = w.min(fw - x);
    let h = h.min(fh - y);
    if w == 0 || h == 0 {
        (0, 0, fw, fh)
    } else {
        (x, y, w, h)
    }
}

fn log_event_block(what: &str, flags: u8, block: &[u8; 16]) {
    match EventBlock::parse(block) {
        Ok(ev) => {
            if let Some((down, keysym)) = ev.key {
                tracing::trace!(
                    flags,
                    down,
                    keysym = format_args!("{keysym:#x}"),
                    "{what} key"
                );
            }
            if let Some((buttons, x, y)) = ev.mouse {
                tracing::trace!(flags, buttons, x, y, "{what} pointer");
            }
        }
        Err(e) => warn!("{what}: {e}"),
    }
}

impl Session {
    /// Falls back to RFB framebuffer updates when HP media stopped on its own.
    async fn check_media(&mut self) -> Result<()> {
        let Some(hp) = &self.cfg.hp else {
            return Ok(());
        };
        if !self.media_active || self.hp_restarting || hp.active(self.id) {
            return Ok(());
        }
        warn!("HP media stopped; falling back to Standard mode");
        blocking(|| hp.stop(self.id));
        self.media_active = false;
        self.out.send(&media::error_update(3, 0)).await?;
        self.pending = Some((0, 0, self.fb_size.0, self.fb_size.1));
        self.send_frame(&[(0, 0, self.fb_size.0, self.fb_size.1)])
            .await
    }
}

impl Session {
    fn log_input_stats(&mut self) {
        let st = &mut self.input_stats;
        let since = *st.since.get_or_insert_with(std::time::Instant::now);
        if since.elapsed() < Duration::from_secs(5) {
            return;
        }
        if st.keys + st.pointer + st.scroll + st.encrypted > 0 {
            info!(
                keys = st.keys,
                pointer = st.pointer,
                scroll = st.scroll,
                encrypted = st.encrypted,
                enc_keys = st.enc_keys,
                "input stats"
            );
        }
        *st = InputStats {
            since: Some(std::time::Instant::now()),
            ..Default::default()
        };
    }
}

/// Apple shared pasteboard (msgs 11/21/31, MiscStateChange 2/3).
impl Session {
    async fn set_shared_clipboard(&mut self, on: bool) -> Result<()> {
        let Some(cb) = self.cfg.clipboard_backend.clone() else {
            return Ok(());
        };
        self.shared_clipboard = on;
        if on {
            self.clipboard_serial = cb.serial();
            if !cb.offered().is_empty() {
                self.out
                    .send(&clip_proto::misc_state_change(
                        clip_proto::STATE_PASTEBOARD_UPDATED,
                    ))
                    .await?;
            }
        }
        Ok(())
    }

    async fn sync_shared_clipboard(&mut self) -> Result<()> {
        let Some(cb) = self.cfg.clipboard_backend.clone() else {
            return Ok(());
        };
        if !self.shared_clipboard {
            return Ok(());
        }
        let serial = cb.serial();
        if serial != self.clipboard_serial {
            self.clipboard_serial = serial;
            self.out
                .send(&clip_proto::misc_state_change(
                    clip_proto::STATE_PASTEBOARD_UPDATED,
                ))
                .await?;
            debug!(serial, "server pasteboard updated");
        }
        if cb.take_data_request() {
            self.out
                .send(&clip_proto::misc_state_change(
                    clip_proto::STATE_PASTEBOARD_NEEDS_DATA,
                ))
                .await?;
            debug!("server pasteboard needs the viewer's data");
        }
        Ok(())
    }

    async fn clipboard_request(&mut self, session: u32) -> Result<()> {
        let Some(cb) = self.cfg.clipboard_backend.clone() else {
            return Ok(());
        };
        let content = match tokio::task::spawn_blocking(move || cb.read()).await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                warn!("reading server clipboard: {e:#}");
                Default::default()
            }
            Err(e) => {
                warn!("clipboard read task: {e}");
                Default::default()
            }
        };
        let msg = clip_proto::server_clipboard_data(session, &content);
        self.out.send(&msg).await?;
        info!(session, types = ?content.summary(), "sent ServerClipboardData");
        Ok(())
    }

    fn clipboard_send(&mut self, flags: u8, uncompressed_len: u32, data: &[u8]) {
        let Some(cb) = self.cfg.clipboard_backend.clone() else {
            return;
        };
        let result = match clip_proto::decode_viewer_send(flags, uncompressed_len, data) {
            Ok(clip_proto::ViewerPasteboard::Data(content)) => {
                info!(types = ?content.summary(), "viewer pasteboard data");
                blocking(|| cb.set(content))
            }
            Ok(clip_proto::ViewerPasteboard::Promises(types)) => {
                info!(?types, "viewer pasteboard promises");
                blocking(|| cb.set_promised(types))
            }
            Err(e) => Err(e),
        };
        if let Err(e) = result {
            warn!("viewer pasteboard: {e:#}");
        }
        self.clipboard_serial = cb.serial();
    }
}

/// Teardown on every exit path, including errors and panics in the session task.
impl Drop for Session {
    fn drop(&mut self) {
        input_result("release", self.input.release_all());
        if let Some(hp) = &self.cfg.hp {
            hp.stop(self.id);
        }
        if let Some(display) = &self.cfg.display
            && let Err(e) = display.restore(self.id)
        {
            warn!("restoring output failed: {e:#}");
        }
        debug!(session = self.id, "session torn down");
    }
}

pub(crate) fn canonical_ip(ip: std::net::IpAddr) -> std::net::IpAddr {
    match ip {
        std::net::IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, std::net::IpAddr::V4),
        v4 => v4,
    }
}

/// Detects silently dropped viewers (sleep, Wi-Fi loss) within about 30 s.
fn set_keepalive(stream: &TcpStream) {
    let ka = socket2::TcpKeepalive::new()
        .with_time(Duration::from_secs(15))
        .with_interval(Duration::from_secs(5));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let ka = ka.with_retries(3);
    if let Err(e) = socket2::SockRef::from(stream).set_tcp_keepalive(&ka) {
        warn!("TCP keepalive: {e}");
    }
}

fn input_result(what: &str, r: Result<()>) {
    if let Err(e) = r {
        warn!("input {what}: {e:#}");
    }
}

fn is_eof(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<std::io::Error>()
            .is_some_and(|io| io.kind() == std::io::ErrorKind::UnexpectedEof)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::StaticPassword;
    use crate::rfb::media::{MediaAnswer, MediaPorts, MediaStreamConfig};
    use crate::rfb::{AuthConfig, DisplayControl, handshake::tests::drive_client};
    use std::sync::Mutex as StdMutex;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[derive(Default)]
    struct Log(StdMutex<Vec<String>>);
    impl Log {
        fn push(&self, s: String) {
            self.0.lock().unwrap().push(s);
        }
        fn has(&self, s: &str) -> bool {
            self.0.lock().unwrap().iter().any(|x| x == s)
        }
    }

    struct FakeDisplay(Arc<Log>);
    impl DisplayControl for FakeDisplay {
        fn configure(
            &self,
            session: SessionId,
            _: &apple::DisplayRequest,
        ) -> Result<Option<AppliedDisplay>> {
            self.0.push(format!("configure {session}"));
            Ok(None)
        }
        fn restore(&self, session: SessionId) -> Result<()> {
            self.0.push(format!("restore {session}"));
            Ok(())
        }
    }

    struct FakeHp(Arc<Log>);
    impl media::HpBackend for FakeHp {
        fn ports(&self) -> MediaPorts {
            MediaPorts::default()
        }
        fn configure(
            &self,
            _: SessionId,
            _: SocketAddr,
            _: MediaStreamConfig,
        ) -> Result<Option<MediaAnswer>> {
            Ok(None)
        }
        fn stop(&self, session: SessionId) {
            self.0.push(format!("hp stop {session}"));
        }
    }

    struct FakeInput(Arc<Log>);
    impl InputSink for FakeInput {
        fn key(&mut self, _: bool, _: u32, _: Option<u16>) -> Result<()> {
            Ok(())
        }
        fn pointer(&mut self, x: i32, _: i32, _: u8) -> Result<()> {
            assert!(x != 666, "simulated input backend panic");
            self.0.push(format!("pointer {x}"));
            Ok(())
        }
        fn scroll(&mut self, _: &input::ScrollEvent) -> Result<()> {
            Ok(())
        }
        fn set_extent(&mut self, _: u32, _: u32) {}
        fn set_input_source(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        fn release_all(&mut self) -> Result<()> {
            self.0.push("release".into());
            Ok(())
        }
    }

    fn test_config() -> Config {
        Config::new(
            "t",
            64,
            48,
            AuthConfig {
                dh: Some(Arc::new(StaticPassword::new("wes", "pw"))),
                srp: None,
            },
        )
    }

    /// Runs one session on loopback; returns the client positioned after ServerInit.
    async fn start(cfg: Config) -> Result<(TcpStream, tokio::task::JoinHandle<Result<()>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            run(stream, Arc::new(cfg), 7).await
        });
        let mut client = TcpStream::connect(addr).await?;
        assert_eq!(drive_client(&mut client, "wes", "pw").await?, 0);
        client.write_all(&[0x81]).await?;
        let mut init = [0u8; 24];
        client.read_exact(&mut init).await?;
        let mut name = vec![0u8; u32::from_be_bytes(init[20..24].try_into().unwrap()) as usize];
        client.read_exact(&mut name).await?;
        Ok((client, server))
    }

    const WAIT: Duration = Duration::from_secs(10);

    async fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !cond() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {what}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn has_seq(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    async fn read_until(
        client: &mut TcpStream,
        what: &str,
        done: impl Fn(&[u8]) -> bool,
    ) -> Vec<u8> {
        let deadline = tokio::time::Instant::now() + WAIT;
        let mut got = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        while !done(&got) {
            match tokio::time::timeout_at(deadline, client.read(&mut buf)).await {
                Ok(Ok(n)) if n > 0 => got.extend_from_slice(&buf[..n]),
                Ok(r) => panic!("connection ended while waiting for {what}: {r:?}"),
                Err(_) => panic!("timed out waiting for {what}"),
            }
        }
        got
    }

    async fn session_with(log: Arc<Log>, after_init: &[u8], until: &str) -> Result<()> {
        let mut cfg = test_config();
        cfg.display = Some(Arc::new(FakeDisplay(log.clone())));
        cfg.hp = Some(Arc::new(FakeHp(log.clone())));
        let input_log = log.clone();
        cfg.input = Some(Arc::new(move |_, _| {
            Ok(Box::new(FakeInput(input_log.clone())) as Box<dyn InputSink>)
        }));
        let (mut client, server) = start(cfg).await?;
        client.write_all(after_init).await?;
        wait_until(until, || log.has(until)).await;
        drop(client);
        match server.await {
            Ok(r) => r,
            Err(e) => Err(anyhow!("session task: {e}")),
        }
    }

    async fn read_msg(client: &mut TcpStream, n: usize) -> Vec<u8> {
        let mut b = vec![0u8; n];
        tokio::time::timeout(Duration::from_secs(2), client.read_exact(&mut b))
            .await
            .unwrap()
            .unwrap();
        b
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shared_clipboard_flow() {
        use crate::clipboard::{ClipContent, MemoryBackend, TextBridge, pack};
        let backend = Arc::new(MemoryBackend::default());
        let mut cfg = test_config();
        cfg.clipboard_backend = Some(backend.clone());
        cfg.clipboard = Some(Arc::new(TextBridge::new(backend.clone())));
        let (mut client, server) = start(cfg).await.unwrap();

        client.write_all(&[21, 0, 0, 1, 0, 0, 0, 0]).await.unwrap();
        client.write_all(&[11, 1, 0, 0, 0, 0, 0, 76]).await.unwrap();
        let hdr = read_msg(&mut client, 16).await;
        assert_eq!(&hdr[..8], &[31, 0, 0, 0, 0, 0, 0, 76]);
        let zlen = u32::from_be_bytes(hdr[12..16].try_into().unwrap()) as usize;
        read_msg(&mut client, zlen).await;
        backend.copy(ClipContent::text("from linux"));
        assert_eq!(read_msg(&mut client, 8).await, [20, 0, 0, 4, 0, 1, 0, 2]);

        client.write_all(&[11, 1, 0, 0, 0, 0, 0, 77]).await.unwrap();
        let hdr = read_msg(&mut client, 16).await;
        assert_eq!(&hdr[..8], &[31, 0, 0, 0, 0, 0, 0, 77]);
        let raw_len = u32::from_be_bytes(hdr[8..12].try_into().unwrap()) as usize;
        let z = read_msg(
            &mut client,
            u32::from_be_bytes(hdr[12..16].try_into().unwrap()) as usize,
        )
        .await;
        let items = pack::unpack(&pack::decompress(&z, raw_len).unwrap()).unwrap();
        assert_eq!(
            ClipContent::from_items(&items).as_text().as_deref(),
            Some("from linux")
        );

        let mac = ClipContent::text("from mac");
        let mut send = crate::clipboard::proto::server_clipboard_data(0, &mac);
        send[2] = 0;
        client.write_all(&send).await.unwrap();
        wait_until("pasteboard data", || {
            backend.paste().and_then(|c| c.as_text()).as_deref() == Some("from mac")
        })
        .await;

        send[2] = 1;
        client.write_all(&send).await.unwrap();
        wait_until("promised pasteboard", || backend.paste().is_none()).await;
        assert_eq!(read_msg(&mut client, 8).await, [20, 0, 0, 4, 0, 1, 0, 3]);

        drop(client);
        server.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn undecodable_viewer_pasteboard_keeps_session_running() {
        use crate::clipboard::{ClipContent, MemoryBackend};
        let backend = Arc::new(MemoryBackend::default());
        let mut cfg = test_config();
        cfg.clipboard_backend = Some(backend.clone());
        let (mut client, server) = start(cfg).await.unwrap();

        let bad_send = |ulen: u32, data: &[u8]| {
            let mut m = vec![31, 0, 0, 0, 0, 0, 0, 0];
            m.extend_from_slice(&ulen.to_be_bytes());
            m.extend_from_slice(&(data.len() as u32).to_be_bytes());
            m.extend_from_slice(data);
            m
        };
        client
            .write_all(&bad_send(12, b"not zlib data"))
            .await
            .unwrap();
        client.write_all(&bad_send(u32::MAX, b"x")).await.unwrap();
        client.write_all(&bad_send(5, &[])).await.unwrap();
        let mut good = crate::clipboard::proto::server_clipboard_data(0, &ClipContent::text("ok"));
        good[2] = 0;
        client.write_all(&good).await.unwrap();
        wait_until("pasteboard data", || {
            backend.paste().and_then(|c| c.as_text()).as_deref() == Some("ok")
        })
        .await;

        assert!(!server.is_finished());
        drop(client);
        server.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_tears_down_live_sessions() {
        let log = Arc::new(Log::default());
        let mut cfg = test_config();
        cfg.display = Some(Arc::new(FakeDisplay(log.clone())));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(crate::rfb::serve_on(listener, Arc::new(cfg), async {
            let _ = stop_rx.await;
        }));
        let mut client = TcpStream::connect(addr).await.unwrap();
        assert_eq!(drive_client(&mut client, "wes", "pw").await.unwrap(), 0);
        client.write_all(&[0x81]).await.unwrap();
        client
            .write_all(include_bytes!("../../tests/fixtures/msg29-0.bin"))
            .await
            .unwrap();
        wait_until("configure 1", || log.has("configure 1")).await;
        stop_tx.send(()).unwrap();
        server.await.unwrap().unwrap();
        assert!(log.has("restore 1"), "{:?}", log.0.lock().unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn teardown_on_disconnect() {
        let log = Arc::new(Log::default());
        let mut msgs = include_bytes!("../../tests/fixtures/msg29-0.bin").to_vec();
        msgs.extend_from_slice(&[5, 0, 0, 1, 0, 1]);
        session_with(log.clone(), &msgs, "pointer 1").await.unwrap();
        for e in [
            "configure 7",
            "pointer 1",
            "release",
            "hp stop 7",
            "restore 7",
        ] {
            assert!(log.has(e), "missing {e:?} in {:?}", log.0.lock().unwrap());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_cursor_source_before_auth() {
        let log = Arc::new(Log::default());
        let mut cfg = test_config();
        let cursor_log = log.clone();
        cfg.cursor_source = Arc::new(move || {
            cursor_log.push("cursor".into());
            Box::new(crate::source::TestCursor::default())
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            run(stream, Arc::new(cfg), 7).await
        });
        let mut client = TcpStream::connect(addr).await.unwrap();
        assert_eq!(drive_client(&mut client, "wes", "nope").await.unwrap(), 1);
        assert!(server.await.unwrap().is_err());
        assert!(!log.has("cursor"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn teardown_on_panic() {
        let log = Arc::new(Log::default());
        let mut msgs = include_bytes!("../../tests/fixtures/msg29-0.bin").to_vec();
        msgs.extend_from_slice(&[5, 0, 0x02, 0x9a, 0, 1]);
        let r = session_with(log.clone(), &msgs, "release").await;
        assert!(r.unwrap_err().to_string().contains("panic"));
        for e in ["configure 7", "release", "hp stop 7", "restore 7"] {
            assert!(log.has(e), "missing {e:?} in {:?}", log.0.lock().unwrap());
        }
    }

    struct ResizingDisplay;
    impl DisplayControl for ResizingDisplay {
        fn configure(
            &self,
            _: SessionId,
            _: &apple::DisplayRequest,
        ) -> Result<Option<AppliedDisplay>> {
            Ok(Some(AppliedDisplay {
                pixels: (80, 48),
                points: (40, 24),
                scale: 2.0,
            }))
        }
        fn restore(&self, _: SessionId) -> Result<()> {
            Ok(())
        }
    }

    /// Running between `configure` and `stop`; `configure` fails while the third field is set.
    struct ActiveHp(
        Arc<Log>,
        std::sync::atomic::AtomicBool,
        std::sync::atomic::AtomicBool,
    );
    impl ActiveHp {
        fn new(log: Arc<Log>) -> Self {
            Self(log, Default::default(), Default::default())
        }
    }
    impl media::HpBackend for ActiveHp {
        fn ports(&self) -> MediaPorts {
            self.0.push("ports".into());
            MediaPorts::default()
        }
        fn configure(
            &self,
            _: SessionId,
            _: SocketAddr,
            _: MediaStreamConfig,
        ) -> Result<Option<MediaAnswer>> {
            if self.2.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(anyhow!("configure failed"));
            }
            self.1.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(Some(MediaAnswer {
                flags: 0,
                audio: Vec::new(),
                video1: Vec::new(),
                video2: Vec::new(),
            }))
        }
        fn stop(&self, _: SessionId) {
            self.1.store(false, std::sync::atomic::Ordering::Relaxed);
            self.0.push("hp stop".into());
        }
        fn active(&self, _: SessionId) -> bool {
            self.1.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// The display grows to 80x48 after HP starts, while the paused RFB source still holds a
    /// 64x48 frame; a full update request must not read past it.
    #[tokio::test(flavor = "multi_thread")]
    async fn full_update_after_hp_resize() {
        use crate::rfb::media::{KEY_LEN, StreamOffer};
        let mut cfg = test_config();
        cfg.display = Some(Arc::new(ResizingDisplay));
        cfg.hp = Some(Arc::new(ActiveHp::new(Arc::new(Log::default()))));
        let (mut client, server) = start(cfg).await.unwrap();
        let mut msgs = vec![2, 0, 0, 1];
        msgs.extend_from_slice(&ENC_DESKTOP_SIZE.to_be_bytes());
        msgs.extend_from_slice(include_bytes!("../../tests/fixtures/msg29-0.bin"));
        let offer = |seed| StreamOffer {
            key_viewer_to_server: [seed; KEY_LEN],
            key_server_to_viewer: [seed; KEY_LEN],
            offer: vec![seed; 40],
        };
        let config = MediaStreamConfig {
            version: 3,
            flags: 0b101,
            v3: 0,
            v4: 0,
            session_id: [0; 16],
            audio: offer(1),
            video1: offer(2),
            video2: None,
        };
        msgs.extend_from_slice(&config.encode());
        client.write_all(&msgs).await.unwrap();
        let size = pseudo::desktop_size(80, 48);
        read_until(&mut client, "DesktopSize", |got| has_seq(got, &size)).await;
        client
            .write_all(&[3, 0, 0, 0, 0, 0, 0, 80, 0, 48])
            .await
            .unwrap();
        let raw = [0, 0, 0, 0, 0, 64, 0, 48, 0, 0, 0, 0];
        read_until(&mut client, "full update", |got| has_seq(got, &raw)).await;
        drop(client);
        if let Err(e) = server.await.unwrap() {
            assert!(!format!("{e:#}").contains("panic"), "{e:#}");
        }
    }

    fn media_config() -> MediaStreamConfig {
        use crate::rfb::media::{KEY_LEN, StreamOffer};
        let offer = |seed| StreamOffer {
            key_viewer_to_server: [seed; KEY_LEN],
            key_server_to_viewer: [seed; KEY_LEN],
            offer: vec![seed; 40],
        };
        MediaStreamConfig {
            version: 3,
            flags: 0b101,
            v3: 0,
            v4: 0,
            session_id: [0; 16],
            audio: offer(1),
            video1: offer(2),
            video2: None,
        }
    }

    /// A msg 29 during HP stops the media and offers a new stream (1010 type 1), as Apple's
    /// host does; the viewer only ends its resize transition on a new stream.
    #[tokio::test(flavor = "multi_thread")]
    async fn resize_during_hp_renegotiates() {
        let log = Arc::new(Log::default());
        let mut cfg = test_config();
        cfg.display = Some(Arc::new(ResizingDisplay));
        cfg.hp = Some(Arc::new(ActiveHp::new(log.clone())));
        let (mut client, server) = start(cfg).await.unwrap();
        let mut msgs = vec![2, 0, 0, 2];
        msgs.extend_from_slice(&ENC_MEDIA_STREAM.to_be_bytes());
        msgs.extend_from_slice(&ENC_DESKTOP_SIZE.to_be_bytes());
        msgs.extend_from_slice(include_bytes!("../../tests/fixtures/msg29-0.bin"));
        msgs.extend_from_slice(&media_config().encode());
        client.write_all(&msgs).await.unwrap();
        wait_until("first offer", || log.has("ports")).await;
        assert!(!log.has("hp stop"), "{:?}", log.0.lock().unwrap());
        client
            .write_all(include_bytes!("../../tests/fixtures/msg29-0.bin"))
            .await
            .unwrap();
        wait_until("renegotiation", || log.0.lock().unwrap().len() >= 3).await;
        let events = log.0.lock().unwrap().clone();
        assert_eq!(events, ["ports", "hp stop", "ports"]);
        // The deliberate stop must not look like a media failure (1010 type 3 to the viewer).
        let got = read_for(&mut client, Duration::from_millis(100)).await;
        let err = media::error_update(3, 0);
        assert!(!has_seq(&got, &err), "sent a media error");
        drop(client);
        let _ = server.await.unwrap();
    }

    async fn read_for(client: &mut TcpStream, wait: Duration) -> Vec<u8> {
        let mut got = Vec::new();
        let mut buf = vec![0u8; 1 << 16];
        while let Ok(Ok(n)) = tokio::time::timeout(wait, client.read(&mut buf)).await {
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
        }
        got
    }

    /// A failed renegotiation after a resize falls back to Standard framebuffer updates.
    #[tokio::test(flavor = "multi_thread")]
    async fn failed_hp_renegotiation_resumes_updates() {
        let log = Arc::new(Log::default());
        let hp = Arc::new(ActiveHp::new(log.clone()));
        let mut cfg = test_config();
        cfg.display = Some(Arc::new(ResizingDisplay));
        cfg.hp = Some(hp.clone());
        let (mut client, server) = start(cfg).await.unwrap();
        let mut msgs = vec![2, 0, 0, 3];
        msgs.extend_from_slice(&ENC_RAW.to_be_bytes());
        msgs.extend_from_slice(&ENC_MEDIA_STREAM.to_be_bytes());
        msgs.extend_from_slice(&ENC_DESKTOP_SIZE.to_be_bytes());
        msgs.extend_from_slice(&media_config().encode());
        client.write_all(&msgs).await.unwrap();
        wait_until("HP start", || {
            hp.1.load(std::sync::atomic::Ordering::Relaxed)
        })
        .await;
        client
            .write_all(include_bytes!("../../tests/fixtures/msg29-0.bin"))
            .await
            .unwrap();
        wait_until("renegotiation", || log.0.lock().unwrap().len() >= 3).await;
        hp.2.store(true, std::sync::atomic::Ordering::Relaxed);
        client.write_all(&media_config().encode()).await.unwrap();
        let err = media::error_update(2, 0);
        let raw = [0, 0, 0, 0, 0, 64, 0, 48, 0, 0, 0, 0];
        read_until(&mut client, "Standard update after media error", |got| {
            got.windows(err.len())
                .position(|w| w == err)
                .is_some_and(|at| has_seq(&got[at..], &raw))
        })
        .await;
        drop(client);
        let _ = server.await.unwrap();
    }

    /// Falling back to Standard stops what is left of the HP backend, such as audio.
    #[tokio::test(flavor = "multi_thread")]
    async fn hp_fallback_stops_backend() {
        let log = Arc::new(Log::default());
        let hp = Arc::new(ActiveHp::new(log.clone()));
        let mut cfg = test_config();
        cfg.hp = Some(hp.clone());
        let (mut client, server) = start(cfg).await.unwrap();
        let mut msgs = vec![2, 0, 0, 1];
        msgs.extend_from_slice(&ENC_MEDIA_STREAM.to_be_bytes());
        msgs.extend_from_slice(&media_config().encode());
        client.write_all(&msgs).await.unwrap();
        wait_until("HP start", || {
            hp.1.load(std::sync::atomic::Ordering::Relaxed)
        })
        .await;
        hp.1.store(false, std::sync::atomic::Ordering::Relaxed);
        let err = media::error_update(3, 0);
        read_until(&mut client, "fallback", |got| has_seq(got, &err)).await;
        wait_until("hp stop", || log.has("hp stop")).await;
        drop(client);
        let _ = server.await.unwrap();
    }

    #[test]
    fn damage_is_clipped_and_merged() {
        assert_eq!(
            damage_rects(
                vec![(5, 5, 10, 10), (95, 0, 10, 1), (200, 0, 1, 1)],
                (100, 50)
            ),
            [(5, 5, 10, 10), (95, 0, 5, 1)]
        );
        let many: Vec<Rect> = (0..100).map(|i| (i, i % 40, 1, 1)).collect();
        assert_eq!(damage_rects(many, (100, 50)), [(0, 0, 100, 40)]);
    }
}
