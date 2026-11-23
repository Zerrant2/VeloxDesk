//! Codec bitstream utilities for zero re-encode Matroska muxing:
//! - Annex-B NALU splitting & 4-byte length-prefixed AVCC/HVCC frame conversion
//! - H.264 `AVCDecoderConfigurationRecord` (`avcC`) builder from Annex-B SPS/PPS
//! - H.265 `HEVCDecoderConfigurationRecord` (`hvcC`) builder from Annex-B VPS/SPS/PPS
//! - AV1 `AV1CodecConfigurationRecord` (`av1C`) builder from OBU packets
//! - Opus 19-byte `OpusHead` (`A_OPUS`) `CodecPrivate` builder

use veloxdesk_core_traits::video::VideoCodec;

/// Matroska `CodecID` string for a supported [`VideoCodec`].
pub fn video_codec_id(codec: VideoCodec) -> Option<&'static str> {
    match codec {
        VideoCodec::H264 => Some("V_MPEG4/ISO/AVC"),
        VideoCodec::H265 => Some("V_MPEGH/ISO/HEVC"),
        VideoCodec::Av1 => Some("V_AV1"),
        VideoCodec::Vp9 => Some("V_VP9"),
        VideoCodec::Unspecified => None,
    }
}

/// Split an Annex-B bitstream (`00 00 01` or `00 00 00 01` start codes) into raw NALU slices.
///
/// If no Annex-B start codes are found, attempts to parse 4-byte big-endian length-prefixed
/// NALUs; if that doesn't match, returns the entire non-empty buffer as a single NALU.
pub fn split_annex_b_nalus(data: &[u8]) -> Vec<&[u8]> {
    let mut starts: Vec<(usize, usize)> = Vec::new(); // (code_start, payload_start)
    let mut i = 0usize;
    while i + 2 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 {
            if data[i + 2] == 1 {
                starts.push((i, i + 3));
                i += 3;
                continue;
            } else if i + 3 < data.len() && data[i + 2] == 0 && data[i + 3] == 1 {
                starts.push((i, i + 4));
                i += 4;
                continue;
            }
        }
        i += 1;
    }

    if !starts.is_empty() {
        let mut nalus = Vec::with_capacity(starts.len());
        for idx in 0..starts.len() {
            let payload_start = starts[idx].1;
            let mut payload_end = if idx + 1 < starts.len() { starts[idx + 1].0 } else { data.len() };
            // Trim trailing zero padding bytes before the next start code or end of AU.
            while payload_end > payload_start && data[payload_end - 1] == 0 {
                payload_end -= 1;
            }
            if payload_end > payload_start {
                nalus.push(&data[payload_start..payload_end]);
            }
        }
        return nalus;
    }

    // Fallback: check if input is already 4-byte big-endian length-prefixed.
    let mut lp_nalus = Vec::new();
    let mut pos = 0usize;
    let mut valid_lp = !data.is_empty();
    while pos < data.len() {
        if pos + 4 > data.len() {
            valid_lp = false;
            break;
        }
        let len = u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4;
        if len == 0 || pos + len > data.len() {
            valid_lp = false;
            break;
        }
        lp_nalus.push(&data[pos..pos + len]);
        pos += len;
    }
    if valid_lp && !lp_nalus.is_empty() {
        return lp_nalus;
    }

    if data.is_empty() { Vec::new() } else { vec![data] }
}

/// Convert an Annex-B H.264/H.265 access unit into 4-byte big-endian length-prefixed
/// AVCC/HVCC format for Matroska `SimpleBlock` payloads.
pub fn annex_b_to_length_prefixed(data: &[u8], out: &mut Vec<u8>) {
    out.clear();
    let nalus = split_annex_b_nalus(data);
    out.reserve(data.len() + nalus.len() * 4);
    for nalu in nalus {
        if nalu.is_empty() {
            continue;
        }
        out.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
        out.extend_from_slice(nalu);
    }
}

