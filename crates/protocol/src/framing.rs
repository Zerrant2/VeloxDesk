//! Stream framing: `u32 LE length | protobuf message`, plus the one-byte [`StreamKind`]
//! preamble written when a stream is opened.

use bytes::{Buf, BufMut, Bytes, BytesMut};
use prost::Message;

use crate::{ProtocolError, Result, StreamKind};

/// Maximum size of a single framed message (16 MiB).
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Encode `msg` as a length-prefixed frame appended to `out`.
pub fn encode_frame<M: Message>(msg: &M, out: &mut BytesMut) -> Result<()> {
    let len = msg.encoded_len();
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::TooLarge(len));
    }
    out.reserve(4 + len);
    out.put_u32_le(len as u32);
    msg.encode(out)?;
    Ok(())
}

/// Encode `msg` into a standalone frame.
pub fn frame_to_bytes<M: Message>(msg: &M) -> Result<Bytes> {
    let mut b = BytesMut::new();
    encode_frame(msg, &mut b)?;
    Ok(b.freeze())
}

/// Try to decode one frame from the front of `buf`. Returns `Ok(None)` if more bytes
/// are needed; on success the frame is consumed from `buf`.
pub fn decode_frame<M: Message + Default>(buf: &mut BytesMut) -> Result<Option<M>> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::TooLarge(len));
    }
    if buf.len() < 4 + len {
        buf.reserve(4 + len - buf.len());
        return Ok(None);
    }
    buf.advance(4);
    let body = buf.split_to(len);
    Ok(Some(M::decode(body)?))
}

/// The preamble byte for a newly opened stream.
#[inline]
pub fn stream_preamble(kind: StreamKind) -> [u8; 1] {
    [kind as u8]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::{envelope::Msg, Envelope, Ping};

    #[test]
    fn frame_roundtrip_partial() {
        let m = Envelope { msg: Some(Msg::Ping(Ping { seq: 42, sent_us: 1_000 })) };
        let bytes = frame_to_bytes(&m).unwrap();
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&bytes[..3]);
        assert!(decode_frame::<Envelope>(&mut buf).unwrap().is_none());
        buf.extend_from_slice(&bytes[3..]);
        buf.extend_from_slice(&bytes);
        assert_eq!(decode_frame::<Envelope>(&mut buf).unwrap(), Some(m.clone()));
        assert_eq!(decode_frame::<Envelope>(&mut buf).unwrap(), Some(m));
        assert!(buf.is_empty());
    }
}
