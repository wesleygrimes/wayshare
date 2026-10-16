//! RTP for HP audio: one raw AAC-ELD access unit per packet, no AU headers.
//!
//! The negotiated AVC mode 8 runs as internal VC mode 9, for which
//! `+[VCAudioStreamConfig bundlingSchemeForAudioStreamMode:payloadType:]` returns scheme 0
//! (no bundling). RFC 3640 AU headers (scheme 2) decode to silence.

use crate::hp::srtp::TAG_LEN;

pub const PAYLOAD_TYPE: u8 = 101;
pub const CLOCK_RATE: u32 = 48_000;
pub const SAMPLES_PER_FRAME: u32 = 480;
pub const CHANNELS: u32 = 2;
pub const MAX_AU: usize = 1400;

pub struct AudioPacketizer {
    ssrc: u32,
    payload_type: u8,
    seq: u16,
    ts: u32,
    talkspurt: bool,
}

impl AudioPacketizer {
    pub fn new(ssrc: u32, payload_type: u8, seq: u16, ts: u32) -> Self {
        Self {
            ssrc,
            payload_type,
            seq,
            ts,
            talkspurt: true,
        }
    }

    pub fn ssrc(&self) -> u32 {
        self.ssrc
    }

    /// Timestamp the next packet will carry.
    pub fn timestamp(&self) -> u32 {
        self.ts
    }

    /// One RTP packet for one encoded frame; advances seq by 1 and the timestamp by a frame.
    /// The marker bit flags the first packet of a talkspurt. Capacity leaves room for the
    /// SRTP tag. Empty or oversized AUs return `None`.
    pub fn packet(&mut self, au: &[u8]) -> Option<Vec<u8>> {
        if au.is_empty() || au.len() > MAX_AU {
            return None;
        }
        let mut p = Vec::with_capacity(12 + au.len() + TAG_LEN);
        p.push(0x80);
        p.push(self.payload_type | if self.talkspurt { 0x80 } else { 0 });
        p.extend_from_slice(&self.seq.to_be_bytes());
        p.extend_from_slice(&self.ts.to_be_bytes());
        p.extend_from_slice(&self.ssrc.to_be_bytes());
        p.extend_from_slice(au);
        self.talkspurt = false;
        self.seq = self.seq.wrapping_add(1);
        self.ts = self.ts.wrapping_add(SAMPLES_PER_FRAME);
        Some(p)
    }

    /// A frame that is not sent (silence suppression): the clock advances and the next packet
    /// starts a new talkspurt.
    pub fn skip_frame(&mut self) {
        self.ts = self.ts.wrapping_add(SAMPLES_PER_FRAME);
        self.talkspurt = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_clock() {
        let mut pk = AudioPacketizer::new(0x11223344, PAYLOAD_TYPE, 0xffff, 0xffff_fe00);
        let au = [0x11u8; 100];
        let p = pk.packet(&au).unwrap();
        assert_eq!(
            &p[..12],
            &[
                0x80,
                0x80 | 101,
                0xff,
                0xff,
                0xff,
                0xff,
                0xfe,
                0x00,
                0x11,
                0x22,
                0x33,
                0x44
            ]
        );
        assert_eq!(&p[12..], &au);
        assert!(p.capacity() >= p.len() + TAG_LEN);
        let q = pk.packet(&au).unwrap();
        assert_eq!(&q[1..8], &[101, 0, 0, 0xff, 0xff, 0xff, 0xe0]);
        pk.skip_frame();
        let r = pk.packet(&au).unwrap();
        assert_eq!(r[1], 0x80 | 101);
        assert_eq!(&r[4..8], &0x0000_03a0u32.to_be_bytes());
        assert!(pk.packet(&[]).is_none());
        assert!(pk.packet(&[0; MAX_AU + 1]).is_none());
    }
}
