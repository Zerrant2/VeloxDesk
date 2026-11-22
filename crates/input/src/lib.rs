//! # veloxdesk-input
//!
//! Host-side input injection.
//!
//! * Windows: [`create_injector`] → `SendInput` (scan codes, Unicode text, absolute mouse
//!   over the virtual desktop, relative mouse, wheel incl. horizontal).
//! * Linux: `/dev/uinput` keyboard + relative mouse device and an absolute pointer device.
//! * Other OSes: `CoreError::Unsupported`.
//!
//! Keys arrive as USB HID usages (page 0x07); see [`keymap`] for the per-OS tables.
//! Virtual gamepads (ViGEmBus / uinput) are M3.

pub mod core;
pub mod gamepad;
pub mod keymap;
pub mod text;

#[cfg(target_os = "linux")]
mod gamepad_linux;
#[cfg(windows)]
mod gamepad_win;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::UinputInjector;
#[cfg(windows)]
pub use windows::{SendInputInjector, INJECTED_TAG};

use veloxdesk_core_traits::{InputInjector, Result};

/// Best injector for this OS.
pub fn create_injector() -> Result<Box<dyn InputInjector>> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows::SendInputInjector::new()))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::UinputInjector::new()?))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(veloxdesk_core_traits::CoreError::unsupported("input injection not implemented on this OS"))
    }
}
