//! Capture + VAAPI HEVC benchmark: `capture_bench [output] [seconds] [dump_seconds] [out_dir]`.
//! Env: ARDD_BACKEND (vaapi|x265), ARDD_CHROMA (420|444), ARDD_X265_PRESET, ARDD_X265_THREADS,
//! ARDD_SAO (0|1), ARDD_DEPTH (frames in flight, 1-2), ARDD_MODE=source (Standard-mode CPU path), ARDD_SKIP_UNCHANGED (0|1), ARDD_SET_BITRATE_AT (frame:bps), ARDD_CAPTURE (ext|wlr), ARDD_MEMORY (gpu|system), ARDD_READBACK, ARDD_BITRATE (total bps, 0 = CQP), ARDD_QP, ARDD_IDR_EVERY (frames),
//! ARDD_SKIP_ENCODE (capture only).

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("capture_bench is Linux-only");
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    bench::run()
}

#[cfg(target_os = "linux")]
mod bench {
    use std::fs::File;
    use std::io::Write;
    use std::time::{Duration, Instant};

    use anyhow::Result;
    use wayshare::capture::{CaptureOptions, Capturer, Memory, Protocol, monotonic_now};
    use wayshare::encode::{Chroma, EncoderBackend, EncoderConfig, TILE_COUNT, X265Options};

    fn env<T: std::str::FromStr>(k: &str) -> Option<T> {
        std::env::var(k).ok()?.parse().ok()
    }

    fn cpu_time() -> Duration {
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
        let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
        tv(ru.ru_utime) + tv(ru.ru_stime)
    }

