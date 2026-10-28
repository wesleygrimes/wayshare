//! Display Control with the same size limits as the wayshare desktop.

use ironrdp_core::{decode, impl_as_any};
use ironrdp_displaycontrol::pdu::{DisplayControlCapabilities, DisplayControlPdu};
use ironrdp_displaycontrol::server::DisplayControlHandler;
use ironrdp_dvc::{DvcMessage, DvcProcessor, DvcServerProcessor};
use ironrdp_pdu::{PduResult, decode_err};
use tracing::debug;

pub fn capabilities(max_monitors: u32) -> DisplayControlCapabilities {
    // The factors bound area, not aspect ratio. Only the primary output is applied.
    DisplayControlCapabilities::new(max_monitors, 8192, 8192).expect("valid Display Control limits")
}

pub(crate) struct DisplayControlServer {
    handler: Box<dyn DisplayControlHandler>,
    max_monitors: u32,
}

impl DisplayControlServer {
    pub(crate) fn new(handler: Box<dyn DisplayControlHandler>, max_monitors: u32) -> Self {
        Self { handler, max_monitors }
    }
}

impl_as_any!(DisplayControlServer);

impl DvcProcessor for DisplayControlServer {
    fn channel_name(&self) -> &str {
        ironrdp_displaycontrol::CHANNEL_NAME
    }

    fn start(&mut self, channel_id: u32) -> PduResult<Vec<DvcMessage>> {
        let caps = capabilities(self.max_monitors);
        tracing::info!(target: "wayshare::rdp", channel_id, ?caps, "RDP Display Control caps advertised");
        Ok(vec![Box::new(DisplayControlPdu::Caps(caps))])
    }

    fn process(&mut self, channel_id: u32, payload: &[u8]) -> PduResult<Vec<DvcMessage>> {
        // Log even malformed/unknown PDUs before attempting to decode them.
        debug!(target: "wayshare::rdp", channel_id, pdu_type = ?payload.get(..4), bytes = payload.len(),
            payload = ?payload, "RDP Display Control PDU received");
        let pdu = decode(payload).map_err(|e| decode_err!(e))?;
        debug!(target: "wayshare::rdp", channel_id, ?pdu, "RDP Display Control PDU decoded");
        match pdu {
            DisplayControlPdu::MonitorLayout(layout) => {
                tracing::info!(target: "wayshare::rdp", channel_id, monitor_count = layout.monitors().len(), "RDP Monitor Layout received");
                for (index, monitor) in layout.monitors().iter().enumerate() {
                    let (width, height) = monitor.dimensions();
                    tracing::info!(target: "wayshare::rdp", channel_id, index, width, height,
                        position = ?monitor.position(), orientation = ?monitor.orientation(),
                        primary = monitor.is_primary(), "RDP Monitor Layout monitor");
                }
                self.handler.monitor_layout(layout);
            }
            DisplayControlPdu::Caps(_) => (),
        }
        Ok(Vec::new())
    }
}

impl DvcServerProcessor for DisplayControlServer {}

#[cfg(test)]
mod tests {
    use super::*;
    use ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout;

    struct Handler;
    impl DisplayControlHandler for Handler {
        fn monitor_layout(&self, _: DisplayControlMonitorLayout) {}
    }

    #[test]
    fn advertised_monitor_count_is_encoded_in_caps() {
        for count in [1, 16] {
            let mut server = DisplayControlServer::new(Box::new(Handler), count);
            let messages = server.start(2).unwrap();
            let bytes = ironrdp_core::encode_vec(messages[0].as_ref()).unwrap();
            assert_eq!(&bytes[..8], &[5, 0, 0, 0, 20, 0, 0, 0]);
            assert_eq!(&bytes[8..12], &count.to_le_bytes());
            assert_eq!(&bytes[12..], &[0, 32, 0, 0, 0, 32, 0, 0]);
        }
    }
}
