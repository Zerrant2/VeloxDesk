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
    let mut by_vendor: BTreeMap<&'static str, Vec<VideoCodec>> = BTreeMap::new();
    for codec in PROBE_CODECS {
        if let Some(vendor) = mf_enc::probe(codec) {
            by_vendor.entry(vendor).or_default().push(codec);
        }
    }
    by_vendor.into_iter().map(|(vendor, codecs)| mf_enc::caps_for(vendor, codecs)).collect()
}

/// Probe available Windows Media Foundation D3D11VA hardware decoders.
pub fn probe_decoders() -> Vec<DecoderCaps> {
    let codecs: Vec<VideoCodec> = PROBE_CODECS.into_iter().filter(|&c| mf_dec::probe(c)).collect();
    if codecs.is_empty() {
        Vec::new()
    } else {
        vec![mf_dec::caps_for(codecs)]
    }
}
