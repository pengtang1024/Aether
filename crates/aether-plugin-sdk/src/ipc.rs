//! Length-prefixed MessagePack framing.
//!
//! Frame layout: `u32 big-endian payload length` followed by a MessagePack
//! payload. The maximum frame size is 8 MiB; file bytes never travel over IPC.

use std::io::{ErrorKind, Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;
use thiserror::Error;

/// Maximum accepted frame payload size (8 MiB).
pub const MAX_FRAME_SIZE: u32 = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("messagepack encode error: {0}")]
    Encode(#[from] rmp_serde::encode::Error),

    #[error("messagepack decode error: {0}")]
    Decode(#[from] rmp_serde::decode::Error),

    #[error("empty frame")]
    EmptyFrame,

    #[error("frame too large: {size} bytes (max {max})")]
    FrameTooLarge { size: u32, max: u32 },

    #[error("truncated frame payload: expected {expected} bytes")]
    TruncatedPayload { expected: usize },
}

/// Encode a value as one complete frame (length prefix included).
pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, IpcError> {
    let payload = rmp_serde::to_vec_named(value)?;
    let len = u32::try_from(payload.len()).map_err(|_| IpcError::FrameTooLarge {
        size: u32::MAX,
        max: MAX_FRAME_SIZE,
    })?;
    if len > MAX_FRAME_SIZE {
        return Err(IpcError::FrameTooLarge {
            size: len,
            max: MAX_FRAME_SIZE,
        });
    }

    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

/// Decode one complete frame (length prefix included).
pub fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, IpcError> {
    if frame.len() < 4 {
        return Err(IpcError::TruncatedPayload {
            expected: 4 - frame.len(),
        });
    }

    let len = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]);
    if len == 0 {
        return Err(IpcError::EmptyFrame);
    }
    if len > MAX_FRAME_SIZE {
        return Err(IpcError::FrameTooLarge {
            size: len,
            max: MAX_FRAME_SIZE,
        });
    }

    let payload = &frame[4..];
    if payload.len() != len as usize {
        return Err(IpcError::TruncatedPayload {
            expected: len as usize,
        });
    }

    Ok(rmp_serde::from_slice(payload)?)
}

/// Write one frame and flush it.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), IpcError> {
    let frame = encode_frame(value)?;
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Read one frame.
///
/// Returns `Ok(None)` on a clean EOF before the length prefix. Any partial
/// header/payload is an error.
pub fn read_frame<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<Option<T>, IpcError> {
    let mut header = [0u8; 4];
    if reader.read(&mut header[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..])?;

    let len = u32::from_be_bytes(header);
    if len == 0 {
        return Err(IpcError::EmptyFrame);
    }
    if len > MAX_FRAME_SIZE {
        return Err(IpcError::FrameTooLarge {
            size: len,
            max: MAX_FRAME_SIZE,
        });
    }

    let mut payload = vec![0u8; len as usize];
    reader.read_exact(&mut payload).map_err(|error| {
        if error.kind() == ErrorKind::UnexpectedEof {
            IpcError::TruncatedPayload {
                expected: len as usize,
            }
        } else {
            IpcError::Io(error)
        }
    })?;

    Ok(Some(rmp_serde::from_slice(&payload)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{PluginEvent, API_VERSION};

    #[test]
    fn frame_roundtrip() {
        let event = PluginEvent::Ready { api: API_VERSION };
        let frame = encode_frame(&event).unwrap();
        let decoded: PluginEvent = decode_frame(&frame).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn write_and_read_stream() {
        let event = PluginEvent::Heartbeat { seq: 1, unix_ms: 2 };
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &event).unwrap();
        let mut cursor = std::io::Cursor::new(buffer);
        let decoded: Option<PluginEvent> = read_frame(&mut cursor).unwrap();
        assert_eq!(decoded, Some(event));
        let eof: Option<PluginEvent> = read_frame(&mut cursor).unwrap();
        assert_eq!(eof, None);
    }

    #[test]
    fn rejects_empty_frame() {
        let frame = [0u8; 4];
        let err = decode_frame::<PluginEvent>(&frame).unwrap_err();
        assert!(matches!(err, IpcError::EmptyFrame));
    }

    #[test]
    fn rejects_oversized_frame_before_allocating() {
        let mut frame = Vec::from((MAX_FRAME_SIZE + 1).to_be_bytes());
        frame.extend_from_slice(&[0; 8]);
        let err = decode_frame::<PluginEvent>(&frame).unwrap_err();
        assert!(matches!(err, IpcError::FrameTooLarge { .. }));
    }

    #[test]
    fn rejects_truncated_payload() {
        let mut frame = Vec::from(10u32.to_be_bytes());
        frame.extend_from_slice(&[0xC0, 0xC0]);
        let err = decode_frame::<PluginEvent>(&frame).unwrap_err();
        assert!(matches!(err, IpcError::TruncatedPayload { expected: 10 }));
    }

    #[test]
    fn rejects_truncated_header() {
        let mut cursor = std::io::Cursor::new(vec![0u8, 0u8]);
        let err = read_frame::<_, PluginEvent>(&mut cursor).unwrap_err();
        assert!(matches!(err, IpcError::Io(_)));
    }

    /// Fuzz-shaped inputs must surface as errors, never panics: garbage
    /// MessagePack, an unknown event tag, and a payload that disagrees with
    /// the header length (both shorter and longer).
    #[test]
    fn malformed_payloads_are_errors_not_panics() {
        let garbage: Vec<u8> = vec![0x00, 0x00, 0x00, 0x04, 0xde, 0xad, 0xbe, 0xef];
        assert!(decode_frame::<PluginEvent>(&garbage).is_err());

        let mut unknown = Vec::from(3u32.to_be_bytes());
        unknown.extend_from_slice(&[0x82, 0xa4, b't', b'y', b'p', b'e', 0xa7, b'u', b'n', b'k']);
        assert!(decode_frame::<PluginEvent>(&unknown).is_err());

        let mut short = Vec::from(5u32.to_be_bytes());
        short.extend_from_slice(&[0xc0]);
        assert!(matches!(
            decode_frame::<PluginEvent>(&short),
            Err(IpcError::TruncatedPayload { expected: 5 })
        ));

        let mut long = Vec::from(1u32.to_be_bytes());
        long.extend_from_slice(&[0xc0, 0xc0, 0xc0]);
        assert!(decode_frame::<PluginEvent>(&long).is_err());
    }

    #[test]
    fn oversized_write_is_refused_before_bytes_hit_the_pipe() {
        let huge = vec![0u8; MAX_FRAME_SIZE as usize + 1];
        let mut sink = Vec::new();
        let err = write_frame(&mut sink, &huge).unwrap_err();
        assert!(matches!(err, IpcError::FrameTooLarge { .. }));
        assert!(
            sink.is_empty(),
            "nothing may be written for a rejected frame"
        );
    }
}
