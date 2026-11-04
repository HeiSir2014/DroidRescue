//! egui input → Android input mapping.

use eframe::egui::{Key, MouseWheelUnit, Vec2};

/// Android keycodes for keys that don't arrive as text.
pub fn android_keycode(key: Key) -> Option<u32> {
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
    notches.clamp(Vec2::splat(-16.0), Vec2::splat(16.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::vec2;

    #[test]
    fn maps_editing_and_navigation_keys() {
        assert_eq!(android_keycode(Key::Enter), Some(66));
        assert_eq!(android_keycode(Key::Backspace), Some(67));
        assert_eq!(android_keycode(Key::ArrowLeft), Some(21));
        assert_eq!(
            android_keycode(Key::A),
            None,
            "letters come through as text"
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
