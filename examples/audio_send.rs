//! audio_send <rx.json> <tone.aus> [--srtp] [--seconds N] [--seq-base HEX]
//! audio_send encode <out.aus> <seconds> [bitrate] [hz] [dbfs] [sbr|nosbr] [bandwidth]  (Linux: fdk-aac tone, hz 0 = noise; default 1 kHz at -70 dBFS)
//!
//! audio_send stream <host:port> <seconds>             (Linux: full AudioSender from the monitor, dummy keys)
//! audio_send capture <seconds>                       (Linux: read the default sink monitor, print peak level only)
//!
//! `.aus` = u16 BE length + AAC-ELD AU, repeated. Sends AUs every 10 ms through the audio
//! packetizer (optionally SRTP) to the `audiorx` harness (127.0.0.1:41014 -> :41002) with an SR
//! once a second, and reports the receiver's RTCP.

use std::net::UdpSocket;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use wayshare::audio::rtp::{AudioPacketizer, CLOCK_RATE, PAYLOAD_TYPE};
use wayshare::hp::rtcp::{self, RtcpEvent};
use wayshare::hp::srtp::{MASTER_LEN, SrtpContext, is_rtcp};

fn json_field(json: &str, key: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let f = v
        .get(key)
        .with_context(|| format!("rx.json has no {key}"))?;
    Ok(f.as_str()
        .map(str::to_string)
        .unwrap_or_else(|| f.to_string()))
}

fn base64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let v: Vec<u8> = s.bytes().filter(|&c| c != b'=').map(val).collect();
    v.chunks(4)
        .flat_map(|c| {
            let n = c
                .iter()
                .enumerate()
                .fold(0u32, |a, (i, &x)| a | u32::from(x) << (18 - 6 * i));
            n.to_be_bytes()[1..c.len()].to_vec()
        })
        .collect()
}

fn read_aus(path: &str) -> Result<Vec<Vec<u8>>> {
    let data = std::fs::read(path)?;
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 <= data.len() {
        let n = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        i += 2;
        if i + n > data.len() {
            bail!("truncated AU file");
        }
        out.push(data[i..i + n].to_vec());
        i += n;
    }
    Ok(out)
}

#[cfg(target_os = "linux")]
fn encode(
    out: &str,
    secs: f64,
    bitrate: u32,
    freq: f64,
    dbfs: f64,
    sbr: bool,
    bandwidth: u32,
) -> Result<()> {
    let amp = 32767.0 * 10f64.powf(dbfs / 20.0);
    use wayshare::audio::fdk::{AacEldEncoder, EncoderConfig};
    let mut enc = AacEldEncoder::new(&EncoderConfig {
        bitrate,
        sbr,
        bandwidth,
    })?;
    println!(
        "fdk ASC {:02x?} frame {}",
        enc.audio_specific_config(),
        enc.frame_length()
    );
    let frames = (secs * 100.0) as usize;
    let mut file = Vec::new();
    let mut n = 0u64;
    let mut sizes = 0usize;
    for _ in 0..frames {
        let pcm: Vec<i16> = (0..480)
            .flat_map(|_| {
                let x = if freq > 0.0 {
                    (2.0 * std::f64::consts::PI * freq * n as f64 / 48000.0).sin()
                } else {
                    let mut z = n.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15);
                    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                    z ^= z >> 31;
                    2.0 * (z >> 11) as f64 / (1u64 << 53) as f64 - 1.0
                };
                let s = (amp * x) as i16;
                n += 1;
                [s, s]
            })
            .collect();
        if let Some(au) = enc.encode(&pcm)? {
            file.extend_from_slice(&(au.len() as u16).to_be_bytes());
            file.extend_from_slice(&au);
            sizes += au.len();
        }
    }
    std::fs::write(out, file)?;
    println!("{frames} frames in, avg AU {} bytes", sizes / frames.max(1));
    Ok(())
}

