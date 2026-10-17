//! VAAPI backend driving libva directly: dmabuf import, VPP crop + RGB -> NV12 per tile, then
//! HEVC encode of the tiled sequence with our own parameter sets and slice headers (packed
//! headers; radeonsi parses them and re-emits equivalent ones) and explicit references.

#[rustfmt::skip]
mod sys;

use std::ffi::{CStr, c_int, c_void};
use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::ptr;
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use super::hevc::{self, RpsEntry, SeqConfig, SliceHeader, SliceType};
use super::plan::{PicKind, TilePic};
use super::tiles::{Backend, Tiles};
use super::{EncodedFrame, EncodedTile, EncoderConfig, TILE_COUNT, TileLayout, VideoEncoder, nal};
use crate::capture::{DmabufBuffer, Frame};
use sys::*;

pub struct VaapiEncoder {
    tiles: Tiles<Session>,
}

// SAFETY: Every VA object belongs to the `Session` inside and is used only through `&mut self`.
// The collector thread only gets the `Dpy` and coded buffer ids (see the `Dpy` impl), and the
// fused tile differ's EGL context is current only during each call.
unsafe impl Send for VaapiEncoder {}

impl VaapiEncoder {
    pub fn new(render_node: &Path, width: u32, height: u32, cfg: &EncoderConfig) -> Result<Self> {
        if cfg.chroma != super::Chroma::Yuv420 {
            bail!("VAAPI HEVC encode here is 4:2:0 only");
        }
        let layout = TileLayout::new(width, height);
        let session = Session::new(render_node, layout, cfg)?;
        // Rate changes go out with the next picture, so they can be frequent.
        Ok(Self {
            tiles: Tiles::new(session, layout, cfg, true, Duration::from_millis(200))?,
        })
    }
}

impl VideoEncoder for VaapiEncoder {
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

    fn set_output(&mut self, tx: mpsc::Sender<Result<EncodedFrame>>) {
        self.tiles.set_output(tx);
    }
}

/// Whether this render node can encode HEVC Main through VAAPI.
pub fn hevc_encode_available(render_node: &Path) -> bool {
    Display::open(render_node).is_ok_and(|d| d.hevc_entrypoint().is_some())
}

struct Display {
    dpy: VADisplay,
    _fd: File,
}

impl Display {
    fn open(node: &Path) -> Result<Self> {
        load()?;
        let fd = OpenOptions::new()
            .read(true)
            .write(true)
            .open(node)
            .with_context(|| format!("open {}", node.display()))?;
        // SAFETY: `fd` is an open DRM node that `Display` keeps alive for as long as the
        // display.
        let dpy = unsafe { va().vaGetDisplayDRM(fd.as_raw_fd()) };
        if dpy.is_null() {
            bail!("vaGetDisplayDRM failed");
        }
        let (mut maj, mut min) = (0, 0);
        check(
            // SAFETY: `dpy` is non-null (checked above), and `maj` and `min` are valid out
            // pointers.
            unsafe { va().vaInitialize(dpy, &mut maj, &mut min) },
            "vaInitialize",
        )?;
        Ok(Self { dpy, _fd: fd })
    }

    fn hevc_entrypoint(&self) -> Option<VAEntrypoint> {
        let mut eps = [0 as c_int; 32];
        let mut n = 0;
        let r = unsafe {
            va().vaQueryConfigEntrypoints(
                self.dpy,
                VAProfile::VAProfileHEVCMain,
                eps.as_mut_ptr().cast(),
                &mut n,
            )
        };
        let eps = &eps[..n.clamp(0, 32) as usize];
        if r != 0 {
            None
        } else if eps.contains(&(VAEntrypoint::VAEntrypointEncSlice as c_int)) {
            Some(VAEntrypoint::VAEntrypointEncSlice)
        } else if eps.contains(&(VAEntrypoint::VAEntrypointEncSliceLP as c_int)) {
            Some(VAEntrypoint::VAEntrypointEncSliceLP)
        } else {
            None
        }
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // SAFETY: `dpy` was initialised in `Display::open` and is terminated only here.
        unsafe { va().vaTerminate(self.dpy) };
    }
}

#[derive(Clone, Copy)]
struct Ref {
    surface: VASurfaceID,
    poc: u32,
}

const RECON_SURFACES: usize = 8;
/// Frames whose pictures may be queued on the VCN at once (input surfaces and coded buffers
/// are double-buffered by frame).
const FLIGHT: usize = 2;

const MAX_IMPORTS: usize = 8;

struct Session {
    layout: TileLayout,
    seq: SeqConfig,
    cfg: EncoderConfig,
    rc_mode: u32,
    vpp_config: VAConfigID,
    vpp_ctx: VAContextID,
    enc_config: VAConfigID,
    enc_ctx: VAContextID,
    /// NV12 VPP outputs, `[slot * TILE_COUNT + tile]`.
    inputs: [VASurfaceID; FLIGHT * TILE_COUNT],
    recon: [VASurfaceID; RECON_SURFACES],
    /// One per picture, `[slot * TILE_COUNT + i]`.
    coded: [VABufferID; FLIGHT * TILE_COUNT],
    free_slots: Vec<usize>,
    collector: Option<Collector>,
    /// Fused change detection + conversion into `inputs` (None: VPP path).
    fused: Option<crate::capture::TileDiffer>,
    /// Slot already filled by `detect` for the next `submit`.
    prepared: Option<usize>,
    /// dmabufs exported from `inputs` for the fused pass.
    _exported: Vec<std::os::fd::OwnedFd>,
    /// Latest coded picture of each tile (the references the RPS keeps alive).
    latest: [Option<Ref>; TILE_COUNT],
    poc: u32,
    send_rate: bool,
    trim: super::rate::RateTrim,
    imports: Vec<(u64, VASurfaceID)>,
    // Last so the display outlives every object above.
    display: Display,
}

