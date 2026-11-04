//! scrcpy-server v5.0.1 control messages (client → device), big-endian.
//! Layouts mirror app/src/control_msg.c of scrcpy v5.0.1.

const INJECT_KEYCODE: u8 = 0;
const INJECT_TEXT: u8 = 1;
const INJECT_TOUCH_EVENT: u8 = 2;
const INJECT_SCROLL_EVENT: u8 = 3;
const BACK_OR_SCREEN_ON: u8 = 4;
const EXPAND_NOTIFICATION_PANEL: u8 = 5;
const SET_CLIPBOARD: u8 = 9;
const SET_DISPLAY_POWER: u8 = 10;
const ROTATE_DEVICE: u8 = 11;

pub const TEXT_MAX_LEN: usize = 300;
pub const POINTER_ID_FINGER: u64 = u64::MAX - 1; // SC_POINTER_ID_GENERIC_FINGER (-2)

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Down = 0,
    Up = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionAction {
    Down = 0,
    Up = 1,
    Move = 2,
}

/// Device-space position plus the video size it refers to (server rescales).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    pub x: i32,
    pub y: i32,
    pub width: u16,
    pub height: u16,
}

fn put_position(buf: &mut Vec<u8>, p: Position) {
    buf.extend_from_slice(&p.x.to_be_bytes());
    buf.extend_from_slice(&p.y.to_be_bytes());
    buf.extend_from_slice(&p.width.to_be_bytes());
    buf.extend_from_slice(&p.height.to_be_bytes());
}

/// [0, 1] → u16 fixed point (1.0 saturates to 0xffff).
fn u16_fixed(f: f32) -> u16 {
    let v = (f.clamp(0.0, 1.0) * 65536.0) as u32;
    v.min(0xffff) as u16
}

/// [-1, 1] → i16 fixed point (1.0 saturates to 0x7fff).
fn i16_fixed(f: f32) -> i16 {
    let v = (f.clamp(-1.0, 1.0) * 32768.0) as i32;
    v.clamp(-0x8000, 0x7fff) as i16
}

/// Longest prefix of `s` that is at most `max` bytes and ends on a char boundary.
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub fn keycode(action: KeyAction, keycode: u32, repeat: u32, metastate: u32) -> Vec<u8> {
    let mut b = vec![INJECT_KEYCODE, action as u8];
    b.extend_from_slice(&keycode.to_be_bytes());
    b.extend_from_slice(&repeat.to_be_bytes());
    b.extend_from_slice(&metastate.to_be_bytes());
    b
}

pub fn text(s: &str) -> Vec<u8> {
    let s = truncate_utf8(s, TEXT_MAX_LEN);
    let mut b = vec![INJECT_TEXT];
    b.extend_from_slice(&(s.len() as u32).to_be_bytes());
    b.extend_from_slice(s.as_bytes());
    b
}

pub fn touch(
    action: MotionAction,
    pointer_id: u64,
    pos: Position,
    pressure: f32,
    action_button: u32,
    buttons: u32,
) -> Vec<u8> {
    let mut b = vec![INJECT_TOUCH_EVENT, action as u8];
    b.extend_from_slice(&pointer_id.to_be_bytes());
    put_position(&mut b, pos);
    b.extend_from_slice(&u16_fixed(pressure).to_be_bytes());
    b.extend_from_slice(&action_button.to_be_bytes());
    b.extend_from_slice(&buttons.to_be_bytes());
    b
}

/// `hscroll`/`vscroll` in "notches", range [-16, 16].
pub fn scroll(pos: Position, hscroll: f32, vscroll: f32, buttons: u32) -> Vec<u8> {
    let mut b = vec![INJECT_SCROLL_EVENT];
    put_position(&mut b, pos);
    b.extend_from_slice(&i16_fixed(hscroll / 16.0).to_be_bytes());
    b.extend_from_slice(&i16_fixed(vscroll / 16.0).to_be_bytes());
    b.extend_from_slice(&buttons.to_be_bytes());
    b
}

