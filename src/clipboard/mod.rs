//! Clipboard sync with Apple's shared pasteboard.

pub mod mime;
pub mod pack;
pub mod proto;
#[cfg(target_os = "linux")]
pub mod wayland;

use std::sync::{Arc, Mutex};

use anyhow::Result;

pub use mime::ClipContent;

use crate::rfb::clipboard::Clipboard;

/// Server-side (Linux) clipboard. Contents are never logged; use `ClipContent::summary`.
pub trait ClipboardBackend: Send + Sync {
    /// Changes when another application takes the server selection; our own sets don't count.
    fn serial(&self) -> u64;
    /// Canonical MIME types currently offered, without reading any data.
    fn offered(&self) -> Vec<&'static str>;
    /// Reads the server selection. Blocks (bounded by a timeout); call off the async runtime.
    fn read(&self) -> Result<ClipContent>;
    /// Replaces the server selection with data from the viewer.
    fn set(&self, content: ClipContent) -> Result<()>;
    /// Takes the selection with types the viewer only promised. When a server application
    /// pastes, `take_data_request` turns true until the data arrives through `set`.
    fn set_promised(&self, types: Vec<&'static str>) -> Result<()>;
    /// True once per paste of promised data; the session then sends MiscStateChange 3.
    fn take_data_request(&self) -> bool;
}

/// In-memory backend for tests and platforms without a compositor.
#[derive(Default)]
pub struct MemoryBackend {
    state: Mutex<MemoryState>,
}

#[derive(Default)]
struct MemoryState {
    serial: u64,
    content: ClipContent,
    promised: Vec<&'static str>,
    request: bool,
}

impl MemoryBackend {
    /// Simulates a copy in a server application.
    pub fn copy(&self, content: ClipContent) {
        let mut s = self.state.lock().unwrap();
        s.serial += 1;
        s.content = content;
        s.promised.clear();
    }

    /// Simulates a server application pasting; returns what it got if available now.
    pub fn paste(&self) -> Option<ClipContent> {
        let mut s = self.state.lock().unwrap();
        if !s.promised.is_empty() {
            s.request = true;
            return None;
        }
        Some(s.content.clone())
    }
}

impl ClipboardBackend for MemoryBackend {
    fn serial(&self) -> u64 {
        self.state.lock().unwrap().serial
    }

    fn offered(&self) -> Vec<&'static str> {
        let s = self.state.lock().unwrap();
        s.content
            .0
            .iter()
            .filter_map(|(m, _)| mime::canonical(m))
            .collect()
    }

    fn read(&self) -> Result<ClipContent> {
        Ok(self.state.lock().unwrap().content.clone())
    }

    fn set(&self, content: ClipContent) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.content = content;
        s.promised.clear();
        Ok(())
    }

    fn set_promised(&self, types: Vec<&'static str>) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.content = ClipContent::default();
        s.promised = types;
        Ok(())
    }

    fn take_data_request(&self) -> bool {
        std::mem::take(&mut self.state.lock().unwrap().request)
    }
}

/// Adapts a backend to the rfb session's text-only `Clipboard` trait (msgs 3/6).
pub struct TextBridge {
    backend: Arc<dyn ClipboardBackend>,
    cache: Mutex<Option<(u64, String)>>,
}

impl TextBridge {
    pub fn new(backend: Arc<dyn ClipboardBackend>) -> Self {
        Self {
            backend,
            cache: Mutex::new(None),
        }
    }
}

impl Clipboard for TextBridge {
    fn set_from_client(&self, text: String) {
        if let Err(e) = self.backend.set(ClipContent::text(&text)) {
            tracing::warn!("clipboard set failed: {e:#}");
        }
    }

    fn server_text(&self) -> Option<(u64, String)> {
        let serial = self.backend.serial();
        let mut cache = self.cache.lock().unwrap();
        if cache.as_ref().map(|c| c.0) != Some(serial) {
            if !self.backend.offered().contains(&mime::TEXT) {
                return None;
            }
            let text = self.backend.read().ok()?.as_text()?;
            *cache = Some((serial, text));
        }
        cache.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::proto::*;
    use super::*;
    use crate::rfb::clipboard::ClipboardSync;

    #[test]
    fn text_bridge_with_rfb_sync() {
        let backend = Arc::new(MemoryBackend::default());
        let bridge = TextBridge::new(backend.clone());
        let mut sync = ClipboardSync::default();
        assert_eq!(sync.pending(&bridge), None);
        backend.copy(ClipContent::text("server copy"));
        assert_eq!(sync.pending(&bridge).as_deref(), Some("server copy"));
        assert_eq!(sync.pending(&bridge), None);
        sync.from_client(&bridge, "from viewer".into());
        assert_eq!(
            backend.read().unwrap().as_text().as_deref(),
            Some("from viewer")
        );
        assert_eq!(sync.pending(&bridge), None);
    }

    /// The whole Apple flow against the memory backend, as the rfb session would drive it.
    #[test]
    fn apple_flow() {
        let backend = MemoryBackend::default();
        // Server copy -> MiscStateChange 2 -> client msg 11 (session 7) -> server msg 31.
        backend.copy(ClipContent(vec![
            (mime::HTML.into(), b"<i>x</i>".to_vec()),
            (mime::TEXT.into(), b"x".to_vec()),
        ]));
        let reply = server_clipboard_data(7, &backend.read().unwrap());
        let ulen = u32::from_be_bytes(reply[8..12].try_into().unwrap());
        let ViewerPasteboard::Data(got) = decode_viewer_send(reply[2], ulen, &reply[16..]).unwrap()
        else {
            panic!()
        };
        assert_eq!(got.get(mime::HTML), Some(&b"<i>x</i>"[..]));

        // Viewer promises -> server app pastes -> MiscStateChange 3 -> client full msg 31.
        backend.set_promised(vec![mime::TEXT]).unwrap();
        assert_eq!(backend.paste(), None);
        assert!(backend.take_data_request());
        assert!(!backend.take_data_request());
        backend.set(ClipContent::text("late")).unwrap();
        assert_eq!(backend.paste().unwrap().as_text().as_deref(), Some("late"));
    }
}
