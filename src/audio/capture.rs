//! Cancellable monitor capture through libpulse, loaded at runtime.
//! The private mainloop never blocks. Routing and volume are left alone; the only
//! change is the opt-in mute of the monitored sink while streaming.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::rtp::{CHANNELS, CLOCK_RATE, SAMPLES_PER_FRAME};
use anyhow::{Result, ensure};
use tracing::{info, warn};

pub const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";
/// Lets fragsize set the source latency, as `pa_simple_new` did.
const PA_STREAM_ADJUST_LATENCY: c_int = 0x2000;
const PA_OPERATION_RUNNING: c_int = 0;

#[repr(C)]
struct SampleSpec {
    format: c_int,
    rate: u32,
    channels: u8,
}
#[repr(C)]
struct BufferAttr {
    maxlength: u32,
    tlength: u32,
    prebuf: u32,
    minreq: u32,
    fragsize: u32,
}
type Ptr = *mut c_void;

macro_rules! pulse_api {
    ($($name:ident: $ty:ty),* $(,)?) => {
        struct Api { $($name: $ty,)* _lib: libloading::Library }
        impl Api {
            fn load() -> Result<Self> {
                // SAFETY: libpulse is the system ABI. Each symbol's signature
                // matches its declaration in the PulseAudio public headers.
                // The library stays loaded until after all objects are freed.
                unsafe {
                    let lib = libloading::Library::new("libpulse.so.0")?;
                    Ok(Self { $($name: *lib.get::<$ty>(concat!(stringify!($name), "\0").as_bytes())?,)* _lib: lib })
                }
            }
        }
    };
}
pulse_api! {
    pa_mainloop_new: unsafe extern "C" fn() -> Ptr,
    pa_mainloop_free: unsafe extern "C" fn(Ptr),
    pa_mainloop_get_api: unsafe extern "C" fn(Ptr) -> Ptr,
    pa_mainloop_iterate: unsafe extern "C" fn(Ptr, c_int, *mut c_int) -> c_int,
    pa_context_new: unsafe extern "C" fn(Ptr, *const c_char) -> Ptr,
    pa_context_connect: unsafe extern "C" fn(Ptr, *const c_char, c_int, *const c_void) -> c_int,
    pa_context_get_state: unsafe extern "C" fn(Ptr) -> c_int,
    pa_context_disconnect: unsafe extern "C" fn(Ptr),
    pa_context_unref: unsafe extern "C" fn(Ptr),
    pa_stream_new: unsafe extern "C" fn(Ptr, *const c_char, *const SampleSpec, *const c_void) -> Ptr,
    pa_stream_connect_record: unsafe extern "C" fn(Ptr, *const c_char, *const BufferAttr, c_int) -> c_int,
    pa_stream_get_state: unsafe extern "C" fn(Ptr) -> c_int,
    pa_stream_peek: unsafe extern "C" fn(Ptr, *mut *const c_void, *mut usize) -> c_int,
    pa_stream_drop: unsafe extern "C" fn(Ptr) -> c_int,
    pa_stream_disconnect: unsafe extern "C" fn(Ptr) -> c_int,
    pa_stream_unref: unsafe extern "C" fn(Ptr),
    pa_context_set_sink_mute_by_name: unsafe extern "C" fn(Ptr, *const c_char, c_int, *const c_void, Ptr) -> Ptr,
    pa_operation_get_state: unsafe extern "C" fn(Ptr) -> c_int,
    pa_operation_unref: unsafe extern "C" fn(Ptr),
}

/// The sink whose monitor `device` names. PulseAudio monitors are `<sink>.monitor`.
fn sink_for_monitor(device: Option<&str>) -> &str {
    device
        .and_then(|d| d.strip_suffix(".monitor"))
        .unwrap_or("@DEFAULT_SINK@")
}

pub struct MonitorCapture {
    api: Api,
    mainloop: Ptr,
    context: Ptr,
    stream: Ptr,
    fragment_offset: usize,
    sink: CString,
    sink_muted: bool,
}

impl MonitorCapture {
    /// Returns None on cancellation, including during connection setup.
    pub fn open(device: Option<&str>, stop: &AtomicBool) -> Result<Option<Self>> {
        Self::open_at(device, None, stop)
    }

