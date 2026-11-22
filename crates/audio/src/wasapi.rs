//! Windows WASAPI low-latency audio loopback capture, microphone capture, and playback.

use std::time::{Duration, Instant};

use veloxdesk_core_traits::audio::{AudioCapture, AudioCaptureConfig, AudioFormat, AudioFrame, AudioPlayback};
use veloxdesk_core_traits::{CoreError, Result, Timestamp};
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::HSTRING;

use crate::pcm::{LinearResampler, SampleEncoding, remix_channels};

const WAVE_FORMAT_PCM_TAG: u16 = 1;
const WAVE_FORMAT_IEEE_FLOAT_TAG: u16 = 3;

thread_local! {
    static COM_INIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn ensure_com() {
    COM_INIT.with(|c| {
        if !c.get() {
            // SAFETY: thread-local COM init; RPC_E_CHANGED_MODE is harmless if already STA.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            c.set(true);
        }
    });
}

fn wasapi_err(ctx: &'static str) -> impl Fn(windows::core::Error) -> CoreError {
    move |e| {
        let code = e.code();
        if code == AUDCLNT_E_DEVICE_INVALIDATED || code == AUDCLNT_E_SERVICE_NOT_RUNNING {
            CoreError::reinit(format!("{ctx}: audio device invalidated ({e})"))
        } else {
            CoreError::platform(format!("{ctx}: {e}"))
        }
    }
}

fn pick_device(flow: EDataFlow, device_id: Option<&str>) -> Result<IMMDevice> {
    ensure_com();
    // SAFETY: standard WASAPI IMMDeviceEnumerator COM calls.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(wasapi_err("MMDeviceEnumerator"))?;
        match device_id {
            Some(id) if !id.is_empty() => {
                let h = HSTRING::from(id);
                enumerator.GetDevice(&h).map_err(wasapi_err("GetDevice"))
            }
            _ => enumerator
                .GetDefaultAudioEndpoint(flow, eConsole)
                .or_else(|_| enumerator.GetDefaultAudioEndpoint(flow, eMultimedia))
                .map_err(|e| CoreError::unsupported(format!("no default WASAPI audio endpoint: {e}"))),
        }
    }
}

fn parse_mix_format(pwfx: *const WAVEFORMATEX) -> Result<(AudioFormat, SampleEncoding)> {
    // SAFETY: `pwfx` comes from `IAudioClient::GetMixFormat`.
    let wfx = unsafe { *pwfx };
    let sample_rate = wfx.nSamplesPerSec;
    let channels = wfx.nChannels;
    let tag = wfx.wFormatTag;
    let bits = wfx.wBitsPerSample;

    let enc = if tag == WAVE_FORMAT_IEEE_FLOAT_TAG && bits == 32 {
        SampleEncoding::F32
    } else if tag == WAVE_FORMAT_PCM_TAG {
        match bits {
            16 => SampleEncoding::I16,
            24 => SampleEncoding::I24Packed,
            32 => SampleEncoding::I32,
            other => return Err(CoreError::unsupported(format!("unsupported PCM bit depth {other}"))),
        }
    } else if tag as u32 == WAVE_FORMAT_EXTENSIBLE {
        // SAFETY: WAVEFORMATEXTENSIBLE begins with WAVEFORMATEX when wFormatTag == WAVE_FORMAT_EXTENSIBLE.
        let ext = unsafe { *(pwfx as *const WAVEFORMATEXTENSIBLE) };
        let sub = { ext.SubFormat };
        if sub == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT && bits == 32 {
            SampleEncoding::F32
        } else if sub == KSDATAFORMAT_SUBTYPE_PCM {
            match bits {
                16 => SampleEncoding::I16,
                24 => SampleEncoding::I24Packed,
                32 => SampleEncoding::I32,
                other => return Err(CoreError::unsupported(format!("unsupported extensible PCM depth {other}"))),
            }
        } else {
            return Err(CoreError::unsupported("unsupported WAVEFORMATEXTENSIBLE subformat"));
        }
    } else {
        return Err(CoreError::unsupported(format!("unsupported WASAPI wave format tag {tag}")));
    };

    Ok((AudioFormat { sample_rate, channels }, enc))
}

