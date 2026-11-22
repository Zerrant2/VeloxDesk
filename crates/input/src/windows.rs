//! Windows backend: `SendInput` with scan codes (`KEYEVENTF_SCANCODE`), Unicode text
//! (`KEYEVENTF_UNICODE`) and absolute mouse over the whole virtual desktop
//! (`MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`).
//!
//! Notes:
//! * `SendInput` is blocked by UIPI for elevated windows / the secure desktop unless the
//!   host runs elevated or as a service on the input desktop (M4 service mode); failures
//!   surface as `CoreError::AccessDenied`.
//! * Relative motion goes through `MOUSEEVENTF_MOVE`, which raw-input games see unscaled.

use std::mem::size_of;

use veloxdesk_core_traits::input::{
    DisplayGeometry, InputCaps, InputEvent, InputFeedback, InputInjector, MouseButton,
};
use veloxdesk_core_traits::{CoreError, Result};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
    XBUTTON1, XBUTTON2,
};

use crate::core::{Action, InjectorCore, VirtualBounds};
use crate::gamepad::{GamepadDriver, GamepadRouter};
use crate::keymap::hid_to_win_scancode;

/// Tag placed in `dwExtraInfo` so local hooks can recognise injected events.
pub const INJECTED_TAG: usize = 0x5644_4B31; // "VDK1"

pub struct SendInputInjector {
    core: InjectorCore,
    buf: Vec<INPUT>,
    gamepad_router: GamepadRouter,
    gamepad: Box<dyn GamepadDriver>,
}

// SAFETY: INPUT is plain data; the buffer is only used from the owning thread.
unsafe impl Send for SendInputInjector {}

impl SendInputInjector {
    pub fn new() -> Self {
        Self::with_gamepad_driver(crate::gamepad_win::open_driver())
    }

    pub fn with_gamepad_driver(gamepad: Box<dyn GamepadDriver>) -> Self {
        Self {
            core: InjectorCore::new(),
            buf: Vec::with_capacity(16),
            gamepad_router: GamepadRouter::new(),
            gamepad,
        }
    }

    fn bounds(&self) -> VirtualBounds {
        if let Some(b) = self.core.bounds() {
            return b;
        }
        // SAFETY: plain Win32 call without pointers.
        let (x, y, w, h) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        VirtualBounds { x, y, width: w.max(1) as u32, height: h.max(1) as u32 }
    }

    fn push_mouse(&mut self, dx: i32, dy: i32, data: i32, flags: u32) {
        self.buf.push(INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data as _,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: INJECTED_TAG,
                },
            },
        });
    }

    fn push_key(&mut self, vk: u16, scan: u16, flags: u32) {
        self.buf.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: INJECTED_TAG },
            },
        });
    }

    fn encode(&mut self, action: &Action) {
        match action {
            Action::MoveAbs { x, y } => {
                let (nx, ny) = self.bounds().normalize(*x, *y);
                self.push_mouse(nx, ny, 0, MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK);
            }
            Action::MoveRel { dx, dy } => self.push_mouse(*dx, *dy, 0, MOUSEEVENTF_MOVE),
            Action::Button { button, down } => {
                let (flags, data) = button_flags(*button, *down);
                self.push_mouse(0, 0, data, flags);
            }
            Action::Wheel { dx, dy } => {
                if *dy != 0 {
                    self.push_mouse(0, 0, *dy, MOUSEEVENTF_WHEEL);
                }
                if *dx != 0 {
                    self.push_mouse(0, 0, *dx, MOUSEEVENTF_HWHEEL);
                }
            }
            Action::Key { usage, down } => match hid_to_win_scancode(*usage) {
                Some(sc) => {
                    let (scan, flags) = scan_flags(sc, *down);
                    self.push_key(0, scan, flags);
                }
                None => tracing::debug!(usage, "unmapped HID usage"),
            },
            Action::Text(text) => {
                for unit in text.encode_utf16() {
                    self.push_key(0, unit, KEYEVENTF_UNICODE);
                    self.push_key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP);
                }
            }
            Action::Gamepad(_) => {}
        }
    }

    fn flush(&mut self) -> Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let n = self.buf.len() as u32;
        // SAFETY: buf holds `n` initialised INPUT structs; cbSize is the struct size.
        let sent = unsafe { SendInput(n, self.buf.as_ptr(), size_of::<INPUT>() as i32) };
        self.buf.clear();
        if sent == n {
            Ok(())
        } else {
            Err(CoreError::AccessDenied(
                format!("SendInput injected {sent}/{n} events (UIPI / secure desktop?)").into(),
            ))
        }
    }

    fn run(&mut self, actions: &[Action]) -> Result<()> {
        for a in actions {
            if let Action::Gamepad(state) = a {
                let g_actions = self.gamepad_router.apply(state);
                self.gamepad.apply_actions(&g_actions)?;
            } else {
                self.encode(a);
            }
        }
        self.flush()
    }
}

impl Default for SendInputInjector {
    fn default() -> Self {
        Self::new()
    }
}

