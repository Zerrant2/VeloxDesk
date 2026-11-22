//! Pure-Rust low-latency Opus encoder & decoder backed by `rusty-opus` (RFC 6716 / RFC 8251).

use std::time::Duration;

use bytes::Bytes;
use rusty_opus::{Application, OpusDecoder as RawDecoder, OpusEncoder as RawEncoder};
use veloxdesk_core_traits::audio::{
    AudioCodecId, AudioDecoder, AudioEncoder, AudioEncoderConfig, AudioFormat, AudioFrame, EncodedAudio,
};
use veloxdesk_core_traits::{CoreError, Result, Timestamp};

const MAX_PACKET_BYTES: usize = 4000;
const VALID_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
const VALID_DURATIONS_US: [u32; 6] = [2_500, 5_000, 10_000, 20_000, 40_000, 60_000];

fn validate_format(fmt: AudioFormat) -> Result<()> {
    if !VALID_RATES.contains(&fmt.sample_rate) {
        return Err(CoreError::invalid(format!(
            "Opus requires sample rate in {:?}, got {}",
            VALID_RATES, fmt.sample_rate
        )));
    }
    if fmt.channels != 1 && fmt.channels != 2 {
        return Err(CoreError::invalid(format!("Opus requires 1 or 2 channels, got {}", fmt.channels)));
    }
    Ok(())
}

fn validate_frame_duration(dur: Duration) -> Result<u32> {
    let us = dur.as_micros() as u32;
    if !VALID_DURATIONS_US.contains(&us) {
        return Err(CoreError::invalid(format!(
            "Opus frame duration must be 2.5, 5, 10, 20, 40 or 60 ms (got {us} us)"
        )));
    }
    Ok(us)
}

fn samples_per_channel(sample_rate: u32, duration_us: u32) -> usize {
    ((sample_rate as u64 * duration_us as u64) / 1_000_000) as usize
}

/// Opus audio encoder implementing [`AudioEncoder`].
///
/// Supports 2.5 / 5 / 10 / 20 / 40 / 60 ms frames, `RESTRICTED_LOWDELAY` mode for gaming,
/// and internal FIFO framing so callers can feed arbitrary chunk sizes from OS capture APIs.
pub struct OpusEncoder {
    enc: RawEncoder,
    format: AudioFormat,
    frame_samples: usize,
    frame_duration_us: u32,
    fifo: Vec<f32>,
    fifo_head_ts: Option<Timestamp>,
    seq: u32,
    scratch: Vec<u8>,
}

impl OpusEncoder {
    pub fn new(cfg: &AudioEncoderConfig) -> Result<Self> {
        validate_format(cfg.format)?;
        let duration_us = validate_frame_duration(cfg.frame_duration)?;
        let frame_samples = samples_per_channel(cfg.format.sample_rate, duration_us);
        let app = if cfg.low_delay { Application::RestrictedLowDelay } else { Application::Audio };
        let mut enc = RawEncoder::new(cfg.format.sample_rate as i32, cfg.format.channels as usize, app)
            .map_err(|e| CoreError::platform(format!("opus encoder init: {e}")))?;
        let bps = cfg.bitrate_kbps.saturating_mul(1000).clamp(6_000, 510_000) as i32;
        enc.bitrate_bps = bps;
        enc.complexity = if cfg.low_delay { 5 } else { 8 };
        enc.packet_loss_perc = 5;
        Ok(Self {
            enc,
            format: cfg.format,
            frame_samples,
            frame_duration_us: duration_us,
            fifo: Vec::with_capacity(frame_samples * cfg.format.channels as usize * 4),
            fifo_head_ts: None,
            seq: 0,
            scratch: vec![0u8; MAX_PACKET_BYTES],
        })
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    pub fn frame_duration_us(&self) -> u32 {
        self.frame_duration_us
    }

    /// Dynamically adjust target bitrate (kbps) without resetting codec state.
    pub fn set_bitrate_kbps(&mut self, kbps: u32) {
        self.enc.bitrate_bps = kbps.saturating_mul(1000).clamp(6_000, 510_000) as i32;
    }

    /// Enable or disable constant bitrate (CBR).
    pub fn set_cbr(&mut self, cbr: bool) {
        self.enc.use_cbr = cbr;
    }
}

impl AudioEncoder for OpusEncoder {
    fn codec(&self) -> AudioCodecId {
        AudioCodecId::Opus
    }

