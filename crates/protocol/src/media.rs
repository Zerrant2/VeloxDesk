//! Media datagram header (ARCHITECTURE §4.2). 24 bytes, little-endian, followed by payload.

use crate::{ProtocolError, Result};

/// Current media header version (high nibble of byte 0).
pub const MEDIA_VERSION: u8 = 1;
/// Size of [`MediaHeader`] on the wire.
pub const MEDIA_HEADER_LEN: usize = 24;
/// Maximum Reed-Solomon shards (data + parity) in one FEC block (GF(2^8)).
pub const MAX_SHARDS_PER_BLOCK: usize = 255;

/// Kind of a media datagram (low nibble of byte 0).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaKind {
    Video = 1,
    Audio = 2,
    /// Bandwidth probe padding used by the ABR estimator.
    Probe = 3,
    /// Full gamepad state snapshot (idempotent, safe to lose).
    GamepadState = 4,
}

impl MediaKind {
    pub fn from_u8(v: u8) -> Result<Self> {
        Ok(match v {
            1 => Self::Video,
            2 => Self::Audio,
            3 => Self::Probe,
            4 => Self::GamepadState,
            other => return Err(ProtocolError::BadKind(other)),
        })
    }
}

/// Bit flags of byte 1.
pub mod flags {
    pub const KEYFRAME: u8 = 1 << 0;
    pub const PARITY: u8 = 1 << 1;
    pub const END_OF_FRAME: u8 = 1 << 2;
    pub const HDR: u8 = 1 << 3;
    /// Intra-refresh wave completed with this frame (decoder fully refreshed).
    pub const RECOVERY_POINT: u8 = 1 << 4;
}

/// Wire codec identifiers (byte 13). Stable: never renumber.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum WireCodec {
    #[default]
    Unknown = 0,
    H264 = 1,
    H265 = 2,
    Av1 = 3,
    Vp9 = 4,
    Opus = 16,
    Pcm = 17,
}

impl WireCodec {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::H264,
            2 => Self::H265,
            3 => Self::Av1,
            4 => Self::Vp9,
            16 => Self::Opus,
            17 => Self::Pcm,
            _ => Self::Unknown,
        }
    }
}

/// Parsed media datagram header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub kind: MediaKind,
    pub flags: u8,
    /// Display index (video) or audio track.
    pub stream_id: u8,
    /// FEC block index within the frame.
    pub block_index: u8,
    /// Frame id (video) or packet sequence (audio). Wraps.
    pub frame_id: u32,
    /// Shard index inside the block: `0..data_shards` = data, then parity.
    pub shard_index: u16,
    pub data_shards: u8,
    pub parity_shards: u8,
    /// Number of FEC blocks composing the frame.
    pub block_count: u8,
    pub codec: WireCodec,
    /// Payload length of every shard of this block (shards are zero-padded to it).
    pub shard_len: u16,
    /// Real (unpadded) number of data bytes in this block.
    pub block_len: u32,
    /// Low 32 bits of the host capture timestamp in microseconds.
    pub capture_ts: u32,
}

impl MediaHeader {
    #[inline]
    pub fn is_parity(&self) -> bool {
        self.flags & flags::PARITY != 0
    }
    #[inline]
    pub fn is_keyframe(&self) -> bool {
        self.flags & flags::KEYFRAME != 0
    }
    #[inline]
    pub fn is_end_of_frame(&self) -> bool {
        self.flags & flags::END_OF_FRAME != 0
    }

    /// Validate field invariants.
    pub fn validate(&self) -> Result<()> {
        let total = self.data_shards as usize + self.parity_shards as usize;
        if self.data_shards == 0 {
            return Err(ProtocolError::Invalid("data_shards == 0"));
        }
        if total > MAX_SHARDS_PER_BLOCK {
            return Err(ProtocolError::Invalid("data+parity > 255"));
        }
        if self.shard_index as usize >= total {
            return Err(ProtocolError::Invalid("shard_index out of range"));
        }
        if self.block_count == 0 || self.block_index >= self.block_count {
            return Err(ProtocolError::Invalid("block_index out of range"));
        }
        if self.block_len as usize > self.shard_len as usize * self.data_shards as usize {
            return Err(ProtocolError::Invalid("block_len exceeds shard capacity"));
        }
        Ok(())
    }

