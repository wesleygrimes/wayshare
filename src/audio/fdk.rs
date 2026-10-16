//! AAC-ELD encoder via libfdk-aac, loaded at runtime (no build dependency, no bundled codec).

use std::ffi::{CString, c_void};

use anyhow::{Result, anyhow, bail, ensure};

use super::rtp::{CHANNELS, CLOCK_RATE, SAMPLES_PER_FRAME};

type Handle = *mut c_void;

#[repr(C)]
struct BufDesc {
    num_bufs: i32,
    bufs: *mut *mut c_void,
    buffer_identifiers: *mut i32,
    buf_sizes: *mut i32,
    buf_el_sizes: *mut i32,
}

#[repr(C)]
struct InArgs {
    num_in_samples: i32,
    num_anc_bytes: i32,
}

#[repr(C)]
#[derive(Default)]
struct OutArgs {
    num_out_bytes: i32,
    num_in_samples: i32,
    num_anc_bytes: i32,
    bit_res_state: i32,
}

#[repr(C)]
struct InfoStruct {
    max_out_buf_bytes: u32,
    max_anc_bytes: u32,
    in_buf_fill_level: u32,
    input_channels: u32,
    frame_length: u32,
    n_delay: u32,
    n_delay_core: u32,
    conf_buf: [u8; 64],
    conf_size: u32,
}

const AACENC_AOT: u32 = 0x0100;
const AACENC_BITRATE: u32 = 0x0101;
const AACENC_SAMPLERATE: u32 = 0x0103;
const AACENC_SBR_MODE: u32 = 0x0104;
const AACENC_GRANULE_LENGTH: u32 = 0x0105;
const AACENC_CHANNELMODE: u32 = 0x0106;
const AACENC_SBR_RATIO: u32 = 0x0108;
const AACENC_AFTERBURNER: u32 = 0x0200;
const AACENC_BANDWIDTH: u32 = 0x0203;
const AACENC_TRANSMUX: u32 = 0x0300;
const IN_AUDIO_DATA: i32 = 0;
const OUT_BITSTREAM_DATA: i32 = 3;
const AOT_ER_AAC_ELD: u32 = 39;

type OpenFn = unsafe extern "C" fn(*mut Handle, u32, u32) -> i32;
type CloseFn = unsafe extern "C" fn(*mut Handle) -> i32;
type SetParamFn = unsafe extern "C" fn(Handle, u32, u32) -> i32;
type EncodeFn = unsafe extern "C" fn(
    Handle,
    *const BufDesc,
    *const BufDesc,
    *const InArgs,
    *mut OutArgs,
) -> i32;
type InfoFn = unsafe extern "C" fn(Handle, *mut InfoStruct) -> i32;

struct Api {
    _lib: *mut c_void,
    open: OpenFn,
    close: CloseFn,
    set_param: SetParamFn,
    encode: EncodeFn,
    info: InfoFn,
}

impl Api {
    fn load() -> Result<Self> {
        let mut lib = std::ptr::null_mut();
        for name in ["libfdk-aac.so.2", "libfdk-aac.so"] {
            let c = CString::new(name).unwrap();
            // SAFETY: The name is NUL-terminated, and a null result is handled right after.
            lib = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            if !lib.is_null() {
                break;
            }
        }
        ensure!(
            !lib.is_null(),
            "libfdk-aac not found (install the libfdk-aac package)"
        );
        let sym = |n: &str| -> Result<*mut c_void> {
            let c = CString::new(n).unwrap();
            // SAFETY: `lib` is non-null (checked before this closure is created), the name is
            // NUL-terminated, and a null result is returned as an error.
            let p = unsafe { libc::dlsym(lib, c.as_ptr()) };
            if p.is_null() {
                Err(anyhow!("libfdk-aac lacks {n}"))
            } else {
                Ok(p)
            }
        };
        // SAFETY: Each pointer is non-null (checked in `sym`) and names the libfdk-aac function
        // whose C signature matches the target type. The library is never dlclosed, so the
        // pointers stay valid.
        unsafe {
            Ok(Self {
                open: std::mem::transmute::<*mut c_void, OpenFn>(sym("aacEncOpen")?),
                close: std::mem::transmute::<*mut c_void, CloseFn>(sym("aacEncClose")?),
                set_param: std::mem::transmute::<*mut c_void, SetParamFn>(sym(
                    "aacEncoder_SetParam",
                )?),
                encode: std::mem::transmute::<*mut c_void, EncodeFn>(sym("aacEncEncode")?),
                info: std::mem::transmute::<*mut c_void, InfoFn>(sym("aacEncInfo")?),
                _lib: lib,
            })
        }
    }
}

#[derive(Clone, Debug)]
pub struct EncoderConfig {
    pub bitrate: u32,
    pub sbr: bool,
    /// Core audio bandwidth in Hz; 0 lets the encoder choose.
    pub bandwidth: u32,
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            bitrate: 128_000,
            sbr: false,
            bandwidth: 20_000,
        }
    }
}

/// 48 kHz stereo AAC-ELD, 480-sample frames. The receiver decodes with its own built-in config
/// (Apple's `aacf` ASC `f8 e6 51 34 e0 00`, LD-SBR start_freq 10) and never sees ours. fdk's
/// SBR headers differ (start_freq 7/9/11/12/13 by bitrate), so its SBR band decodes to
/// silence there; the default is SBR off with a 20 kHz core band, which that decoder plays
/// flat to 20 kHz.
pub struct AacEldEncoder {
    api: Api,
    h: Handle,
    out: Vec<u8>,
    asc: Vec<u8>,
    frame_length: u32,
}

