//! System service / daemon helpers (Windows SCM + Linux systemd) and active-desktop
//! detection (interactive desktop vs UAC / Winlogon secure desktop).

use std::fmt;
use std::path::Path;

use tokio::sync::watch;
use veloxdesk_core_traits::{CoreError, Result};

/// Service name registered with Windows SCM / Linux systemd.
pub const SERVICE_NAME: &str = "VeloxDesk";

/// Current installation/runtime status of the host system service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    NotInstalled,
    Stopped,
    Running,
    Unknown(String),
}

impl fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled => write!(f, "not installed"),
            Self::Stopped => write!(f, "stopped"),
            Self::Running => write!(f, "running"),
            Self::Unknown(s) => write!(f, "unknown ({s})"),
        }
    }
}

/// Active input desktop classification (used to detect UAC / lock screen / secure desktop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopKind {
    /// Normal interactive user desktop (e.g. `"Default"` on Windows, X11/Wayland on Linux).
    Interactive(String),
    /// Elevated/secure desktop (Windows `"Winlogon"` UAC prompt or lock screen).
    SecureDesktop(String),
    /// Could not determine the active desktop.
    Unknown,
}

impl fmt::Display for DesktopKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Interactive(name) => write!(f, "Interactive({name})"),
            Self::SecureDesktop(name) => write!(f, "SecureDesktop({name})"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

/// Detect whether the current input desktop is an interactive desktop or a secure (UAC/Winlogon) desktop.
pub fn detect_desktop() -> DesktopKind {
    #[cfg(windows)]
    {
        detect_desktop_windows()
    }
    #[cfg(not(windows))]
    {
        if let Ok(d) = std::env::var("WAYLAND_DISPLAY")
            && !d.is_empty()
        {
            return DesktopKind::Interactive(format!("wayland:{d}"));
        }
        if let Ok(d) = std::env::var("DISPLAY")
            && !d.is_empty()
        {
            return DesktopKind::Interactive(format!("x11:{d}"));
        }
        DesktopKind::Unknown
    }
}

#[cfg(windows)]
fn detect_desktop_windows() -> DesktopKind {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, GetLastError};
    use windows_sys::Win32::System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_READOBJECTS, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
    };

    // SAFETY: OpenInputDesktop/GetUserObjectInformationW/CloseDesktop on stack UTF-16 buffer.
    unsafe {
        let hdesk = OpenInputDesktop(0, 0, DESKTOP_READOBJECTS);
        if hdesk.is_null() {
            if GetLastError() == ERROR_ACCESS_DENIED {
                return DesktopKind::SecureDesktop("Winlogon".into());
            }
            return DesktopKind::Unknown;
        }
        let mut buf = [0u16; 128];
        let mut needed = 0u32;
        let ok = GetUserObjectInformationW(
            hdesk,
            UOI_NAME,
            buf.as_mut_ptr().cast::<c_void>(),
            (buf.len() * 2) as u32,
            &raw mut needed,
        );
        let _ = CloseDesktop(hdesk);
        if ok == 0 {
            return DesktopKind::Unknown;
        }
        let words = ((needed as usize) / 2).min(buf.len());
        let len = buf[..words].iter().position(|&c| c == 0).unwrap_or(words);
        let name = String::from_utf16_lossy(&buf[..len]);
        if name.eq_ignore_ascii_case("winlogon") || name.eq_ignore_ascii_case("screensaver") {
            DesktopKind::SecureDesktop(name)
        } else {
            DesktopKind::Interactive(name)
        }
    }
}

