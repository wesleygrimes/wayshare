//! HEVC parameter set and slice header writer for the tiled sequence (ITU-T H.265 7.3).
//!
//! The whole screen is one sequence of strip pictures (`width x tile_height`), coded in
//! tile order. Every picture of tile `t` predicts only from the previous picture of tile `t`;
//! the short-term RPS keeps the latest picture of every tile alive.

use super::{ColorTags, Primaries, Transfer};

pub const NAL_TRAIL_R: u8 = 1;
pub const NAL_IDR_W_RADL: u8 = 19;
pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;

pub const LOG2_MAX_POC_LSB: u32 = 16;
/// Largest POC distance a short-term reference may have (delta_poc_s0_minus1 is < 2^15).
pub const MAX_REF_DISTANCE: u32 = 1 << 15;
/// Room for the latest picture of each tile plus the current one.
pub const MAX_DEC_PIC_BUFFERING: u32 = 5;
pub const CTB_SIZE: u32 = 64;

#[derive(Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    cur: u8,
    nbits: u32,
}

impl BitWriter {
    pub fn u(&mut self, bits: u32, value: u64) {
        for i in (0..bits).rev() {
            self.cur = (self.cur << 1) | ((value >> i) & 1) as u8;
            self.nbits += 1;
            if self.nbits.is_multiple_of(8) {
                self.bytes.push(self.cur);
                self.cur = 0;
            }
        }
    }

    pub fn flag(&mut self, b: bool) {
        self.u(1, b as u64);
    }

    pub fn ue(&mut self, v: u32) {
        let v = v as u64 + 1;
        let len = 64 - v.leading_zeros();
        self.u(len - 1, 0);
        self.u(len, v);
    }

    pub fn se(&mut self, v: i32) {
        self.ue(if v > 0 {
            2 * v as u32 - 1
        } else {
            2 * v.unsigned_abs()
        });
    }

    /// rbsp_trailing_bits / byte_alignment: a one bit, then zeros to the byte boundary.
    pub fn trailing(&mut self) {
        self.u(1, 1);
        while !self.nbits.is_multiple_of(8) {
            self.u(1, 0);
        }
    }

    pub fn into_bytes(self) -> Vec<u8> {
        assert!(self.nbits.is_multiple_of(8), "unaligned RBSP");
        self.bytes
    }
}

