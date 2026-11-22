//! Virtual display management (Windows IDD/VDD discovery + Linux XRandR/DRM + simulated manager).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use veloxdesk_core_traits::{CoreError, Result};

/// Specification for creating a virtual monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDisplaySpec {
    pub width: u32,
    pub height: u32,
    pub refresh_hz: u32,
    pub name: String,
}

impl Default for VirtualDisplaySpec {
    fn default() -> Self {
        Self { width: 1920, height: 1080, refresh_hz: 60, name: "VeloxDesk Virtual".into() }
    }
}

/// Active virtual display handle and configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtualDisplayInfo {
    pub id: u32,
    pub spec: VirtualDisplaySpec,
}

/// Manages headless / secondary virtual monitors.
pub trait VirtualDisplayManager: Send + Sync {
    fn backend(&self) -> &'static str;
    fn is_supported(&self) -> bool;
    fn create(&self, spec: VirtualDisplaySpec) -> Result<VirtualDisplayInfo>;
    fn remove(&self, id: u32) -> Result<()>;
    fn list(&self) -> Vec<VirtualDisplayInfo>;
}

pub(crate) fn validate_spec(spec: &VirtualDisplaySpec) -> Result<()> {
    if spec.width < 320 || spec.height < 240 || spec.width > 7680 || spec.height > 4320 {
        return Err(CoreError::invalid(format!(
            "virtual display resolution {}x{} out of bounds (320x240..=7680x4320)",
            spec.width, spec.height
        )));
    }
    if spec.refresh_hz < 24 || spec.refresh_hz > 360 {
        return Err(CoreError::invalid(format!(
            "virtual display refresh {} Hz out of bounds (24..=360)",
            spec.refresh_hz
        )));
    }
    Ok(())
}

/// In-memory virtual display manager for deterministic tests and headless simulation.
pub struct SimulatedVirtualDisplayManager {
    next_id: AtomicU32,
    displays: Mutex<Vec<VirtualDisplayInfo>>,
}

impl SimulatedVirtualDisplayManager {
    pub fn new() -> Self {
        Self { next_id: AtomicU32::new(1), displays: Mutex::new(Vec::new()) }
    }
}

impl Default for SimulatedVirtualDisplayManager {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualDisplayManager for SimulatedVirtualDisplayManager {
    fn backend(&self) -> &'static str {
        "simulated-vdd"
    }

    fn is_supported(&self) -> bool {
        true
    }

    fn create(&self, spec: VirtualDisplaySpec) -> Result<VirtualDisplayInfo> {
        validate_spec(&spec)?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let info = VirtualDisplayInfo { id, spec };
        self.displays.lock().expect("poisoned").push(info.clone());
        Ok(info)
    }

    fn remove(&self, id: u32) -> Result<()> {
        let mut guard = self.displays.lock().expect("poisoned");
        let before = guard.len();
        guard.retain(|d| d.id != id);
        if guard.len() == before {
            return Err(CoreError::invalid(format!("virtual display id {id} not found")));
        }
        Ok(())
    }

    fn list(&self) -> Vec<VirtualDisplayInfo> {
        self.displays.lock().expect("poisoned").clone()
    }
}

#[cfg(windows)]
pub mod win {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use veloxdesk_core_traits::{CoreError, Result};
    use windows::Win32::Graphics::Gdi::{
        CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISPLAY_DEVICEW, DM_DISPLAYFREQUENCY, DM_PELSWIDTH,
        DM_PELSHEIGHT, DISP_CHANGE_SUCCESSFUL, EnumDisplayDevicesW,
    };
    use windows::core::{HSTRING, PCWSTR};

    use super::{VirtualDisplayInfo, VirtualDisplayManager, VirtualDisplaySpec, validate_spec};

    fn wide_str(buf: &[u16]) -> String {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
    }

    fn is_vdd_adapter_name(s: &str) -> bool {
        let lower = s.to_ascii_lowercase();
        lower.contains("virtual display")
            || lower.contains("iddsample")
            || lower.contains("mttvdd")
            || lower.contains("parsec virtual")
            || lower.contains("sudomaker")
            || lower.contains("indirect display")
    }

    /// Discover installed Windows Indirect Display Driver (IDD/VDD) device names via GDI.
    pub fn discover_vdd_devices() -> Vec<String> {
        let mut found = Vec::new();
        let mut idx = 0u32;
        loop {
            let mut dd = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
            // SAFETY: valid DISPLAY_DEVICEW with cb initialized.
            let ok = unsafe { EnumDisplayDevicesW(PCWSTR::null(), idx, &mut dd, 0) }.as_bool();
            if !ok {
                break;
            }
            idx += 1;
            let desc = wide_str(&dd.DeviceString);
            let name = wide_str(&dd.DeviceName);
            if !name.is_empty() && is_vdd_adapter_name(&desc) {
                found.push(name);
            }
        }
        found
    }

