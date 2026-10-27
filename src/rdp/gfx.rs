//! EGFX negotiation and bounded frame delivery. Surface commands and frames share the
//! display queue so a reset or a RemoteFX fallback cannot overtake an encoded picture.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ironrdp_core::{Encode, EncodeResult, WriteCursor, encode_vec};
use ironrdp_dvc::{DvcEncode, DvcMessage, encode_dvc_messages};
use ironrdp_egfx::pdu::*;
use ironrdp_egfx::server::{GraphicsPipelineHandler, GraphicsPipelineServer};
use ironrdp_graphics::zgfx::wrap_uncompressed;
use ironrdp_pdu::gcc::{Monitor, MonitorFlags};
use ironrdp_pdu::geometry::ExclusiveRectangle;
use ironrdp_server::{
    DesktopSize, GfxDvcBridge, GfxServerFactory, GfxServerHandle, ServerEvent, ServerEventSender,
};
use ironrdp_svc::ChannelFlags;
use tracing::{debug, info, warn};

use super::encoder::Encoder;
use crate::source::Frame;

const ACK_TIMEOUT: Duration = Duration::from_secs(2);
const CAPS_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Waiting,
    Avc420,
    EgfxRemoteFx,
    Legacy,
}

pub struct Connection {
    pub handle: Mutex<Option<GfxServerHandle>>,
    state: Arc<Mutex<State>>,
    fail_after: Mutex<Option<u64>>,
    experimental_remotefx: bool,
}

struct State {
    mode: Mode,
    started: Option<Instant>,
    flow: Flow,
}

impl Connection {
    pub fn new(fail_after: Option<u64>, experimental_remotefx: bool) -> Arc<Self> {
        Arc::new(Self {
            handle: Mutex::new(None),
            experimental_remotefx,
            fail_after: Mutex::new(fail_after),
            state: Arc::new(Mutex::new(State {
                mode: Mode::Waiting,
                started: None,
                flow: Flow::new(),
            })),
        })
    }

    pub fn mode(&self) -> Mode {
        let mut state = self.state.lock().unwrap();
        let started = *state.started.get_or_insert_with(Instant::now);
        if state.mode == Mode::Waiting && started.elapsed() >= CAPS_TIMEOUT {
            state.mode = Mode::Legacy;
            info!(
                codec = "RemoteFX/bitmap",
                reason = "client did not negotiate EGFX/AVC420",
                "RDP codec negotiated"
            );
        }
        state.mode
    }

    pub fn fallback(&self, reason: &str) {
        self.state.lock().unwrap().mode = Mode::Legacy;
        info!(codec = "RemoteFX/bitmap", %reason, "RDP codec negotiated");
    }
}

pub struct Factory(pub Arc<Connection>);
impl ServerEventSender for Factory {
    fn set_sender(&mut self, _: tokio::sync::mpsc::UnboundedSender<ServerEvent>) {}
}
impl GfxServerFactory for Factory {
    fn build_gfx_handler(&self) -> Box<dyn GraphicsPipelineHandler> {
        Box::new(Handler {
            state: self.0.state.clone(),
            preferred: vec![],
            experimental_remotefx: self.0.experimental_remotefx,
        })
    }
    fn build_server_with_handle(&self) -> Option<(GfxDvcBridge, GfxServerHandle)> {
        let server = Arc::new(Mutex::new(GraphicsPipelineServer::new(
            self.build_gfx_handler(),
        )));
        *self.0.handle.lock().unwrap() = Some(server.clone());
        Some((GfxDvcBridge::new(server.clone()), server))
    }
}

struct Handler {
    state: Arc<Mutex<State>>,
    preferred: Vec<CapabilitySet>,
    experimental_remotefx: bool,
}

fn supports_avc420(cap: &CapabilitySet) -> bool {
    match cap {
        CapabilitySet::V8 { .. } => false,
        CapabilitySet::V8_1 { flags } => flags.contains(CapabilitiesV81Flags::AVC420_ENABLED),
        CapabilitySet::V10 { flags } | CapabilitySet::V10_2 { flags } => {
            !flags.contains(CapabilitiesV10Flags::AVC_DISABLED)
        }
        CapabilitySet::V10_1 => true,
        CapabilitySet::V10_3 { flags } => !flags.contains(CapabilitiesV103Flags::AVC_DISABLED),
        CapabilitySet::V10_4 { flags }
        | CapabilitySet::V10_5 { flags }
        | CapabilitySet::V10_6 { flags }
        | CapabilitySet::V10_6Err { flags } => !flags.contains(CapabilitiesV104Flags::AVC_DISABLED),
        CapabilitySet::V10_7 { flags } => !flags.contains(CapabilitiesV107Flags::AVC_DISABLED),
    }
}

