//! ext-data-control-v1 backend. Runs its own Wayland connection on a thread.

use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use tracing::{debug, warn};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop, event_created_child,
};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self as device, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self as offer, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self as source, ExtDataControlSourceV1},
};

use super::mime::{self, ClipContent};
use super::{ClipboardBackend, proto};

/// Offered alongside our own data so we can recognise our selection when it comes back.
const MARKER: &str = "application/x-wayshare-selection";
const READ_TIMEOUT: Duration = Duration::from_secs(2);
const PROMISE_TIMEOUT: Duration = Duration::from_secs(5);

type OfferMimes = Mutex<Vec<String>>;

enum Cmd {
    Read(SyncSender<Result<ClipContent>>),
    Set(ClipContent, Vec<&'static str>),
}

struct Shared {
    serial: AtomicU64,
    offered: Mutex<Vec<&'static str>>,
    data_request: AtomicBool,
}

pub struct WaylandClipboard {
    shared: Arc<Shared>,
    cmds: Mutex<Sender<Cmd>>,
    wake: OwnedFd,
}

impl WaylandClipboard {
    /// Connects to `$WAYLAND_DISPLAY` and starts watching the selection.
    pub fn spawn() -> Result<Self> {
        let conn = crate::display::connect_wayland().context("connect to Wayland display")?;
        let (globals, queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=1, ()).context("wl_seat")?;
        let mgr: ExtDataControlManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .context("ext_data_control_manager_v1 not offered")?;
        let dev = mgr.get_data_device(&seat, &qh, ());
        let shared = Arc::new(Shared {
            serial: AtomicU64::new(0),
            offered: Mutex::new(Vec::new()),
            data_request: AtomicBool::new(false),
        });
        let (wake_r, wake_w) = pipe()?;
        let (tx, rx) = channel();
        let mut st = State {
            shared: shared.clone(),
            selection: None,
            own: None,
            pending: Vec::new(),
            dead: false,
        };
        let mut queue = queue;
        queue.roundtrip(&mut st).context("data-control setup")?;
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                if let Err(e) = run(conn, queue, st, mgr, dev, rx, wake_r) {
                    warn!("clipboard thread stopped: {e:#}");
                }
            })?;
        Ok(Self {
            shared,
            cmds: Mutex::new(tx),
            wake: wake_w,
        })
    }

    fn send(&self, cmd: Cmd) -> Result<()> {
        self.cmds
            .lock()
            .unwrap()
            .send(cmd)
            .map_err(|_| anyhow!("clipboard thread gone"))?;
        let _ = std::fs::File::from(self.wake.try_clone()?).write(&[1]);
        Ok(())
    }
}

impl ClipboardBackend for WaylandClipboard {
    fn serial(&self) -> u64 {
        self.shared.serial.load(Ordering::SeqCst)
    }

    fn offered(&self) -> Vec<&'static str> {
        self.shared.offered.lock().unwrap().clone()
    }

    fn read(&self) -> Result<ClipContent> {
        let (tx, rx) = sync_channel(1);
        self.send(Cmd::Read(tx))?;
        rx.recv_timeout(READ_TIMEOUT * 4)
            .context("clipboard read timed out")?
    }

    fn set(&self, content: ClipContent) -> Result<()> {
        self.send(Cmd::Set(content, Vec::new()))
    }

    fn set_promised(&self, types: Vec<&'static str>) -> Result<()> {
        self.send(Cmd::Set(ClipContent::default(), types))
    }

    fn take_data_request(&self) -> bool {
        self.shared.data_request.swap(false, Ordering::SeqCst)
    }
}

struct Own {
    source: ExtDataControlSourceV1,
    content: ClipContent,
    promised: Vec<&'static str>,
}