/// Generate a Linux `systemd` service unit file for running VeloxDesk as a background host daemon.
pub fn systemd_unit(bin_path: &Path, extra_args: &[String]) -> String {
    let mut exec = format!("\"{}\" --host", bin_path.display());
    for a in extra_args {
        exec.push(' ');
        exec.push_str(a);
    }
    format!(
        "[Unit]\n\
         Description=VeloxDesk Remote Desktop Host Service\n\
         After=network-online.target graphical.target\n\
         Wants=network-online.target\n\n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec}\n\
         Restart=on-failure\n\
         RestartSec=3\n\n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

/// Build the Windows SCM `binPath` command line for `veloxdesk --host --service-run ...`.
pub fn windows_service_command_line(bin_path: &Path, extra_args: &[String]) -> String {
    let mut cmd = format!("\"{}\" --host --service-run", bin_path.display());
    for a in extra_args {
        cmd.push(' ');
        if a.contains(' ') {
            cmd.push('"');
            cmd.push_str(a);
            cmd.push('"');
        } else {
            cmd.push_str(a);
        }
    }
    cmd
}

/// Install VeloxDesk as an OS system service (Windows SCM via `sc.exe`, or Linux `systemd` unit).
pub fn install_service(bin_path: &Path, extra_args: &[String]) -> Result<String> {
    #[cfg(windows)]
    {
        let bin_cmd = windows_service_command_line(bin_path, extra_args);
        let out = std::process::Command::new("sc.exe")
            .args([
                "create",
                SERVICE_NAME,
                &format!("binPath= {bin_cmd}"),
                "start=",
                "auto",
                "DisplayName=",
                "VeloxDesk Host Service",
            ])
            .output()
            .map_err(CoreError::Io)?;
        let msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() {
            Ok(msg)
        } else if msg.contains("Access is denied") || out.status.code() == Some(5) {
            Err(CoreError::AccessDenied("installing Windows service requires Administrator privileges".into()))
        } else {
            Err(CoreError::platform(format!("sc create failed: {msg}")))
        }
    }
    #[cfg(target_os = "linux")]
    {
        let unit = systemd_unit(bin_path, extra_args);
        let path = Path::new("/etc/systemd/system/veloxdesk.service");
        std::fs::write(path, unit).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                CoreError::AccessDenied("writing /etc/systemd/system/veloxdesk.service requires root".into())
            } else {
                CoreError::Io(e)
            }
        })?;
        Ok(format!("installed {}", path.display()))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (bin_path, extra_args);
        Err(CoreError::unsupported("service installation is not implemented on this OS"))
    }
}

/// Uninstall the VeloxDesk system service.
pub fn uninstall_service() -> Result<String> {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("sc.exe")
            .args(["delete", SERVICE_NAME])
            .output()
            .map_err(CoreError::Io)?;
        let msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() {
            Ok(msg)
        } else if msg.contains("Access is denied") || out.status.code() == Some(5) {
            Err(CoreError::AccessDenied("uninstalling Windows service requires Administrator privileges".into()))
        } else {
            Err(CoreError::platform(format!("sc delete failed: {msg}")))
        }
    }
    #[cfg(target_os = "linux")]
    {
        let path = Path::new("/etc/systemd/system/veloxdesk.service");
        match std::fs::remove_file(path) {
            Ok(()) => Ok(format!("removed {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("service not installed".into()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                Err(CoreError::AccessDenied("removing systemd unit requires root".into()))
            }
            Err(e) => Err(CoreError::Io(e)),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(CoreError::unsupported("service uninstallation is not implemented on this OS"))
    }
}

/// Query the OS service manager for VeloxDesk's current status.
pub fn service_status() -> Result<ServiceStatus> {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("sc.exe")
            .args(["query", SERVICE_NAME])
            .output()
            .map_err(CoreError::Io)?;
        let text = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            if out.status.code() == Some(1060) || text.contains("1060") {
                return Ok(ServiceStatus::NotInstalled);
            }
            return Ok(ServiceStatus::Unknown(text.trim().to_string()));
        }
        if text.contains("RUNNING") {
            Ok(ServiceStatus::Running)
        } else if text.contains("STOPPED") {
            Ok(ServiceStatus::Stopped)
        } else {
            Ok(ServiceStatus::Unknown(text.trim().to_string()))
        }
    }
    #[cfg(target_os = "linux")]
    {
        if !Path::new("/etc/systemd/system/veloxdesk.service").exists() {
            return Ok(ServiceStatus::NotInstalled);
        }
        let out = std::process::Command::new("systemctl")
            .args(["is-active", "veloxdesk.service"])
            .output();
        match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                match s.as_str() {
                    "active" => Ok(ServiceStatus::Running),
                    "inactive" | "failed" => Ok(ServiceStatus::Stopped),
                    other => Ok(ServiceStatus::Unknown(other.to_string())),
                }
            }
            Err(_) => Ok(ServiceStatus::Unknown("systemctl unavailable".into())),
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Ok(ServiceStatus::NotInstalled)
    }
}

/// Await until a shutdown signal transitions to `true` (or its sender is dropped).
pub async fn wait_for_shutdown(mut rx: watch::Receiver<bool>) {
    while !*rx.borrow() {
        if rx.changed().await.is_err() {
            break;
        }
    }
}

