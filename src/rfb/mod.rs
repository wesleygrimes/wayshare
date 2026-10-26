pub mod clipboard;
pub mod crypt;
pub mod dh;
pub mod encode;
pub mod handshake;
pub mod media;
pub mod messages;
pub mod pixel;
pub mod pseudo;
pub mod session;
pub mod srp;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpListener;
use tracing::{Instrument, info, info_span, warn};

pub use crate::input::InputFactory;
pub use crate::source::{CursorSourceFactory, FrameSourceFactory};
pub use handshake::AuthConfig;

pub struct DeviceInfo {
    pub identifier: String,
    pub color: String,
    pub enclosure_color: String,
}

pub struct Config {
    pub name: String,
    pub width: u16,
    pub height: u16,
    pub auth: AuthConfig,
    pub clipboard: Option<Arc<dyn clipboard::Clipboard>>,
    pub hp: Option<Arc<dyn media::HpBackend>>,
    /// Apple in-band stream encryption (encoding 1103). Unverified against the real client.
    pub stream_encryption: bool,
    pub device_info: Option<DeviceInfo>,
    /// Creates the framebuffer source for each session. Defaults to a test pattern.
    pub frame_source: FrameSourceFactory,
    pub cursor_source: CursorSourceFactory,
    pub display: Option<Arc<dyn DisplayControl>>,
    /// Apple shared pasteboard. Also set `clipboard` to a `TextBridge` over it for msgs 3/6.
    pub clipboard_backend: Option<Arc<dyn crate::clipboard::ClipboardBackend>>,
    /// Injects viewer input. `None` logs it only.
    pub input: Option<InputFactory>,
}

/// Identifies one viewer connection; backends use it to own and release shared resources.
pub type SessionId = u64;

/// Applied result of a client display request (msg 29).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AppliedDisplay {
    pub pixels: (u16, u16),
    pub points: (u16, u16),
    pub scale: f64,
}

/// Manages display changes owned by authenticated sessions.
pub trait DisplayControl: Send + Sync {
    /// Called after authentication with session teardown already armed.
    fn session_started(&self, _session: SessionId) -> Result<()> {
        Ok(())
    }
    /// Returns `None` when another session already owns the output's mode.
    fn configure(
        &self,
        session: SessionId,
        request: &crate::display::apple::DisplayRequest,
    ) -> Result<Option<AppliedDisplay>>;
    /// Fits a pixel size at the output's current scale. Shares ownership with RFB.
    fn configure_size(
        &self,
        _session: SessionId,
        _width: u32,
        _height: u32,
    ) -> Result<Option<AppliedDisplay>> {
        Ok(None)
    }
    /// Releases this session's display changes, even if session startup failed.
    fn restore(&self, session: SessionId) -> Result<()>;
}

impl Config {
    pub fn new(name: impl Into<String>, width: u16, height: u16, auth: AuthConfig) -> Self {
        Self {
            name: name.into(),
            width,
            height,
            auth,
            clipboard: None,
            hp: None,
            stream_encryption: false,
            device_info: None,
            display: None,
            input: None,
            clipboard_backend: None,
            frame_source: Arc::new(move || {
                Ok(Box::new(crate::source::TestPattern::new(width, height))
                    as Box<dyn crate::source::FrameSource>)
            }),
            cursor_source: Arc::new(|| Box::new(crate::source::TestCursor::default())),
        }
    }

    /// Client message types for the ServerInit bitmap: only ones we parse and act on.
    /// HP (28/29) is left out when the backend is busy with another viewer.
    pub fn server_cmds(&self, peer: std::net::IpAddr) -> Vec<u8> {
        let mut cmds = handshake::BASE_CLIENT_MESSAGES.to_vec();
        if self.stream_encryption {
            cmds.push(handshake::MSG_SET_ENCRYPTION);
        }
        if self.clipboard_backend.is_some() {
            cmds.extend(crate::clipboard::proto::CLIENT_MSGS);
        }
        if self.hp.as_ref().is_some_and(|hp| hp.available(peer)) {
            cmds.push(handshake::MSG_MEDIA_STREAM_CONFIG);
            cmds.push(handshake::MSG_SET_DISPLAY_CONFIG);
        }
        cmds.sort_unstable();
        cmds.dedup();
        cmds
    }
}

/// Accepts viewers until `shutdown` resolves, then ends every session (running each
/// session's teardown: input release, HP stop, output restore) before returning.
pub async fn serve(
    listen: SocketAddr,
    cfg: Arc<Config>,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let listener = TcpListener::bind(listen).await?;
    info!(%listen, "listening");
    serve_on(listener, cfg, shutdown).await
}

pub async fn serve_on(
    listener: TcpListener,
    cfg: Arc<Config>,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let mut sessions = tokio::task::JoinSet::new();
    let mut next_id: SessionId = 1;
    tokio::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            a = listener.accept() => a,
            _ = &mut shutdown => break,
            Some(_) = sessions.join_next(), if !sessions.is_empty() => continue,
        };
        let (stream, peer) = match accepted {
            Ok(c) => c,
            Err(e) => {
                warn!("accept failed: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        let id = next_id;
        next_id += 1;
        let cfg = cfg.clone();
        // A panic ends only this task; Session's Drop still restores shared state.
        sessions.spawn(
            async move {
                info!("connection accepted");
                match session::run(stream, cfg, id).await {
                    Ok(()) => info!("connection closed"),
                    Err(e) => warn!("connection ended: {e:#}"),
                }
            }
            .instrument(info_span!("conn", %peer, session = id)),
        );
    }
    info!(sessions = sessions.len(), "shutting down");
    sessions.abort_all();
    while sessions.join_next().await.is_some() {}
    info!("shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoHp;
    impl media::HpBackend for NoHp {
        fn ports(&self) -> media::MediaPorts {
            media::MediaPorts::default()
        }
        fn configure(
            &self,
            _: SessionId,
            _: SocketAddr,
            _: media::MediaStreamConfig,
        ) -> Result<Option<media::MediaAnswer>> {
            anyhow::bail!("stub")
        }
    }

    #[test]
    fn bitmap_only_advertises_configured_features() {
        let mut cfg = Config::new("t", 10, 10, AuthConfig::default());
        assert_eq!(
            cfg.server_cmds(std::net::Ipv4Addr::LOCALHOST.into()),
            [0, 2, 3, 4, 5, 6, 9, 33]
        );
        cfg.hp = Some(Arc::new(NoHp));
        cfg.stream_encryption = true;
        assert_eq!(
            cfg.server_cmds(std::net::Ipv4Addr::LOCALHOST.into()),
            [0, 2, 3, 4, 5, 6, 9, 18, 28, 29, 33]
        );
        cfg.clipboard_backend = Some(Arc::new(crate::clipboard::MemoryBackend::default()));
        assert_eq!(
            cfg.server_cmds(std::net::Ipv4Addr::LOCALHOST.into()),
            [0, 2, 3, 4, 5, 6, 9, 11, 18, 21, 28, 29, 31, 33]
        );
    }
}