fn rank(cap: &CapabilitySet) -> u8 {
    match cap {
        CapabilitySet::V8 { .. } => 0,
        CapabilitySet::V8_1 { .. } => 1,
        CapabilitySet::V10 { .. } => 2,
        CapabilitySet::V10_1 => 3,
        CapabilitySet::V10_2 { .. } => 4,
        CapabilitySet::V10_3 { .. } => 5,
        CapabilitySet::V10_4 { .. } => 6,
        CapabilitySet::V10_5 { .. } => 7,
        CapabilitySet::V10_6 { .. } => 8,
        CapabilitySet::V10_6Err { .. } => 9,
        CapabilitySet::V10_7 { .. } => 10,
    }
}

fn select_cap(caps: impl IntoIterator<Item = CapabilitySet>) -> Option<CapabilitySet> {
    // Preserve the offered flags verbatim. EGFX 0.3 intersects server/client flags,
    // which would otherwise erase a client's AVC_DISABLED flag.
    caps.into_iter().max_by_key(rank)
}

impl GraphicsPipelineHandler for Handler {
    fn capabilities_advertise(&mut self, pdu: &CapabilitiesAdvertisePdu) {
        debug!(?pdu, "RDP EGFX CapsAdvertise");
        self.preferred = select_cap(pdu.0.iter().filter_map(|raw| raw.parsed().ok().flatten()))
            .into_iter()
            .collect();
    }
    fn preferred_capabilities(&self) -> Vec<CapabilitySet> {
        self.preferred.clone()
    }
    fn on_ready(&mut self, cap: &CapabilitySet) {
        debug!(?cap, "RDP EGFX CapsConfirm");
        let mut state = self.state.lock().unwrap();
        if state.mode == Mode::Waiting {
            state.mode = if self.experimental_remotefx {
                Mode::EgfxRemoteFx
            } else if supports_avc420(cap) {
                Mode::Avc420
            } else {
                Mode::Legacy
            };
            if state.mode == Mode::EgfxRemoteFx {
                info!(
                    codec = "EGFX RemoteFX",
                    reason = "experimental rdp.codec = egfx-remotefx",
                    "RDP codec negotiated"
                );
            } else if state.mode == Mode::Legacy {
                info!(
                    codec = "RemoteFX/bitmap",
                    reason = "client EGFX caps disable AVC420; using legacy graphics",
                    "RDP codec negotiated"
                );
            }
        }
    }
    fn on_frame_ack(&mut self, id: u32, depth: u32, decoded: u32) {
        debug!(id, depth, decoded, "RDP EGFX frame ACK");
        self.state
            .lock()
            .unwrap()
            .flow
            .ack(id, depth, decoded, Instant::now());
    }
    fn on_close(&mut self) {
        self.state.lock().unwrap().mode = Mode::Legacy;
        debug!("RDP EGFX channel closed");
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Pace {
    Ready,
    Wait,
    Recover,
}
struct Flow {
    pending: VecDeque<(u32, Instant)>,
    next: u32,
    suspended: bool,
    suspended_from: u32,
    depth: u32,
    decoded: Option<u32>,
    stalled_decode: Option<Instant>,
    recoveries: u8,
}
impl Flow {
    fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            next: 1,
            suspended: false,
            suspended_from: 1,
            depth: 0,
            decoded: None,
            stalled_decode: None,
            recoveries: 0,
        }
    }
    fn ack(&mut self, id: u32, depth: u32, decoded: u32, now: Instant) {
        if depth == u32::MAX {
            if !self.suspended {
                self.suspended_from = self.next;
            }
            self.suspended = true;
            self.pending.clear();
            self.depth = 0;
            self.stalled_decode = None;
            return;
        }
        if self.suspended {
            if self.next.wrapping_sub(id) == 0
                || self.next.wrapping_sub(id) > self.next.wrapping_sub(self.suspended_from)
            {
                return;
            }
        } else {
            let Some(index) = self.pending.iter().position(|(frame, _)| *frame == id) else {
                return;
            };
            self.pending.drain(..=index);
        }
        self.suspended = false;
        self.depth = depth;
        if self.decoded.is_none_or(|old| {
            decoded.wrapping_sub(old) > 0 && decoded.wrapping_sub(old) < (1 << 31)
        }) {
            self.stalled_decode = None;
            self.decoded = Some(decoded);
        } else {
            self.stalled_decode.get_or_insert(now);
        }
        if depth >= 2 {
            self.stalled_decode.get_or_insert(now);
        }
    }
    fn pace(&self, now: Instant) -> Pace {
        if self
            .pending
            .front()
            .is_some_and(|(_, sent)| now.duration_since(*sent) >= ACK_TIMEOUT)
            || self
                .stalled_decode
                .is_some_and(|since| now.duration_since(since) >= ACK_TIMEOUT)
        {
            Pace::Recover
        } else if !self.suspended && (self.pending.len() >= 2 || self.depth >= 2) {
            Pace::Wait
        } else {
            Pace::Ready
        }
    }
    fn sent(&mut self, now: Instant) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        if !self.suspended {
            self.pending.push_back((id, now));
        }
        id
    }
    fn reset(&mut self) {
        self.pending.clear();
        self.suspended_from = self.next;
        self.depth = 0;
        self.decoded = None;
        self.stalled_decode = None;
    }
    fn recover(&mut self) -> bool {
        self.reset();
        self.recoveries += 1;
        self.recoveries <= 1
    }
}

