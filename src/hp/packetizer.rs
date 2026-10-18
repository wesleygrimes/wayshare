//! HEVC RTP packetizer in Apple's HP format (RFC 7798 with deviations).

use std::borrow::Cow;

pub const CLOCK_RATE: u32 = 24_000;
pub const EXT_PROFILE: u16 = 0x9311;
pub const DEFAULT_MTU_PAYLOAD: usize = 1300;
// Control-trailer header bits (VideoProcessing control byte).
const CB_SCREEN_WINDOW: u8 = 0x01;
const CB_END_OF_FRAME: u8 = 0x02;
const CB_FRAME_SIZE: u8 = 0x08;
const CB_TILE_ORIGIN: u8 = 0x10;
const NAL_IRAP_FIRST: u8 = 16;
const NAL_IRAP_LAST: u8 = 23;
const NAL_VPS: u8 = 32;
const NAL_SPS: u8 = 33;
const NAL_PPS: u8 = 34;
const NAL_AP: u8 = 48;
const NAL_FU: u8 = 49;
const NAL_SEI_PREFIX: u8 = 39;
const NAL_SEI_SUFFIX: u8 = 40;
const SRTP_TAG_ROOM: usize = 10;

#[derive(Debug, Clone)]
pub struct PacketizerConfig {
    /// Tile `i` is sent from `ssrc_base + i` (the SSRC in our negotiation answer).
    pub ssrc_base: u32,
    pub payload_type: u8,
    pub tiles: u8,
    pub mtu_payload: usize,
    /// Must be nonzero: the receiver treats DON 0 as invalid.
    pub don_base: u16,
    pub frame_number_base: u16,
    /// First RTP sequence number of every tile.
    pub initial_seq: u16,
    /// Picture size signalled on keyframes.
    pub frame_size: (u16, u16),
    /// Tile `i` covers rows `i * tile_height ..`.
    pub tile_height: u16,
    /// Size signalled on the last tile of each frame. Apple sends the display's pixel size.
    pub display_size: (u16, u16),
}

impl PacketizerConfig {
    pub fn new(ssrc_base: u32, payload_type: u8, tiles: u8, frame_size: (u16, u16)) -> Self {
        PacketizerConfig {
            frame_size,
            tile_height: tile_height(frame_size, tiles),
            display_size: frame_size,
            ssrc_base,
            payload_type,
            tiles,
            mtu_payload: DEFAULT_MTU_PAYLOAD,
            don_base: rand::random::<u16>() | 1,
            frame_number_base: rand::random(),
            initial_seq: rand::random(),
        }
    }
}

pub struct Packetizer {
    cfg: PacketizerConfig,
    seq: Vec<u16>,
    frame: u32,
    don: u16,
    /// Latest VPS/SPS/PPS, and which tiles have not carried them since the last refresh.
    params: Vec<Vec<u8>>,
    needs_params: Vec<bool>,
}

/// Splits an Annex B byte stream into NAL units (start codes and trailing zeros removed).
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i..i + 3] == [0, 0, 1] {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .filter_map(|(k, &s)| {
            let end = starts.get(k + 1).map_or(data.len(), |&n| n - 3);
            let mut nal = &data[s..end];
            while let [rest @ .., 0] = nal {
                nal = rest;
            }
            (!nal.is_empty()).then_some(nal)
        })
        .collect()
}

fn nal_type(nal: &[u8]) -> u8 {
    (nal[0] >> 1) & 0x3f
}

fn tile_height(frame_size: (u16, u16), tiles: u8) -> u16 {
    frame_size
        .1
        .div_ceil(tiles.max(1).into())
        .next_multiple_of(16)
}

impl Packetizer {
    pub fn new(cfg: PacketizerConfig) -> Self {
        let seq = vec![cfg.initial_seq; cfg.tiles.into()];
        Packetizer {
            don: cfg.don_base,
            needs_params: vec![true; cfg.tiles.into()],
            cfg,
            seq,
            frame: 0,
            params: Vec::new(),
        }
    }

