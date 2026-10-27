//! AVC420 encoders take the same CPU framebuffer as the RemoteFX path.

use crate::encode::Chroma;
use crate::encode::convert::{self, Coeffs, Planes};
use crate::source::Frame;
use anyhow::{Result, ensure};
use openh264::encoder::{EncoderConfig, Profile, RateControlMode, VuiConfig};
use openh264::formats::YUVBuffer;

pub enum Encoder {
    #[cfg(target_os = "linux")]
    Vaapi(Box<crate::encode::vaapi::h264::H264Encoder>),
    Software(Box<openh264::encoder::Encoder>),
}

impl Encoder {
    pub fn new(width: u16, height: u16) -> Result<Self> {
        #[cfg(target_os = "linux")]
        match crate::encode::vaapi::h264::H264Encoder::new(
            std::path::Path::new("/dev/dri/renderD128"),
            width,
            height,
        ) {
            Ok(encoder) => return Ok(Self::Vaapi(Box::new(encoder))),
            Err(error) => tracing::debug!(%error, "RDP VA-API H.264 unavailable; trying OpenH264"),
        }
        Self::software()
    }

    fn software() -> Result<Self> {
        let config = EncoderConfig::new()
            .profile(Profile::Baseline)
            .rate_control_mode(RateControlMode::Off)
            .skip_frames(false)
            .vui(VuiConfig::srgb());
        Ok(Self::Software(Box::new(
            openh264::encoder::Encoder::with_api_config(
                openh264::OpenH264API::from_source(),
                config,
            )?,
        )))
    }

    pub fn name(&self) -> &'static str {
        match self {
            #[cfg(target_os = "linux")]
            Self::Vaapi(_) => "VA-API H.264 Constrained Baseline",
            Self::Software(_) => "OpenH264 Baseline (software)",
        }
    }

    pub fn encode(&mut self, frame: &Frame, idr: bool) -> Result<Vec<u8>> {
        let data = match self {
            #[cfg(target_os = "linux")]
            Self::Vaapi(encoder) => encoder.encode(frame, idr)?,
            Self::Software(encoder) => {
                if idr {
                    encoder.force_intra_frame();
                }
                encoder.encode(&yuv(frame)?)?.to_vec()
            }
        };
        let types: Vec<_> = crate::encode::nal::split_annexb(&data)
            .filter_map(|n| n.first().map(|b| b & 31))
            .collect();
        ensure!(
            types.contains(&1) || types.contains(&5),
            "H.264 encoder returned no picture"
        );
        if idr {
            ensure!(
                types.contains(&7) && types.contains(&8) && types.contains(&5),
                "H.264 IDR lacks SPS/PPS/IDR"
            );
        }
        Ok(data)
    }
}

fn yuv(frame: &Frame) -> Result<YUVBuffer> {
    ensure!(
        frame.width > 0
            && frame.height > 0
            && frame.pixels.len() == usize::from(frame.width) * usize::from(frame.height),
        "invalid H.264 framebuffer"
    );
    let (width, height) = (
        usize::from(frame.width).next_multiple_of(16),
        usize::from(frame.height).next_multiple_of(16),
    );
    let mut rgb = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let p = frame.pixels[y.min(usize::from(frame.height) - 1) * usize::from(frame.width)
                + x.min(usize::from(frame.width) - 1)];
            rgb.extend_from_slice(&p.to_le_bytes());
        }
    }
    let mut data = vec![0; width * height * 3 / 2];
    let (luma, chroma) = data.split_at_mut(width * height);
    let (u, v) = chroma.split_at_mut(width * height / 4);
    convert::bgrx_to_yuv(
        &rgb,
        width * 4,
        width,
        height,
        Chroma::Yuv420,
        &Coeffs::bt709(true),
        Planes {
            y: luma,
            u,
            v,
            y_stride: width,
            c_stride: width / 2,
        },
    );
    Ok(YUVBuffer::from_vec(data, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use openh264::formats::YUVSource;

    #[test]
    fn software_idr_and_pictures_decode_at_macroblock_size() {
        let frame = Frame {
            width: 202,
            height: 301,
            pixels: vec![0x33aa66; 202 * 301],
        };
        assert_eq!(yuv(&frame).unwrap().dimensions(), (208, 304));
        let mut encoder = Encoder::software().unwrap();
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        for idr in [true, false, true] {
            let data = encoder.encode(&frame, idr).unwrap();
            let picture = decoder.decode(&data).unwrap().unwrap();
            assert_eq!(picture.dimensions(), (208, 304));
            assert!(picture.y()[0] > 100);
        }
    }
}
