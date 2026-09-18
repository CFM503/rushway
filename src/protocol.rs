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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxHeader {
    pub stream_id: u32,
    pub command: MuxCommand,
    pub payload_len: usize,
}

impl MuxHeader {
    pub fn parse(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < MUX_HEADER_LEN {
            return Err(ProtocolError::TruncatedHeader(buf.len()));
        }
        let stream_id = u32::from_be_bytes(buf[0..4].try_into().unwrap());
        let command = MuxCommand::try_from(buf[4])?;
        let payload_len = u16::from_be_bytes(buf[5..7].try_into().unwrap()) as usize;
        // Trailing bytes past the declared payload are obfuscation padding
        // (GoWay `-obfs`): slice by the declared length and ignore the tail.
        // Only a short buffer is an error.
        if buf.len() < MUX_HEADER_LEN + payload_len {
            return Err(ProtocolError::LengthMismatch {
                declared: payload_len,
                available: buf.len().saturating_sub(MUX_HEADER_LEN),
            });
        }
        Ok(Self {
            stream_id,
            command,
            payload_len,
        })
    }

    #[allow(dead_code)]
    pub fn payload<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[MUX_HEADER_LEN..MUX_HEADER_LEN + self.payload_len]
    }
}

pub fn encode_header(
    out: &mut Vec<u8>,
    stream_id: u32,
    command: MuxCommand,
    payload_len: usize,
) -> Result<(), ProtocolError> {
    if payload_len > MAX_MUX_PAYLOAD {
        return Err(ProtocolError::PayloadTooLarge(payload_len));
    }
    out.reserve(MUX_HEADER_LEN);
    out.extend_from_slice(&stream_id.to_be_bytes());
    out.push(command.as_u8());
    out.extend_from_slice(&(payload_len as u16).to_be_bytes());
    Ok(())
}

pub fn write_frame_parts(
    out: &mut Vec<u8>,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
) -> Result<(), ProtocolError> {
    encode_header(out, stream_id, command, payload.len())?;
    out.extend_from_slice(payload);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxFrame {
    pub stream_id: u32,
    pub command: MuxCommand,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct OwnedMuxFrame {
    pub stream_id: u32,
    pub command: MuxCommand,
    storage: Vec<u8>,
    payload_len: usize,
}

impl OwnedMuxFrame {
    /// Payload bounded by the declared MUX length. Storage may hold extra
    /// trailing bytes (GoWay `-obfs` padding); those must never leak into
    /// the relayed stream.
    pub fn payload(&self) -> &[u8] {
        &self.storage[MUX_HEADER_LEN..MUX_HEADER_LEN + self.payload_len]
    }

    #[allow(dead_code)]
    pub fn into_storage(self) -> Vec<u8> {
        self.storage
    }
}

#[allow(dead_code)]
impl MuxFrame {
    pub fn new(
        stream_id: u32,
        command: MuxCommand,
        payload: Vec<u8>,
    ) -> Result<Self, ProtocolError> {
        if payload.len() > MAX_MUX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(payload.len()));
        }
        Ok(Self {
            stream_id,
            command,
            payload,
        })
    }

    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
        write_frame_parts(out, self.stream_id, self.command, &self.payload)
    }

    #[allow(dead_code)]
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let header = MuxHeader::parse(buf)?;
        Ok(Self {
            stream_id: header.stream_id,
            command: header.command,
            payload: header.payload(buf).to_vec(),
        })
    }

    pub fn decode_owned(buf: Vec<u8>) -> Result<OwnedMuxFrame, ProtocolError> {
        let header = MuxHeader::parse(&buf)?;
        Ok(OwnedMuxFrame {
            stream_id: header.stream_id,
            command: header.command,
            payload_len: header.payload_len,
            storage: buf,
        })
    }
}

/// SYN payload: uint16 target length, target bytes, optional initial data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct SynPayload {
    pub target: Vec<u8>,
    pub initial_data: Vec<u8>,
}