/// Build an ISO/IEC 14496-15 `AVCDecoderConfigurationRecord` (`CodecPrivate` for `V_MPEG4/ISO/AVC`)
/// by extracting SPS (type 7) and PPS (type 8) NALUs from an Annex-B H.264 keyframe.
pub fn build_avcc_from_annex_b(keyframe: &[u8]) -> Vec<u8> {
    let nalus = split_annex_b_nalus(keyframe);
    let mut sps_list: Vec<&[u8]> = Vec::new();
    let mut pps_list: Vec<&[u8]> = Vec::new();

    for nalu in nalus {
        if nalu.is_empty() {
            continue;
        }
        match nalu[0] & 0x1F {
            7 => sps_list.push(nalu),
            8 => pps_list.push(nalu),
            _ => {}
        }
    }

    let (profile_idc, profile_compat, level_idc) = sps_list
        .first()
        .filter(|s| s.len() >= 4)
        .map(|s| (s[1], s[2], s[3]))
        .unwrap_or((0x42, 0x00, 0x1F)); // Baseline L3.1 fallback

    let mut avcc = Vec::with_capacity(16 + sps_list.iter().map(|s| s.len()).sum::<usize>());
    avcc.push(0x01); // configurationVersion = 1
    avcc.push(profile_idc);
    avcc.push(profile_compat);
    avcc.push(level_idc);
    avcc.push(0xFF); // 6 reserved 1-bits + lengthSizeMinusOne = 3 (4-byte lengths)

    let sps_count = sps_list.len().min(31);
    avcc.push(0xE0 | (sps_count as u8));
    for sps in sps_list.iter().take(sps_count) {
        avcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(sps);
    }

    let pps_count = pps_list.len().min(255);
    avcc.push(pps_count as u8);
    for pps in pps_list.iter().take(pps_count) {
        avcc.extend_from_slice(&(pps.len() as u16).to_be_bytes());
        avcc.extend_from_slice(pps);
    }

    avcc
}

/// Strip Annex-B emulation-prevention bytes (`00 00 03` -> `00 00`) into RBSP.
fn strip_epb(nalu_payload: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(nalu_payload.len());
    let mut zeros = 0u8;
    for &b in nalu_payload {
        if zeros == 2 && b == 0x03 {
            zeros = 0;
            continue;
        }
        rbsp.push(b);
        if b == 0 {
            zeros = zeros.saturating_add(1);
        } else {
            zeros = 0;
        }
    }
    rbsp
}

/// Build an ISO/IEC 14496-15 `HEVCDecoderConfigurationRecord` (`CodecPrivate` for `V_MPEGH/ISO/HEVC`)
/// by extracting VPS (32), SPS (33), and PPS (34) NALUs from an Annex-B HEVC keyframe.
pub fn build_hvcc_from_annex_b(keyframe: &[u8]) -> Vec<u8> {
    let nalus = split_annex_b_nalus(keyframe);
    let mut vps_list: Vec<&[u8]> = Vec::new();
    let mut sps_list: Vec<&[u8]> = Vec::new();
    let mut pps_list: Vec<&[u8]> = Vec::new();

    for nalu in nalus {
        if nalu.len() < 2 {
            continue;
        }
        let nalu_type = (nalu[0] >> 1) & 0x3F;
        match nalu_type {
            32 => vps_list.push(nalu),
            33 => sps_list.push(nalu),
            34 => pps_list.push(nalu),
            _ => {}
        }
    }

    // Defaults: Main profile, Level 4.0, 4:2:0 8-bit.
    let mut ptl_profile_byte: u8 = 0x01;
    let mut ptl_compat_flags: [u8; 4] = [0x60, 0x00, 0x00, 0x00];
    let mut ptl_constraint_flags: [u8; 6] = [0x90, 0x00, 0x00, 0x00, 0x00, 0x00];
    let mut ptl_level_idc: u8 = 120;

    // Parse profile_tier_level(1, max_sub_layers_minus1) from SPS RBSP if long enough:
    // After 2-byte NALU header, byte 0 is vps_id(4)|max_sub_layers(3)|temporal_nesting(1),
    // followed immediately by 12 bytes of general_profile_tier_level.
    if let Some(sps) = sps_list.first()
        && sps.len() > 2
    {
        let rbsp = strip_epb(&sps[2..]);
        if rbsp.len() >= 13 {
            ptl_profile_byte = rbsp[1];
            ptl_compat_flags.copy_from_slice(&rbsp[2..6]);
            ptl_constraint_flags.copy_from_slice(&rbsp[6..12]);
            ptl_level_idc = rbsp[12];
        }
    }

    let arrays: [(u8, &[&[u8]]); 3] = [(32, &vps_list), (33, &sps_list), (34, &pps_list)];
    let num_arrays = arrays.iter().filter(|(_, list)| !list.is_empty()).count() as u8;

    let mut hvcc = Vec::with_capacity(64);
    hvcc.push(0x01); // configurationVersion = 1
    hvcc.push(ptl_profile_byte);
    hvcc.extend_from_slice(&ptl_compat_flags);
    hvcc.extend_from_slice(&ptl_constraint_flags);
    hvcc.push(ptl_level_idc);
    hvcc.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc = 0
    hvcc.push(0xFC); // parallelismType = 0
    hvcc.push(0xFD); // chromaFormat = 1 (4:2:0)
    hvcc.push(0xF8); // bitDepthLumaMinus8 = 0 (8-bit)
    hvcc.push(0xF8); // bitDepthChromaMinus8 = 0 (8-bit)
    hvcc.extend_from_slice(&[0x00, 0x00]); // avgFrameRate = 0 (unspecified)
    hvcc.push(0x0F); // constantFrameRate=0 | numTemporalLayers=1 | temporalIdNested=1 | lengthSizeMinusOne=3 (4 bytes)
    hvcc.push(num_arrays);

    for (nal_type, list) in arrays {
        if list.is_empty() {
            continue;
        }
        hvcc.push(0x80 | (nal_type & 0x3F)); // array_completeness = 1
        hvcc.extend_from_slice(&(list.len() as u16).to_be_bytes());
        for nalu in list {
            hvcc.extend_from_slice(&(nalu.len() as u16).to_be_bytes());
            hvcc.extend_from_slice(nalu);
        }
    }

    hvcc
}