#[cfg(target_os = "linux")]
fn capture(secs: u64) -> Result<()> {
    use wayshare::audio::capture::MonitorCapture;
    let mut cap = MonitorCapture::open(None)?;
    let mut pcm = [0i16; 960];
    for s in 0..secs {
        let mut peak = 0i32;
        for _ in 0..100 {
            cap.read_frame(&mut pcm)?;
            peak = peak.max(pcm.iter().map(|&x| (x as i32).abs()).max().unwrap_or(0));
        }
        println!(
            "sec {s}: 100 frames, peak {:.1} dBFS",
            20.0 * ((peak.max(1)) as f64 / 32768.0).log10()
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn stream(dest: &str, secs: u64) -> Result<()> {
    use wayshare::audio::sender::{AudioParams, AudioSender};
    let mut s = AudioSender::start(AudioParams {
        peer: dest.parse()?,
        local_port: 0,
        ssrc: 0x1234_5678,
        remote_ssrc: 0x8765_4321,
        payload_type: PAYLOAD_TYPE,
        send_key: [7; MASTER_LEN],
        recv_key: [9; MASTER_LEN],
        encoder: Default::default(),
        device: None,
    })?;
    std::thread::sleep(Duration::from_secs(secs));
    println!("running after {secs}s: {}", s.running());
    s.stop();
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("stream") {
        #[cfg(target_os = "linux")]
        return stream(&args[1], args[2].parse()?);
        #[cfg(not(target_os = "linux"))]
        bail!("stream needs Linux");
    }
    if args.first().map(String::as_str) == Some("capture") {
        #[cfg(target_os = "linux")]
        return capture(args[1].parse()?);
        #[cfg(not(target_os = "linux"))]
        bail!("capture needs Linux");
    }
    if args.first().map(String::as_str) == Some("encode") {
        #[cfg(target_os = "linux")]
        return encode(
            &args[1],
            args[2].parse()?,
            args.get(3)
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(96_000),
            args.get(4)
                .map(|s| s.parse())
                .transpose()?
                .unwrap_or(1000.0),
            args.get(5).map(|s| s.parse()).transpose()?.unwrap_or(-70.0),
            args.get(6).map(String::as_str) != Some("nosbr"),
            args.get(7).map(|s| s.parse()).transpose()?.unwrap_or(0),
        );
        #[cfg(not(target_os = "linux"))]
        bail!("encode needs Linux (libfdk-aac)");
    }
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| args[i + 1].clone())
    };
    let json = std::fs::read_to_string(&args[0])?;
    let ssrc: u32 = json_field(&json, "remoteSSRC")?.parse()?;
    let remote_ssrc: u32 = json_field(&json, "localSSRC")?.parse()?;
    let pt: u8 = json_field(&json, "rxPayloadType")?.parse()?;
    if pt != PAYLOAD_TYPE {
        println!("note: receiver expects PT {pt}");
    }
    let srtp = args.iter().any(|a| a == "--srtp");
    let key = |k: &str| -> Result<[u8; MASTER_LEN]> {
        base64(&json_field(&json, k)?)
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad key {k}"))
    };
    let mut tx = SrtpContext::new(&key("kSV")?);
    let mut rx = SrtpContext::new(&key("kVS")?);
    let aus = read_aus(&args[1])?;
    let secs: f64 = flag("--seconds")
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(aus.len() as f64 / 100.0);
    let seq = flag("--seq-base")
        .map(|s| u16::from_str_radix(&s, 16))
        .transpose()?
        .unwrap_or(0x1234);
    let mut pk = AudioPacketizer::new(ssrc, pt, seq, rand::random());

    let sock = UdpSocket::bind("127.0.0.1:41014")?;
    sock.connect("127.0.0.1:41002")?;
    sock.set_nonblocking(true)?;
    let start = Instant::now();
    let (mut sent, mut octets) = (0u32, 0u32);
    let mut next_sr = start;
    let (mut rr, mut other, mut auth_fail) = (0u32, 0u32, 0u32);
    let mut last_rr = String::new();
    let total = (secs * 100.0) as usize;
    for i in 0..total {
        let due = start + Duration::from_millis(10 * i as u64);
        while Instant::now() < due {
            let mut buf = [0u8; 2048];
            if let Ok(n) = sock.recv(&mut buf) {
                let mut p = buf[..n].to_vec();
                if is_rtcp(&p) && (!srtp || rx.unprotect_rtcp(&mut p).is_ok()) {
                    for ev in rtcp::parse(&p).unwrap_or_default() {
                        match ev {
                            RtcpEvent::ReceiverReport { .. } => {
                                rr += 1;
                                last_rr = format!("{ev:?}");
                            }
                            _ => other += 1,
                        }
                    }
                } else if srtp {
                    auth_fail += 1;
                }
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let au = &aus[i % aus.len()];
        let mut p = pk.packet(au).context("AU too large")?;
        if srtp {
            tx.protect_rtp(&mut p)?;
        }
        sock.send(&p)?;
        sent += 1;
        octets += au.len() as u32;
        if Instant::now() >= next_sr {
            let ts = pk.timestamp();
            let mut sr = rtcp::sender_report(ssrc, remote_ssrc, rtcp::ntp_now(), ts, sent, octets);
            if srtp {
                tx.protect_rtcp(&mut sr)?;
            }
            sock.send(&sr)?;
            next_sr += Duration::from_secs(1);
        }
    }
    println!(
        "sent {sent} packets ({} s at {CLOCK_RATE} Hz), receiver RTCP: {rr} RR, {other} other, {auth_fail} undecryptable",
        sent / 100
    );
    println!("last RR: {last_rr}");
    Ok(())
}