struct State {
    shared: Arc<Shared>,
    selection: Option<ExtDataControlOfferV1>,
    own: Option<Own>,
    /// Promised-data requests from server apps: (canonical mime, fd, when asked).
    pending: Vec<(&'static str, OwnedFd, Instant)>,
    dead: bool,
}

fn run(
    conn: Connection,
    mut queue: EventQueue<State>,
    mut st: State,
    mgr: ExtDataControlManagerV1,
    dev: ExtDataControlDeviceV1,
    rx: Receiver<Cmd>,
    wake: OwnedFd,
) -> Result<()> {
    let qh = queue.handle();
    let mut wake = std::fs::File::from(wake);
    set_nonblocking(&wake)?;
    while !st.dead {
        queue.dispatch_pending(&mut st)?;
        conn.flush()?;
        if let Some(guard) = conn.prepare_read() {
            let mut fds = [
                libc::pollfd {
                    fd: guard.connection_fd().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: wake.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            // SAFETY: `fds` is a live array of two pollfds and the count is 2.
            unsafe { libc::poll(fds.as_mut_ptr(), 2, 500) };
            if fds[0].revents != 0 {
                match guard.read() {
                    Ok(_) => {}
                    Err(wayland_client::backend::WaylandError::Io(e))
                        if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e.into()),
                }
            }
            let _ = wake.read(&mut [0u8; 64]);
        }
        queue.dispatch_pending(&mut st)?;
        while let Ok(cmd) = rx.try_recv() {
            match cmd {
                Cmd::Read(reply) => {
                    let r = st.read(&conn);
                    let _ = reply.send(r);
                }
                Cmd::Set(content, promised) => {
                    st.take_selection(&mgr, &dev, &qh, content, promised)
                }
            }
        }
        st.expire_pending();
    }
    bail!("data-control device finished")
}

impl State {
    fn read(&mut self, conn: &Connection) -> Result<ClipContent> {
        if let Some(own) = &self.own {
            return Ok(own.content.clone());
        }
        let Some(offer) = &self.selection else {
            return Ok(ClipContent::default());
        };
        let names = offer.data::<OfferMimes>().unwrap().lock().unwrap().clone();
        let mut out = ClipContent::default();
        for canonical in [mime::RTF, mime::HTML, mime::PNG, mime::JPEG, mime::TEXT] {
            let Some(name) = mime::offered_names(canonical)
                .into_iter()
                .find(|n| names.iter().any(|o| o == n))
            else {
                continue;
            };
            let (r, w) = pipe()?;
            offer.receive(name.to_string(), w.as_fd());
            drop(w);
            conn.flush()?;
            match read_fd(r, proto::MAX_UNCOMPRESSED) {
                Ok(data) => out.0.push((canonical.to_string(), data)),
                Err(e) => warn!(mime = canonical, "clipboard read failed: {e:#}"),
            }
        }
        debug!(summary = ?out.summary(), "read server selection");
        Ok(out)
    }

    fn take_selection(
        &mut self,
        mgr: &ExtDataControlManagerV1,
        dev: &ExtDataControlDeviceV1,
        qh: &QueueHandle<State>,
        content: ClipContent,
        promised: Vec<&'static str>,
    ) {
        let types: Vec<&'static str> = if promised.is_empty() {
            content
                .0
                .iter()
                .filter_map(|(m, _)| mime::canonical(m))
                .collect()
        } else {
            promised.clone()
        };
        let src = mgr.create_data_source(qh, ());
        for t in &types {
            for name in mime::offered_names(t) {
                src.offer(name.to_string());
            }
        }
        src.offer(MARKER.to_string());
        dev.set_selection(Some(&src));
        if let Some(old) = self.own.replace(Own {
            source: src,
            content,
            promised,
        }) {
            old.source.destroy();
        }
        self.fulfil_pending();
        debug!(?types, "took server selection");
    }

    fn fulfil_pending(&mut self) {
        let Some(own) = &self.own else { return };
        for (m, fd, _) in std::mem::take(&mut self.pending) {
            match own.content.get(m) {
                Some(d) => write_async(fd, d.to_vec()),
                None if own.promised.contains(&m) => self.pending.push((m, fd, Instant::now())),
                None => {}
            }
        }
    }

    fn expire_pending(&mut self) {
        self.pending
            .retain(|(_, _, t)| t.elapsed() < PROMISE_TIMEOUT);
    }
}

fn pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` has room for the two descriptors pipe2 writes.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error()).context("pipe2");
    }
    // SAFETY: pipe2 succeeded, so both descriptors are new and owned by nobody else.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

fn set_nonblocking(f: &impl AsRawFd) -> Result<()> {
    let fd = f.as_raw_fd();
    // SAFETY: F_GETFL takes no pointer argument, and `fd` is borrowed from a live handle.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: F_SETFL takes an int, and `fd` is borrowed from a live handle.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error()).context("fcntl");
    }
    Ok(())
}

fn read_fd(fd: OwnedFd, cap: usize) -> Result<Vec<u8>> {
    let mut f = std::fs::File::from(fd);
    set_nonblocking(&f)?;
    let deadline = Instant::now() + READ_TIMEOUT;
    let mut out = Vec::new();
    let mut buf = [0u8; 65536];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            bail!("timed out after {} bytes", out.len());
        }
        let mut p = libc::pollfd {
            fd: f.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `p` is a valid pollfd for the call, the count is 1, and `f` keeps the fd
        // open.
        unsafe { libc::poll(&mut p, 1, left.as_millis() as i32) };
        match f.read(&mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > cap {
                    bail!("selection larger than {cap} bytes");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }
}

fn write_async(fd: OwnedFd, data: Vec<u8>) {
    std::thread::spawn(move || {
        if let Err(e) = std::fs::File::from(fd).write_all(&data) {
            debug!("clipboard paste write ended early: {e}");
        }
    });
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ExtDataControlManagerV1);

impl Dispatch<ExtDataControlDeviceV1, ()> for State {
    fn event(
        st: &mut Self,
        _: &ExtDataControlDeviceV1,
        ev: device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match ev {
            device::Event::Selection { id } => {
                if let Some(old) = st.selection.take() {
                    old.destroy();
                }
                let names = id
                    .as_ref()
                    .map(|o| o.data::<OfferMimes>().unwrap().lock().unwrap().clone())
                    .unwrap_or_default();
                if names.iter().any(|n| n == MARKER) {
                    if let Some(o) = id {
                        o.destroy();
                    }
                    if let Some(own) = &st.own {
                        let types = if own.promised.is_empty() {
                            own.content
                                .0
                                .iter()
                                .filter_map(|(m, _)| mime::canonical(m))
                                .collect()
                        } else {
                            own.promised.clone()
                        };
                        *st.shared.offered.lock().unwrap() = types;
                    }
                    return;
                }
                let mut offered: Vec<&'static str> =
                    names.iter().filter_map(|n| mime::canonical(n)).collect();
                offered.sort();
                offered.dedup();
                *st.shared.offered.lock().unwrap() = offered;
                st.shared.serial.fetch_add(1, Ordering::SeqCst);
                st.selection = id;
                if let Some(own) = st.own.take() {
                    own.source.destroy();
                }
            }
            device::Event::PrimarySelection { id: Some(o) } => o.destroy(),
            device::Event::Finished => st.dead = true,
            _ => {}
        }
    }

    event_created_child!(State, ExtDataControlDeviceV1, [
        device::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, OfferMimes::default()),
    ]);
}

impl Dispatch<ExtDataControlOfferV1, OfferMimes> for State {
    fn event(
        _: &mut Self,
        _: &ExtDataControlOfferV1,
        ev: offer::Event,
        names: &OfferMimes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let offer::Event::Offer { mime_type } = ev {
            names.lock().unwrap().push(mime_type);
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for State {
    fn event(
        st: &mut Self,
        src: &ExtDataControlSourceV1,
        ev: source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(own) = st.own.as_ref().filter(|o| &o.source == src) else {
            if let source::Event::Cancelled = ev {
                src.destroy();
            }
            return;
        };
        match ev {
            source::Event::Send { mime_type, fd } => {
                let Some(m) = mime::canonical(&mime_type) else {
                    return;
                };
                match own.content.get(m) {
                    Some(d) => write_async(fd, d.to_vec()),
                    None if own.promised.contains(&m) => {
                        st.pending.push((m, fd, Instant::now()));
                        st.shared.data_request.store(true, Ordering::SeqCst);
                    }
                    None => {}
                }
            }
            source::Event::Cancelled => {
                src.destroy();
                st.own = None;
            }
            _ => {}
        }
    }
}

/// For probes: which data-control globals the compositor offers.
pub fn data_control_globals() -> Result<Vec<(String, u32)>> {
    let conn = crate::display::connect_wayland()?;
    let (globals, _q) = registry_queue_init::<State>(&conn)?;
    Ok(globals
        .contents()
        .clone_list()
        .into_iter()
        .filter(|g| g.interface.contains("data_control"))
        .map(|g| (g.interface, g.version))
        .collect())
}
