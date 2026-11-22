//! Media Foundation plumbing shared by the hardware encoder and decoder.

use std::sync::OnceLock;

use veloxdesk_core_traits::video::VideoCodec;
use veloxdesk_core_traits::{CoreError, Result};
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::{GUID, PWSTR};

pub(crate) fn mf_err(ctx: &'static str) -> impl Fn(windows::core::Error) -> CoreError {
    move |e| CoreError::platform(format!("{ctx}: {e}"))
}

thread_local! {
    static COM_INIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Initialise COM (MTA; ignored if the thread is already STA) and Media Foundation.
pub fn ensure_mf() -> Result<()> {
    COM_INIT.with(|c| {
        if !c.get() {
            // SAFETY: plain COM init; RPC_E_CHANGED_MODE (already STA) is harmless for MF.
            let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            c.set(true);
        }
    });
    static MF: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    MF.get_or_init(|| {
        // SAFETY: process-wide MF startup, never shut down (MF is refcounted).
        unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.map_err(|e| e.to_string())
    })
    .clone()
    .map_err(|e| CoreError::unsupported(format!("MFStartup: {e}")))
}

pub fn subtype(codec: VideoCodec) -> Result<GUID> {
    match codec {
        VideoCodec::H264 => Ok(MFVideoFormat_H264),
        VideoCodec::H265 => Ok(MFVideoFormat_HEVC),
        VideoCodec::Av1 => Ok(MFVideoFormat_AV1),
        other => Err(CoreError::unsupported(format!("MF codec {other:?}"))),
    }
}

/// One enumerated MFT.
pub struct MftInfo {
    pub activate: IMFActivate,
    pub name: String,
    pub vendor: &'static str,
}

fn vendor_from_id(id: &str) -> &'static str {
    let id = id.to_ascii_uppercase();
    if id.contains("10DE") {
        "NVENC"
    } else if id.contains("1002") || id.contains("1022") {
        "AMF"
    } else if id.contains("8086") {
        "QSV"
    } else {
        "MediaFoundation"
    }
}

fn get_string(a: &IMFActivate, key: &GUID) -> Option<String> {
    let mut p = PWSTR::null();
    let mut len = 0u32;
    // SAFETY: MF allocates the string with CoTaskMemAlloc; we free it after copying.
    unsafe {
        a.GetAllocatedString(key, &mut p, &mut len).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

/// Enumerate MFTs in `category` for `input → output` subtypes. `adapter_luid` restricts
/// hardware MFTs to that adapter (MFTEnum2, Win10 1703+), falling back to MFTEnumEx.
pub fn enum_mfts(category: GUID, flags: MFT_ENUM_FLAG, input: GUID, output: GUID, adapter_luid: Option<u64>) -> Vec<MftInfo> {
    let in_t = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: input };
    let out_t = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: output };
    let mut arr: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut n = 0u32;
    // SAFETY: MF fills a CoTaskMem array of `n` AddRef'd activates; we take ownership of each
    // element with ptr::read and free the array.
    unsafe {
        let mut ok = false;
        if let Some(luid) = adapter_luid.filter(|l| *l != 0) {
            let mut attrs = None;
            if MFCreateAttributes(&mut attrs, 1).is_ok()
                && let Some(attrs) = attrs
            {
                let bytes = [(luid as u32).to_le_bytes(), ((luid >> 32) as u32).to_le_bytes()].concat();
                let _ = attrs.SetBlob(&MFT_ENUM_ADAPTER_LUID, &bytes);
                ok = MFTEnum2(category, flags, Some(&in_t), Some(&out_t), &attrs, &mut arr, &mut n).is_ok();
            }
        }
        if !ok {
            arr = std::ptr::null_mut();
            n = 0;
            if MFTEnumEx(category, flags, Some(&in_t), Some(&out_t), &mut arr, &mut n).is_err() {
                return Vec::new();
            }
        }
        let mut out = Vec::new();
        for i in 0..n as usize {
            if let Some(a) = std::ptr::read(arr.add(i)) {
                let name = get_string(&a, &MFT_FRIENDLY_NAME_Attribute).unwrap_or_default();
                let vendor =
                    get_string(&a, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute).map(|v| vendor_from_id(&v)).unwrap_or("MediaFoundation");
                out.push(MftInfo { activate: a, name, vendor });
            }
        }
        if !arr.is_null() {
            CoTaskMemFree(Some(arr as *const _));
        }
        out
    }
}

pub fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

pub fn video_type(subtype: &GUID, w: u32, h: u32, fps: u32) -> Result<IMFMediaType> {
    // SAFETY: attribute setters on a fresh media type.
    unsafe {
        let t = MFCreateMediaType().map_err(mf_err("MFCreateMediaType"))?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).map_err(mf_err("SetGUID"))?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype).map_err(mf_err("SetGUID"))?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(w, h)).map_err(mf_err("frame size"))?;
        if fps > 0 {
            t.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1)).map_err(mf_err("frame rate"))?;
        }
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1)).map_err(mf_err("par"))?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32).map_err(mf_err("interlace"))?;
        Ok(t)
    }
}

/// Copy the contents of an MF sample into `dst` (contiguous).
pub fn sample_bytes(sample: &IMFSample, dst: &mut Vec<u8>) -> Result<()> {
    // SAFETY: Lock/Unlock paired; pointer valid for `len` bytes while locked.
    unsafe {
        let buf = sample.ConvertToContiguousBuffer().map_err(mf_err("ConvertToContiguousBuffer"))?;
        let mut p = std::ptr::null_mut();
        let mut len = 0u32;
        buf.Lock(&mut p, None, Some(&mut len)).map_err(mf_err("Lock"))?;
        dst.clear();
        dst.extend_from_slice(std::slice::from_raw_parts(p, len as usize));
        let _ = buf.Unlock();
    }
    Ok(())
}

/// Build an MF sample wrapping a copy of `data`.
pub fn memory_sample(data: &[u8], time_hns: i64, duration_hns: i64) -> Result<IMFSample> {
    // SAFETY: Lock/Unlock paired, copy within max length.
    unsafe {
        let buf = MFCreateMemoryBuffer(data.len().max(1) as u32).map_err(mf_err("MFCreateMemoryBuffer"))?;
        let mut p = std::ptr::null_mut();
        buf.Lock(&mut p, None, None).map_err(mf_err("Lock"))?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        let _ = buf.Unlock();
        buf.SetCurrentLength(data.len() as u32).map_err(mf_err("SetCurrentLength"))?;
        let s = MFCreateSample().map_err(mf_err("MFCreateSample"))?;
        s.AddBuffer(&buf).map_err(mf_err("AddBuffer"))?;
        s.SetSampleTime(time_hns).map_err(mf_err("SetSampleTime"))?;
        if duration_hns > 0 {
            let _ = s.SetSampleDuration(duration_hns);
        }
        Ok(s)
    }
}
