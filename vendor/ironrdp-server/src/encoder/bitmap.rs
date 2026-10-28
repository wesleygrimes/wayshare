use ironrdp_core::{Encode as _, WriteCursor, cast_length, invalid_field_err};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::rdp6::{
    ABgrChannels, ARgbChannels, BgrAChannels, BitmapEncodeError, BitmapStreamEncoder, RgbAChannels,
};
use ironrdp_pdu::bitmap::{self, BitmapData, BitmapUpdateData, Compression};
use ironrdp_pdu::geometry::InclusiveRectangle;

use crate::BitmapUpdate;

// PERF: we could also remove the need for this buffer
#[derive(Clone)]
pub(crate) struct BitmapEncoder {
    buffer: Vec<u8>,
}

impl BitmapEncoder {
    pub(crate) fn new() -> Self {
        Self {
            buffer: vec![0; usize::from(u16::MAX)],
        }
    }

    pub(crate) fn encode(&mut self, bitmap: &BitmapUpdate, output: &mut [u8]) -> Result<usize, BitmapEncodeError> {
        // RDP bitmap scanlines have a multiple-of-four pixel width. Pad only
        // the coded rows; the destination rectangle still clips to the desktop.
        let width = bitmap
            .width
            .get()
            .checked_add(3)
            .ok_or_else(|| BitmapEncodeError::Encode(invalid_field_err!("bitmap", "Width cannot be padded")))?
            & !3;
        let bytes_per_pixel = u16::from(bitmap.format.bytes_per_pixel());
        let row_len = bitmap.width.get() * bytes_per_pixel;
        let coded_row_len = width * bytes_per_pixel;
        let chunk_height = u16::MAX / coded_row_len;

        let mut cursor = WriteCursor::new(output);
        let stride = bitmap.stride.get();
        let total = bitmap.height.get().div_ceil(chunk_height);
        BitmapUpdateData::encode_header(total, &mut cursor).map_err(BitmapEncodeError::Encode)?;

        // Sub-regions retain the framebuffer stride, but the final row ends at
        // the visible width. Byte length divided by stride would lose that row.
        for y in (0..bitmap.height.get()).step_by(usize::from(chunk_height)) {
            let height = chunk_height.min(bitmap.height.get() - y);
            let top = bitmap.y + y;

            let encoder = BitmapStreamEncoder::new(usize::from(width), usize::from(height));

            let len = {
                let pixels = (0..height)
                    .rev()
                    .map(|row| {
                        let start = usize::from(y + row) * stride;
                        &bitmap.data[start..start + usize::from(row_len)]
                    })
                    .flat_map(|row| {
                        row.chunks(usize::from(bytes_per_pixel)).chain(std::iter::repeat_n(
                            &row[usize::from(row_len - bytes_per_pixel)..],
                            usize::from(width - bitmap.width.get()),
                        ))
                    });

                Self::encode_iter(encoder, bitmap.format, pixels, self.buffer.as_mut_slice())?
            };

            let data = BitmapData {
                rectangle: InclusiveRectangle {
                    left: bitmap.x,
                    top,
                    right: bitmap.x + bitmap.width.get() - 1,
                    bottom: top + height - 1,
                },
                width,
                height,
                bits_per_pixel: u16::from(bitmap.format.bytes_per_pixel()) * 8,
                compression_flags: Compression::BITMAP_COMPRESSION,
                compressed_data_header: Some(bitmap::CompressedDataHeader {
                    main_body_size: cast_length!("main body size", len).map_err(BitmapEncodeError::Encode)?,
                    scan_width: width,
                    uncompressed_size: height * coded_row_len,
                }),
                bitmap_data: &self.buffer[..len],
            };

            data.encode(&mut cursor).map_err(BitmapEncodeError::Encode)?;
        }

        Ok(cursor.pos())
    }

    fn encode_iter<'a, P>(
        mut encoder: BitmapStreamEncoder,
        format: PixelFormat,
        src: P,
        dst: &mut [u8],
    ) -> Result<usize, BitmapEncodeError>
    where
        P: Iterator<Item = &'a [u8]> + Clone,
    {
        let written = match format {
            PixelFormat::ARgb32 | PixelFormat::XRgb32 => {
                encoder.encode_pixels_stream::<_, ARgbChannels>(src, dst, true)?
            }
            PixelFormat::RgbA32 | PixelFormat::RgbX32 => {
                encoder.encode_pixels_stream::<_, RgbAChannels>(src, dst, true)?
            }
            PixelFormat::ABgr32 | PixelFormat::XBgr32 => {
                encoder.encode_pixels_stream::<_, ABgrChannels>(src, dst, true)?
            }
            PixelFormat::BgrA32 | PixelFormat::BgrX32 => {
                encoder.encode_pixels_stream::<_, BgrAChannels>(src, dst, true)?
            }
        };

        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::num::{NonZeroU16, NonZeroUsize};
    use ironrdp_core::decode;
    use ironrdp_graphics::rdp6::BitmapStreamDecoder;

    #[test]
    fn cropped_bitmaps_decode_every_row_with_padding_outside_the_destination() {
        let (width, height) = (1579_u16, 797_u16);
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[17, (x % 251) as u8, (y % 251) as u8, 255]);
            }
        }
        let full = BitmapUpdate {
            x: 0,
            y: 0,
            width: NonZeroU16::new(width).unwrap(),
            height: NonZeroU16::new(height).unwrap(),
            format: PixelFormat::BgrX32,
            data: pixels.into(),
            stride: NonZeroUsize::new(usize::from(width) * 4).unwrap(),
        };
        for (w, h) in [(191, 171), (192, 193), (1, 1)] {
            let cropped = full
                .sub(35, 17, NonZeroU16::new(w).unwrap(), NonZeroU16::new(h).unwrap())
                .unwrap();
            let mut encoder = BitmapEncoder::new();
            let mut output = vec![0; cropped.data.len() * 2 + 65536];
            let len = encoder.encode(&cropped, &mut output).unwrap();
            let pdu: BitmapUpdateData<'_> = decode(&output[..len]).unwrap();
            let mut rows = 0;
            for rect in &pdu.rectangles {
                assert!(rect.height > 0);
                assert_eq!(rect.rectangle.top, cropped.y + rows);
                assert_eq!(rect.rectangle.right, cropped.x + w - 1);
                assert_eq!(rect.rectangle.bottom, rect.rectangle.top + rect.height - 1);
                assert_eq!(rect.width, (w + 3) & !3);
                let mut rgb = Vec::new();
                BitmapStreamDecoder::default()
                    .decode_bitmap_stream_to_rgb24(
                        rect.bitmap_data,
                        &mut rgb,
                        usize::from(rect.width),
                        usize::from(rect.height),
                    )
                    .unwrap();
                assert_eq!(rgb.len(), usize::from(rect.width) * usize::from(rect.height) * 3);
                let red = (rect.rectangle.bottom % 251) as u8;
                assert_eq!(&rgb[..3], &[red, 35, 17]);
                let last = usize::from(rect.width - 1) * 3;
                assert_eq!(&rgb[last..last + 3], &[red, ((35 + w - 1) % 251) as u8, 17]);
                rows += rect.height;
            }
            assert_eq!(rows, h);
        }
    }
}
