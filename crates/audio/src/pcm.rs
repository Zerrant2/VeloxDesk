//! Interleaved `f32` PCM helpers: sample format decoding, channel remixing, and linear resampling.

/// Decode raw WASAPI / OS bytes into interleaved `f32` samples in `[-1.0, 1.0]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleEncoding {
    F32,
    I16,
    I24Packed,
    I32,
}

impl SampleEncoding {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Self::F32 | Self::I32 => 4,
            Self::I24Packed => 3,
            Self::I16 => 2,
        }
    }

    /// Decode `frame_count * channels` samples from `raw` into `dst` (clears `dst` first).
    pub fn decode_to_f32(self, raw: &[u8], frame_count: usize, channels: usize, dst: &mut Vec<f32>) {
        let total = frame_count * channels;
        dst.clear();
        dst.reserve(total);
        match self {
            Self::F32 => {
                for chunk in raw.chunks_exact(4).take(total) {
                    dst.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
                }
            }
            Self::I16 => {
                const SCALE: f32 = 1.0 / 32768.0;
                for chunk in raw.chunks_exact(2).take(total) {
                    let v = i16::from_le_bytes([chunk[0], chunk[1]]);
                    dst.push(v as f32 * SCALE);
                }
            }
            Self::I24Packed => {
                const SCALE: f32 = 1.0 / 8_388_608.0;
                for chunk in raw.chunks_exact(3).take(total) {
                    // Sign-extend 24-bit little-endian to i32.
                    let raw_i32 = ((chunk[0] as i32) | ((chunk[1] as i32) << 8) | ((chunk[2] as i32) << 16)) << 8 >> 8;
                    dst.push(raw_i32 as f32 * SCALE);
                }
            }
            Self::I32 => {
                const SCALE: f32 = 1.0 / 2_147_483_648.0;
                for chunk in raw.chunks_exact(4).take(total) {
                    let v = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    dst.push(v as f32 * SCALE);
                }
            }
        }
    }
}

/// Remix interleaved `f32` samples from `src_ch` to `dst_ch` (supports 1↔2 and multi-channel→stereo/mono).
pub fn remix_channels(src: &[f32], src_ch: usize, dst_ch: usize, dst: &mut Vec<f32>) {
    dst.clear();
    if src_ch == 0 || dst_ch == 0 || src.is_empty() {
        return;
    }
    if src_ch == dst_ch {
        dst.extend_from_slice(src);
        return;
    }
    let frames = src.len() / src_ch;
    dst.reserve(frames * dst_ch);
    match (src_ch, dst_ch) {
        (1, 2) => {
            for &s in src.iter().take(frames) {
                dst.push(s);
                dst.push(s);
            }
        }
        (2, 1) => {
            for f in src.chunks_exact(2) {
                dst.push(0.5 * (f[0] + f[1]));
            }
        }
        (n, 2) if n >= 3 => {
            // Standard SMPTE/WAVE channel order: FL, FR, FC, LFE, BL, BR, ...
            // Fold center (0.707) and surround pairs (0.5) into L/R with soft clamp.
            for f in src.chunks_exact(n) {
                let mut l = f[0];
                let mut r = f[1];
                if n >= 3 {
                    let c = f[2] * 0.707_106_77;
                    l += c;
                    r += c;
                }
                if n >= 6 {
                    l += f[4] * 0.5;
                    r += f[5] * 0.5;
                }
                if n >= 8 {
                    l += f[6] * 0.5;
                    r += f[7] * 0.5;
                }
                dst.push(l.clamp(-1.0, 1.0));
                dst.push(r.clamp(-1.0, 1.0));
            }
        }
        (n, 1) => {
            let inv = 1.0 / n as f32;
            for f in src.chunks_exact(n) {
                let sum: f32 = f.iter().copied().sum();
                dst.push((sum * inv).clamp(-1.0, 1.0));
            }
        }
        (n, m) => {
            for f in src.chunks_exact(n) {
                for ch in 0..m {
                    dst.push(f.get(ch).copied().unwrap_or(0.0));
                }
            }
        }
    }
}