enum FrameEncoder {
    Avc420(Encoder),
    RemoteFx,
}

impl FrameEncoder {
    fn new(mode: Mode, size: DesktopSize) -> Result<Self> {
        match mode {
            Mode::Avc420 => Ok(Self::Avc420(Encoder::new(size.width, size.height)?)),
            Mode::EgfxRemoteFx => Ok(Self::RemoteFx),
            _ => anyhow::bail!("EGFX codec is not ready"),
        }
    }

    fn encode(&mut self, frame: &Frame, idr: bool) -> Result<Vec<u8>> {
        match self {
            Self::Avc420(encoder) => encoder.encode(frame, idr),
            Self::RemoteFx => ironrdp_server::encode_remotefx(&super::bitmap(frame)?),
        }
    }

    fn description(&self) -> (&'static str, &'static str) {
        match self {
            Self::Avc420(_) => ("H.264/AVC420", "client EGFX caps support AVC420"),
            Self::RemoteFx => ("EGFX RemoteFX", "experimental rdp.codec = egfx-remotefx"),
        }
    }
}

pub struct Stream {
    connection: Arc<Connection>,
    encoder: FrameEncoder,
    size: DesktopSize,
    surface: u16,
    first: bool,
    frames: u64,
    started: Instant,
}

impl Stream {
    pub fn new(connection: Arc<Connection>, size: DesktopSize) -> Result<Self> {
        let encoder = FrameEncoder::new(connection.mode(), size)?;
        let (codec, reason) = encoder.description();
        let implementation = match &encoder {
            FrameEncoder::Avc420(encoder) => encoder.name(),
            FrameEncoder::RemoteFx => "RemoteFX RLGR3 (software)",
        };
        info!(
            codec,
            encoder = implementation,
            reason,
            "RDP codec negotiated"
        );
        let handle = connection
            .handle
            .lock()
            .unwrap()
            .clone()
            .context("EGFX handle missing")?;
        let mut server = handle.lock().unwrap();
        reset(&mut server, size);
        let surface = server
            .create_surface(size.width, size.height)
            .context("EGFX create surface")?;
        anyhow::ensure!(
            server.map_surface_to_output(surface, 0, 0),
            "EGFX map surface"
        );
        connection.state.lock().unwrap().flow.reset();
        info!(
            width = size.width,
            height = size.height,
            surface,
            codec,
            "RDP EGFX reset; next frame is a full refresh"
        );
        Ok(Self {
            connection,
            encoder,
            size,
            surface,
            first: true,
            frames: 0,
            started: Instant::now(),
        })
    }

    pub fn request_refresh(&mut self) {
        self.first = true;
    }