impl Session {
    fn new(node: &Path, layout: TileLayout, cfg: &EncoderConfig) -> Result<Self> {
        let display = Display::open(node)?;
        let dpy = display.dpy;
        let entrypoint = display
            .hevc_entrypoint()
            .context("no VAAPI HEVC encode entrypoint")?;
        let seq = SeqConfig {
            width: layout.width,
            height: layout.tile_height,
            pic_rate: cfg.fps * TILE_COUNT as u32,
            color: cfg.color,
            cu_qp_delta: cfg.bitrate_bps.is_some(),
            sao: cfg.sao,
        };
        let rc_mode = if cfg.bitrate_bps.is_some() {
            VA_RC_VBR
        } else {
            VA_RC_CQP
        };

        let mut attribs = [
            VAConfigAttrib {
                type_: VAConfigAttribType::VAConfigAttribRTFormat,
                value: 0,
            },
            VAConfigAttrib {
                type_: VAConfigAttribType::VAConfigAttribRateControl,
                value: 0,
            },
            VAConfigAttrib {
                type_: VAConfigAttribType::VAConfigAttribEncPackedHeaders,
                value: 0,
            },
        ];
        check(
            // SAFETY: `dpy` is live, and `attribs` holds the 3 entries the count refers to.
            unsafe {
                va().vaGetConfigAttributes(
                    dpy,
                    VAProfile::VAProfileHEVCMain,
                    entrypoint,
                    attribs.as_mut_ptr(),
                    3,
                )
            },
            "vaGetConfigAttributes",
        )?;
        let packed = VA_ENC_PACKED_HEADER_SEQUENCE | VA_ENC_PACKED_HEADER_SLICE;
        if attribs[0].value & VA_RT_FORMAT_YUV420 == 0
            || attribs[1].value & rc_mode == 0
            || attribs[2].value & packed != packed
        {
            bail!(
                "driver lacks HEVC encode features: rt {:#x} rc {:#x} packed {:#x}",
                attribs[0].value,
                attribs[1].value,
                attribs[2].value
            );
        }
        attribs[0].value = VA_RT_FORMAT_YUV420;
        attribs[1].value = rc_mode;
        attribs[2].value = packed;

        let mut s = Self {
            layout,
            seq,
            cfg: cfg.clone(),
            rc_mode,
            vpp_config: VA_INVALID_ID,
            vpp_ctx: VA_INVALID_ID,
            enc_config: VA_INVALID_ID,
            enc_ctx: VA_INVALID_ID,
            inputs: [VA_INVALID_SURFACE; FLIGHT * TILE_COUNT],
            recon: [VA_INVALID_SURFACE; RECON_SURFACES],
            coded: [VA_INVALID_ID; FLIGHT * TILE_COUNT],
            free_slots: (0..FLIGHT).rev().collect(),
            collector: None,
            fused: None,
            prepared: None,
            _exported: Vec::new(),
            latest: [None; TILE_COUNT],
            poc: 0,
            send_rate: true,
            trim: super::rate::RateTrim::new(cfg.bitrate_bps.unwrap_or(0)),
            imports: Vec::new(),
            display,
        };
        let (cw, ch) = (s.seq.coded_width(), s.seq.coded_height());
        // SAFETY: `dpy` is live. Each pointer argument refers to a field or local of `s` whose
        // length matches the count passed, and the ids written into `s` are destroyed in
        // `Session::drop`, including on the early returns below.
        unsafe {
            check(
                va().vaCreateConfig(
                    dpy,
                    VAProfile::VAProfileHEVCMain,
                    entrypoint,
                    attribs.as_mut_ptr(),
                    3,
                    &mut s.enc_config,
                ),
                "va().vaCreateConfig(HEVC encode)",
            )?;
            check(
                va().vaCreateSurfaces(
                    dpy,
                    VA_RT_FORMAT_YUV420,
                    cw,
                    ch,
                    s.inputs.as_mut_ptr(),
                    s.inputs.len() as u32,
                    ptr::null_mut(),
                    0,
                ),
                "create input surfaces",
            )?;
            check(
                va().vaCreateSurfaces(
                    dpy,
                    VA_RT_FORMAT_YUV420,
                    cw,
                    ch,
                    s.recon.as_mut_ptr(),
                    RECON_SURFACES as u32,
                    ptr::null_mut(),
                    0,
                ),
                "create reconstructed surfaces",
            )?;
            let mut targets: Vec<VASurfaceID> = s.inputs.iter().chain(&s.recon).copied().collect();
            check(
                va().vaCreateContext(
                    dpy,
                    s.enc_config,
                    cw as c_int,
                    ch as c_int,
                    VA_PROGRESSIVE as c_int,
                    targets.as_mut_ptr(),
                    targets.len() as c_int,
                    &mut s.enc_ctx,
                ),
                "va().vaCreateContext(HEVC encode)",
            )?;
            let coded_size = cw * ch * 3 / 2 + (1 << 16);
            for c in &mut s.coded {
                check(
                    va().vaCreateBuffer(
                        dpy,
                        s.enc_ctx,
                        VABufferType::VAEncCodedBufferType,
                        coded_size,
                        1,
                        ptr::null_mut(),
                        c,
                    ),
                    "create coded buffer",
                )?;
            }
            check(
                va().vaCreateConfig(
                    dpy,
                    VAProfile::VAProfileNone,
                    VAEntrypoint::VAEntrypointVideoProc,
                    ptr::null_mut(),
                    0,
                    &mut s.vpp_config,
                ),
                "va().vaCreateConfig(VideoProc)",
            )?;
            check(
                va().vaCreateContext(
                    dpy,
                    s.vpp_config,
                    cw as c_int,
                    ch as c_int,
                    0,
                    ptr::null_mut(),
                    0,
                    &mut s.vpp_ctx,
                ),
                "va().vaCreateContext(VideoProc)",
            )?;
        }
        s.collector = Some(Collector::spawn(Dpy(dpy))?);
        if cfg.skip_unchanged {
            match s.build_fused() {
                Ok(f) => s.fused = Some(f),
                Err(e) => tracing::warn!("fused diff+convert unavailable, using VPP: {e:#}"),
            }
        }
        Ok(s)
    }