/// Stateful streaming linear resampler for interleaved `f32` PCM (used when an OS capture
/// device runs at e.g. 44.1 kHz or 96 kHz while the Opus stream is 48 kHz).
pub struct LinearResampler {
    src_rate: u32,
    dst_rate: u32,
    channels: usize,
    pos: f64,
    prev: Vec<f32>,
    has_prev: bool,
}

impl LinearResampler {
    pub fn new(src_rate: u32, dst_rate: u32, channels: usize) -> Self {
        Self {
            src_rate: src_rate.max(1),
            dst_rate: dst_rate.max(1),
            channels: channels.max(1),
            pos: 0.0,
            prev: vec![0.0; channels.max(1)],
            has_prev: false,
        }
    }

    pub fn is_passthrough(&self) -> bool {
        self.src_rate == self.dst_rate
    }

    pub fn process(&mut self, src: &[f32], dst: &mut Vec<f32>) {
        dst.clear();
        let ch = self.channels;
        let in_frames = src.len() / ch;
        if in_frames == 0 {
            return;
        }
        if self.src_rate == self.dst_rate {
            dst.extend_from_slice(&src[..in_frames * ch]);
            return;
        }

        let step = self.src_rate as f64 / self.dst_rate as f64;
        let est = ((in_frames as f64) / step).ceil() as usize + 2;
        dst.reserve(est * ch);

        let sample_at = |idx: isize, c: usize, prev: &[f32]| -> f32 {
            if idx < 0 {
                prev[c]
            } else {
                src[idx as usize * ch + c]
            }
        };

        if !self.has_prev {
            self.prev.copy_from_slice(&src[..ch]);
            self.has_prev = true;
        }

        let limit = (in_frames as f64) - 1.0;
        while self.pos <= limit {
            let i0 = self.pos.floor() as isize;
            let frac = (self.pos - i0 as f64) as f32;
            let i1 = (i0 + 1).min(in_frames as isize - 1);
            for c in 0..ch {
                let s0 = sample_at(i0, c, &self.prev);
                let s1 = sample_at(i1, c, &self.prev);
                dst.push(s0 + (s1 - s0) * frac);
            }
            self.pos += step;
        }
        self.pos -= in_frames as f64;
        self.prev.copy_from_slice(&src[(in_frames - 1) * ch..in_frames * ch]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_i16_and_i24() {
        let mut out = Vec::new();
        let i16_bytes = [0u8, 0, 0xFF, 0x7F, 0x00, 0x80];
        SampleEncoding::I16.decode_to_f32(&i16_bytes, 3, 1, &mut out);
        assert!((out[0] - 0.0).abs() < 1e-6);
        assert!((out[1] - 32767.0 / 32768.0).abs() < 1e-5);
        assert!((out[2] + 1.0).abs() < 1e-6);

        let i24_bytes = [0x00u8, 0x00, 0x80, 0xFF, 0xFF, 0x7F];
        SampleEncoding::I24Packed.decode_to_f32(&i24_bytes, 2, 1, &mut out);
        assert!((out[0] + 1.0).abs() < 1e-6);
        assert!((out[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn remix_mono_stereo_and_51() {
        let mut dst = Vec::new();
        remix_channels(&[0.25, -0.5], 1, 2, &mut dst);
        assert_eq!(dst, vec![0.25, 0.25, -0.5, -0.5]);

        remix_channels(&[0.4, 0.2, 0.0, 0.0, 0.0, 0.0], 6, 2, &mut dst);
        assert_eq!(dst, vec![0.4, 0.2]);
    }

    #[test]
    fn resample_44100_to_48000_preserves_sine() {
        let mut rs = LinearResampler::new(44_100, 48_000, 2);
        let in_frames = 441; // 10 ms at 44.1 kHz
        let mut src = Vec::with_capacity(in_frames * 2);
        for i in 0..in_frames {
            let t = i as f32 / 44_100.0;
            let v = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
            src.push(v);
            src.push(-v);
        }
        let mut dst = Vec::new();
        rs.process(&src, &mut dst);
        let out_frames = dst.len() / 2;
        assert!((out_frames as i32 - 480).abs() <= 2, "got {out_frames} frames");
        // Check L == -R symmetry and amplitude.
        for f in dst.chunks_exact(2) {
            assert!((f[0] + f[1]).abs() < 1e-5);
            assert!(f[0].abs() <= 0.51);
        }
    }
}
