//! Platform-neutral virtual gamepad state routing, XInput report conversion, monotonic
//! sequence filtering (up to 4 simultaneous pads), and rumble feedback helpers.

use veloxdesk_core_traits::Result;
use veloxdesk_core_traits::input::{GamepadState, InputFeedback, Rumble};

/// Maximum simultaneous virtual controllers supported (XInput / Parsec standard).
pub const MAX_GAMEPADS: usize = 4;

/// Standard XInput `wButtons` bitflags (matching `XINPUT_GAMEPAD_*`).
pub mod xinput_btn {
    pub const DPAD_UP: u16 = 0x0001;
    pub const DPAD_DOWN: u16 = 0x0002;
    pub const DPAD_LEFT: u16 = 0x0004;
    pub const DPAD_RIGHT: u16 = 0x0008;
    pub const START: u16 = 0x0010;
    pub const BACK: u16 = 0x0020;
    pub const LEFT_THUMB: u16 = 0x0040;
    pub const RIGHT_THUMB: u16 = 0x0080;
    pub const LEFT_SHOULDER: u16 = 0x0100;
    pub const RIGHT_SHOULDER: u16 = 0x0200;
    pub const GUIDE: u16 = 0x0400;
    pub const A: u16 = 0x1000;
    pub const B: u16 = 0x2000;
    pub const X: u16 = 0x4000;
    pub const Y: u16 = 0x8000;
}

/// Exact 12-byte XUSB / XInput gamepad state report (`XUSB_REPORT` / `XINPUT_GAMEPAD`).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XInputReport {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
}

impl XInputReport {
    /// Convert a wire [`GamepadState`] into a clamped [`XInputReport`].
    pub fn from_proto(s: &GamepadState) -> Self {
        Self {
            buttons: (s.buttons & 0xFFFF) as u16,
            left_trigger: s.left_trigger.min(255) as u8,
            right_trigger: s.right_trigger.min(255) as u8,
            left_x: s.left_x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            left_y: s.left_y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            right_x: s.right_x.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
            right_y: s.right_y.clamp(i16::MIN as i32, i16::MAX as i32) as i16,
        }
    }
}

/// Scale an 8-bit XInput/ViGEm motor value (`0..=255`) to the wire 16-bit range (`0..=65535`).
#[inline]
pub const fn motor_u8_to_u16(v: u8) -> u16 {
    (v as u16) * 257
}

/// Actions emitted by [`GamepadRouter`] for a platform virtual gamepad backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamepadAction {
    Connect(u32),
    Update(u32, XInputReport),
    Disconnect(u32),
}

#[derive(Debug, Default, Clone, Copy)]
struct PadSlot {
    connected: bool,
    last_seq: Option<u32>,
}

/// Tracks up to [`MAX_GAMEPADS`] virtual controllers with per-pad wrap-safe monotonic `seq`
/// filtering so out-of-order UDP datagrams are silently dropped.
#[derive(Debug, Default)]
pub struct GamepadRouter {
    slots: [PadSlot; MAX_GAMEPADS],
}

