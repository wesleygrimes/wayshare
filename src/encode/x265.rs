//! Software backend: one libx265 encoder (loaded with dlopen) over the tiled sequence, 4:2:0
//! Main or 4:4:4 Main 4:4:4 8-bit, zero-latency tuning. Reads CPU frames, so capture with
//! `Memory::System`.
//!
//! x265 manages its own references, so every picture may reference any of the last four. To
//! keep the same-tile picture among them, all four tiles are coded whenever any changed.

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

use super::convert::{Coeffs, Planes, bgrx_to_yuv};
use super::plan::{PicKind, TilePic};
use super::tiles::{Backend, Tiles};
use super::{
    Chroma, EncodedFrame, EncodedTile, EncoderConfig, Primaries, TILE_COUNT, TileLayout, Transfer,
    VideoEncoder, nal,
};
use crate::capture::Frame;

pub struct X265Encoder {
    tiles: Tiles<Session>,
}

// SAFETY: The x265 handles are owned only by `Session` and used only through `&mut self`, and
// the tile differ's EGL context is made current only for the duration of each call, so the
// encoder is never used from two threads at once.
unsafe impl Send for X265Encoder {}

impl X265Encoder {
    pub fn new(width: u32, height: u32, cfg: &EncoderConfig) -> Result<Self> {
        let layout = TileLayout::new(width, height);
        let session = Session::new(layout, cfg)?;
        // A bitrate change restarts the sequence (IDR), so keep them rare.
        Ok(Self {
            tiles: Tiles::new(session, layout, cfg, false, Duration::from_secs(1))?,
        })
    }
}

impl VideoEncoder for X265Encoder {
    type Frame = Frame;

    fn layout(&self) -> TileLayout {
        self.tiles.layout()
    }

    fn request_keyframe(&mut self, tile: Option<usize>) {
        self.tiles.request_keyframe(tile);
    }

    fn set_bitrate(&mut self, bps: u64) {
        self.tiles.set_bitrate(bps);
    }

    fn submit(&mut self, frame: &Frame) -> Result<()> {
        self.tiles.submit(frame)
    }

    fn receive(&mut self, timeout: Option<Duration>) -> Result<Option<EncodedFrame>> {
        self.tiles.receive(timeout)
    }

    fn pending(&self) -> usize {
        self.tiles.pending()
    }

    fn can_submit(&mut self) -> bool {
        self.tiles.can_submit()
    }

    fn wants_frame(&self) -> bool {
        self.tiles.wants_frame()
    }

    fn set_output(&mut self, tx: std::sync::mpsc::Sender<Result<EncodedFrame>>) {
        self.tiles.set_output(tx);
    }
}

struct TilePlanes {
    planes: [Vec<u8>; 3],
}

struct Session {
    layout: TileLayout,
    cfg: EncoderConfig,
    api: &'static Api,
    param: *mut c_void,
    enc: *mut c_void,
    pic: *mut u8,
    bufs: Vec<TilePlanes>,
    strides: [usize; 3],
    coeffs: Coeffs,
    pts: i64,
    reopen: bool,
    done: std::collections::VecDeque<EncodedFrame>,
}

impl Session {
    fn new(layout: TileLayout, cfg: &EncoderConfig) -> Result<Self> {
        let api = api().map_err(|e| anyhow!("{e}"))?;
        let (w, th) = (layout.width as usize, layout.tile_height as usize);
        let (cw, ch) = match cfg.chroma {
            Chroma::Yuv444 => (w, th),
            Chroma::Yuv420 => (w.div_ceil(2), th.div_ceil(2)),
        };
        let strides = [
            w.next_multiple_of(64),
            cw.next_multiple_of(64),
            cw.next_multiple_of(64),
        ];
        let bufs = (0..TILE_COUNT)
            .map(|_| TilePlanes {
                planes: [
                    vec![0; strides[0] * th],
                    vec![128; strides[1] * ch],
                    vec![128; strides[2] * ch],
                ],
            })
            .collect();
        let mut s = Self {
            layout,
            cfg: cfg.clone(),
            api,
            param: ptr::null_mut(),
            enc: ptr::null_mut(),
            pic: ptr::null_mut(),
            bufs,
            strides,
            coeffs: Coeffs::bt709(cfg.color.full_range),
            pts: 0,
            reopen: false,
            done: Default::default(),
        };
        s.open()?;
        Ok(s)
    }

