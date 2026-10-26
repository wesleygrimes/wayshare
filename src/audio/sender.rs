//! HP audio stream: monitor capture -> AAC-ELD -> RTP -> SRTP -> UDP, plus RTCP SR/RR.

use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Mute the captured sink while streaming, so the sound plays only on the viewer.
    pub mute_local: bool,
}

pub struct AudioSender {
    control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Control {
    stop: AtomicBool,
    activated: AtomicBool,
    send_lock: Mutex<()>,
}

impl Control {
    fn stop(&self) {
        // Linearize shutdown with both RTP and RTCP sends. An in-flight send
        // may finish first, but no new send starts after this returns.
        let _guard = self.send_lock.lock().unwrap();
        self.stop.store(true, Ordering::Relaxed);
    }

    fn send(&self, sock: &UdpSocket, packet: &[u8]) -> Option<std::io::Result<usize>> {
        let _guard = self.send_lock.lock().unwrap();
        (!self.stop.load(Ordering::Relaxed)).then(|| sock.send(packet))
    }
}

impl AudioSender {
    pub fn start(p: AudioParams) -> Result<Self> {
        let sock = UdpSocket::bind(SocketAddr::new(
            if p.peer.is_ipv4() {
                std::net::Ipv4Addr::UNSPECIFIED.into()
            } else {
                std::net::Ipv6Addr::UNSPECIFIED.into()
            },
            p.local_port,
        ))
        .with_context(|| format!("binding audio UDP {}", p.local_port))?;
        sock.connect(p.peer)?;
        sock.set_nonblocking(true)?;
        let control = Arc::new(Control::default());
        let s = control.clone();
        let thread = std::thread::Builder::new()
            .name("hp-audio".into())
            .spawn(move || {
                if let Err(e) = run(p, sock, &s) {
                    warn!("HP audio stopped, continuing with video only: {e:#}");
                }
            })?;
        Ok(Self {
            control,
            thread: Some(thread),
        })
    }

    pub fn running(&self) -> bool {
        self.thread.as_ref().is_some_and(|t| !t.is_finished())
    }