fn make_float_waveformat(format: AudioFormat) -> WAVEFORMATEXTENSIBLE {
    let channels = format.channels.max(1);
    let block_align = channels * 4;
    let channel_mask = match channels {
        1 => 0x4, // SPEAKER_FRONT_CENTER
        2 => 0x3, // SPEAKER_FRONT_LEFT | SPEAKER_FRONT_RIGHT
        _ => 0,
    };
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: format.sample_rate,
            nAvgBytesPerSec: format.sample_rate * block_align as u32,
            nBlockAlign: block_align,
            wBitsPerSample: 32,
            cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 { wValidBitsPerSample: 32 },
        dwChannelMask: channel_mask,
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    }
}

struct CaptureState {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: Option<HANDLE>,
    native_fmt: AudioFormat,
    out_fmt: AudioFormat,
    encoding: SampleEncoding,
    resampler: LinearResampler,
    raw_f32: Vec<f32>,
    remixed: Vec<f32>,
    resampled: Vec<f32>,
}

// SAFETY: WASAPI COM interfaces are free-threaded (MTA); AudioCapture is driven from a single thread.
unsafe impl Send for CaptureState {}

impl Drop for CaptureState {
    fn drop(&mut self) {
        // SAFETY: stop audio stream and close optional event handle.
        unsafe {
            let _ = self.client.Stop();
            if let Some(h) = self.event.take() {
                let _ = CloseHandle(h);
            }
        }
    }
}

/// Windows WASAPI audio capture (supports both system loopback and microphone input).
pub struct WasapiCapture {
    default_microphone: bool,
    state: Option<CaptureState>,
}

impl WasapiCapture {
    pub fn loopback() -> Self {
        Self { default_microphone: false, state: None }
    }

    pub fn microphone() -> Self {
        Self { default_microphone: true, state: None }
    }
}