    fn pct(v: &mut [f64], p: f64) -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        v.sort_by(|a, b| a.total_cmp(b));
        v[((v.len() - 1) as f64 * p).round() as usize]
    }

    fn source_bench(output: Option<&str>, seconds: f64) -> Result<()> {
        use wayshare::source::FrameSource;
        let mut src = wayshare::capture::CaptureSource::new(output)?;
        println!("CaptureSource {:?}", src.size());
        let (mut changed, mut polls, mut rects) = (0u64, 0u64, 0usize);
        let mut adv_ms = Vec::new();
        let start = Instant::now();
        let cpu0 = cpu_time();
        while start.elapsed().as_secs_f64() < seconds {
            let t = Instant::now();
            polls += 1;
            if src.advance() {
                adv_ms.push(t.elapsed().as_secs_f64() * 1e3);
                changed += 1;
                rects += src.dirty_rects().len();
                if changed <= 3 {
                    println!(
                        "dirty {:?}",
                        &src.dirty_rects()[..src.dirty_rects().len().min(4)]
                    );
                }
            }
        }
        let wall = start.elapsed().as_secs_f64();
        println!(
            "changed frames {changed} ({:.1}/s) of {polls} polls, {:.1} rects/frame, cpu {:.1}% of one core",
            changed as f64 / wall,
            rects as f64 / changed.max(1) as f64,
            (cpu_time() - cpu0).as_secs_f64() / wall * 100.0
        );
        println!(
            "advance (changed) ms: p50 {:.2} p95 {:.2} max {:.2}",
            pct(&mut adv_ms, 0.5),
            pct(&mut adv_ms, 0.95),
            pct(&mut adv_ms, 1.0)
        );
        Ok(())
    }

    #[derive(Default)]
    struct Acc {
        frames: u64,
        bytes: u64,
        idrs: u64,
        max_frame: usize,
        tile_counts: [u64; TILE_COUNT],
        per_second: Vec<u64>,
        lat_ms: Vec<f64>,
        diff_ms: Vec<f64>,
        conv_ms: Vec<f64>,
        vcn_ms: Vec<f64>,
    }

    fn consume(
        rx: std::sync::mpsc::Receiver<Result<wayshare::encode::EncodedFrame>>,
        start: Instant,
        dump_seconds: f64,
        mut dump: File,
    ) -> Result<Acc> {
        let mut a = Acc::default();
        for out in rx {
            let out = out?;
            if let Some(p) = out.presented {
                a.lat_ms
                    .push(monotonic_now().saturating_sub(p).as_secs_f64() * 1e3);
            }
            a.diff_ms.push(out.timings.diff.as_secs_f64() * 1e3);
            a.conv_ms.push(out.timings.convert.as_secs_f64() * 1e3);
            a.vcn_ms.push(out.timings.encode.as_secs_f64() * 1e3);
            a.frames += 1;
            let dumping = start.elapsed().as_secs_f64() < dump_seconds;
            let mut fbytes = 0;
            for t in &out.tiles {
                a.tile_counts[t.index as usize] += 1;
                a.idrs += t.keyframe as u64;
                fbytes += t.data.len();
                if dumping {
                    dump.write_all(&t.data)?;
                }
            }
            a.max_frame = a.max_frame.max(fbytes);
            a.bytes += fbytes as u64;
            let sec = start.elapsed().as_secs() as usize;
            if a.per_second.len() <= sec {
                a.per_second.resize(sec + 1, 0);
            }
            a.per_second[sec] += fbytes as u64;
        }
        Ok(a)
    }

    pub fn run() -> Result<()> {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
        let args: Vec<String> = std::env::args().collect();
        let output = args.get(1).map(String::as_str).filter(|s| !s.is_empty());
        let seconds: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(30.0);
        let dump_seconds: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5.0);
        let out_dir = args.get(4).cloned().unwrap_or_else(|| "out".into());

        let bitrate: u64 = env("ARDD_BITRATE").unwrap_or(40_000_000);
        let cfg = EncoderConfig {
            fps: 60,
            bitrate_bps: (bitrate > 0).then_some(bitrate),
            qp: env("ARDD_QP").unwrap_or(24),
            skip_unchanged: env::<u8>("ARDD_SKIP_UNCHANGED").unwrap_or(1) != 0,
            sao: env::<u8>("ARDD_SAO").unwrap_or(0) != 0,
            max_in_flight: env("ARDD_DEPTH").unwrap_or(1),
            backend: match std::env::var("ARDD_BACKEND").as_deref() {
                Ok("vaapi") => Some(EncoderBackend::Vaapi),
                Ok("x265") => Some(EncoderBackend::X265),
                _ => None,
            },
            chroma: if env::<u32>("ARDD_CHROMA") == Some(444) {
                Chroma::Yuv444
            } else {
                Chroma::Yuv420
            },
            x265: X265Options {
                preset: std::env::var("ARDD_X265_PRESET").unwrap_or_else(|_| "ultrafast".into()),
                threads_per_tile: env("ARDD_X265_THREADS"),
            },
            ..Default::default()
        };
        // "frame:bps": switch the bitrate at that frame; bitrate is reported before/after.
        let rate_switch: Option<(u64, u64)> =
            std::env::var("ARDD_SET_BITRATE_AT").ok().and_then(|v| {
                let (f, b) = v.split_once(':')?;
                Some((f.parse().ok()?, b.parse().ok()?))
            });
        let skip_encode = std::env::var_os("ARDD_SKIP_ENCODE").is_some();
        let idr_every: u64 = env("ARDD_IDR_EVERY").unwrap_or(0);

        if std::env::var("ARDD_MODE").as_deref() == Ok("source") {
            return source_bench(output, seconds);
        }
        let protocol = match std::env::var("ARDD_CAPTURE").as_deref() {
            Ok("wlr") => Some(Protocol::WlrScreencopy),
            Ok("ext") => Some(Protocol::ExtImageCopyCapture),
            _ => None,
        };
        let backend = EncoderBackend::auto(std::path::Path::new("/dev/dri/renderD128"), &cfg);
        let memory = match std::env::var("ARDD_MEMORY").as_deref() {
            Ok("system") => Memory::System,
            Ok("gpu") => Memory::Gpu,
            _ => backend.capture_memory(),
        };
        println!("backend {backend:?}, capture memory {memory:?}");
        let mut cap = Capturer::with_options(output, CaptureOptions { protocol, memory })?;
        println!(
            "protocol {:?}, render node {}",
            cap.protocol(),
            cap.render_node().display()
        );

        let first = loop {
            if let Some(f) = cap.next_frame(Some(Duration::from_secs(2)))? {
                break f;
            }
        };
        let b = &first.buffer;
        println!(
            "buffer {}x{} fourcc {:?} modifier {:#x} planes {}",
            b.width,
            b.height,
            String::from_utf8_lossy(&b.fourcc.to_le_bytes()),
            b.modifier,
            b.planes.len()
        );
        let mut enc = backend.open(cap.render_node(), b.width, b.height, &cfg)?;
        println!("layout {:?}, cfg {cfg:?}", enc.layout());

        std::fs::create_dir_all(&out_dir)?;
        // One HEVC sequence: tile pictures in coding (DON) order.
        let dump = File::create(format!("{out_dir}/seq.h265"))?;

        let readback = std::env::var_os("ARDD_READBACK").is_some();
        let mut prev = Vec::new();
        let mut copy_ms = Vec::new();
        let mut cmp_ms = Vec::new();
        let mut wait_ms = Vec::new();
        let mut ready_ms = Vec::new();
        let mut enc_ms = Vec::new();
        let mut captured = 0u64;
        let mut t_switch = None;

        let start = Instant::now();
        let cpu0 = cpu_time();
        // Coded frames arrive here from the encoder's collector thread, as a sender would get them.
        let (tx, rx) = std::sync::mpsc::channel();
        enc.set_output(tx);
        let consumer = std::thread::spawn(move || consume(rx, start, dump_seconds, dump));
        let mut held = Some(first);
        let mut dropped = 0u64;
        while start.elapsed().as_secs_f64() < seconds {
            // Latest-frame policy (as in the server): only the newest capture waits for the encoder.
            let wait = if held.is_some() { 2 } else { 100 };
            if let Some(f) = cap.next_frame(Some(Duration::from_millis(wait)))? {
                wait_ms.push((f.ready_at - f.requested_at).as_secs_f64() * 1e3);
                if let Some(p) = f.presented {
                    let ready_mono = monotonic_now().saturating_sub(f.ready_at.elapsed());
                    ready_ms.push(ready_mono.as_secs_f64() * 1e3 - p.as_secs_f64() * 1e3);
                }
                captured += 1;
                if held.replace(f).is_some() {
                    dropped += 1;
                }
            }
            if skip_encode {
                held = None;
                continue;
            }
            if held.is_none() || !enc.can_submit() {
                continue;
            }
            let frame = held.take().unwrap();
            if let Some((at, bps)) = rate_switch
                && captured >= at
                && t_switch.is_none()
            {
                enc.set_bitrate(bps);
                t_switch = Some(start.elapsed().as_secs_f64());
            }
            // ARDD_RATE_WOBBLE=N: alternate the target +-20% every N frames (RCTL-like churn).
            if let Some(n) = env::<u64>("ARDD_RATE_WOBBLE")
                && captured.is_multiple_of(n)
            {
                let base = cfg.bitrate_bps.unwrap_or(33_000_000);
                enc.set_bitrate(if (captured / n).is_multiple_of(2) {
                    base * 6 / 5
                } else {
                    base * 4 / 5
                });
            }
            if idr_every > 0 && captured > 0 && captured.is_multiple_of(idr_every) {
                enc.request_keyframe(None);
            }
            if readback {
                let t = Instant::now();
                let g = frame.buffer.read()?;
                let row_bytes = frame.buffer.width as usize * 4;
                let mut cur = vec![0u8; row_bytes * frame.buffer.height as usize];
                for y in 0..frame.buffer.height as usize {
                    cur[y * row_bytes..][..row_bytes].copy_from_slice(g.row(y, row_bytes));
                }
                drop(g);
                copy_ms.push(t.elapsed().as_secs_f64() * 1e3);
                let t = Instant::now();
                let n = (cur == prev) as usize;
                std::hint::black_box(n);
                cmp_ms.push(t.elapsed().as_secs_f64() * 1e3);
                prev = cur;
            }
            let l = enc.layout();
            let b = &frame.buffer;
            if (b.width, b.height) != (l.width, l.height) {
                anyhow::bail!(
                    "output resized to {}x{} during the bench; rerun",
                    b.width,
                    b.height
                );
            }
            let t0 = Instant::now();
            enc.submit(&frame)?;
            enc_ms.push(frame.ready_at.elapsed().as_secs_f64() * 1e3);
            let _ = t0;
        }
        let wall = start.elapsed().as_secs_f64();
        drop(enc);
        let mut acc = consumer.join().unwrap()?;
        let cpu = (cpu_time() - cpu0).as_secs_f64();
        let frames = acc.frames;

        println!(
            "captured {captured}, dropped {dropped}, coded {frames} in {wall:.1}s = {:.1} fps",
            frames as f64 / wall
        );
        println!(
            "tiles encoded per index {:?}, IDR tile-frames {}",
            acc.tile_counts, acc.idrs
        );
        println!(
            "bitrate {:.2} Mbps avg, largest frame {} KiB",
            acc.bytes as f64 * 8.0 / wall / 1e6,
            acc.max_frame / 1024
        );
        println!("cpu {:.1}% of one core", cpu / wall * 100.0);
        let mbps: Vec<String> = acc
            .per_second
            .iter()
            .map(|b| format!("{:.1}", *b as f64 * 8.0 / 1e6))
            .collect();
        println!("Mbps per second: {}", mbps.join(" "));
        if let Some(ts) = t_switch {
            println!("bitrate switched at {ts:.1}s");
        }
        let (lat_ms, diff_ms, conv_ms, vcn_ms) = (
            &mut acc.lat_ms,
            &mut acc.diff_ms,
            &mut acc.conv_ms,
            &mut acc.vcn_ms,
        );
        for (name, v) in [
            ("capture wait", &mut wait_ms),
            ("ready->submitted", &mut enc_ms),
            ("present->encoded", lat_ms),
            ("  present->ready", &mut ready_ms),
            ("  diff", diff_ms),
            ("  convert", conv_ms),
            ("  submit->coded", vcn_ms),
            ("cpu copy 30MB", &mut copy_ms),
            ("cpu compare", &mut cmp_ms),
        ] {
            println!(
                "{name:>17} ms: p50 {:.2} p95 {:.2} p99 {:.2} max {:.2} (n={})",
                pct(v, 0.5),
                pct(v, 0.95),
                pct(v, 0.99),
                pct(v, 1.0),
                v.len()
            );
        }
        Ok(())
    }
}
