//! Remote host power management (`SystemPowerRequest` / `SystemPowerResult`, v0.5.0).
//!
//! Provides the [`PowerExecutor`] abstraction with a production OS implementation
//! ([`SystemPowerExecutor`]) and a deterministic test double ([`SimulatedPowerExecutor`]).

use std::sync::Mutex;

use veloxdesk_core_traits::{CoreError, Result};
use veloxdesk_protocol::control::SystemPowerAction;

/// Executes host OS power actions requested by an authorized remote viewer.
pub trait PowerExecutor: Send + Sync {
    fn execute(&self, action: SystemPowerAction, reason: &str, delay_secs: u32) -> Result<()>;
}

/// Production OS power executor (Windows Win32 / Linux systemd-logind).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemPowerExecutor;

impl SystemPowerExecutor {
    pub fn new() -> Self {
        Self
    }
}

impl PowerExecutor for SystemPowerExecutor {
    fn execute(&self, action: SystemPowerAction, reason: &str, delay_secs: u32) -> Result<()> {
        #[cfg(windows)]
        {
            win::execute(action, reason, delay_secs)
        }
        #[cfg(target_os = "linux")]
        {
            linux::execute(action, reason, delay_secs)
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            let _ = (action, reason, delay_secs);
            Err(CoreError::unsupported("remote power management is not implemented on this OS"))
        }
    }
}

/// Recorded invocation captured by [`SimulatedPowerExecutor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedPowerAction {
    pub action: SystemPowerAction,
    pub reason: String,
    pub delay_secs: u32,
}

/// Deterministic in-memory [`PowerExecutor`] for unit and integration tests.
#[derive(Default)]
pub struct SimulatedPowerExecutor {
    calls: Mutex<Vec<RecordedPowerAction>>,
    fail_error: Mutex<Option<String>>,
}

impl SimulatedPowerExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn calls(&self) -> Vec<RecordedPowerAction> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn clear(&self) {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Force subsequent [`PowerExecutor::execute`] calls to fail with `err` (after recording).
    pub fn set_error(&self, err: Option<String>) {
        *self.fail_error.lock().unwrap_or_else(|e| e.into_inner()) = err;
    }
}

impl PowerExecutor for SimulatedPowerExecutor {
    fn execute(&self, action: SystemPowerAction, reason: &str, delay_secs: u32) -> Result<()> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(RecordedPowerAction {
            action,
            reason: reason.to_string(),
            delay_secs,
        });
        if let Some(msg) = self.fail_error.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            return Err(CoreError::platform(msg));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod win {
    use std::ptr;

    use veloxdesk_core_traits::{CoreError, Result};
    use veloxdesk_protocol::control::SystemPowerAction;
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID,
    };
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
        SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Power::SetSuspendState;
    use windows_sys::Win32::System::Shutdown::{
        InitiateSystemShutdownExW, LockWorkStation, SHTDN_REASON_FLAG_PLANNED,
        SHTDN_REASON_MAJOR_OTHER, SHTDN_REASON_MINOR_OTHER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    fn enable_shutdown_privilege() -> Result<()> {
        // SAFETY: Win32 process token privilege adjustment on current process handle.
        unsafe {
            let mut token: HANDLE = ptr::null_mut();
            if OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
                &raw mut token,
            ) == 0
            {
                let err = GetLastError();
                return Err(CoreError::platform(format!(
                    "OpenProcessToken failed (Win32 error {err})"
                )));
            }

            let res = (|| {
                let mut luid = LUID { LowPart: 0, HighPart: 0 };
                if LookupPrivilegeValueW(ptr::null(), SE_SHUTDOWN_NAME, &raw mut luid) == 0 {
                    let err = GetLastError();
                    return Err(CoreError::platform(format!(
                        "LookupPrivilegeValueW(SE_SHUTDOWN_NAME) failed (Win32 error {err})"
                    )));
                }
                let tp = TOKEN_PRIVILEGES {
                    PrivilegeCount: 1,
                    Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
                };
                if AdjustTokenPrivileges(token, 0, &raw const tp, 0, ptr::null_mut(), ptr::null_mut()) == 0 {
                    let err = GetLastError();
                    return Err(CoreError::platform(format!(
                        "AdjustTokenPrivileges(SE_SHUTDOWN_NAME) failed (Win32 error {err})"
                    )));
                }
                let err = GetLastError();
                if err == ERROR_NOT_ALL_ASSIGNED {
                    return Err(CoreError::AccessDenied(
                        "SE_SHUTDOWN_NAME privilege not granted to process".into(),
                    ));
                }
                Ok(())
            })();

            let _ = CloseHandle(token);
            res
        }
    }

