//! RTCP for the HP video stream: our SR+SDES, and receiver feedback (RR, DON acks, RCTL, FIR).

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, ensure};

pub const PT_FIR: u8 = 192;
pub const PT_SR: u8 = 200;
pub const PT_RR: u8 = 201;
pub const PT_SDES: u8 = 202;
pub const PT_BYE: u8 = 203;
pub const PT_APP: u8 = 204;
pub const PT_PSFB: u8 = 206;
const PSFB_PLI: u8 = 1;
const PSFB_FIR: u8 = 4;
pub const APP_DON_ACK: [u8; 4] = [0, 0, 0, 5];
pub const APP_RATE_CONTROL: [u8; 4] = *b"RCTL";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReportBlock {
    pub ssrc: u32,
    pub fraction_lost: u8,
    pub cumulative_lost: u32,
    pub highest_seq: u32,
    pub jitter: u32,
    pub last_sr: u32,
    pub delay_since_last_sr: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RtcpEvent {
    SenderReport {
        ssrc: u32,
        ntp: u64,
        rtp_ts: u32,
        packets: u32,
        octets: u32,
        blocks: Vec<ReportBlock>,
    },
    ReceiverReport {
        ssrc: u32,
        blocks: Vec<ReportBlock>,
    },
    /// APP `0x00000005`: the receiver decoded the frame carrying `don`.
    DonAck {
        ssrc: u32,
        don: u32,
    },
    /// APP `RCTL`: rate-control feedback, payload not yet decoded.
    RateControl {
        ssrc: u32,
        subtype: u8,
        data: Vec<u8>,
    },
    App {
        ssrc: u32,
        subtype: u8,
        name: [u8; 4],
        data: Vec<u8>,
    },
    /// Legacy FIR (PT 192, what Apple's receiver sends) or PSFB PLI/FIR (PT 206):
    /// send a refresh frame.
    KeyframeRequest {
        ssrc: u32,
    },
    Sdes,
    Bye {
        ssrcs: Vec<u32>,
    },
    Other {
        pt: u8,
        data: Vec<u8>,
    },
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}

fn report_blocks(body: &[u8], count: u8) -> Vec<ReportBlock> {
    body.as_chunks::<24>()
        .0
        .iter()
        .take(count.into())
        .map(|b| ReportBlock {
            ssrc: be32(b, 0),
            fraction_lost: b[4],
            cumulative_lost: be32(b, 4) & 0x00ff_ffff,
            highest_seq: be32(b, 8),
            jitter: be32(b, 12),
            last_sr: be32(b, 16),
            delay_since_last_sr: be32(b, 20),
        })
        .collect()
}

/// Parses a (decrypted) compound RTCP packet.
pub fn parse(buf: &[u8]) -> Result<Vec<RtcpEvent>> {
    let mut out = Vec::new();
    let mut o = 0;
    while o + 4 <= buf.len() {
        let len = (usize::from(u16::from_be_bytes([buf[o + 2], buf[o + 3]])) + 1) * 4;
        ensure!(buf[o] >> 6 == 2, "bad RTCP version");
        ensure!(o + len <= buf.len(), "truncated RTCP packet");
        let p = &buf[o..o + len];
        let count = p[0] & 0x1f;
        let pt = p[1];
        let ssrc = if len >= 8 { be32(p, 4) } else { 0 };
        out.push(match pt {
            PT_SR if len >= 28 => RtcpEvent::SenderReport {
                ssrc,
                ntp: u64::from(be32(p, 8)) << 32 | u64::from(be32(p, 12)),
                rtp_ts: be32(p, 16),
                packets: be32(p, 20),
                octets: be32(p, 24),
                blocks: report_blocks(&p[28..], count),
            },
            PT_RR if len >= 8 => RtcpEvent::ReceiverReport {
                ssrc,
                blocks: report_blocks(&p[8..], count),
            },
            PT_APP if len >= 12 => {
                let name: [u8; 4] = p[8..12].try_into()?;
                let data = &p[12..];
                match name {
                    APP_DON_ACK if data.len() >= 4 => RtcpEvent::DonAck {
                        ssrc,
                        don: be32(data, 0),
                    },
                    APP_RATE_CONTROL => RtcpEvent::RateControl {
                        ssrc,
                        subtype: count,
                        data: data.to_vec(),
                    },
                    _ => RtcpEvent::App {
                        ssrc,
                        subtype: count,
                        name,
                        data: data.to_vec(),
                    },
                }
            }
            PT_FIR if len >= 8 => RtcpEvent::KeyframeRequest { ssrc },
            PT_PSFB if len >= 12 && matches!(count, PSFB_PLI | PSFB_FIR) => {
                RtcpEvent::KeyframeRequest { ssrc }
            }
            PT_SDES => RtcpEvent::Sdes,
            PT_BYE => RtcpEvent::Bye {
                ssrcs: p[4..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .take(count.into())
                    .map(|c| be32(c, 0))
                    .collect(),
            },
            _ => RtcpEvent::Other {
                pt,
                data: p.to_vec(),
            },
        });
        o += len;
    }
    Ok(out)
}

/// Current wall clock as a 64-bit NTP timestamp.
pub fn ntp_now() -> u64 {
    let d = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() + 2_208_988_800;
    (secs << 32) | ((u64::from(d.subsec_nanos()) << 32) / 1_000_000_000)
}

/// SR from `ssrc` (with one empty report block for the viewer's `remote_ssrc`, as Apple sends)
/// followed by SDES with an empty CNAME.
pub fn sender_report(
    ssrc: u32,
    remote_ssrc: u32,
    ntp: u64,
    rtp_ts: u32,
    packets: u32,
    octets: u32,
) -> Vec<u8> {
    let mut p = Vec::with_capacity(52 + 16 + 14);
    p.extend_from_slice(&[0x81, PT_SR, 0, 12]);
    for w in [
        ssrc,
        (ntp >> 32) as u32,
        ntp as u32,
        rtp_ts,
        packets,
        octets,
        remote_ssrc,
    ] {
        p.extend_from_slice(&w.to_be_bytes());
    }
    p.extend_from_slice(&[0; 20]);
    p.extend_from_slice(&[0x81, PT_SDES, 0, 2]);
    p.extend_from_slice(&ssrc.to_be_bytes());
    p.extend_from_slice(&[1, 0, 0, 0]);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn apple_sr_layout() {
        // Apple's own sender, loopback capture.
        let apple = hex(
            "81c8000cc6df153eee7957daab6a400000005838000000550000b9f3e8ed5b060000000100000000000000000000000000000000\
                         81ca0002c6df153e01000000",
        );
        let ours = sender_report(
            0xc6df153e,
            0xe8ed5b06,
            0xee7957da_ab6a4000,
            0x5838,
            0x55,
            0xb9f3,
        );
        assert_eq!(ours.len(), apple.len());
        assert_eq!(ours[..32], apple[..32]);
        let ev = parse(&ours).unwrap();
        assert!(
            matches!(&ev[0], RtcpEvent::SenderReport { ssrc: 0xc6df153e, packets: 0x55, blocks, .. } if blocks[0].ssrc == 0xe8ed5b06)
        );
        assert_eq!(ev[1], RtcpEvent::Sdes);
    }

    #[test]
    fn feedback() {
        let rr = hex(
            "81c90007e8ed5b06c6df153e000000010000000000000000000000000000000081ca0002e8ed5b0601000000",
        );
        let ev = parse(&rr).unwrap();
        let RtcpEvent::ReceiverReport {
            ssrc: 0xe8ed5b06,
            blocks,
        } = &ev[0]
        else {
            panic!("{ev:?}")
        };
        assert_eq!((blocks[0].ssrc, blocks[0].cumulative_lost), (0xc6df153e, 1));

        let mut app = hex("80cc0003e8ed5b0600000005");
        app.extend_from_slice(&0x1238u32.to_be_bytes());
        app.extend_from_slice(&hex("80c00001e8ed5b06"));
        app.extend_from_slice(&hex("81cc0003e8ed5b06"));
        app.extend_from_slice(b"RCTL");
        app.extend_from_slice(&[1, 2, 3, 4]);
        let ev = parse(&app).unwrap();
        assert_eq!(
            ev[0],
            RtcpEvent::DonAck {
                ssrc: 0xe8ed5b06,
                don: 0x1238
            }
        );
        assert_eq!(ev[1], RtcpEvent::KeyframeRequest { ssrc: 0xe8ed5b06 });
        let psfb = hex("81ce0002e8ed5b06c6df153e84ce0004e8ed5b0600000000c6df153e01000000");
        assert_eq!(
            parse(&psfb).unwrap(),
            vec![RtcpEvent::KeyframeRequest { ssrc: 0xe8ed5b06 }; 2]
        );
        assert_eq!(
            ev[2],
            RtcpEvent::RateControl {
                ssrc: 0xe8ed5b06,
                subtype: 1,
                data: vec![1, 2, 3, 4]
            }
        );
        assert!(parse(&app[..10]).is_err());
    }
}
