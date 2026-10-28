//! Authenticated TLS RDP adapter over the shared capture and input factories.

mod encoder;
mod gfx;
mod input;
mod transport;
use crate::config::RdpCodec;
use std::io::{ErrorKind, Write};
use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroUsize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::auth::Authenticator;
use crate::rfb::{AppliedDisplay, AuthConfig, DisplayControl, SessionId};
use anyhow::{Context, Result, ensure};
use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;
use ironrdp_server::tokio_rustls::{TlsAcceptor, rustls};
use ironrdp_server::{
    BitmapUpdate, CredentialDecision, CredentialValidationError, CredentialValidator, Credentials,
    DesktopSize, DisplayUpdate, PixelFormat, RdpServer, RdpServerDisplay, RdpServerDisplayUpdates,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;
use tracing::{Instrument, debug, info, warn};
use zeroize::Zeroizing;

use crate::input::InputFactory;

use crate::source::{Frame, FrameSourceFactory};

/// Each connection owns its capture and input state, including disconnect cleanup.
pub async fn serve(
    listen: SocketAddr,
    identity: &Path,
    frames: FrameSourceFactory,
    inputs: Option<InputFactory>,
    display: Option<Arc<dyn DisplayControl>>,
    auth: AuthConfig,
    codec: RdpCodec,
) -> Result<()> {
    // The protocol library uses Rc internally, so poll connections on this local executor.
    tokio::task::LocalSet::new()
        .run_until(serve_local(
            listen, identity, frames, inputs, display, auth, codec,
        ))
        .await
}

async fn serve_local(
    listen: SocketAddr,
    identity: &Path,
    frames: FrameSourceFactory,
    inputs: Option<InputFactory>,
    display: Option<Arc<dyn DisplayControl>>,
    auth: AuthConfig,
    codec: RdpCodec,
) -> Result<()> {
    ensure!(
        !auth.security_types().is_empty(),
        "RDP requires Apple mode credentials"
    );
    let auth = Arc::new(auth);
    let (tls, fingerprint) = tls_acceptor(identity)?;
    let probe = frames.clone();
    let (width, height) =
        tokio::task::spawn_blocking(move || Ok::<_, anyhow::Error>(probe()?.size())).await??;
    ensure!(width > 0 && height > 0, "empty RDP desktop");
    if codec != RdpCodec::Remotefx {
        match encoder::Encoder::new(256, 256) {
            Ok(encoder) => info!(encoder = encoder.name(), "RDP H.264 available"),
            Err(error) => warn!(%error, "RDP H.264 unavailable; RemoteFX fallback"),
        }
    }
    let listener = bind_listener(listen, Some(&auth)).await?;
    let bound = listener.local_addr()?;
    let mut sessions = tokio::task::JoinSet::new();
    let mut peers = transport::Peers::default();
    info!(listen = %bound, "RDP listener ready with TLS and shared Apple credentials");
    info!(sha256 = %fingerprint, "RDP TLS certificate fingerprint (SHA-256, colon-separated hex)");
    // RFB allocates low IDs; reserve the upper half for RDP display ownership.
    let mut session: SessionId = 1 << 63;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                peers.reap();
                let replacing = if let Some(old) = peers.remove(peer.ip()) {
                    info!(%peer, "RDP same-peer connection replacing stale session");
                    old.abort();
                    // Drop capture and restore the old display before the new login resizes it.
                    tokio::task::yield_now().await;
                    true
                } else { false };
                // Bound unauthenticated handshakes and credential workers as well as sessions.
                if !replacing && sessions.len() >= 8 { drop(stream); continue; }
                let flow = Arc::new(transport::Output::default());
                let socket = match transport::socket_monitor(&stream) {
                    Ok(socket) => socket,
                    Err(error) => { warn!(%peer, %error, "RDP socket setup failed"); continue; }
                };
                let authenticated = Arc::new(AtomicBool::new(false));
                let login = Login { auth: auth.clone(), authenticated: authenticated.clone() };
                let size = DesktopSize { width, height };
                let extent = Arc::new(Mutex::new(size));
                let target = Arc::new(Mutex::new(ResizeState::new(size)));
                let fail_after = if listen.ip().is_loopback() {
                    std::env::var("WAYSHARE_TEST_RDP_FAIL_AFTER").ok().and_then(|s| s.parse().ok())
                } else { None };
                let gfx = (codec != RdpCodec::Remotefx).then(|| gfx::Connection::new(fail_after, codec == RdpCodec::EgfxRemotefx));
                if gfx.is_none() {
                    info!(codec = "RemoteFX/bitmap", reason = "rdp.codec = remotefx", "RDP codec negotiated");
                }
                let gfx_factory = gfx.as_ref().map(|connection| {
                    Box::new(gfx::Factory(connection.clone())) as Box<dyn ironrdp_server::GfxServerFactory>
                });
                let mut server = RdpServer::builder()
                    .with_addr(bound)
                    .with_tls(tls.clone())
                    .with_input_handler(input::Input {
                        factory: inputs.clone(), authenticated: authenticated.clone(),
                        extent: extent.clone(), width, height, sink: None, buttons: 0, x: 0, y: 0, surrogate: [None; 2], shortcut_modifiers: 0,
                    })
                    .with_display_handler(Display {
                        frames: frames.clone(), size: DesktopSize { width, height },
                        authenticated: authenticated.clone(),
                        extent, target, control: display.clone(), session, started: false, gfx,
                        applied: None, client_size: size, legacy_codec: "bitmap", flow: flow.clone(),
                    })
                    .with_display_suppressed_handle(flow.suppressed.clone())
                    .with_gfx_factory(gfx_factory)
                    .with_honor_client_desktop_size(true)
                    .with_credential_validator(Some(Arc::new(login)))
                    .build();
                let span = tracing::info_span!("rdp", %peer, session);
                session += 1;
                let peer_socket = Arc::downgrade(&socket);
                let task = sessions.spawn_local(async move {
                    // An idle pre-login client cannot occupy a slot indefinitely.
                    let deadline = async {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        if authenticated.load(Ordering::Acquire) { std::future::pending::<()>().await; }
                    };
                    tokio::select! {
                        result = server.run_connection(stream) => {
                            let reason = result.err().map_or_else(|| "client session ended".to_owned(), |error| format!("{error:#}"));
                            info!(%peer, %reason, "RDP connection closed");
                        },
                        () = deadline => info!(%peer, "RDP login timed out"),
                        result = transport::watch(socket.clone(), &flow) => {
                            if let Err(error) = result { warn!(%peer, %error, "RDP socket monitor failed"); }
                        },
                    }
                }.instrument(span));
                peers.insert(peer.ip(), task, peer_socket);
            },
            Some(result) = sessions.join_next(), if !sessions.is_empty() => {
                if let Err(error) = result && !error.is_cancelled() {
                    warn!(%error, "RDP session task failed");
                }
            },
        }
    }
}