    fn initiate_reboot(reason: &str, delay_secs: u32) -> Result<()> {
        enable_shutdown_privilege()?;
        let msg_text = if reason.trim().is_empty() { "VeloxDesk remote reboot" } else { reason };
        let wide_msg: Vec<u16> = msg_text.encode_utf16().chain(std::iter::once(0)).collect();
        let timeout = delay_secs.max(1);
        let dw_reason = SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_MINOR_OTHER | SHTDN_REASON_FLAG_PLANNED;

        // SAFETY: `wide_msg` is NUL-terminated UTF-16 and lives for the duration of the call.
        let ok = unsafe {
            InitiateSystemShutdownExW(
                ptr::null(),
                wide_msg.as_ptr(),
                timeout,
                1, // bForceAppsClosed = true
                1, // bRebootAfterShutdown = true
                dw_reason,
            )
        };
        if ok == 0 {
            let err = unsafe { GetLastError() };
            if err == ERROR_ACCESS_DENIED {
                return Err(CoreError::AccessDenied(
                    "InitiateSystemShutdownExW access denied (elevation required)".into(),
                ));
            }
            return Err(CoreError::platform(format!(
                "InitiateSystemShutdownExW failed (Win32 error {err})"
            )));
        }
        Ok(())
    }

    pub(super) fn execute(action: SystemPowerAction, reason: &str, delay_secs: u32) -> Result<()> {
        match action {
            SystemPowerAction::Reboot => initiate_reboot(reason, delay_secs),
            SystemPowerAction::RebootSafeMode => {
                let out = std::process::Command::new("bcdedit")
                    .args(["/set", "{current}", "safeboot", "network"])
                    .output()
                    .map_err(CoreError::Io)?;
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    let detail = if !stderr.is_empty() { stderr } else { stdout };
                    return Err(CoreError::AccessDenied(
                        format!(
                            "bcdedit /set {{current}} safeboot network failed (Administrator privileges required; exit {:?}): {detail}",
                            out.status.code()
                        )
                        .into(),
                    ));
                }
                initiate_reboot(reason, delay_secs)
            }
            SystemPowerAction::LockScreen => {
                // SAFETY: LockWorkStation takes no arguments.
                let ok = unsafe { LockWorkStation() };
                if ok == 0 {
                    let err = unsafe { GetLastError() };
                    return Err(CoreError::platform(format!(
                        "LockWorkStation failed (Win32 error {err})"
                    )));
                }
                Ok(())
            }
            SystemPowerAction::Sleep => {
                // SAFETY: SetSuspendState(bHibernate=false, bForce=false, bWakeupEventsDisabled=false) enters S3/Modern Standby.
                let ok = unsafe { SetSuspendState(false, false, false) };
                if !ok {
                    let err = unsafe { GetLastError() };
                    return Err(CoreError::platform(format!(
                        "SetSuspendState failed (Win32 error {err})"
                    )));
                }
                Ok(())
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use veloxdesk_core_traits::{CoreError, Result};
    use veloxdesk_protocol::control::SystemPowerAction;

    fn run_cmd(program: &str, args: &[&str]) -> Result<()> {
        let out = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(CoreError::Io)?;
        if out.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(CoreError::platform(format!(
                "{program} {} failed (exit {:?}): {stderr}",
                args.join(" "),
                out.status.code()
            )))
        }
    }

    pub(super) fn execute(action: SystemPowerAction, _reason: &str, _delay_secs: u32) -> Result<()> {
        match action {
            SystemPowerAction::Reboot => run_cmd("systemctl", &["reboot"]),
            SystemPowerAction::RebootSafeMode => {
                Err(CoreError::unsupported("RebootSafeMode is not supported on Linux"))
            }
            SystemPowerAction::LockScreen => run_cmd("loginctl", &["lock-session"]),
            SystemPowerAction::Sleep => run_cmd("systemctl", &["suspend"]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_power_executor_records_and_errors() {
        let sim = SimulatedPowerExecutor::new();
        assert!(sim.calls().is_empty());

        sim.execute(SystemPowerAction::LockScreen, "idle", 0).unwrap();
        sim.execute(SystemPowerAction::Reboot, "maintenance", 5).unwrap();
        assert_eq!(
            sim.calls(),
            vec![
                RecordedPowerAction {
                    action: SystemPowerAction::LockScreen,
                    reason: "idle".into(),
                    delay_secs: 0,
                },
                RecordedPowerAction {
                    action: SystemPowerAction::Reboot,
                    reason: "maintenance".into(),
                    delay_secs: 5,
                },
            ]
        );

        sim.set_error(Some("simulated elevation denied".into()));
        let err = sim.execute(SystemPowerAction::RebootSafeMode, "diag", 3).unwrap_err();
        assert!(err.to_string().contains("simulated elevation denied"));
        assert_eq!(sim.calls().len(), 3);

        sim.clear();
        assert!(sim.calls().is_empty());
    }
}
