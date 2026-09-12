//! GoWay v1.8.4 wire primitives.
//!
//! The MUX header is exactly 7 bytes:
//!   uint32 stream id (big endian)
//!   uint8 command
//!   uint16 payload length (big endian)
//!
//! Commands are SYN=0x01, DATA=0x02, FIN=0x03 and RST=0x04.

use std::fmt;

pub const MUX_HEADER_LEN: usize = 7;
pub const MUX_SYN: u8 = 0x01;
pub const MUX_DATA: u8 = 0x02;
pub const MUX_FIN: u8 = 0x03;
pub const MUX_RST: u8 = 0x04;
pub const MAX_MUX_PAYLOAD: usize = u16::MAX as usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxCommand {
    Syn,
    Data,
    Fin,
    Rst,
}

impl MuxCommand {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Syn => MUX_SYN,
            Self::Data => MUX_DATA,
            Self::Fin => MUX_FIN,
            Self::Rst => MUX_RST,
        }
    }
}

impl TryFrom<u8> for MuxCommand {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            MUX_SYN => Ok(Self::Syn),
            MUX_DATA => Ok(Self::Data),
            MUX_FIN => Ok(Self::Fin),
            MUX_RST => Ok(Self::Rst),
            other => Err(ProtocolError::UnknownCommand(other)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxFrame {
    pub stream_id: u32,
    pub command: MuxCommand,
    pub payload: Vec<u8>,
}

impl MuxFrame {
    pub fn new(stream_id: u32, command: MuxCommand, payload: Vec<u8>) -> Result<Self, ProtocolError> {
        if payload.len() > MAX_MUX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(payload.len()));
        }
        Ok(Self { stream_id, command, payload })
    }

    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
        if self.payload.len() > MAX_MUX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(self.payload.len()));
        }
        out.reserve(MUX_HEADER_LEN + self.payload.len());
        out.extend_from_slice(&self.stream_id.to_be_bytes());
        out.push(self.command.as_u8());
        out.extend_from_slice(&(self.payload.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.payload);
        Ok(())
    }

    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < MUX_HEADER_LEN {
            return Err(ProtocolError::TruncatedHeader(buf.len()));
        }
        let stream_id = u32::from_be_bytes(buf[0..4].try_into().unwrap());
        let command = MuxCommand::try_from(buf[4])?;
        let payload_len = u16::from_be_bytes(buf[5..7].try_into().unwrap()) as usize;
        if buf.len() != MUX_HEADER_LEN + payload_len {
            return Err(ProtocolError::LengthMismatch {
                declared: payload_len,
                available: buf.len().saturating_sub(MUX_HEADER_LEN),
            });
        }
        Ok(Self {
            stream_id,
            command,
            payload: buf[MUX_HEADER_LEN..].to_vec(),
        })
    }
}

/// SYN payload: uint16 target length, target bytes, optional initial data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynPayload {
    pub target: Vec<u8>,
    pub initial_data: Vec<u8>,
}

impl SynPayload {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.target.len() > u16::MAX as usize {
            return Err(ProtocolError::TargetTooLarge(self.target.len()));
        }
        let mut out = Vec::with_capacity(2 + self.target.len() + self.initial_data.len());
        out.extend_from_slice(&(self.target.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.target);
        out.extend_from_slice(&self.initial_data);
        Ok(out)
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProtocolError> {
        if payload.len() < 2 {
            return Err(ProtocolError::TruncatedSyn);
        }
        let target_len = u16::from_be_bytes(payload[0..2].try_into().unwrap()) as usize;
        if payload.len() < 2 + target_len {
            return Err(ProtocolError::TruncatedSyn);
        }
        Ok(Self {
            target: payload[2..2 + target_len].to_vec(),
            initial_data: payload[2 + target_len..].to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    TruncatedHeader(usize),
    TruncatedSyn,
    UnknownCommand(u8),
    PayloadTooLarge(usize),
    TargetTooLarge(usize),
    LengthMismatch { declared: usize, available: usize },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader(n) => write!(f, "truncated MUX header: {n} bytes"),
            Self::TruncatedSyn => write!(f, "truncated SYN payload"),
            Self::UnknownCommand(c) => write!(f, "unknown MUX command: 0x{c:02x}"),
            Self::PayloadTooLarge(n) => write!(f, "MUX payload exceeds uint16: {n} bytes"),
            Self::TargetTooLarge(n) => write!(f, "SYN target exceeds uint16: {n} bytes"),
            Self::LengthMismatch { declared, available } => {
                write!(f, "MUX payload length mismatch: declared {declared}, available {available}")
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mux_header_is_exactly_seven_bytes() {
        let frame = MuxFrame::new(0x01020304, MuxCommand::Data, vec![1, 2, 3]).unwrap();
        let mut encoded = Vec::new();
        frame.encode(&mut encoded).unwrap();
        assert_eq!(&encoded[..7], &[1, 2, 3, 4, MUX_DATA, 0, 3]);
        assert_eq!(encoded.len(), MUX_HEADER_LEN + 3);
        assert_eq!(MuxFrame::decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn all_commands_round_trip() {
        for command in [MuxCommand::Syn, MuxCommand::Data, MuxCommand::Fin, MuxCommand::Rst] {
            let frame = MuxFrame::new(7, command, vec![]).unwrap();
            let mut bytes = Vec::new();
            frame.encode(&mut bytes).unwrap();
            assert_eq!(MuxFrame::decode(&bytes).unwrap(), frame);
        }
    }

    #[test]
    fn syn_payload_round_trip_with_initial_data() {
        let payload = SynPayload { target: b"example.com:443".to_vec(), initial_data: b"hello".to_vec() };
        let encoded = payload.encode().unwrap();
        assert_eq!(&encoded[..2], &[0, 15]);
        assert_eq!(SynPayload::decode(&encoded).unwrap(), payload);
    }

    #[test]
    fn malformed_frames_are_rejected() {
        assert!(matches!(MuxFrame::decode(&[0; 6]), Err(ProtocolError::TruncatedHeader(6))));
        assert!(matches!(MuxFrame::decode(&[0, 0, 0, 1, 0xff, 0, 0]), Err(ProtocolError::UnknownCommand(0xff))));
        assert!(matches!(MuxFrame::decode(&[0, 0, 0, 1, MUX_DATA, 0, 2, 1]), Err(ProtocolError::LengthMismatch { .. })));
    }
}
