use bitflags::bitflags;
use bytes::{BufMut, Bytes, BytesMut};
use phantom_core::{PhantomError, Result};

use phantom_core::constants::PROTOCOL_VERSION;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct FrameFlags: u8 {
        const SYN  = 0x01;
        const FIN  = 0x02;
        const RST  = 0x04;
        const ACK  = 0x08;
        const DATA = 0x10;
        const PING = 0x20;
        const PONG = 0x40;
        const UDP  = 0x80;
    }
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub version: u8,
    pub stream_id: u32,
    pub flags: FrameFlags,
    pub payload: Bytes,
}

impl Frame {
    pub fn syn(stream_id: u32, payload: impl Into<Bytes>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            stream_id,
            flags: FrameFlags::SYN | FrameFlags::DATA,
            payload: payload.into(),
        }
    }

    pub fn ack(stream_id: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            stream_id,
            flags: FrameFlags::ACK,
            payload: Bytes::new(),
        }
    }

    pub fn data(stream_id: u32, payload: impl Into<Bytes>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            stream_id,
            flags: FrameFlags::DATA,
            payload: payload.into(),
        }
    }

    pub fn fin(stream_id: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            stream_id,
            flags: FrameFlags::FIN,
            payload: Bytes::new(),
        }
    }

    pub fn rst(stream_id: u32) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            stream_id,
            flags: FrameFlags::RST,
            payload: Bytes::new(),
        }
    }

    pub fn ping() -> Self {
        Self {
            version: PROTOCOL_VERSION,
            stream_id: 0,
            flags: FrameFlags::PING,
            payload: Bytes::new(),
        }
    }

    pub fn pong() -> Self {
        Self {
            version: PROTOCOL_VERSION,
            stream_id: 0,
            flags: FrameFlags::PONG,
            payload: Bytes::new(),
        }
    }

    /// Encode frame to bytes.
    /// Wire format: [ver:1][stream_id:4 BE][flags:1][payload_len:2 BE][payload]
    pub fn encode(&self) -> Bytes {
        let payload_len = self.payload.len();
        assert!(payload_len <= phantom_core::constants::MAX_FRAME_PAYLOAD);

        let mut buf = BytesMut::with_capacity(phantom_core::constants::FRAME_HEADER_SIZE + payload_len);
        buf.put_u8(self.version);
        buf.put_u32(self.stream_id);
        buf.put_u8(self.flags.bits());
        buf.put_u16(payload_len as u16);
        buf.extend_from_slice(&self.payload);
        buf.freeze()
    }

    /// Decode frame from Bytes, using zero-copy slice for the payload.
    /// Wire format: [ver:1][stream_id:4 BE][flags:1][payload_len:2 BE][payload]
    pub fn decode(data: Bytes) -> Result<Self> {
        if data.len() < phantom_core::constants::FRAME_HEADER_SIZE {
            return Err(PhantomError::Protocol("Frame too short".to_string()));
        }

        let version = data[0];
        if version != PROTOCOL_VERSION {
            return Err(PhantomError::Protocol(format!(
                "Unsupported version: {}",
                version
            )));
        }

        let stream_id = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
        let flags = FrameFlags::from_bits_truncate(data[5]);
        let payload_len = u16::from_be_bytes([data[6], data[7]]) as usize;

        if data.len() < phantom_core::constants::FRAME_HEADER_SIZE + payload_len {
            return Err(PhantomError::Protocol("Frame payload truncated".to_string()));
        }

        // Zero-copy: slice the original Bytes (increments reference count only)
        let payload = data.slice(phantom_core::constants::FRAME_HEADER_SIZE..phantom_core::constants::FRAME_HEADER_SIZE + payload_len);

        Ok(Self {
            version,
            stream_id,
            flags,
            payload,
        })
    }

    /// Decode frame from a byte slice (convenience wrapper, copies payload).
    pub fn decode_from_slice(src: &[u8]) -> Result<Self> {
        Self::decode(Bytes::copy_from_slice(src))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_encode_decode_roundtrip() {
        let frame = Frame::syn(1, Bytes::from(&b"hello world"[..]));
        let encoded = frame.encode();
        let decoded = Frame::decode(encoded).unwrap();
        assert_eq!(decoded.stream_id, 1);
        assert!(decoded.flags.contains(FrameFlags::SYN));
        assert!(decoded.flags.contains(FrameFlags::DATA));
        assert_eq!(&decoded.payload[..], b"hello world");
    }

    #[test]
    fn data_frame_roundtrip() {
        let frame = Frame::data(42, vec![0u8; 100]);
        let encoded = frame.encode();
        let decoded = Frame::decode(encoded).unwrap();
        assert_eq!(decoded.stream_id, 42);
        assert!(decoded.flags.contains(FrameFlags::DATA));
        assert_eq!(decoded.payload.len(), 100);
    }

    #[test]
    fn ping_pong_frames() {
        let ping = Frame::ping();
        let encoded = ping.encode();
        let decoded = Frame::decode(encoded).unwrap();
        assert_eq!(decoded.stream_id, 0);
        assert!(decoded.flags.contains(FrameFlags::PING));

        let pong = Frame::pong();
        let encoded = pong.encode();
        let decoded = Frame::decode(encoded).unwrap();
        assert!(decoded.flags.contains(FrameFlags::PONG));
    }

    use std::assert_matches;

    #[test]
    fn decode_too_short() {
        let result = Frame::decode(Bytes::copy_from_slice(&[0u8; 4]));
        assert_matches!(result, Err(_));
    }
}
