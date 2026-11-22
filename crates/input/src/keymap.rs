//! USB HID usage (keyboard page 0x07) → platform key code tables.
//!
//! * Windows: PS/2 "set 1" scan codes as used by `SendInput` with `KEYEVENTF_SCANCODE`.
//!   Values `0xE0xx` are extended keys (`KEYEVENTF_EXTENDEDKEY` + low byte).
//!   Values follow Chromium's `dom_code_data.inc` (e.g. NumLock = `0xE045`, Pause = `0x45`).
//! * Linux: evdev `KEY_*` codes (`linux/input-event-codes.h`).
//!
//! The tables are plain `const fn` matches so they are usable (and unit-tested) on every OS.

/// HID usage of Left Control (first modifier).
pub const HID_LEFT_CTRL: u32 = 0xE0;
/// HID usage of Right GUI (last modifier).
pub const HID_RIGHT_GUI: u32 = 0xE7;
/// HID usage of Pause.
pub const HID_PAUSE: u32 = 0x48;

/// Windows set-1 scan code for a HID usage. `0xE0xx` = extended key. `None` = unmapped.
pub const fn hid_to_win_scancode(usage: u32) -> Option<u16> {
    Some(match usage {
        // letters a..z
        0x04 => 0x1E, // A
        0x05 => 0x30, // B
        0x06 => 0x2E, // C
        0x07 => 0x20, // D
        0x08 => 0x12, // E
        0x09 => 0x21, // F
        0x0A => 0x22, // G
        0x0B => 0x23, // H
        0x0C => 0x17, // I
        0x0D => 0x24, // J
        0x0E => 0x25, // K
        0x0F => 0x26, // L
        0x10 => 0x32, // M
        0x11 => 0x31, // N
        0x12 => 0x18, // O
        0x13 => 0x19, // P
        0x14 => 0x10, // Q
        0x15 => 0x13, // R
        0x16 => 0x1F, // S
        0x17 => 0x14, // T
        0x18 => 0x16, // U
        0x19 => 0x2F, // V
        0x1A => 0x11, // W
        0x1B => 0x2D, // X
        0x1C => 0x15, // Y
        0x1D => 0x2C, // Z
        // digits 1..0
        0x1E => 0x02,
        0x1F => 0x03,
        0x20 => 0x04,
        0x21 => 0x05,
        0x22 => 0x06,
        0x23 => 0x07,
        0x24 => 0x08,
        0x25 => 0x09,
        0x26 => 0x0A,
        0x27 => 0x0B,
        0x28 => 0x1C, // Enter
        0x29 => 0x01, // Escape
        0x2A => 0x0E, // Backspace
        0x2B => 0x0F, // Tab
        0x2C => 0x39, // Space
        0x2D => 0x0C, // - _
        0x2E => 0x0D, // = +
        0x2F => 0x1A, // [ {
        0x30 => 0x1B, // ] }
        0x31 => 0x2B, // \ |
        0x32 => 0x2B, // Non-US # ~ (same position as backslash)
        0x33 => 0x27, // ; :
        0x34 => 0x28, // ' "
        0x35 => 0x29, // ` ~
        0x36 => 0x33, // , <
        0x37 => 0x34, // . >
        0x38 => 0x35, // / ?
        0x39 => 0x3A, // Caps Lock
        // F1..F12
        0x3A => 0x3B,
        0x3B => 0x3C,
        0x3C => 0x3D,
        0x3D => 0x3E,
        0x3E => 0x3F,
        0x3F => 0x40,
        0x40 => 0x41,
        0x41 => 0x42,
        0x42 => 0x43,
        0x43 => 0x44,
        0x44 => 0x57,
        0x45 => 0x58,
        0x46 => 0xE037, // Print Screen
        0x47 => 0x46,   // Scroll Lock
        0x48 => 0x45,   // Pause (Windows maps plain 0x45 to VK_PAUSE)
        0x49 => 0xE052, // Insert
        0x4A => 0xE047, // Home
        0x4B => 0xE049, // Page Up
        0x4C => 0xE053, // Delete
        0x4D => 0xE04F, // End
        0x4E => 0xE051, // Page Down
        0x4F => 0xE04D, // Right
        0x50 => 0xE04B, // Left
        0x51 => 0xE050, // Down
        0x52 => 0xE048, // Up
        0x53 => 0xE045, // Num Lock (Windows treats it as extended; Pause is plain 0x45)
        0x54 => 0xE035, // KP /
        0x55 => 0x37,   // KP *
        0x56 => 0x4A,   // KP -
        0x57 => 0x4E,   // KP +
        0x58 => 0xE01C, // KP Enter
        0x59 => 0x4F,   // KP 1
        0x5A => 0x50,   // KP 2
        0x5B => 0x51,   // KP 3
        0x5C => 0x4B,   // KP 4
        0x5D => 0x4C,   // KP 5
        0x5E => 0x4D,   // KP 6
        0x5F => 0x47,   // KP 7
        0x60 => 0x48,   // KP 8
        0x61 => 0x49,   // KP 9
        0x62 => 0x52,   // KP 0
        0x63 => 0x53,   // KP .
        0x64 => 0x56,   // Non-US \ |
        0x65 => 0xE05D, // Application (Menu)
        0x66 => 0xE05E, // Power
        0x67 => 0x59,   // KP =
        // F13..F24
        0x68 => 0x64,
        0x69 => 0x65,
        0x6A => 0x66,
        0x6B => 0x67,
        0x6C => 0x68,
        0x6D => 0x69,
        0x6E => 0x6A,
        0x6F => 0x6B,
        0x70 => 0x6C,
        0x71 => 0x6D,
        0x72 => 0x6E,
        0x73 => 0x76,
        0x7F => 0xE020, // Mute
        0x80 => 0xE030, // Volume Up
        0x81 => 0xE02E, // Volume Down
        0x85 => 0x7E,   // KP , (Brazilian)
        0x87 => 0x73,   // International1 (Ro)
        0x88 => 0x70,   // International2 (Katakana/Hiragana)
        0x89 => 0x7D,   // International3 (Yen)
        0x8A => 0x79,   // International4 (Henkan)
        0x8B => 0x7B,   // International5 (Muhenkan)
        0x90 => 0x72,   // LANG1 (Hangul/English)
        0x91 => 0x71,   // LANG2 (Hanja)
        // modifiers
        0xE0 => 0x1D,   // Left Ctrl
        0xE1 => 0x2A,   // Left Shift
        0xE2 => 0x38,   // Left Alt
        0xE3 => 0xE05B, // Left GUI
        0xE4 => 0xE01D, // Right Ctrl
        0xE5 => 0x36,   // Right Shift
        0xE6 => 0xE038, // Right Alt (AltGr)
        0xE7 => 0xE05C, // Right GUI
        _ => return None,
    })
}