// SAFETY: The encoder handle is owned only by this value (closed only in `Drop`) and is used
// only through `&self`/`&mut self`, so it is never accessed from two threads at once.
unsafe impl Send for AacEldEncoder {}

impl AacEldEncoder {
    pub fn new(cfg: &EncoderConfig) -> Result<Self> {
        let api = Api::load()?;
        let mut h: Handle = std::ptr::null_mut();
        // SAFETY: `&mut h` is a valid out pointer, and the handle is null-checked on the next
        // line.
        let e = unsafe { (api.open)(&mut h, 0, CHANNELS) };
        ensure!(e == 0 && !h.is_null(), "aacEncOpen failed: {e:#x}");
        let mut enc = Self {
            api,
            h,
            out: vec![0; 8192],
            asc: Vec::new(),
            frame_length: 0,
        };
        for (p, v) in [
            (AACENC_AOT, AOT_ER_AAC_ELD),
            (AACENC_SAMPLERATE, CLOCK_RATE),
            (AACENC_CHANNELMODE, CHANNELS),
            (AACENC_GRANULE_LENGTH, SAMPLES_PER_FRAME),
            (AACENC_SBR_MODE, cfg.sbr as u32),
            (AACENC_TRANSMUX, 0),
            (AACENC_AFTERBURNER, 1),
            (AACENC_BITRATE, cfg.bitrate),
        ]
        .into_iter()
        .chain(cfg.sbr.then_some((AACENC_SBR_RATIO, 1)))
        .chain((cfg.bandwidth > 0).then_some((AACENC_BANDWIDTH, cfg.bandwidth)))
        {
            // SAFETY: `enc.h` is the non-null handle from aacEncOpen.
            let e = unsafe { (enc.api.set_param)(enc.h, p, v) };
            ensure!(e == 0, "aacEncoder_SetParam({p:#x}, {v}) failed: {e:#x}");
        }
        // SAFETY: `enc.h` is a live handle, and null descriptors and arguments make
        // aacEncEncode initialise the encoder. The result is checked below.
        let e = unsafe {
            (enc.api.encode)(
                enc.h,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
            )
        };
        ensure!(
            e == 0,
            "aacEncEncode init failed: {e:#x} (bitrate {} unsupported?)",
            cfg.bitrate
        );
        // SAFETY: `InfoStruct` is `repr(C)` with only integer fields, so all-zero is a valid
        // value.
        let mut info: InfoStruct = unsafe { std::mem::zeroed() };
        // SAFETY: `enc.h` is a live handle and `info` is a valid out pointer.
        let e = unsafe { (enc.api.info)(enc.h, &mut info) };
        ensure!(e == 0, "aacEncInfo failed: {e:#x}");
        enc.asc = info.conf_buf[..info.conf_size.min(64) as usize].to_vec();
        enc.frame_length = info.frame_length;
        ensure!(
            enc.frame_length == SAMPLES_PER_FRAME,
            "encoder frame length {}",
            enc.frame_length
        );
        enc.out.resize(info.max_out_buf_bytes.max(1024) as usize, 0);
        Ok(enc)
    }

    /// AudioSpecificConfig the encoder produced (for comparison with Apple's).
    pub fn audio_specific_config(&self) -> &[u8] {
        &self.asc
    }

    pub fn frame_length(&self) -> u32 {
        self.frame_length
    }

    /// Encodes one 10 ms frame of interleaved stereo i16 (960 values). Returns an AU once the
    /// encoder's look-ahead is filled (the first few calls return `None`).
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Option<Vec<u8>>> {
        ensure!(
            pcm.len() == (SAMPLES_PER_FRAME * CHANNELS) as usize,
            "need {} samples",
            SAMPLES_PER_FRAME * CHANNELS
        );
        let mut in_ptr = pcm.as_ptr() as *mut c_void;
        let mut in_id = IN_AUDIO_DATA;
        let mut in_size = (pcm.len() * 2) as i32;
        let mut in_el = 2i32;
        let in_desc = BufDesc {
            num_bufs: 1,
            bufs: &mut in_ptr,
            buffer_identifiers: &mut in_id,
            buf_sizes: &mut in_size,
            buf_el_sizes: &mut in_el,
        };
        let mut out_ptr = self.out.as_mut_ptr() as *mut c_void;
        let mut out_id = OUT_BITSTREAM_DATA;
        let mut out_size = self.out.len() as i32;
        let mut out_el = 1i32;
        let out_desc = BufDesc {
            num_bufs: 1,
            bufs: &mut out_ptr,
            buffer_identifiers: &mut out_id,
            buf_sizes: &mut out_size,
            buf_el_sizes: &mut out_el,
        };
        let in_args = InArgs {
            num_in_samples: pcm.len() as i32,
            num_anc_bytes: 0,
        };
        let mut out_args = OutArgs::default();
        // SAFETY: The descriptors point at locals that outlive the call. The input buffer
        // covers all of `pcm` (length checked above) and the fdk encoder only reads it. The
        // output buffer covers `self.out`, whose length is passed as the size.
        let e = unsafe { (self.api.encode)(self.h, &in_desc, &out_desc, &in_args, &mut out_args) };
        if e != 0 {
            bail!("aacEncEncode failed: {e:#x}");
        }
        Ok((out_args.num_out_bytes > 0)
            .then(|| self.out[..out_args.num_out_bytes as usize].to_vec()))
    }
}

impl Drop for AacEldEncoder {
    fn drop(&mut self) {
        // SAFETY: `self.h` is the live handle from aacEncOpen and is closed only here.
        unsafe { (self.api.close)(&mut self.h) };
    }
}
