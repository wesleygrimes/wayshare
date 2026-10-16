//! hp_send <rx.json> <interleaved.hevc> [--pattern all|skipT:E|staticT] [--srtp] [--don-base HEX] [--seq-base HEX] [--prime-ms N] [--size WxH] [--display WxH] [--loss PCT] [--blackhole N] [--drop-frame N] [--cap-mbps N] [--dump-rtcp FILE]
//!         [--switch-at N --file2 FILE --size2 WxH [--don2 HEX] [--ssrc2 N]]
//!
//! `--switch-at`: after N frames, continue with FILE (pattern all) at size WxH through a new
//! packetizer, like a mid-stream resize. DON and per-tile RTP sequence numbers continue unless
//! `--don2` resets the DON; `--ssrc2` moves the tiles to a new SSRC base.
//!
//! The input is ONE HEVC sequence of 1920x272 pictures in DON order (tile 0..3 of frame 0, then
//! frame 1, ...), as the receiver decodes all tiles with a single decoder. `--pattern` says which
//! (frame, tile) pictures the file contains (see `/tmp/hp/mkil.py`). Sends them through the hp
//! packetizer (optionally SRTP) to the `hprx` harness (127.0.0.1:41012 -> :41000) at 60 fps and
//! reports the receiver's RTCP feedback.

use std::collections::{BTreeMap, HashSet};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use wayshare::hp::packetizer::{Packetizer, PacketizerConfig, split_annexb};
use wayshare::hp::rtcp::{self, RtcpEvent};
use wayshare::hp::srtp::{MASTER_LEN, SrtpContext, is_rtcp};

fn json_field<'a>(json: &'a str, key: &str) -> Result<&'a str> {
    let at = json
        .find(&format!("\"{key}\""))
        .with_context(|| format!("rx.json has no {key}"))?;
    let rest = json[at + key.len() + 2..].trim_start_matches([':', ' ']);
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Ok(rest[..end].trim_matches('"'))
}

fn base64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let bits: Vec<u8> = s
        .bytes()
        .filter(|&c| c != b'=' && c != b'\\')
        .map(val)
        .collect();
    bits.chunks(4)
        .flat_map(|c| {
            let n = c
                .iter()
                .enumerate()
                .fold(0u32, |a, (i, &v)| a | u32::from(v) << (18 - 6 * i));
            n.to_be_bytes()[1..c.len()].to_vec()
        })
        .collect()
}

fn access_units(data: &[u8]) -> Vec<Vec<u8>> {
    let mut aus = Vec::new();
    let mut cur = Vec::new();
    for nal in split_annexb(data) {
        cur.extend_from_slice(&[0, 0, 0, 1]);
        cur.extend_from_slice(nal);
        if (nal[0] >> 1) & 0x3f < 32 {
            aus.push(std::mem::take(&mut cur));
        }
    }
    aus
}