pub fn back_or_screen_on(action: KeyAction) -> Vec<u8> {
    vec![BACK_OR_SCREEN_ON, action as u8]
}

pub fn expand_notification_panel() -> Vec<u8> {
    vec![EXPAND_NOTIFICATION_PANEL]
}

pub fn set_display_power(on: bool) -> Vec<u8> {
    vec![SET_DISPLAY_POWER, u8::from(on)]
}

pub fn rotate_device() -> Vec<u8> {
    vec![ROTATE_DEVICE]
}

pub fn set_clipboard(sequence: u64, paste: bool, s: &str) -> Vec<u8> {
    let mut b = vec![SET_CLIPBOARD];
    b.extend_from_slice(&sequence.to_be_bytes());
    b.push(u8::from(paste));
    b.extend_from_slice(&(s.len() as u32).to_be_bytes());
    b.extend_from_slice(s.as_bytes());
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    const POS: Position = Position {
        x: 100,
        y: 200,
        width: 1080,
        height: 1920,
    };

    #[test]
    fn keycode_layout() {
        assert_eq!(
            keycode(KeyAction::Up, 66, 5, 0x41),
            [0, 1, 0, 0, 0, 66, 0, 0, 0, 5, 0, 0, 0, 0x41]
        );
    }

    #[test]
    fn text_layout_and_utf8_truncation() {
        assert_eq!(text("hi"), [1, 0, 0, 0, 2, b'h', b'i']);
        let long = "中".repeat(200); // 600 bytes
        let msg = text(&long);
        let len = u32::from_be_bytes(msg[1..5].try_into().unwrap()) as usize;
        assert_eq!(len, 300);
        assert!(std::str::from_utf8(&msg[5..]).is_ok());
    }

    #[test]
    fn touch_layout() {
        let msg = touch(MotionAction::Down, POINTER_ID_FINGER, POS, 1.0, 0, 0);
        assert_eq!(msg.len(), 32);
        assert_eq!(msg[0], 2);
        assert_eq!(msg[1], 0);
        assert_eq!(&msg[2..10], &(u64::MAX - 1).to_be_bytes());
        assert_eq!(&msg[10..14], &100i32.to_be_bytes());
        assert_eq!(&msg[14..18], &200i32.to_be_bytes());
        assert_eq!(&msg[18..20], &1080u16.to_be_bytes());
        assert_eq!(&msg[20..22], &1920u16.to_be_bytes());
        assert_eq!(&msg[22..24], &[0xff, 0xff]);
        assert_eq!(&msg[24..32], &[0; 8]);
    }

    #[test]
    fn scroll_layout() {
        let msg = scroll(POS, 16.0, -16.0, 0);
        assert_eq!(msg.len(), 21);
        assert_eq!(msg[0], 3);
        assert_eq!(&msg[13..15], &0x7fffi16.to_be_bytes());
        assert_eq!(&msg[15..17], &(-0x8000i16).to_be_bytes());
    }

    #[test]
    fn small_messages() {
        assert_eq!(back_or_screen_on(KeyAction::Down), [4, 0]);
        assert_eq!(set_display_power(false), [10, 0]);
        assert_eq!(rotate_device(), [11]);
        assert_eq!(expand_notification_panel(), [5]);
    }

    #[test]
    fn clipboard_layout() {
        let msg = set_clipboard(7, true, "ab");
        assert_eq!(msg, [9, 0, 0, 0, 0, 0, 0, 0, 7, 1, 0, 0, 0, 2, b'a', b'b']);
    }

    #[test]
    fn fixed_point_conversions() {
        assert_eq!(u16_fixed(0.5), 0x8000);
        assert_eq!(u16_fixed(1.0), 0xffff);
        assert_eq!(i16_fixed(0.5), 0x4000);
        assert_eq!(i16_fixed(-1.0), -0x8000);
    }
}
