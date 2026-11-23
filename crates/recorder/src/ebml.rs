//! Pure-Rust EBML (RFC 8794) binary encoding and decoding primitives for Matroska (`.mkv`).

/// 8-byte EBML unknown-size VINT placeholder (`0x01FFFFFFFFFFFFFF`), used for
/// streaming Segment headers until finalized on `stop()`.
pub const UNKNOWN_SIZE_8: [u8; 8] = [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

/// Encode a 1-to-4-byte EBML Element ID in big-endian form (marker bit already embedded in `id`).
pub fn write_element_id(buf: &mut Vec<u8>, id: u32) {
    if id <= 0xFF {
        buf.push(id as u8);
    } else if id <= 0xFFFF {
        buf.extend_from_slice(&(id as u16).to_be_bytes());
    } else if id <= 0x00FF_FFFF {
        let b = id.to_be_bytes();
        buf.extend_from_slice(&b[1..4]);
    } else {
        buf.extend_from_slice(&id.to_be_bytes());
    }
}

/// Encode a VINT data size (1..=8 bytes), avoiding the all-1s reserved unknown-size pattern.
pub fn write_vint_size(buf: &mut Vec<u8>, size: u64) {
    // Maximum encodable known size per byte width is (1 << (7 * width)) - 2
    // because all-1s data bits indicate "unknown size" in EBML.
    if size < (1u64 << 7) - 1 {
        buf.push(0x80 | (size as u8));
    } else if size < (1u64 << 14) - 1 {
        let v = 0x4000u16 | (size as u16);
        buf.extend_from_slice(&v.to_be_bytes());
    } else if size < (1u64 << 21) - 1 {
        let v = 0x0020_0000u32 | (size as u32);
        let b = v.to_be_bytes();
        buf.extend_from_slice(&b[1..4]);
    } else if size < (1u64 << 28) - 1 {
        let v = 0x1000_0000u32 | (size as u32);
        buf.extend_from_slice(&v.to_be_bytes());
    } else if size < (1u64 << 35) - 1 {
        let v = 0x0000_0008_0000_0000u64 | size;
        let b = v.to_be_bytes();
        buf.extend_from_slice(&b[3..8]);
    } else if size < (1u64 << 42) - 1 {
        let v = 0x0000_0400_0000_0000u64 | size;
        let b = v.to_be_bytes();
        buf.extend_from_slice(&b[2..8]);
    } else if size < (1u64 << 49) - 1 {
        let v = 0x0002_0000_0000_0000u64 | size;
        let b = v.to_be_bytes();
        buf.extend_from_slice(&b[1..8]);
    } else {
        buf.extend_from_slice(&encode_vint_size_fixed8(size));
    }
}

/// Encode a known size as a fixed 8-byte EBML VINT (`0x01` marker + 56-bit value),
/// suitable for patching an `UNKNOWN_SIZE_8` placeholder in-place.
pub fn encode_vint_size_fixed8(size: u64) -> [u8; 8] {
    let clamped = size.min((1u64 << 56) - 2);
    (0x0100_0000_0000_0000u64 | clamped).to_be_bytes()
}

/// Write an unsigned integer EBML element using minimal big-endian byte width (1..=8 bytes).
pub fn write_uint_element(buf: &mut Vec<u8>, id: u32, val: u64) {
    let bytes = val.to_be_bytes();
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(7);
    let slice = &bytes[start..];
    write_element_id(buf, id);
    write_vint_size(buf, slice.len() as u64);
    buf.extend_from_slice(slice);
}

/// Write an 8-byte IEEE 754 `f64` EBML element.
pub fn write_f64_element(buf: &mut Vec<u8>, id: u32, val: f64) {
    write_element_id(buf, id);
    write_vint_size(buf, 8);
    buf.extend_from_slice(&val.to_be_bytes());
}

/// Write a UTF-8 / ASCII string EBML element.
pub fn write_str_element(buf: &mut Vec<u8>, id: u32, s: &str) {
    write_element_id(buf, id);
    write_vint_size(buf, s.len() as u64);
    buf.extend_from_slice(s.as_bytes());
}

/// Write a raw binary EBML element.
pub fn write_bytes_element(buf: &mut Vec<u8>, id: u32, data: &[u8]) {
    write_element_id(buf, id);
    write_vint_size(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

/// Write a master EBML element wrapping `body`.
pub fn write_master_element(buf: &mut Vec<u8>, id: u32, body: &[u8]) {
    write_element_id(buf, id);
    write_vint_size(buf, body.len() as u64);
    buf.extend_from_slice(body);
}

/// Read an EBML Element ID (1..=4 bytes, keeping the leading VINT marker bit).
pub fn read_element_id(data: &[u8], pos: &mut usize) -> Option<u32> {
    let first = *data.get(*pos)?;
    if first == 0 {
        return None;
    }
    let width = first.leading_zeros() as usize + 1;
    if width > 4 || *pos + width > data.len() {
        return None;
    }
    let mut id = 0u32;
    for i in 0..width {
        id = (id << 8) | (data[*pos + i] as u32);
    }
    *pos += width;
    Some(id)
}

/// Read an EBML VINT data size (1..=8 bytes).
/// Returns `Some(None)` if the size is the EBML "unknown size" all-1s sentinel,
/// `Some(Some(size))` for a concrete size, or `None` on malformed/truncated input.
pub fn read_vint_size(data: &[u8], pos: &mut usize) -> Option<Option<u64>> {
    let first = *data.get(*pos)?;
    if first == 0 {
        return None;
    }
    let width = first.leading_zeros() as usize + 1;
    if width > 8 || *pos + width > data.len() {
        return None;
    }
    let mask = if width == 8 { 0u8 } else { (1u8 << (8 - width)) - 1 };
    let mut val = (first & mask) as u64;
    for i in 1..width {
        val = (val << 8) | (data[*pos + i] as u64);
    }
    *pos += width;
    let all_ones = (1u64 << (7 * width)) - 1;
    if val == all_ones {
        Some(None)
    } else {
        Some(Some(val))
    }
}

/// Read a general EBML VINT value (e.g. TrackNumber inside SimpleBlock).
pub fn read_vint_value(data: &[u8], pos: &mut usize) -> Option<u64> {
    let first = *data.get(*pos)?;
    if first == 0 {
        return None;
    }
    let width = first.leading_zeros() as usize + 1;
    if width > 8 || *pos + width > data.len() {
        return None;
    }
    let mask = if width == 8 { 0u8 } else { (1u8 << (8 - width)) - 1 };
    let mut val = (first & mask) as u64;
    for i in 1..width {
        val = (val << 8) | (data[*pos + i] as u64);
    }
    *pos += width;
    Some(val)
}

/// Decode a big-endian EBML unsigned integer payload (0..=8 bytes).
pub fn read_uint(bytes: &[u8]) -> u64 {
    let mut v = 0u64;
    for &b in bytes.iter().take(8) {
        v = (v << 8) | (b as u64);
    }
    v
}
