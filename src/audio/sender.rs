//! HP audio stream: monitor capture -> AAC-ELD -> RTP -> SRTP -> UDP, plus RTCP SR/RR.

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tracing::{debug, info, warn};

use super::capture::MonitorCapture;
use super::fdk::{AacEldEncoder, EncoderConfig};
use super::rtp::{AudioPacketizer, CHANNELS, SAMPLES_PER_FRAME};
use crate::hp::rtcp::{self, RtcpEvent};
use crate::hp::srtp::{MASTER_LEN, SrtpContext, is_rtcp};

pub struct AudioParams {
    /// Viewer address and its audio port (the port we advertised in the 1010 type-1 message).
    pub peer: SocketAddr,
    pub local_port: u16,
    /// `Answer.ssrc` / `.remote_ssrc` / `.payload_type` from `hp::negotiation::answer` (mode 8).
    pub ssrc: u32,
    pub remote_ssrc: u32,
    pub payload_type: u8,
    /// Audio stream keys from msg 28, server→viewer and viewer→server.
    pub send_key: [u8; MASTER_LEN],
    pub recv_key: [u8; MASTER_LEN],
    pub encoder: EncoderConfig,
    /// PulseAudio source; `None` = default sink monitor.
    pub device: Option<String>,
}

pub struct AudioSender {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioSender {
    pub fn start(p: AudioParams) -> Result<Self> {
        let sock = UdpSocket::bind(SocketAddr::new(
            std::net::Ipv4Addr::UNSPECIFIED.into(),
            p.local_port,
        ))
        .with_context(|| format!("binding audio UDP {}", p.local_port))?;
        sock.connect(p.peer)?;
        sock.set_nonblocking(true)?;
        let cap = MonitorCapture::open(p.device.as_deref())?;
        let enc = AacEldEncoder::new(&p.encoder)?;
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let thread = std::thread::Builder::new()
            .name("hp-audio".into())
            .spawn(move || {
                if let Err(e) = run(p, sock, cap, enc, &s) {
                    warn!("HP audio stopped: {e:#}");
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }

    pub fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for AudioSender {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    p: AudioParams,
    sock: UdpSocket,
    mut cap: MonitorCapture,
    mut enc: AacEldEncoder,
    stop: &AtomicBool,
) -> Result<()> {
    let mut tx = SrtpContext::new(&p.send_key);
    let mut rx = SrtpContext::new(&p.recv_key);
    let mut pk = AudioPacketizer::new(p.ssrc, p.payload_type, rand::random(), rand::random());
    let mut pcm = [0i16; (SAMPLES_PER_FRAME * CHANNELS) as usize];
    let (mut packets, mut octets, mut rrs, mut send_errors) = (0u32, 0u32, 0u64, 0u64);
    let mut next_sr = Instant::now();
    info!(peer = %p.peer, ssrc = format_args!("{:#x}", p.ssrc), "HP audio started");
    while !stop.load(Ordering::Relaxed) {
        cap.read_frame(&mut pcm)?;
        let Some(au) = enc.encode(&pcm)? else {
            continue;
        };
        let Some(mut pkt) = pk.packet(&au) else {
            warn!(len = au.len(), "dropping oversized AU");
            continue;
        };
        tx.protect_rtp(&mut pkt)?;
        match sock.send(&pkt) {
            Ok(_) => {
                packets = packets.wrapping_add(1);
                octets = octets.wrapping_add(au.len() as u32);
            }
            Err(_) => send_errors += 1,
        }
        let now = Instant::now();
        if now >= next_sr {
            let mut sr = rtcp::sender_report(
                p.ssrc,
                p.remote_ssrc,
                rtcp::ntp_now(),
                pk.timestamp(),
                packets,
                octets,
            );
            tx.protect_rtcp(&mut sr)?;
            let _ = sock.send(&sr);
            next_sr = now + Duration::from_secs(1);
            debug!(packets, rrs, send_errors, "HP audio stats");
        }
        let mut buf = [0u8; 1500];
        while let Ok(n) = sock.recv(&mut buf) {
            let mut pkt = buf[..n].to_vec();
            if is_rtcp(&pkt) && rx.unprotect_rtcp(&mut pkt).is_ok() {
                rrs += rtcp::parse(&pkt)
                    .unwrap_or_default()
                    .iter()
                    .filter(|e| matches!(e, RtcpEvent::ReceiverReport { .. }))
                    .count() as u64;
            }
        }
    }
    info!(packets, rrs, send_errors, "HP audio stopped");
    Ok(())
}
