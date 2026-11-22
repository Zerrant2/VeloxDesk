//! Minimal Annex-B bitstream inspection (start codes, NAL types).

use veloxdesk_core_traits::video::VideoCodec;

/// Iterate over NAL unit payloads (without start codes) of an Annex-B buffer.
pub fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|&s| {
            // strip the start code (and an optional leading zero of a 4-byte code)
            let mut e = s - 3;
            while e > 0 && data[e - 1] == 0 {
                e -= 1;
            }
            e
        })
        .chain(std::iter::once(data.len()))
        .collect();
    starts.into_iter().zip(ends).filter(|(s, e)| e > s).map(move |(s, e)| &data[s..e])
}

fn nal_type(codec: VideoCodec, nal: &[u8]) -> Option<u8> {
    let b = *nal.first()?;
    match codec {
        VideoCodec::H264 => Some(b & 0x1F),
        VideoCodec::H265 => Some((b >> 1) & 0x3F),
        _ => None,
    }
}

/// True if the access unit contains an IDR / IRAP picture.
pub fn is_keyframe(codec: VideoCodec, data: &[u8]) -> bool {
    nal_units(data).any(|n| match (codec, nal_type(codec, n)) {
        (VideoCodec::H264, Some(5)) => true,
        (VideoCodec::H265, Some(t)) => (16..=21).contains(&t),
        _ => false,
    })
}

/// True if the access unit carries a sequence parameter set.
pub fn has_sps(codec: VideoCodec, data: &[u8]) -> bool {
    nal_units(data).any(|n| matches!((codec, nal_type(codec, n)), (VideoCodec::H264, Some(7)) | (VideoCodec::H265, Some(33))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_h264() {
        let au = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9, 0];
        let n: Vec<_> = nal_units(&au).collect();
        assert_eq!(n.len(), 3);
        assert_eq!(n[0], &[0x67, 1, 2]);
        assert_eq!(n[1], &[0x68, 3]);
        assert!(is_keyframe(VideoCodec::H264, &au));
        assert!(has_sps(VideoCodec::H264, &au));
        assert!(!is_keyframe(VideoCodec::H264, &[0, 0, 1, 0x41, 5]));
    }

    #[test]
    fn parse_hevc() {
        let au = [0, 0, 1, 0x40, 1, 0, 0, 1, 0x26, 1, 7];
        assert!(has_sps(VideoCodec::H265, &[0, 0, 1, 0x42, 1]));
        assert!(is_keyframe(VideoCodec::H265, &au));
    }
}
