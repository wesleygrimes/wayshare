//! Socket backlog bounds capture work and detects clients that stop reading.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::task::AbortHandle;
use tracing::info;

const MAX_BACKLOG: usize = 64 * 1024;
const STALL: Duration = Duration::from_secs(5);

#[derive(Default)]
pub(super) struct Output {
    pub suppressed: Arc<AtomicBool>,
    pub in_flight: AtomicBool,
    pub refresh: AtomicBool,
    pub invalidate: AtomicBool,
    blocked: AtomicBool,
}

impl Output {
    pub fn can_capture(&self) -> bool {
        !self.suppressed.load(Ordering::Acquire)
            && !self.in_flight.load(Ordering::Acquire)
            && !self.blocked.load(Ordering::Acquire)
    }
}

#[derive(Default)]
pub(super) struct Peers(HashMap<IpAddr, Peer>);

pub(super) struct Peer {
    task: AbortHandle,
    socket: Weak<std::net::TcpStream>,
}

impl Peer {
    pub fn abort(self) {
        if let Some(socket) = self.socket.upgrade() {
            reset_on_close(&socket);
        }
        self.task.abort();
    }
}

pub(super) fn reset_on_close(socket: &std::net::TcpStream) {
    // FIN waits behind queued graphics and cannot reach a zero-window client.
    // Reset stalled and replaced sessions so the client sees RST immediately.
    if let Err(error) = socket2::SockRef::from(socket).set_linger(Some(Duration::ZERO)) {
        tracing::warn!(%error, "RDP abortive close setup failed");
    }
}

impl Peers {
    pub fn reap(&mut self) {
        self.0.retain(|_, task| !task.task.is_finished());
    }

    pub fn remove(&mut self, ip: IpAddr) -> Option<Peer> {
        self.0.remove(&ip)
    }

    pub fn insert(&mut self, ip: IpAddr, task: AbortHandle, socket: Weak<std::net::TcpStream>) {
        self.0.insert(ip, Peer { task, socket });
    }
}

pub(super) fn socket_monitor(stream: &tokio::net::TcpStream) -> Result<Arc<std::net::TcpStream>> {
    // A duplicated descriptor keeps ioctl tied to this socket even during cancellation.
    let socket = socket2::SockRef::from(stream);
    socket.set_send_buffer_size(MAX_BACKLOG)?;
    Ok(Arc::new(socket.try_clone()?.into()))
}

struct Progress {
    previous: usize,
    since: Instant,
}

impl Progress {
    fn new(now: Instant) -> Self {
        Self {
            previous: 0,
            since: now,
        }
    }

    fn observe(&mut self, outstanding: usize, now: Instant) -> Option<Duration> {
        if outstanding == 0 || self.previous == 0 || outstanding < self.previous {
            self.since = now;
        }
        self.previous = outstanding;
        let elapsed = now.duration_since(self.since);
        (outstanding > 0 && elapsed >= STALL).then_some(elapsed)
    }
}

pub(super) async fn watch(socket: Arc<std::net::TcpStream>, flow: &Output) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let peer = socket.peer_addr()?;
        let mut progress = Progress::new(Instant::now());
        let mut timer = tokio::time::interval(Duration::from_millis(20));
        loop {
            timer.tick().await;
            let mut outstanding: libc::c_int = 0;
            // SIOCOUTQ reports bytes written but not acknowledged. The owned socket
            // outlives ioctl, which writes one initialized c_int through this pointer.
            let result =
                unsafe { libc::ioctl(socket.as_raw_fd(), libc::TIOCOUTQ, &mut outstanding) };
            if result < 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            let outstanding = usize::try_from(outstanding)?;
            flow.blocked
                .store(outstanding >= MAX_BACKLOG, Ordering::Release);
            if let Some(stall_time) = progress.observe(outstanding, Instant::now()) {
                info!(%peer, outstanding_bytes = outstanding, stall_time = ?stall_time, "RDP client stalled; closing connection");
                reset_on_close(&socket);
                return Ok(());
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (socket, flow);
        std::future::pending().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_coalesces_until_previous_frame_and_backlog_clear() {
        let flow = Output::default();
        assert!(flow.can_capture());
        flow.in_flight.store(true, Ordering::Release);
        assert!(!flow.can_capture());
        flow.blocked.store(true, Ordering::Release);
        flow.in_flight.store(false, Ordering::Release);
        assert!(!flow.can_capture());
        flow.blocked.store(false, Ordering::Release);
        assert!(flow.can_capture());
        flow.suppressed.store(true, Ordering::Release);
        flow.refresh.store(true, Ordering::Release);
        assert!(!flow.can_capture());
        flow.suppressed.store(false, Ordering::Release);
        assert!(flow.can_capture());
        assert!(flow.refresh.load(Ordering::Acquire));
    }

    #[test]
    fn watchdog_requires_five_seconds_without_progress() {
        let now = Instant::now();
        let mut p = Progress::new(now);
        assert_eq!(p.observe(100, now), None);
        assert_eq!(p.observe(100, now + STALL - Duration::from_millis(1)), None);
        assert_eq!(p.observe(100, now + STALL), Some(STALL));
        assert_eq!(p.observe(90, now + STALL), None);
        assert_eq!(p.observe(90, now + STALL * 2), Some(STALL));
        assert_eq!(p.observe(0, now + STALL * 3), None);
        assert_eq!(p.observe(100, now + STALL * 4), None);
    }

    #[tokio::test]
    async fn same_peer_replaces_only_its_old_session() {
        let a = tokio::spawn(std::future::pending::<()>());
        let b = tokio::spawn(std::future::pending::<()>());
        let mut peers = Peers::default();
        let ip = "127.0.0.1".parse().unwrap();
        let other = "127.0.0.2".parse().unwrap();
        peers.insert(ip, a.abort_handle(), Weak::new());
        peers.insert(other, b.abort_handle(), Weak::new());
        peers.remove(ip).unwrap().abort();
        assert!(a.await.unwrap_err().is_cancelled());
        assert!(!b.is_finished());
        assert!(peers.remove(ip).is_none());
        b.abort();
    }
}
