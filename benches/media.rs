use std::hint::black_box;
use std::time::{Duration, Instant};

use wayshare::hp::packetizer::{Packetizer, PacketizerConfig};
use wayshare::hp::srtp::SrtpContext;

fn measure(name: &str, mut run: impl FnMut()) {
    let mut samples = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        let mut runs = 0;
        while start.elapsed() < Duration::from_millis(200) {
            run();
            runs += 1;
        }
        samples.push(start.elapsed().as_secs_f64() * 1e6 / runs as f64);
    }
    samples.sort_by(f64::total_cmp);
    println!("{name}: {:.2} us (median of 7 samples)", samples[3]);
}

fn access_unit(size: usize, refresh: bool) -> Vec<u8> {
    let mut au = Vec::with_capacity(size + 6);
    au.extend_from_slice(&[0, 0, 0, 1, if refresh { 19 << 1 } else { 1 << 1 }, 1]);
    au.extend((0..size).map(|i| (i % 251 + 1) as u8));
    au
}

fn main() {
    // Fixed inputs keep packet counts and payload contents comparable between runs.
    let key = [0x37; 46];
    let mut tx = SrtpContext::new(&key);
    let mut packet = vec![0x5a; 1320];
    packet[..12].copy_from_slice(&[0x80, 100, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    packet.reserve(10);
    let mut seq = 0u16;
    measure("SRTP 1308-byte payload", || {
        packet.truncate(1320);
        packet[2..4].copy_from_slice(&seq.to_be_bytes());
        seq = seq.wrapping_add(1);
        tx.protect_rtp(black_box(&mut packet)).unwrap();
        black_box(&packet);
    });

    for (name, size, refresh) in [
        ("HEVC 64 KiB frame", 16 * 1024, false),
        ("HEVC 4 MiB refresh", 1024 * 1024, true),
    ] {
        let au = access_unit(size, refresh);
        let mut pk = Packetizer::new(PacketizerConfig::new(1000, 100, 4, (3840, 2160)));
        let tiles = [Some(au.as_slice()); 4];
        let mut timestamp = 0u32;
        measure(name, || {
            timestamp = timestamp.wrapping_add(400);
            black_box(pk.packetize(black_box(&tiles), timestamp));
        });
    }

    #[cfg(target_os = "linux")]
    if let Ok(output) = std::env::var("WAYSHARE_BENCH_OUTPUT") {
        gpu(&output);
    }
}

#[cfg(target_os = "linux")]
fn gpu(output: &str) {
    use wayshare::capture::{Capturer, TileDiffer};
    use wayshare::encode::{EncoderBackend, EncoderConfig};

    let mut cap = Capturer::new(Some(output)).unwrap();
    let frame = cap
        .next_frame(Some(Duration::from_secs(5)))
        .unwrap()
        .unwrap();
    let (w, h) = (frame.buffer.width, frame.buffer.height);
    println!("GPU input: {output}, {w}x{h}");
    let mut differ = TileDiffer::new(wayshare::encode::TileLayout::new(w, h)).unwrap();
    measure("GPU tile diff", || {
        black_box(differ.diff(black_box(&frame.buffer)).unwrap());
    });
    drop(differ);

    let mut enc = EncoderBackend::Vaapi
        .open(cap.render_node(), w, h, &EncoderConfig::default())
        .unwrap();
    measure("VAAPI unchanged frame (diff + conversion)", || {
        black_box(enc.encode(black_box(&frame)).unwrap());
    });
}