    pub fn frame(&mut self, frame: &Frame) -> Result<Option<Vec<Vec<u8>>>> {
        let pace = self
            .connection
            .state
            .lock()
            .unwrap()
            .flow
            .pace(Instant::now());
        match pace {
            Pace::Wait => return Ok(None),
            Pace::Recover => self.recover("client ACK/decode progress timed out")?,
            Pace::Ready => (),
        }
        let mut idr = self.first || self.frames.is_multiple_of(300);
        let injected = matches!(self.encoder, FrameEncoder::Avc420(_)) && {
            let mut fail_after = self.connection.fail_after.lock().unwrap();
            if *fail_after == Some(self.frames) {
                fail_after.take();
                true
            } else {
                false
            }
        };
        let encoded = if injected {
            Err(anyhow::anyhow!("injected H.264 encoder failure"))
        } else {
            self.encoder.encode(frame, idr)
        };
        let data = match encoded {
            Ok(data) => data,
            Err(error) => {
                self.recover(&format!("encoder error: {error:#}"))?;
                idr = true;
                self.encoder.encode(frame, true)?
            }
        };
        let handle = self
            .connection
            .handle
            .lock()
            .unwrap()
            .clone()
            .context("EGFX handle missing")?;
        let mut server = handle.lock().unwrap();
        let channel = server.channel_id().context("EGFX channel missing")?;
        let mut messages = server.drain_output();
        let id = self
            .connection
            .state
            .lock()
            .unwrap()
            .flow
            .sent(Instant::now());
        let millis = self.started.elapsed().as_millis() as u64;
        let timestamp = Timestamp {
            hours: ((millis / 3_600_000) % 24) as u16,
            minutes: ((millis / 60_000) % 60) as u8,
            seconds: ((millis / 1000) % 60) as u8,
            milliseconds: (millis % 1000) as u16,
        };
        let bytes = data.len();
        let (codec_id, bitmap_data) = match self.encoder {
            FrameEncoder::Avc420(_) => (
                Codec1Type::Avc420,
                encode_avc420_bitmap_stream(
                    &[Avc420Region::full_frame(
                        self.size.width,
                        self.size.height,
                        24,
                    )],
                    &data,
                ),
            ),
            FrameEncoder::RemoteFx => (Codec1Type::RemoteFx, data),
        };
        // Keep our own bounded frame tracker: EGFX 0.3 only removes exact ACK ids and
        // retains sent frames indefinitely when ACKs are suspended. ACKs are cumulative.
        for pdu in [
            GfxPdu::StartFrame(StartFramePdu {
                timestamp,
                frame_id: id,
            }),
            GfxPdu::WireToSurface1(WireToSurface1Pdu {
                surface_id: self.surface,
                codec_id,
                pixel_format: PixelFormat::XRgb,
                destination_rectangle: ExclusiveRectangle {
                    left: 0,
                    top: 0,
                    right: self.size.width,
                    bottom: self.size.height,
                },
                bitmap_data,
            }),
            GfxPdu::EndFrame(EndFramePdu { frame_id: id }),
        ] {
            messages.push(Box::new(Wrapped(wrap_uncompressed(&encode_vec(&pdu)?))));
        }
        self.frames += 1;
        self.first = false;
        debug!(
            id,
            idr,
            bytes,
            frame = self.frames,
            codec = self.description().0,
            "RDP EGFX frame sent"
        );
        wire(channel, messages).map(Some)
    }

    pub fn recover(&mut self, reason: &str) -> Result<()> {
        anyhow::ensure!(
            self.connection.state.lock().unwrap().flow.recover(),
            "EGFX recovery exhausted: {reason}"
        );
        warn!(%reason, "RDP EGFX recovery: reset surface and encoder for full refresh");
        self.encoder = FrameEncoder::new(self.connection.mode(), self.size)?;
        let handle = self
            .connection
            .handle
            .lock()
            .unwrap()
            .clone()
            .context("EGFX handle missing")?;
        let mut server = handle.lock().unwrap();
        reset(&mut server, self.size);
        self.surface = server
            .create_surface(self.size.width, self.size.height)
            .context("EGFX recovery surface")?;
        anyhow::ensure!(
            server.map_surface_to_output(self.surface, 0, 0),
            "EGFX recovery mapping"
        );
        self.first = true;
        Ok(())
    }

