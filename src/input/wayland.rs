//! wlr-virtual-pointer-unstable-v1 + virtual-keyboard-unstable-v1 backend.

use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use tracing::{debug, warn};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_pointer, wl_registry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

use super::keyboard::{KbAction, KeyboardConfig, KeyboardEngine, layout_for_input_source};
use super::pointer::{Axis, AxisSource, PointerConfig, PointerEngine, PtrAction};
use super::{InputSink, ScrollEvent};

const KEYMAP_FORMAT_XKB_V1: u32 = 1;

struct State {
    outputs: Vec<(wl_output::WlOutput, Option<String>)>,
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

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        o: &wl_output::WlOutput,
        ev: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = ev
            && let Some(e) = state.outputs.iter_mut().find(|(w, _)| w == o)
        {
            e.1 = Some(name);
        }
    }
}

delegate_noop!(State: ignore WlSeat);
delegate_noop!(State: ZwlrVirtualPointerManagerV1);
delegate_noop!(State: ZwlrVirtualPointerV1);
delegate_noop!(State: ZwpVirtualKeyboardManagerV1);
delegate_noop!(State: ZwpVirtualKeyboardV1);

pub struct WaylandInput {
    conn: Connection,
    queue: EventQueue<State>,
    state: State,
    pointer: ZwlrVirtualPointerV1,
    keyboard: ZwpVirtualKeyboardV1,
    kb: KeyboardEngine,
    ptr: PointerEngine,
    epoch: Instant,
    /// Keeps the current keymap alive until the next upload.
    _keymap: Option<OwnedFd>,
}

impl WaylandInput {
    /// Binds to `$WAYLAND_DISPLAY`. With `output`, pointer coordinates are relative to that
    /// output; otherwise to the whole layout.
    pub fn connect(
        output: Option<&str>,
        width: u32,
        height: u32,
        kcfg: KeyboardConfig,
        pcfg: PointerConfig,
    ) -> Result<Self> {
        let conn = crate::display::connect_wayland().context("connect to Wayland display")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let seat: WlSeat = globals.bind(&qh, 1..=1, ()).context("wl_seat")?;
        let vpm: ZwlrVirtualPointerManagerV1 = globals
            .bind(&qh, 1..=2, ())
            .context("zwlr_virtual_pointer_manager_v1 not offered")?;
        let vkm: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .context("zwp_virtual_keyboard_manager_v1 not offered")?;
        let mut state = State {
            outputs: Vec::new(),
        };
        globals.contents().with_list(|list| {
            for g in list
                .iter()
                .filter(|g| g.interface == "wl_output" && g.version >= 4)
            {
                let o = globals
                    .registry()
                    .bind::<wl_output::WlOutput, _, _>(g.name, 4, &qh, ());
                state.outputs.push((o, None));
            }
        });
        queue.roundtrip(&mut state)?;

        let pointer = match output {
            Some(name) => {
                if vpm.version() < 2 {
                    bail!(
                        "virtual pointer manager v{} can't bind to an output",
                        vpm.version()
                    );
                }
                let (o, _) = state
                    .outputs
                    .iter()
                    .find(|(_, n)| n.as_deref() == Some(name))
                    .ok_or_else(|| anyhow!("no wl_output named {name}"))?;
                vpm.create_virtual_pointer_with_output(Some(&seat), Some(o), &qh, ())
            }
            None => vpm.create_virtual_pointer(Some(&seat), &qh, ()),
        };
        let keyboard = vkm.create_virtual_keyboard(&seat, &qh, ());
        let kb = KeyboardEngine::new(kcfg);
        let mut me = Self {
            conn,
            queue,
            state,
            pointer,
            keyboard,
            ptr: PointerEngine::new(pcfg, width, height),
            epoch: Instant::now(),
            _keymap: None,
            kb,
        };
        let init = me.kb.init();
        me.run_kb(init)?;
        me.queue
            .roundtrip(&mut me.state)
            .context("virtual input setup rejected")?;
        Ok(me)
    }

    /// Round trip to the compositor; surfaces protocol errors from earlier requests.
    pub fn sync(&mut self) -> Result<()> {
        self.queue
            .roundtrip(&mut self.state)
            .context("wayland roundtrip")?;
        Ok(())
    }

    fn now(&self) -> u32 {
        self.epoch.elapsed().as_millis() as u32
    }