/// `(wScan, dwFlags)` for a set-1 scan code (`0xE0xx` = extended).
pub(crate) fn scan_flags(sc: u16, down: bool) -> (u16, u32) {
    let mut flags = KEYEVENTF_SCANCODE;
    if sc & 0xFF00 == 0xE000 {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    (sc & 0xFF, flags)
}

/// `(dwFlags, mouseData)` for a mouse button transition.
pub(crate) fn button_flags(button: MouseButton, down: bool) -> (u32, i32) {
    match (button, down) {
        (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
        (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as i32),
        (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1 as i32),
        (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as i32),
        (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2 as i32),
    }
}

impl InputInjector for SendInputInjector {
    fn caps(&self) -> InputCaps {
        let gp_backend = self.gamepad.backend_name();
        InputCaps {
            backend: "SendInput",
            keyboard: true,
            mouse_absolute: true,
            mouse_relative: true,
            gamepads: gp_backend.is_some(),
            gamepad_backend: gp_backend,
            max_gamepads: self.gamepad.max_gamepads(),
            touch: false,
        }
    }

    fn set_displays(&mut self, displays: &[DisplayGeometry]) {
        self.core.set_displays(displays);
    }

    fn inject(&mut self, event: &InputEvent) -> Result<()> {
        let actions = self.core.translate(event);
        self.run(&actions)
    }

    fn release_all(&mut self) -> Result<()> {
        let gp_disc = self.gamepad_router.disconnect_all();
        let gp_res = self.gamepad.apply_actions(&gp_disc);
        let actions = self.core.release_actions();
        let kb_res = self.run(&actions);
        kb_res.and(gp_res)
    }

    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        self.gamepad.poll_feedback()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scancode_flags() {
        let (s, f) = scan_flags(0x1E, true);
        assert_eq!(s, 0x1E);
        assert_eq!(f, KEYEVENTF_SCANCODE);
        let (s, f) = scan_flags(0xE04D, false);
        assert_eq!(s, 0x4D);
        assert_eq!(f, KEYEVENTF_SCANCODE | KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP);
    }

    #[test]
    fn xbuttons() {
        assert_eq!(button_flags(MouseButton::Back, true), (MOUSEEVENTF_XDOWN, 1));
        assert_eq!(button_flags(MouseButton::Forward, false), (MOUSEEVENTF_XUP, 2));
    }

    #[test]
    fn encodes_batch_without_sending() {
        let mut inj = SendInputInjector::new();
        inj.set_displays(&[DisplayGeometry { index: 0, x: 0, y: 0, width: 1920, height: 1080 }]);
        inj.encode(&Action::MoveAbs { x: 1919, y: 0 });
        inj.encode(&Action::Wheel { dx: 120, dy: -240 });
        inj.encode(&Action::Text("a😀".into())); // 1 + 2 UTF-16 units → 6 key events
        assert_eq!(inj.buf.len(), 1 + 2 + 6);
        // SAFETY: we wrote these variants above.
        unsafe {
            assert_eq!(inj.buf[0].Anonymous.mi.dx, 65535);
            assert_eq!(inj.buf[1].Anonymous.mi.mouseData as i32, -240);
            assert_eq!(inj.buf[2].Anonymous.mi.dwFlags, MOUSEEVENTF_HWHEEL);
            assert_eq!(inj.buf[5].Anonymous.ki.wScan, 0xD83D);
        }
        inj.buf.clear();
    }

    #[test]
    fn injector_routes_gamepads_rumble_and_release() {
        use veloxdesk_core_traits::input::{GamepadState, InputEventKind, Rumble};
        use crate::gamepad::{GamepadDriver, XInputReport, xinput_btn};

        struct FakePad {
            plugged: [bool; 4],
            updates: usize,
            rumbles: Vec<Rumble>,
        }
        impl GamepadDriver for FakePad {
            fn backend_name(&self) -> Option<&'static str> {
                Some("fake-vigem")
            }
            fn max_gamepads(&self) -> u32 {
                4
            }
            fn connect(&mut self, pad_id: u32) -> Result<()> {
                self.plugged[pad_id as usize] = true;
                Ok(())
            }
            fn update(&mut self, _pad_id: u32, _report: XInputReport) -> Result<()> {
                self.updates += 1;
                Ok(())
            }
            fn disconnect(&mut self, pad_id: u32) -> Result<()> {
                self.plugged[pad_id as usize] = false;
                Ok(())
            }
            fn poll_rumble(&mut self) -> Vec<Rumble> {
                std::mem::take(&mut self.rumbles)
            }
        }

        let drv = FakePad {
            plugged: [false; 4],
            updates: 0,
            rumbles: vec![Rumble { pad_id: 0, low_freq: 40000, high_freq: 20000 }],
        };
        let mut inj = SendInputInjector::with_gamepad_driver(Box::new(drv));
        let caps = inj.caps();
        assert!(caps.gamepads);
        assert_eq!(caps.gamepad_backend, Some("fake-vigem"));
        assert_eq!(caps.max_gamepads, 4);

        let ev = InputEvent {
            ts_us: 1,
            event: Some(InputEventKind::Gamepad(GamepadState {
                pad_id: 0,
                connected: true,
                buttons: xinput_btn::A as u32,
                seq: 1,
                ..Default::default()
            })),
        };
        inj.inject(&ev).unwrap();
        assert_eq!(inj.gamepad_router.connected_count(), 1);
        assert_eq!(inj.poll_feedback(), vec![InputFeedback::Rumble(Rumble { pad_id: 0, low_freq: 40000, high_freq: 20000 })]);
        inj.release_all().unwrap();
        assert_eq!(inj.gamepad_router.connected_count(), 0);
    }
}
