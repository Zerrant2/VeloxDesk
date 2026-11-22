//! Windows hardware video encoding and decoding (D3D11 + Media Foundation).

pub mod d3d;
pub mod mf;
pub mod mf_dec;
pub mod mf_enc;

use std::collections::BTreeMap;

use veloxdesk_core_traits::video::{DecoderCaps, EncoderCaps, VideoCodec};

const PROBE_CODECS: [VideoCodec; 3] = [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1];

/// Probe available Windows Media Foundation hardware encoders grouped by vendor.
pub fn probe_encoders() -> Vec<EncoderCaps> {
    let mut by_vendor: BTreeMap<&'static str, (Vec<VideoCodec>, bool, bool)> = BTreeMap::new();
    for codec in PROBE_CODECS {
        if let Some(info) = mf_enc::probe(codec) {
            let entry = by_vendor.entry(info.vendor).or_insert_with(|| (Vec::new(), false, false));
            entry.0.push(codec);
            entry.1 |= info.ten_bit;
            entry.2 |= info.yuv444;
        }
    }
    by_vendor
        .into_iter()
        .map(|(vendor, (codecs, ten_bit, yuv444))| mf_enc::caps_for(vendor, codecs, ten_bit, yuv444))
        .collect()
}

/// Probe available Windows Media Foundation D3D11VA hardware decoders.
pub fn probe_decoders() -> Vec<DecoderCaps> {
    let mut codecs = Vec::new();
    let mut ten_bit = false;
    for codec in PROBE_CODECS {
        if let Some(info) = mf_dec::probe(codec) {
            codecs.push(codec);
            ten_bit |= info.ten_bit;
        }
    }
    if codecs.is_empty() {
        Vec::new()
    } else {
        vec![mf_dec::caps_for(codecs, ten_bit)]
    }
}