    fn close(&mut self) {
        let api = self.api;
        // SAFETY: Each handle is non-null here, came from the matching alloc/open function, and
        // is nulled right after, so it is freed once.
        unsafe {
            if !self.enc.is_null() {
                (api.encoder_close)(self.enc);
            }
            if !self.pic.is_null() {
                (api.picture_free)(self.pic);
            }
            if !self.param.is_null() {
                (api.param_free)(self.param);
            }
        }
        (self.enc, self.pic, self.param) = (ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
    }

    fn open(&mut self) -> Result<()> {
        self.close();
        let (api, cfg) = (self.api, &self.cfg);
        // SAFETY: x265_param_alloc has no preconditions, and null is checked on the next line.
        self.param = unsafe { (api.param_alloc)() };
        if self.param.is_null() {
            bail!("x265_param_alloc failed");
        }
        let preset = CString::new(cfg.x265.preset.as_str())?;
        // SAFETY: `self.param` is non-null (checked above), and the preset and tune strings are
        // NUL-terminated.
        if unsafe {
            (api.param_default_preset)(self.param, preset.as_ptr(), c"zerolatency".as_ptr())
        } < 0
        {
            bail!("x265: unknown preset {}", cfg.x265.preset);
        }
        let threads = cfg.x265.threads_per_tile.map_or_else(
            || std::thread::available_parallelism().map_or(4, |n| n.get() as u32),
            |t| t * TILE_COUNT as u32,
        );
        let color = cfg.color;
        let mut params: Vec<(&str, String)> = vec![
            (
                "input-res",
                format!("{}x{}", self.layout.width, self.layout.tile_height),
            ),
            ("fps", (cfg.fps * TILE_COUNT as u32).to_string()),
            (
                "input-csp",
                if cfg.chroma == Chroma::Yuv444 {
                    "i444"
                } else {
                    "i420"
                }
                .into(),
            ),
            ("pools", threads.to_string()),
            ("frame-threads", "1".into()),
            ("wpp", "1".into()),
            ("bframes", "0".into()),
            ("rc-lookahead", "0".into()),
            ("lookahead-slices", "0".into()),
            // Each tile predicts from its own previous picture, four pictures back.
            ("ref", TILE_COUNT.to_string()),
            ("keyint", "-1".into()),
            // Forced I pictures inside min-keyint stay non-IDR TRAIL_R (see notes).
            ("min-keyint", "1000000".into()),
            ("scenecut", "0".into()),
            ("open-gop", "0".into()),
            ("repeat-headers", "1".into()),
            ("annexb", "1".into()),
            ("aud", "0".into()),
            ("info", "0".into()),
            ("hash", "0".into()),
            ("log-level", "warning".into()),
            (
                "range",
                if color.full_range { "full" } else { "limited" }.into(),
            ),
            ("colormatrix", "bt709".into()),
            (
                "colorprim",
                match color.primaries {
                    Primaries::Bt709 => "bt709",
                    Primaries::DisplayP3 => "smpte432",
                }
                .into(),
            ),
            (
                "transfer",
                match color.transfer {
                    Transfer::Bt709 => "bt709",
                    Transfer::Srgb => "iec61966-2-1",
                }
                .into(),
            ),
        ];
        match cfg.bitrate_bps {
            Some(total) => {
                let kbps = (total / 1000).max(1);
                params.push(("bitrate", kbps.to_string()));
                params.push(("vbv-maxrate", (kbps * 3 / 2).to_string()));
                params.push(("vbv-bufsize", (kbps / 4).max(1).to_string()));
            }
            None => params.push(("qp", cfg.qp.to_string())),
        }
        for (k, v) in &params {
            let (kc, vc) = (CString::new(*k)?, CString::new(v.as_str())?);
            // SAFETY: `self.param` is non-null, and `kc` and `vc` are live NUL-terminated
            // strings.
            let r = unsafe { (api.param_parse)(self.param, kc.as_ptr(), vc.as_ptr()) };
            if r != 0 {
                bail!("x265: bad parameter {k}={v} ({r})");
            }
        }
        let profile = if cfg.chroma == Chroma::Yuv444 {
            c"main444-8"
        } else {
            c"main"
        };
        // SAFETY: `self.param` is non-null, and `profile` is a NUL-terminated literal.
        if unsafe { (api.param_apply_profile)(self.param, profile.as_ptr()) } < 0 {
            bail!("x265: profile {profile:?} rejected");
        }
        // SAFETY: `self.param` is non-null and configured above. The encoder handle is null-
        // checked before use, and the picture is null-checked before picture_init. On failure the
        // handles stay in `self` for `close` to free.
        unsafe {
            self.enc = (api.encoder_open)(self.param);
            if self.enc.is_null() {
                bail!("x265_encoder_open failed");
            }
            self.pic = (api.picture_alloc)();
            if self.pic.is_null() {
                bail!("x265_picture_alloc failed");
            }
            (api.picture_init)(self.param, self.pic);
        }
        Ok(())
    }

    /// Converts the requested tiles in parallel, one thread per tile.
    fn convert(&mut self, frame: &Frame, pics: &[TilePic]) -> Result<()> {
        frame.buffer.with_read(|guard| {
            let (src, stride) = (guard.bytes(), guard.stride);
            let (l, chroma, k, strides) = (self.layout, self.cfg.chroma, self.coeffs, self.strides);
            std::thread::scope(|s| {
                for (t, buf) in self.bufs.iter_mut().enumerate() {
                    if !pics.iter().any(|p| p.tile == t) {
                        continue;
                    }
                    s.spawn(move || {
                        let (w, rows, th) = (
                            l.width as usize,
                            l.tile_rows(t) as usize,
                            l.tile_height as usize,
                        );
                        let [y, u, v] = &mut buf.planes;
                        bgrx_to_yuv(
                            &src[l.tile_y(t) as usize * stride..],
                            stride,
                            w,
                            rows,
                            chroma,
                            &k,
                            Planes {
                                y,
                                u,
                                v,
                                y_stride: strides[0],
                                c_stride: strides[1],
                            },
                        );
                        if rows < th {
                            let (crows, cth) = match chroma {
                                Chroma::Yuv444 => (rows, th),
                                Chroma::Yuv420 => (rows.div_ceil(2), th.div_ceil(2)),
                            };
                            for (i, plane) in buf.planes.iter_mut().enumerate() {
                                let (valid, total) = if i == 0 { (rows, th) } else { (crows, cth) };
                                let s = strides[i];
                                for y in valid..total {
                                    plane.copy_within((valid - 1) * s..valid * s, y * s);
                                }
                            }
                        }
                    });
                }
            });
        })
    }

    fn encode_pic(&mut self, p: TilePic) -> Result<EncodedTile> {
        if self.enc.is_null() || self.pic.is_null() {
            bail!("x265 encoder is not open");
        }
        // SAFETY: `self.enc` and `self.pic` are non-null (checked above) and stay valid until
        // `close`.
        unsafe {
            let pic = self.pic;
            (pic.add(PIC_PTS) as *mut i64).write(self.pts);
            self.pts += 1;
            let planes = pic.add(PIC_PLANES) as *mut *mut u8;
            let strides = pic.add(PIC_STRIDE) as *mut c_int;
            for i in 0..3 {
                planes
                    .add(i)
                    .write(self.bufs[p.tile].planes[i].as_mut_ptr());
                strides.add(i).write(self.strides[i] as c_int);
            }
            let ty = match p.kind {
                PicKind::Idr => X265_TYPE_IDR,
                PicKind::Intra => X265_TYPE_I,
                PicKind::Inter => X265_TYPE_P,
            };
            (pic.add(PIC_SLICE_TYPE) as *mut c_int).write(ty);

            let mut nals: *mut Nal = ptr::null_mut();
            let mut n: u32 = 0;
            let r = (self.api.encoder_encode)(self.enc, &mut nals, &mut n, pic, ptr::null_mut());
            if r < 0 {
                bail!("x265_encoder_encode failed");
            }
            if r == 0 || n == 0 {
                bail!("x265 buffered a picture (latency settings not applied?)");
            }
            let mut data = Vec::new();
            for nal in std::slice::from_raw_parts(nals, n as usize) {
                data.extend_from_slice(std::slice::from_raw_parts(nal.payload, nal.size as usize));
            }
            Ok(EncodedTile {
                index: p.tile as u8,
                keyframe: p.kind != PicKind::Inter,
                data: nal::strip_sei(&data),
            })
        }
    }
}

impl Backend for Session {
    /// Encodes synchronously; `receive` hands the results back in order.
    fn submit(&mut self, frame: &Frame, pics: &[TilePic], mut meta: EncodedFrame) -> Result<()> {
        if self.reopen {
            self.open()?;
            self.reopen = false;
        }
        let t0 = std::time::Instant::now();
        self.convert(frame, pics)?;
        let t1 = std::time::Instant::now();
        meta.timings.convert = t1 - t0;
        meta.tiles = pics
            .iter()
            .map(|&p| self.encode_pic(p))
            .collect::<Result<Vec<_>>>()?;
        meta.timings.encode = t1.elapsed();
        self.done.push_back(meta);
        Ok(())
    }