    fn dpy(&self) -> VADisplay {
        self.display.dpy
    }

    fn import(&mut self, buf: &DmabufBuffer) -> Result<VASurfaceID> {
        if let Some(&(_, s)) = self.imports.iter().find(|(id, _)| *id == buf.id) {
            return Ok(s);
        }
        // SAFETY: `self.dpy()` is live, and `buf` is borrowed, so its plane fds stay open for the
        // call.
        let surface = unsafe { import_dmabuf(self.dpy(), buf)? };
        if self.imports.len() >= MAX_IMPORTS {
            let (_, mut old) = self.imports.remove(0);
            // SAFETY: `old` is a surface this session created and just removed from `imports`,
            // so it is destroyed once.
            unsafe { va().vaDestroySurfaces(self.dpy(), &mut old, 1) };
        }
        self.imports.push((buf.id, surface));
        Ok(surface)
    }

    /// Crops tile `t` out of `src` into its NV12 input surface (padding rows repeat the last row).
    fn convert(&self, src: VASurfaceID, t: usize, dst: VASurfaceID) -> Result<()> {
        let l = &self.layout;
        let (y, rows, w) = (l.tile_y(t), l.tile_rows(t), l.width);
        self.vpp(src, rect(y, rows, w), dst, rect(0, rows, w))?;
        if rows < l.tile_height {
            self.vpp(
                src,
                rect(y + rows - 1, 1, w),
                dst,
                rect(rows, l.tile_height - rows, w),
            )?;
        }
        Ok(())
    }

    fn vpp(
        &self,
        src: VASurfaceID,
        src_rect: VARectangle,
        dst: VASurfaceID,
        dst_rect: VARectangle,
    ) -> Result<()> {
        // SAFETY: `VAProcPipelineParameterBuffer` is a plain C struct of integers and pointers,
        // so all-zero is a valid value.
        let mut p: VAProcPipelineParameterBuffer = unsafe { std::mem::zeroed() };
        p.surface = src;
        p.surface_region = &src_rect;
        p.surface_color_standard = _VAProcColorStandardType_VAProcColorStandardSRGB;
        p.output_region = &dst_rect;
        p.output_background_color = 0xff00_0000;
        p.output_color_standard = _VAProcColorStandardType_VAProcColorStandardBT709;
        p.input_color_properties.color_range = VA_SOURCE_RANGE_FULL as u8;
        p.output_color_properties.color_range = if self.cfg.color.full_range {
            VA_SOURCE_RANGE_FULL
        } else {
            VA_SOURCE_RANGE_REDUCED
        } as u8;
        p.output_color_properties.matrix_coefficients = 1;
        let buf = self.buffer(
            self.vpp_ctx,
            VABufferType::VAProcPipelineParameterBufferType,
            &mut p,
        )?;
        let r = self.picture(self.vpp_ctx, dst, &[buf]);
        // SAFETY: `buf` was created by `self.buffer` just above and is destroyed once.
        unsafe { va().vaDestroyBuffer(self.dpy(), buf) };
        r
    }

    fn buffer<T>(&self, ctx: VAContextID, ty: VABufferType, data: &mut T) -> Result<VABufferID> {
        self.raw_buffer(ctx, ty, (data as *mut T).cast(), size_of::<T>())
    }

    fn raw_buffer(
        &self,
        ctx: VAContextID,
        ty: VABufferType,
        data: *mut c_void,
        size: usize,
    ) -> Result<VABufferID> {
        let mut id = VA_INVALID_ID;
        check(
            // SAFETY: `data` points at `size` readable bytes (callers pass `&mut T` with
            // `size_of::<T>()`, or a byte slice), and VA copies them during the call. `id` is a
            // valid out pointer.
            unsafe { va().vaCreateBuffer(self.dpy(), ctx, ty, size as u32, 1, data, &mut id) },
            "vaCreateBuffer",
        )?;
        Ok(id)
    }

    fn picture(&self, ctx: VAContextID, target: VASurfaceID, bufs: &[VABufferID]) -> Result<()> {
        let dpy = self.dpy();
        // SAFETY: `dpy` and `ctx` are live, and the buffer ids come from `vaCreateBuffer` on
        // that context. `b` is a local copy whose length is passed.
        unsafe {
            check(va().vaBeginPicture(dpy, ctx, target), "vaBeginPicture")?;
            let mut b = bufs.to_vec();
            let r = check(
                va().vaRenderPicture(dpy, ctx, b.as_mut_ptr(), b.len() as c_int),
                "vaRenderPicture",
            );
            let e = check(va().vaEndPicture(dpy, ctx), "vaEndPicture");
            r.and(e)
        }
    }

    fn misc<T>(&self, ty: VAEncMiscParameterType, payload: T) -> Result<VABufferID> {
        #[repr(C)]
        struct Misc<T> {
            ty: u32,
            payload: T,
        }
        let mut m = Misc {
            ty: ty as u32,
            payload,
        };
        self.buffer(
            self.enc_ctx,
            VABufferType::VAEncMiscParameterBufferType,
            &mut m,
        )
    }

    fn packed(&self, ty: u32, nals: &[u8]) -> Result<[VABufferID; 2]> {
        let mut param = VAEncPackedHeaderParameterBuffer {
            type_: ty,
            bit_length: nals.len() as u32 * 8,
            has_emulation_bytes: 1,
            va_reserved: [0; 4],
        };
        let p = self.buffer(
            self.enc_ctx,
            VABufferType::VAEncPackedHeaderParameterBufferType,
            &mut param,
        )?;
        let d = self.raw_buffer(
            self.enc_ctx,
            VABufferType::VAEncPackedHeaderDataBufferType,
            nals.as_ptr() as *mut c_void,
            nals.len(),
        )?;
        Ok([p, d])
    }

