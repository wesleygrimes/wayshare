use ironrdp_connector::{ConnectorError, ConnectorErrorExt as _, ConnectorResult, Sequence, State, Written};
use ironrdp_core::WriteBuf;
use ironrdp_pdu::rdp;
use ironrdp_pdu::x224::X224;
use tracing::debug;

use crate::util::{self, wrap_share_data};

// Credentials are checked after finalization. Allow small bursts of early input,
// but bound unauthenticated retention to 256 packets and 1 MiB per connection.
const MAX_INPUT_PACKETS: usize = 256;
const MAX_INPUT_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
pub struct FinalizationSequence {
    state: FinalizationState,
    user_channel_id: u16,
    io_channel_id: u16,

    input_events: Vec<Vec<u8>>,
    input_bytes: usize,
}

#[derive(Default, Debug)]
pub enum FinalizationState {
    #[default]
    Consumed,

    WaitSynchronize,
    WaitControlCooperate,
    WaitRequestControl,
    WaitFontList,

    SendSynchronizeConfirm,
    SendControlCooperateConfirm,
    SendGrantedControlConfirm,
    SendFontMap,

    Finished,
}

impl State for FinalizationState {
    fn name(&self) -> &'static str {
        match self {
            Self::Consumed => "Consumed",
            Self::WaitSynchronize => "WaitSynchronize",
            Self::WaitControlCooperate => "WaitControlCooperate",
            Self::WaitRequestControl => "WaitRequestControl",
            Self::WaitFontList => "WaitFontList",
            Self::SendSynchronizeConfirm => "SendSynchronizeConfirm",
            Self::SendControlCooperateConfirm => "SendControlCooperateConfirm",
            Self::SendGrantedControlConfirm => "SendGrantedControlConfirm",
            Self::SendFontMap => "SendFontMap",
            Self::Finished => "Finished",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

impl Sequence for FinalizationSequence {
    fn next_pdu_hint(&self) -> Option<&dyn ironrdp_pdu::PduHint> {
        match &self.state {
            FinalizationState::Consumed => None,
            FinalizationState::WaitSynchronize => Some(&ironrdp_pdu::X224Hint),
            FinalizationState::WaitControlCooperate => Some(&ironrdp_pdu::X224Hint),
            FinalizationState::WaitRequestControl => Some(&ironrdp_pdu::X224Hint),
            FinalizationState::WaitFontList => Some(&ironrdp_pdu::RdpHint),
            FinalizationState::SendSynchronizeConfirm => None,
            FinalizationState::SendControlCooperateConfirm => None,
            FinalizationState::SendGrantedControlConfirm => None,
            FinalizationState::SendFontMap => None,
            FinalizationState::Finished => None,
        }
    }

    fn state(&self) -> &dyn State {
        &self.state
    }

    fn step(&mut self, input: &[u8], output: &mut WriteBuf) -> ConnectorResult<Written> {
        let (written, next_state) = match core::mem::take(&mut self.state) {
            FinalizationState::WaitSynchronize => {
                let synchronize = decode_share_control(input);

                debug!(message = ?synchronize, "Received");

                (Written::Nothing, FinalizationState::WaitControlCooperate)
            }

            FinalizationState::WaitControlCooperate => {
                let cooperate = decode_share_control(input);

                debug!(message = ?cooperate, "Received");

                (Written::Nothing, FinalizationState::WaitRequestControl)
            }

            FinalizationState::WaitRequestControl => {
                let control = decode_share_control(input)?;

                debug!(message = ?control, "Received");

                (Written::Nothing, FinalizationState::WaitFontList)
            }

            FinalizationState::WaitFontList => match decode_font_list(input) {
                Ok(font_list) => {
                    debug!(message = ?font_list, "Received");

                    (Written::Nothing, FinalizationState::SendSynchronizeConfirm)
                }

                Err(()) => {
                    if self.input_events.len() >= MAX_INPUT_PACKETS || input.len() > MAX_INPUT_BYTES - self.input_bytes
                    {
                        return Err(ConnectorError::general("pre-login input backlog limit exceeded"));
                    }
                    self.input_bytes += input.len();
                    self.input_events.push(input.to_vec());

                    (Written::Nothing, FinalizationState::WaitFontList)
                }
            },

            FinalizationState::SendSynchronizeConfirm => {
                let synchronize_confirm = create_synchronize_confirm();

                debug!(message = ?synchronize_confirm, "Send");

                let share_data = wrap_share_data(synchronize_confirm, self.io_channel_id);
                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &share_data, output)?;

                (
                    Written::from_size(written)?,
                    FinalizationState::SendControlCooperateConfirm,
                )
            }

            FinalizationState::SendControlCooperateConfirm => {
                let cooperate_confirm = create_cooperate_confirm();

                debug!(message = ?cooperate_confirm, "Send");

                let share_data = wrap_share_data(cooperate_confirm, self.io_channel_id);
                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &share_data, output)?;

                (
                    Written::from_size(written)?,
                    FinalizationState::SendGrantedControlConfirm,
                )
            }

            FinalizationState::SendGrantedControlConfirm => {
                let control_confirm = create_control_confirm(self.user_channel_id);

                debug!(message = ?control_confirm, "Send");

                let share_data = wrap_share_data(control_confirm, self.io_channel_id);
                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &share_data, output)?;

                (Written::from_size(written)?, FinalizationState::SendFontMap)
            }

            FinalizationState::SendFontMap => {
                let font_map = create_font_map();

                debug!(message = ?font_map, "Send");

                let share_data = wrap_share_data(font_map, self.io_channel_id);
                let written =
                    util::encode_send_data_indication(self.user_channel_id, self.io_channel_id, &share_data, output)?;

                (Written::from_size(written)?, FinalizationState::Finished)
            }

            _ => unreachable!(),
        };