    /// Serialize into `out[..24]`.
    pub fn write(&self, out: &mut [u8]) -> Result<()> {
        if out.len() < MEDIA_HEADER_LEN {
            return Err(ProtocolError::Truncated { need: MEDIA_HEADER_LEN, have: out.len() });
        }
        out[0] = (MEDIA_VERSION << 4) | (self.kind as u8 & 0x0F);
        out[1] = self.flags;
        out[2] = self.stream_id;
        out[3] = self.block_index;
        out[4..8].copy_from_slice(&self.frame_id.to_le_bytes());
        out[8..10].copy_from_slice(&self.shard_index.to_le_bytes());
        out[10] = self.data_shards;
        out[11] = self.parity_shards;
        out[12] = self.block_count;
        out[13] = self.codec as u8;
        out[14..16].copy_from_slice(&self.shard_len.to_le_bytes());
        out[16..20].copy_from_slice(&self.block_len.to_le_bytes());
        out[20..24].copy_from_slice(&self.capture_ts.to_le_bytes());
        Ok(())
    }

    /// Serialize into a fresh array.
    pub fn to_bytes(&self) -> [u8; MEDIA_HEADER_LEN] {
        let mut b = [0u8; MEDIA_HEADER_LEN];
        // Cannot fail: buffer has exact size.
        let _ = self.write(&mut b);
        b
    }

    /// Parse a datagram, returning the header and the payload slice.
    pub fn parse(buf: &[u8]) -> Result<(Self, &[u8])> {
        if buf.len() < MEDIA_HEADER_LEN {
            return Err(ProtocolError::Truncated { need: MEDIA_HEADER_LEN, have: buf.len() });
        }
        let version = buf[0] >> 4;
        if version != MEDIA_VERSION {
            return Err(ProtocolError::BadVersion(version));
        }
        let h = Self {
            kind: MediaKind::from_u8(buf[0] & 0x0F)?,
            flags: buf[1],
            stream_id: buf[2],
            block_index: buf[3],
            frame_id: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            shard_index: u16::from_le_bytes([buf[8], buf[9]]),
            data_shards: buf[10],
            parity_shards: buf[11],
            block_count: buf[12],
            codec: WireCodec::from_u8(buf[13]),
            shard_len: u16::from_le_bytes([buf[14], buf[15]]),
            block_len: u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
            capture_ts: u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]),
        };
        Ok((h, &buf[MEDIA_HEADER_LEN..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> MediaHeader {
        MediaHeader {
            kind: MediaKind::Video,
            flags: flags::KEYFRAME | flags::END_OF_FRAME,
            stream_id: 1,
            block_index: 2,
            frame_id: 0xDEAD_BEEF,
            shard_index: 7,
            data_shards: 20,
            parity_shards: 4,
            block_count: 3,
            codec: WireCodec::H265,
            shard_len: 1150,
            block_len: 20 * 1150 - 13,
            capture_ts: 123_456_789,
        }
    }

    #[test]
    fn roundtrip() {
        let h = sample();
        h.validate().unwrap();
        let mut dgram = h.to_bytes().to_vec();
        dgram.extend_from_slice(b"payload");
        let (p, payload) = MediaHeader::parse(&dgram).unwrap();
        assert_eq!(p, h);
        assert_eq!(payload, b"payload");
        assert!(p.is_keyframe() && p.is_end_of_frame() && !p.is_parity());
    }

    #[test]
    fn rejects_bad_input() {
        assert!(MediaHeader::parse(&[0u8; 10]).is_err());
        let mut b = sample().to_bytes();
        b[0] = (2 << 4) | 1;
        assert!(matches!(MediaHeader::parse(&b), Err(ProtocolError::BadVersion(2))));
        let mut h = sample();
        h.shard_index = 24;
        assert!(h.validate().is_err());
    }
}