impl GamepadRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// True if `seq` is strictly newer than `last` (wrap-safe signed 32-bit difference),
    /// or if no packet has been accepted for this slot yet.
    ///
    /// Note: `seq == 0` is treated as unsequenced (e.g. reliable control-stream test events)
    /// when `last_seq` is `None` or `Some(0)`.
    fn is_fresh(last: Option<u32>, seq: u32) -> bool {
        match last {
            None => true,
            Some(0) if seq == 0 => true,
            Some(prev) => (seq.wrapping_sub(prev) as i32) > 0,
        }
    }

    /// Process an incoming [`GamepadState`] snapshot. Returns zero, one, or two actions
    /// (e.g. `[Connect, Update]` on first packet for a pad, or `[Disconnect]`).
    pub fn apply(&mut self, state: &GamepadState) -> Vec<GamepadAction> {
        let idx = state.pad_id as usize;
        if idx >= MAX_GAMEPADS {
            tracing::debug!(pad_id = state.pad_id, "ignoring gamepad with pad_id >= {MAX_GAMEPADS}");
            return Vec::new();
        }
        let slot = &mut self.slots[idx];
        if !Self::is_fresh(slot.last_seq, state.seq) {
            return Vec::new();
        }
        slot.last_seq = Some(state.seq);

        if !state.connected {
            if slot.connected {
                slot.connected = false;
                return vec![GamepadAction::Disconnect(state.pad_id)];
            }
            return Vec::new();
        }

        let mut actions = Vec::with_capacity(2);
        if !slot.connected {
            slot.connected = true;
            actions.push(GamepadAction::Connect(state.pad_id));
        }
        actions.push(GamepadAction::Update(state.pad_id, XInputReport::from_proto(state)));
        actions
    }

    /// Disconnect all currently plugged-in virtual pads and reset sequence tracking.
    pub fn disconnect_all(&mut self) -> Vec<GamepadAction> {
        let mut out = Vec::new();
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.connected {
                out.push(GamepadAction::Disconnect(i as u32));
            }
            *slot = PadSlot::default();
        }
        out
    }

    /// Number of currently connected virtual controllers.
    pub fn connected_count(&self) -> usize {
        self.slots.iter().filter(|s| s.connected).count()
    }
}

/// Platform virtual gamepad driver abstraction (ViGEmBus on Windows, uinput on Linux, or mock in tests).
pub trait GamepadDriver: Send {
    fn backend_name(&self) -> Option<&'static str>;
    fn max_gamepads(&self) -> u32;
    fn connect(&mut self, pad_id: u32) -> Result<()>;
    fn update(&mut self, pad_id: u32, report: XInputReport) -> Result<()>;
    fn disconnect(&mut self, pad_id: u32) -> Result<()>;
    fn poll_rumble(&mut self) -> Vec<Rumble>;

    fn apply_actions(&mut self, actions: &[GamepadAction]) -> Result<()> {
        for &a in actions {
            match a {
                GamepadAction::Connect(id) => self.connect(id)?,
                GamepadAction::Update(id, r) => self.update(id, r)?,
                GamepadAction::Disconnect(id) => self.disconnect(id)?,
            }
        }
        Ok(())
    }

    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        self.poll_rumble().into_iter().map(InputFeedback::Rumble).collect()
    }
}

/// Graceful no-op driver used when no virtual gamepad driver (e.g. ViGEmBus) is installed.
#[derive(Debug, Default)]
pub struct NullGamepadDriver {
    warned: bool,
}