async fn bind_listener(
    listen: SocketAddr,
    auth: Option<&AuthConfig>,
) -> Result<tokio::net::TcpListener> {
    // Require the same credentials that each connection installs as its validator.
    ensure!(
        listen.ip().is_loopback() || auth.is_some_and(|auth| !auth.security_types().is_empty()),
        "RDP non-loopback listener {listen} requires Apple mode credentials"
    );
    tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding RDP listener {listen}"))
}

struct Login {
    auth: Arc<AuthConfig>,
    authenticated: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl CredentialValidator for Login {
    async fn validate(
        &self,
        credentials: &Credentials,
    ) -> Result<CredentialDecision, CredentialValidationError> {
        let started = tokio::time::Instant::now();
        let mut accepted = false;
        if credentials.domain.as_deref().is_none_or(str::is_empty) {
            let auth = self.auth.clone();
            let user = credentials.username.clone();
            let password = Zeroizing::new(credentials.password.clone());
            match tokio::task::spawn_blocking(move || auth.verify(&user, password.as_bytes())).await
            {
                Ok(valid) => accepted = valid,
                Err(_) => warn!("RDP credential worker failed"),
            }
        }
        if accepted {
            self.authenticated.store(true, Ordering::Release);
            info!("RDP login accepted");
            Ok(CredentialDecision::Accept)
        } else {
            // Sleep only this connection, including failures rejected before credential verification.
            tokio::time::sleep_until(started + Duration::from_secs(2)).await;
            info!("RDP login rejected; no frames sent");
            Ok(CredentialDecision::Reject)
        }
    }
}

fn certificate_fingerprint(cert: &[u8]) -> String {
    Sha256::digest(cert)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn tls_acceptor(path: &Path) -> Result<(TlsAcceptor, String)> {
    if !path.try_exists()? {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let rcgen::CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec![
            "localhost".into(),
            "127.0.0.1".into(),
            "::1".into(),
        ])?;
        let pem = format!("{}{}", cert.pem(), signing_key.serialize_pem());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(path) {
            Ok(mut file) => {
                file.write_all(pem.as_bytes())?;
                file.sync_all()?;
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).context("creating RDP identity"),
        }
    }
    let pem =
        std::fs::read(path).with_context(|| format!("reading RDP identity {}", path.display()))?;
    let cert = CertificateDer::from_pem_slice(&pem).context("reading RDP certificate")?;
    let fingerprint = certificate_fingerprint(cert.as_ref());
    let key = PrivateKeyDer::from_pem_slice(&pem).context("reading RDP private key")?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)?;
    Ok((TlsAcceptor::from(Arc::new(config)), fingerprint))
}

struct Display {
    frames: FrameSourceFactory,
    size: DesktopSize,
    authenticated: Arc<AtomicBool>,
    extent: Arc<Mutex<DesktopSize>>,
    target: Arc<Mutex<ResizeState>>,
    control: Option<Arc<dyn DisplayControl>>,
    session: SessionId,
    started: bool,
    gfx: Option<Arc<gfx::Connection>>,
    applied: Option<AppliedDisplay>,
    client_size: DesktopSize,
    legacy_codec: &'static str,
    flow: Arc<transport::Output>,
}

struct ResizeState {
    target: DesktopSize,
    announced: DesktopSize,
}

impl ResizeState {
    fn new(target: DesktopSize) -> Self {
        Self {
            target,
            announced: target,
        }
    }

    fn reactivate(&mut self, current: DesktopSize, force: bool) -> Option<DesktopSize> {
        if self.target == current && !force {
            return None;
        }
        self.announced = self.target;
        Some(self.announced)
    }
}

fn validate_size(width: u32, height: u32) -> Result<DesktopSize> {
    // Core desktop dimensions may be odd. Display Control validates its own wire constraints.
    ensure!(
        (200..=8192).contains(&width) && (200..=8192).contains(&height),
        "RDP size out of range"
    );
    Ok(DesktopSize {
        width: width as u16,
        height: height as u16,
    })
}

fn layout_size(layout: &DisplayControlMonitorLayout) -> Result<DesktopSize> {
    let monitors = layout.monitors();
    let monitor = monitors
        .iter()
        .find(|monitor| monitor.is_primary())
        .or_else(|| monitors.first())
        .ok_or_else(|| anyhow::anyhow!("RDP layout has no monitors"))?;
    if monitors.len() > 1 {
        for other in monitors
            .iter()
            .filter(|other| !std::ptr::eq(*other, monitor))
        {
            info!(
                ?other,
                "RDP additional monitor ignored; applying primary or first monitor"
            );
        }
    }
    let (width, height) = monitor.dimensions();
    let size = validate_size(width, height)?;
    Ok(size)
}