    pub fn config(&self) -> &PacketizerConfig {
        &self.cfg
    }

    /// Frames packetized so far.
    pub fn frame_count(&self) -> u32 {
        self.frame
    }

    /// DON of the next tile-frame sent.
    pub fn next_don(&self) -> u16 {
        self.don
    }

    /// Switches to a new picture size mid-stream; the next frame must be a refresh. The receiver
    /// drops everything after a DON or SSRC change, so both continue.
    pub fn set_frame_size(&mut self, frame_size: (u16, u16)) {
        self.cfg.frame_size = frame_size;
        self.cfg.tile_height = tile_height(frame_size, self.cfg.tiles);
        self.cfg.display_size = frame_size;
    }

    /// Packetizes one capture frame. `tiles[i]` is the Annex B access unit for tile `i`,
    /// or `None` if that tile is unchanged and sends nothing. DONs stay consecutive across
    /// the tiles actually sent, as Apple's sender does.
    pub fn packetize(&mut self, tiles: &[Option<&[u8]>], rtp_ts: u32) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        self.packetize_each(tiles, rtp_ts, |p| out.push(p));
        out
    }

    /// Delivers packets in wire order, buffering at most one tile's packets at a time.
    pub fn packetize_each(
        &mut self,
        tiles: &[Option<&[u8]>],
        rtp_ts: u32,
        mut emit: impl FnMut(Vec<u8>),
    ) {
        let tiles = &tiles[..tiles.len().min(self.cfg.tiles.into())];
        let last = tiles.iter().rposition(Option::is_some);
        let nals: Vec<_> = tiles.iter().map(|au| au.map(split_annexb)).collect();
        // Every tile of a frame that starts with an IRAP picture is marked as a refresh.
        let refresh = nals
            .iter()
            .flatten()
            .flatten()
            .any(|n| (NAL_IRAP_FIRST..=NAL_IRAP_LAST).contains(&nal_type(n)));
        for (i, nals) in nals.into_iter().enumerate() {
            let Some(nals) = nals else { continue };
            let payloads = self.payloads(nals, self.don, i, refresh, Some(i) == last);
            self.don = self.don.wrapping_add(1);
            let count = payloads.len() as u16;
            let fnum = self.cfg.frame_number_base.wrapping_add(self.frame as u16);
            let ssrc = self.cfg.ssrc_base.wrapping_add(i as u32);
            let len = payloads.len();
            for (k, mut p) in payloads.into_iter().enumerate() {
                let marker = if k + 1 == len { 0x80 } else { 0 };
                p[..2].copy_from_slice(&[0x90, marker | self.cfg.payload_type]);
                p[2..4].copy_from_slice(&self.seq[i].to_be_bytes());
                p[4..8].copy_from_slice(&rtp_ts.to_be_bytes());
                p[8..12].copy_from_slice(&ssrc.to_be_bytes());
                p[12..14].copy_from_slice(&EXT_PROFILE.to_be_bytes());
                p[14..16].copy_from_slice(&1u16.to_be_bytes());
                p[16..18].copy_from_slice(&count.to_be_bytes());
                p[18..20].copy_from_slice(&fnum.to_be_bytes());
                emit(p);
                self.seq[i] = self.seq[i].wrapping_add(1);
            }
        }
        self.frame = self.frame.wrapping_add(1);
    }

    /// Fields in ascending bit order, then the header byte. Bit layout comes from the
    /// negotiated feature string ("SW:0;EOD:1;HTS:2;RR:3;POSE:4;POS:5").
    fn trailer(&self, tile: u16, keyframe: bool, last: bool) -> Vec<u8> {
        let mut header = CB_TILE_ORIGIN;
        let mut t = Vec::with_capacity(23);
        if last {
            header |= CB_SCREEN_WINDOW | CB_END_OF_FRAME;
            let (w, h) = self.cfg.display_size;
            for v in [0, 0, w, h] {
                t.extend_from_slice(&v.to_be_bytes());
            }
        }
        if keyframe {
            header |= CB_FRAME_SIZE;
            t.extend_from_slice(&self.cfg.frame_size.0.to_be_bytes());
            t.extend_from_slice(&self.cfg.frame_size.1.to_be_bytes());
        }
        t.extend_from_slice(&[0, 0]);
        t.extend_from_slice(&(tile * self.cfg.tile_height).to_be_bytes());
        t.extend_from_slice(&[0, 0, header]);
        t
    }

    fn payloads(
        &mut self,
        nals: Vec<&[u8]>,
        don: u16,
        tile: usize,
        refresh: bool,
        last_tile: bool,
    ) -> Vec<Vec<u8>> {
        let donl = don.to_be_bytes();
        let mtu = self.cfg.mtu_payload;
        let nals: Vec<&[u8]> = nals
            .into_iter()
            .filter(|n| n.len() >= 2 && !matches!(nal_type(n), NAL_SEI_PREFIX | NAL_SEI_SUFFIX))
            .collect();
        let (mut non_vcl, vcl): (Vec<&[u8]>, Vec<&[u8]>) =
            nals.into_iter().partition(|n| nal_type(n) >= 32);
        let params: Vec<Vec<u8>> = non_vcl
            .iter()
            .filter(|n| (NAL_VPS..=NAL_PPS).contains(&nal_type(n)))
            .map(|n| n.to_vec())
            .collect();
        // Every refresh resets the receiver's per-tile packet buffers, and each needs the
        // parameter sets again ("Need image description"), even when they are unchanged.
        if !params.is_empty() {
            self.params = params;
            self.needs_params.fill(true);
        }
        let cached;
        if self.needs_params[tile] && !non_vcl.iter().any(|n| nal_type(n) == NAL_SPS) {
            cached = self.params.clone();
            non_vcl.splice(0..0, cached.iter().map(Vec::as_slice));
        }
        self.needs_params[tile] = false;
        let tile = tile as u16;
        let mut out = Vec::new();

        let mut ap: Vec<u8> = Vec::new();
        for n in non_vcl {
            if !ap.is_empty() && ap.len() - 20 + 2 + n.len() > mtu {
                out.push(std::mem::take(&mut ap));
            }
            if ap.is_empty() {
                ap = packet_buffer(mtu);
                ap.extend_from_slice(&[(n[0] & 0x81) | NAL_AP << 1, n[1]]);
                ap.extend_from_slice(&donl);
            }
            ap.extend_from_slice(&(n.len() as u16).to_be_bytes());
            ap.extend_from_slice(n);
        }
        if !ap.is_empty() {
            out.push(ap);
        }

        let last = vcl.len().wrapping_sub(1);
        for (k, n) in vcl.into_iter().enumerate() {
            let body: Cow<'_, [u8]> = if k == last {
                let trailer = self.trailer(tile, refresh, last_tile);
                // Each trailer byte can add at most one emulation-prevention byte.
                let mut body = Vec::with_capacity(n.len() - 2 + 2 * trailer.len());
                body.extend_from_slice(&n[2..]);
                append_escaped(&mut body, &trailer);
                Cow::Owned(body)
            } else {
                Cow::Borrowed(&n[2..])
            };
            if body.len() + 4 <= mtu {
                let mut p = packet_buffer(body.len() + 4);
                p.extend_from_slice(&n[..2]);
                p.extend_from_slice(&donl);
                p.extend_from_slice(&body);
                out.push(p);
                continue;
            }
            let hdr = [(n[0] & 0x81) | NAL_FU << 1, n[1]];
            // A FU with both S and E set is invalid ("Bad Slice size"), so always split in two or more.
            let chunks: Vec<&[u8]> = body.chunks(mtu.min(body.len().div_ceil(2))).collect();
            for (c, chunk) in chunks.iter().enumerate() {
                let start = if c == 0 { 0x80 } else { 0 };
                let end = if c + 1 == chunks.len() { 0x40 } else { 0 };
                let mut p = packet_buffer(chunk.len() + 5);
                p.extend_from_slice(&hdr);
                p.push(start | end | nal_type(n));
                p.extend_from_slice(&donl);
                p.extend_from_slice(chunk);
                out.push(p);
            }
        }
        out
    }
}