    pub fn stop(&mut self) {
        self.control.stop();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    pub fn activate(&self) {
        self.control.activated.store(true, Ordering::Release);
    }
}

impl Drop for AudioSender {
    fn drop(&mut self) {
        self.stop();
    }
}

fn wait_for_receiver(
    sock: &UdpSocket,
    rx: &mut SrtpContext,
    stop: &AtomicBool,
    timeout: Duration,
) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 1500];
    while !stop.load(Ordering::Relaxed) {
        if Instant::now() >= deadline {
            // RTCP is optional before the first RTP packet. Keep the native
            // client's fast readiness path without deadlocking passive peers.
            warn!("no initial audio RTCP; starting audio after readiness grace period");
            return Ok(true);
        }
        match sock.recv(&mut buf) {
            Ok(n) => {
                let mut pkt = buf[..n].to_vec();
                if is_rtcp(&pkt) && rx.unprotect_rtcp(&mut pkt).is_ok() {
                    let events = rtcp::parse(&pkt).unwrap_or_default();
                    if events.iter().any(|e| matches!(e, RtcpEvent::Bye { .. })) {
                        return Ok(false);
                    }
                    if events.iter().any(|e| {
                        matches!(
                            e,
                            RtcpEvent::ReceiverReport { .. } | RtcpEvent::SenderReport { .. }
                        )
                    }) {
                        return Ok(true);
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e).context("waiting for audio receiver"),
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(false)
}

fn run(p: AudioParams, sock: UdpSocket, control: &Control) -> Result<()> {
    let stop = &control.stop;
    while !control.activated.load(Ordering::Acquire) {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut tx = SrtpContext::new(&p.send_key);
    let mut rx = SrtpContext::new(&p.recv_key);
    // The client sends SRTCP once its audio endpoint is ready. Sending during
    // negotiation can race socket setup. This wait starts only after the answer
    // is written. Do not buffer capture while waiting.
    info!(peer = %p.peer, "waiting for HP audio receiver");
    if !wait_for_receiver(&sock, &mut rx, stop, Duration::from_secs(5))? {
        return Ok(());
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    info!("HP audio readiness wait complete");
    let Some(mut cap) = MonitorCapture::open(p.device.as_deref(), stop)? else {
        return Ok(());
    };
    if p.mute_local {
        match cap.mute_sink() {
            Ok(()) => info!("muted local speakers while streaming"),
            Err(e) => warn!("could not mute local speakers: {e:#}"),
        }
    }
    let mut enc = AacEldEncoder::new(&p.encoder)?;
    let mut pk = AudioPacketizer::new(p.ssrc, p.payload_type, rand::random(), rand::random());
    let mut pcm = [0i16; (SAMPLES_PER_FRAME * CHANNELS) as usize];
    let (mut packets, mut octets, mut rrs, mut send_errors) = (0u32, 0u32, 0u64, 0u64);
    let mut rtcp_send_errors = 0u64;
    let mut next_sr = Instant::now();
    info!(peer = %p.peer, ssrc = format_args!("{:#x}", p.ssrc), "HP audio started");
    while !stop.load(Ordering::Relaxed) {
        if !cap.read_frame(&mut pcm, stop)? {
            break;
        }
        // A stop request can arrive while capture blocks for the next frame.
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Some(au) = enc.encode(&pcm)? else {
            continue;
        };
        let Some(mut pkt) = pk.packet(&au) else {
            warn!(len = au.len(), "dropping oversized AU");
            continue;
        };
        tx.protect_rtp(&mut pkt)?;
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match control.send(&sock, &pkt) {
            Some(Ok(_)) => {
                packets = packets.wrapping_add(1);
                octets = octets.wrapping_add(au.len() as u32);
            }
            Some(Err(_)) => send_errors += 1,
            None => break,
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
            match control.send(&sock, &sr) {
                Some(Err(_)) => rtcp_send_errors += 1,
                None => break,
                Some(Ok(_)) => {}
            }
            next_sr = now + Duration::from_secs(1);
            debug!(
                packets,
                rrs, send_errors, rtcp_send_errors, "HP audio stats"
            );
        }
        let mut buf = [0u8; 1500];
        // A continuously readable socket must not starve capture or shutdown.
        for _ in 0..32 {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(n) = sock.recv(&mut buf) else { break };
            let mut pkt = buf[..n].to_vec();
            if is_rtcp(&pkt) && rx.unprotect_rtcp(&mut pkt).is_ok() {
                let events = rtcp::parse(&pkt).unwrap_or_default();
                if events.iter().any(|e| matches!(e, RtcpEvent::Bye { .. })) {
                    control.stop();
                    break;
                }
                rrs += events
                    .iter()
                    .filter(|e| matches!(e, RtcpEvent::ReceiverReport { .. }))
                    .count() as u64;
            }
        }
    }
    info!(
        packets,
        rrs, send_errors, rtcp_send_errors, "HP audio stopped"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sockets() -> (UdpSocket, UdpSocket) {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let viewer = UdpSocket::bind("127.0.0.1:0").unwrap();
        server.connect(viewer.local_addr().unwrap()).unwrap();
        viewer.connect(server.local_addr().unwrap()).unwrap();
        server.set_nonblocking(true).unwrap();
        (server, viewer)
    }

    fn report(key: &[u8; MASTER_LEN]) -> Vec<u8> {
        let mut pkt = rtcp::sender_report(1, 2, 0, 0, 0, 0);
        SrtpContext::new(key).protect_rtcp(&mut pkt).unwrap();
        pkt
    }

    #[test]
    fn receiver_requires_current_session_authentication() {
        let (server, viewer) = sockets();
        let key = [7; MASTER_LEN];
        viewer.send(&report(&[8; MASTER_LEN])).unwrap();
        viewer.send(b"not SRTCP").unwrap();
        let mut rx = SrtpContext::new(&key);
        let stop = AtomicBool::new(false);
        let before = Instant::now();
        assert!(wait_for_receiver(&server, &mut rx, &stop, Duration::from_millis(30)).unwrap());
        assert!(before.elapsed() >= Duration::from_millis(30));
        viewer.send(&report(&key)).unwrap();
        assert!(wait_for_receiver(&server, &mut rx, &stop, Duration::from_secs(1)).unwrap());
    }

    #[test]
    fn passive_receiver_can_start_after_grace_without_readiness_probes() {
        let (server, viewer) = sockets();
        let mut rx = SrtpContext::new(&[7; MASTER_LEN]);
        assert!(
            wait_for_receiver(
                &server,
                &mut rx,
                &AtomicBool::new(false),
                Duration::from_millis(20),
            )
            .unwrap()
        );
        viewer.set_nonblocking(true).unwrap();
        assert_eq!(
            viewer.recv(&mut [0; 1500]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn receiver_wait_can_be_cancelled() {
        let (server, _viewer) = sockets();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let waiter = std::thread::spawn(move || {
            wait_for_receiver(
                &server,
                &mut SrtpContext::new(&[7; MASTER_LEN]),
                &thread_stop,
                Duration::from_secs(1),
            )
        });
        stop.store(true, Ordering::Relaxed);
        assert!(!waiter.join().unwrap().unwrap());
    }

    #[test]
    fn authenticated_bye_does_not_start_audio() {
        let (server, viewer) = sockets();
        let key = [7; MASTER_LEN];
        let mut bye = vec![0x81, rtcp::PT_BYE, 0, 1, 0, 0, 0, 1];
        SrtpContext::new(&key).protect_rtcp(&mut bye).unwrap();
        viewer.send(&bye).unwrap();
        assert!(
            !wait_for_receiver(
                &server,
                &mut SrtpContext::new(&key),
                &AtomicBool::new(false),
                Duration::from_secs(1)
            )
            .unwrap()
        );
    }

    #[test]
    fn stop_prevents_both_rtp_and_rtcp_sends() {
        let (server, viewer) = sockets();
        let control = Control::default();
        control.send(&server, b"before stop").unwrap().unwrap();
        let mut buf = [0; 1500];
        viewer.recv(&mut buf).unwrap();
        control.stop();
        assert!(control.send(&server, b"RTP").is_none());
        assert!(control.send(&server, &report(&[7; MASTER_LEN])).is_none());
        viewer.set_nonblocking(true).unwrap();
        assert_eq!(
            viewer.recv(&mut buf).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn prepared_sender_stops_without_activation_or_capture() {
        let viewer = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut sender = AudioSender::start(AudioParams {
            peer: viewer.local_addr().unwrap(),
            local_port: 0,
            ssrc: 1,
            remote_ssrc: 2,
            payload_type: 96,
            send_key: [7; MASTER_LEN],
            recv_key: [8; MASTER_LEN],
            encoder: EncoderConfig::default(),
            device: Some("nonexistent-test-monitor".into()),
            mute_local: false,
        })
        .unwrap();
        sender.stop();
        assert!(!sender.running());
        viewer.set_nonblocking(true).unwrap();
        assert_eq!(
            viewer.recv(&mut [0; 1500]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    #[ignore = "requires a PulseAudio monitor and AAC encoder"]
    fn passive_peer_receives_audio_without_initial_rtcp() {
        let viewer = UdpSocket::bind("127.0.0.1:0").unwrap();
        viewer
            .set_read_timeout(Some(Duration::from_secs(8)))
            .unwrap();
        let mut sender = AudioSender::start(AudioParams {
            peer: viewer.local_addr().unwrap(),
            local_port: 0,
            ssrc: 1,
            remote_ssrc: 2,
            payload_type: 96,
            send_key: [7; MASTER_LEN],
            recv_key: [8; MASTER_LEN],
            encoder: EncoderConfig::default(),
            device: None,
            mute_local: false,
        })
        .unwrap();
        sender.activate();
        let mut buf = [0; 1500];
        let n = viewer.recv(&mut buf).unwrap();
        let mut pkt = buf[..n].to_vec();
        SrtpContext::new(&[7; MASTER_LEN])
            .unprotect_rtp(&mut pkt)
            .unwrap();
        assert!(!is_rtcp(&pkt));
        let before = Instant::now();
        sender.stop();
        assert!(before.elapsed() < Duration::from_secs(1));
    }
}