#[allow(dead_code)]
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
    #[allow(dead_code)]
    TruncatedSyn,
    UnknownCommand(u8),
    PayloadTooLarge(usize),
    #[allow(dead_code)]
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
            Self::LengthMismatch {
                declared,
                available,
            } => {
                write!(
                    f,
                    "MUX payload length mismatch: declared {declared}, available {available}"
                )
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
    fn zero_copy_header_parse_round_trip() {
        let mut encoded = Vec::new();
        write_frame_parts(&mut encoded, 0x01020304, MuxCommand::Data, &[1, 2, 3, 4]).unwrap();
        let header = MuxHeader::parse(&encoded).unwrap();
        assert_eq!(header.stream_id, 0x01020304);
        assert_eq!(header.command, MuxCommand::Data);
        assert_eq!(header.payload_len, 4);
        assert_eq!(header.payload(&encoded), &[1, 2, 3, 4]);
    }

    #[test]
    fn owned_decode_keeps_original_payload_storage() {
        let mut encoded = Vec::with_capacity(MUX_HEADER_LEN + 4);
        write_frame_parts(&mut encoded, 7, MuxCommand::Data, &[1, 2, 3, 4]).unwrap();
        let ptr = encoded.as_ptr();
        let frame = MuxFrame::decode_owned(encoded).unwrap();
        assert_eq!(frame.payload(), &[1, 2, 3, 4]);
        assert_eq!(frame.payload().as_ptr(), unsafe { ptr.add(MUX_HEADER_LEN) });
    }

    #[test]
    fn all_commands_round_trip() {
        for command in [
            MuxCommand::Syn,
            MuxCommand::Data,
            MuxCommand::Fin,
            MuxCommand::Rst,
        ] {
            let frame = MuxFrame::new(7, command, vec![]).unwrap();
            let mut bytes = Vec::new();
            frame.encode(&mut bytes).unwrap();
            assert_eq!(MuxFrame::decode(&bytes).unwrap(), frame);
        }
    }

    #[test]
    fn syn_payload_round_trip_with_initial_data() {
        let payload = SynPayload {
            target: b"example.com:443".to_vec(),
            initial_data: b"hello".to_vec(),
        };
        let encoded = payload.encode().unwrap();
        assert_eq!(&encoded[..2], &[0, 15]);
        assert_eq!(SynPayload::decode(&encoded).unwrap(), payload);
    }

    #[test]
    fn malformed_frames_are_rejected() {
        assert!(matches!(
            MuxFrame::decode(&[0; 6]),
            Err(ProtocolError::TruncatedHeader(6))
        ));
        assert!(matches!(
            MuxFrame::decode(&[0, 0, 0, 1, 0xff, 0, 0]),
            Err(ProtocolError::UnknownCommand(0xff))
        ));
        assert!(matches!(
            MuxFrame::decode(&[0, 0, 0, 1, MUX_DATA, 0, 2, 1]),
            Err(ProtocolError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn trailing_obfs_padding_is_ignored() {
        // Declared payload is 2 bytes ("hi"); 1400 random pad bytes follow
        // (GoWay `-obfs` unilateral padding must not break decoding).
        let mut encoded = vec![0, 0, 0, 1, MUX_DATA, 0, 2, b'h', b'i'];
        encoded.extend_from_slice(&[0x5Au8; 1400]);
        let frame = MuxFrame::decode(&encoded).unwrap();
        assert_eq!(frame.stream_id, 1);
        assert_eq!(frame.command, MuxCommand::Data);
        assert_eq!(frame.payload, b"hi");
        // The zero-copy owned path must bound by the declared length too —
        // this was the live data-leak (pad bytes forwarded into streams).
        let owned = MuxFrame::decode_owned(encoded).unwrap();
        assert_eq!(owned.payload(), b"hi");
    }
}