impl AudioCapture for WasapiCapture {
    fn backend(&self) -> &'static str {
        if self.default_microphone { "wasapi-mic" } else { "wasapi-loopback" }
    }

    fn start(&mut self, cfg: &AudioCaptureConfig) -> Result<AudioFormat> {
        self.stop();
        let is_mic = cfg.microphone || self.default_microphone;
        let flow = if is_mic { eCapture } else { eRender };
        let device = pick_device(flow, cfg.device.as_deref())?;

        // SAFETY: WASAPI IAudioClient / IAudioCaptureClient initialization.
        unsafe {
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(wasapi_err("Activate(IAudioClient)"))?;
            let pwfx = client.GetMixFormat().map_err(wasapi_err("GetMixFormat"))?;
            let parsed = parse_mix_format(pwfx);
            let (native_fmt, encoding) = match parsed {
                Ok(v) => v,
                Err(e) => {
                    CoTaskMemFree(Some(pwfx as *const _));
                    return Err(e);
                }
            };

            let buf_hns = (cfg.chunk.as_nanos() / 100).clamp(200_000, 1_000_000) as i64; // 20–100 ms ring
            let mut flags = if is_mic {
                AUDCLNT_STREAMFLAGS_EVENTCALLBACK
            } else {
                // Loopback does NOT signal event callback during silence on Windows; poll instead.
                AUDCLNT_STREAMFLAGS_LOOPBACK
            };

            let event = if is_mic {
                let h = CreateEventW(None, false, false, None).map_err(wasapi_err("CreateEventW"))?;
                Some(h)
            } else {
                None
            };

            let init_res = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, buf_hns, 0, pwfx, None);
            CoTaskMemFree(Some(pwfx as *const _));
            if let Err(e) = init_res {
                if let Some(h) = event {
                    let _ = CloseHandle(h);
                }
                return Err(wasapi_err("IAudioClient::Initialize")(e));
            }

            if let Some(h) = event
                && let Err(e) = client.SetEventHandle(h)
            {
                let _ = CloseHandle(h);
                return Err(wasapi_err("SetEventHandle")(e));
            }
            let _ = &mut flags;

            let capture: IAudioCaptureClient = client.GetService().map_err(wasapi_err("GetService(IAudioCaptureClient)"))?;
            client.Start().map_err(wasapi_err("IAudioClient::Start"))?;

            let out_fmt = if cfg.format.sample_rate > 0 && cfg.format.channels > 0 { cfg.format } else { native_fmt };
            let resampler = LinearResampler::new(native_fmt.sample_rate, out_fmt.sample_rate, out_fmt.channels as usize);

            tracing::info!(
                backend = self.backend(),
                native = ?native_fmt,
                output = ?out_fmt,
                "WASAPI capture started"
            );

            self.state = Some(CaptureState {
                client,
                capture,
                event,
                native_fmt,
                out_fmt,
                encoding,
                resampler,
                raw_f32: Vec::new(),
                remixed: Vec::new(),
                resampled: Vec::new(),
            });
            Ok(out_fmt)
        }
    }

    fn read(&mut self, timeout: Duration) -> Result<Option<AudioFrame>> {
        let Some(st) = self.state.as_mut() else {
            return Err(CoreError::invalid("WasapiCapture::read called before start()"));
        };
        let deadline = Instant::now() + timeout;

        loop {
            // SAFETY: WASAPI GetNextPacketSize / GetBuffer / ReleaseBuffer paired cleanly.
            unsafe {
                let packet_frames = st.capture.GetNextPacketSize().map_err(wasapi_err("GetNextPacketSize"))?;
                if packet_frames > 0 {
                    let mut data_ptr = std::ptr::null_mut();
                    let mut num_frames = 0u32;
                    let mut flags = 0u32;
                    st.capture
                        .GetBuffer(&mut data_ptr, &mut num_frames, &mut flags, None, None)
                        .map_err(wasapi_err("IAudioCaptureClient::GetBuffer"))?;
                    let capture_ts = Timestamp::now();
                    let n_frames = num_frames as usize;
                    let n_ch = st.native_fmt.channels as usize;
                    let silent = (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data_ptr.is_null();

                    if silent {
                        st.raw_f32.clear();
                        st.raw_f32.resize(n_frames * n_ch, 0.0);
                    } else {
                        let byte_len = n_frames * n_ch * st.encoding.bytes_per_sample();
                        let slice = std::slice::from_raw_parts(data_ptr, byte_len);
                        st.encoding.decode_to_f32(slice, n_frames, n_ch, &mut st.raw_f32);
                    }

                    let rel = st.capture.ReleaseBuffer(num_frames);
                    rel.map_err(wasapi_err("IAudioCaptureClient::ReleaseBuffer"))?;

                    if n_frames == 0 {
                        continue;
                    }

                    let out_ch = st.out_fmt.channels as usize;
                    remix_channels(&st.raw_f32, n_ch, out_ch, &mut st.remixed);
                    let samples = if st.resampler.is_passthrough() {
                        st.remixed.clone()
                    } else {
                        st.resampler.process(&st.remixed, &mut st.resampled);
                        st.resampled.clone()
                    };

                    return Ok(Some(AudioFrame { format: st.out_fmt, samples, capture_ts }));
                }
            }

            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }

            if let Some(ev) = st.event {
                let wait_ms = left.as_millis().clamp(1, u32::MAX as u128) as u32;
                // SAFETY: valid event handle.
                let wr = unsafe { WaitForSingleObject(ev, wait_ms) };
                if wr != WAIT_OBJECT_0 {
                    return Ok(None);
                }
            } else {
                std::thread::sleep(left.min(Duration::from_millis(2)));
            }
        }
    }

    fn stop(&mut self) {
        self.state = None;
    }
}

struct PlaybackState {
    client: IAudioClient,
    render: IAudioRenderClient,
    format: AudioFormat,
    buffer_frames: u32,
    max_fill_frames: u32,
}

// SAFETY: WASAPI COM interfaces are free-threaded (MTA); AudioPlayback is driven from one thread.
unsafe impl Send for PlaybackState {}

