//! Display Control with the same size limits as the wayshare desktop.

use ironrdp_core::{decode, impl_as_any};
use ironrdp_displaycontrol::pdu::{DisplayControlCapabilities, DisplayControlPdu};
use ironrdp_displaycontrol::server::DisplayControlHandler;
use ironrdp_dvc::{DvcMessage, DvcProcessor, DvcServerProcessor};
use ironrdp_pdu::{PduResult, decode_err};
use tracing::debug;

pub fn capabilities() -> DisplayControlCapabilities {
    // One output, up to 8192 by 8192. The factors bound area, not aspect ratio.
    DisplayControlCapabilities::new(1, 8192, 8192).expect("valid Display Control limits")
}

pub(crate) struct DisplayControlServer {
    handler: Box<dyn DisplayControlHandler>,
}

impl DisplayControlServer {
    pub(crate) fn new(handler: Box<dyn DisplayControlHandler>) -> Self {
        Self { handler }
    }
}

impl_as_any!(DisplayControlServer);

impl DvcProcessor for DisplayControlServer {
    fn channel_name(&self) -> &str {
        ironrdp_displaycontrol::CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        let caps = capabilities();
        debug!(target: "wayshare::rdp", channel_id, ?caps, "RDP Display Control caps queued");
        Ok(vec![Box::new(DisplayControlPdu::Caps(caps))])
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        // Log even malformed/unknown PDUs before attempting to decode them.
        debug!(target: "wayshare::rdp", channel_id, pdu_type = ?payload.get(..4), bytes = payload.len(),
            payload = ?payload, "RDP Display Control PDU received");
        let pdu = decode(payload).map_err(|e| decode_err!(e))?;
        debug!(target: "wayshare::rdp", channel_id, ?pdu, "RDP Display Control PDU decoded");
        match pdu {
            DisplayControlPdu::MonitorLayout(layout) => self.handler.monitor_layout(layout),
            DisplayControlPdu::Caps(_) => (),
        }
        Ok(Vec::new())
    }
}

impl DvcServerProcessor for DisplayControlServer {}