    fn rate_buffers(&self) -> Result<Vec<VABufferID>> {
        let mut out = Vec::new();
        // SAFETY: `VAEncMiscParameterFrameRate` is a plain C struct of integers, so all-zero is
        // a valid value.
        let mut fr: VAEncMiscParameterFrameRate = unsafe { std::mem::zeroed() };
        fr.framerate = self.seq.pic_rate;
        out.push(self.misc(VAEncMiscParameterType::VAEncMiscParameterTypeFrameRate, fr)?);
        if self.cfg.bitrate_bps.is_some() {
            let total = self.trim.requested();
            let target = total.min(u32::MAX as u64) as u32;
            let max = (total * 3 / 2).min(u32::MAX as u64) as u32;
            // SAFETY: `VAEncMiscParameterRateControl` is a plain C struct of integers, so all-
            // zero is a valid value.
            let mut rc: VAEncMiscParameterRateControl = unsafe { std::mem::zeroed() };
            rc.bits_per_second = max;
            rc.target_percentage = (target as u64 * 100 / max.max(1) as u64) as u32;
            rc.window_size = 1000;
            out.push(self.misc(
                VAEncMiscParameterType::VAEncMiscParameterTypeRateControl,
                rc,
            )?);
            let hrd = VAEncMiscParameterHRD {
                initial_buffer_fullness: max / 2,
                buffer_size: max,
                va_reserved: [0; 4],
            };
            out.push(self.misc(VAEncMiscParameterType::VAEncMiscParameterTypeHRD, hrd)?);
        }
        Ok(out)
    }

    fn seq_params(&self) -> VAEncSequenceParameterBufferHEVC {
        // SAFETY: `VAEncSequenceParameterBufferHEVC` is a plain C struct of integers and bit-
        // field unions, so all-zero is a valid value.
        let mut s: VAEncSequenceParameterBufferHEVC = unsafe { std::mem::zeroed() };
        s.general_profile_idc = 1;
        s.general_level_idc = self.seq.level_idc();
        s.ip_period = 1;
        s.bits_per_second = self.cfg.bitrate_bps.unwrap_or(0).min(u32::MAX as u64) as u32;
        s.pic_width_in_luma_samples = self.seq.coded_width() as u16;
        s.pic_height_in_luma_samples = self.seq.coded_height() as u16;
        // SAFETY: `seq_fields` is a union whose `bits` view covers the whole value, and the
        // struct was zero-initialised.
        let f = unsafe { &mut s.seq_fields.bits };
        f.set_chroma_format_idc(1);
        f.set_amp_enabled_flag(1);
        f.set_sample_adaptive_offset_enabled_flag(self.seq.sao as u32);
        f.set_low_delay_seq(1);
        s.log2_diff_max_min_luma_coding_block_size = 3;
        s.log2_diff_max_min_transform_block_size = 3;
        s.max_transform_hierarchy_depth_inter = 4;
        s.max_transform_hierarchy_depth_intra = 4;
        s.vui_parameters_present_flag = 1;
        // SAFETY: `vui_fields` is a union whose `bits` view covers the whole value, and the
        // struct was zero-initialised.
        let v = unsafe { &mut s.vui_fields.bits };
        v.set_aspect_ratio_info_present_flag(1);
        v.set_vui_timing_info_present_flag(1);
        s.aspect_ratio_idc = 1;
        s.vui_num_units_in_tick = 1;
        s.vui_time_scale = self.seq.pic_rate;
        s
    }

    fn free_recon(&self, taken: &[VASurfaceID]) -> VASurfaceID {
        *self
            .recon
            .iter()
            .find(|s| !self.latest.iter().flatten().any(|r| r.surface == **s) && !taken.contains(s))
            .expect("recon pool larger than live references")
    }

