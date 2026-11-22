//! Windows `CF_HDROP` file-list clipboard read/write helpers.

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use veloxdesk_core_traits::{CoreError, Result};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_HDROP;
use windows::Win32::UI::Shell::{DROPFILES, DragQueryFileW, HDROP};

struct ClipboardLock;

impl ClipboardLock {
    fn open() -> Result<Self> {
        // Retry briefly if another process holds the clipboard lock.
        for _ in 0..5 {
            // SAFETY: OpenClipboard with null HWND associates with current task.
            if unsafe { OpenClipboard(Some(HWND::default())) }.is_ok() {
                return Ok(Self);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        Err(CoreError::platform("OpenClipboard failed (clipboard busy)"))
    }
}

impl Drop for ClipboardLock {
    fn drop(&mut self) {
        // SAFETY: paired with OpenClipboard.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Current Windows clipboard change counter (cheap lock-free poll check).
pub fn sequence_number() -> u32 {
    // SAFETY: GetClipboardSequenceNumber has no preconditions.
    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() }
}

/// Read `CF_HDROP` file paths if currently present on the Windows clipboard.
pub fn read_file_list() -> Result<Option<Vec<String>>> {
    // SAFETY: format availability check without opening clipboard.
    if unsafe { IsClipboardFormatAvailable(CF_HDROP.0 as u32) }.is_err() {
        return Ok(None);
    }
    let _lock = ClipboardLock::open()?;
    // SAFETY: clipboard is open; handle is owned by the system (do not free).
    let handle = match unsafe { GetClipboardData(CF_HDROP.0 as u32) } {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    let hdrop = HDROP(handle.0);
    // SAFETY: 0xFFFF_FFFF queries the total number of dropped files.
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    if count == 0 {
        return Ok(None);
    }
    let mut paths = Vec::with_capacity(count as usize);
    for i in 0..count {
        // SAFETY: passing None returns required character count (excluding NUL).
        let len = unsafe { DragQueryFileW(hdrop, i, None) };
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len as usize + 1];
        // SAFETY: buf has len + 1 UTF-16 code units.
        let written = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) } as usize;
        if written > 0 {
            let os = OsString::from_wide(&buf[..written]);
            paths.push(os.to_string_lossy().into_owned());
        }
    }
    Ok(if paths.is_empty() { None } else { Some(paths) })
}

/// Write a list of file paths to the Windows clipboard as `CF_HDROP` (Unicode).
pub fn write_file_list(paths: &[String]) -> Result<()> {
    let mut wide: Vec<u16> = Vec::new();
    for p in paths {
        if p.is_empty() {
            continue;
        }
        wide.extend(Path::new(p).as_os_str().encode_wide());
        wide.push(0);
    }
    wide.push(0); // double-NUL terminator

    let header_len = std::mem::size_of::<DROPFILES>();
    let total_bytes = header_len + wide.len() * std::mem::size_of::<u16>();

    let _lock = ClipboardLock::open()?;
    // SAFETY: GlobalAlloc -> GlobalLock -> write DROPFILES + UTF-16 paths -> GlobalUnlock -> SetClipboardData.
    unsafe {
        let hmem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total_bytes)
            .map_err(|e| CoreError::platform(format!("GlobalAlloc(CF_HDROP): {e}")))?;
        let ptr = GlobalLock(hmem);
        if ptr.is_null() {
            let _ = GlobalFree(Some(hmem));
            return Err(CoreError::platform("GlobalLock(CF_HDROP) returned null"));
        }
        let df = DROPFILES {
            pFiles: header_len as u32,
            pt: Default::default(),
            fNC: false.into(),
            fWide: true.into(),
        };
        std::ptr::write(ptr as *mut DROPFILES, df);
        std::ptr::copy_nonoverlapping(
            wide.as_ptr() as *const u8,
            (ptr as *mut u8).add(header_len),
            wide.len() * std::mem::size_of::<u16>(),
        );
        let _ = GlobalUnlock(hmem);

        if let Err(e) = EmptyClipboard() {
            let _ = GlobalFree(Some(hmem));
            return Err(CoreError::platform(format!("EmptyClipboard: {e}")));
        }
        // On success, the system takes ownership of `hmem`.
        if let Err(e) = SetClipboardData(CF_HDROP.0 as u32, Some(HANDLE(hmem.0))) {
            let _ = GlobalFree(Some(hmem));
            return Err(CoreError::platform(format!("SetClipboardData(CF_HDROP): {e}")));
        }
    }
    Ok(())
}
