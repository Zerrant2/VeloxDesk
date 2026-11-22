//! Local physical gamepad polling (up to 4 simultaneous XInput controllers on Windows)
//! and force-feedback rumble (`XInputSetState`), M3 Gaming Mode.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use tokio::sync::mpsc;
use veloxdesk_protocol::control::{GamepadState, Rumble};

/// Maximum simultaneous physical controllers polled (XInput supports 0..4).
pub const MAX_GAMEPADS: usize = 4;

/// Raw snapshot of one controller without sequence metadata, used for change detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PadSnapshot {
    pub connected: bool,
    pub buttons: u32,
    pub left_trigger: u32,
    pub right_trigger: u32,
    pub left_x: i32,
    pub left_y: i32,
    pub right_x: i32,
    pub right_y: i32,
}

impl PadSnapshot {
    pub fn to_wire(self, pad_id: u32, seq: u32) -> GamepadState {
        GamepadState {
            pad_id,
            connected: self.connected,
            buttons: self.buttons,
            left_trigger: self.left_trigger,
            right_trigger: self.right_trigger,
            left_x: self.left_x,
            left_y: self.left_y,
            right_x: self.right_x,
            right_y: self.right_y,
            seq,
        }
    }
}

/// Tracks 4 gamepad slots, emitting idempotent [`GamepadState`] snapshots on state change,
/// connect/disconnect, or periodic heartbeat (so UDP datagram loss self-heals immediately).
#[derive(Debug, Default)]
pub struct GamepadTracker {
    prev: [PadSnapshot; MAX_GAMEPADS],
    seq: [u32; MAX_GAMEPADS],
    ticks_since_send: [u32; MAX_GAMEPADS],
}

impl GamepadTracker {
    /// Heartbeat interval in poll ticks (e.g. 12 ticks @ 4 ms ≈ 48 ms while connected).
    const HEARTBEAT_TICKS: u32 = 12;

    /// Compare `cur` against previous states and return snapshots that should be transmitted.
    /// Also returns the current count of connected gamepads.
    pub fn update(&mut self, cur: &[PadSnapshot; MAX_GAMEPADS]) -> (Vec<GamepadState>, u8) {
        let mut out = Vec::new();
        let mut active = 0u8;
        for (idx, snap) in cur.iter().copied().enumerate() {
            if snap.connected {
                active += 1;
            }
            let changed = snap != self.prev[idx];
            self.ticks_since_send[idx] = self.ticks_since_send[idx].saturating_add(1);
            let heartbeat = snap.connected && self.ticks_since_send[idx] >= Self::HEARTBEAT_TICKS;
            if changed || heartbeat {
                self.prev[idx] = snap;
                self.seq[idx] = self.seq[idx].wrapping_add(1).max(1);
                self.ticks_since_send[idx] = 0;
                out.push(snap.to_wire(idx as u32, self.seq[idx]));
            }
        }
        (out, active)
    }

    /// Emit disconnect snapshots for every currently connected pad (called on session teardown).
    pub fn disconnect_all(&mut self) -> Vec<GamepadState> {
        let mut out = Vec::new();
        for idx in 0..MAX_GAMEPADS {
            if self.prev[idx].connected {
                self.prev[idx] = PadSnapshot::default();
                self.seq[idx] = self.seq[idx].wrapping_add(1).max(1);
                out.push(PadSnapshot::default().to_wire(idx as u32, self.seq[idx]));
            }
        }
        out
    }
}

/// Background thread polling up to 4 physical controllers and applying incoming rumble.
#[cfg_attr(not(feature = "net"), allow(dead_code))]
pub struct GamepadPoller {
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

#[cfg_attr(not(feature = "net"), allow(dead_code))]
impl GamepadPoller {
    /// Start polling local controllers at ~250 Hz (4 ms), sending changed states to `tx`
    /// and updating `active_count`.
    pub fn start(tx: mpsc::UnboundedSender<GamepadState>, active_count: Arc<AtomicU8>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let join = std::thread::Builder::new()
            .name("viewer-gamepad".into())
            .spawn(move || poll_loop(tx, active_count, stop_bg))
            .ok();
        Self { stop, join }
    }

    /// Apply force-feedback motor speeds (`0..=65535`) to physical controller `r.pad_id`.
    pub fn apply_rumble(r: &Rumble) {
        platform::set_rumble(r.pad_id, r.low_freq.min(u16::MAX as u32) as u16, r.high_freq.min(u16::MAX as u32) as u16);
    }

