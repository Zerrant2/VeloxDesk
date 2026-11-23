//! winit input → protocol `InputEvent` (USB HID usage codes, 1/120 wheel units).

use veloxdesk_core_traits::Timestamp;
use veloxdesk_protocol::control::{
    InputEvent, KeyEvent, MouseButton, MouseButtonEvent, MouseMove, MouseRelative, MouseWheel, input_event::Event,
};
use winit::event::{MouseButton as WButton, MouseScrollDelta};
use winit::keyboard::{KeyCode, ModifiersState};

/// Physical key → USB HID usage (keyboard page 0x07). `None` for keys without a usage.
pub fn keycode_to_hid(k: KeyCode) -> Option<u32> {
    use KeyCode::*;
    let v = match k {
        KeyA => 0x04,
        KeyB => 0x05,
        KeyC => 0x06,
        KeyD => 0x07,
        KeyE => 0x08,
        KeyF => 0x09,
        KeyG => 0x0A,
        KeyH => 0x0B,
        KeyI => 0x0C,
        KeyJ => 0x0D,
        KeyK => 0x0E,
        KeyL => 0x0F,
        KeyM => 0x10,
        KeyN => 0x11,
        KeyO => 0x12,
        KeyP => 0x13,
        KeyQ => 0x14,
        KeyR => 0x15,
        KeyS => 0x16,
        KeyT => 0x17,
        KeyU => 0x18,
        KeyV => 0x19,
        KeyW => 0x1A,
        KeyX => 0x1B,
        KeyY => 0x1C,
        KeyZ => 0x1D,
        Digit1 => 0x1E,
        Digit2 => 0x1F,
        Digit3 => 0x20,
        Digit4 => 0x21,
        Digit5 => 0x22,
        Digit6 => 0x23,
        Digit7 => 0x24,
        Digit8 => 0x25,
        Digit9 => 0x26,
        Digit0 => 0x27,
        Enter => 0x28,
        Escape => 0x29,
        Backspace => 0x2A,
        Tab => 0x2B,
        Space => 0x2C,
        Minus => 0x2D,
        Equal => 0x2E,
        BracketLeft => 0x2F,
        BracketRight => 0x30,
        Backslash => 0x31,
        Semicolon => 0x33,
        Quote => 0x34,
        Backquote => 0x35,
        Comma => 0x36,
        Period => 0x37,
        Slash => 0x38,
        CapsLock => 0x39,
        F1 => 0x3A,
        F2 => 0x3B,
        F3 => 0x3C,
        F4 => 0x3D,
        F5 => 0x3E,
        F6 => 0x3F,
        F7 => 0x40,
        F8 => 0x41,
        F9 => 0x42,
        F10 => 0x43,
        F11 => 0x44,
        F12 => 0x45,
        PrintScreen => 0x46,
        ScrollLock => 0x47,
        Pause => 0x48,
        Insert => 0x49,
        Home => 0x4A,
        PageUp => 0x4B,
        Delete => 0x4C,
        End => 0x4D,
        PageDown => 0x4E,
        ArrowRight => 0x4F,
        ArrowLeft => 0x50,
        ArrowDown => 0x51,
        ArrowUp => 0x52,
        NumLock => 0x53,
        NumpadDivide => 0x54,
        NumpadMultiply => 0x55,
        NumpadSubtract => 0x56,
        NumpadAdd => 0x57,
        NumpadEnter => 0x58,
        Numpad1 => 0x59,
        Numpad2 => 0x5A,
        Numpad3 => 0x5B,
        Numpad4 => 0x5C,
        Numpad5 => 0x5D,
        Numpad6 => 0x5E,
        Numpad7 => 0x5F,
        Numpad8 => 0x60,
        Numpad9 => 0x61,
        Numpad0 => 0x62,
        NumpadDecimal => 0x63,
        IntlBackslash => 0x64,
        ContextMenu => 0x65,
        Power => 0x66,
        NumpadEqual => 0x67,
        F13 => 0x68,
        F14 => 0x69,
        F15 => 0x6A,
        F16 => 0x6B,
        F17 => 0x6C,
        F18 => 0x6D,
        F19 => 0x6E,
        F20 => 0x6F,
        F21 => 0x70,
        F22 => 0x71,
        F23 => 0x72,
        F24 => 0x73,
        AudioVolumeMute => 0x7F,
        AudioVolumeUp => 0x80,
        AudioVolumeDown => 0x81,
        NumpadComma => 0x85,
        IntlRo => 0x87,
        KanaMode => 0x88,
        IntlYen => 0x89,
        Convert => 0x8A,
        NonConvert => 0x8B,
        Lang1 => 0x90,
        Lang2 => 0x91,
        ControlLeft => 0xE0,
        ShiftLeft => 0xE1,
        AltLeft => 0xE2,
        SuperLeft => 0xE3,
        ControlRight => 0xE4,
        ShiftRight => 0xE5,
        AltRight => 0xE6,
        SuperRight => 0xE7,
        _ => return None,
    };
    Some(v)
}

