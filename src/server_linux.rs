//! Linux glue between the rfb server and the capture, encode and hp modules.

mod media;

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::capture::CaptureSource;
use crate::encode::{EncoderBackend, EncoderConfig};
use crate::hp::negotiation::{self, AnswerParams, Offer};
use crate::rfb::media::{HpBackend, MediaAnswer, MediaPorts, MediaStreamConfig};
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
        let params = media::MediaParams {
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
                if let Err(e) = media::run_media(params, &thread_stop) {
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
