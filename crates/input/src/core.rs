//! Platform-neutral part of the injector: translates protocol [`InputEvent`]s into
//! primitive OS actions, maps absolute coordinates onto the virtual desktop and tracks
//! pressed keys/buttons so [`InjectorCore::release_actions`] can undo them.

use std::collections::BTreeSet;

use veloxdesk_core_traits::input::{DisplayGeometry, InputEvent, InputEventKind, MouseButton};

/// One primitive action executed by an OS backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Absolute position in virtual-desktop pixels.
    MoveAbs { x: i32, y: i32 },
    MoveRel { dx: i32, dy: i32 },
    Button { button: MouseButton, down: bool },
    /// 1/120 notch units; positive = up / right.
    Wheel { dx: i32, dy: i32 },
    /// USB HID usage (page 0x07).
    Key { usage: u32, down: bool },
    Text(String),
}

/// Bounding box of all displays in virtual-desktop pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtualBounds {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl VirtualBounds {
    /// Map a virtual-desktop pixel to the normalized 0..=65535 range used by
    /// `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK` (and uinput ABS ranges).
    pub fn normalize(&self, x: i32, y: i32) -> (i32, i32) {
        fn n(v: i32, origin: i32, len: u32) -> i32 {
            if len <= 1 {
                return 0;
            }
            let rel = (v as i64 - origin as i64).clamp(0, len as i64 - 1);
            ((rel * 65535 + (len as i64 - 1) / 2) / (len as i64 - 1)) as i32
        }
        (n(x, self.x, self.width), n(y, self.y, self.height))
    }
}

#[derive(Debug, Default)]
pub struct InjectorCore {
    displays: Vec<DisplayGeometry>,
    bounds: Option<VirtualBounds>,
    pressed_keys: BTreeSet<u32>,
    pressed_buttons: BTreeSet<i32>,
}

impl InjectorCore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_displays(&mut self, displays: &[DisplayGeometry]) {
        self.displays = displays.to_vec();
        self.bounds = union(displays);
    }

    /// Union of all displays; `None` until `set_displays` was called with a non-empty list.
    pub fn bounds(&self) -> Option<VirtualBounds> {
        self.bounds
    }

    /// Map display-local pixel coordinates to the virtual desktop (clamped to the display).
    pub fn map_abs(&self, x: i32, y: i32, display: u32) -> (i32, i32) {
        let geom = self
            .displays
            .iter()
            .find(|d| d.index == display)
            .or_else(|| self.displays.first());
        match geom {
            Some(d) => {
                let cx = x.clamp(0, d.width.saturating_sub(1) as i32);
                let cy = y.clamp(0, d.height.saturating_sub(1) as i32);
                (d.x + cx, d.y + cy)
            }
            // No layout known: treat as virtual-desktop coordinates.
            None => (x, y),
        }
    }

    /// Translate one protocol event. Unsupported kinds (gamepad/touch, M3) yield nothing.
    pub fn translate(&mut self, ev: &InputEvent) -> Vec<Action> {
        let Some(kind) = &ev.event else { return Vec::new() };
        match kind {
            InputEventKind::MouseMove(m) => {
                let (x, y) = self.map_abs(m.x, m.y, m.display);
                vec![Action::MoveAbs { x, y }]
            }
            InputEventKind::MouseRelative(m) => {
                if m.dx == 0 && m.dy == 0 {
                    Vec::new()
                } else {
                    vec![Action::MoveRel { dx: m.dx, dy: m.dy }]
                }
            }
            InputEventKind::MouseButton(b) => {
                let Ok(button) = MouseButton::try_from(b.button) else { return Vec::new() };
                if b.down {
                    self.pressed_buttons.insert(b.button);
                } else {
                    // A release of a button we never pressed is still forwarded (fixes
                    // buttons stuck by a previous session).
                    self.pressed_buttons.remove(&b.button);
                }
                vec![Action::Button { button, down: b.down }]
            }
            InputEventKind::MouseWheel(w) => {
                if w.dx == 0 && w.dy == 0 {
                    Vec::new()
                } else {
                    vec![Action::Wheel { dx: w.dx, dy: w.dy }]
                }
            }
            InputEventKind::Key(k) => {
                if k.down {
                    self.pressed_keys.insert(k.hid_usage);
                } else {
                    self.pressed_keys.remove(&k.hid_usage);
                }
                vec![Action::Key { usage: k.hid_usage, down: k.down }]
            }
            InputEventKind::Text(t) => {
                if t.text.is_empty() {
                    Vec::new()
                } else {
                    vec![Action::Text(t.text.clone())]
                }
            }
            InputEventKind::Gamepad(_) | InputEventKind::Touch(_) => Vec::new(),
        }
    }

    /// Actions releasing every key and button currently held; clears the state.
    /// Regular keys are released before modifiers (HID 0xE0..=0xE7 sort last).
    pub fn release_actions(&mut self) -> Vec<Action> {
        let mut out: Vec<Action> = std::mem::take(&mut self.pressed_keys)
            .into_iter()
            .map(|usage| Action::Key { usage, down: false })
            .collect();
        out.extend(
            std::mem::take(&mut self.pressed_buttons)
                .into_iter()
                .filter_map(|b| MouseButton::try_from(b).ok())
                .map(|button| Action::Button { button, down: false }),
        );
        out
    }

    pub fn pressed_key_count(&self) -> usize {
        self.pressed_keys.len()
    }
}