    fn receive(&mut self, _timeout: Option<Duration>) -> Result<Option<EncodedFrame>> {
        Ok(self.done.pop_front())
    }

    /// `x265_encoder_reconfig` accepted rate changes but they didn't take effect (see notes),
    /// so the encoder is reopened, which starts a new sequence.
    fn set_bitrate(&mut self, bps: u64) -> bool {
        self.cfg.bitrate_bps = Some(bps);
        self.reopen = true;
        true
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
    }
}

// x265 4.x API (X265_BUILD 217): picture field offsets checked against the headers on the VM.
const X265_BUILD: c_int = 217;
const SIZEOF_PICTURE: c_int = 16904;
const PIC_PTS: usize = 0;
const PIC_PLANES: usize = 32;
const PIC_STRIDE: usize = 64;
const PIC_SLICE_TYPE: usize = 84;
const X265_TYPE_IDR: c_int = 1;
const X265_TYPE_I: c_int = 2;
const X265_TYPE_P: c_int = 3;

#[repr(C)]
struct Nal {
    ty: u32,
    size: u32,
    payload: *const u8,
}

type P = *mut c_void;

#[repr(C)]
struct Api {
    api_major_version: c_int,
    api_build_number: c_int,
    sizeof_param: c_int,
    sizeof_picture: c_int,
    sizeof_analysis_data: c_int,
    sizeof_zone: c_int,
    sizeof_stats: c_int,
    bit_depth: c_int,
    version_str: *const c_char,
    build_info_str: *const c_char,
    param_alloc: unsafe extern "C" fn() -> P,
    param_free: unsafe extern "C" fn(P),
    param_default: unsafe extern "C" fn(P),
    param_parse: unsafe extern "C" fn(P, *const c_char, *const c_char) -> c_int,
    scenecut_aware_qp_param_parse: unsafe extern "C" fn(P, *const c_char, *const c_char) -> c_int,
    param_apply_profile: unsafe extern "C" fn(P, *const c_char) -> c_int,
    param_default_preset: unsafe extern "C" fn(P, *const c_char, *const c_char) -> c_int,
    picture_alloc: unsafe extern "C" fn() -> *mut u8,
    picture_free: unsafe extern "C" fn(*mut u8),
    picture_init: unsafe extern "C" fn(P, *mut u8),
    encoder_open: unsafe extern "C" fn(P) -> P,
    encoder_parameters: unsafe extern "C" fn(P, P),
    encoder_reconfig: unsafe extern "C" fn(P, P) -> c_int,
    encoder_reconfig_zone: unsafe extern "C" fn(P, P) -> c_int,
    encoder_headers: unsafe extern "C" fn(P, *mut *mut Nal, *mut u32) -> c_int,
    configure_vbv_end: unsafe extern "C" fn(P, *mut u8, f64),
    encoder_encode: unsafe extern "C" fn(P, *mut *mut Nal, *mut u32, *mut u8, *mut u8) -> c_int,
    encoder_get_stats: unsafe extern "C" fn(P, P, u32),
    encoder_log: unsafe extern "C" fn(P, c_int, *mut *mut c_char),
    encoder_close: unsafe extern "C" fn(P),
}

const _: () = assert!(std::mem::offset_of!(Api, param_alloc) == 48);
const _: () = assert!(std::mem::offset_of!(Api, encoder_encode) == 176);
const _: () = assert!(std::mem::offset_of!(Api, encoder_close) == 200);

// SAFETY: The table is plain function pointers, integers and static strings returned by
// x265_api_query. The library is never dlclosed, so it lives for the rest of the process.
unsafe impl Sync for Api {}

type ApiQuery =
    unsafe extern "C" fn(bit_depth: c_int, api_version: c_int, err: *mut c_int) -> *const Api;

fn api() -> Result<&'static Api, String> {
    static API: OnceLock<Result<usize, String>> = OnceLock::new();
    let addr = API.get_or_init(|| {
        let handle = ["libx265.so", "libx265.so.217"].iter().find_map(|name| {
            let c = CString::new(*name).unwrap();
            // SAFETY: The name is NUL-terminated, and a null result is skipped.
            let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
            (!h.is_null()).then_some(h)
        });
        let handle = handle.ok_or("libx265 not found")?;
        // SAFETY: `handle` is a live, non-null dlopen handle and the name is a NUL-terminated
        // literal.
        let query = unsafe { libc::dlsym(handle, c"x265_api_query".as_ptr()) };
        if query.is_null() {
            return Err("libx265 has no x265_api_query".into());
        }
        // SAFETY: `query` is non-null (checked above) and is x265_api_query, whose C signature
        // is `ApiQuery`.
        let query: ApiQuery = unsafe { std::mem::transmute(query) };
        let mut err = 0;
        // SAFETY: `err` is a valid out pointer, and the result is null-checked below.
        let a = unsafe { query(8, X265_BUILD, &mut err) };
        if a.is_null() {
            return Err(format!(
                "libx265 doesn't provide 8-bit API build {X265_BUILD} (error {err})"
            ));
        }
        // SAFETY: `a` is non-null (checked above) and points at the library's static API table.
        let ap = unsafe { &*a };
        if ap.api_build_number != X265_BUILD || ap.sizeof_picture != SIZEOF_PICTURE {
            return Err(format!(
                "libx265 build {} (picture {} bytes) doesn't match the bindings ({X265_BUILD})",
                ap.api_build_number, ap.sizeof_picture
            ));
        }
        Ok(a as usize)
    });
    // SAFETY: `a` is the non-null table pointer validated in the initialiser, and the library
    // handle is never closed, so the table lives for the rest of the process.
    addr.clone().map(|a| unsafe { &*(a as *const Api) })
}

/// Why libx265 can't be used, if it can't.
pub fn check() -> Result<(), String> {
    api().map(|_| ())
}
