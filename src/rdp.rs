//! Authenticated TLS RDP adapter over the shared capture and input factories.

mod input;
#[cfg(target_os = "linux")]
mod pam;
#[cfg(not(target_os = "linux"))]
mod pam {
    pub fn service() -> &'static str {
        "wayshare"
    }
    pub fn current_user() -> anyhow::Result<String> {
        anyhow::bail!("RDP PAM login requires Linux")
    }
    pub fn authenticate(_: &str, _: &str, _: &str) -> anyhow::Result<bool> {
        anyhow::bail!("RDP PAM login requires Linux")
    }
}

use std::io::{ErrorKind, Write};
use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroUsize};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::rfb::{DisplayControl, SessionId};
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
use tracing::{debug, info, warn};
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
    allowed_user: Option<&str>,
) -> Result<()> {
    // The protocol library uses Rc internally, so poll connections on this local executor.
    tokio::task::LocalSet::new()
        .run_until(serve_local(
            listen,
            identity,
            frames,
            inputs,
            display,
            allowed_user,
        ))
        .await
}

async fn serve_local(
    listen: SocketAddr,
    identity: &Path,
    frames: FrameSourceFactory,
    inputs: Option<InputFactory>,
    display: Option<Arc<dyn DisplayControl>>,
    allowed_user: Option<&str>,
) -> Result<()> {
    let pam_login = Arc::new(PamLogin::new(allowed_user)?);
    let (tls, fingerprint) = tls_acceptor(identity)?;
    let probe = frames.clone();
    let (width, height) =
        tokio::task::spawn_blocking(move || Ok::<_, anyhow::Error>(probe()?.size())).await??;
    ensure!(width > 0 && height > 0, "empty RDP desktop");
    let listener = bind_listener(listen, Some(&pam_login)).await?;
    let bound = listener.local_addr()?;
    let mut sessions = tokio::task::JoinSet::new();
    info!(listen = %bound, user = %pam_login.user, service = pam_login.service,
        "RDP listener ready with TLS and PAM");
    info!(sha256 = %fingerprint, "RDP TLS certificate fingerprint (SHA-256, colon-separated hex)");
    // RFB allocates low IDs; reserve the upper half for RDP display ownership.
    let mut session: SessionId = 1 << 63;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                // Bound unauthenticated handshakes and slow PAM workers as well as sessions.
                if sessions.len() >= 8 { drop(stream); continue; }
                let authenticated = Arc::new(AtomicBool::new(false));
                let login = Login { pam: pam_login.clone(), authenticated: authenticated.clone() };
                let size = DesktopSize { width, height };
                let extent = Arc::new(Mutex::new(size));
                let target = Arc::new(Mutex::new(size));
                let mut server = RdpServer::builder()
                    .with_addr(bound)
                    .with_tls(tls.clone())
                    .with_input_handler(input::Input {
                        factory: inputs.clone(), authenticated: authenticated.clone(),
                        extent: extent.clone(), width, height, sink: None, buttons: 0, x: 0, y: 0, surrogate: [None; 2],
                    })
                    .with_display_handler(Display {
                        frames: frames.clone(), size: DesktopSize { width, height },
                        authenticated: authenticated.clone(),
                        extent, target, control: display.clone(), session, started: false,
                    })
                    .with_honor_client_desktop_size(true)
                    .with_credential_validator(Some(Arc::new(login)))
                    .build();
                session += 1;
                sessions.spawn_local(async move {
                    // An idle pre-login client cannot occupy a slot indefinitely.
                    let deadline = async {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                        if authenticated.load(Ordering::Acquire) { std::future::pending::<()>().await; }
                    };
                    tokio::select! {
                        result = server.run_connection(stream) => {
                            if result.is_err() { info!(%peer, "RDP connection closed"); }
                        },
                        () = deadline => info!(%peer, "RDP login timed out"),
                    }
                });
            },
            Some(result) = sessions.join_next(), if !sessions.is_empty() => {
                if let Err(error) = result { warn!(%error, "RDP session task failed"); }
            },
        }
    }
}

struct PamLogin {
    user: String,
    service: &'static str,
}

impl PamLogin {
    fn new(allowed_user: Option<&str>) -> Result<Self> {
        ensure!(cfg!(target_os = "linux"), "RDP PAM login requires Linux");
        let user = match allowed_user {
            Some(user) => user.to_owned(),
            None => pam::current_user()?,
        };
        ensure!(
            !user.is_empty() && !user.contains(['\0', '\\', '@']),
            "invalid RDP account"
        );
        Ok(Self {
            user,
            service: pam::service(),
        })
    }
}

async fn bind_listener(
    listen: SocketAddr,
    pam_login: Option<&PamLogin>,
) -> Result<tokio::net::TcpListener> {
    // Require the same PAM policy that each connection installs as its validator.
    ensure!(
        listen.ip().is_loopback() || pam_login.is_some(),
        "RDP non-loopback listener {listen} requires active PAM authentication"
    );
    tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("binding RDP listener {listen}"))
}

