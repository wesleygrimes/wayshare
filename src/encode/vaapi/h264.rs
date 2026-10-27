//! Full-frame AVC420 encoder with packed Constrained Baseline headers.

use super::*;
use crate::encode::h264;
use crate::source::Frame as RgbFrame;
use anyhow::ensure;

pub struct H264Encoder {
    width: u32,
    height: u32,
    enc_config: VAConfigID,
    enc_ctx: VAContextID,
    vpp_config: VAConfigID,
    vpp_ctx: VAContextID,
    rgb: VASurfaceID,
    input: VASurfaceID,
    recon: [VASurfaceID; 2],
    coded: VABufferID,
    image: Option<VAImage>,
    reference: Option<VAPictureH264>,
    number: u16,
    display: Display,
}

impl H264Encoder {
    pub fn new(node: &Path, width: u16, height: u16) -> Result<Self> {
        ensure!(width > 0 && height > 0, "empty H.264 picture");
        let display = Display::open(node)?;
        let profile = VAProfile::VAProfileH264ConstrainedBaseline;
        let entry = display
            .encode_entrypoint(profile)
            .context("no VA-API H.264 encode entrypoint")?;
        let mut attrs = [
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
        // SAFETY: attrs contains the three query/output entries and display is live.
        check(
            unsafe {
                va().vaGetConfigAttributes(display.dpy, profile, entry, attrs.as_mut_ptr(), 3)
            },
            "query H.264 attributes",
        )?;
        ensure!(
            attrs[0].value & VA_RT_FORMAT_YUV420 != 0 && attrs[1].value & VA_RC_CQP != 0,
            "H.264 driver lacks YUV420/CQP"
        );
        let packed = VA_ENC_PACKED_HEADER_SEQUENCE
            | VA_ENC_PACKED_HEADER_PICTURE
            | VA_ENC_PACKED_HEADER_SLICE;
        ensure!(
            attrs[2].value != VA_ATTRIB_NOT_SUPPORTED && attrs[2].value & packed == packed,
            "H.264 driver lacks packed SPS/PPS/slice headers"
        );
        attrs[2].value = packed;
        attrs[0].value = VA_RT_FORMAT_YUV420;
        attrs[1].value = VA_RC_CQP;
        let mut s = Self {
            width: u32::from(width).next_multiple_of(16),
            height: u32::from(height).next_multiple_of(16),
            enc_config: VA_INVALID_ID,
            enc_ctx: VA_INVALID_ID,
            vpp_config: VA_INVALID_ID,
            vpp_ctx: VA_INVALID_ID,
            rgb: VA_INVALID_SURFACE,
            input: VA_INVALID_SURFACE,
            recon: [VA_INVALID_SURFACE; 2],
            coded: VA_INVALID_ID,
            image: None,
            reference: None,
            number: 0,
            display,
        };
        let dpy = s.display.dpy;
        // SAFETY: All output pointers refer to initialized fields; Drop cleans up partial construction.
        // Each VA object belongs to this encoder and the display remains alive until after destruction.
        unsafe {
            check(
                va().vaCreateConfig(
                    dpy,
                    profile,
                    entry,
                    attrs.as_mut_ptr(),
                    3,
                    &mut s.enc_config,
                ),
                "create H.264 config",
            )?;
            check(
                va().vaCreateSurfaces(
                    dpy,
                    VA_RT_FORMAT_YUV420,
                    s.width,
                    s.height,
                    &mut s.input,
                    1,
                    ptr::null_mut(),
                    0,
                ),
                "H.264 input",
            )?;
            check(
                va().vaCreateSurfaces(
                    dpy,
                    VA_RT_FORMAT_YUV420,
                    s.width,
                    s.height,
                    s.recon.as_mut_ptr(),
                    2,
                    ptr::null_mut(),
                    0,
                ),
                "H.264 references",
            )?;
            let mut targets = [s.input, s.recon[0], s.recon[1]];
            check(
                va().vaCreateContext(
                    dpy,
                    s.enc_config,
                    s.width as i32,
                    s.height as i32,
                    VA_PROGRESSIVE as i32,
                    targets.as_mut_ptr(),
                    3,
                    &mut s.enc_ctx,
                ),
                "H.264 context",
            )?;
            check(
                va().vaCreateBuffer(
                    dpy,
                    s.enc_ctx,
                    VABufferType::VAEncCodedBufferType,
                    s.width * s.height * 3 / 2 + 65536,
                    1,
                    ptr::null_mut(),
                    &mut s.coded,
                ),
                "H.264 coded buffer",
            )?;
            let mut attr = VASurfaceAttrib {
                type_: VASurfaceAttribType::VASurfaceAttribPixelFormat,
                flags: VA_SURFACE_ATTRIB_SETTABLE,
                value: VAGenericValue {
                    type_: VAGenericValueType::VAGenericValueTypeInteger,
                    value: _VAGenericValue__bindgen_ty_1 {
                        i: VA_FOURCC_BGRA as i32,
                    },
                },
            };
            check(
                va().vaCreateSurfaces(
                    dpy,
                    VA_RT_FORMAT_RGB32,
                    s.width,
                    s.height,
                    &mut s.rgb,
                    1,
                    &mut attr,
                    1,
                ),
                "H.264 RGB surface",
            )?;
            check(
                va().vaCreateConfig(
                    dpy,
                    VAProfile::VAProfileNone,
                    VAEntrypoint::VAEntrypointVideoProc,
                    ptr::null_mut(),
                    0,
                    &mut s.vpp_config,
                ),
                "H.264 VPP config",
            )?;
            check(
                va().vaCreateContext(
                    dpy,
                    s.vpp_config,
                    s.width as i32,
                    s.height as i32,
                    0,
                    ptr::null_mut(),
                    0,
                    &mut s.vpp_ctx,
                ),
                "H.264 VPP context",
            )?;
            let mut format = VAImageFormat {
                fourcc: VA_FOURCC_BGRA,
                byte_order: VA_LSB_FIRST,
                bits_per_pixel: 32,
                depth: 32,
                red_mask: 0xff0000,
                green_mask: 0xff00,
                blue_mask: 0xff,
                alpha_mask: 0xff000000,
                va_reserved: [0; 4],
            };
            let mut image: VAImage = std::mem::zeroed();
            check(
                va().vaCreateImage(
                    dpy,
                    &mut format,
                    s.width as i32,
                    s.height as i32,
                    &mut image,
                ),
                "H.264 upload image",
            )?;
            s.image = Some(image);
        }
        Ok(s)
    }

    pub fn encode(&mut self, frame: &RgbFrame, idr: bool) -> Result<Vec<u8>> {
        ensure!(
            u32::from(frame.width) <= self.width
                && u32::from(frame.height) <= self.height
                && frame.width > 0
                && frame.height > 0,
            "H.264 input dimensions changed"
        );
        ensure!(
            frame.pixels.len() == usize::from(frame.width) * usize::from(frame.height),
            "invalid H.264 pixels"
        );
        let image = self.image.as_ref().unwrap();
        let pitch = image.pitches[0] as usize;
        let offset = image.offsets[0] as usize;
        ensure!(
            offset + (self.height as usize - 1) * pitch + self.width as usize * 4
                <= image.data_size as usize,
            "invalid VA upload layout"
        );
        let dpy = self.display.dpy;
        // SAFETY: The mapped allocation has image.data_size bytes. Bounds were checked above.
        // No VA operation uses the image until it has been unmapped; input pixels stay borrowed.
        unsafe {
            let mut mapped = ptr::null_mut();
            check(
                va().vaMapBuffer(dpy, image.buf, &mut mapped),
                "map H.264 upload",
            )?;
            let dst = std::slice::from_raw_parts_mut(mapped.cast::<u8>(), image.data_size as usize);
            for y in 0..self.height as usize {
                let row = y.min(usize::from(frame.height) - 1) * usize::from(frame.width);
                for x in 0..self.width as usize {
                    let p = frame.pixels[row + x.min(usize::from(frame.width) - 1)] | 0xff000000;
                    let start = offset + y * pitch + x * 4;
                    dst[start..start + 4].copy_from_slice(&p.to_le_bytes());
                }
            }
            check(va().vaUnmapBuffer(dpy, image.buf), "unmap H.264 upload")?;
            check(
                va().vaPutImage(
                    dpy,
                    self.rgb,
                    image.image_id,
                    0,
                    0,
                    self.width,
                    self.height,
                    0,
                    0,
                    self.width,
                    self.height,
                ),
                "upload H.264 RGB",
            )?;
        }
        // MS-RDPEGFX AVC420 uses full-range BT.709. Keep the coded size aligned to 16;
        // the full-surface region mask, rather than SPS cropping, removes the padding.
        // SAFETY: This C struct contains integers and pointers. Zero initializes its reserved
        // fields and optional pointers; the active surface and colour fields are set below.
        let mut vpp: VAProcPipelineParameterBuffer = unsafe { std::mem::zeroed() };
        vpp.surface = self.rgb;
        vpp.surface_color_standard = _VAProcColorStandardType_VAProcColorStandardSRGB;
        vpp.output_color_standard = _VAProcColorStandardType_VAProcColorStandardBT709;
        vpp.input_color_properties.color_range = VA_SOURCE_RANGE_FULL as u8;
        vpp.output_color_properties.color_range = VA_SOURCE_RANGE_FULL as u8;
        vpp.output_color_properties.matrix_coefficients = 1;
        self.render(
            self.vpp_ctx,
            self.input,
            &mut [(
                VABufferType::VAProcPipelineParameterBufferType,
                parameter(&mut vpp),
            )],
        )?;
        // SAFETY: Synchronizes this encoder's input before reuse by its encode context.
        check(
            unsafe { va().vaSyncSurface(dpy, self.input) },
            "H.264 conversion",
        )?;
        let idr = idr || self.reference.is_none();
        if idr {
            self.number = 0;
            self.reference = None;
        }
        let invalid = VAPictureH264 {
            picture_id: VA_INVALID_SURFACE,
            flags: VA_PICTURE_H264_INVALID,
            ..unsafe { std::mem::zeroed() }
        };
        // SAFETY: These libva parameter structs contain only integers, bitfields and arrays.
        // All-zero initializes reserved fields; active fields and invalid references are set below.
        let mut seq: VAEncSequenceParameterBufferH264 = unsafe { std::mem::zeroed() };
        let macroblocks = self.width * self.height / 256;
        ensure!(macroblocks <= 36864, "H.264 dimensions exceed level 5.1");
        seq.level_idc = if macroblocks <= 8192 {
            40
        } else if macroblocks <= 22080 {
            50
        } else {
            51
        };
        seq.intra_period = 300;
        seq.intra_idr_period = 300;
        seq.ip_period = 1;
        seq.max_num_ref_frames = 1;
        seq.picture_width_in_mbs = (self.width / 16) as u16;
        seq.picture_height_in_mbs = (self.height / 16) as u16;
        // SAFETY: Accessing the initialized union's bitfield member, whose representation is u32.
        unsafe {
            seq.seq_fields.bits.set_chroma_format_idc(1);
            seq.seq_fields.bits.set_frame_mbs_only_flag(1);
            seq.seq_fields.bits.set_direct_8x8_inference_flag(1);
            seq.seq_fields.bits.set_log2_max_frame_num_minus4(4);
            seq.seq_fields.bits.set_pic_order_cnt_type(2);
        }
        let current = VAPictureH264 {
            picture_id: self.recon[usize::from(self.number % 2)],
            frame_idx: u32::from(self.number),
            flags: VA_PICTURE_H264_SHORT_TERM_REFERENCE,
            TopFieldOrderCnt: i32::from(self.number) * 2,
            BottomFieldOrderCnt: i32::from(self.number) * 2,
            va_reserved: [0; 4],
        };
        let mut pic: VAEncPictureParameterBufferH264 = unsafe { std::mem::zeroed() };
        pic.CurrPic = current;
        pic.ReferenceFrames = [invalid; 16];
        if let Some(reference) = self.reference {
            pic.ReferenceFrames[0] = reference;
        }
        pic.coded_buf = self.coded;
        pic.frame_num = self.number;
        pic.pic_init_qp = 24;
        unsafe {
            pic.pic_fields.bits.set_idr_pic_flag(u32::from(idr));
            pic.pic_fields.bits.set_reference_pic_flag(1);
            pic.pic_fields
                .bits
                .set_deblocking_filter_control_present_flag(1);
        }
        let mut slice: VAEncSliceParameterBufferH264 = unsafe { std::mem::zeroed() };
        slice.num_macroblocks = self.width * self.height / 256;
        slice.macroblock_info = VA_INVALID_ID;
        slice.slice_type = if idr { 2 } else { 0 };
        slice.RefPicList0 = [invalid; 32];
        slice.RefPicList1 = [invalid; 32];
        if let Some(reference) = self.reference {
            slice.RefPicList0[0] = reference;
        }
        let level = seq.level_idc;
        let mut params = vec![];
        if idr {
            params.push((
                VABufferType::VAEncSequenceParameterBufferType,
                parameter(&mut seq),
            ));
        }
        params.push((
            VABufferType::VAEncPictureParameterBufferType,
            parameter(&mut pic),
        ));
        params.push((
            VABufferType::VAEncSliceParameterBufferType,
            parameter(&mut slice),
        ));
        let mut packed_headers = Vec::new();
        if idr {
            for (i, data) in h264::parameter_sets(self.width, self.height, level)
                .into_iter()
                .enumerate()
            {
                let header = VAEncPackedHeaderParameterBuffer {
                    type_: i as u32 + 1,
                    bit_length: data.len() as u32 * 8,
                    has_emulation_bytes: 1,
                    va_reserved: [0; 4],
                };
                packed_headers.push((header, data));
            }
        }
        let (data, bit_length) = h264::slice_header(self.number, idr);
        packed_headers.push((
            VAEncPackedHeaderParameterBuffer {
                type_: 3,
                bit_length,
                has_emulation_bytes: 1,
                va_reserved: [0; 4],
            },
            data,
        ));
        for (header, data) in &mut packed_headers {
            params.push((
                VABufferType::VAEncPackedHeaderParameterBufferType,
                parameter(header),
            ));
            params.push((
                VABufferType::VAEncPackedHeaderDataBufferType,
                (data.as_mut_ptr().cast(), data.len() as u32),
            ));
        }
        self.render(self.enc_ctx, self.input, &mut params)?;
        // SAFETY: The coded buffer belongs to this live context and remains allocated during readback.
        check(
            unsafe { va().vaSyncBuffer(dpy, self.coded, 2_000_000_000) },
            "H.264 encode completion",
        )?;
        let data = unsafe { read_coded(dpy, self.coded) }?;
        ensure!(!data.is_empty(), "empty H.264 access unit");
        self.reference = Some(current);
        self.number = (self.number + 1) % 256;
        Ok(data)
    }

    fn render(
        &self,
        ctx: VAContextID,
        target: VASurfaceID,
        params: &mut [(VABufferType, (*mut c_void, u32))],
    ) -> Result<()> {
        let dpy = self.display.dpy;
        let mut buffers = vec![];
        let result = (|| {
            // SAFETY: Every parameter points at a live initialized C struct or byte buffer owned
            // by encode(). Those locals stay alive until vaCreateBuffer has copied them.
            // All ids belong to this context. EndPicture pairs with BeginPicture even on render failure.
            unsafe {
                for (ty, (data, size)) in params {
                    let mut id = VA_INVALID_ID;
                    check(
                        va().vaCreateBuffer(dpy, ctx, *ty, *size, 1, *data, &mut id),
                        "H.264 parameter buffer",
                    )?;
                    buffers.push(id);
                }
                check(va().vaBeginPicture(dpy, ctx, target), "H.264 begin")?;
                let render = check(
                    va().vaRenderPicture(dpy, ctx, buffers.as_mut_ptr(), buffers.len() as i32),
                    "H.264 render",
                );
                let end = check(va().vaEndPicture(dpy, ctx), "H.264 end");
                render.and(end)
            }
        })();
        for b in buffers {
            // SAFETY: Each buffer was created above and is no longer needed after EndPicture.
            unsafe { va().vaDestroyBuffer(dpy, b) };
        }
        result
    }
}

fn parameter<T>(value: &mut T) -> (*mut c_void, u32) {
    ((value as *mut T).cast(), size_of::<T>() as u32)
}

impl Drop for H264Encoder {
    fn drop(&mut self) {
        let dpy = self.display.dpy;
        // SAFETY: This synchronous encoder has no concurrent users. All valid ids were created
        // by this instance. Partial construction leaves invalid ids, which are skipped.
        unsafe {
            if let Some(image) = self.image {
                va().vaDestroyImage(dpy, image.image_id);
            }
            if self.coded != VA_INVALID_ID {
                va().vaDestroyBuffer(dpy, self.coded);
            }
            for ctx in [self.enc_ctx, self.vpp_ctx] {
                if ctx != VA_INVALID_ID {
                    va().vaDestroyContext(dpy, ctx);
                }
            }
            for mut surface in [self.rgb, self.input, self.recon[0], self.recon[1]] {
                if surface != VA_INVALID_SURFACE {
                    va().vaDestroySurfaces(dpy, &mut surface, 1);
                }
            }
            for config in [self.enc_config, self.vpp_config] {
                if config != VA_INVALID_ID {
                    va().vaDestroyConfig(dpy, config);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::formats::YUVSource;

    #[test]
    #[ignore = "requires a VA-API H.264 encoder on /dev/dri/renderD128"]
    fn vaapi_idr_pictures_and_reset_decode() {
        for (width, height) in [(256, 256), (836, 1580), (1580, 836)] {
            let frame = RgbFrame {
                width,
                height,
                pixels: vec![0x33aa66; usize::from(width) * usize::from(height)],
            };
            let mut encoder =
                H264Encoder::new(Path::new("/dev/dri/renderD128"), width, height).unwrap();
            let mut decoder = openh264::decoder::Decoder::new().unwrap();
            let pictures = if width == 256 { 310 } else { 4 };
            for picture in 0..pictures {
                let idr = matches!(picture, 0 | 3 | 303);
                let data = encoder.encode(&frame, idr).unwrap();
                let types: Vec<_> = nal::split_annexb(&data).map(|n| n[0] & 31).collect();
                if idr {
                    assert!(
                        types.contains(&7) && types.contains(&8) && types.contains(&5),
                        "{types:?}"
                    );
                }
                let decoded = decoder.decode(&data).unwrap().unwrap();
                assert_eq!(
                    decoded.dimensions(),
                    (
                        usize::from(width).next_multiple_of(16),
                        usize::from(height).next_multiple_of(16)
                    )
                );
                assert!(decoded.y()[0] > 100);
            }
        }
    }
}