    /// Windows Indirect Display Driver (IDD/VDD) virtual monitor manager with graceful fallback.
    pub struct WindowsVirtualDisplayManager {
        vdd_devices: Vec<String>,
        next_id: AtomicU32,
        active: Mutex<Vec<(VirtualDisplayInfo, String)>>,
    }

    impl WindowsVirtualDisplayManager {
        pub fn new() -> Self {
            let vdd_devices = discover_vdd_devices();
            tracing::debug!(count = vdd_devices.len(), ?vdd_devices, "probed Windows virtual display drivers");
            Self { vdd_devices, next_id: AtomicU32::new(1), active: Mutex::new(Vec::new()) }
        }
    }

    impl Default for WindowsVirtualDisplayManager {
        fn default() -> Self {
            Self::new()
        }
    }

    impl VirtualDisplayManager for WindowsVirtualDisplayManager {
        fn backend(&self) -> &'static str {
            if self.vdd_devices.is_empty() { "windows-vdd-unavailable" } else { "windows-idd-vdd" }
        }

        fn is_supported(&self) -> bool {
            !self.vdd_devices.is_empty()
        }

        fn create(&self, spec: VirtualDisplaySpec) -> Result<VirtualDisplayInfo> {
            validate_spec(&spec)?;
            if self.vdd_devices.is_empty() {
                return Err(CoreError::unsupported(
                    "no Windows Indirect Display Driver (IDD/VDD) detected; install a VDD driver to create virtual displays",
                ));
            }
            let mut guard = self.active.lock().expect("poisoned");
            let dev_name = self
                .vdd_devices
                .iter()
                .find(|d| !guard.iter().any(|(_, used)| used == *d))
                .ok_or_else(|| CoreError::unsupported("all detected Windows virtual display outputs are already in use"))?
                .clone();

            let dm = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                dmFields: DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY,
                dmPelsWidth: spec.width,
                dmPelsHeight: spec.height,
                dmDisplayFrequency: spec.refresh_hz,
                ..Default::default()
            };
            let wide_dev = HSTRING::from(dev_name.as_str());
            // SAFETY: valid device name and DEVMODEW with dmSize set.
            let res = unsafe { ChangeDisplaySettingsExW(&wide_dev, Some(&dm), None, CDS_UPDATEREGISTRY, None) };
            if res != DISP_CHANGE_SUCCESSFUL {
                return Err(CoreError::platform(format!(
                    "ChangeDisplaySettingsExW({dev_name}, {}x{}@{}Hz) failed with code {}",
                    spec.width, spec.height, spec.refresh_hz, res.0
                )));
            }
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let info = VirtualDisplayInfo { id, spec };
            guard.push((info.clone(), dev_name));
            Ok(info)
        }

        fn remove(&self, id: u32) -> Result<()> {
            let mut guard = self.active.lock().expect("poisoned");
            let pos = guard
                .iter()
                .position(|(info, _)| info.id == id)
                .ok_or_else(|| CoreError::invalid(format!("virtual display id {id} not found")))?;
            let (_, dev_name) = guard.remove(pos);
            let wide_dev = HSTRING::from(dev_name.as_str());
            // Reset detached mode (0x0) when removing virtual display.
            let dm = DEVMODEW {
                dmSize: std::mem::size_of::<DEVMODEW>() as u16,
                dmFields: DM_PELSWIDTH | DM_PELSHEIGHT,
                dmPelsWidth: 0,
                dmPelsHeight: 0,
                ..Default::default()
            };
            // SAFETY: valid device name and DEVMODEW.
            let _ = unsafe { ChangeDisplaySettingsExW(&wide_dev, Some(&dm), None, CDS_UPDATEREGISTRY, None) };
            Ok(())
        }

        fn list(&self) -> Vec<VirtualDisplayInfo> {
            self.active.lock().expect("poisoned").iter().map(|(i, _)| i.clone()).collect()
        }
    }
}

#[cfg(target_os = "linux")]
pub mod linux {
    use veloxdesk_core_traits::{CoreError, Result};

    use super::{VirtualDisplayInfo, VirtualDisplayManager, VirtualDisplaySpec, validate_spec};

    /// Linux XRandR / DRM virtual display manager interface.
    #[derive(Default)]
    pub struct LinuxVirtualDisplayManager;

    impl LinuxVirtualDisplayManager {
        pub fn new() -> Self {
            Self
        }
    }

    impl VirtualDisplayManager for LinuxVirtualDisplayManager {
        fn backend(&self) -> &'static str {
            "linux-xrandr-drm"
        }

        fn is_supported(&self) -> bool {
            false
        }

        fn create(&self, spec: VirtualDisplaySpec) -> Result<VirtualDisplayInfo> {
            validate_spec(&spec)?;
            Err(CoreError::unsupported(
                "Linux XRandR/DRM virtual display creation requires a configured dummy/VKMS output",
            ))
        }

        fn remove(&self, _id: u32) -> Result<()> {
            Err(CoreError::unsupported("no active Linux virtual displays"))
        }

        fn list(&self) -> Vec<VirtualDisplayInfo> {
            Vec::new()
        }
    }
}
