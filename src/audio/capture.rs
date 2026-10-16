//! System audio capture from the default sink's monitor through libpulse-simple (served by
//! pipewire-pulse), loaded at runtime. Read-only: records a monitor, never changes routing.

use std::ffi::{CString, c_char, c_int, c_void};

use anyhow::{Result, anyhow, ensure};

use super::rtp::{CHANNELS, CLOCK_RATE, SAMPLES_PER_FRAME};

const PA_STREAM_RECORD: c_int = 2;
const PA_SAMPLE_S16LE: c_int = 3;
pub const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";

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

type NewFn = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    c_int,
    *const c_char,
    *const c_char,
    *const SampleSpec,
    *const c_void,
    *const BufferAttr,
    *mut c_int,
) -> *mut c_void;
type ReadFn = unsafe extern "C" fn(*mut c_void, *mut c_void, usize, *mut c_int) -> c_int;
type FreeFn = unsafe extern "C" fn(*mut c_void);

pub struct MonitorCapture {
    s: *mut c_void,
    read: ReadFn,
    free: FreeFn,
    _lib: *mut c_void,
}

// SAFETY: `s` is the pa_simple stream owned only by this value (freed only in `Drop`) and is
// used only through `&mut self`, so it is never accessed from two threads at once.
unsafe impl Send for MonitorCapture {}

impl MonitorCapture {
    /// `device`: a PulseAudio source name; `None` = the default sink's monitor.
    pub fn open(device: Option<&str>) -> Result<Self> {
        let name = CString::new("libpulse-simple.so.0").unwrap();
        // SAFETY: The name is NUL-terminated, and a null result is rejected on the next line.
        let lib = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        ensure!(!lib.is_null(), "libpulse-simple.so.0 not found");
        let sym = |n: &str| -> Result<*mut c_void> {
            let c = CString::new(n).unwrap();
            // SAFETY: `lib` is non-null (checked above), the name is NUL-terminated, and a null
            // result is rejected below.
            let p = unsafe { libc::dlsym(lib, c.as_ptr()) };
            if p.is_null() {
                Err(anyhow!("libpulse-simple lacks {n}"))
            } else {
                Ok(p)
            }
        };
        // SAFETY: Each pointer is non-null (checked in `sym`) and names the libpulse-simple
        // function whose C signature matches the target type. The library is never dlclosed, so
        // the pointers stay valid.
        let (new, read, free): (NewFn, ReadFn, FreeFn) = unsafe {
            (
                std::mem::transmute::<*mut c_void, NewFn>(sym("pa_simple_new")?),
                std::mem::transmute::<*mut c_void, ReadFn>(sym("pa_simple_read")?),
                std::mem::transmute::<*mut c_void, FreeFn>(sym("pa_simple_free")?),
            )
        };
        let spec = SampleSpec {
            format: PA_SAMPLE_S16LE,
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
        let app = CString::new("wayshare").unwrap();
        let stream = CString::new("screen sharing audio").unwrap();
        let dev = CString::new(device.unwrap_or(DEFAULT_MONITOR))?;
        let mut err = 0;
        // SAFETY: Every pointer argument is null or points to a value that lives past the call,
        // and `err` is a valid out pointer. A null stream is rejected below.
        let s = unsafe {
            new(
                std::ptr::null(),
                app.as_ptr(),
                PA_STREAM_RECORD,
                dev.as_ptr(),
                stream.as_ptr(),
                &spec,
                std::ptr::null(),
                &attr,
                &mut err,
            )
        };
        ensure!(
            !s.is_null(),
            "pa_simple_new({}) failed: error {err}",
            dev.to_string_lossy()
        );
        Ok(Self {
            s,
            read,
            free,
            _lib: lib,
        })
    }

    /// Blocks for one 10 ms frame of interleaved stereo i16.
    pub fn read_frame(
        &mut self,
        out: &mut [i16; (SAMPLES_PER_FRAME * CHANNELS) as usize],
    ) -> Result<()> {
        let mut err = 0;
        // SAFETY: `self.s` is the live stream from pa_simple_new, `out` is writable for
        // `out.len() * 2` bytes, and `err` is a valid out pointer.
        let r = unsafe {
            (self.read)(
                self.s,
                out.as_mut_ptr() as *mut c_void,
                out.len() * 2,
                &mut err,
            )
        };
        ensure!(r == 0, "pa_simple_read failed: error {err}");
        Ok(())
    }
}

impl Drop for MonitorCapture {
    fn drop(&mut self) {
        // SAFETY: `self.s` is the non-null stream from pa_simple_new and is freed only here.
        unsafe { (self.free)(self.s) };
    }
}