    /// Submits one picture; returns whether it is intra.
    fn submit_pic(
        &mut self,
        pic: TilePic,
        input: VASurfaceID,
        coded: VABufferID,
        taken: &mut Vec<VASurfaceID>,
    ) -> Result<bool> {
        let t = pic.tile;
        let kind = match pic.kind {
            PicKind::Inter if self.latest[t].is_none() => PicKind::Intra,
            k => k,
        };
        if kind == PicKind::Idr {
            self.latest = [None; TILE_COUNT];
            self.poc = 0;
        } else {
            self.poc += 1;
        }
        let poc = self.poc;
        let recon = self.free_recon(taken);
        taken.push(recon);

        // RPS: the latest picture of every tile, nearest first; only this tile's is used.
        let mut keep: Vec<(Ref, bool)> = (0..TILE_COUNT)
            .filter_map(|u| self.latest[u].map(|r| (r, u == t)))
            .filter(|&(_, own)| !(own && kind != PicKind::Inter))
            .collect();
        keep.sort_by_key(|(r, _)| std::cmp::Reverse(r.poc));
        let rps: Vec<RpsEntry> = keep
            .iter()
            .map(|&(r, used)| RpsEntry {
                distance: poc - r.poc,
                used,
            })
            .collect();
        if rps
            .last()
            .is_some_and(|e| e.distance > hevc::MAX_REF_DISTANCE)
        {
            bail!("reference out of short-term range");
        }

        let (nal_type, slice_type) = match kind {
            PicKind::Idr => (hevc::NAL_IDR_W_RADL, SliceType::I),
            PicKind::Intra => (hevc::NAL_TRAIL_R, SliceType::I),
            PicKind::Inter => (hevc::NAL_TRAIL_R, SliceType::P),
        };
        let qp_delta = if self.rc_mode == VA_RC_CQP {
            self.cfg.qp as i32 - 26
        } else {
            0
        };

        let mut bufs = Vec::new();
        let r = (|| -> Result<()> {
            if kind == PicKind::Idr {
                let mut sp = self.seq_params();
                bufs.push(self.buffer(
                    self.enc_ctx,
                    VABufferType::VAEncSequenceParameterBufferType,
                    &mut sp,
                )?);
                self.send_rate = true;
            }
            if std::mem::take(&mut self.send_rate) {
                bufs.extend(self.rate_buffers()?);
            }

            let invalid = VAPictureHEVC {
                picture_id: VA_INVALID_SURFACE,
                pic_order_cnt: 0,
                flags: VA_PICTURE_HEVC_INVALID,
                va_reserved: [0; 4],
            };
            let vapic = |r: Ref| VAPictureHEVC {
                picture_id: r.surface,
                pic_order_cnt: r.poc as i32,
                flags: 0,
                va_reserved: [0; 4],
            };

            // SAFETY: `VAEncPictureParameterBufferHEVC` is a plain C struct of integers and
            // bit-field unions, so all-zero is a valid value.
            let mut pp: VAEncPictureParameterBufferHEVC = unsafe { std::mem::zeroed() };
            pp.decoded_curr_pic = vapic(Ref {
                surface: recon,
                poc,
            });
            pp.reference_frames = [invalid; 15];
            for (i, (r, _)) in keep.iter().enumerate() {
                pp.reference_frames[i] = vapic(*r);
            }
            pp.coded_buf = coded;
            pp.collocated_ref_pic_index = 0xff;
            pp.pic_init_qp = 26;
            pp.nal_unit_type = nal_type;
            // SAFETY: `pic_fields` is a union whose `bits` view covers the whole value, and the
            // struct was zero-initialised.
            let f = unsafe { &mut pp.pic_fields.bits };
            f.set_idr_pic_flag((kind == PicKind::Idr) as u32);
            f.set_coding_type(if slice_type == SliceType::I { 1 } else { 2 });
            f.set_reference_pic_flag(1);
            f.set_dependent_slice_segments_enabled_flag(1);
            f.set_transform_skip_enabled_flag(1);
            f.set_cu_qp_delta_enabled_flag(self.seq.cu_qp_delta as u32);
            f.set_pps_loop_filter_across_slices_enabled_flag(1);
            bufs.push(self.buffer(
                self.enc_ctx,
                VABufferType::VAEncPictureParameterBufferType,
                &mut pp,
            )?);

            // SAFETY: `VAEncSliceParameterBufferHEVC` is a plain C struct of integers and bit-
            // field unions, so all-zero is a valid value.
            let mut sl: VAEncSliceParameterBufferHEVC = unsafe { std::mem::zeroed() };
            sl.num_ctu_in_slice = self.seq.ctus();
            sl.slice_type = slice_type as u8;
            sl.ref_pic_list0 = [invalid; 15];
            sl.ref_pic_list1 = [invalid; 15];
            if kind == PicKind::Inter {
                sl.ref_pic_list0[0] = vapic(self.latest[t].unwrap());
            }
            sl.max_num_merge_cand = 5;
            sl.slice_qp_delta = qp_delta as i8;
            // SAFETY: `slice_fields` is a union whose `bits` view covers the whole value, and
            // the struct was zero-initialised.
            let f = unsafe { &mut sl.slice_fields.bits };
            f.set_last_slice_of_pic_flag(1);
            f.set_slice_sao_luma_flag(self.seq.sao as u32);
            f.set_slice_sao_chroma_flag(self.seq.sao as u32);
            f.set_slice_loop_filter_across_slices_enabled_flag(1);
            bufs.push(self.buffer(
                self.enc_ctx,
                VABufferType::VAEncSliceParameterBufferType,
                &mut sl,
            )?);

            if kind == PicKind::Idr {
                let ps = [self.seq.vps(), self.seq.sps(), self.seq.pps()].concat();
                bufs.extend(self.packed(1, &ps)?); // VAEncPackedHeaderSequence
            }
            let sh = SliceHeader {
                nal_type,
                slice_type,
                poc,
                rps: &rps,
                qp_delta,
                sao: self.seq.sao,
            }
            .write();
            bufs.extend(self.packed(3, &sh)?); // VAEncPackedHeaderSlice

            self.picture(self.enc_ctx, input, &bufs)
        })();
        for b in bufs {
            // SAFETY: Each buffer was created on this context in this call and is destroyed
            // once.
            unsafe { va().vaDestroyBuffer(self.dpy(), b) };
        }
        r?;
        self.latest[t] = Some(Ref {
            surface: recon,
            poc,
        });
        Ok(kind != PicKind::Inter)
    }
}

impl Backend for Session {
    fn detects(&self) -> bool {
        self.fused.is_some()
    }

    fn reset_detection(&mut self) {
        if let Some(f) = &mut self.fused {
            f.reset();
        }
    }

    fn detect(&mut self, frame: &Frame) -> Result<Option<[bool; TILE_COUNT]>> {
        if self.fused.is_none() {
            return Ok(None);
        }
        let slot = match self.prepared.take() {
            Some(s) => s,
            None => self.take_slot()?,
        };
        let result = self
            .fused
            .as_mut()
            .unwrap()
            .diff_convert(&frame.buffer, slot);
        Ok(finish_detection(
            &mut self.fused,
            &mut self.prepared,
            &mut self.free_slots,
            slot,
            result,
        ))
    }