/// Run a host entry point under the OS service manager (Windows SCM) when launched by
/// `services.msc` / `sc start`, or transparently in the foreground console (stopping cleanly
/// on Ctrl+C) when launched interactively from a terminal.
///
/// `run_fn` receives a [`watch::Receiver<bool>`] shutdown signal that becomes `true` when
/// Windows SCM delivers `SERVICE_CONTROL_STOP` / `SERVICE_CONTROL_SHUTDOWN` or when the user
/// presses Ctrl+C in foreground console mode.
pub fn run_service_or_console<F>(run_fn: F) -> Result<()>
where
    F: FnOnce(watch::Receiver<bool>) -> Result<()> + Send + 'static,
{
    #[cfg(windows)]
    {
        win_scm::run_service_or_console(run_fn)
    }
    #[cfg(not(windows))]
    {
        let (_stop_tx, stop_rx) = watch::channel(false);
        run_fn(stop_rx)
    }
}

#[cfg(windows)]
mod win_scm {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::Mutex;

    use tokio::sync::watch;
    use veloxdesk_core_traits::{CoreError, Result};
    use windows_sys::Win32::Foundation::{
        ERROR_CALL_NOT_IMPLEMENTED, ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, GetLastError, NO_ERROR,
    };
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
    };
    use windows_sys::Win32::System::Services::{
        RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE,
        SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS,
        SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING, SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS,
        SetServiceStatus, StartServiceCtrlDispatcherW,
    };

    use super::SERVICE_NAME;

    type ServiceTask = Box<dyn FnOnce(watch::Receiver<bool>) -> Result<()> + Send + 'static>;

    struct ScmSlot {
        run_fn: Option<ServiceTask>,
        stop_tx: Option<watch::Sender<bool>>,
        stop_rx: Option<watch::Receiver<bool>>,
        status_handle: usize,
        result: Option<Result<()>>,
    }

    static SCM_STATE: Mutex<Option<ScmSlot>> = Mutex::new(None);
    static CONSOLE_STOP: Mutex<Option<watch::Sender<bool>>> = Mutex::new(None);

    fn wide_service_name() -> Vec<u16> {
        SERVICE_NAME.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn report_status(handle: SERVICE_STATUS_HANDLE, state: u32, win32_exit_code: u32, wait_hint: u32) {
        if handle.is_null() {
            return;
        }
        let controls_accepted = if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
        } else {
            0
        };
        let status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: state,
            dwControlsAccepted: controls_accepted,
            dwWin32ExitCode: win32_exit_code,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: u32::from(matches!(state, SERVICE_START_PENDING | SERVICE_STOP_PENDING)),
            dwWaitHint: wait_hint,
        };
        // SAFETY: `handle` is a valid SERVICE_STATUS_HANDLE returned by RegisterServiceCtrlHandlerExW.
        unsafe {
            let _ = SetServiceStatus(handle, &raw const status);
        }
    }

    unsafe extern "system" fn service_ctrl_handler(
        control: u32,
        _event_type: u32,
        _event_data: *mut c_void,
        _context: *mut c_void,
    ) -> u32 {
        match control {
            SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
                let (handle, stop_tx) = {
                    let g = SCM_STATE.lock().unwrap_or_else(|e| e.into_inner());
                    match g.as_ref() {
                        Some(slot) => (slot.status_handle as SERVICE_STATUS_HANDLE, slot.stop_tx.clone()),
                        None => (ptr::null_mut(), None),
                    }
                };
                report_status(handle, SERVICE_STOP_PENDING, NO_ERROR, 5_000);
                if let Some(tx) = stop_tx {
                    let _ = tx.send(true);
                }
                NO_ERROR
            }
            SERVICE_CONTROL_INTERROGATE => NO_ERROR,
            _ => ERROR_CALL_NOT_IMPLEMENTED,
        }
    }

    unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
        let name = wide_service_name();
        // SAFETY: `name` is NUL-terminated UTF-16; `service_ctrl_handler` matches LPHANDLER_FUNCTION_EX.
        let handle = unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(service_ctrl_handler), ptr::null()) };
        if handle.is_null() {
            let err = unsafe { GetLastError() };
            let mut g = SCM_STATE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(slot) = g.as_mut() {
                slot.result = Some(Err(CoreError::platform(format!(
                    "RegisterServiceCtrlHandlerExW failed (Win32 error {err})"
                ))));
            }
            return;
        }

        let (run_fn, stop_rx) = {
            let mut g = SCM_STATE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(slot) = g.as_mut() {
                slot.status_handle = handle as usize;
                (slot.run_fn.take(), slot.stop_rx.take())
            } else {
                (None, None)
            }
        };

        report_status(handle, SERVICE_START_PENDING, NO_ERROR, 5_000);
        report_status(handle, SERVICE_RUNNING, NO_ERROR, 0);

        let res = match (run_fn, stop_rx) {
            (Some(f), Some(rx)) => f(rx),
            _ => Err(CoreError::platform("Windows SCM service_main invoked without a task")),
        };
        let exit_code = if res.is_ok() { NO_ERROR } else { 1 };

        {
            let mut g = SCM_STATE.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(slot) = g.as_mut() {
                slot.result = Some(res);
                slot.status_handle = 0;
            }
        }
        report_status(handle, SERVICE_STOPPED, exit_code, 0);
    }

    unsafe extern "system" fn console_ctrl_handler(ctrl_type: u32) -> i32 {
        match ctrl_type {
            CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT | CTRL_SHUTDOWN_EVENT => {
                let tx = CONSOLE_STOP.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(tx) = tx {
                    let _ = tx.send(true);
                    return 1;
                }
                0
            }
            _ => 0,
        }
    }

    struct ConsoleHandlerGuard;

    impl ConsoleHandlerGuard {
        fn install(stop_tx: watch::Sender<bool>) -> Self {
            *CONSOLE_STOP.lock().unwrap_or_else(|e| e.into_inner()) = Some(stop_tx);
            // SAFETY: `console_ctrl_handler` matches PHANDLER_ROUTINE; removed on Drop.
            unsafe {
                let _ = SetConsoleCtrlHandler(Some(console_ctrl_handler), 1);
            }
            Self
        }
    }

    impl Drop for ConsoleHandlerGuard {
        fn drop(&mut self) {
            // SAFETY: unregisters the previously registered handler.
            unsafe {
                let _ = SetConsoleCtrlHandler(Some(console_ctrl_handler), 0);
            }
            *CONSOLE_STOP.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    pub(super) fn run_service_or_console<F>(run_fn: F) -> Result<()>
    where
        F: FnOnce(watch::Receiver<bool>) -> Result<()> + Send + 'static,
    {
        let (stop_tx, stop_rx) = watch::channel(false);
        {
            let mut g = SCM_STATE.lock().unwrap_or_else(|e| e.into_inner());
            if g.is_some() {
                return Err(CoreError::invalid("run_service_or_console is already active in this process"));
            }
            *g = Some(ScmSlot {
                run_fn: Some(Box::new(run_fn)),
                stop_tx: Some(stop_tx),
                stop_rx: Some(stop_rx),
                status_handle: 0,
                result: None,
            });
        }

        let mut name = wide_service_name();
        let table = [
            SERVICE_TABLE_ENTRYW { lpServiceName: name.as_mut_ptr(), lpServiceProc: Some(service_main) },
            SERVICE_TABLE_ENTRYW { lpServiceName: ptr::null_mut(), lpServiceProc: None },
        ];

        // SAFETY: `table` is properly null-terminated and `name` outlives the blocking call.
        let started = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
        if started != 0 {
            let slot = SCM_STATE.lock().unwrap_or_else(|e| e.into_inner()).take();
            return slot.and_then(|s| s.result).unwrap_or(Ok(()));
        }

        let err = unsafe { GetLastError() };
        let slot = SCM_STATE.lock().unwrap_or_else(|e| e.into_inner()).take();
        if err == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT {
            let Some(mut slot) = slot else {
                return Err(CoreError::platform("SCM fallback missing task slot"));
            };
            let (Some(f), Some(tx), Some(rx)) = (slot.run_fn.take(), slot.stop_tx.take(), slot.stop_rx.take()) else {
                return Err(CoreError::platform("SCM fallback task already consumed"));
            };
            tracing::debug!("not launched by Windows SCM; running in foreground console mode");
            let _guard = ConsoleHandlerGuard::install(tx);
            return f(rx);
        }

        Err(CoreError::platform(format!("StartServiceCtrlDispatcherW failed (Win32 error {err})")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_and_unit_generation() {
        let d = detect_desktop();
        assert!(!d.to_string().is_empty());

        let unit = systemd_unit(Path::new("/usr/local/bin/veloxdesk"), &["--portable".into()]);
        assert!(unit.contains("ExecStart=\"/usr/local/bin/veloxdesk\" --host --portable"));
        assert!(unit.contains("Restart=on-failure"));

        let win_cmd = windows_service_command_line(Path::new(r"C:\Program Files\VeloxDesk\veloxdesk.exe"), &[]);
        assert!(win_cmd.contains("--host --service-run"));
    }

    #[test]
    fn service_status_queries_cleanly() {
        let st = service_status().expect("service_status should not panic");
        assert!(!st.to_string().is_empty());
    }

    #[test]
    fn run_service_or_console_falls_back_in_console() {
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ran_c = ran.clone();
        run_service_or_console(move |shutdown_rx| {
            assert!(!*shutdown_rx.borrow(), "shutdown signal should start false");
            ran_c.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .expect("console fallback should succeed");
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
    }
}