    pub fn description(&self) -> (&'static str, &'static str) {
        self.encoder.description()
    }

    pub fn needs_frame(&self) -> bool {
        self.connection
            .state
            .lock()
            .unwrap()
            .flow
            .pace(Instant::now())
            == Pace::Recover
    }

    pub fn retire(&self) -> Result<Vec<Vec<u8>>> {
        let handle = self
            .connection
            .handle
            .lock()
            .unwrap()
            .clone()
            .context("EGFX handle missing")?;
        let mut server = handle.lock().unwrap();
        reset(&mut server, self.size);
        wire(
            server.channel_id().context("EGFX channel missing")?,
            server.drain_output(),
        )
    }
}

fn reset(server: &mut GraphicsPipelineServer, size: DesktopSize) {
    server.resize_with_monitors(
        size.width,
        size.height,
        vec![Monitor {
            left: 0,
            top: 0,
            right: i32::from(size.width) - 1,
            bottom: i32::from(size.height) - 1,
            flags: MonitorFlags::PRIMARY,
        }],
    );
}

fn wire(channel: u32, messages: Vec<DvcMessage>) -> Result<Vec<Vec<u8>>> {
    encode_dvc_messages(channel, messages, ChannelFlags::SHOW_PROTOCOL)?
        .into_iter()
        .map(|m| Ok(m.encode_unframed_pdu()?))
        .collect()
}
struct Wrapped(Vec<u8>);
impl Encode for Wrapped {
    fn encode(&self, cursor: &mut WriteCursor<'_>) -> EncodeResult<()> {
        cursor.write_slice(&self.0);
        Ok(())
    }
    fn size(&self) -> usize {
        self.0.len()
    }
    fn name(&self) -> &'static str {
        "RDP EGFX frame"
    }
}
impl DvcEncode for Wrapped {}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_dvc::DvcProcessor;

    fn negotiate(caps: &[CapabilitySet]) -> (CapabilitySet, Mode) {
        let connection = Connection::new(None, false);
        let factory = Factory(connection.clone());
        let (_, handle) = factory.build_server_with_handle().unwrap();
        let mut server = handle.lock().unwrap();
        server.start(7).unwrap();
        server
            .process(
                7,
                &encode_vec(&GfxPdu::CapabilitiesAdvertise(
                    CapabilitiesAdvertisePdu::from_typed(caps),
                ))
                .unwrap(),
            )
            .unwrap();
        (
            server.negotiated_capabilities().unwrap().clone(),
            connection.mode(),
        )
    }

    #[test]
    fn iphone_caps_choose_legacy_without_clearing_avc_disabled() {
        let raw = [
            (0x00080004, 0x2_u32),
            (0x00080105, 0x2),
            (0x000a0002, 0x22),
            (0x000a0200, 0x22),
            (0x000a0301, 0x20),
            (0x000a0400, 0x22),
            (0x000b0101, 0x1a2),
            (0x000b0300, 0x1a2),
        ];
        let caps = CapabilitiesAdvertisePdu(
            raw.into_iter()
                .map(|(version, flags)| {
                    RawCapabilitySet::new(CapabilityVersion(version), flags.to_le_bytes().to_vec())
                })
                .collect(),
        );
        let connection = Connection::new(None, false);
        let (_, handle) = Factory(connection.clone())
            .build_server_with_handle()
            .unwrap();
        let mut server = handle.lock().unwrap();
        server.start(7).unwrap();
        server
            .process(
                7,
                &encode_vec(&GfxPdu::CapabilitiesAdvertise(caps)).unwrap(),
            )
            .unwrap();
        assert_eq!(
            server.negotiated_capabilities(),
            Some(&CapabilitySet::V10_4 {
                flags: CapabilitiesV104Flags::SMALL_CACHE | CapabilitiesV104Flags::AVC_DISABLED,
            })
        );
        assert_eq!(connection.mode(), Mode::Legacy);
    }

    #[test]
    fn remotefx_context_and_tileset_modes_agree() {
        let frame = Frame {
            width: 65,
            height: 65,
            pixels: vec![0x00336699; 65 * 65],
        };
        let data = FrameEncoder::RemoteFx.encode(&frame, true).unwrap();
        let mut remaining = data.as_slice();
        let mut context_mode = None;
        let mut tileset_mode = None;
        while !remaining.is_empty() {
            let block_type = u16::from_le_bytes(remaining[..2].try_into().unwrap());
            let len = u32::from_le_bytes(remaining[2..6].try_into().unwrap()) as usize;
            let block = &remaining[..len];
            match block_type {
                0xccc3 => {
                    let properties = u16::from_le_bytes(block[11..13].try_into().unwrap());
                    context_mode = Some(properties & 7);
                }
                0xccc7 => {
                    let properties = u16::from_le_bytes(block[12..14].try_into().unwrap());
                    tileset_mode = Some((properties >> 1) & 7);
                }
                _ => (),
            }
            remaining = &remaining[len..];
        }
        assert!(context_mode.is_some());
        assert_eq!(
            context_mode, tileset_mode,
            "MS-RDPRFX 2.2.2.3.4 requires matching flags"
        );
    }

    #[test]
    fn experimental_remotefx_requires_explicit_opt_in() {
        let connection = Connection::new(None, true);
        let mut handler = Factory(connection.clone()).build_gfx_handler();
        handler.on_ready(&CapabilitySet::V10_4 {
            flags: CapabilitiesV104Flags::AVC_DISABLED,
        });
        assert_eq!(connection.mode(), Mode::EgfxRemoteFx);
    }

    #[test]
    fn reset_surface_and_monitor_use_exact_odd_dimensions() {
        for (width, height) in [
            (836, 1580),
            (1580, 797),
            (1579, 797),
            (2868, 1320),
            (1320, 2868),
        ] {
            let connection = Connection::new(None, false);
            let (_, handle) = Factory(connection.clone())
                .build_server_with_handle()
                .unwrap();
            let mut server = handle.lock().unwrap();
            server.start(7).unwrap();
            server
                .process(
                    7,
                    &encode_vec(&GfxPdu::CapabilitiesAdvertise(
                        CapabilitiesAdvertisePdu::from_typed(&[CapabilitySet::V10_4 {
                            flags: CapabilitiesV104Flags::AVC_DISABLED,
                        }]),
                    ))
                    .unwrap(),
                )
                .unwrap();
            server.drain_output();
            let size = DesktopSize { width, height };
            reset(&mut server, size);
            server.create_surface(width, height).unwrap();
            let mut pdus = Vec::new();
            for msg in server.drain_output() {
                let bytes = encode_vec(&*msg).unwrap();
                let mut raw = Vec::new();
                ironrdp_graphics::zgfx::Decompressor::new()
                    .decompress(&bytes, &mut raw)
                    .unwrap();
                pdus.push(ironrdp_core::decode::<GfxPdu>(&raw).unwrap());
            }
            assert!(pdus.iter().any(|p| matches!(p, GfxPdu::ResetGraphics(p)
                if p.width == u32::from(width) && p.height == u32::from(height)
                && p.monitors.len() == 1 && p.monitors[0].right == i32::from(width) - 1
                && p.monitors[0].bottom == i32::from(height) - 1)));
            assert!(pdus.iter().any(|p| matches!(p, GfxPdu::CreateSurface(p)
                if p.width == width && p.height == height)));
        }
    }

    #[test]
    fn negotiation_preserves_disabled_flags_in_every_version() {
        for cap in [
            CapabilitySet::V8 {
                flags: CapabilitiesV8Flags::empty(),
            },
            CapabilitySet::V8_1 {
                flags: CapabilitiesV81Flags::SMALL_CACHE,
            },
            CapabilitySet::V10 {
                flags: CapabilitiesV10Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_2 {
                flags: CapabilitiesV10Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_3 {
                flags: CapabilitiesV103Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_4 {
                flags: CapabilitiesV104Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_5 {
                flags: CapabilitiesV104Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_6 {
                flags: CapabilitiesV104Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_6Err {
                flags: CapabilitiesV104Flags::AVC_DISABLED,
            },
            CapabilitySet::V10_7 {
                flags: CapabilitiesV107Flags::AVC_DISABLED,
            },
        ] {
            let (confirmed, mode) = negotiate(std::slice::from_ref(&cap));
            assert_eq!(confirmed, cap);
            assert_eq!(mode, Mode::Legacy);
        }
    }

    #[test]
    fn negotiation_chooses_highest_offered_version_without_bypassing_avc_disabled() {
        let v81 = CapabilitySet::V8_1 {
            flags: CapabilitiesV81Flags::AVC420_ENABLED,
        };
        let v107 = CapabilitySet::V10_7 {
            flags: CapabilitiesV107Flags::SMALL_CACHE,
        };
        let (cap, mode) = negotiate(&[v81.clone(), v107.clone()]);
        assert_eq!(cap, v107);
        assert_eq!(mode, Mode::Avc420);
        let (cap, mode) = negotiate(&[
            v81.clone(),
            CapabilitySet::V10 {
                flags: CapabilitiesV10Flags::AVC_DISABLED,
            },
        ]);
        assert_eq!(
            cap,
            CapabilitySet::V10 {
                flags: CapabilitiesV10Flags::AVC_DISABLED
            }
        );
        assert_eq!(mode, Mode::Legacy);
        assert_eq!(
            negotiate(&[CapabilitySet::V10_1]),
            (CapabilitySet::V10_1, Mode::Avc420)
        );
    }

    #[test]
    fn absent_egfx_and_explicit_fallback_are_permanent_for_the_session() {
        let connection = Connection::new(None, false);
        connection.state.lock().unwrap().started = Some(Instant::now() - CAPS_TIMEOUT);
        assert_eq!(connection.mode(), Mode::Legacy);
        let mut handler = Handler {
            state: connection.state.clone(),
            preferred: vec![],
            experimental_remotefx: false,
        };
        handler.on_ready(&CapabilitySet::V10_1);
        assert_eq!(connection.mode(), Mode::Legacy);
        connection.fallback("encoder unavailable");
        assert_eq!(connection.mode(), Mode::Legacy);
    }

    #[test]
    fn cumulative_ack_releases_two_frames_and_ignores_stale_or_future_ack() {
        let mut flow = Flow::new();
        let now = Instant::now();
        let first = flow.sent(now);
        let second = flow.sent(now);
        assert_eq!(flow.pace(now), Pace::Wait);
        flow.ack(second + 10, 0, 100, now);
        assert_eq!(flow.pending.len(), 2);
        flow.ack(second, 0, 2, now);
        assert!(flow.pending.is_empty());
        assert_eq!(flow.pace(now), Pace::Ready);
        flow.ack(first, 100, 1, now);
        assert_eq!(flow.depth, 0);
    }

    #[test]
    fn ack_suspension_keeps_memory_bounded_and_normal_ack_resumes_limits() {
        let mut flow = Flow::new();
        let now = Instant::now();
        flow.sent(now);
        flow.ack(0, u32::MAX, 0, now);
        for _ in 0..1000 {
            flow.sent(now);
        }
        assert!(flow.pending.is_empty());
        assert_eq!(flow.pace(now), Pace::Ready);
        // A normal ACK for a frame sent while suspended resumes acknowledgement tracking.
        flow.ack(flow.next.wrapping_sub(50), 0, 951, now);
        assert!(!flow.suspended);
        flow.sent(now);
        flow.sent(now);
        assert_eq!(flow.pace(now), Pace::Wait);
    }

    #[test]
    fn ack_timeout_and_decode_stall_allow_one_idr_reset_then_fallback() {
        let mut flow = Flow::new();
        let now = Instant::now();
        flow.sent(now);
        assert_eq!(flow.pace(now + ACK_TIMEOUT), Pace::Recover);
        assert!(flow.recover());
        let id = flow.sent(now);
        flow.ack(id, 3, 0, now);
        assert_eq!(flow.pace(now), Pace::Wait);
        assert_eq!(flow.pace(now + ACK_TIMEOUT), Pace::Recover);
        assert!(!flow.recover());
        assert!(flow.pending.is_empty());
    }

    #[test]
    fn frame_ids_survive_resize_and_wrap_without_accepting_old_acks() {
        let mut flow = Flow::new();
        let now = Instant::now();
        flow.next = u32::MAX;
        assert_eq!(flow.sent(now), u32::MAX);
        flow.reset();
        assert_eq!(flow.sent(now), 0);
        flow.ack(u32::MAX, 0, 1, now);
        assert_eq!(flow.pending.len(), 1);
        flow.ack(0, 0, 2, now);
        assert!(flow.pending.is_empty());
    }
}
