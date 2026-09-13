//! The TCP wire format of `inu-r132 serve`.
//!
//! Client to server: plain UTF-8 text, one command per line (`\n`).
//!
//! Server to client: a sequence of length prefixed envelopes so text replies
//! and binary frames can be interleaved on one connection:
//!
//! ```text
//! u32 be  length of the rest of the envelope (type + payload)
//! u8      type: 0 = text, 1 = frame, 2 = bye
//! ...     payload
//! ```
//!
//! A text payload is a UTF-8 line without the trailing newline. A frame
//! payload starts with a 10 byte header:
//!
//! ```text
//! u32 be  width
//! u32 be  height
//! u8      codec (1 = JPEG)
//! u8      flags (reserved, 0)
//! ...     codec payload
//! ```

use std::io::{self, Read};

use tokio::io::{AsyncWrite, AsyncWriteExt};

pub const TYPE_TEXT: u8 = 0;
pub const TYPE_FRAME: u8 = 1;
pub const TYPE_BYE: u8 = 2;

pub const CODEC_JPEG: u8 = 1;
/// Raw little-endian `u16` samples, one per pixel (depth in mm / disparity).
pub const CODEC_Z16: u8 = 2;

pub const FRAME_HEADER_LEN: usize = 10;

/// Largest envelope we accept, a guard against a hostile or broken peer.
pub const MAX_MESSAGE: usize = 128 * 1024 * 1024;

/// The default TCP port of the camera protocol.
pub const DEFAULT_PORT: u16 = 5534;

pub fn encode_text(text: &str) -> Vec<u8> {
    encode_message(TYPE_TEXT, text.as_bytes())
}

pub fn encode_bye(text: &str) -> Vec<u8> {
    encode_message(TYPE_BYE, text.as_bytes())
}

pub fn encode_frame(width: usize, height: usize, codec: u8, data: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(FRAME_HEADER_LEN + data.len());
    payload.extend_from_slice(&(width as u32).to_be_bytes());
    payload.extend_from_slice(&(height as u32).to_be_bytes());
    payload.push(codec);
    payload.push(0);
    payload.extend_from_slice(data);
    encode_message(TYPE_FRAME, &payload)
}

pub fn encode_message(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.extend_from_slice(&((1 + payload.len()) as u32).to_be_bytes());
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

/// The header of a frame envelope.
#[derive(Debug, Clone, Copy)]
pub struct FrameHeader {
    pub width: u32,
    pub height: u32,
    pub codec: u8,
}

pub fn parse_frame(payload: &[u8]) -> Option<(FrameHeader, &[u8])> {
    if payload.len() < FRAME_HEADER_LEN {
        return None;
    }
    let width = u32::from_be_bytes(payload[0..4].try_into().ok()?);
    let height = u32::from_be_bytes(payload[4..8].try_into().ok()?);
    let codec = payload[8];
    Some((
        FrameHeader {
            width,
            height,
            codec,
        },
        &payload[FRAME_HEADER_LEN..],
    ))
}

/// Write one already-encoded envelope.
pub async fn write_message<W>(writer: &mut W, message: &[u8]) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(message).await
}

/// Blocking variant used by the remote display client.
pub fn read_message_blocking<R: Read>(reader: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let mut len = [0u8; 4];
    reader.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > MAX_MESSAGE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid envelope length {len}"),
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    let kind = buf[0];
    buf.remove(0);
    Ok((kind, buf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let data = vec![1u8, 2, 3, 4, 5];
        let encoded = encode_frame(640, 480, CODEC_JPEG, &data);
        assert_eq!(&encoded[0..4], &((1 + FRAME_HEADER_LEN + data.len()) as u32).to_be_bytes());
        assert_eq!(encoded[4], TYPE_FRAME);
        let (header, payload) = parse_frame(&encoded[5..]).unwrap();
        assert_eq!(header.width, 640);
        assert_eq!(header.height, 480);
        assert_eq!(header.codec, CODEC_JPEG);
        assert_eq!(payload, &data[..]);
    }

    #[test]
    fn text_roundtrip() {
        let encoded = encode_text("ok subscribed");
        assert_eq!(&encoded[0..4], &(1 + "ok subscribed".len() as u32).to_be_bytes());
        assert_eq!(encoded[4], TYPE_TEXT);
        assert_eq!(&encoded[5..], b"ok subscribed");
    }
}