impl GamepadDriver for NullGamepadDriver {
    fn backend_name(&self) -> Option<&'static str> {
        None
    }

    fn max_gamepads(&self) -> u32 {
        0
    }

    fn connect(&mut self, pad_id: u32) -> Result<()> {
        if !self.warned {
            tracing::info!(pad_id, "virtual gamepad driver unavailable; ignoring gamepad input");
            self.warned = true;
        }
        Ok(())
    }

    fn update(&mut self, _pad_id: u32, _report: XInputReport) -> Result<()> {
        Ok(())
    }

    fn disconnect(&mut self, _pad_id: u32) -> Result<()> {
        Ok(())
    }

    fn poll_rumble(&mut self) -> Vec<Rumble> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(pad_id: u32, connected: bool, seq: u32, buttons: u32) -> GamepadState {
        GamepadState {
            pad_id,
            connected,
            buttons,
            left_trigger: 128,
            right_trigger: 255,
            left_x: -32768,
            left_y: 32767,
            right_x: 1000,
            right_y: -1000,
            seq,
        }
    }

    #[test]
    fn report_size_and_clamping() {
        assert_eq!(std::mem::size_of::<XInputReport>(), 12);
        let s = GamepadState {
            pad_id: 0,
            connected: true,
            buttons: 0x0001_F001,
            left_trigger: 999,
            right_trigger: 42,
            left_x: 100_000,
            left_y: -100_000,
            right_x: 123,
            right_y: -456,
            seq: 1,
        };
        let r = XInputReport::from_proto(&s);
        assert_eq!(r.buttons, 0xF001);
        assert_eq!(r.left_trigger, 255);
        assert_eq!(r.right_trigger, 42);
        assert_eq!(r.left_x, i16::MAX);
        assert_eq!(r.left_y, i16::MIN);
        assert_eq!(r.right_x, 123);
        assert_eq!(r.right_y, -456);
        assert_eq!(motor_u8_to_u16(0), 0);
        assert_eq!(motor_u8_to_u16(255), 65535);
    }

    #[test]
    fn router_four_pads_seq_filter_and_disconnect_all() {
        let mut router = GamepadRouter::new();
        for p in 0..4u32 {
            let acts = router.apply(&state(p, true, 10, xinput_btn::A as u32));
            assert_eq!(acts.len(), 2);
            assert!(matches!(acts[0], GamepadAction::Connect(id) if id == p));
            assert!(matches!(acts[1], GamepadAction::Update(id, r) if id == p && r.buttons == xinput_btn::A));
        }
        assert_eq!(router.connected_count(), 4);

        // Out-of-range pad ignored.
        assert!(router.apply(&state(4, true, 1, 0)).is_empty());

        // Stale or duplicate seq dropped.
        assert!(router.apply(&state(0, true, 10, xinput_btn::B as u32)).is_empty());
        assert!(router.apply(&state(0, true, 9, xinput_btn::B as u32)).is_empty());

        // Newer seq updates without reconnecting.
        let acts = router.apply(&state(0, true, 11, xinput_btn::B as u32));
        assert_eq!(acts.len(), 1);
        assert!(matches!(acts[0], GamepadAction::Update(0, r) if r.buttons == xinput_btn::B));

        // Explicit disconnect of pad 2.
        let acts = router.apply(&state(2, false, 11, 0));
        assert_eq!(acts, vec![GamepadAction::Disconnect(2)]);
        assert_eq!(router.connected_count(), 3);

        // Wrap-around u32::MAX -> 0 accepted (stepping by < 2^31).
        assert_eq!(router.apply(&state(1, true, 0x8000_0005, xinput_btn::X as u32)).len(), 1);
        assert_eq!(router.apply(&state(1, true, u32::MAX, xinput_btn::X as u32)).len(), 1);
        assert_eq!(router.apply(&state(1, true, 0, xinput_btn::Y as u32)).len(), 1);

        // Disconnect all tears down remaining 3 pads.
        let disc = router.disconnect_all();
        assert_eq!(
            disc,
            vec![GamepadAction::Disconnect(0), GamepadAction::Disconnect(1), GamepadAction::Disconnect(3)]
        );
        assert_eq!(router.connected_count(), 0);
    }

    #[test]
    fn mock_driver_applies_and_drains_rumble() {
        struct MockDriver {
            plugged: [bool; 4],
            reports: Vec<(u32, XInputReport)>,
            rumbles: Vec<Rumble>,
        }
        impl GamepadDriver for MockDriver {
            fn backend_name(&self) -> Option<&'static str> {
                Some("mock")
            }
            fn max_gamepads(&self) -> u32 {
                4
            }
            fn connect(&mut self, id: u32) -> Result<()> {
                self.plugged[id as usize] = true;
                Ok(())
            }
            fn update(&mut self, id: u32, r: XInputReport) -> Result<()> {
                self.reports.push((id, r));
                Ok(())
            }
            fn disconnect(&mut self, id: u32) -> Result<()> {
                self.plugged[id as usize] = false;
                Ok(())
            }
            fn poll_rumble(&mut self) -> Vec<Rumble> {
                std::mem::take(&mut self.rumbles)
            }
        }

        let mut router = GamepadRouter::new();
        let mut drv = MockDriver {
            plugged: [false; 4],
            reports: Vec::new(),
            rumbles: vec![Rumble { pad_id: 1, low_freq: 32000, high_freq: 65535 }],
        };
        let acts = router.apply(&state(1, true, 1, xinput_btn::START as u32));
        drv.apply_actions(&acts).unwrap();
        assert!(drv.plugged[1]);
        assert_eq!(drv.reports.len(), 1);
        let fb = drv.poll_feedback();
        assert_eq!(fb, vec![InputFeedback::Rumble(Rumble { pad_id: 1, low_freq: 32000, high_freq: 65535 })]);
        assert!(drv.poll_feedback().is_empty());
    }
}