/// Linux evdev key code for a HID usage. `None` = unmapped.
pub const fn hid_to_evdev(usage: u32) -> Option<u16> {
    Some(match usage {
        0x04 => 30, // KEY_A
        0x05 => 48, // KEY_B
        0x06 => 46, // KEY_C
        0x07 => 32, // KEY_D
        0x08 => 18, // KEY_E
        0x09 => 33, // KEY_F
        0x0A => 34, // KEY_G
        0x0B => 35, // KEY_H
        0x0C => 23, // KEY_I
        0x0D => 36, // KEY_J
        0x0E => 37, // KEY_K
        0x0F => 38, // KEY_L
        0x10 => 50, // KEY_M
        0x11 => 49, // KEY_N
        0x12 => 24, // KEY_O
        0x13 => 25, // KEY_P
        0x14 => 16, // KEY_Q
        0x15 => 19, // KEY_R
        0x16 => 31, // KEY_S
        0x17 => 20, // KEY_T
        0x18 => 22, // KEY_U
        0x19 => 47, // KEY_V
        0x1A => 17, // KEY_W
        0x1B => 45, // KEY_X
        0x1C => 21, // KEY_Y
        0x1D => 44, // KEY_Z
        0x1E => 2,  // KEY_1
        0x1F => 3,
        0x20 => 4,
        0x21 => 5,
        0x22 => 6,
        0x23 => 7,
        0x24 => 8,
        0x25 => 9,
        0x26 => 10,
        0x27 => 11, // KEY_0
        0x28 => 28, // KEY_ENTER
        0x29 => 1,  // KEY_ESC
        0x2A => 14, // KEY_BACKSPACE
        0x2B => 15, // KEY_TAB
        0x2C => 57, // KEY_SPACE
        0x2D => 12, // KEY_MINUS
        0x2E => 13, // KEY_EQUAL
        0x2F => 26, // KEY_LEFTBRACE
        0x30 => 27, // KEY_RIGHTBRACE
        0x31 => 43, // KEY_BACKSLASH
        0x32 => 43, // Non-US # (KEY_BACKSLASH position)
        0x33 => 39, // KEY_SEMICOLON
        0x34 => 40, // KEY_APOSTROPHE
        0x35 => 41, // KEY_GRAVE
        0x36 => 51, // KEY_COMMA
        0x37 => 52, // KEY_DOT
        0x38 => 53, // KEY_SLASH
        0x39 => 58, // KEY_CAPSLOCK
        0x3A => 59, // KEY_F1
        0x3B => 60,
        0x3C => 61,
        0x3D => 62,
        0x3E => 63,
        0x3F => 64,
        0x40 => 65,
        0x41 => 66,
        0x42 => 67,
        0x43 => 68,  // KEY_F10
        0x44 => 87,  // KEY_F11
        0x45 => 88,  // KEY_F12
        0x46 => 99,  // KEY_SYSRQ
        0x47 => 70,  // KEY_SCROLLLOCK
        0x48 => 119, // KEY_PAUSE
        0x49 => 110, // KEY_INSERT
        0x4A => 102, // KEY_HOME
        0x4B => 104, // KEY_PAGEUP
        0x4C => 111, // KEY_DELETE
        0x4D => 107, // KEY_END
        0x4E => 109, // KEY_PAGEDOWN
        0x4F => 106, // KEY_RIGHT
        0x50 => 105, // KEY_LEFT
        0x51 => 108, // KEY_DOWN
        0x52 => 103, // KEY_UP
        0x53 => 69,  // KEY_NUMLOCK
        0x54 => 98,  // KEY_KPSLASH
        0x55 => 55,  // KEY_KPASTERISK
        0x56 => 74,  // KEY_KPMINUS
        0x57 => 78,  // KEY_KPPLUS
        0x58 => 96,  // KEY_KPENTER
        0x59 => 79,  // KEY_KP1
        0x5A => 80,
        0x5B => 81,
        0x5C => 75,
        0x5D => 76,
        0x5E => 77,
        0x5F => 71,
        0x60 => 72,
        0x61 => 73,  // KEY_KP9
        0x62 => 82,  // KEY_KP0
        0x63 => 83,  // KEY_KPDOT
        0x64 => 86,  // KEY_102ND
        0x65 => 127, // KEY_COMPOSE
        0x66 => 116, // KEY_POWER
        0x67 => 117, // KEY_KPEQUAL
        0x68 => 183, // KEY_F13
        0x69 => 184,
        0x6A => 185,
        0x6B => 186,
        0x6C => 187,
        0x6D => 188,
        0x6E => 189,
        0x6F => 190,
        0x70 => 191,
        0x71 => 192,
        0x72 => 193,
        0x73 => 194, // KEY_F24
        0x7F => 113, // KEY_MUTE
        0x80 => 115, // KEY_VOLUMEUP
        0x81 => 114, // KEY_VOLUMEDOWN
        0x85 => 121, // KEY_KPCOMMA
        0x87 => 89,  // KEY_RO
        0x88 => 93,  // KEY_KATAKANAHIRAGANA
        0x89 => 124, // KEY_YEN
        0x8A => 92,  // KEY_HENKAN
        0x8B => 94,  // KEY_MUHENKAN
        0x90 => 122, // KEY_HANGEUL
        0x91 => 123, // KEY_HANJA
        0xE0 => 29,  // KEY_LEFTCTRL
        0xE1 => 42,  // KEY_LEFTSHIFT
        0xE2 => 56,  // KEY_LEFTALT
        0xE3 => 125, // KEY_LEFTMETA
        0xE4 => 97,  // KEY_RIGHTCTRL
        0xE5 => 54,  // KEY_RIGHTSHIFT
        0xE6 => 100, // KEY_RIGHTALT
        0xE7 => 126, // KEY_RIGHTMETA
        _ => return None,
    })
}