    fn submit(&mut self, frame: &Frame, pics: &[TilePic], mut meta: EncodedFrame) -> Result<()> {
        self.record_sizes();
        let converted = self.prepared.take();
        let slot = match converted {
            Some(s) => s,
            None => self.take_slot()?,
        };
        let r = (|| -> Result<()> {
            let t0 = Instant::now();
            if converted.is_none() {
                let src = self.import(&frame.buffer)?;
                // Convert all tiles first (the GPU queues them back to back), then submit each
                // tile's picture as soon as its input is ready so the VCN starts early.
                for p in pics {
                    self.convert(src, p.tile, self.inputs[slot * TILE_COUNT + p.tile])?;
                }
            }
            let mut taken = Vec::new();
            let mut job = Job {
                slot,
                pics: Vec::new(),
                submitted: Instant::now(),
                meta: None,
            };
            for (i, &p) in pics.iter().enumerate() {
                let (input, coded) = (
                    self.inputs[slot * TILE_COUNT + p.tile],
                    self.coded[slot * TILE_COUNT + i],
                );
                if converted.is_none() {
                    check(
                        // SAFETY: `input` is one of `self.inputs`, which stay alive until
                        // `Drop`.
                        unsafe { va().vaSyncSurface(self.dpy(), input) },
                        "vaSyncSurface",
                    )?;
                }
                if i == 0 {
                    job.submitted = Instant::now();
                    meta.timings.convert = job.submitted - t0;
                }
                let keyframe = self.submit_pic(p, input, coded, &mut taken)?;
                job.pics.push((p.tile as u8, keyframe, coded));
            }
            job.meta = Some(std::mem::replace(
                &mut meta,
                EncodedFrame {
                    seq: 0,
                    changed_tiles: [false; TILE_COUNT],
                    tiles: Vec::new(),
                    timings: Default::default(),
                    presented: None,
                    frame_size: (0, 0),
                },
            ));
            self.collector()?
                .jobs
                .as_ref()
                .unwrap()
                .send(job)
                .map_err(|_| anyhow::anyhow!("encode collector died"))?;
            Ok(())
        })();
        if r.is_err() {
            self.free_slots.push(slot);
        }
        r
    }

    fn receive(&mut self, timeout: Option<Duration>) -> Result<Option<EncodedFrame>> {
        self.record_sizes();
        let c = self.collector()?;
        let r = match timeout {
            None => c.results.recv().ok(),
            Some(t) if t.is_zero() => c.results.try_recv().ok(),
            Some(t) => c.results.recv_timeout(t).ok(),
        };
        r.transpose()
    }

    fn can_submit(&mut self) -> bool {
        self.record_sizes();
        let in_flight = FLIGHT - self.free_slots.len() - self.prepared.is_some() as usize;
        in_flight < (self.cfg.max_in_flight as usize).clamp(1, FLIGHT)
    }

    fn set_output(&mut self, tx: mpsc::Sender<Result<EncodedFrame>>) -> bool {
        if let Some(c) = &self.collector {
            *c.output.lock().unwrap() = Some(tx);
        }
        true
    }

    fn set_bitrate(&mut self, bps: u64) -> bool {
        if self.rc_mode == VA_RC_CQP {
            tracing::warn!("bitrate change ignored in constant-QP mode");
            return false;
        }
        self.cfg.bitrate_bps = Some(bps);
        self.trim.set_target(bps);
        self.send_rate = true;
        false
    }
}

