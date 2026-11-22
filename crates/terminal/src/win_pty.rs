//! Windows ConPTY backend (`CreatePseudoConsole` / `ResizePseudoConsole` /
//! `ClosePseudoConsole` + `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`, zero external C deps).

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot};
use veloxdesk_core_traits::{CoreError, Result};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};

struct SendHandle(HANDLE);
// SAFETY: Win32 kernel handles (pipes, process handles) are process-global and thread-safe
// when owned/synchronized cleanly.
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

struct SendHpcon(HPCON);
// SAFETY: HPCON is only accessed while holding the Mutex in `PtySession`.
unsafe impl Send for SendHpcon {}

/// Running Windows ConPTY process. Dropping kills the child and closes the pseudo-console.
pub struct PtyProcess {
    hpc: Arc<Mutex<Option<SendHpcon>>>,
    write_pipe: Arc<Mutex<Option<SendHandle>>>,
    process: Arc<Mutex<Option<SendHandle>>>,
}

pub struct PtyStreams {
    pub pty: PtyProcess,
    pub output: mpsc::UnboundedReceiver<Bytes>,
    pub exit: oneshot::Receiver<i32>,
}

fn close_handle_opt(slot: &Mutex<Option<SendHandle>>) {
    if let Some(h) = slot.lock().unwrap_or_else(|e| e.into_inner()).take()
        && !h.0.is_null()
        && h.0 != INVALID_HANDLE_VALUE
    {
        // SAFETY: owned Win32 handle taken out of the slot once.
        unsafe {
            let _ = CloseHandle(h.0);
        }
    }
}

fn close_hpcon_once(slot: &Mutex<Option<SendHpcon>>) {
    if let Some(h) = slot.lock().unwrap_or_else(|e| e.into_inner()).take() {
        // SAFETY: owned HPCON closed at most once.
        unsafe {
            ClosePseudoConsole(h.0);
        }
    }
}