struct Login {
    pam: Arc<PamLogin>,
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
        if credentials.username == self.pam.user
            && credentials.domain.as_deref().is_none_or(str::is_empty)
            && !credentials.password.is_empty()
            && credentials.password.len() <= 4096
        {
            let user = self.pam.user.clone();
            let password = Zeroizing::new(credentials.password.clone());
            let service = self.pam.service;
            match tokio::task::spawn_blocking(move || pam::authenticate(service, &user, &password))
                .await
            {
                Ok(Ok(valid)) => accepted = valid,
                _ => warn!("RDP PAM backend failed"),
            }
        }
        if accepted {
            self.authenticated.store(true, Ordering::Release);
            info!("RDP login accepted");
            Ok(CredentialDecision::Accept)
        } else {
            // Sleep only this connection, including failures rejected before reaching PAM.
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
    target: Arc<Mutex<DesktopSize>>,
    control: Option<Arc<dyn DisplayControl>>,
    session: SessionId,
    started: bool,
}

fn validate_size(width: u32, height: u32) -> Result<DesktopSize> {
    // MS-RDPEDISP 2.2.2.2.1. Portrait is valid; only the width must be even.
    ensure!(
        (200..=8192).contains(&width) && (200..=8192).contains(&height),
        "RDP size out of range"
    );
    ensure!(width.is_multiple_of(2), "RDP width must be even");
    Ok(DesktopSize {
        width: width as u16,
        height: height as u16,
    })
}

fn layout_size(layout: &DisplayControlMonitorLayout) -> Result<DesktopSize> {
    ensure!(layout.monitors().len() == 1, "RDP supports one monitor");
    let monitor = &layout.monitors()[0];
    ensure!(
        monitor.is_primary() && monitor.position() == Some((0, 0)),
        "RDP requires a primary monitor at the origin"
    );
    let (width, height) = monitor.dimensions();
    let size = validate_size(width, height)?;
    // Match the one-monitor area advertised by DisplayControlServer 0.8.0.
    ensure!(
        width * height <= 3840 * 2400,
        "RDP layout exceeds advertised monitor area"
    );
    Ok(size)
}

impl Display {
    fn resize_output(&self, size: DesktopSize) {
        if let Some(control) = &self.control {
            match control.configure_size(self.session, size.width.into(), size.height.into()) {
                Ok(Some(applied)) => debug!(?size, ?applied, "RDP output resized"),
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
        debug!(?client_size, "RDP client desktop size");
        if !self.authenticated.load(Ordering::Acquire) {
            return self.size;
        }
        if !self.started {
            self.started = true;
            // Initial Core Data permits odd widths. Normalize before the first frame;
            // a Resize update makes the server renegotiate if this changes the size.
            let width = u32::from(client_size.width) & !1;
            if let Ok(size) = validate_size(width, client_size.height.into()) {
                self.size = client_size;
                *self.target.lock().unwrap() = size;
                let ready = self.control.as_ref().is_none_or(|control| {
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
            self.size = client_size;
        }
        *self.extent.lock().unwrap() = self.size;
        self.size
    }

    fn request_layout(&mut self, layout: DisplayControlMonitorLayout) {
        debug!(?layout, "RDP Display Control layout request");
        if !self.started || !self.authenticated.load(Ordering::Acquire) {
            return;
        }
        match layout_size(&layout) {
            Ok(size) => {
                if *self.target.lock().unwrap() != size {
                    self.resize_output(size);
                    *self.target.lock().unwrap() = size;
                }
            }
            Err(error) => warn!(%error, "RDP layout rejected"),
        }
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
        let thread = std::thread::Builder::new()
            .name("rdp-capture".into())
            .spawn(move || {
                let result = capture(frames, size, &target, &sender, &worker_stop);
                if let Err(e) = result {
                    let _ = sender.blocking_send(Err(e));
                }
            })?;
        Ok(Box::new(Updates {
            receiver,
            stop,
            thread: Some(thread),
        }))
    }
}

fn capture(
    frames: FrameSourceFactory,
    size: DesktopSize,
    target: &Mutex<DesktopSize>,
    sender: &mpsc::Sender<Result<DisplayUpdate>>,
    stop: &AtomicBool,
) -> Result<()> {
    // Resize updates end this worker. Reactivation opens capture at the new mode
    // and builds a fresh encoder before any bitmap of the new dimensions is sent.
    let requested = *target.lock().unwrap();
    if requested != size {
        let _ = sender.blocking_send(Ok(DisplayUpdate::Resize(requested)));
        return Ok(());
    }
    let mut source = frames()?;
    let mut first = true;
    while !stop.load(Ordering::Relaxed) {
        let requested = *target.lock().unwrap();
        if requested != size {
            let _ = sender.blocking_send(Ok(DisplayUpdate::Resize(requested)));
            return Ok(());
        }
        let changed = source.advance();
        if first || changed {
            let frame = source.frame();
            let fitted;
            let frame = if (frame.width, frame.height) == (size.width, size.height) {
                frame
            } else {
                fitted = fit_frame(frame, size)?;
                &fitted
            };
            let update = bitmap(frame)?;
            if sender
                .blocking_send(Ok(DisplayUpdate::Bitmap(update)))
                .is_err()
            {
                break;
            }
            first = false;
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
    receiver: mpsc::Receiver<Result<DisplayUpdate>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for Updates {
    async fn next_update(&mut self) -> Result<Option<DisplayUpdate>> {
        // recv is cancellation safe; capture never runs inside the selected future.
        self.receiver.recv().await.transpose()
    }
}

impl Drop for Updates {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
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

    fn display(size: DesktopSize) -> Display {
        Display {
            frames: Arc::new(|| Ok(Box::new(TestPattern::new(64, 64)))),
            size,
            extent: Arc::new(Mutex::new(size)),
            target: Arc::new(Mutex::new(size)),
            authenticated: Arc::new(AtomicBool::new(true)),
            control: None,
            session: 1 << 63,
            started: false,
        }
    }

    #[test]
    fn resize_validation_accepts_portrait_and_rejects_invalid_sizes_and_layouts() {
        for (width, height) in [(200, 200), (8192, 8192), (836, 1580), (1580, 837)] {
            assert!(validate_size(width, height).is_ok());
        }
        for (width, height) in [
            (0, 0),
            (199, 200),
            (200, 199),
            (8194, 200),
            (200, 8193),
            (837, 1580),
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
        assert!(
            layout_size(&DisplayControlMonitorLayout::new(&[primary, secondary]).unwrap()).is_err()
        );
        assert!(
            layout_size(
                &DisplayControlMonitorLayout::new_single_primary_monitor(8192, 8192, None, None)
                    .unwrap()
            )
            .is_err()
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
            matches!(updates.next_update().await.unwrap(), Some(DisplayUpdate::Bitmap(b)) if b.width.get() == 836 && b.height.get() == 1580)
        );
        display.request_layout(
            DisplayControlMonitorLayout::new_single_primary_monitor(1580, 836, None, None).unwrap(),
        );
        assert_eq!(*display.extent.lock().unwrap(), portrait);
        loop {
            if let Some(DisplayUpdate::Resize(size)) = updates.next_update().await.unwrap() {
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
            matches!(updates.next_update().await.unwrap(), Some(DisplayUpdate::Bitmap(b)) if b.width.get() == 1580 && b.height.get() == 836)
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
            matches!(updates.next_update().await.unwrap(), Some(DisplayUpdate::Bitmap(b)) if b.width.get() == 1580 && b.height.get() == 836)
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
                matches!(updates.next_update().await.unwrap(), Some(DisplayUpdate::Bitmap(b)) if b.width.get() == 200 && b.height.get() == 400 && b.data[0] == color)
            );
        }
    }

    #[tokio::test]
    async fn initial_odd_width_is_renegotiated_before_capture() {
        let mut display = display(DesktopSize {
            width: 3456,
            height: 2160,
        });
        display.frames = Arc::new(|| panic!("capture before normalized resize"));
        display
            .request_initial_size(DesktopSize {
                width: 837,
                height: 1581,
            })
            .await;
        let mut updates = display.updates().await.unwrap();
        assert!(matches!(
            updates.next_update().await.unwrap(),
            Some(DisplayUpdate::Resize(DesktopSize {
                width: 836,
                height: 1581
            }))
        ));
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
    async fn network_bind_requires_pam_before_opening_a_socket() {
        for address in ["0.0.0.0:0", "[::]:0", "192.0.2.1:0", "[2001:db8::1]:0"] {
            let error = bind_listener(address.parse().unwrap(), None)
                .await
                .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("requires active PAM authentication")
            );
        }
        let local = bind_listener("127.0.0.1:0".parse().unwrap(), None)
            .await
            .unwrap();
        assert!(local.local_addr().unwrap().ip().is_loopback());
        #[cfg(target_os = "linux")]
        {
            let pam = PamLogin::new(None).unwrap();
            let network = bind_listener("0.0.0.0:0".parse().unwrap(), Some(&pam))
                .await
                .unwrap();
            assert!(network.local_addr().unwrap().ip().is_unspecified());
        }
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
            pam: Arc::new(PamLogin {
                user: "allowed".into(),
                service: "wayshare",
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
            target: Arc::new(Mutex::new(DesktopSize {
                width: 64,
                height: 64,
            })),
            control: None,
            session: 1 << 63,
            started: false,
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
            target: Arc::new(Mutex::new(DesktopSize {
                width: 64,
                height: 64,
            })),
            control: None,
            session: 1 << 63,
            started: false,
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
