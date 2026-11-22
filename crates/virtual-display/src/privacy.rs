//! Privacy mode controller (physical screen blanking + local input lock) with RAII [`PrivacyGuard`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use veloxdesk_core_traits::{CoreError, Result};

/// Controls host privacy mode (blanking physical monitors and blocking local input).
pub trait PrivacyController: Send + Sync {
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
    fn is_enabled(&self) -> bool;
}

/// RAII guard for an active privacy-mode session.
///
/// Guarantees [`PrivacyController::disable`] is invoked on [`Drop`] so a session disconnect
/// or panic never leaves the host screen blanked or local input locked.
pub struct PrivacyGuard {
    controller: Arc<dyn PrivacyController>,
    active: bool,
}

impl PrivacyGuard {
    /// Enable privacy mode and return a guard that restores normal state on drop.
    pub fn activate(controller: Arc<dyn PrivacyController>) -> Result<Self> {
        controller.enable()?;
        Ok(Self { controller, active: true })
    }

    pub fn is_active(&self) -> bool {
        self.active && self.controller.is_enabled()
    }

    /// Explicitly disable privacy mode and consume the guard.
    pub fn disable(mut self) -> Result<()> {
        if self.active {
            self.active = false;
            self.controller.disable()?;
        }
        Ok(())
    }
}

impl Drop for PrivacyGuard {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            if let Err(e) = self.controller.disable() {
                tracing::error!("PrivacyGuard::drop failed to restore display/input: {e}");
            }
        }
    }
}

/// Deterministic in-memory [`PrivacyController`] for unit and integration tests.
#[derive(Default)]
pub struct SimulatedPrivacyController {
    enabled: AtomicBool,
    enables: AtomicU64,
    disables: AtomicU64,
    fail_enable: AtomicBool,
}

impl SimulatedPrivacyController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enable_count(&self) -> u64 {
        self.enables.load(Ordering::Acquire)
    }

    pub fn disable_count(&self) -> u64 {
        self.disables.load(Ordering::Acquire)
    }

    pub fn set_fail_enable(&self, fail: bool) {
        self.fail_enable.store(fail, Ordering::Release);
    }
}

impl PrivacyController for SimulatedPrivacyController {
    fn enable(&self) -> Result<()> {
        if self.fail_enable.load(Ordering::Acquire) {
            return Err(CoreError::AccessDenied("simulated privacy mode denied".into()));
        }
        self.enables.fetch_add(1, Ordering::AcqRel);
        self.enabled.store(true, Ordering::Release);
        Ok(())
    }

    fn disable(&self) -> Result<()> {
        self.disables.fetch_add(1, Ordering::AcqRel);
        self.enabled.store(false, Ordering::Release);
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
}

#[cfg(windows)]
pub mod win {
    use std::sync::atomic::{AtomicBool, Ordering};

    use veloxdesk_core_traits::Result;
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::BlockInput;
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SC_MONITORPOWER, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SYSCOMMAND,
    };

    use super::PrivacyController;

    const MONITOR_OFF: isize = 2;
    const MONITOR_ON: isize = -1;

    /// Windows privacy mode controller: powers down physical monitors (`SC_MONITORPOWER`) and
    /// blocks local mouse/keyboard input (`BlockInput`, degrading gracefully if non-elevated).
    #[derive(Default)]
    pub struct WindowsPrivacyController {
        enabled: AtomicBool,
        input_blocked: AtomicBool,
    }

    impl WindowsPrivacyController {
        pub fn new() -> Self {
            Self::default()
        }

        fn set_monitor_power(state: isize) {
            let mut result = 0usize;
            // SAFETY: non-blocking broadcast with 250 ms timeout so hung windows cannot stall us.
            let _ = unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    WM_SYSCOMMAND,
                    WPARAM(SC_MONITORPOWER as usize),
                    LPARAM(state),
                    SMTO_ABORTIFHUNG,
                    250,
                    Some(&mut result),
                )
            };
        }
    }

    impl PrivacyController for WindowsPrivacyController {
        fn enable(&self) -> Result<()> {
            // SAFETY: Win32 BlockInput requires elevation; log and degrade gracefully if non-elevated.
            match unsafe { BlockInput(true) } {
                Ok(()) => self.input_blocked.store(true, Ordering::Release),
                Err(e) => {
                    tracing::warn!("BlockInput(true) unavailable (likely non-elevated): {e}; blanking monitor only");
                    self.input_blocked.store(false, Ordering::Release);
                }
            }
            Self::set_monitor_power(MONITOR_OFF);
            self.enabled.store(true, Ordering::Release);
            Ok(())
        }

        fn disable(&self) -> Result<()> {
            if self.input_blocked.swap(false, Ordering::AcqRel) {
                // SAFETY: restore local input blocked by enable().
                let _ = unsafe { BlockInput(false) };
            }
            Self::set_monitor_power(MONITOR_ON);
            self.enabled.store(false, Ordering::Release);
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            self.enabled.load(Ordering::Acquire)
        }
    }

    impl Drop for WindowsPrivacyController {
        fn drop(&mut self) {
            if self.is_enabled() {
                let _ = self.disable();
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub mod linux {
    use std::sync::atomic::{AtomicBool, Ordering};

    use veloxdesk_core_traits::{CoreError, Result};

    use super::PrivacyController;

    /// Linux DPMS privacy controller interface.
    #[derive(Default)]
    pub struct LinuxPrivacyController {
        enabled: AtomicBool,
    }

    impl LinuxPrivacyController {
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl PrivacyController for LinuxPrivacyController {
        fn enable(&self) -> Result<()> {
            Err(CoreError::unsupported("Linux DPMS privacy mode requires an active X11/Wayland compositor hook"))
        }

        fn disable(&self) -> Result<()> {
            self.enabled.store(false, Ordering::Release);
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            self.enabled.load(Ordering::Acquire)
        }
    }
}