    fn run_kb(&mut self, actions: Vec<KbAction>) -> Result<()> {
        let t = self.now();
        for a in actions {
            match a {
                KbAction::Keymap(text) => {
                    let fd = keymap_fd(&text)?;
                    self.keyboard
                        .keymap(KEYMAP_FORMAT_XKB_V1, fd.as_fd(), text.len() as u32 + 1);
                    self._keymap = Some(fd);
                }
                KbAction::Key { code, down } => self.keyboard.key(t, code, down as u32),
                KbAction::Modifiers { depressed, locked } => {
                    self.keyboard.modifiers(depressed, 0, locked, 0)
                }
            }
        }
        self.flush()
    }

    fn run_ptr(&mut self, actions: Vec<PtrAction>) -> Result<()> {
        let t = self.now();
        let axis = |a: Axis| match a {
            Axis::Vertical => wl_pointer::Axis::VerticalScroll,
            Axis::Horizontal => wl_pointer::Axis::HorizontalScroll,
        };
        for a in actions {
            match a {
                PtrAction::Motion {
                    x,
                    y,
                    width,
                    height,
                } => self.pointer.motion_absolute(t, x, y, width, height),
                PtrAction::Button { code, pressed } => {
                    let s = if pressed {
                        wl_pointer::ButtonState::Pressed
                    } else {
                        wl_pointer::ButtonState::Released
                    };
                    self.pointer.button(t, code, s)
                }
                PtrAction::AxisSource(s) => self.pointer.axis_source(match s {
                    AxisSource::Wheel => wl_pointer::AxisSource::Wheel,
                    AxisSource::Finger => wl_pointer::AxisSource::Finger,
                }),
                PtrAction::Axis { axis: a, value } => self.pointer.axis(t, axis(a), value),
                PtrAction::AxisDiscrete {
                    axis: a,
                    value,
                    steps,
                } => self.pointer.axis_discrete(t, axis(a), value, steps),
                PtrAction::AxisStop(a) => self.pointer.axis_stop(t, axis(a)),
                PtrAction::Frame => self.pointer.frame(),
            }
        }
        self.flush()
    }

    /// Sends queued requests and drains incoming events so the compositor never blocks on us.
    fn flush(&mut self) -> Result<()> {
        self.conn.flush().context("wayland flush")?;
        if let Some(guard) = self.conn.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e).context("wayland read"),
            }
        }
        self.queue.dispatch_pending(&mut self.state)?;
        Ok(())
    }
}

fn keymap_fd(text: &str) -> Result<OwnedFd> {
    // SAFETY: The name is a NUL-terminated literal.
    let raw = unsafe { libc::memfd_create(c"wayshare-keymap".as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error()).context("memfd_create");
    }
    // SAFETY: `raw` is a fresh fd, checked non-negative above, that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut f = std::fs::File::from(fd);
    f.write_all(text.as_bytes())?;
    f.write_all(&[0])?;
    Ok(f.into())
}

impl InputSink for WaylandInput {
    fn key(&mut self, down: bool, keysym: u32, mac_keycode: Option<u16>) -> Result<()> {
        let a = self.kb.key(down, keysym, mac_keycode);
        if a.is_empty() {
            debug!(
                down,
                keysym = format_args!("{keysym:#x}"),
                ?mac_keycode,
                "key dropped"
            );
        }
        self.run_kb(a)
    }

    fn pointer(&mut self, x: i32, y: i32, buttons: u8) -> Result<()> {
        let a = self.ptr.pointer(x, y, buttons);
        self.run_ptr(a)
    }

    fn scroll(&mut self, ev: &ScrollEvent) -> Result<()> {
        let a = self.ptr.scroll(ev);
        self.run_ptr(a)
    }

    fn set_extent(&mut self, width: u32, height: u32) {
        self.ptr.set_extent(width, height);
    }

    fn set_input_source(&mut self, id: &str) -> Result<()> {
        match layout_for_input_source(id) {
            Some((layout, variant)) => {
                let a = self.kb.set_layout(layout, variant);
                self.run_kb(a)
            }
            None => {
                warn!(id, "unknown keyboard input source; keeping layout");
                Ok(())
            }
        }
    }

    fn release_all(&mut self) -> Result<()> {
        let k = self.kb.release_all();
        self.run_kb(k)?;
        let p = self.ptr.release_all();
        self.run_ptr(p)
    }
}

impl Drop for WaylandInput {
    fn drop(&mut self) {
        let _ = self.release_all();
        self.pointer.destroy();
        self.keyboard.destroy();
        for (o, _) in self.state.outputs.drain(..) {
            o.release();
        }
        let _ = self.conn.flush();
    }
}
