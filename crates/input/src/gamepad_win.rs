//! Windows virtual Xbox 360 gamepads via Nefarius **ViGEmBus** (`SetupDi` + `DeviceIoControl`,
//! zero C/vcpkg dependencies). Supports up to 4 simultaneous virtual X360 controllers and
//! non-blocking overlapped rumble notifications (`IOCTL_XUSB_REQUEST_NOTIFICATION`).
//!
//! If ViGEmBus is not installed on the host, [`open_driver`] gracefully falls back to
//! [`NullGamepadDriver`] so keyboard and mouse injection remain fully operational.

use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};

use veloxdesk_core_traits::input::Rumble;
use veloxdesk_core_traits::{CoreError, Result};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::System::Threading::{CreateEventW, ResetEvent};
use windows_sys::core::GUID;

use crate::gamepad::{GamepadDriver, MAX_GAMEPADS, NullGamepadDriver, XInputReport, motor_u8_to_u16};

/// `GUID_DEVINTERFACE_BUSENUM_VIGEM` = `{96E42B22-F5E9-42F8-B043-ED0F932F014F}`.
const GUID_DEVINTERFACE_BUSENUM_VIGEM: GUID = GUID {
    data1: 0x96E4_2B22,
    data2: 0xF5E9,
    data3: 0x42F8,
    data4: [0xB0, 0x43, 0xED, 0x0F, 0x93, 0x2F, 0x01, 0x4F],
};

const FILE_DEVICE_BUSENUM: u32 = 0x0000_002A;
const METHOD_BUFFERED: u32 = 0;
const FILE_ANY_ACCESS: u32 = 0;
const FILE_READ_ACCESS: u32 = 1;
const FILE_WRITE_ACCESS: u32 = 2;

const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

/// ViGEmBus IOCTL codes (Nefarius `ViGEm/km/BusShared.h`).
pub(crate) const IOCTL_VIGEM_CHECK_VERSION: u32 =
    ctl_code(FILE_DEVICE_BUSENUM, 0x800, METHOD_BUFFERED, FILE_ANY_ACCESS);
pub(crate) const IOCTL_VIGEM_PLUGIN_TARGET: u32 =
    ctl_code(FILE_DEVICE_BUSENUM, 0x801, METHOD_BUFFERED, FILE_WRITE_ACCESS);
pub(crate) const IOCTL_VIGEM_UNPLUG_TARGET: u32 =
    ctl_code(FILE_DEVICE_BUSENUM, 0x802, METHOD_BUFFERED, FILE_WRITE_ACCESS);
pub(crate) const IOCTL_XUSB_REQUEST_NOTIFICATION: u32 =
    ctl_code(FILE_DEVICE_BUSENUM, 0x803, METHOD_BUFFERED, FILE_READ_ACCESS | FILE_WRITE_ACCESS);
pub(crate) const IOCTL_XUSB_SUBMIT_REPORT: u32 =
    ctl_code(FILE_DEVICE_BUSENUM, 0x804, METHOD_BUFFERED, FILE_WRITE_ACCESS);

const VIGEM_COMMON_VERSION: u32 = 0x0001;
const VIGEM_TARGET_XBOX360_WIRED: u32 = 0;
const XBOX360_VENDOR_ID: u16 = 0x045E;
const XBOX360_PRODUCT_ID: u16 = 0x028E;

