//! View-only RDP adapter over the shared framebuffer source.

use std::io::{ErrorKind, Write};
use std::net::SocketAddr;
use std::num::{NonZeroU16, NonZeroUsize};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use ironrdp_server::tokio_rustls::{TlsAcceptor, rustls};
use ironrdp_server::{
    BitmapUpdate, DesktopSize, DisplayUpdate, PixelFormat, RdpServer, RdpServerDisplay,
    RdpServerDisplayUpdates,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::mpsc;
use tracing::info;

use crate::source::{Frame, FrameSourceFactory};

/// Runs until an error or cancellation. Only local viewers are allowed without auth.
pub async fn serve(listen: SocketAddr, identity: &Path, frames: FrameSourceFactory) -> Result<()> {
    ensure!(listen.ip().is_loopback(), "RDP requires a loopback address");
    let tls = tls_acceptor(identity)?;
    let probe = frames.clone();
    let (width, height) =
        tokio::task::spawn_blocking(move || Ok::<_, anyhow::Error>(probe()?.size())).await??;
    ensure!(width > 0 && height > 0, "empty RDP desktop");
    let mut server = RdpServer::builder()
        .with_addr(listen)
        .with_tls(tls)
        .with_no_input()
        .with_display_handler(Display {
            frames,
            size: DesktopSize { width, height },
        })
        .build();
    info!(%listen, width, height, "starting view-only RDP with TLS; local viewers are not authenticated");
    server.run().await
}

fn tls_acceptor(path: &Path) -> Result<TlsAcceptor> {
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
    let key = PrivateKeyDer::from_pem_slice(&pem).context("reading RDP private key")?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

struct Display {
    frames: FrameSourceFactory,
    size: DesktopSize,
}

#[async_trait::async_trait]
impl RdpServerDisplay for Display {
    async fn size(&mut self) -> DesktopSize {
        self.size
    }

    async fn updates(&mut self) -> Result<Box<dyn RdpServerDisplayUpdates>> {
        let (sender, receiver) = mpsc::channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let frames = self.frames.clone();
        let size = self.size;
        let thread = std::thread::Builder::new()
            .name("rdp-capture".into())
            .spawn(move || {
                let result = capture(frames, size, &sender, &worker_stop);
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
    sender: &mpsc::Sender<Result<DisplayUpdate>>,
    stop: &AtomicBool,
) -> Result<()> {
    let mut source = frames()?;
    let mut first = true;
    while !stop.load(Ordering::Relaxed) {
        let changed = source.advance();
        if first || changed {
            ensure!(
                source.size() == (size.width, size.height),
                "RDP desktop size changed; reconnect after restarting the server"
            );
            let update = bitmap(source.frame())?;
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
            size: DesktopSize {
                width: 64,
                height: 64,
            },
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