    /// Silence all motors and stop the polling thread.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        for pad in 0..MAX_GAMEPADS as u32 {
            platform::set_rumble(pad, 0, 0);
        }
    }
}

impl Drop for GamepadPoller {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

fn poll_loop(tx: mpsc::UnboundedSender<GamepadState>, active_count: Arc<AtomicU8>, stop: Arc<AtomicBool>) {
    let mut tracker = GamepadTracker::default();
    while !stop.load(Ordering::Relaxed) {
        let snaps = platform::poll_all();
        let (events, active) = tracker.update(&snaps);
        active_count.store(active, Ordering::Relaxed);
        for ev in events {
            if tx.send(ev).is_err() {
                return;
            }
        }
        // Poll faster (4 ms = 250 Hz) when a pad is attached, slower (250 ms) while idle.
        let sleep_ms = if active > 0 { 4 } else { 250 };
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }
    for ev in tracker.disconnect_all() {
        let _ = tx.send(ev);
    }
    active_count.store(0, Ordering::Relaxed);
}

#[cfg(windows)]
mod platform {
    use windows::Win32::UI::Input::XboxController::{XINPUT_STATE, XINPUT_VIBRATION, XInputGetState, XInputSetState};

    use super::{MAX_GAMEPADS, PadSnapshot};

    pub fn poll_all() -> [PadSnapshot; MAX_GAMEPADS] {
        let mut out = [PadSnapshot::default(); MAX_GAMEPADS];
        for (i, slot) in out.iter_mut().enumerate() {
            let mut state = XINPUT_STATE::default();
            // SAFETY: XInputGetState writes into a stack XINPUT_STATE for slot 0..4.
            let err = unsafe { XInputGetState(i as u32, &mut state) };
            if err == 0 {
                let g = state.Gamepad;
                *slot = PadSnapshot {
                    connected: true,
                    buttons: g.wButtons.0 as u32,
                    left_trigger: g.bLeftTrigger as u32,
                    right_trigger: g.bRightTrigger as u32,
                    left_x: g.sThumbLX as i32,
                    left_y: g.sThumbLY as i32,
                    right_x: g.sThumbRX as i32,
                    right_y: g.sThumbRY as i32,
                };
            }
        }
        out
    }

    pub fn set_rumble(pad_id: u32, low_freq: u16, high_freq: u16) {
        if pad_id >= MAX_GAMEPADS as u32 {
            return;
        }
        let vib = XINPUT_VIBRATION { wLeftMotorSpeed: low_freq, wRightMotorSpeed: high_freq };
        // SAFETY: valid XINPUT_VIBRATION pointer for pad_id < 4.
        unsafe {
            let _ = XInputSetState(pad_id, &vib);
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::{MAX_GAMEPADS, PadSnapshot};

    pub fn poll_all() -> [PadSnapshot; MAX_GAMEPADS] {
        [PadSnapshot::default(); MAX_GAMEPADS]
    }

    pub fn set_rumble(_pad_id: u32, _low_freq: u16, _high_freq: u16) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_emits_changes_heartbeats_and_disconnects() {
        let mut tr = GamepadTracker::default();
        let idle = [PadSnapshot::default(); MAX_GAMEPADS];
        let (evs, active) = tr.update(&idle);
        assert!(evs.is_empty());
        assert_eq!(active, 0);

        let mut two = idle;
        two[0] = PadSnapshot { connected: true, buttons: 0x1000, left_trigger: 200, left_x: 12000, ..Default::default() };
        two[2] = PadSnapshot { connected: true, buttons: 0x2000, right_y: -8000, ..Default::default() };
        let (evs, active) = tr.update(&two);
        assert_eq!(active, 2);
        assert_eq!(evs.len(), 2);
        assert_eq!((evs[0].pad_id, evs[0].seq, evs[0].buttons), (0, 1, 0x1000));
        assert_eq!((evs[1].pad_id, evs[1].seq, evs[1].buttons), (2, 1, 0x2000));

        // Same state produces no immediate packet until heartbeat threshold.
        for _ in 0..(GamepadTracker::HEARTBEAT_TICKS - 1) {
            let (evs, active) = tr.update(&two);
            assert!(evs.is_empty());
            assert_eq!(active, 2);
        }
        let (evs, _) = tr.update(&two);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].seq, 2);
        assert_eq!(evs[1].seq, 2);

        // Disconnect all emits connected=false with incremented seq.
        let disc = tr.disconnect_all();
        assert_eq!(disc.len(), 2);
        assert!(disc.iter().all(|s| !s.connected && s.seq == 3));
    }
}
