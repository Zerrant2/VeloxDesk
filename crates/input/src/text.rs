//! Text helpers for backends that can only press keys (uinput): printable ASCII on a US
//! layout → HID usage + shift.

/// US-layout printable ASCII (plus `\n`, `\t`) → `(HID usage, shift)`.
pub fn ascii_to_hid(ch: char) -> Option<(u32, bool)> {
    Some(match ch {
        'a'..='z' => (0x04 + (ch as u32 - 'a' as u32), false),
        'A'..='Z' => (0x04 + (ch as u32 - 'A' as u32), true),
        '1'..='9' => (0x1E + (ch as u32 - '1' as u32), false),
        '0' => (0x27, false),
        '\n' => (0x28, false),
        '\t' => (0x2B, false),
        ' ' => (0x2C, false),
        '!' => (0x1E, true),
        '@' => (0x1F, true),
        '#' => (0x20, true),
        '$' => (0x21, true),
        '%' => (0x22, true),
        '^' => (0x23, true),
        '&' => (0x24, true),
        '*' => (0x25, true),
        '(' => (0x26, true),
        ')' => (0x27, true),
        '-' => (0x2D, false),
        '_' => (0x2D, true),
        '=' => (0x2E, false),
        '+' => (0x2E, true),
        '[' => (0x2F, false),
        '{' => (0x2F, true),
        ']' => (0x30, false),
        '}' => (0x30, true),
        '\\' => (0x31, false),
        '|' => (0x31, true),
        ';' => (0x33, false),
        ':' => (0x33, true),
        '\'' => (0x34, false),
        '"' => (0x34, true),
        '`' => (0x35, false),
        '~' => (0x35, true),
        ',' => (0x36, false),
        '<' => (0x36, true),
        '.' => (0x37, false),
        '>' => (0x37, true),
        '/' => (0x38, false),
        '?' => (0x38, true),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_coverage() {
        for c in ' '..='~' {
            assert!(ascii_to_hid(c).is_some(), "{c:?}");
        }
        assert_eq!(ascii_to_hid('a'), Some((0x04, false)));
        assert_eq!(ascii_to_hid('Z'), Some((0x1D, true)));
        assert_eq!(ascii_to_hid('0'), Some((0x27, false)));
        assert_eq!(ascii_to_hid('?'), Some((0x38, true)));
        assert_eq!(ascii_to_hid('é'), None);
    }
}