        self.state = next_state;
        Ok(written)
    }
}

impl FinalizationSequence {
    pub fn new(user_channel_id: u16, io_channel_id: u16) -> Self {
        Self {
            state: FinalizationState::WaitSynchronize,
            user_channel_id,
            io_channel_id,
            input_events: Vec::new(),
            input_bytes: 0,
        }
    }

    pub fn into_input_events(self) -> Vec<Vec<u8>> {
        self.input_events
    }

    pub fn is_done(&self) -> bool {
        self.state.is_terminal()
    }
}

fn create_synchronize_confirm() -> rdp::headers::ShareDataPdu {
    rdp::headers::ShareDataPdu::Synchronize(rdp::finalization_messages::SynchronizePdu { target_user_id: 0 })
}

fn create_cooperate_confirm() -> rdp::headers::ShareDataPdu {
    rdp::headers::ShareDataPdu::Control(rdp::finalization_messages::ControlPdu {
        action: rdp::finalization_messages::ControlAction::Cooperate,
        grant_id: 0,
        control_id: 0,
    })
}

fn create_control_confirm(user_id: u16) -> rdp::headers::ShareDataPdu {
    rdp::headers::ShareDataPdu::Control(rdp::finalization_messages::ControlPdu {
        action: rdp::finalization_messages::ControlAction::GrantedControl,
        grant_id: user_id,
        control_id: u32::from(rdp::capability_sets::SERVER_CHANNEL_ID),
    })
}

fn create_font_map() -> rdp::headers::ShareDataPdu {
    rdp::headers::ShareDataPdu::FontMap(rdp::finalization_messages::FontPdu::default())
}

fn decode_share_control(input: &[u8]) -> ConnectorResult<rdp::headers::ShareControlHeader> {
    let data_request = ironrdp_core::decode::<X224<ironrdp_pdu::mcs::SendDataRequest<'_>>>(input)
        .map_err(ConnectorError::decode)
        .map(|p| p.0)?;
    let share_control = ironrdp_core::decode::<rdp::headers::ShareControlHeader>(data_request.user_data.as_ref())
        .map_err(ConnectorError::decode)?;
    Ok(share_control)
}

fn decode_font_list(input: &[u8]) -> Result<rdp::finalization_messages::FontPdu, ()> {
    use ironrdp_pdu::rdp::headers::{ShareControlPdu, ShareDataPdu};

    let share_control = decode_share_control(input).map_err(|_| ())?;

    let ShareControlPdu::Data(data_pdu) = share_control.share_control_pdu else {
        return Err(());
    };

    let ShareDataPdu::FontList(font_pdu) = data_pdu.share_data_pdu else {
        return Err(());
    };

    Ok(font_pdu)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waiting_for_fonts() -> FinalizationSequence {
        let mut sequence = FinalizationSequence::new(1001, 1003);
        sequence.state = FinalizationState::WaitFontList;
        sequence
    }

    #[test]
    fn pre_login_input_rejects_64_mib_before_retaining_more_than_one_mib() {
        let mut sequence = waiting_for_fonts();
        let packet = vec![0; 32 * 1024];
        let mut output = WriteBuf::new();
        for index in 0..2048 {
            let result = sequence.step(&packet, &mut output);
            let retained: usize = sequence.input_events.iter().map(Vec::len).sum();
            assert!(retained <= MAX_INPUT_BYTES);
            if result.is_err() {
                assert_eq!(index, 32);
                assert_eq!(retained, MAX_INPUT_BYTES);
                return;
            }
        }
        panic!("the 64 MiB input burst must be rejected");
    }

    #[test]
    fn pre_login_input_also_limits_packet_count() {
        let mut sequence = waiting_for_fonts();
        let mut output = WriteBuf::new();
        for _ in 0..MAX_INPUT_PACKETS {
            sequence.step(&[0], &mut output).unwrap();
        }
        assert!(sequence.step(&[0], &mut output).is_err());
        assert_eq!(sequence.input_events.len(), MAX_INPUT_PACKETS);
    }

    #[test]
    fn small_input_burst_finishes_and_preserves_packets() {
        use ironrdp_core::encode_vec;
        use std::borrow::Cow;

        let mut sequence = waiting_for_fonts();
        let mut output = WriteBuf::new();
        let packets = vec![vec![0; 8], vec![1; 16]];
        for packet in &packets {
            sequence.step(packet, &mut output).unwrap();
        }
        let fonts = wrap_share_data(
            rdp::headers::ShareDataPdu::FontList(rdp::finalization_messages::FontPdu::default()),
            1001,
        );
        let fonts = encode_vec(&fonts).unwrap();
        let packet = encode_vec(&X224(ironrdp_pdu::mcs::SendDataRequest {
            initiator_id: 1001,
            channel_id: 1003,
            user_data: Cow::Borrowed(&fonts),
        }))
        .unwrap();
        sequence.step(&packet, &mut output).unwrap();
        while !sequence.is_done() {
            sequence.step(&[], &mut output).unwrap();
        }
        assert_eq!(sequence.into_input_events(), packets);
    }
}