/// Largest evdev key code produced by [`hid_to_evdev`] (for uinput `UI_SET_KEYBIT`).
pub const EVDEV_MAX_KEY: u16 = 194;

/// Iterate every HID usage that has a mapping on the Windows table.
pub fn mapped_hid_usages() -> impl Iterator<Item = u32> {
    (0u32..=0xE7).filter(|u| hid_to_win_scancode(*u).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn letters_and_digits() {
        assert_eq!(hid_to_win_scancode(0x04), Some(0x1E)); // A
        assert_eq!(hid_to_win_scancode(0x1D), Some(0x2C)); // Z
        assert_eq!(hid_to_win_scancode(0x27), Some(0x0B)); // 0
        assert_eq!(hid_to_evdev(0x04), Some(30));
        assert_eq!(hid_to_evdev(0x1D), Some(44));
        assert_eq!(hid_to_evdev(0x27), Some(11));
        for u in 0x04..=0x27 {
            assert!(hid_to_win_scancode(u).is_some(), "win {u:#x}");
            assert!(hid_to_evdev(u).is_some(), "evdev {u:#x}");
        }
    }

    #[test]
    fn extended_keys() {
        for (u, sc) in [
            (0x4F, 0xE04D), // Right
            (0x50, 0xE04B),
            (0x51, 0xE050),
            (0x52, 0xE048),
            (0x49, 0xE052), // Insert
            (0x4C, 0xE053), // Delete
            (0x58, 0xE01C), // KP Enter
            (0x54, 0xE035), // KP /
            (0xE4, 0xE01D), // RCtrl
            (0xE6, 0xE038), // RAlt
            (0xE3, 0xE05B), // LWin
        ] {
            assert_eq!(hid_to_win_scancode(u), Some(sc), "{u:#x}");
        }
        // non-extended counterparts
        assert_eq!(hid_to_win_scancode(0xE0), Some(0x1D)); // LCtrl
        assert_eq!(hid_to_win_scancode(0x28), Some(0x1C)); // Enter
    }

    #[test]
    fn unmapped() {
        for u in [0x00, 0x01, 0x02, 0x03, 0xA5, 0xDF, 0xE8, 0xFFFF] {
            assert_eq!(hid_to_win_scancode(u), None);
            assert_eq!(hid_to_evdev(u), None);
        }
    }

    #[test]
    fn tables_cover_same_usages_and_are_unique() {
        let mut win_seen: HashMap<u16, u32> = HashMap::new();
        let mut ev_seen: HashMap<u16, u32> = HashMap::new();
        // Usages that intentionally alias another key (same physical position).
        let aliases = [0x32u32];
        for u in 0..=0xFFu32 {
            let w = hid_to_win_scancode(u);
            let e = hid_to_evdev(u);
            assert_eq!(w.is_some(), e.is_some(), "coverage mismatch at {u:#x}");
            if aliases.contains(&u) {
                continue;
            }
            if let Some(w) = w {
                assert!(win_seen.insert(w, u).is_none(), "dup win scancode {w:#x} at {u:#x}");
            }
            if let Some(e) = e {
                assert!(e <= EVDEV_MAX_KEY);
                assert!(ev_seen.insert(e, u).is_none(), "dup evdev {e} at {u:#x}");
            }
        }
        assert!(mapped_hid_usages().count() > 120);
    }
}