    fn open_at(
        device: Option<&str>,
        server: Option<&str>,
        stop: &AtomicBool,
    ) -> Result<Option<Self>> {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let api = Api::load()?;
        // SAFETY: constructor has no arguments; null is rejected below.
        let mainloop = unsafe { (api.pa_mainloop_new)() };
        ensure!(!mainloop.is_null(), "pa_mainloop_new failed");
        let mut cap = Self {
            api,
            mainloop,
            context: std::ptr::null_mut(),
            stream: std::ptr::null_mut(),
            fragment_offset: 0,
            sink: CString::new(sink_for_monitor(device))?,
            sink_muted: false,
        };
        let spec = SampleSpec {
            format: 3,
            rate: CLOCK_RATE,
            channels: CHANNELS as u8,
        };
        let attr = BufferAttr {
            maxlength: u32::MAX,
            tlength: u32::MAX,
            prebuf: u32::MAX,
            minreq: u32::MAX,
            fragsize: SAMPLES_PER_FRAME * CHANNELS * 2,
        };
        let dev = CString::new(device.unwrap_or(DEFAULT_MONITOR))?;
        let server = server.map(CString::new).transpose()?;
        // SAFETY: mainloop is live; names are NUL terminated. All objects are
        // owned by cap, and Drop handles partial initialization on errors.
        unsafe {
            cap.context = (cap.api.pa_context_new)(
                (cap.api.pa_mainloop_get_api)(mainloop),
                c"wayshare".as_ptr(),
            );
            ensure!(!cap.context.is_null(), "pa_context_new failed");
            ensure!(
                (cap.api.pa_context_connect)(
                    cap.context,
                    server.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
                    0,
                    std::ptr::null()
                ) == 0,
                "pa_context_connect failed"
            );
        }
        if !cap.wait_ready(false, stop)? {
            return Ok(None);
        }
        // SAFETY: context is ready; sample spec, attrs and names live through
        // their calls. No callback retains Rust stack pointers.
        unsafe {
            cap.stream = (cap.api.pa_stream_new)(
                cap.context,
                c"screen sharing audio".as_ptr(),
                &spec,
                std::ptr::null(),
            );
            ensure!(!cap.stream.is_null(), "pa_stream_new failed");
            ensure!(
                (cap.api.pa_stream_connect_record)(
                    cap.stream,
                    dev.as_ptr(),
                    &attr,
                    PA_STREAM_ADJUST_LATENCY,
                ) == 0,
                "pa_stream_connect_record failed"
            );
        }
        if !cap.wait_ready(true, stop)? {
            return Ok(None);
        }
        Ok(Some(cap))
    }

    /// Mutes the sink being captured so the sound plays only on the viewer. The monitor
    /// still carries the audio. Dropping the capture unmutes it.
    pub fn mute_sink(&mut self) -> Result<()> {
        self.set_sink_mute(true)?;
        self.sink_muted = true;
        Ok(())
    }

