//! System service / daemon helpers (Windows SCM + Linux systemd) and active-desktop
//! detection (interactive desktop vs UAC / Winlogon secure desktop).

use std::fmt;
use std::path::Path;

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
}
