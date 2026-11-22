//! Linux hardware video codec interface (VA-API / Vulkan Video, DMA-BUF zero-copy path).
//!
//! Currently returns no hardware MFTs so Linux CI and desktops automatically fall back to
//! `openh264` until native libva / Vulkan Video bindings are enabled.

use veloxdesk_core_traits::video::{DecoderCaps, DecoderConfig, EncoderCaps, EncoderConfig, VideoDecoder, VideoEncoder};
use veloxdesk_core_traits::{CoreError, Result};

pub fn probe_encoders() -> Vec<EncoderCaps> {
    Vec::new()
}

pub fn probe_decoders() -> Vec<DecoderCaps> {
    Vec::new()
}

pub fn create_encoder(_cfg: &EncoderConfig) -> Result<Box<dyn VideoEncoder>> {
    Err(CoreError::unsupported("Linux VA-API/Vulkan Video hardware encoder not yet implemented"))
}

pub fn create_decoder(_cfg: &DecoderConfig) -> Result<Box<dyn VideoDecoder>> {
    Err(CoreError::unsupported("Linux VA-API/Vulkan Video hardware decoder not yet implemented"))
}