fn packet_buffer(payload_capacity: usize) -> Vec<u8> {
    let mut p = Vec::with_capacity(20 + payload_capacity + SRTP_TAG_ROOM);
    p.resize(20, 0);
    p
}

/// Appends `bytes` to NAL payload `nal`, inserting emulation-prevention bytes as needed.
fn append_escaped(nal: &mut Vec<u8>, bytes: &[u8]) {
    let mut zeros = nal.iter().rev().take(2).take_while(|&&b| b == 0).count();
    for &b in bytes {
        if zeros >= 2 && b <= 3 {
            nal.push(3);
            zeros = 0;
        }
        nal.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
}

/// RTP timestamp (24 kHz) for a capture time measured from stream start.
pub fn rtp_timestamp(since_start: std::time::Duration, offset: u32) -> u32 {
    let ticks = since_start.as_micros() * u128::from(CLOCK_RATE) / 1_000_000;
    offset.wrapping_add(ticks as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nal(t: u8, len: usize) -> Vec<u8> {
        let mut n = vec![t << 1, 1];
        n.extend((0..len - 2).map(|i| (i % 251) as u8 + 1));
        n
    }

    fn annexb(nals: &[Vec<u8>]) -> Vec<u8> {
        nals.iter()
            .flat_map(|n| [&[0, 0, 0, 1][..], n].concat())
            .collect()
    }

    #[test]
    fn set_frame_size_keeps_don() {
        let mut pk = Packetizer::new(cfg());
        let don = pk.next_don();
        pk.set_frame_size((2560, 1440));
        let c = pk.config();
        assert_eq!(
            (c.frame_size, c.tile_height, c.display_size),
            ((2560, 1440), 368, (2560, 1440))
        );
        assert_eq!(pk.next_don(), don);
    }

    fn cfg() -> PacketizerConfig {
        PacketizerConfig {
            ssrc_base: 1000,
            payload_type: 100,
            tiles: 4,
            mtu_payload: 1300,
            don_base: 0x1234,
            frame_number_base: 0x2000,
            initial_seq: 0xfffe,
            frame_size: (1920, 1080),
            tile_height: 272,
            display_size: (5120, 2880),
        }
    }

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Trailers cut from Apple's own last-VCL NALs (loopback capture), escaped as on the wire.
    #[test]
    fn trailer_matches_apple() {
        let p = Packetizer::new(cfg());
        for (tile, key, last, apple) in [
            (0, false, false, "000003000003000010"),
            (1, false, false, "0000030110000010"),
            (3, false, true, "000003000014000b400000030330000013"),
            (0, true, false, "07800438000003000003000018"),
            (3, true, true, "000003000014000b4007800438000003033000001b"),
            (0, false, true, "000003000014000b40000003000003000013"),
        ] {
            let mut body = vec![0x80];
            append_escaped(&mut body, &p.trailer(tile, key, last));
            assert_eq!(body[1..], hex(apple), "tile {tile} key {key} last {last}");
        }
    }

    #[test]
    fn split() {
        let d = [0, 0, 0, 1, 0x40, 1, 9, 0, 0, 1, 0x42, 1, 0, 0];
        assert_eq!(split_annexb(&d), vec![&[0x40, 1, 9][..], &[0x42, 1][..]]);
    }

    #[test]
    fn idr_frame_layout() {
        let (vps, sps, pps, sei, idr) = (
            nal(32, 24),
            nal(33, 40),
            nal(34, 8),
            nal(39, 500),
            nal(19, 3000),
        );
        let au = annexb(&[vps.clone(), sps.clone(), pps.clone(), sei, idr.clone()]);
        let mut p = Packetizer::new(cfg());
        let small = annexb(&[nal(1, 100)]);
        let pkts = p.packetize(&[Some(&au), None, Some(&small), None], 4800);

        let t0: Vec<_> = pkts
            .iter()
            .filter(|p| p[8..12] == 1000u32.to_be_bytes())
            .collect();
        assert_eq!(t0.len(), 1 + 3);
        for (k, pk) in t0.iter().enumerate() {
            assert_eq!(pk[0], 0x90);
            assert_eq!(pk[1], 100 | if k == 3 { 0x80 } else { 0 });
            assert_eq!(pk[4..8], 4800u32.to_be_bytes());
            assert_eq!(pk[12..20], [0x93, 0x11, 0, 1, 0, 4, 0x20, 0]);
        }
        let seq0 = u16::from_be_bytes([t0[0][2], t0[0][3]]);
        assert_eq!(
            u16::from_be_bytes([t0[3][2], t0[3][3]]),
            seq0.wrapping_add(3)
        );

        let ap = &t0[0][20..];
        assert_eq!(nal_type(ap), NAL_AP);
        assert_eq!(ap[2..4], [0x12, 0x34]);
        let mut o = 4;
        for n in [&vps, &sps, &pps] {
            assert_eq!(usize::from(u16::from_be_bytes([ap[o], ap[o + 1]])), n.len());
            assert_eq!(&ap[o + 2..o + 2 + n.len()], n.as_slice());
            o += 2 + n.len();
        }
        assert_eq!(o, ap.len(), "no DOND, no SEI");

        let mut body = Vec::new();
        for (k, pk) in t0[1..].iter().enumerate() {
            let fu = &pk[20..];
            assert_eq!(nal_type(fu), NAL_FU);
            assert_eq!(fu[2] & 0x3f, 19);
            assert_eq!(fu[2] & 0x80 != 0, k == 0);
            assert_eq!(fu[2] & 0x40 != 0, k == 2);
            assert_eq!(fu[3..5], [0x12, 0x34], "DONL in every fragment");
            body.extend_from_slice(&fu[5..]);
        }
        assert_eq!(
            body,
            [&idr[2..], &hex("07800438000003000003000018")].concat(),
            "keyframe trailer, not last tile"
        );

        let t2: Vec<_> = pkts
            .iter()
            .filter(|p| p[8..12] == 1002u32.to_be_bytes())
            .collect();
        assert_eq!(
            t2.len(),
            2,
            "first picture of tile 2 gets the cached parameter sets"
        );
        assert_eq!(nal_type(&t2[0][20..]), NAL_AP);
        assert_eq!(t2[0][20 + 4..20 + 6], (vps.len() as u16).to_be_bytes());
        assert_eq!(t2[1][12..20], [0x93, 0x11, 0, 1, 0, 2, 0x20, 0]);
        assert_eq!(
            t2[1][20..24],
            [2, 1, 0x12, 0x35],
            "DON skips nothing for the unsent tile 1"
        );
        assert!(
            t2[1].ends_with(&hex("000003000014000b4007800438000003022000001b")),
            "last tile of a refresh frame"
        );
        assert_eq!(t2[1].len(), 20 + 100 + 2 + 21);
        let pkts = p.packetize(&[None, None, Some(&small), None], 5200);
        assert_eq!(pkts.len(), 1, "parameter sets only once per tile");
        let pkts = p.packetize(&[Some(&au), Some(&small), None, None], 5600);
        let t1: Vec<_> = pkts
            .iter()
            .filter(|p| p[8..12] == 1001u32.to_be_bytes())
            .collect();
        assert_eq!(
            nal_type(&t1[0][20..]),
            NAL_AP,
            "a refresh with unchanged parameter sets re-sends them to every tile"
        );
    }

    #[test]
    fn multiple_slices_only_append_the_trailer_to_the_last_slice() {
        let first = nal(1, 3000);
        let last = nal(1, 50);
        let au = annexb(&[first.clone(), last.clone()]);
        let mut pk = Packetizer::new(cfg());
        let packets = pk.packetize(&[Some(&au)], 0);
        assert_eq!(packets.len(), 4);
        let mut body = Vec::new();
        for p in &packets[..3] {
            assert_eq!(nal_type(&p[20..]), NAL_FU);
            assert_eq!(p[1] & 0x80, 0);
            body.extend_from_slice(&p[25..]);
        }
        assert_eq!(body, first[2..]);
        assert_eq!(packets[3][1] & 0x80, 0x80);
        assert_eq!(&packets[3][24..24 + last.len() - 2], &last[2..]);
        assert!(packets[3].ends_with(&hex("000003000014000b40000003000003000013")));
    }

    #[test]
    fn streaming_refresh_on_later_tile_marks_every_tile() {
        let mut pk = Packetizer::new(cfg());
        let inter = annexb(&[nal(1, 50)]);
        let refresh = annexb(&[nal(19, 3000)]);
        let mut packets = Vec::new();
        pk.packetize_each(&[Some(&inter), None, Some(&refresh)], 1234, |p| {
            assert!(p.capacity() >= p.len() + SRTP_TAG_ROOM);
            packets.push(p);
        });
        assert_eq!(packets.len(), 4);
        assert_eq!(&packets[0][8..12], &1000u32.to_be_bytes());
        assert!(packets[0].ends_with(&hex("07800438000003000003000018")));
        for (k, p) in packets[1..].iter().enumerate() {
            assert_eq!(&p[8..12], &1002u32.to_be_bytes());
            assert_eq!(&p[4..8], &1234u32.to_be_bytes());
            assert_eq!(&p[16..18], &3u16.to_be_bytes());
            assert_eq!(&p[23..25], &0x1235u16.to_be_bytes());
            assert_eq!(p[1] & 0x80 != 0, k == 2);
        }
        assert!(packets[3].ends_with(&hex("000003000014000b4007800438000003022000001b")));
        assert_eq!(pk.next_don(), 0x1236);
        assert_eq!(pk.frame_count(), 1);
    }

    #[test]
    fn don_advances_per_frame() {
        let mut p = Packetizer::new(cfg());
        let au = annexb(&[nal(1, 50)]);
        p.packetize(&[Some(au.as_slice()); 4], 0);
        let pkts = p.packetize(&[Some(au.as_slice()); 4], 400);
        let dons: Vec<u16> = pkts
            .iter()
            .map(|p| u16::from_be_bytes([p[22], p[23]]))
            .collect();
        assert_eq!(dons, [0x1238, 0x1239, 0x123a, 0x123b]);
        assert!(pkts.iter().all(|p| p[18..20] == [0x20, 1]));
        let pkts = p.packetize(&[None, Some(au.as_slice()), None, Some(au.as_slice())], 800);
        let dons: Vec<u16> = pkts
            .iter()
            .map(|p| u16::from_be_bytes([p[22], p[23]]))
            .collect();
        assert_eq!(dons, [0x123c, 0x123d]);
    }

    #[test]
    fn fu_never_single_fragment() {
        let mut p = Packetizer::new(cfg());
        for len in [1297, 1298, 1299, 1300, 1301, 2600, 2603] {
            let au = annexb(&[nal(1, len)]);
            let pkts = p.packetize(&[Some(au.as_slice())], 0);
            if pkts.len() == 1 {
                assert_ne!(nal_type(&pkts[0][20..]), NAL_FU);
                continue;
            }
            assert!(
                pkts.iter()
                    .all(|p| nal_type(&p[20..]) == NAL_FU && p.len() <= 20 + 5 + 1300)
            );
            assert_eq!(pkts[0][22] & 0xc0, 0x80, "len {len}");
            assert_eq!(pkts.last().unwrap()[22] & 0xc0, 0x40);
            let total: usize = pkts.iter().map(|p| p.len() - 25).sum();
            assert_eq!(total, len - 2 + 18);
        }
    }

    #[test]
    fn timestamp() {
        assert_eq!(
            rtp_timestamp(std::time::Duration::from_millis(1000), 5),
            24_005
        );
    }
}