    fn set_sink_mute(&mut self, mute: bool) -> Result<()> {
        // SAFETY: context is ready and the sink name outlives the call. No
        // callback is registered, so nothing retains a pointer into self.
        let op = unsafe {
            (self.api.pa_context_set_sink_mute_by_name)(
                self.context,
                self.sink.as_ptr(),
                c_int::from(mute),
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        ensure!(!op.is_null(), "pa_context_set_sink_mute_by_name failed");
        let deadline = Instant::now() + Duration::from_secs(1);
        let result = loop {
            if let Err(e) = self.iterate() {
                break Err(e);
            }
            // SAFETY: op is a live operation until the unref below.
            if unsafe { (self.api.pa_operation_get_state)(op) } != PA_OPERATION_RUNNING {
                break Ok(());
            }
            if Instant::now() >= deadline {
                break Err(anyhow::anyhow!("PulseAudio mute timeout"));
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        // SAFETY: releases our reference to the operation exactly once.
        unsafe { (self.api.pa_operation_unref)(op) };
        result
    }

    fn iterate(&mut self) -> Result<()> {
        // SAFETY: private live mainloop, used on its owning audio thread only.
        ensure!(
            unsafe { (self.api.pa_mainloop_iterate)(self.mainloop, 0, std::ptr::null_mut()) } >= 0,
            "PulseAudio mainloop failed"
        );
        Ok(())
    }

    fn wait_ready(&mut self, stream: bool, stop: &AtomicBool) -> Result<bool> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !stop.load(Ordering::Relaxed) {
            self.iterate()?;
            // SAFETY: the selected object was constructed before this call.
            let state = unsafe {
                if stream {
                    (self.api.pa_stream_get_state)(self.stream)
                } else {
                    (self.api.pa_context_get_state)(self.context)
                }
            };
            let ready = if stream { 2 } else { 4 };
            if state == ready {
                return Ok(true);
            }
            ensure!(
                state < ready,
                "PulseAudio connection failed (state {state})"
            );
            ensure!(Instant::now() < deadline, "PulseAudio connection timeout");
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(false)
    }

    /// Reads one frame, returning false on cancellation even if capture is stalled.
    pub fn read_frame(
        &mut self,
        out: &mut [i16; (SAMPLES_PER_FRAME * CHANNELS) as usize],
        stop: &AtomicBool,
    ) -> Result<bool> {
        // SAFETY: writable bytes of a live i16 array; all bit patterns are valid.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                out.as_mut_ptr().cast::<u8>(),
                std::mem::size_of_val(out),
            )
        };
        let mut filled = 0;
        while filled < bytes.len() {
            if stop.load(Ordering::Relaxed) {
                return Ok(false);
            }
            self.iterate()?;
            let (mut data, mut len) = (std::ptr::null(), 0);
            // SAFETY: live recording stream; valid out pointers. Peek's data
            // stays valid until drop; no pointer is retained across iterations.
            ensure!(
                unsafe { (self.api.pa_stream_peek)(self.stream, &mut data, &mut len) } == 0,
                "pa_stream_peek failed"
            );
            if len == 0 {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            ensure!(
                self.fragment_offset < len,
                "PulseAudio fragment changed before drop"
            );
            let n = (len - self.fragment_offset).min(bytes.len() - filled);
            if data.is_null() {
                bytes[filled..filled + n].fill(0); // A PulseAudio hole is silence.
            } else {
                // SAFETY: peek returned len readable bytes, and both source
                // and destination ranges were bounded above. They do not alias.
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        data.cast::<u8>().add(self.fragment_offset),
                        bytes[filled..].as_mut_ptr(),
                        n,
                    )
                };
            }
            self.fragment_offset += n;
            filled += n;
            if self.fragment_offset == len {
                // SAFETY: a nonempty fragment was peeked and fully consumed.
                ensure!(
                    unsafe { (self.api.pa_stream_drop)(self.stream) } == 0,
                    "pa_stream_drop failed"
                );
                self.fragment_offset = 0;
            }
        }
        Ok(true)
    }
}

impl Drop for MonitorCapture {
    fn drop(&mut self) {
        if self.sink_muted {
            match self.set_sink_mute(false) {
                Ok(()) => info!(sink = ?self.sink, "unmuted local speakers"),
                Err(e) => warn!(sink = ?self.sink, "could not unmute local speakers: {e:#}"),
            }
        }
        // SAFETY: only successfully created objects are released, in dependency
        // order, on the owning thread. No callbacks or other threads use them.
        unsafe {
            if !self.stream.is_null() {
                (self.api.pa_stream_disconnect)(self.stream);
                (self.api.pa_stream_unref)(self.stream);
            }
            if !self.context.is_null() {
                (self.api.pa_context_disconnect)(self.context);
                (self.api.pa_context_unref)(self.context);
            }
            (self.api.pa_mainloop_free)(self.mainloop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutes_the_sink_behind_the_monitor() {
        assert_eq!(sink_for_monitor(None), "@DEFAULT_SINK@");
        assert_eq!(
            sink_for_monitor(Some("alsa_output.pci.analog-stereo.monitor")),
            "alsa_output.pci.analog-stereo"
        );
        assert_eq!(
            sink_for_monitor(Some("@DEFAULT_MONITOR@")),
            "@DEFAULT_SINK@"
        );
    }

    #[test]
    fn cancelled_open_needs_no_audio_server() {
        assert!(
            MonitorCapture::open(None, &AtomicBool::new(true))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    #[ignore = "requires a running PulseAudio-compatible monitor"]
    fn live_monitor_read_and_cancel() {
        let stop = AtomicBool::new(false);
        let mut cap = MonitorCapture::open(None, &stop).unwrap().unwrap();
        let mut frame = [0; (SAMPLES_PER_FRAME * CHANNELS) as usize];
        for _ in 0..10 {
            assert!(cap.read_frame(&mut frame, &stop).unwrap());
        }
        stop.store(true, Ordering::Relaxed);
        assert!(!cap.read_frame(&mut frame, &stop).unwrap());
        // Simulate a connected monitor that stops producing data, without
        // changing the host's audio server or routing.
        unsafe extern "C" fn empty_peek(
            _: Ptr,
            data: *mut *const c_void,
            len: *mut usize,
        ) -> c_int {
            // SAFETY: read_frame supplies valid out pointers for this test stub.
            unsafe {
                *data = std::ptr::null();
                *len = 0;
            }
            0
        }
        cap.api.pa_stream_peek = empty_peek;
        stop.store(false, Ordering::Relaxed);
        let before = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                stop.store(true, Ordering::Relaxed);
            });
            assert!(!cap.read_frame(&mut frame, &stop).unwrap());
        });
        assert!(before.elapsed() < Duration::from_secs(1));
    }

    #[test]
    #[ignore = "requires libpulse.so.0"]
    fn stalled_audio_connection_can_be_cancelled() {
        use std::os::unix::net::UnixListener;
        let path =
            std::env::temp_dir().join(format!("wayshare-pulse-test-{}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        let stop = AtomicBool::new(false);
        let before = Instant::now();
        std::thread::scope(|s| {
            s.spawn(|| {
                std::thread::sleep(Duration::from_millis(50));
                stop.store(true, Ordering::Relaxed);
            });
            let server = format!("unix:{}", path.display());
            assert!(
                MonitorCapture::open_at(None, Some(&server), &stop)
                    .unwrap()
                    .is_none()
            );
        });
        drop(listener);
        std::fs::remove_file(path).unwrap();
        assert!(before.elapsed() < Duration::from_secs(1));
    }
}