fn union(displays: &[DisplayGeometry]) -> Option<VirtualBounds> {
    let mut it = displays.iter().filter(|d| d.width > 0 && d.height > 0);
    let first = it.next()?;
    let (mut x0, mut y0) = (first.x as i64, first.y as i64);
    let (mut x1, mut y1) = (x0 + first.width as i64, y0 + first.height as i64);
    for d in it {
        x0 = x0.min(d.x as i64);
        y0 = y0.min(d.y as i64);
        x1 = x1.max(d.x as i64 + d.width as i64);
        y1 = y1.max(d.y as i64 + d.height as i64);
    }
    Some(VirtualBounds { x: x0 as i32, y: y0 as i32, width: (x1 - x0) as u32, height: (y1 - y0) as u32 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use veloxdesk_protocol::control::{KeyEvent, MouseButtonEvent, MouseMove, MouseWheel, TextInput};

    fn ev(kind: InputEventKind) -> InputEvent {
        InputEvent { ts_us: 0, event: Some(kind) }
    }

    fn two_displays() -> Vec<DisplayGeometry> {
        vec![
            DisplayGeometry { index: 0, x: 0, y: 0, width: 1920, height: 1080 },
            // secondary on the left, shifted down
            DisplayGeometry { index: 1, x: -2560, y: -200, width: 2560, height: 1440 },
        ]
    }

    #[test]
    fn bounds_and_mapping() {
        let mut c = InjectorCore::new();
        assert!(c.bounds().is_none());
        c.set_displays(&two_displays());
        let b = c.bounds().unwrap();
        assert_eq!(b, VirtualBounds { x: -2560, y: -200, width: 4480, height: 1440 });
        assert_eq!(c.map_abs(10, 20, 0), (10, 20));
        assert_eq!(c.map_abs(10, 20, 1), (-2550, -180));
        // clamped to display
        assert_eq!(c.map_abs(5000, -5, 0), (1919, 0));
        // unknown display index → first
        assert_eq!(c.map_abs(1, 1, 7), (1, 1));
        // normalization corners
        assert_eq!(b.normalize(-2560, -200), (0, 0));
        assert_eq!(b.normalize(1919, 1239), (65535, 65535));
        let (nx, _) = b.normalize(0, 0);
        assert!((nx - 37457).abs() <= 1, "{nx}");
    }

    #[test]
    fn translate_and_release() {
        let mut c = InjectorCore::new();
        c.set_displays(&two_displays());
        let a = c.translate(&ev(InputEventKind::MouseMove(MouseMove { x: 100, y: 50, display: 1 })));
        assert_eq!(a, vec![Action::MoveAbs { x: -2460, y: -150 }]);
        c.translate(&ev(InputEventKind::Key(KeyEvent { hid_usage: 0xE0, down: true, modifiers: 0 })));
        c.translate(&ev(InputEventKind::Key(KeyEvent { hid_usage: 0x04, down: true, modifiers: 2 })));
        c.translate(&ev(InputEventKind::Key(KeyEvent { hid_usage: 0x04, down: false, modifiers: 2 })));
        c.translate(&ev(InputEventKind::Key(KeyEvent { hid_usage: 0x16, down: true, modifiers: 2 })));
        c.translate(&ev(InputEventKind::MouseButton(MouseButtonEvent {
            button: MouseButton::Right as i32,
            down: true,
        })));
        assert_eq!(c.pressed_key_count(), 2);
        let wheel = c.translate(&ev(InputEventKind::MouseWheel(MouseWheel { dx: -120, dy: 240 })));
        assert_eq!(wheel, vec![Action::Wheel { dx: -120, dy: 240 }]);
        let rel = c.release_actions();
        assert_eq!(
            rel,
            vec![
                Action::Key { usage: 0x16, down: false },
                Action::Key { usage: 0xE0, down: false },
                Action::Button { button: MouseButton::Right, down: false },
            ]
        );
        assert!(c.release_actions().is_empty());
        let t = c.translate(&ev(InputEventKind::Text(TextInput { text: "hé😀".into() })));
        assert_eq!(t, vec![Action::Text("hé😀".into())]);
        assert!(c.translate(&InputEvent { ts_us: 0, event: None }).is_empty());
    }
}