impl Display {
    fn accept_client_size(&mut self, client_size: DesktopSize) -> DesktopSize {
        debug!(?client_size, "RDP client desktop size");
        if !self.authenticated.load(Ordering::Acquire) {
            return self.size;
        }
        self.client_size = client_size;
        if !self.started {
            let first = !self.started;
            self.started = true;
            if let Ok(size) = validate_size(client_size.width.into(), client_size.height.into()) {
                self.size = size;
                {
                    let mut state = self.target.lock().unwrap();
                    state.target = size;
                    state.announced = size;
                }
                let ready = !first
                    || self.control.as_ref().is_none_or(|control| {
                        control
                            .session_started(self.session)
                            .map_err(|error| {
                                warn!(%error, "RDP display startup failed; scaling frames");
                            })
                            .is_ok()
                    });
                if ready {
                    self.resize_output(size);
                }
            }
        } else {
            // Demand Active already announced this target. Some clients echo their
            // original Bitmap caps on every Confirm Active, even after a resize.
            self.size = self.target.lock().unwrap().announced;
            if self.size != client_size {
                debug!(?client_size, announced = ?self.size, "RDP keeping announced size after client confirm echo");
            }
        }
        *self.extent.lock().unwrap() = self.size;
        self.size
    }

    fn resize_output(&mut self, size: DesktopSize) {
        if let Some(control) = &self.control {
            match control.configure_size(self.session, size.width.into(), size.height.into()) {
                Ok(Some(applied)) => {
                    debug!(
                        ?size,
                        ?applied,
                        "RDP output resized; preserving protocol size"
                    );
                    self.applied = Some(applied);
                }
                Ok(None) => debug!(?size, "RDP output unchanged; scaling frames"),
                Err(error) => warn!(%error, "RDP output resize failed; scaling frames"),
            }
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        if self.started
            && let Some(control) = &self.control
            && let Err(error) = control.restore(self.session)
        {
            warn!(%error, "RDP display restore failed");
        }
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        self.size
    }

    async fn request_initial_size(&mut self, client_size: DesktopSize) -> DesktopSize {
        self.accept_client_size(client_size)
    }

    fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
        debug!(?layout, "RDP Display Control layout request");
        if !self.started || !self.authenticated.load(Ordering::Acquire) {
            return;
        }
        match layout_size(&layout) {
            Ok(size) => {
                let allowed = {
                    let state = self.target.lock().unwrap();
                    state.target != size
                };
                if allowed {
                    self.resize_output(size);
                    self.target.lock().unwrap().target = size;
                    if self
                        .gfx
                        .as_ref()
                        .is_some_and(|connection| connection.uses_egfx())
                    {
                        self.size = size;
                        *self.extent.lock().unwrap() = size;
                    }
                } else if size != self.size {
                    warn!(?size, current = ?self.size, "RDP layout ignored; unchanged target");
                }
            }
            Err(error) => warn!(%error, "RDP layout rejected"),
        }
    }

    fn negotiated_legacy_codec(&mut self, codec: &'static str) {
        self.legacy_codec = codec;
    }

    fn request_refresh(&mut self) {
        self.flow.refresh.store(true, Ordering::Release);
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        ensure!(
            self.authenticated.load(Ordering::Acquire),
            "RDP login required before capture"
        );
        info!("RDP streaming started");
        let (sender, receiver) = mpsc::channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let frames = self.frames.clone();
        let size = self.size;
        let target = self.target.clone();
        let summary = CaptureContext {
            gfx: self.gfx.clone(),
            client: self.client_size,
            applied: self.applied,
            legacy_codec: self.legacy_codec,
            flow: self.flow.clone(),
        };
        let span = tracing::Span::current();
        let thread = std::thread::Builder::new()
            .name("rdp-capture".into())
            .spawn(move || {
                let _guard = span.enter();
                let result = capture(frames, size, &target, &sender, &worker_stop, summary);
                if let Err(e) = result {
                    let _ = sender.blocking_send(Err(e));
                }
            })?;
        Ok(Box::new(Updates {
            flow: self.flow.clone(),
            receiver,
            stop,
            thread: Some(thread),
        }))
    }
}

struct CaptureContext {
    gfx: Option<Arc<gfx::Connection>>,
    client: DesktopSize,
    applied: Option<AppliedDisplay>,
    legacy_codec: &'static str,
    flow: Arc<transport::Output>,
}