#[derive(Default)]
struct Feedback {
    acks: HashSet<u32>,
    by_pt: BTreeMap<u8, u32>,
    firs: u32,
    rctl: u32,
    auth_fail: u32,
    samples: Vec<String>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .map(|i| args[i + 1].clone())
    };
    let srtp = args.iter().any(|a| a == "--srtp");
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            !a.starts_with("--")
                && (*i == 0
                    || !matches!(
                        args[i - 1].as_str(),
                        "--don-base"
                            | "--pattern"
                            | "--seq-base"
                            | "--prime-ms"
                            | "--size"
                            | "--display"
                            | "--switch-at"
                            | "--file2"
                            | "--size2"
                            | "--don2"
                            | "--ssrc2"
                    ))
        })
        .map(|(_, a)| a)
        .collect();
    let info = std::fs::read_to_string(positional[0])?;
    let base: u32 = json_field(&info, "remoteSSRC")?.parse()?;
    let viewer: u32 = json_field(&info, "localSSRC")?.parse()?;
    let pt: u8 = json_field(&info, "rxPayloadType")?.parse()?;
    let key = |k: &str| -> Result<[u8; MASTER_LEN]> {
        base64(json_field(&info, k)?)
            .try_into()
            .map_err(|_| anyhow::anyhow!("{k} is not 46 bytes"))
    };
    const TILES: usize = 4;
    let parse_size = |v: String| -> Result<(u16, u16)> {
        let (w, h) = v.split_once('x').context("WxH")?;
        Ok((w.parse()?, h.parse()?))
    };
    let size = flag("--size").map_or(Ok((1920, 1088)), parse_size)?;
    let switch_at: Option<usize> = flag("--switch-at").map(|v| v.parse()).transpose()?;
    let size2 = flag("--size2").map(parse_size).transpose()?;
    let pictures2: Vec<Vec<u8>> = match flag("--file2") {
        Some(p) => access_units(&std::fs::read(p)?),
        None => Vec::new(),
    };
    let pictures = access_units(&std::fs::read(positional[1])?);
    let pattern = flag("--pattern").unwrap_or_else(|| "all".into());
    let present = |f: usize, t: usize| -> bool {
        if let Some(rest) = pattern.strip_prefix("skip") {
            let (tt, e) = rest.split_once(':').unwrap();
            let (tt, e): (usize, usize) = (tt.parse().unwrap(), e.parse().unwrap());
            !(t == tt && f % e == e - 1)
        } else if let Some(tt) = pattern.strip_prefix("static") {
            f == 0 || t == tt.parse::<usize>().unwrap()
        } else {
            true
        }
    };
    let mut next_picture = 0usize;
    let mut schedule = Vec::new();
    while next_picture < pictures.len() {
        let f = schedule.len();
        let row: Vec<Option<usize>> = (0..TILES)
            .map(|t| {
                present(f, t).then(|| {
                    next_picture += 1;
                    next_picture - 1
                })
            })
            .collect();
        if row.iter().flatten().any(|&i| i >= pictures.len()) {
            break;
        }
        schedule.push(row);
    }
    if let Some(at) = switch_at {
        schedule.truncate(at);
        for f2 in 0..pictures2.len() / TILES {
            schedule.push((0..TILES).map(|t| Some(f2 * TILES + t)).collect());
        }
    }
    let frames = schedule.len();
    let picture = |f: usize, i: usize| -> &[u8] {
        match switch_at {
            Some(at) if f >= at => pictures2[i].as_slice(),
            _ => pictures[i].as_slice(),
        }
    };

    let mut cfg = PacketizerConfig::new(base, pt, TILES as u8, size);
    if let Some(d) = flag("--don-base") {
        cfg.don_base = u16::from_str_radix(d.trim_start_matches("0x"), 16)?;
    }
    if let Some(v) = flag("--display") {
        let (w, h) = v.split_once('x').context("--display WxH")?;
        cfg.display_size = (w.parse()?, h.parse()?);
    }
    if let Some(d) = flag("--seq-base") {
        cfg.initial_seq = u16::from_str_radix(d.trim_start_matches("0x"), 16)?;
    }
    let don_base = cfg.don_base;
    let mut pk = Packetizer::new(cfg.clone());
    // Per-tile RTP sequence numbers, kept across the switch to a new packetizer.
    let mut next_seq: Vec<Option<u16>> = vec![None; TILES];
    let mut ssrc_base = base;
    let mut tx = srtp
        .then(|| key("kSV"))
        .transpose()?
        .map(|k| SrtpContext::new(&k));
    let mut rx = srtp
        .then(|| key("kVS"))
        .transpose()?
        .map(|k| SrtpContext::new(&k));

    let sock = UdpSocket::bind("127.0.0.1:41012")?;
    sock.connect("127.0.0.1:41000")?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    let fb = Arc::new(Mutex::new(Feedback::default()));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let start = Instant::now();
    let mut rtcp_log = flag("--dump-rtcp").map(std::fs::File::create).transpose()?;
    let rx_thread = {
        let (sock, fb, done) = (sock.try_clone()?, fb.clone(), done.clone());
        std::thread::spawn(move || {
            use std::io::Write;
            let mut buf = [0u8; 2048];
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let Ok(n) = sock.recv(&mut buf) else { continue };
                let mut pkt = buf[..n].to_vec();
                let mut fb = fb.lock().unwrap();
                if !is_rtcp(&pkt) {
                    continue;
                }
                if let Some(rx) = rx.as_mut()
                    && rx.unprotect_rtcp(&mut pkt).is_err()
                {
                    fb.auth_fail += 1;
                    continue;
                }
                if let Some(f) = rtcp_log.as_mut() {
                    let ms = start.elapsed().as_millis() as u32;
                    let _ = f.write_all(
                        &[
                            &ms.to_be_bytes()[..],
                            &(pkt.len() as u16).to_be_bytes(),
                            &pkt,
                        ]
                        .concat(),
                    );
                }
                for ev in rtcp::parse(&pkt).unwrap_or_default() {
                    let pt = match &ev {
                        RtcpEvent::DonAck { don, .. } => {
                            fb.acks.insert(*don);
                            204
                        }
                        RtcpEvent::KeyframeRequest { .. } => {
                            fb.firs += 1;
                            192
                        }
                        RtcpEvent::RateControl { .. } => {
                            fb.rctl += 1;
                            204
                        }
                        RtcpEvent::ReceiverReport { .. } => 201,
                        RtcpEvent::Sdes => 202,
                        _ => 0,
                    };
                    *fb.by_pt.entry(pt).or_default() += 1;
                    if fb.samples.len() < 6 && !matches!(ev, RtcpEvent::Sdes) {
                        fb.samples.push(format!("{ev:?}"));
                    }
                }
            }
        })
    };

    let prime: u64 = flag("--prime-ms").map_or(Ok(0), |v| v.parse())?;
    // Network emulation: random RTP loss (percent) and a send-rate cap (Mbit/s).
    let loss: f64 = flag("--loss").map_or(Ok(0.0), |v| v.parse())?;
    let cap_bps: f64 = flag("--cap-mbps").map_or(Ok(0.0), |v| v.parse::<f64>().map(|m| m * 1e6))?;
    let mut bucket_t = Instant::now();
    // Packetize but don't send the first N frames, like a viewer that isn't listening yet.
    let blackhole: usize = flag("--blackhole").map_or(Ok(0), |v| v.parse())?;
    // Drop the first packet of frame N to provoke keyframe requests.
    let drop_frame: Option<usize> = flag("--drop-frame").map(|v| v.parse()).transpose()?;
    let mut t0 = Instant::now();
    let (mut sent, mut octets, mut sent_dons) = (0u32, 0u32, Vec::new());
    let mut switch_pic = usize::MAX;
    for (f, row) in schedule.iter().enumerate() {
        if switch_at == Some(f) {
            let mut cfg2 =
                PacketizerConfig::new(ssrc_base, pt, TILES as u8, size2.context("--size2")?);
            cfg2.don_base = pk.next_don();
            cfg2.frame_number_base = cfg.frame_number_base.wrapping_add(f as u16);
            if let Some(d) = flag("--don2") {
                cfg2.don_base = u16::from_str_radix(d.trim_start_matches("0x"), 16)?;
            }
            if let Some(s) = flag("--ssrc2") {
                cfg2.ssrc_base = s.parse()?;
                ssrc_base = cfg2.ssrc_base;
            }
            println!(
                "switch at frame {f}: {:?} tile_height {} DON {:#x} ssrc {}",
                cfg2.frame_size, cfg2.tile_height, cfg2.don_base, cfg2.ssrc_base
            );
            pk = Packetizer::new(cfg2);
            switch_pic = sent_dons.len();
        }
        let tiles: Vec<Option<&[u8]>> = row.iter().map(|p| p.map(|i| picture(f, i))).collect();
        let mut d = pk.next_don();
        for t in &tiles {
            if t.is_some() {
                sent_dons.push(u32::from(d));
                d = d.wrapping_add(1);
            }
        }
        let ts = f as u32 * 400;
        for (k, mut p) in pk.packetize(&tiles, ts).into_iter().enumerate() {
            if drop_frame == Some(f) && k == 0 {
                continue;
            }
            let tile = u32::from_be_bytes(p[8..12].try_into()?).wrapping_sub(ssrc_base) as usize;
            let seq = next_seq[tile].unwrap_or(u16::from_be_bytes([p[2], p[3]]));
            p[2..4].copy_from_slice(&seq.to_be_bytes());
            next_seq[tile] = Some(seq.wrapping_add(1));
            octets += (p.len() - 20) as u32;
            if let Some(tx) = tx.as_mut() {
                tx.protect_rtp(&mut p)?;
            }
            if cap_bps > 0.0 {
                bucket_t += Duration::from_secs_f64(p.len() as f64 * 8.0 / cap_bps);
                std::thread::sleep(bucket_t.saturating_duration_since(Instant::now()));
                bucket_t = bucket_t.max(Instant::now() - Duration::from_millis(5));
            }
            if f < blackhole || (loss > 0.0 && rand::random::<f64>() * 100.0 < loss) {
                continue;
            }
            sock.send(&p)?;
            sent += 1;
        }
        if f % 60 == 0 {
            let mut sr = rtcp::sender_report(base, viewer, rtcp::ntp_now(), ts, sent, octets);
            if let Some(tx) = tx.as_mut() {
                tx.protect_rtcp(&mut sr)?;
            }
            sock.send(&sr)?;
        }
        if f == 0 && prime > 0 {
            // Lets a debugger-slowed receiver create its decoder before the stream continues.
            std::thread::sleep(Duration::from_millis(prime));
            t0 = Instant::now() - Duration::from_secs_f64(1.0 / 60.0);
        }
        let next = t0 + Duration::from_secs_f64((f + 1) as f64 / 60.0);
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    std::thread::sleep(Duration::from_secs(2));
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    rx_thread.join().unwrap();

    let fb = fb.lock().unwrap();
    let acked_sent = sent_dons.iter().filter(|d| fb.acks.contains(d)).count();
    println!(
        "sent {sent} packets, {frames} frames x {} tiles ({} pictures, pattern {pattern}), srtp={srtp}, DON base {don_base:#x}",
        TILES,
        sent_dons.len()
    );
    println!(
        "feedback by pt {:?}; unique DON acks {} (matching a sent DON: {acked_sent}); FIR {}; RCTL {}; SRTCP auth failures {}",
        fb.by_pt,
        fb.acks.len(),
        fb.firs,
        fb.rctl,
        fb.auth_fail
    );
    for s in &fb.samples {
        println!("  {s}");
    }
    let mut acks: Vec<_> = fb.acks.iter().copied().collect();
    acks.sort();
    println!("ack range {:?}..{:?}", acks.first(), acks.last());
    let missing: Vec<_> = sent_dons
        .iter()
        .enumerate()
        .filter(|(_, d)| !fb.acks.contains(d))
        .take(10)
        .collect();
    println!("unacked (index, DON): {missing:?}");
    if switch_pic < sent_dons.len() {
        let after = &sent_dons[switch_pic..];
        let acked_after = after.iter().filter(|d| fb.acks.contains(d)).count();
        println!(
            "after switch: {acked_after}/{} pictures acked (before: {}/{switch_pic})",
            after.len(),
            acked_sent - acked_after
        );
    }
    Ok(())
}
