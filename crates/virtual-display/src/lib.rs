//! # veloxdesk-virtual-display
//!
//! Virtual monitor management (Windows IDD/VDD, Linux XRandR/DRM) and RAII Privacy Mode
//! (physical display blanking + local input lock that always restores on drop).
//!
//! See `docs/SWARM.md` (M4 cross-crate APIs) for the binding public contract.

pub mod display;
pub mod privacy;

use std::sync::Arc;

pub use display::{SimulatedVirtualDisplayManager, VirtualDisplayInfo, VirtualDisplayManager, VirtualDisplaySpec};
pub use privacy::{PrivacyController, PrivacyGuard, SimulatedPrivacyController};

/// Create the platform [`VirtualDisplayManager`].
pub fn create_virtual_display_manager() -> Box<dyn VirtualDisplayManager> {
    #[cfg(windows)]
    {
        Box::new(display::win::WindowsVirtualDisplayManager::new())
    }
    #[cfg(target_os = "linux")]
    {
        Box::new(display::linux::LinuxVirtualDisplayManager::new())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Box::new(SimulatedVirtualDisplayManager::new())
    }
}

/// Create the platform [`PrivacyController`].
pub fn create_privacy_controller() -> Arc<dyn PrivacyController> {
    #[cfg(windows)]
    {
        Arc::new(privacy::win::WindowsPrivacyController::new())
    }
    #[cfg(target_os = "linux")]
    {
        Arc::new(privacy::linux::LinuxPrivacyController::new())
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Arc::new(SimulatedPrivacyController::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simulated_virtual_display_lifecycle_and_validation() {
        let mgr = SimulatedVirtualDisplayManager::new();
        assert!(mgr.is_supported());
        assert!(mgr.list().is_empty());

        // Reject invalid dimensions / refresh.
        assert!(
            mgr.create(VirtualDisplaySpec { width: 100, height: 100, refresh_hz: 60, name: "bad".into() }).is_err()
        );
        assert!(
            mgr.create(VirtualDisplaySpec { width: 1920, height: 1080, refresh_hz: 5, name: "bad".into() }).is_err()
        );

        let d1 = mgr.create(VirtualDisplaySpec::default()).expect("create d1");
        let d2 = mgr
            .create(VirtualDisplaySpec { width: 2560, height: 1440, refresh_hz: 144, name: "Gaming".into() })
            .expect("create d2");
        assert_ne!(d1.id, d2.id);
        assert_eq!(mgr.list().len(), 2);

        mgr.remove(d1.id).expect("remove d1");
        assert_eq!(mgr.list(), vec![d2.clone()]);
        assert!(mgr.remove(d1.id).is_err());
        mgr.remove(d2.id).expect("remove d2");
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn privacy_guard_always_restores_on_drop_and_panic() {
        let ctrl = Arc::new(SimulatedPrivacyController::new());
        assert!(!ctrl.is_enabled());

        // Normal drop restores automatically.
        {
            let guard = PrivacyGuard::activate(ctrl.clone()).expect("activate");
            assert!(guard.is_active());
            assert!(ctrl.is_enabled());
            assert_eq!(ctrl.enable_count(), 1);
            assert_eq!(ctrl.disable_count(), 0);
        }
        assert!(!ctrl.is_enabled());
        assert_eq!(ctrl.disable_count(), 1);

        // Explicit disable does not double-disable on drop.
        let guard = PrivacyGuard::activate(ctrl.clone()).expect("activate 2");
        guard.disable().expect("explicit disable");
        assert!(!ctrl.is_enabled());
        assert_eq!(ctrl.enable_count(), 2);
        assert_eq!(ctrl.disable_count(), 2);

        // Unwind/panic restores automatically.
        let ctrl_panic = ctrl.clone();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = PrivacyGuard::activate(ctrl_panic).expect("activate 3");
            panic!("simulated session crash");
        }));
        assert!(res.is_err());
        assert!(!ctrl.is_enabled());
        assert_eq!(ctrl.enable_count(), 3);
        assert_eq!(ctrl.disable_count(), 3);

        // Failed enable returns error without leaving controller enabled.
        ctrl.set_fail_enable(true);
        assert!(PrivacyGuard::activate(ctrl.clone()).is_err());
        assert!(!ctrl.is_enabled());
    }

    #[test]
    fn platform_factories_probe_cleanly() {
        let mgr = create_virtual_display_manager();
        eprintln!("platform virtual display manager: backend={} supported={}", mgr.backend(), mgr.is_supported());
        assert!(mgr.list().is_empty());
        if !mgr.is_supported() {
            assert!(mgr.create(VirtualDisplaySpec::default()).is_err());
        }

        // Only verify factory construction & initial state (do not blank developer's monitor during tests!).
        let privacy = create_privacy_controller();
        assert!(!privacy.is_enabled());
    }
}