    fn encode(&mut self, pcm: &AudioFrame) -> Result<Vec<EncodedAudio>> {
        if pcm.format != self.format {
            return Err(CoreError::invalid(format!(
                "OpusEncoder expected {:?}, got {:?}",
                self.format, pcm.format
            )));
        }
        let ch = self.format.channels as usize;
        if pcm.samples.len() % ch != 0 {
            return Err(CoreError::invalid("PCM sample count not divisible by channel count"));
        }
        if pcm.samples.is_empty() {
            return Ok(Vec::new());
        }

        // Compute the timestamp of the first sample currently in the FIFO.
        if self.fifo.is_empty() {
            self.fifo_head_ts = Some(pcm.capture_ts);
        } else if self.fifo_head_ts.is_none() {
            let buffered_frames = (self.fifo.len() / ch) as u64;
            let buffered_us = buffered_frames * 1_000_000 / self.format.sample_rate as u64;
            self.fifo_head_ts = Some(Timestamp(pcm.capture_ts.as_micros().saturating_sub(buffered_us)));
        }
        self.fifo.extend_from_slice(&pcm.samples);

        let chunk_len = self.frame_samples * ch;
        let mut packets = Vec::new();
        let mut consumed = 0usize;
        while self.fifo.len() - consumed >= chunk_len {
            let slice = &self.fifo[consumed..consumed + chunk_len];
            let n = self
                .enc
                .encode(slice, self.frame_samples, &mut self.scratch)
                .map_err(|e| CoreError::platform(format!("opus encode: {e}")))?;
            let ts = self.fifo_head_ts.unwrap_or(pcm.capture_ts);
            let seq = self.seq;
            self.seq = self.seq.wrapping_add(1);
            packets.push(EncodedAudio {
                data: Bytes::copy_from_slice(&self.scratch[..n]),
                codec: AudioCodecId::Opus,
                seq,
                capture_ts: ts,
                duration_us: self.frame_duration_us,
            });
            self.fifo_head_ts = Some(Timestamp(ts.as_micros().saturating_add(self.frame_duration_us as u64)));
            consumed += chunk_len;
        }
        if consumed > 0 {
            self.fifo.drain(..consumed);
            if self.fifo.is_empty() {
                self.fifo_head_ts = None;
            }
        }
        Ok(packets)
    }
}

/// Opus audio decoder implementing [`AudioDecoder`] with built-in Packet Loss Concealment (PLC).
pub struct OpusDecoder {
    dec: RawDecoder,
    format: AudioFormat,
    last_frame_samples: usize,
    last_ts: Timestamp,
    scratch: Vec<f32>,
}

impl OpusDecoder {
    /// Create a decoder expecting `format`; `default_frame_duration` is used if PLC is
    /// requested before the first valid packet arrives (typically 10 ms or 5 ms).
    pub fn new(format: AudioFormat, default_frame_duration: Duration) -> Result<Self> {
        validate_format(format)?;
        let dur_us = validate_frame_duration(default_frame_duration)?;
        let dec = RawDecoder::new(format.sample_rate as i32, format.channels as usize)
            .map_err(|e| CoreError::platform(format!("opus decoder init: {e}")))?;
        let max_samples = samples_per_channel(format.sample_rate, 60_000);
        let default_samples = samples_per_channel(format.sample_rate, dur_us);
        Ok(Self {
            dec,
            format,
            last_frame_samples: default_samples,
            last_ts: Timestamp(0),
            scratch: vec![0.0; max_samples * format.channels as usize],
        })
    }

    /// Convenience constructor using 10 ms default PLC frame duration.
    pub fn with_format(format: AudioFormat) -> Result<Self> {
        Self::new(format, Duration::from_millis(10))
    }

    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Decode in-band FEC (LBRR) carried by the *next* packet to recover a lost frame.
    pub fn decode_fec(&mut self, next_packet: &EncodedAudio) -> Result<AudioFrame> {
        let ch = self.format.channels as usize;
        let frame_samples = self.last_frame_samples;
        let n = self
            .dec
            .decode_fec(&next_packet.data, frame_samples, &mut self.scratch[..frame_samples * ch])
            .map_err(|e| CoreError::platform(format!("opus decode_fec: {e}")))?;
        let step_us = (n as u64 * 1_000_000) / self.format.sample_rate as u64;
        self.last_ts = Timestamp(self.last_ts.as_micros().saturating_add(step_us));
        Ok(AudioFrame {
            format: self.format,
            samples: self.scratch[..n * ch].to_vec(),
            capture_ts: self.last_ts,
        })
    }
}

impl AudioDecoder for OpusDecoder {
    fn codec(&self) -> AudioCodecId {
        AudioCodecId::Opus
    }

    fn decode(&mut self, packet: Option<&EncodedAudio>) -> Result<AudioFrame> {
        let ch = self.format.channels as usize;
        let max_frame_samples = self.scratch.len() / ch;
        match packet {
            Some(pkt) => {
                if pkt.codec != AudioCodecId::Opus {
                    return Err(CoreError::invalid(format!("OpusDecoder got {:?} packet", pkt.codec)));
                }
                let n = self
                    .dec
                    .decode(&pkt.data, max_frame_samples, &mut self.scratch)
                    .map_err(|e| CoreError::platform(format!("opus decode: {e}")))?;
                self.last_frame_samples = n;
                self.last_ts = pkt.capture_ts;
                Ok(AudioFrame {
                    format: self.format,
                    samples: self.scratch[..n * ch].to_vec(),
                    capture_ts: pkt.capture_ts,
                })
            }
            None => {
                // Packet loss concealment (empty slice signals lost packet to rusty-opus).
                let req = self.last_frame_samples;
                let n = self
                    .dec
                    .decode(&[], req, &mut self.scratch[..req * ch])
                    .map_err(|e| CoreError::platform(format!("opus plc: {e}")))?;
                let step_us = (n as u64 * 1_000_000) / self.format.sample_rate as u64;
                self.last_ts = Timestamp(self.last_ts.as_micros().saturating_add(step_us));
                Ok(AudioFrame {
                    format: self.format,
                    samples: self.scratch[..n * ch].to_vec(),
                    capture_ts: self.last_ts,
                })
            }
        }
    }
}
