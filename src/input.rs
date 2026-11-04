//! egui input → Android input mapping.

use eframe::egui::{Key, Modifiers, MouseWheelUnit, Vec2};

/// Android keycodes for keys that don't arrive as text. With Ctrl or Alt
/// held, letters and digits arrive only as keys (no text event), so they
/// map too: Ctrl+A selects all, Ctrl+Z undoes, …
pub fn android_keycode(key: Key, modifiers: Modifiers) -> Option<u32> {
    if modifiers.ctrl || modifiers.alt {
        let name = key.name();
        if let [c] = name.as_bytes() {
            match c {
                b'A'..=b'Z' => return Some(29 + u32::from(c - b'A')), // KEYCODE_A..Z
                b'0'..=b'9' => return Some(7 + u32::from(c - b'0')),  // KEYCODE_0..9
                _ => {}
            }
        }
    }
    Some(match key {
        Key::Enter => 66,
        Key::Backspace => 67,
        Key::Delete => 112,
        Key::Tab => 61,
        Key::Escape => 111,
        Key::ArrowUp => 19,
        Key::ArrowDown => 20,
        Key::ArrowLeft => 21,
        Key::ArrowRight => 22,
        Key::Home => 122,
        Key::End => 123,
        Key::PageUp => 92,
        Key::PageDown => 93,
        _ => return None,
    })
}

/// Android `KeyEvent` meta state for the held modifiers (the generic
/// flag plus the left-key flag, as a real keyboard reports them).
pub fn metastate(m: Modifiers) -> u32 {
    let mut meta = 0;
    if m.shift {
        meta |= 0x01 | 0x40; // META_SHIFT_ON | META_SHIFT_LEFT_ON
    }
    if m.alt {
        meta |= 0x02 | 0x10; // META_ALT_ON | META_ALT_LEFT_ON
    }
    if m.ctrl {
        meta |= 0x1000 | 0x2000; // META_CTRL_ON | META_CTRL_LEFT_ON
    }
    meta
}

/// HID keyboard usages for non-text keys (rescue-mode keyboard passthrough).
pub fn hid_usage(key: Key) -> Option<u8> {
    use crate::aoa;
    Some(match key {
        Key::Enter => aoa::KEY_ENTER,
        Key::Escape => aoa::KEY_ESCAPE,
        Key::Backspace => aoa::KEY_BACKSPACE,
        Key::Tab => aoa::KEY_TAB,
        Key::ArrowRight => aoa::KEY_RIGHT,
        Key::ArrowLeft => aoa::KEY_LEFT,
        Key::ArrowDown => aoa::KEY_DOWN,
        Key::ArrowUp => aoa::KEY_UP,
        _ => return None,
    })
}

/// Wheel delta → scroll "notches" (scrcpy expects ±1 per notch, max ±16).
pub fn wheel_notches(unit: MouseWheelUnit, delta: Vec2) -> Vec2 {
    let notches = match unit {
        MouseWheelUnit::Line => delta,
        MouseWheelUnit::Point => delta / 50.0,
        MouseWheelUnit::Page => delta * 10.0,
    };
    // egui's delta moves the content (+x = content right = view scrolls
    // left); Android's AXIS_HSCROLL is +1 = scroll right. Vertical agrees.
    let notches = Vec2::new(-notches.x, notches.y);
    notches.clamp(Vec2::splat(-16.0), Vec2::splat(16.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;

    #[test]
    fn maps_editing_and_navigation_keys() {
        let none = Modifiers::NONE;
        assert_eq!(android_keycode(Key::Enter, none), Some(66));
        assert_eq!(android_keycode(Key::Backspace, none), Some(67));
        assert_eq!(android_keycode(Key::ArrowLeft, none), Some(21));
        assert_eq!(
            android_keycode(Key::A, none),
            None,
            "letters come through as text"
        );
        assert_eq!(
            android_keycode(Key::A, Modifiers::SHIFT),
            None,
            "shifted letters are text too"
        );
    }

    #[test]
    fn ctrl_letters_and_digits_become_keycodes() {
        assert_eq!(android_keycode(Key::A, Modifiers::CTRL), Some(29));
        assert_eq!(android_keycode(Key::Z, Modifiers::CTRL), Some(54));
        assert_eq!(android_keycode(Key::Num0, Modifiers::ALT), Some(7));
        assert_eq!(android_keycode(Key::Num9, Modifiers::CTRL), Some(16));
    }

    #[test]
    fn modifiers_become_android_meta_state() {
        assert_eq!(metastate(Modifiers::NONE), 0);
        assert_eq!(metastate(Modifiers::CTRL), 0x3000);
        assert_eq!(metastate(Modifiers::SHIFT | Modifiers::ALT), 0x53);
    }

    #[test]
    fn horizontal_wheel_follows_android_direction() {
        // egui +x = content right = scrolling left = Android hscroll -1.
        assert_eq!(
            wheel_notches(MouseWheelUnit::Line, vec2(1.0, 0.0)),
            vec2(-1.0, 0.0)
        );
    }

    #[test]
    fn maps_navigation_keys_to_hid_usages() {
        assert_eq!(hid_usage(Key::Enter), Some(0x28));
        assert_eq!(hid_usage(Key::ArrowDown), Some(0x51));
        assert_eq!(hid_usage(Key::Tab), Some(0x2B));
        assert_eq!(hid_usage(Key::A), None, "letters arrive as text");
    }

    #[test]
    fn wheel_units_become_notches() {
        assert_eq!(
            wheel_notches(MouseWheelUnit::Line, vec2(0.0, -1.0)),
            vec2(0.0, -1.0)
        );
        assert_eq!(
            wheel_notches(MouseWheelUnit::Point, vec2(0.0, 100.0)),
            vec2(0.0, 2.0)
        );
        assert_eq!(
            wheel_notches(MouseWheelUnit::Page, vec2(0.0, 5.0)),
            vec2(0.0, 16.0)
        );
    }
}