/// Protocol modifier bitmask: 1 Shift, 2 Ctrl, 4 Alt, 8 Meta.
pub fn modifiers_mask(m: ModifiersState) -> u32 {
    let mut v = 0;
    if m.shift_key() {
        v |= 1;
    }
    if m.control_key() {
        v |= 2;
    }
    if m.alt_key() {
        v |= 4;
    }
    if m.super_key() {
        v |= 8;
    }
    v
}

fn ev(event: Event) -> InputEvent {
    InputEvent { ts_us: Timestamp::now().as_micros(), event: Some(event) }
}

pub fn key_event(k: KeyCode, down: bool, mods: ModifiersState) -> Option<InputEvent> {
    let hid = keycode_to_hid(k)?;
    Some(ev(Event::Key(KeyEvent { hid_usage: hid, down, modifiers: modifiers_mask(mods) })))
}

/// Synthesize press + release events for Android Back navigation (`Escape`, HID `0x29`).
pub fn android_back_events() -> Vec<InputEvent> {
    [
        key_event(KeyCode::Escape, true, ModifiersState::empty()),
        key_event(KeyCode::Escape, false, ModifiersState::empty()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Synthesize press + release events for Android Home navigation (`SuperLeft` / MetaLeft, HID `0xE3`).
pub fn android_home_events() -> Vec<InputEvent> {
    [
        key_event(KeyCode::SuperLeft, true, ModifiersState::empty()),
        key_event(KeyCode::SuperLeft, false, ModifiersState::empty()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Synthesize press + release events for Android Recents switcher (`Alt+Tab`, HID `0xE2` + `0x2B`).
pub fn android_recents_events() -> Vec<InputEvent> {
    [
        key_event(KeyCode::AltLeft, true, ModifiersState::ALT),
        key_event(KeyCode::Tab, true, ModifiersState::ALT),
        key_event(KeyCode::Tab, false, ModifiersState::ALT),
        key_event(KeyCode::AltLeft, false, ModifiersState::empty()),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub fn mouse_button(b: WButton, down: bool) -> Option<InputEvent> {
    let button = match b {
        WButton::Left => MouseButton::Left,
        WButton::Right => MouseButton::Right,
        WButton::Middle => MouseButton::Middle,
        WButton::Back => MouseButton::Back,
        WButton::Forward => MouseButton::Forward,
        WButton::Other(_) => return None,
    };
    Some(ev(Event::MouseButton(MouseButtonEvent { button: button as i32, down })))
}

pub fn mouse_move(x: i32, y: i32, display: u32) -> InputEvent {
    ev(Event::MouseMove(MouseMove { x, y, display }))
}

pub fn mouse_relative(dx: i32, dy: i32) -> InputEvent {
    ev(Event::MouseRelative(MouseRelative { dx, dy }))
}

/// Wheel accumulator: converts line/pixel deltas into 1/120 notch units, keeping the
/// fractional remainder so smooth touchpads aren't truncated.
#[derive(Debug, Default)]
pub struct WheelAccum {
    rem_x: f64,
    rem_y: f64,
}

/// Pixels per notch for pixel-precise devices (Windows uses 120 units per ~ 1 line).
const PIXELS_PER_NOTCH: f64 = 40.0;

impl WheelAccum {
    pub fn push(&mut self, delta: MouseScrollDelta) -> Option<InputEvent> {
        let (fx, fy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x as f64 * 120.0, y as f64 * 120.0),
            MouseScrollDelta::PixelDelta(p) => (p.x / PIXELS_PER_NOTCH * 120.0, p.y / PIXELS_PER_NOTCH * 120.0),
        };
        self.rem_x += fx;
        self.rem_y += fy;
        let (dx, dy) = (self.rem_x.trunc(), self.rem_y.trunc());
        self.rem_x -= dx;
        self.rem_y -= dy;
        if dx == 0.0 && dy == 0.0 {
            return None;
        }
        Some(ev(Event::MouseWheel(MouseWheel { dx: dx as i32, dy: dy as i32 })))
    }
}

/// Accumulates raw relative motion between sends (sub-pixel remainders preserved).
#[derive(Debug, Default)]
pub struct RelativeAccum {
    x: f64,
    y: f64,
}

impl RelativeAccum {
    pub fn push(&mut self, dx: f64, dy: f64) {
        self.x += dx;
        self.y += dy;
    }
    pub fn take(&mut self) -> Option<InputEvent> {
        let (ix, iy) = (self.x.trunc(), self.y.trunc());
        if ix == 0.0 && iy == 0.0 {
            return None;
        }
        self.x -= ix;
        self.y -= iy;
        Some(mouse_relative(ix as i32, iy as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;

    #[test]
    fn hid_table() {
        assert_eq!(keycode_to_hid(KeyCode::KeyA), Some(0x04));
        assert_eq!(keycode_to_hid(KeyCode::KeyZ), Some(0x1D));
        assert_eq!(keycode_to_hid(KeyCode::Digit0), Some(0x27));
        assert_eq!(keycode_to_hid(KeyCode::Enter), Some(0x28));
        assert_eq!(keycode_to_hid(KeyCode::F12), Some(0x45));
        assert_eq!(keycode_to_hid(KeyCode::ArrowUp), Some(0x52));
        assert_eq!(keycode_to_hid(KeyCode::Numpad0), Some(0x62));
        assert_eq!(keycode_to_hid(KeyCode::SuperRight), Some(0xE7));
        assert_eq!(keycode_to_hid(KeyCode::Fn), None);
    }

    #[test]
    fn hid_table_is_injective() {
        use KeyCode::*;
        let keys = [
            KeyA, KeyQ, KeyZ, Digit1, Digit0, Enter, Escape, Space, Minus, Backquote, CapsLock, F1, F24, Insert,
            Delete, ArrowLeft, NumLock, NumpadEnter, Numpad9, IntlBackslash, ControlLeft, AltRight, ShiftRight,
        ];
        let mut seen = std::collections::HashSet::new();
        for k in keys {
            assert!(seen.insert(keycode_to_hid(k).unwrap()), "{k:?}");
        }
    }

    #[test]
    fn modifiers() {
        assert_eq!(modifiers_mask(ModifiersState::SHIFT | ModifiersState::ALT), 5);
        assert_eq!(modifiers_mask(ModifiersState::CONTROL | ModifiersState::SUPER), 10);
    }

    #[test]
    fn wheel_units() {
        let mut w = WheelAccum::default();
        let e = w.push(MouseScrollDelta::LineDelta(0.0, 1.0)).unwrap();
        assert!(matches!(e.event, Some(Event::MouseWheel(MouseWheel { dx: 0, dy: 120 }))));
        // 10 px → 30 units, accumulates fractions
        assert!(w.push(MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 0.1))).is_none());
        let e = w.push(MouseScrollDelta::PixelDelta(PhysicalPosition::new(0.0, 10.0))).unwrap();
        assert!(matches!(e.event, Some(Event::MouseWheel(MouseWheel { dy: 30, .. }))));
    }

    #[test]
    fn relative_accumulates() {
        let mut r = RelativeAccum::default();
        r.push(0.4, -0.6);
        assert!(r.take().is_none());
        r.push(0.7, -0.6);
        let e = r.take().unwrap();
        assert!(matches!(e.event, Some(Event::MouseRelative(MouseRelative { dx: 1, dy: -1 }))));
    }

    #[test]
    fn android_nav_key_sequences_have_exact_hid_codes() {
        let unpack = |evs: Vec<InputEvent>| -> Vec<(u32, bool, u32)> {
            evs.into_iter()
                .map(|e| match e.event {
                    Some(Event::Key(k)) => (k.hid_usage, k.down, k.modifiers),
                    other => panic!("expected Key event, got {other:?}"),
                })
                .collect()
        };
        assert_eq!(unpack(android_back_events()), vec![(0x29, true, 0), (0x29, false, 0)]);
        assert_eq!(unpack(android_home_events()), vec![(0xE3, true, 0), (0xE3, false, 0)]);
        assert_eq!(
            unpack(android_recents_events()),
            vec![(0xE2, true, 4), (0x2B, true, 4), (0x2B, false, 4), (0xE2, false, 0)]
        );
    }
}