// A compositor can select a layout EGL cannot import (for example, auxiliary compression
// planes). Release the unused conversion slot and let submit import it through VA/VPP.
fn finish_detection<T>(
    fused: &mut Option<T>,
    prepared: &mut Option<usize>,
    free_slots: &mut Vec<usize>,
    slot: usize,
    result: Result<[bool; TILE_COUNT]>,
) -> Option<[bool; TILE_COUNT]> {
    match result {
        Ok(changed) => {
            *prepared = Some(slot);
            Some(changed)
        }
        Err(e) => {
            tracing::warn!("fused diff+convert failed, using VPP: {e:#}");
            *fused = None;
            *prepared = None;
            free_slots.push(slot);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_fused_import_releases_slot_for_vpp() {
        let mut fused = Some(());
        let mut prepared = None;
        let mut free = vec![0];
        let changed = finish_detection(
            &mut fused,
            &mut prepared,
            &mut free,
            1,
            Err(anyhow::anyhow!("diff supports single-plane buffers only")),
        );
        assert!(changed.is_none());
        assert!(fused.is_none());
        // submit sees no converted surface and can take the released slot for VPP.
        assert!(prepared.is_none());
        assert_eq!(free, [0, 1]);
        assert_eq!(free.pop(), Some(1));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.collector.take();
        let dpy = self.dpy();
        // SAFETY: The collector thread was joined above, so nothing else is using these
        // objects. Every id was created by this session and is destroyed once, invalid ids are
        // skipped, and each array pointer is passed with its real length. The display is
        // dropped last (field order).
        unsafe {
            for (_, mut s) in self.imports.drain(..) {
                va().vaDestroySurfaces(dpy, &mut s, 1);
            }
            for &b in &self.coded {
                if b != VA_INVALID_ID {
                    va().vaDestroyBuffer(dpy, b);
                }
            }
            for ctx in [self.enc_ctx, self.vpp_ctx] {
                if ctx != VA_INVALID_ID {
                    va().vaDestroyContext(dpy, ctx);
                }
            }
            let mut surfaces: Vec<VASurfaceID> = self
                .inputs
                .iter()
                .chain(&self.recon)
                .copied()
                .filter(|&s| s != VA_INVALID_SURFACE)
                .collect();
            if !surfaces.is_empty() {
                va().vaDestroySurfaces(dpy, surfaces.as_mut_ptr(), surfaces.len() as c_int);
            }
            for cfg in [self.enc_config, self.vpp_config] {
                if cfg != VA_INVALID_ID {
                    va().vaDestroyConfig(dpy, cfg);
                }
            }
        }
    }
}

impl Session {
    fn take_slot(&mut self) -> Result<usize> {
        self.record_sizes();
        match self.free_slots.pop() {
            Some(s) => Ok(s),
            // Both slots queued: wait for the collector to finish the older frame.
            None => self
                .collector()?
                .free
                .recv()
                .map_err(|_| anyhow::anyhow!("encode collector died")),
        }
    }

    /// Exports every input surface as dmabuf planes for the GL pass to write.
    fn build_fused(&mut self) -> Result<crate::capture::TileDiffer> {
        use crate::capture::{Nv12Target, PlaneDesc};
        use std::os::fd::{FromRawFd, OwnedFd};
        let mut slots = Vec::new();
        for slot in 0..FLIGHT {
            let mut targets = Vec::new();
            for t in 0..TILE_COUNT {
                // SAFETY: `VADRMPRIMESurfaceDescriptor` is a plain C struct of integers and
                // arrays, so all-zero is a valid value.
                let mut d: VADRMPRIMESurfaceDescriptor = unsafe { std::mem::zeroed() };
                check(
                    // SAFETY: The surface is one of `self.inputs`, and `d` is the descriptor
                    // type that VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 requires.
                    unsafe {
                        va().vaExportSurfaceHandle(
                            self.dpy(),
                            self.inputs[slot * TILE_COUNT + t],
                            VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,
                            VA_EXPORT_SURFACE_WRITE_ONLY | VA_EXPORT_SURFACE_SEPARATE_LAYERS,
                            (&mut d as *mut VADRMPRIMESurfaceDescriptor).cast(),
                        )
                    },
                    "vaExportSurfaceHandle",
                )?;
                for o in &d.objects[..d.num_objects as usize] {
                    // SAFETY: vaExportSurfaceHandle gives the caller ownership of each exported
                    // object fd, and each is wrapped once.
                    self._exported.push(unsafe { OwnedFd::from_raw_fd(o.fd) });
                }
                if d.num_layers != 2 {
                    bail!("expected 2 NV12 layers, got {}", d.num_layers);
                }
                let plane = |layer: usize, w: u32, h: u32| {
                    let l = &d.layers[layer];
                    let o = &d.objects[l.object_index[0] as usize];
                    PlaneDesc {
                        fd: o.fd,
                        fourcc: l.drm_format,
                        width: w,
                        height: h,
                        offset: l.offset[0],
                        pitch: l.pitch[0],
                        modifier: o.drm_format_modifier,
                    }
                };
                let (w, h) = (d.width, d.height);
                targets.push(Nv12Target {
                    y: plane(0, w, h),
                    uv: plane(1, w / 2, h / 2),
                });
            }
            slots.push(targets);
        }
        crate::capture::TileDiffer::with_conversion(self.layout, slots, self.cfg.color.full_range)
    }

    fn collector(&self) -> Result<&Collector> {
        self.collector.as_ref().context("encode collector stopped")
    }

    /// Feeds delivered sizes to the outer rate loop and reclaims finished slots.
    fn record_sizes(&mut self) {
        let Some(c) = &self.collector else { return };
        self.free_slots.extend(c.free.try_iter());
        let sizes: Vec<usize> = c.sizes.try_iter().collect();
        if self.rc_mode != VA_RC_CQP {
            for bytes in sizes {
                self.send_rate |= self.trim.record(bytes, Instant::now());
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Dpy(VADisplay);
// SAFETY: libva calls on one display may come from several threads. The collector thread is
// joined in `Collector::drop`, which `Session::drop` runs before destroying any buffer or
// terminating the display, so the pointer stays valid for the thread's whole life.
unsafe impl Send for Dpy {}

struct Job {
    slot: usize,
    /// (tile, keyframe, coded buffer) in coding order.
    pics: Vec<(u8, bool, VABufferID)>,
    submitted: Instant,
    meta: Option<EncodedFrame>,
}

type JobResult = Result<EncodedFrame>;
type Output = std::sync::Arc<std::sync::Mutex<Option<mpsc::Sender<JobResult>>>>;

/// Waits for coded pictures off the submitting thread, so the next frame's conversion and
/// submission overlap the VCN's work on this one.
struct Collector {
    jobs: Option<mpsc::Sender<Job>>,
    results: mpsc::Receiver<JobResult>,
    free: mpsc::Receiver<usize>,
    /// Bytes delivered per frame, for the outer rate loop.
    sizes: mpsc::Receiver<usize>,
    /// When set, results go here instead of `results`.
    output: Output,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Collector {
    fn spawn(dpy: Dpy) -> Result<Self> {
        let (jobs, job_rx) = mpsc::channel::<Job>();
        let (res_tx, results) = mpsc::channel();
        let (free_tx, free) = mpsc::channel();
        let (size_tx, sizes) = mpsc::channel();
        let output: Output = Default::default();
        let out = output.clone();
        let thread = std::thread::Builder::new()
            .name("hevc-collect".into())
            .spawn(move || {
                let dpy = dpy;
                for mut job in job_rx {
                    let r = job
                        .pics
                        .iter()
                        .map(|&(index, keyframe, buf)| {
                            // SAFETY: `dpy` outlives this thread (see the `Dpy` impl), and
                            // `buf` is one of the session's coded buffers, which are not reused
                            // until this job reports its slot free.
                            let data = unsafe { read_coded(dpy.0, buf)? };
                            Ok(EncodedTile {
                                index,
                                keyframe,
                                data: nal::strip_sei(&data),
                            })
                        })
                        .collect::<Result<Vec<_>>>()
                        .map(|tiles| {
                            let mut f = job.meta.take().expect("job without metadata");
                            f.timings.encode = job.submitted.elapsed();
                            let _ = size_tx.send(tiles.iter().map(|t| t.data.len()).sum());
                            f.tiles = tiles;
                            f
                        });
                    let _ = free_tx.send(job.slot);
                    let sent = match &*out.lock().unwrap() {
                        Some(tx) => tx.send(r).is_ok(),
                        None => res_tx.send(r).is_ok(),
                    };
                    if !sent {
                        break;
                    }
                }
            })?;
        Ok(Self {
            jobs: Some(jobs),
            results,
            free,
            sizes,
            output,
            thread: Some(thread),
        })
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        self.jobs.take();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Waits for `buf` with short timeouts: vaSyncBuffer holds the context lock while waiting,
/// which would otherwise block submission of the next frame.
///
/// # Safety
///
/// `dpy` must be an initialised display and `buf` a coded buffer on it that has been submitted
/// and is not destroyed during the call.
unsafe fn read_coded(dpy: VADisplay, buf: VABufferID) -> Result<Vec<u8>> {
    loop {
        // SAFETY: Upheld by this function's contract.
        match unsafe { va().vaSyncBuffer(dpy, buf, 1_000_000) } {
            0 => break,
            s if s == VA_STATUS_ERROR_TIMEDOUT as VAStatus => continue,
            s => check(s, "vaSyncBuffer")?,
        }
    }
    let mut data = Vec::new();
    // SAFETY: Upheld by this function's contract. The segment list from vaMapBuffer is valid
    // until vaUnmapBuffer, each `buf` points at `size` bytes, and `seg` is null-checked before
    // each dereference.
    unsafe {
        let mut seg: *mut c_void = ptr::null_mut();
        check(va().vaMapBuffer(dpy, buf, &mut seg), "map coded buffer")?;
        while !seg.is_null() {
            let s = &*(seg as *const VACodedBufferSegment);
            data.extend_from_slice(std::slice::from_raw_parts(
                s.buf as *const u8,
                s.size as usize,
            ));
            seg = s.next;
        }
        va().vaUnmapBuffer(dpy, buf);
    }
    Ok(data)
}

fn rect(y: u32, height: u32, width: u32) -> VARectangle {
    VARectangle {
        x: 0,
        y: y as i16,
        width: width as u16,
        height: height as u16,
    }
}

static VA: OnceLock<Result<Va, String>> = OnceLock::new();

fn load() -> Result<&'static Va> {
    // SAFETY: `Va::new` only dlopens the library and resolves its symbols; the `OnceLock` runs
    // it once, before any VA call.
    VA.get_or_init(|| unsafe { Va::new("libva-drm.so.2") }.map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| anyhow!("load libva: {e}"))
}

/// Only valid once `Display::open` has loaded libva.
fn va() -> &'static Va {
    VA.get()
        .and_then(|r| r.as_ref().ok())
        .expect("libva not loaded")
}

fn check(status: VAStatus, what: &str) -> Result<()> {
    if status == 0 {
        return Ok(());
    }
    // SAFETY: vaErrorStr returns a NUL-terminated string that outlives the call.
    let msg = unsafe { CStr::from_ptr(va().vaErrorStr(status)) }.to_string_lossy();
    bail!("{what}: {msg} ({status})")
}

fn va_fourcc(drm: u32) -> Result<u32> {
    Ok(match &drm.to_le_bytes() {
        b"XR24" => VA_FOURCC_BGRX,
        b"AR24" => VA_FOURCC_BGRA,
        b"XB24" => VA_FOURCC_RGBX,
        b"AB24" => VA_FOURCC_RGBA,
        _ => bail!("unsupported DRM format {drm:#x}"),
    })
}

///
/// # Safety
///
/// `dpy` must be an initialised display, and the plane fds of `buf` must stay open for the
/// duration of the call.
unsafe fn import_dmabuf(dpy: VADisplay, buf: &DmabufBuffer) -> Result<VASurfaceID> {
    // SAFETY: `VADRMPRIMESurfaceDescriptor` is a plain C struct of integers and arrays, so all-
    // zero is a valid value.
    let mut desc: VADRMPRIMESurfaceDescriptor = unsafe { std::mem::zeroed() };
    desc.fourcc = va_fourcc(buf.fourcc)?;
    desc.width = buf.width;
    desc.height = buf.height;
    desc.num_layers = 1;
    let layer = &mut desc.layers[0];
    layer.drm_format = buf.fourcc;
    layer.num_planes = buf.planes.len() as u32;
    // gbm hands out one fd per plane even when they share a BO; VA wants one object per BO.
    let mut inodes = Vec::new();
    for (i, p) in buf.planes.iter().enumerate() {
        let ino = File::from(p.fd.try_clone()?).metadata()?.ino();
        let obj = match inodes.iter().position(|&n| n == ino) {
            Some(o) => o,
            None => {
                let o = inodes.len();
                let fd = p.fd.as_raw_fd();
                // SAFETY: `fd` is a plane fd owned by `buf`, which the caller keeps open.
                let size = unsafe { libc::lseek(fd, 0, libc::SEEK_END) };
                desc.objects[o].fd = fd;
                desc.objects[o].size = size.max(0) as u32;
                desc.objects[o].drm_format_modifier = buf.modifier;
                inodes.push(ino);
                o
            }
        };
        layer.object_index[i] = obj as u32;
        layer.offset[i] = p.offset;
        layer.pitch[i] = p.stride;
    }
    desc.num_objects = inodes.len() as u32;

    let mut attribs = [
        VASurfaceAttrib {
            type_: VASurfaceAttribType::VASurfaceAttribMemoryType,
            flags: VA_SURFACE_ATTRIB_SETTABLE,
            value: VAGenericValue {
                type_: VAGenericValueType::VAGenericValueTypeInteger,
                value: _VAGenericValue__bindgen_ty_1 {
                    i: VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2 as i32,
                },
            },
        },
        VASurfaceAttrib {
            type_: VASurfaceAttribType::VASurfaceAttribExternalBufferDescriptor,
            flags: VA_SURFACE_ATTRIB_SETTABLE,
            value: VAGenericValue {
                type_: VAGenericValueType::VAGenericValueTypePointer,
                value: _VAGenericValue__bindgen_ty_1 {
                    p: (&mut desc as *mut VADRMPRIMESurfaceDescriptor).cast(),
                },
            },
        },
    ];
    let mut surface = VA_INVALID_SURFACE;
    check(
        // SAFETY: `desc` is live and referenced by `attribs[1]` until the call returns, and its
        // fds are open. `attribs` has the 2 entries passed as the count, and `surface` is a
        // valid out pointer for 1 surface.
        unsafe {
            va().vaCreateSurfaces(
                dpy,
                VA_RT_FORMAT_RGB32,
                buf.width,
                buf.height,
                &mut surface,
                1,
                attribs.as_mut_ptr(),
                2,
            )
        },
        "import dmabuf as VA surface",
    )?;
    Ok(surface)
}