/// Annex B NAL unit (4-byte start code) with emulation prevention applied to the payload.
pub fn nal(nal_type: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + rbsp.len() / 64 + 6);
    out.extend_from_slice(&[0, 0, 0, 1, nal_type << 1, 1]);
    let mut zeros = 0;
    for &b in rbsp {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

#[derive(Clone, Debug)]
pub struct SeqConfig {
    /// Visible picture (strip) size.
    pub width: u32,
    pub height: u32,
    /// Pictures per second (frame rate x tiles).
    pub pic_rate: u32,
    pub color: ColorTags,
    /// RC modes other than CQP adjust QP per CU.
    pub cu_qp_delta: bool,
    pub sao: bool,
}

impl SeqConfig {
    pub fn coded_width(&self) -> u32 {
        self.width.next_multiple_of(16)
    }

    pub fn coded_height(&self) -> u32 {
        self.height.next_multiple_of(16)
    }

    pub fn ctus(&self) -> u32 {
        self.coded_width().div_ceil(CTB_SIZE) * self.coded_height().div_ceil(CTB_SIZE)
    }

    /// Lowest Main-tier level whose picture size and sample rate fit (Table A.8).
    pub fn level_idc(&self) -> u8 {
        let ps = self.coded_width() as u64 * self.coded_height() as u64;
        let sr = ps * self.pic_rate as u64;
        const LEVELS: [(u8, u64, u64); 9] = [
            (93, 983_040, 33_177_600),
            (120, 2_228_224, 66_846_720),
            (123, 2_228_224, 133_693_440),
            (150, 8_912_896, 267_386_880),
            (153, 8_912_896, 534_773_760),
            (156, 8_912_896, 1_069_547_520),
            (180, 35_651_584, 1_069_547_520),
            (183, 35_651_584, 2_139_095_040),
            (186, 35_651_584, 4_278_190_080),
        ];
        LEVELS
            .iter()
            .find(|&&(_, mps, msr)| ps <= mps && sr <= msr)
            .map_or(186, |l| l.0)
    }

    pub fn primaries(&self) -> u8 {
        match self.color.primaries {
            Primaries::Bt709 => 1,
            Primaries::DisplayP3 => 12,
        }
    }

    pub fn transfer(&self) -> u8 {
        match self.color.transfer {
            Transfer::Bt709 => 1,
            Transfer::Srgb => 13,
        }
    }

    fn profile_tier_level(&self, w: &mut BitWriter) {
        w.u(2, 0); // general_profile_space
        w.flag(false); // general_tier_flag
        w.u(5, 1); // general_profile_idc: Main
        w.u(32, 0x6000_0000); // compatible with Main and Main 10
        w.flag(true); // progressive_source
        w.flag(false); // interlaced_source
        w.flag(true); // non_packed_constraint
        w.flag(true); // frame_only_constraint
        w.u(43, 0);
        w.u(1, 0); // general_inbld_flag
        w.u(8, self.level_idc() as u64);
    }

    pub fn vps(&self) -> Vec<u8> {
        let mut w = BitWriter::default();
        w.u(4, 0); // vps_video_parameter_set_id
        w.flag(true); // base_layer_internal
        w.flag(true); // base_layer_available
        w.u(6, 0); // max_layers_minus1
        w.u(3, 0); // max_sub_layers_minus1
        w.flag(true); // temporal_id_nesting
        w.u(16, 0xffff);
        self.profile_tier_level(&mut w);
        w.flag(false); // sub_layer_ordering_info_present
        w.ue(MAX_DEC_PIC_BUFFERING - 1);
        w.ue(0); // max_num_reorder_pics
        w.ue(0); // max_latency_increase_plus1
        w.u(6, 0); // vps_max_layer_id
        w.ue(0); // num_layer_sets_minus1
        w.flag(false); // vps_timing_info_present
        w.flag(false); // vps_extension
        w.trailing();
        nal(NAL_VPS, &w.into_bytes())
    }

    pub fn sps(&self) -> Vec<u8> {
        let mut w = BitWriter::default();
        w.u(4, 0); // sps_video_parameter_set_id
        w.u(3, 0); // max_sub_layers_minus1
        w.flag(true); // temporal_id_nesting
        self.profile_tier_level(&mut w);
        w.ue(0); // sps_seq_parameter_set_id
        w.ue(1); // chroma_format_idc 4:2:0
        w.ue(self.coded_width());
        w.ue(self.coded_height());
        let (crop_r, crop_b) = (
            self.coded_width() - self.width,
            self.coded_height() - self.height,
        );
        w.flag(crop_r > 0 || crop_b > 0);
        if crop_r > 0 || crop_b > 0 {
            w.ue(0);
            w.ue(crop_r / 2);
            w.ue(0);
            w.ue(crop_b / 2);
        }
        w.ue(0); // bit_depth_luma_minus8
        w.ue(0); // bit_depth_chroma_minus8
        w.ue(LOG2_MAX_POC_LSB - 4);
        w.flag(false); // sub_layer_ordering_info_present
        w.ue(MAX_DEC_PIC_BUFFERING - 1);
        w.ue(0); // max_num_reorder_pics
        w.ue(0); // max_latency_increase_plus1
        w.ue(0); // log2_min_luma_coding_block_size_minus3: 8
        w.ue(3); // log2_diff_max_min: 64
        w.ue(0); // log2_min_transform_block_size_minus2: 4
        w.ue(3); // log2_diff_max_min_transform: 32
        w.ue(4); // max_transform_hierarchy_depth_inter
        w.ue(4); // max_transform_hierarchy_depth_intra
        w.flag(false); // scaling_list_enabled
        w.flag(true); // amp_enabled
        w.flag(self.sao); // sample_adaptive_offset_enabled
        w.flag(false); // pcm_enabled
        w.ue(0); // num_short_term_ref_pic_sets: all explicit in slice headers
        w.flag(false); // long_term_ref_pics_present
        w.flag(false); // sps_temporal_mvp_enabled
        w.flag(false); // strong_intra_smoothing_enabled
        w.flag(true); // vui_parameters_present
        // VUI
        w.flag(true); // aspect_ratio_info_present
        w.u(8, 1); // square pixels
        w.flag(false); // overscan_info_present
        w.flag(true); // video_signal_type_present
        w.u(3, 5); // video_format: unspecified
        w.flag(self.color.full_range);
        w.flag(true); // colour_description_present
        w.u(8, self.primaries() as u64);
        w.u(8, self.transfer() as u64);
        w.u(8, 1); // matrix_coeffs: BT.709
        w.flag(false); // chroma_loc_info_present
        w.flag(false); // neutral_chroma_indication
        w.flag(false); // field_seq
        w.flag(false); // frame_field_info_present
        w.flag(false); // default_display_window
        w.flag(true); // vui_timing_info_present
        w.u(32, 1);
        w.u(32, self.pic_rate as u64);
        w.flag(false); // poc_proportional_to_timing
        w.flag(false); // vui_hrd_parameters_present
        w.flag(false); // bitstream_restriction
        w.flag(false); // sps_extension_present
        w.trailing();
        nal(NAL_SPS, &w.into_bytes())
    }

    pub fn pps(&self) -> Vec<u8> {
        let mut w = BitWriter::default();
        w.ue(0); // pps_pic_parameter_set_id
        w.ue(0); // pps_seq_parameter_set_id
        w.flag(true); // dependent_slice_segments_enabled (what radeonsi advertises)
        w.flag(false); // output_flag_present
        w.u(3, 0); // num_extra_slice_header_bits
        w.flag(false); // sign_data_hiding
        w.flag(false); // cabac_init_present
        w.ue(0); // num_ref_idx_l0_default_active_minus1
        w.ue(0); // num_ref_idx_l1_default_active_minus1
        w.se(0); // init_qp_minus26
        w.flag(false); // constrained_intra_pred
        w.flag(true); // transform_skip_enabled
        w.flag(self.cu_qp_delta);
        if self.cu_qp_delta {
            w.ue(0); // diff_cu_qp_delta_depth
        }
        w.se(0); // pps_cb_qp_offset
        w.se(0); // pps_cr_qp_offset
        w.flag(false); // pps_slice_chroma_qp_offsets_present
        w.flag(false); // weighted_pred
        w.flag(false); // weighted_bipred
        w.flag(false); // transquant_bypass_enabled
        w.flag(false); // tiles_enabled
        w.flag(false); // entropy_coding_sync_enabled
        w.flag(true); // pps_loop_filter_across_slices_enabled
        w.flag(false); // deblocking_filter_control_present
        w.flag(false); // pps_scaling_list_data_present
        w.flag(false); // lists_modification_present
        w.ue(0); // log2_parallel_merge_level_minus2
        w.flag(false); // slice_segment_header_extension_present
        w.flag(false); // pps_extension_present
        w.trailing();
        nal(NAL_PPS, &w.into_bytes())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliceType {
    P = 1,
    I = 2,
}

/// One entry of the explicit short-term RPS: a picture `distance` POCs before the current one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RpsEntry {
    pub distance: u32,
    pub used: bool,
}

pub struct SliceHeader<'a> {
    pub nal_type: u8,
    pub slice_type: SliceType,
    pub poc: u32,
    /// Sorted by increasing distance.
    pub rps: &'a [RpsEntry],
    pub qp_delta: i32,
    /// Must match the SPS.
    pub sao: bool,
}

impl SliceHeader<'_> {
    /// The slice NAL header and slice_segment_header(), byte aligned (no slice data).
    pub fn write(&self) -> Vec<u8> {
        let mut w = BitWriter::default();
        w.flag(true); // first_slice_segment_in_pic
        if (16..=23).contains(&self.nal_type) {
            w.flag(false); // no_output_of_prior_pics
        }
        w.ue(0); // slice_pic_parameter_set_id
        w.ue(self.slice_type as u32);
        if self.nal_type != NAL_IDR_W_RADL {
            w.u(
                LOG2_MAX_POC_LSB,
                (self.poc % (1 << LOG2_MAX_POC_LSB)) as u64,
            );
            w.flag(false); // short_term_ref_pic_set_sps_flag
            // st_ref_pic_set(num_short_term_ref_pic_sets = 0): no inter-RPS prediction.
            w.ue(self.rps.len() as u32);
            w.ue(0); // num_positive_pics
            let mut prev = 0;
            for e in self.rps {
                debug_assert!(e.distance > prev && e.distance <= MAX_REF_DISTANCE);
                w.ue(e.distance - prev - 1);
                w.flag(e.used);
                prev = e.distance;
            }
        }
        if self.sao {
            w.flag(true); // slice_sao_luma
            w.flag(true); // slice_sao_chroma
        }
        if self.slice_type == SliceType::P {
            w.flag(false); // num_ref_idx_active_override
            w.ue(0); // five_minus_max_num_merge_cand
        }
        w.se(self.qp_delta);
        // Deblocking is on, so this is always present.
        w.flag(true); // slice_loop_filter_across_slices_enabled
        w.trailing(); // byte_alignment()
        nal(self.nal_type, &w.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits(f: impl FnOnce(&mut BitWriter)) -> Vec<u8> {
        let mut w = BitWriter::default();
        f(&mut w);
        w.trailing();
        w.into_bytes()
    }

    #[test]
    fn exp_golomb() {
        // ue(0)=1, ue(1)=010, ue(4)=00101, then the stop bit.
        assert_eq!(
            bits(|w| {
                w.ue(0);
                w.ue(1);
                w.ue(4);
            }),
            [0b1010_0010, 0b1100_0000]
        );
        // se(1)=010, se(-1)=011
        assert_eq!(
            bits(|w| {
                w.se(1);
                w.se(-1);
            }),
            [0b0100_1110]
        );
    }

    #[test]
    fn emulation_prevention() {
        assert_eq!(
            nal(1, &[0, 0, 1, 0, 0, 0, 5]),
            [0, 0, 0, 1, 2, 1, 0, 0, 3, 1, 0, 0, 3, 0, 5]
        );
    }

    #[test]
    fn levels() {
        let c = |w, h, r| SeqConfig {
            width: w,
            height: h,
            pic_rate: r,
            color: ColorTags::default(),
            cu_qp_delta: true,
            sao: false,
        };
        assert_eq!(c(3456, 544, 240).level_idc(), 153);
        assert_eq!(c(1920, 272, 240).level_idc(), 123);
        assert_eq!(c(1366, 192, 240).coded_width(), 1376);
    }
}