/// Extract a Sequence Header OBU (`obu_type == 1`) from an AV1 OBU frame if present.
fn find_av1_sequence_header_obu(data: &[u8]) -> Option<&[u8]> {
    let mut pos = 0usize;
    while pos < data.len() {
        let header = data[pos];
        // Forbidden bit (MSB) must be 0.
        if (header & 0x80) != 0 {
            return None;
        }
        let obu_type = (header >> 3) & 0x0F;
        let has_extension = (header & 0x04) != 0;
        let has_size_field = (header & 0x02) != 0;

        let obu_start = pos;
        pos += 1;
        if has_extension {
            if pos >= data.len() {
                return None;
            }
            pos += 1;
        }

        let payload_len = if has_size_field {
            // LEB128 unsigned integer.
            let mut size = 0usize;
            let mut shift = 0u32;
            loop {
                let b = *data.get(pos)?;
                pos += 1;
                size |= ((b & 0x7F) as usize) << shift;
                if (b & 0x80) == 0 {
                    break;
                }
                shift += 7;
                if shift >= 28 {
                    return None;
                }
            }
            size
        } else {
            data.len() - pos
        };

        let obu_end = pos.checked_add(payload_len)?;
        if obu_end > data.len() {
            return None;
        }
        if obu_type == 1 {
            return Some(&data[obu_start..obu_end]);
        }
        pos = obu_end;
    }
    None
}

/// Build an `AV1CodecConfigurationRecord` (`av1C`, `CodecPrivate` for `V_AV1`),
/// embedding the Sequence Header OBU as `configOBUs` when present in `keyframe`.
pub fn build_av1c_from_obu(keyframe: &[u8]) -> Vec<u8> {
    let mut av1c = vec![
        0x81, // marker(1)=1 | version(7)=1
        0x08, // seq_profile=0 (Main) | seq_level_idx_0=8 (4.0)
        0x0C, // seq_tier_0=0 | high_bitdepth=0 | twelve_bit=0 | monochrome=0 | chroma_subsampling_x=1 | y=1 | pos=0
        0x00, // reserved=0 | initial_presentation_delay_present=0
    ];

    if let Some(seq_obu) = find_av1_sequence_header_obu(keyframe) {
        // Parse seq_profile (top 3 bits of Sequence Header payload) if available.
        let hdr = seq_obu[0];
        let mut p = 1 + usize::from((hdr & 0x04) != 0);
        if (hdr & 0x02) != 0 {
            while p < seq_obu.len() && (seq_obu[p] & 0x80) != 0 {
                p += 1;
            }
            p += 1;
        }
        if let Some(&first_payload) = seq_obu.get(p) {
            let seq_profile = (first_payload >> 5) & 0x07;
            av1c[1] = (seq_profile << 5) | 0x08;
        }
        av1c.extend_from_slice(seq_obu);
    }

    av1c
}

/// Build the standard 19-byte RFC 7845 `OpusHead` (`CodecPrivate` for `A_OPUS`):
/// version 1, channel count, pre-skip 312 (6.5 ms @ 48 kHz), input sample rate,
/// output gain 0, channel mapping family 0.
pub fn build_opus_head(channels: u16, sample_rate: u32) -> [u8; 19] {
    let mut head = [0u8; 19];
    head[0..8].copy_from_slice(b"OpusHead");
    head[8] = 1; // version 1
    head[9] = channels.clamp(1, 255) as u8;
    head[10..12].copy_from_slice(&312u16.to_le_bytes()); // standard 312-sample pre-skip
    head[12..16].copy_from_slice(&sample_rate.to_le_bytes());
    head[16..18].copy_from_slice(&0i16.to_le_bytes()); // output gain = 0 dB
    head[18] = 0; // channel mapping family 0 (mono/stereo)
    head
}