impl PtyProcess {
    /// Spawn `shell` (empty = `%COMSPEC%` or `cmd.exe`) attached to a ConPTY of `cols`×`rows`.
    pub fn spawn(cols: u16, rows: u16, shell: &str) -> Result<PtyStreams> {
        let cols = cols.clamp(2, 1000) as i16;
        let rows = rows.clamp(2, 1000) as i16;

        let mut pty_in_read: HANDLE = null_mut();
        let mut pty_in_write: HANDLE = null_mut();
        let mut pty_out_read: HANDLE = null_mut();
        let mut pty_out_write: HANDLE = null_mut();

        // SAFETY: creating two standard anonymous pipes.
        unsafe {
            if CreatePipe(&raw mut pty_in_read, &raw mut pty_in_write, null(), 0) == 0 {
                return Err(CoreError::platform(format!("CreatePipe(in) failed: {}", GetLastError())));
            }
            if CreatePipe(&raw mut pty_out_read, &raw mut pty_out_write, null(), 0) == 0 {
                let err = GetLastError();
                let _ = CloseHandle(pty_in_read);
                let _ = CloseHandle(pty_in_write);
                return Err(CoreError::platform(format!("CreatePipe(out) failed: {err}")));
            }
        }

        let mut hpc: HPCON = 0;
        let hr = unsafe {
            CreatePseudoConsole(COORD { X: cols, Y: rows }, pty_in_read, pty_out_write, 0, &raw mut hpc)
        };
        // ConPTY owns duplicates of these two ends; closing ours ensures EOF when ConPTY closes.
        unsafe {
            let _ = CloseHandle(pty_in_read);
            let _ = CloseHandle(pty_out_write);
        }
        if hr < 0 {
            unsafe {
                let _ = CloseHandle(pty_in_write);
                let _ = CloseHandle(pty_out_read);
            }
            return Err(CoreError::platform(format!("CreatePseudoConsole failed: HRESULT 0x{:08x}", hr as u32)));
        }

        let cmd_str = if shell.trim().is_empty() {
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
        } else {
            shell.to_string()
        };
        let mut cmd_utf16: Vec<u16> = cmd_str.encode_utf16().chain(std::iter::once(0)).collect();

        // Prepare STARTUPINFOEXW with PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE.
        let mut attr_size: usize = 0;
        unsafe {
            let _ = InitializeProcThreadAttributeList(null_mut(), 1, 0, &raw mut attr_size);
        }
        let mut attr_buf = vec![0u8; attr_size.max(64)];
        let attr_list = attr_buf.as_mut_ptr().cast::<c_void>() as LPPROC_THREAD_ATTRIBUTE_LIST;

        let mut si: STARTUPINFOEXW = unsafe { zeroed() };
        si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        // Clear inherited redirected pipes (e.g. when host itself runs under redirected stdout)
        // so the child attaches its stdin/stdout/stderr to the ConPTY rather than parent's pipes.
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.lpAttributeList = attr_list;

        let mut pi: PROCESS_INFORMATION = unsafe { zeroed() };
        let spawn_res = unsafe {
            if InitializeProcThreadAttributeList(attr_list, 1, 0, &raw mut attr_size) == 0 {
                Err(format!("InitializeProcThreadAttributeList failed: {}", GetLastError()))
            } else {
                let upd = UpdateProcThreadAttribute(
                    attr_list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    hpc as *const c_void,
                    size_of::<HPCON>(),
                    null_mut(),
                    null_mut(),
                );
                let res = if upd == 0 {
                    Err(format!("UpdateProcThreadAttribute failed: {}", GetLastError()))
                } else if CreateProcessW(
                    null(),
                    cmd_utf16.as_mut_ptr(),
                    null(),
                    null(),
                    0,
                    EXTENDED_STARTUPINFO_PRESENT,
                    null(),
                    null(),
                    &raw const si.StartupInfo,
                    &raw mut pi,
                ) == 0
                {
                    Err(format!("CreateProcessW({cmd_str:?}) failed: {}", GetLastError()))
                } else {
                    Ok(())
                };
                DeleteProcThreadAttributeList(attr_list);
                res
            }
        };

        if let Err(msg) = spawn_res {
            unsafe {
                ClosePseudoConsole(hpc);
                let _ = CloseHandle(pty_in_write);
                let _ = CloseHandle(pty_out_read);
            }
            return Err(CoreError::platform(msg));
        }

        // We don't need the primary thread handle.
        unsafe {
            let _ = CloseHandle(pi.hThread);
        }

        let hpc_arc = Arc::new(Mutex::new(Some(SendHpcon(hpc))));
        let write_pipe = Arc::new(Mutex::new(Some(SendHandle(pty_in_write))));
        let process_arc = Arc::new(Mutex::new(Some(SendHandle(pi.hProcess))));

        // Dedicated reader thread drains pty_out_read until ConPTY closes it on exit.
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let read_handle = SendHandle(pty_out_read);
        std::thread::Builder::new()
            .name("vdk-pty-read".into())
            .spawn(move || {
                let _ = &read_handle;
                let h = read_handle.0;
                let mut buf = [0u8; 4096];
                loop {
                    let mut n = 0u32;
                    // SAFETY: `h` is owned exclusively by this thread; `buf` is valid for `buf.len()`.
                    let ok = unsafe { ReadFile(h, buf.as_mut_ptr(), buf.len() as u32, &raw mut n, null_mut()) };
                    if ok == 0 || n == 0 {
                        break;
                    }
                    if out_tx.send(Bytes::copy_from_slice(&buf[..n as usize])).is_err() {
                        break;
                    }
                }
                // SAFETY: owned read end of the ConPTY output pipe.
                unsafe {
                    let _ = CloseHandle(h);
                }
            })
            .map_err(CoreError::Io)?;

        // Dedicated waiter thread waits for child exit, closes ConPTY so output flushes & hits EOF,
        // and delivers the process exit code.
        let (exit_tx, exit_rx) = oneshot::channel();
        let hpc_wait = hpc_arc.clone();
        let proc_wait = process_arc.clone();
        let raw_proc = SendHandle(pi.hProcess);
        std::thread::Builder::new()
            .name("vdk-pty-wait".into())
            .spawn(move || {
                let _ = &raw_proc;
                // SAFETY: `raw_proc` remains valid until `proc_wait` is closed below or in Drop
                // (WaitForSingleObject wakes immediately if TerminateProcess is called in Drop).
                unsafe {
                    WaitForSingleObject(raw_proc.0, INFINITE);
                }
                let mut code = 0u32;
                let got = {
                    let guard = proc_wait.lock().unwrap_or_else(|e| e.into_inner());
                    guard.as_ref().is_some_and(|h| unsafe { GetExitCodeProcess(h.0, &raw mut code) != 0 })
                };
                // Closing the pseudo-console flushes buffered output to `pty_out_read` and signals EOF.
                close_hpcon_once(&hpc_wait);
                close_handle_opt(&proc_wait);
                let _ = exit_tx.send(if got { code as i32 } else { -1 });
            })
            .map_err(CoreError::Io)?;

        Ok(PtyStreams {
            pty: PtyProcess { hpc: hpc_arc, write_pipe, process: process_arc },
            output: out_rx,
            exit: exit_rx,
        })
    }

    pub fn write(&self, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let guard = self.write_pipe.lock().unwrap_or_else(|e| e.into_inner());
        let Some(h) = guard.as_ref() else {
            return Err(CoreError::Closed);
        };
        let mut offset = 0usize;
        while offset < data.len() {
            let mut written = 0u32;
            let chunk = &data[offset..];
            // SAFETY: valid handle guarded by mutex; chunk pointer valid for chunk.len().
            let ok = unsafe {
                WriteFile(h.0, chunk.as_ptr(), chunk.len().min(u32::MAX as usize) as u32, &raw mut written, null_mut())
            };
            if ok == 0 {
                return Err(CoreError::Closed);
            }
            if written == 0 {
                break;
            }
            offset += written as usize;
        }
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let cols = cols.clamp(2, 1000) as i16;
        let rows = rows.clamp(2, 1000) as i16;
        let guard = self.hpc.lock().unwrap_or_else(|e| e.into_inner());
        let Some(h) = guard.as_ref() else {
            return Ok(());
        };
        // SAFETY: valid HPCON guarded by mutex.
        let hr = unsafe { ResizePseudoConsole(h.0, COORD { X: cols, Y: rows }) };
        if hr < 0 {
            Err(CoreError::platform(format!("ResizePseudoConsole failed: 0x{:08x}", hr as u32)))
        } else {
            Ok(())
        }
    }
}

impl Drop for PtyProcess {
    fn drop(&mut self) {
        // Terminate child if still alive; waiter thread will observe exit and close process handle.
        if let Some(h) = self.process.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            // SAFETY: terminating child process on terminal stream close/drop.
            unsafe {
                let _ = TerminateProcess(h.0, 1);
            }
        }
        close_hpcon_once(&self.hpc);
        close_handle_opt(&self.write_pipe);
    }
}