#[repr(C)]
#[derive(Clone, Copy)]
struct VigemCheckVersion {
    size: u32,
    version: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VigemPluginTarget {
    size: u32,
    serial_no: u32,
    target_type: u32,
    vendor_id: u16,
    product_id: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VigemUnplugTarget {
    size: u32,
    serial_no: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct XusbSubmitReport {
    size: u32,
    serial_no: u32,
    report: XInputReport,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct XusbRequestNotification {
    size: u32,
    serial_no: u32,
    large_motor: u8,
    small_motor: u8,
    led_number: u8,
    _pad: u8,
}

struct PendingNotify {
    /// Boxed so pointers passed to overlapped `DeviceIoControl` remain stable in memory.
    req: Box<XusbRequestNotification>,
    overlapped: Box<OVERLAPPED>,
    event: HANDLE,
    armed: bool,
}

impl PendingNotify {
    fn new(serial_no: u32) -> Result<Self> {
        // SAFETY: manual-reset, initially unsignaled anonymous event.
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            return Err(CoreError::platform("CreateEventW failed for ViGEm rumble"));
        }
        let mut overlapped: Box<OVERLAPPED> = Box::new(unsafe { zeroed() });
        overlapped.hEvent = event;
        let req = Box::new(XusbRequestNotification {
            size: size_of::<XusbRequestNotification>() as u32,
            serial_no,
            ..Default::default()
        });
        Ok(Self { req, overlapped, event, armed: false })
    }

    fn arm(&mut self, bus: HANDLE) -> Option<Rumble> {
        // SAFETY: `self.req` and `self.overlapped` live on the heap for the entire lifetime of the I/O.
        unsafe {
            let _ = ResetEvent(self.event);
            self.req.large_motor = 0;
            self.req.small_motor = 0;
            self.req.led_number = 0;
            let mut returned = 0u32;
            let ok = DeviceIoControl(
                bus,
                IOCTL_XUSB_REQUEST_NOTIFICATION,
                (&raw const *self.req).cast(),
                size_of::<XusbRequestNotification>() as u32,
                (&raw mut *self.req).cast(),
                size_of::<XusbRequestNotification>() as u32,
                &raw mut returned,
                &raw mut *self.overlapped,
            );
            if ok != 0 {
                self.armed = false;
                return Some(Rumble {
                    pad_id: self.req.serial_no.saturating_sub(1),
                    low_freq: motor_u8_to_u16(self.req.large_motor) as u32,
                    high_freq: motor_u8_to_u16(self.req.small_motor) as u32,
                });
            }
            if GetLastError() == ERROR_IO_PENDING {
                self.armed = true;
            }
            None
        }
    }

    fn poll(&mut self, bus: HANDLE) -> Option<Rumble> {
        if !self.armed {
            return self.arm(bus);
        }
        let mut bytes = 0u32;
        // SAFETY: non-blocking check (`bWait = 0`) on the armed overlapped request.
        let ok = unsafe { GetOverlappedResult(bus, &raw const *self.overlapped, &raw mut bytes, 0) };
        if ok != 0 {
            self.armed = false;
            let r = Rumble {
                pad_id: self.req.serial_no.saturating_sub(1),
                low_freq: motor_u8_to_u16(self.req.large_motor) as u32,
                high_freq: motor_u8_to_u16(self.req.small_motor) as u32,
            };
            // Re-arm immediately for the next motor change; if it completes synchronously, prefer latest.
            return Some(self.arm(bus).unwrap_or(r));
        }
        // SAFETY: plain Win32 error query.
        let err = unsafe { GetLastError() };
        if err != ERROR_IO_INCOMPLETE && err != ERROR_IO_PENDING {
            self.armed = false;
        }
        None
    }

    fn cancel(&mut self, bus: HANDLE) {
        if self.armed {
            // SAFETY: cancel only this specific pending overlapped I/O and wait for completion
            // before freeing `self.req` / `self.overlapped`.
            unsafe {
                let _ = CancelIoEx(bus, &raw const *self.overlapped);
                let mut bytes = 0u32;
                let _ = GetOverlappedResult(bus, &raw const *self.overlapped, &raw mut bytes, 1);
            }
            self.armed = false;
        }
    }
}

impl Drop for PendingNotify {
    fn drop(&mut self) {
        if !self.event.is_null() {
            // SAFETY: owned Win32 event handle.
            unsafe {
                let _ = CloseHandle(self.event);
            }
        }
    }
}

struct PadTarget {
    serial_no: u32,
    notify: Option<PendingNotify>,
}

/// Live ViGEmBus handle managing up to [`MAX_GAMEPADS`] virtual Xbox 360 controllers.
pub struct ViGEmDriver {
    bus: HANDLE,
    sync_event: HANDLE,
    pads: [Option<PadTarget>; MAX_GAMEPADS],
}

// SAFETY: ViGEmDriver is owned exclusively by the injector thread; Win32 handles are process-global.
unsafe impl Send for ViGEmDriver {}

impl ViGEmDriver {
    /// Discover the ViGEmBus device interface via `SetupDi` and open it.
    /// Returns `Ok(None)` if ViGEmBus is not installed.
    pub fn try_open() -> Result<Option<Self>> {
        let Some(path_utf16) = find_vigem_device_path() else {
            return Ok(None);
        };
        // SAFETY: `path_utf16` is NUL-terminated UTF-16 from SetupDi.
        let bus = unsafe {
            CreateFileW(
                path_utf16.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if bus == INVALID_HANDLE_VALUE || bus.is_null() {
            tracing::debug!("ViGEmBus interface found but CreateFileW failed");
            return Ok(None);
        }
        // SAFETY: manual-reset event used to synchronize overlapped control IOCTLs.
        let sync_event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if sync_event.is_null() {
            unsafe {
                let _ = CloseHandle(bus);
            }
            return Err(CoreError::platform("CreateEventW failed"));
        }

        let drv = Self { bus, sync_event, pads: [None, None, None, None] };
        let check = VigemCheckVersion { size: size_of::<VigemCheckVersion>() as u32, version: VIGEM_COMMON_VERSION };
        if let Err(e) = drv.ioctl_sync(IOCTL_VIGEM_CHECK_VERSION, &check) {
            tracing::warn!("ViGEmBus version check failed (driver incompatible?): {e}");
            return Ok(None);
        }
        tracing::info!("ViGEmBus virtual gamepad driver connected");
        Ok(Some(drv))
    }

    fn ioctl_sync<T>(&self, code: u32, input: &T) -> Result<()> {
        // SAFETY: overlapped struct and input stay valid until GetOverlappedResult completes.
        unsafe {
            let _ = ResetEvent(self.sync_event);
            let mut ov: OVERLAPPED = zeroed();
            ov.hEvent = self.sync_event;
            let mut returned = 0u32;
            let ok = DeviceIoControl(
                self.bus,
                code,
                (input as *const T).cast(),
                size_of::<T>() as u32,
                null_mut(),
                0,
                &raw mut returned,
                &raw mut ov,
            );
            if ok != 0 {
                return Ok(());
            }
            if GetLastError() == ERROR_IO_PENDING {
                let res = GetOverlappedResult(self.bus, &raw const ov, &raw mut returned, 1);
                if res != 0 {
                    return Ok(());
                }
            }
            Err(CoreError::platform(format!("ViGEm DeviceIoControl 0x{code:08x} failed (err={})", GetLastError())))
        }
    }
}

impl Drop for ViGEmDriver {
    fn drop(&mut self) {
        for pad_id in 0..MAX_GAMEPADS as u32 {
            let _ = self.disconnect(pad_id);
        }
        // SAFETY: owned Win32 handles, all overlapped I/O cancelled above.
        unsafe {
            let _ = CloseHandle(self.sync_event);
            let _ = CloseHandle(self.bus);
        }
    }
}

impl GamepadDriver for ViGEmDriver {
    fn backend_name(&self) -> Option<&'static str> {
        Some("ViGEmBus")
    }

    fn max_gamepads(&self) -> u32 {
        MAX_GAMEPADS as u32
    }

    fn connect(&mut self, pad_id: u32) -> Result<()> {
        let idx = pad_id as usize;
        if idx >= MAX_GAMEPADS || self.pads[idx].is_some() {
            return Ok(());
        }
        let serial_no = pad_id + 1;
        let plugin = VigemPluginTarget {
            size: size_of::<VigemPluginTarget>() as u32,
            serial_no,
            target_type: VIGEM_TARGET_XBOX360_WIRED,
            vendor_id: XBOX360_VENDOR_ID,
            product_id: XBOX360_PRODUCT_ID,
        };
        self.ioctl_sync(IOCTL_VIGEM_PLUGIN_TARGET, &plugin)?;
        let mut notify = PendingNotify::new(serial_no).ok();
        if let Some(n) = notify.as_mut() {
            let _ = n.arm(self.bus);
        }
        self.pads[idx] = Some(PadTarget { serial_no, notify });
        tracing::info!(pad_id, serial_no, "ViGEmBus X360 controller plugged in");
        Ok(())
    }

    fn update(&mut self, pad_id: u32, report: XInputReport) -> Result<()> {
        let idx = pad_id as usize;
        let Some(target) = self.pads.get(idx).and_then(|s| s.as_ref()) else {
            return Ok(());
        };
        let submit = XusbSubmitReport {
            size: size_of::<XusbSubmitReport>() as u32,
            serial_no: target.serial_no,
            report,
        };
        self.ioctl_sync(IOCTL_XUSB_SUBMIT_REPORT, &submit)
    }

    fn disconnect(&mut self, pad_id: u32) -> Result<()> {
        let idx = pad_id as usize;
        let Some(mut target) = self.pads.get_mut(idx).and_then(Option::take) else {
            return Ok(());
        };
        if let Some(mut n) = target.notify.take() {
            n.cancel(self.bus);
        }
        let unplug = VigemUnplugTarget {
            size: size_of::<VigemUnplugTarget>() as u32,
            serial_no: target.serial_no,
        };
        let res = self.ioctl_sync(IOCTL_VIGEM_UNPLUG_TARGET, &unplug);
        tracing::info!(pad_id, serial_no = target.serial_no, "ViGEmBus X360 controller unplugged");
        res
    }

    fn poll_rumble(&mut self) -> Vec<Rumble> {
        let mut out = Vec::new();
        for pad in self.pads.iter_mut().flatten() {
            if let Some(n) = pad.notify.as_mut()
                && let Some(r) = n.poll(self.bus)
            {
                out.push(r);
            }
        }
        out
    }
}

/// Enumerate the first present ViGEmBus device interface path (NUL-terminated UTF-16).
fn find_vigem_device_path() -> Option<Vec<u16>> {
    // SAFETY: standard SetupDi device-interface enumeration with proper cleanup.
    unsafe {
        let guid = GUID_DEVINTERFACE_BUSENUM_VIGEM;
        let dev_info = SetupDiGetClassDevsW(
            &raw const guid,
            null(),
            null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        );
        if dev_info == INVALID_HANDLE_VALUE as isize || dev_info == 0 {
            return None;
        }
        let mut if_data: SP_DEVICE_INTERFACE_DATA = zeroed();
        if_data.cbSize = size_of::<SP_DEVICE_INTERFACE_DATA>() as u32;
        let found = SetupDiEnumDeviceInterfaces(
            dev_info,
            null(),
            &raw const guid,
            0,
            &raw mut if_data,
        );
        if found == 0 {
            let _ = SetupDiDestroyDeviceInfoList(dev_info);
            return None;
        }
        let mut required_bytes = 0u32;
        let _ = SetupDiGetDeviceInterfaceDetailW(dev_info, &raw const if_data, null_mut(), 0, &raw mut required_bytes, null_mut());
        if required_bytes < size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 {
            let _ = SetupDiDestroyDeviceInfoList(dev_info);
            return None;
        }
        let words = (required_bytes as usize).div_ceil(2);
        let mut buf = vec![0u16; words];
        let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        let ok = SetupDiGetDeviceInterfaceDetailW(
            dev_info,
            &raw const if_data,
            detail,
            required_bytes,
            &raw mut required_bytes,
            null_mut(),
        );
        let _ = SetupDiDestroyDeviceInfoList(dev_info);
        if ok == 0 {
            return None;
        }
        // `DevicePath` starts right after the 4-byte `cbSize` field (word offset 2).
        let path_words = &buf[2..];
        let end = path_words.iter().position(|&w| w == 0).unwrap_or(path_words.len());
        let mut out = path_words[..end].to_vec();
        out.push(0);
        Some(out)
    }
}

/// Open the ViGEmBus driver if installed, otherwise fall back cleanly to [`NullGamepadDriver`].
pub fn open_driver() -> Box<dyn GamepadDriver> {
    match ViGEmDriver::try_open() {
        Ok(Some(drv)) => Box::new(drv),
        Ok(None) => Box::new(NullGamepadDriver::default()),
        Err(e) => {
            tracing::warn!("ViGEmBus initialization error (falling back to disabled gamepads): {e}");
            Box::new(NullGamepadDriver::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vigem_abi_sizes_and_ioctls() {
        assert_eq!(IOCTL_VIGEM_CHECK_VERSION, 0x002A_2000);
        assert_eq!(IOCTL_VIGEM_PLUGIN_TARGET, 0x002A_A004);
        assert_eq!(IOCTL_VIGEM_UNPLUG_TARGET, 0x002A_A008);
        assert_eq!(IOCTL_XUSB_REQUEST_NOTIFICATION, 0x002A_E00C);
        assert_eq!(IOCTL_XUSB_SUBMIT_REPORT, 0x002A_A010);

        assert_eq!(size_of::<VigemCheckVersion>(), 8);
        assert_eq!(size_of::<VigemPluginTarget>(), 16);
        assert_eq!(size_of::<VigemUnplugTarget>(), 8);
        assert_eq!(size_of::<XusbSubmitReport>(), 20);
        assert_eq!(size_of::<XusbRequestNotification>(), 12);
    }

    #[test]
    fn open_driver_graceful_fallback() {
        let drv = open_driver();
        // Either ViGEmBus is installed (max_gamepads == 4) or NullGamepadDriver fallback (0).
        if drv.backend_name().is_some() {
            assert_eq!(drv.backend_name(), Some("ViGEmBus"));
            assert_eq!(drv.max_gamepads(), 4);
        } else {
            assert_eq!(drv.max_gamepads(), 0);
        }
    }
}