impl Drop for PlaybackState {
    fn drop(&mut self) {
        // SAFETY: stop render stream.
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

/// Windows WASAPI low-latency shared-mode playback sink.
pub struct WasapiPlayback {
    state: Option<PlaybackState>,
}

impl WasapiPlayback {
    pub fn new() -> Self {
        Self { state: None }
    }
}

impl Default for WasapiPlayback {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioPlayback for WasapiPlayback {
    fn backend(&self) -> &'static str {
        "wasapi-render"
    }

    fn start(&mut self, format: AudioFormat, target_latency: Duration) -> Result<()> {
        self.stop();
        if format.sample_rate == 0 || format.channels == 0 {
            return Err(CoreError::invalid("invalid playback AudioFormat"));
        }
        let device = pick_device(eRender, None)?;
        let wf = make_float_waveformat(format);
        // Use AUTOCONVERTPCM so Windows accepts 48 kHz stereo f32 regardless of endpoint format.
        let flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        // Allocate at least 40 ms ring (or 2x target_latency) so occasional bursts don't underrun.
        let ring_dur = target_latency.saturating_mul(2).max(Duration::from_millis(40));
        let buf_hns = (ring_dur.as_nanos() / 100).min(2_000_000) as i64;

        // SAFETY: standard WASAPI render initialization.
        unsafe {
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(wasapi_err("Activate(IAudioClient)"))?;
            client
                .Initialize(AUDCLNT_SHAREMODE_SHARED, flags, buf_hns, 0, &wf.Format, None)
                .map_err(wasapi_err("IAudioClient::Initialize(render)"))?;
            let buffer_frames = client.GetBufferSize().map_err(wasapi_err("GetBufferSize"))?;
            let render: IAudioRenderClient = client.GetService().map_err(wasapi_err("GetService(IAudioRenderClient)"))?;
            client.Start().map_err(wasapi_err("IAudioClient::Start(render)"))?;

            let target_frames =
                ((format.sample_rate as u128 * target_latency.as_micros()) / 1_000_000) as u32;
            let max_fill_frames = target_frames.max(buffer_frames / 2).min(buffer_frames);

            tracing::info!(
                format = ?format,
                buffer_frames,
                max_fill_frames,
                "WASAPI playback started"
            );

            self.state = Some(PlaybackState { client, render, format, buffer_frames, max_fill_frames });
        }
        Ok(())
    }

    fn push(&mut self, frame: &AudioFrame) -> Result<()> {
        let Some(st) = self.state.as_mut() else {
            return Err(CoreError::invalid("WasapiPlayback::push called before start()"));
        };
        if frame.format != st.format {
            return Err(CoreError::invalid(format!(
                "WasapiPlayback expected {:?}, got {:?}",
                st.format, frame.format
            )));
        }
        let ch = st.format.channels as usize;
        let frames = frame.frames_per_channel();
        if frames == 0 {
            return Ok(());
        }

        // SAFETY: WASAPI GetCurrentPadding / GetBuffer / ReleaseBuffer paired cleanly.
        unsafe {
            let padding = st.client.GetCurrentPadding().map_err(wasapi_err("GetCurrentPadding"))?;
            let avail = st.buffer_frames.saturating_sub(padding);
            // If playback buffer already exceeds latency cap, drop this burst frame to stay low-latency.
            if padding >= st.max_fill_frames && avail < frames as u32 {
                tracing::debug!(padding, avail, frames, "WASAPI playback dropping excess audio frame to keep low latency");
                return Ok(());
            }
            let to_write = (frames as u32).min(avail);
            if to_write == 0 {
                return Ok(());
            }
            let ptr = st.render.GetBuffer(to_write).map_err(wasapi_err("IAudioRenderClient::GetBuffer"))?;
            let byte_count = to_write as usize * ch * std::mem::size_of::<f32>();
            std::ptr::copy_nonoverlapping(frame.samples.as_ptr() as *const u8, ptr, byte_count);
            st.render.ReleaseBuffer(to_write, 0).map_err(wasapi_err("IAudioRenderClient::ReleaseBuffer"))?;
        }
        Ok(())
    }

    fn buffered_us(&self) -> u32 {
        let Some(st) = self.state.as_ref() else {
            return 0;
        };
        // SAFETY: read-only padding query on active IAudioClient.
        let padding = unsafe { st.client.GetCurrentPadding().unwrap_or(0) };
        ((padding as u64 * 1_000_000) / st.format.sample_rate.max(1) as u64) as u32
    }

    fn stop(&mut self) {
        self.state = None;
    }
}
