//! macOS hardware video codec interface (VideoToolbox / `CVPixelBuffer` / `IOSurface`).

use veloxdesk_core_traits::video::{DecoderCaps, DecoderConfig, EncoderCaps, EncoderConfig, VideoDecoder, VideoEncoder};
use veloxdesk_core_traits::{CoreError, Result};

pub fn probe_encoders() -> Vec<EncoderCaps> {
    Vec::new()
}

pub fn probe_decoders() -> Vec<DecoderCaps> {
    Vec::new()
}

pub fn create_encoder(_cfg: &EncoderConfig) -> Result<Box<dyn VideoEncoder>> {
    Err(CoreError::unsupported("macOS VideoToolbox hardware encoder not yet implemented"))
}

pub fn create_decoder(_cfg: &DecoderConfig) -> Result<Box<dyn VideoDecoder>> {
    Err(CoreError::unsupported("macOS VideoToolbox hardware decoder not yet implemented"))
}
