//! VeloxDesk Android Viewer core + safe JNI bridge (`libveloxdesk_android.so`).
//!
//! 100% protocol-compatible with VeloxDesk desktop host v0.3.0+.

pub mod clipboard;
pub mod jni_bridge;
pub mod session;
pub mod stats;
pub mod target;

use veloxdesk_protocol::control::{VideoCodec, VideoCodecCap};

/// Formatted build display string (e.g. `v0.3.0 (abc1234)`).
pub const BUILD_DISPLAY: &str = env!("VELOXDESK_BUILD_DISPLAY");

/// Shared protocol and bridge constants matching `BridgeModels.kt::BridgeConstants`.
pub mod constants {
    // SessionMode
    pub const MODE_DESKTOP: i32 = 0;
    pub const MODE_GAMING: i32 = 1;

    // VideoCodec
    pub const CODEC_UNSPECIFIED: i32 = 0;
    pub const CODEC_H264: i32 = 1;
    pub const CODEC_H265: i32 = 2;
    pub const CODEC_AV1: i32 = 3;
    pub const CODEC_VP9: i32 = 4;

    // PathKind
    pub const PATH_UNKNOWN: i32 = 0;
    pub const PATH_DIRECT: i32 = 1;
    pub const PATH_P2P_HOLE_PUNCH: i32 = 2;
    pub const PATH_RELAY: i32 = 3;

    // StatusCode emitted by Rust -> onStatus(statusCode, detail)
    pub const STATUS_CONNECTING: i32 = 0;
    pub const STATUS_WAITING_FOR_HOST: i32 = 1;
    pub const STATUS_AUTHENTICATING: i32 = 2;
    pub const STATUS_WAITING_FOR_ACCEPT: i32 = 3;
    pub const STATUS_CONNECTED: i32 = 4;
    pub const STATUS_RECONNECTING: i32 = 5;

    // CodecCapsMask passed to nativeConnect
    pub const CAP_H264_HW: u32 = 1 << 0;
    pub const CAP_H265_HW: u32 = 1 << 1;
    pub const CAP_AV1_HW: u32 = 1 << 2;
    pub const CAP_H264_SW: u32 = 1 << 3;
    pub const CAP_H265_SW: u32 = 1 << 4;
    pub const CAP_AV1_SW: u32 = 1 << 5;
}

/// Decode `codecCapsMask` (bits 0..5 from `BridgeConstants`) into protobuf [`VideoCodecCap`]s.
/// Falls back to software H.264 if `mask == 0` so `Hello` always advertises a decodable codec.
pub fn decode_codec_caps_mask(
    mask: u32,
    viewer_width: u32,
    viewer_height: u32,
    viewer_refresh_hz: u32,
) -> Vec<VideoCodecCap> {
    let max_width = viewer_width.max(3840);
    let max_height = viewer_height.max(2160);
    let max_fps = viewer_refresh_hz.max(120);

    let mut caps = Vec::with_capacity(3);
    let specs = [
        (VideoCodec::H264, constants::CAP_H264_HW, constants::CAP_H264_SW),
        (VideoCodec::H265, constants::CAP_H265_HW, constants::CAP_H265_SW),
        (VideoCodec::Av1, constants::CAP_AV1_HW, constants::CAP_AV1_SW),
    ];

    for (codec, hw_bit, sw_bit) in specs {
        let hw = (mask & hw_bit) != 0;
        let sw = (mask & sw_bit) != 0;
        if hw || sw {
            caps.push(VideoCodecCap {
                codec: codec as i32,
                hw_decode: hw,
                max_width,
                max_height,
                max_fps,
                yuv444: false,
                ten_bit: false,
            });
        }
    }

    if caps.is_empty() {
        caps.push(VideoCodecCap {
            codec: VideoCodec::H264 as i32,
            hw_decode: false,
            max_width,
            max_height,
            max_fps,
            yuv444: false,
            ten_bit: false,
        });
    }

    caps
}

/// Convert interleaved `f32` audio samples (`-1.0..=1.0`, clamped & NaN-safe) into `i16` PCM.
pub fn f32_to_pcm16(src: &[f32], dst: &mut Vec<i16>) {
    dst.clear();
    dst.reserve(src.len());
    for &sample in src {
        if !sample.is_finite() {
            dst.push(0);
        } else {
            let clamped = sample.clamp(-1.0, 1.0);
            let scaled = (clamped * 32767.0).round();
            dst.push(scaled.clamp(i16::MIN as f32, i16::MAX as f32) as i16);
        }
    }
}

/// Overwrite the backing bytes of a `String` with zeros using volatile writes before clearing.
pub(crate) fn zeroize_string(s: &mut String) {
    // SAFETY: writing all-zero bytes to a `String`'s buffer produces valid UTF-8 (`\0` bytes),
    // and we immediately clear the string afterward.
    unsafe {
        let bytes = s.as_bytes_mut();
        zeroize_bytes(bytes);
    }
    s.clear();
}

/// Overwrite a byte slice with zeros using volatile writes + compiler fence.
pub(crate) fn zeroize_bytes(bytes: &mut [u8]) {
    for b in bytes.iter_mut() {
        // SAFETY: `b` is a valid mutable reference to an initialized `u8`.
        unsafe {
            std::ptr::write_volatile(b, 0);
        }
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

/// Sensitive string wrapper whose backing buffer is zeroized on `Drop` and redacted in `Debug`.
#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(inner: String) -> Self {
        Self(inner)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for SecretString {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl From<&str> for SecretString {
    fn from(s: &str) -> Self {
        Self::new(s.to_owned())
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

impl Drop for SecretString {
    fn drop(&mut self) {
        zeroize_string(&mut self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_caps_mask_decodes_all_combinations() {
        // Empty mask falls back to H.264 SW
        let fallback = decode_codec_caps_mask(0, 1920, 1080, 60);
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].codec, VideoCodec::H264 as i32);
        assert!(!fallback[0].hw_decode);

        // H.264 HW + H.265 SW + AV1 HW+SW
        let mask = constants::CAP_H264_HW | constants::CAP_H265_SW | constants::CAP_AV1_HW | constants::CAP_AV1_SW;
        let caps = decode_codec_caps_mask(mask, 2400, 1080, 144);
        assert_eq!(caps.len(), 3);
        assert_eq!(caps[0].codec, VideoCodec::H264 as i32);
        assert!(caps[0].hw_decode);
        assert_eq!(caps[0].max_fps, 144);
        assert_eq!(caps[1].codec, VideoCodec::H265 as i32);
        assert!(!caps[1].hw_decode);
        assert_eq!(caps[2].codec, VideoCodec::Av1 as i32);
        assert!(caps[2].hw_decode);
    }

    #[test]
    fn f32_to_pcm16_clamps_and_handles_non_finite() {
        let input = [0.0f32, 0.5, -0.5, 1.0, -1.0, 2.5, -3.0, f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
        let mut out = Vec::new();
        f32_to_pcm16(&input, &mut out);
        assert_eq!(out, vec![0, 16384, -16384, 32767, -32767, 32767, -32767, 0, 0, 0]);
    }

    #[test]
    fn zeroize_clears_secret() {
        let mut s = String::from("SuperSecret123!");
        zeroize_string(&mut s);
        assert!(s.is_empty());

        let mut key = [0xABu8; 32];
        zeroize_bytes(&mut key);
        assert_eq!(key, [0u8; 32]);

        let sec = SecretString::from("TopSecretPassword");
        assert_eq!(format!("{sec:?}"), "<redacted>");
        assert_eq!(sec.as_str(), "TopSecretPassword");
        drop(sec);
    }
}