fn capture(
    frames: FrameSourceFactory,
    mut size: DesktopSize,
    target: &Mutex<ResizeState>,
    sender: &mpsc::Sender<Result<DisplayUpdate>>,
    stop: &AtomicBool,
    summary: CaptureContext,
) -> Result<()> {
    let gfx = &summary.gfx;
    // Legacy resize updates end this worker. Reactivation opens capture at the new mode
    // and builds a fresh encoder before any bitmap of the new dimensions is sent.
    if !gfx
        .as_ref()
        .is_some_and(|connection| connection.uses_egfx())
        && let Some(requested) = target.lock().unwrap().reactivate(size, false)
    {
        let _ = sender.blocking_send(Ok(DisplayUpdate::Resize(requested)));
        return Ok(());
    }
    let mut source = frames()?;
    let mut first = true;
    let mut dirty = true;
    let mut delivered = 0_u64;
    let mut stream: Option<gfx::Stream> = None;
    while !stop.load(Ordering::Relaxed) {
        let requested = target.lock().unwrap().target;
        if requested != size {
            if gfx
                .as_ref()
                .is_some_and(|connection| connection.uses_egfx())
            {
                if let Some(active) = &mut stream {
                    let messages = active.resize(requested)?;
                    if sender
                        .blocking_send(Ok(DisplayUpdate::Egfx(messages)))
                        .is_err()
                    {
                        break;
                    }
                }
                size = requested;
                target.lock().unwrap().announced = size;
                source = frames()?;
                first = true;
                dirty = true;
            } else {
                let requested = target.lock().unwrap().reactivate(size, false).unwrap();
                let _ = sender.blocking_send(Ok(DisplayUpdate::Resize(requested)));
                return Ok(());
            }
        }
        dirty |= source.advance();
        if let Some(connection) = gfx {
            match connection.mode() {
                gfx::Mode::Waiting => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                gfx::Mode::Avc420 | gfx::Mode::EgfxRemoteFx | gfx::Mode::Planar
                    if stream.is_none() =>
                {
                    match gfx::Stream::new(connection.clone(), size) {
                        Ok(mut new) => {
                            if sender
                                .blocking_send(Ok(DisplayUpdate::Egfx(new.take_control())))
                                .is_err()
                            {
                                break;
                            }
                            stream = Some(new);
                            dirty = true;
                        }
                        Err(error) => {
                            connection.fallback(&format!("EGFX initialization failed: {error:#}"))
                        }
                    }
                }
                gfx::Mode::Legacy if stream.is_some() => {
                    connection.fallback("EGFX channel closed");
                    // A closed DVC can no longer carry DeleteSurface; reactivate legacy graphics.
                    stream = None;
                    if let Some(requested) = target.lock().unwrap().reactivate(size, true) {
                        let _ = sender.blocking_send(Ok(DisplayUpdate::Resize(requested)));
                        return Ok(());
                    }
                }
                _ => (),
            }
        }
        dirty |= summary.flow.refresh.load(Ordering::Acquire);
        if !summary.flow.can_capture() {
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        let refresh = summary.flow.refresh.swap(false, Ordering::AcqRel);
        dirty |= refresh;
        if refresh {
            summary.flow.invalidate.store(true, Ordering::Release);
        }
        if first || dirty || stream.as_ref().is_some_and(gfx::Stream::needs_frame) {
            let frame = source.frame();
            let fitted;
            let frame = if (frame.width, frame.height) == (size.width, size.height) {
                frame
            } else {
                fitted = fit_frame(frame, size)?;
                &fitted
            };
            if first {
                debug!(frame = ?(frame.width, frame.height), ?size, "RDP first frame size");
                let (codec, reason) = if let Some(stream) = &stream {
                    stream.description()
                } else {
                    (
                        summary.legacy_codec,
                        "legacy client capabilities or EGFX fallback",
                    )
                };
                info!(client = ?summary.client, announced = ?size, applied = ?summary.applied,
                    codec, reason, "RDP session summary");
            }
            let update = if let Some(active) = &mut stream {
                if refresh {
                    active.request_refresh();
                }
                match active.frame(frame) {
                    Ok(Some(messages)) => DisplayUpdate::Egfx(messages),
                    Ok(None) => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => {
                        gfx.as_ref()
                            .unwrap()
                            .fallback(&format!("EGFX failed: {error:#}"));
                        if let Ok(messages) = active.retire() {
                            let _ = sender.blocking_send(Ok(DisplayUpdate::Egfx(messages)));
                        }
                        stream = None;
                        if let Some(requested) = target.lock().unwrap().reactivate(size, true) {
                            let _ = sender.blocking_send(Ok(DisplayUpdate::Resize(requested)));
                            return Ok(());
                        }
                        continue;
                    }
                }
            } else {
                DisplayUpdate::Bitmap(bitmap(frame)?)
            };
            summary.flow.in_flight.store(true, Ordering::Release);
            if sender.blocking_send(Ok(update)).is_err() {
                break;
            }
            delivered += 1;
            debug!(frames = delivered, "RDP frame queued");
            first = false;
            dirty = false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn fit_frame(frame: &Frame, size: DesktopSize) -> Result<Frame> {
    ensure!(
        frame.width > 0 && frame.height > 0 && size.width > 0 && size.height > 0,
        "empty RDP frame"
    );
    ensure!(
        frame.pixels.len() == usize::from(frame.width) * usize::from(frame.height),
        "invalid RDP framebuffer length"
    );
    let mut pixels = Vec::with_capacity(usize::from(size.width) * usize::from(size.height));
    for y in 0..u32::from(size.height) {
        let row = y * u32::from(frame.height) / u32::from(size.height);
        for x in 0..u32::from(size.width) {
            let col = x * u32::from(frame.width) / u32::from(size.width);
            pixels.push(frame.pixels[(row * u32::from(frame.width) + col) as usize]);
        }
    }
    Ok(Frame {
        width: size.width,
        height: size.height,
        pixels,
    })
}

fn bitmap(frame: &Frame) -> Result<BitmapUpdate> {
    let width = NonZeroU16::new(frame.width).context("empty RDP frame width")?;
    let height = NonZeroU16::new(frame.height).context("empty RDP frame height")?;
    ensure!(
        frame.pixels.len() == usize::from(frame.width) * usize::from(frame.height),
        "invalid RDP framebuffer length"
    );
    // Explicit bytes preserve the shared 0x00RRGGBB format on either host byte order.
    let data: Vec<u8> = frame.pixels.iter().flat_map(|p| p.to_le_bytes()).collect();
    Ok(BitmapUpdate {
        x: 0,
        y: 0,
        width,
        height,
        format: PixelFormat::BgrX32,
        data: data.into(),
        stride: NonZeroUsize::from(width)
            .checked_mul(NonZeroUsize::new(4).unwrap())
            .unwrap(),
    })
}

struct Updates {
    flow: Arc<transport::Output>,
    receiver: mpsc::Receiver<Result<DisplayUpdate>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    fn update_sent(&mut self) {
        self.flow.in_flight.store(false, Ordering::Release);
    }

    fn take_refresh(&mut self) -> bool {
        self.flow.invalidate.swap(false, Ordering::AcqRel)
    }

    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        // recv is cancellation safe; capture never runs inside the selected future.
        self.receiver.recv().await.transpose()
    }
}

impl Drop for Updates {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.flow.in_flight.store(false, Ordering::Release);
        self.receiver.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::TestPattern;

    async fn completed_update(
        updates: &mut Box<dyn RdpServerDisplayUpdates>,
    ) -> Result<Option<DisplayUpdate>> {
        updates.update_sent();
        updates.next_update().await
    }

    fn display(size: DesktopSize) -> Display {
        Display {
            frames: Arc::new(|| Ok(Box::new(TestPattern::new(64, 64)))),
            size,
            extent: Arc::new(Mutex::new(size)),
            target: Arc::new(Mutex::new(ResizeState::new(size))),
            authenticated: Arc::new(AtomicBool::new(true)),
            control: None,
            session: 1 << 63,
            started: false,
            gfx: None,
            applied: None,
            client_size: size,
            legacy_codec: "bitmap",
            flow: Arc::default(),
        }
    }

    #[test]
    fn display_control_caps_allow_an_8192_square_output() {
        let caps = ironrdp_server::display_control::capabilities(16);
        assert_eq!(caps.max_monitor_area(), 16 * 8192 * 8192);
        let bytes =
            ironrdp_core::encode_vec(&ironrdp_displaycontrol::pdu::DisplayControlPdu::Caps(caps))
                .unwrap();
        assert_eq!(bytes.len(), 20);
        assert_eq!(&bytes[8..12], &16_u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &8192_u32.to_le_bytes());
        assert_eq!(&bytes[16..20], &8192_u32.to_le_bytes());
    }

    #[test]
    fn iphone_portrait_layout_preserves_zero_wire_orientation() {
        // iPhone Windows App payload captured on 2026-10-28 after Save Session Info.
        let bytes = [
            2, 0, 0, 0, 56, 0, 0, 0, 40, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            68, 3, 0, 0, 44, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100, 0, 0, 0, 100, 0, 0,
            0,
        ];
        let ironrdp_displaycontrol::pdu::DisplayControlPdu::MonitorLayout(layout) =
            ironrdp_core::decode(&bytes).unwrap()
        else {
            panic!("expected monitor layout");
        };
        assert_eq!(
            layout_size(&layout).unwrap(),
            DesktopSize {
                width: 836,
                height: 1580
            }
        );
        assert_eq!(
            layout.monitors()[0].orientation(),
            Some(ironrdp_displaycontrol::pdu::MonitorOrientation::Landscape)
        );
    }

    #[test]
    fn resize_validation_accepts_portrait_and_rejects_invalid_sizes_and_layouts() {
        for (width, height) in [
            (200, 200),
            (8192, 8192),
            (836, 1580),
            (1580, 797),
            (1579, 797),
        ] {
            assert!(validate_size(width, height).is_ok());
        }
        for (width, height) in [
            (0, 0),
            (199, 200),
            (200, 199),
            (8194, 200),
            (200, 8193),
            (u32::MAX, 200),
        ] {
            assert!(validate_size(width, height).is_err());
        }
        use ironrdp_displaycontrol::pdu::MonitorLayoutEntry;
        let primary = MonitorLayoutEntry::new_primary(836, 1580).unwrap();
        let secondary = MonitorLayoutEntry::new_secondary(800, 600).unwrap();
        let layout = DisplayControlMonitorLayout::new(std::slice::from_ref(&primary)).unwrap();
        assert_eq!(
            layout_size(&layout).unwrap(),
            DesktopSize {
                width: 836,
                height: 1580
            }
        );
        assert_eq!(
            layout_size(&DisplayControlMonitorLayout::new(&[secondary, primary]).unwrap()).unwrap(),
            DesktopSize {
                width: 836,
                height: 1580
            }
        );
        // Decoding permits a client layout without a primary flag. Use its first monitor.
        let mut bytes = ironrdp_core::encode_vec(&layout).unwrap();
        bytes[8..12].fill(0);
        let no_primary = ironrdp_core::decode::<DisplayControlMonitorLayout>(&bytes).unwrap();
        assert_eq!(
            layout_size(&no_primary).unwrap(),
            DesktopSize {
                width: 836,
                height: 1580
            }
        );
        assert!(
            layout_size(
                &DisplayControlMonitorLayout::new_single_primary_monitor(8192, 8192, None, None)
                    .unwrap()
            )
            .is_ok()
        );
        assert!(
            layout_size(
                &DisplayControlMonitorLayout::new_single_primary_monitor(8192, 1125, None, None)
                    .unwrap()
            )
            .is_ok()
        );
    }

    #[tokio::test]
    async fn initial_size_and_rotation_reactivate_before_new_bitmaps_and_input_extent() {
        let mut display = display(DesktopSize {
            width: 3456,
            height: 2160,
        });
        let portrait = DesktopSize {
            width: 836,
            height: 1580,
        };
        assert_eq!(display.request_initial_size(portrait).await, portrait);
        assert_eq!(*display.extent.lock().unwrap(), portrait);
        let mut updates = display.updates().await.unwrap();
        assert!(
            matches!(completed_update(&mut updates).await.unwrap(), Some(DisplayUpdate::Bitmap(b)) if b.width.get() == 836 && b.height.get() == 1580)
        );
        display.request_layout(
            DisplayControlMonitorLayout::new_single_primary_monitor(1580, 836, None, None).unwrap(),
        );
        assert_eq!(*display.extent.lock().unwrap(), portrait);
        loop {
            if let Some(DisplayUpdate::Resize(size)) = completed_update(&mut updates).await.unwrap()
            {
                assert_eq!(
                    size,
                    DesktopSize {
                        width: 1580,
                        height: 836
                    }
                );
                break;
            }
        }
        drop(updates);
        let landscape = DesktopSize {
            width: 1580,
            height: 836,
        };
        display.request_initial_size(landscape).await;
        assert_eq!(*display.extent.lock().unwrap(), landscape);
        let mut updates = display.updates().await.unwrap();
        assert!(
            matches!(completed_update(&mut updates).await.unwrap(), Some(DisplayUpdate::Bitmap(b)) if b.width.get() == 1580 && b.height.get() == 836)
        );
    }

    #[derive(Default)]
    struct DisplayOwner(Mutex<Option<SessionId>>);

    impl DisplayControl for DisplayOwner {
        fn configure(
            &self,
            _: SessionId,
            _: &crate::display::apple::DisplayRequest,
        ) -> Result<Option<crate::rfb::AppliedDisplay>> {
            unreachable!()
        }
        fn configure_size(
            &self,
            session: SessionId,
            width: u32,
            height: u32,
        ) -> Result<Option<crate::rfb::AppliedDisplay>> {
            let mut owner = self.0.lock().unwrap();
            if owner.is_some_and(|id| id != session) {
                return Ok(None);
            }
            *owner = Some(session);
            Ok(Some(crate::rfb::AppliedDisplay {
                pixels: (width as u16, height as u16),
                points: (width as u16, height as u16),
                scale: 1.0,
            }))
        }
        fn restore(&self, session: SessionId) -> Result<()> {
            let mut owner = self.0.lock().unwrap();
            if *owner == Some(session) {
                *owner = None;
            }
            Ok(())
        }
    }

    struct RoundedDisplay;

    impl DisplayControl for RoundedDisplay {
        fn configure(
            &self,
            _: SessionId,
            _: &crate::display::apple::DisplayRequest,
        ) -> Result<Option<crate::rfb::AppliedDisplay>> {
            unreachable!()
        }
        fn configure_size(
            &self,
            _: SessionId,
            width: u32,
            height: u32,
        ) -> Result<Option<crate::rfb::AppliedDisplay>> {
            Ok(Some(crate::rfb::AppliedDisplay {
                pixels: (width as u16, (height & !1) as u16),
                points: (width as u16 / 2, height as u16 / 2),
                scale: 2.0,
            }))
        }
        fn restore(&self, _: SessionId) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn rounded_modes_keep_exact_desktops_and_confirm_echo_does_not_reactivate() {
        for (width, height) in [
            (836, 1580),
            (1580, 797),
            (2868, 1320),
            (1320, 2868),
            (1579, 797),
            (201, 203),
            (205, 207),
            (809, 601),
        ] {
            let requested = DesktopSize { width, height };
            let mut display = display(DesktopSize {
                width: 3456,
                height: 2160,
            });
            display.control = Some(Arc::new(RoundedDisplay));
            assert_eq!(display.request_initial_size(requested).await, requested);
            let mut updates = display.updates().await.unwrap();
            assert!(
                matches!(completed_update(&mut updates).await.unwrap(), Some(DisplayUpdate::Bitmap(b))
                if (b.width.get(), b.height.get()) == (width, height))
            );
            drop(updates);
            let next = DesktopSize {
                width: height & !1,
                height: width,
            };
            display.request_layout(
                DisplayControlMonitorLayout::new_single_primary_monitor(
                    next.width.into(),
                    next.height.into(),
                    None,
                    None,
                )
                .unwrap(),
            );
            let mut updates = display.updates().await.unwrap();
            assert!(
                matches!(completed_update(&mut updates).await.unwrap(), Some(DisplayUpdate::Resize(s)) if s == next)
            );
            drop(updates);
            // The iPhone repeats its original Bitmap capabilities on Confirm Active.
            assert_eq!(display.request_initial_size(requested).await, next);
            assert_eq!(*display.extent.lock().unwrap(), next);
            let mut updates = display.updates().await.unwrap();
            assert!(
                matches!(completed_update(&mut updates).await.unwrap(), Some(DisplayUpdate::Bitmap(b))
                if (b.width.get(), b.height.get()) == (next.width, next.height))
            );
        }
    }

    #[test]
    fn layouts_queued_during_activation_do_not_change_the_announced_size() {
        let portrait = DesktopSize {
            width: 836,
            height: 1580,
        };
        let landscape = DesktopSize {
            width: 1580,
            height: 797,
        };
        let mut state = ResizeState::new(portrait);
        state.target = landscape;
        assert_eq!(state.reactivate(portrait, false), Some(landscape));
        state.target = portrait;
        assert_eq!(state.announced, landscape);
        assert_eq!(state.reactivate(landscape, false), Some(portrait));
    }

    #[test]
    fn reactivations_have_no_lifetime_limit() {
        let mut current = DesktopSize {
            width: 836,
            height: 1580,
        };
        let mut state = ResizeState::new(current);
        for _ in 0..1000 {
            state.target = DesktopSize {
                width: current.height,
                height: current.width,
            };
            current = state.reactivate(current, false).unwrap();
        }
        assert_eq!(state.reactivate(current, true), Some(current));
    }

    #[tokio::test]
    async fn sessions_share_display_ownership_and_only_the_owner_releases_it() {
        let owner = Arc::new(DisplayOwner::default());
        let mut first = display(DesktopSize {
            width: 3456,
            height: 2160,
        });
        first.control = Some(owner.clone());
        let portrait = DesktopSize {
            width: 836,
            height: 1580,
        };
        first.request_initial_size(portrait).await;
        assert_eq!(*owner.0.lock().unwrap(), Some(first.session));
        let mut second = display(portrait);
        second.session += 1;
        second.control = Some(owner.clone());
        let landscape = DesktopSize {
            width: 1580,
            height: 836,
        };
        second.request_initial_size(landscape).await;
        assert_eq!(*owner.0.lock().unwrap(), Some(first.session));
        // A viewer without mode ownership still receives its negotiated frame size.
        let mut updates = second.updates().await.unwrap();
        assert!(
            matches!(completed_update(&mut updates).await.unwrap(), Some(DisplayUpdate::Bitmap(b)) if b.width.get() == 1580 && b.height.get() == 836)
        );
        drop(updates);
        drop(second);
        assert_eq!(*owner.0.lock().unwrap(), Some(first.session));
        drop(first);
        assert_eq!(*owner.0.lock().unwrap(), None);
    }

    #[tokio::test]
    async fn capture_dimension_changes_keep_frames_inside_the_negotiated_desktop() {
        use crate::source::FrameSource;
        struct ChangingSource(Frame, bool);
        impl FrameSource for ChangingSource {
            fn size(&self) -> (u16, u16) {
                (self.0.width, self.0.height)
            }
            fn advance(&mut self) -> bool {
                if self.1 {
                    self.1 = false;
                } else {
                    self.0 = Frame {
                        width: 4,
                        height: 2,
                        pixels: vec![7; 8],
                    };
                }
                true
            }
            fn frame(&self) -> &Frame {
                &self.0
            }
        }
        let size = DesktopSize {
            width: 200,
            height: 400,
        };
        let mut display = display(size);
        display.frames = Arc::new(|| {
            Ok(Box::new(ChangingSource(
                Frame {
                    width: 2,
                    height: 4,
                    pixels: vec![1; 8],
                },
                true,
            )))
        });
        let mut updates = display.updates().await.unwrap();
        for color in [1, 7] {
            assert!(
                matches!(completed_update(&mut updates).await.unwrap(), Some(DisplayUpdate::Bitmap(b)) if b.width.get() == 200 && b.height.get() == 400 && b.data[0] == color)
            );
        }
    }

    #[tokio::test]
    async fn pending_output_keeps_only_the_latest_capture_and_resume_redraws() {
        use crate::source::FrameSource;
        use std::sync::atomic::AtomicU32;
        struct Source {
            color: Arc<AtomicU32>,
            observed: Arc<AtomicU32>,
            frame: Frame,
        }
        impl FrameSource for Source {
            fn size(&self) -> (u16, u16) {
                (200, 200)
            }
            fn advance(&mut self) -> bool {
                let color = self.color.load(Ordering::Acquire);
                let dirty = self.frame.pixels[0] != color;
                self.frame.pixels.fill(color);
                self.observed.store(color, Ordering::Release);
                dirty
            }
            fn frame(&self) -> &Frame {
                &self.frame
            }
        }
        let color = Arc::new(AtomicU32::new(1));
        let c = color.clone();
        let observed = Arc::new(AtomicU32::new(0));
        let source_observed = observed.clone();
        let mut display = display(DesktopSize {
            width: 200,
            height: 200,
        });
        display.frames = Arc::new(move || {
            Ok(Box::new(Source {
                color: c.clone(),
                observed: source_observed.clone(),
                frame: Frame {
                    width: 200,
                    height: 200,
                    pixels: vec![1; 40000],
                },
            }))
        });
        let mut updates = display.updates().await.unwrap();
        assert!(matches!(
            updates.next_update().await.unwrap(),
            Some(DisplayUpdate::Bitmap(_))
        ));
        color.store(2, Ordering::Release);
        assert!(
            tokio::time::timeout(Duration::from_millis(160), updates.next_update())
                .await
                .is_err()
        );
        color.store(7, Ordering::Release);
        tokio::time::timeout(Duration::from_secs(2), async {
            while observed.load(Ordering::Acquire) != 7 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        updates.update_sent();
        let update = tokio::time::timeout(Duration::from_secs(2), updates.next_update())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(update, Some(DisplayUpdate::Bitmap(b)) if b.data[0] == 7));
        display.flow.suppressed.store(true, Ordering::Release);
        updates.update_sent();
        display.request_refresh();
        assert!(
            tokio::time::timeout(Duration::from_millis(160), updates.next_update())
                .await
                .is_err()
        );
        display.flow.suppressed.store(false, Ordering::Release);
        let update = tokio::time::timeout(Duration::from_secs(2), updates.next_update())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(update, Some(DisplayUpdate::Bitmap(b)) if b.data[0] == 7));
        assert!(updates.take_refresh());
    }

    #[test]
    fn scaled_frame_covers_the_whole_source() {
        let frame = Frame {
            width: 4,
            height: 2,
            pixels: vec![1, 2, 3, 4, 5, 6, 7, 8],
        };
        let fitted = fit_frame(
            &frame,
            DesktopSize {
                width: 2,
                height: 4,
            },
        )
        .unwrap();
        assert_eq!(fitted.pixels, vec![1, 3, 1, 3, 5, 7, 5, 7]);
        assert_eq!((fitted.width, fitted.height), (2, 4));
    }

    #[tokio::test]
    async fn network_bind_requires_apple_credentials_before_opening_a_socket() {
        for address in ["0.0.0.0:0", "[::]:0", "192.0.2.1:0", "[2001:db8::1]:0"] {
            let error = bind_listener(address.parse().unwrap(), None)
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("requires Apple mode credentials")
            );
        }
        let local = bind_listener("127.0.0.1:0".parse().unwrap(), None)
            .await
            .unwrap();
        assert!(local.local_addr().unwrap().ip().is_loopback());
        let auth = AuthConfig {
            dh: Some(Arc::new(crate::auth::StaticPassword::new("viewer", "test"))),
            ..Default::default()
        };
        let network = bind_listener("0.0.0.0:0".parse().unwrap(), Some(&auth))
            .await
            .unwrap();
        assert!(network.local_addr().unwrap().ip().is_unspecified());
        assert!(
            bind_listener("0.0.0.0:0".parse().unwrap(), Some(&AuthConfig::default()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn login_accepts_apple_srp_username_without_local_account_rules() {
        let authenticated = Arc::new(AtomicBool::new(false));
        let login = Login {
            auth: Arc::new(AuthConfig {
                srp: Some(crate::rfb::srp::VerifierRecord::generate(
                    "Viewer@rdp",
                    b"test",
                    4,
                )),
                dh: None,
            }),
            authenticated: authenticated.clone(),
        };
        let mut credentials = Credentials {
            username: "Viewer@rdp".into(),
            password: "test".into(),
            domain: Some(String::new()),
        };
        assert!(matches!(
            login.validate(&credentials).await.unwrap(),
            CredentialDecision::Accept
        ));
        assert!(authenticated.load(Ordering::Acquire));
        authenticated.store(false, Ordering::Release);
        credentials.domain = Some("DOMAIN".into());
        assert!(matches!(
            login.validate(&credentials).await.unwrap(),
            CredentialDecision::Reject
        ));
        assert!(!authenticated.load(Ordering::Acquire));
    }

    #[test]
    fn fingerprint_is_sha256_in_colon_hex_form() {
        assert_eq!(
            certificate_fingerprint(b"abc"),
            "BA:78:16:BF:8F:01:CF:EA:41:41:40:DE:5D:AE:22:23:B0:03:61:A3:96:17:7A:9C:B4:10:FF:61:F2:00:15:AD"
        );
    }

    #[tokio::test]
    async fn denied_login_cannot_start_capture_and_failure_delay_is_per_connection() {
        let authenticated = Arc::new(AtomicBool::new(false));
        let login = Login {
            auth: Arc::new(AuthConfig {
                dh: Some(Arc::new(crate::auth::StaticPassword::new(
                    "allowed", "test",
                ))),
                ..Default::default()
            }),
            authenticated: authenticated.clone(),
        };
        let credentials = Credentials {
            username: "other".into(),
            password: "wrong".into(),
            domain: None,
        };
        let mut display = Display {
            frames: Arc::new(|| panic!("capture before authentication")),
            size: DesktopSize {
                width: 64,
                height: 64,
            },
            extent: Arc::new(Mutex::new(DesktopSize {
                width: 64,
                height: 64,
            })),
            target: Arc::new(Mutex::new(ResizeState::new(DesktopSize {
                width: 64,
                height: 64,
            }))),
            control: None,
            session: 1 << 63,
            started: false,
            gfx: None,
            applied: None,
            client_size: DesktopSize {
                width: 64,
                height: 64,
            },
            legacy_codec: "bitmap",
            flow: Arc::default(),
            authenticated: authenticated.clone(),
        };
        assert!(display.updates().await.is_err());
        let started = tokio::time::Instant::now();
        let both =
            async { tokio::join!(login.validate(&credentials), login.validate(&credentials)) };
        let (first, second) = tokio::time::timeout(Duration::from_secs(4), both)
            .await
            .unwrap();
        assert!(matches!(first.unwrap(), CredentialDecision::Reject));
        assert!(matches!(second.unwrap(), CredentialDecision::Reject));
        assert!(started.elapsed() >= Duration::from_secs(2));
        assert!(!authenticated.load(Ordering::Acquire));
    }

    #[test]
    fn bitmap_preserves_colors_and_rejects_invalid_frames() {
        let mut frame = Frame {
            width: 2,
            height: 1,
            pixels: vec![0x00123456, 0x00abcdef],
        };
        let update = bitmap(&frame).unwrap();
        assert_eq!(
            &update.data[..],
            &[0x56, 0x34, 0x12, 0, 0xef, 0xcd, 0xab, 0]
        );
        assert_eq!(update.stride.get(), 8);
        frame.pixels.pop();
        assert!(bitmap(&frame).is_err());
        frame.width = 0;
        assert!(bitmap(&frame).is_err());
    }

    #[tokio::test]
    async fn first_frame_arrives_and_disconnect_releases_capture() {
        let mut display = Display {
            frames: Arc::new(|| Ok(Box::new(TestPattern::new(64, 64)))),
            authenticated: Arc::new(AtomicBool::new(true)),
            size: DesktopSize {
                width: 64,
                height: 64,
            },
            extent: Arc::new(Mutex::new(DesktopSize {
                width: 64,
                height: 64,
            })),
            target: Arc::new(Mutex::new(ResizeState::new(DesktopSize {
                width: 64,
                height: 64,
            }))),
            control: None,
            session: 1 << 63,
            started: false,
            gfx: None,
            applied: None,
            client_size: DesktopSize {
                width: 64,
                height: 64,
            },
            legacy_codec: "bitmap",
            flow: Arc::default(),
        };
        let mut updates = display.updates().await.unwrap();
        let update = tokio::time::timeout(Duration::from_secs(5), updates.next_update())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            matches!(update, DisplayUpdate::Bitmap(b) if b.width.get() == 64 && b.height.get() == 64)
        );
        // Fill the bounded queue before Drop so cleanup must unblock the producer.
        tokio::time::sleep(Duration::from_millis(350)).await;
        drop(updates);
    }

    #[test]
    fn identity_is_private_persistent_and_invalid_files_are_not_replaced() {
        let dir = std::env::temp_dir().join(format!(
            "wayshare-rdp-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let path = dir.join("identity.pem");
        tls_acceptor(&path).unwrap();
        let first = std::fs::read(&path).unwrap();
        tls_acceptor(&path).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&path, "invalid").unwrap();
        assert!(tls_acceptor(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
