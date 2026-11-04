//! Rescue mode: Android Open Accessory v2 HID over USB control transfers.
//! Lets the PC act as a USB keyboard / pen / media keys for the tablet with
//! **no USB debugging and no authorisation** (works on the lock screen) —
//! the mechanism behind `scrcpy --otg`. Transport: native WinUSB on the
//! tablet's MI_01 only (see usbdev.rs) — MTP and adb keep their interfaces.

use crate::usbdev::UsbDevice;
use std::time::Duration;

const REQ_GET_PROTOCOL: u8 = 51;
const REQ_REGISTER_HID: u8 = 54;
const REQ_UNREGISTER_HID: u8 = 55;
const REQ_SET_HID_REPORT_DESC: u8 = 56;
const REQ_SEND_HID_EVENT: u8 = 57;
const VENDOR_OUT: u8 = 0x40;
const VENDOR_IN: u8 = 0xC0;

pub const HID_KEYBOARD: u16 = 1;
pub const HID_MOUSE: u16 = 2;
pub const HID_CONSUMER: u16 = 3;

/// Boot keyboard: [modifiers, reserved, 6 key usages].
pub const KEYBOARD_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, // Generic Desktop / Keyboard / Application
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81,
    0x02, // modifiers
    0x75, 0x08, 0x95, 0x01, 0x81, 0x01, // reserved byte
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x75, 0x01, 0x95, 0x05, 0x91, 0x02, // LEDs
    0x75, 0x03, 0x95, 0x01, 0x91, 0x01, // LED padding
    0x05, 0x07, 0x15, 0x00, 0x25, 0x65, 0x19, 0x00, 0x29, 0x65, 0x75, 0x08, 0x95, 0x06, 0x81,
    0x00, // keys
    0xC0,
];

/// Relative mouse: [buttons, dx, dy, wheel].
pub const MOUSE_DESC: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, // Mouse / Pointer
    0x05, 0x09, 0x19, 0x01, 0x29, 0x05, 0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81,
    0x02, // 5 buttons
    0x95, 0x01, 0x75, 0x03, 0x81, 0x01, // padding
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x03,
    0x81, 0x06, // X, Y, wheel (relative)
    0xC0, 0xC0,
];

/// Consumer control: one 16-bit usage (AC Home 0x223, AC Back 0x224, …).
pub const CONSUMER_DESC: &[u8] = &[
    0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x03, 0x19, 0x00, 0x2A, 0xFF, 0x03,
    0x75, 0x10, 0x95, 0x01, 0x81, 0x00, 0xC0,
];

pub const HID_TOUCH: u16 = 4;

/// Absolute pen digitizer: [flags (bit0 tip, bit1 in-range), x u16 LE,
/// y u16 LE], both 0..=TOUCH_MAX across the whole screen.
///
/// Why "Pen" (0x02) and not "Touch Screen" (0x04): Linux hid-input sets
/// INPUT_PROP_DIRECT only for the Pen application (hid-input.c,
/// hidinput_configure_usage); a single-touch Touch Screen gets no property
/// and Android then classifies it as an indirect *pointer* device. With
/// INPUT_PROP_DIRECT, Android's TouchInputMapper makes it a touch screen:
/// absolute positions, no pointer acceleration.
pub const TOUCH_DESC: &[u8] = &[
    0x05, 0x0D, 0x09, 0x02, 0xA1, 0x01, // Digitizer / Pen / Application
    0x09, 0x42, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81,
    0x02, // Tip Switch → BTN_TOUCH
    0x09, 0x32, 0x81, 0x02, // In Range → BTN_TOOL_PEN
    0x75, 0x06, 0x95, 0x01, 0x81, 0x03, // padding
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81,
    0x02, // X, Y absolute
    0xC0,
];

/// Map a point on the *rotated* (as displayed) screen to the raw digitizer
/// coordinates Android expects. Android treats raw touch-screen coordinates
/// as the display's natural (ROTATION_0) orientation and rotates them
/// (TouchInputMapper::computeInputTransforms, orientationAware=true):
/// rotateInRaw = ui::Transform(toRotationFlags(-rotation), …), which for
/// ROTATION_90 maps raw (x, y) → (y, 1−x). This is its inverse.
pub fn raw_from_rotated(fx: f32, fy: f32, rotation: u8) -> (f32, f32) {
    match rotation % 4 {
        0 => (fx, fy),
        1 => (1.0 - fy, fx),
        2 => (1.0 - fx, 1.0 - fy),
        _ => (fy, 1.0 - fx),
    }
}
pub const TOUCH_MAX: u16 = 0x7FFF;

/// `x`, `y` as fractions of the screen (clamped to 0..=1).
pub fn touch_report(down: bool, x: f32, y: f32) -> [u8; 5] {
    let scale = |v: f32| (v.clamp(0.0, 1.0) * f32::from(TOUCH_MAX)).round() as u16;
    let (x, y) = (scale(x).to_le_bytes(), scale(y).to_le_bytes());
    [if down { 0b11 } else { 0b10 }, x[0], x[1], y[0], y[1]]
}

/// Consumer "Menu Pick" → Linux KEY_SELECT → Android KEYCODE_DPAD_CENTER.
pub const USAGE_MENU_PICK: u16 = 0x41;
pub const USAGE_AC_HOME: u16 = 0x223;
pub const USAGE_AC_BACK: u16 = 0x224;

pub const KEY_ENTER: u8 = 0x28;
pub const KEY_KP_ENTER: u8 = 0x58;
pub const KEY_ESCAPE: u8 = 0x29;
pub const KEY_BACKSPACE: u8 = 0x2A;
pub const KEY_TAB: u8 = 0x2B;
pub const KEY_SPACE: u8 = 0x2C;
pub const KEY_RIGHT: u8 = 0x4F;
pub const KEY_LEFT: u8 = 0x50;
pub const KEY_DOWN: u8 = 0x51;
pub const KEY_UP: u8 = 0x52;
pub const MOD_SHIFT: u8 = 0x02;

/// US-layout ASCII → (modifiers, key usage).
pub fn ascii_to_hid(c: char) -> Option<(u8, u8)> {
    let plain = |u| Some((0, u));
    let shift = |u| Some((MOD_SHIFT, u));
    match c {
        'a'..='z' => plain(0x04 + (c as u8 - b'a')),
        'A'..='Z' => shift(0x04 + (c as u8 - b'A')),
        '1'..='9' => plain(0x1E + (c as u8 - b'1')),
        '0' => plain(0x27),
        '\n' => plain(KEY_ENTER),
        '\t' => plain(KEY_TAB),
        ' ' => plain(KEY_SPACE),
        '-' => plain(0x2D),
        '=' => plain(0x2E),
        '[' => plain(0x2F),
        ']' => plain(0x30),
        '\\' => plain(0x31),
        ';' => plain(0x33),
        '\'' => plain(0x34),
        '`' => plain(0x35),
        ',' => plain(0x36),
        '.' => plain(0x37),
        '/' => plain(0x38),
        '!' => shift(0x1E),
        '@' => shift(0x1F),
        '#' => shift(0x20),
        '$' => shift(0x21),
        '%' => shift(0x22),
        '^' => shift(0x23),
        '&' => shift(0x24),
        '*' => shift(0x25),
        '(' => shift(0x26),
        ')' => shift(0x27),
        '_' => shift(0x2D),
        '+' => shift(0x2E),
        '{' => shift(0x2F),
        '}' => shift(0x30),
        '|' => shift(0x31),
        ':' => shift(0x33),
        '"' => shift(0x34),
        '~' => shift(0x35),
        '<' => shift(0x36),
        '>' => shift(0x37),
        '?' => shift(0x38),
        _ => None,
    }
}

pub fn keyboard_report(modifiers: u8, key: u8) -> [u8; 8] {
    [modifiers, 0, key, 0, 0, 0, 0, 0]
}

pub fn mouse_report(buttons: u8, dx: i8, dy: i8, wheel: i8) -> [u8; 4] {
    [buttons, dx as u8, dy as u8, wheel as u8]
}

/// Split a large relative move into HID-sized (±127) steps.
pub fn mouse_steps(dx: i32, dy: i32) -> Vec<(i8, i8)> {
    let n = (dx.abs().max(dy.abs()) + 126) / 127;
    if n == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(n as usize);
    let (mut done_x, mut done_y) = (0, 0);
    for i in 1..=n {
        let (tx, ty) = (dx * i / n, dy * i / n);
        out.push(((tx - done_x) as i8, (ty - done_y) as i8));
        (done_x, done_y) = (tx, ty);
    }
    out
}

/// An Android device that accepted our HID registrations.
pub struct Aoa {
    dev: UsbDevice,
    pub description: String,
    /// USB serial number — equals `ro.serialno`, so it keys the saved
    /// lock-screen password even without adb.
    pub serial: String,
    registered: Vec<u16>,
}

impl Aoa {
    /// Parent device instance id, e.g. `USB\VID_2D95&PID_6012\<serial>`.
    pub fn parent(&self) -> &str {
        &self.dev.parent
    }

    /// The tablet (via its WinUSB MI_01), if it speaks AOA ≥ 2. Refuses when
    /// more than one prepared tablet is attached, or when `serial` differs.
    pub fn open(serial: Option<&str>) -> Result<Aoa, String> {
        let dev = UsbDevice::open(serial)?;
        let mut buf = [0u8; 2];
        let protocol = match dev.control(VENDOR_IN, REQ_GET_PROTOCOL, 0, 0, &mut buf) {
            Ok(2) => u16::from_le_bytes(buf),
            Ok(_) => 0,
            Err(e) => return Err(format!("查询 AOA 协议失败: {e}")),
        };
        if protocol < 2 {
            return Err(format!("设备不支持 AOA v2（协议版本 {protocol}）"));
        }
        let description = format!("{} (AOA v{protocol})", dev.parent);
        Ok(Aoa {
            serial: dev.serial.clone(),
            dev,
            description,
            registered: Vec::new(),
        })
    }

    fn out(&self, request: u8, value: u16, index: u16, data: &[u8]) -> Result<(), String> {
        let mut buf = data.to_vec();
        self.dev
            .control(VENDOR_OUT, request, value, index, &mut buf)
            .map(|_| ())
            .map_err(|e| format!("AOA 请求 {request} 失败: {e}"))
    }

    pub fn register(&mut self, id: u16, descriptor: &[u8]) -> Result<(), String> {
        self.out(REQ_REGISTER_HID, id, descriptor.len() as u16, &[])?;
        self.out(REQ_SET_HID_REPORT_DESC, id, 0, descriptor)?;
        self.registered.push(id);
        Ok(())
    }

    pub fn send(&self, id: u16, report: &[u8]) -> Result<(), String> {
        self.out(REQ_SEND_HID_EVENT, id, 0, report)
    }

    /// Register keyboard + mouse + consumer keys.
    pub fn register_all(&mut self) -> Result<(), String> {
        self.register(HID_KEYBOARD, KEYBOARD_DESC)?;
        self.register(HID_MOUSE, MOUSE_DESC)?;
        self.register(HID_CONSUMER, CONSUMER_DESC)?;
        self.register(HID_TOUCH, TOUCH_DESC)?;
        // Android needs a moment to create the input devices.
        std::thread::sleep(Duration::from_millis(400));
        Ok(())
    }

    pub fn tap_key(&self, modifiers: u8, key: u8) -> Result<(), String> {
        self.send(HID_KEYBOARD, &keyboard_report(modifiers, key))?;
        std::thread::sleep(Duration::from_millis(30));
        self.send(HID_KEYBOARD, &keyboard_report(0, 0))?;
        std::thread::sleep(Duration::from_millis(30));
        Ok(())
    }

    pub fn type_text(&self, text: &str) -> Result<usize, String> {
        let mut skipped = 0;
        for c in text.chars() {
            match ascii_to_hid(c) {
                Some((m, k)) => self.tap_key(m, k)?,
                None => skipped += 1,
            }
        }
        Ok(skipped)
    }

    pub fn consumer(&self, usage: u16) -> Result<(), String> {
        self.send(HID_CONSUMER, &usage.to_le_bytes())?;
        std::thread::sleep(Duration::from_millis(30));
        self.send(HID_CONSUMER, &[0, 0])
    }

    /// Pen tap at a screen fraction as displayed (absolute — no pointer, no
    /// focus, no acceleration). `rotation` = Android display rotation 0..3.
    pub fn tap(&self, fx: f32, fy: f32, rotation: u8) -> Result<(), String> {
        let (x, y) = raw_from_rotated(fx, fy, rotation);
        self.send(HID_TOUCH, &touch_report(false, x, y))?; // hover in range
        std::thread::sleep(Duration::from_millis(20));
        self.send(HID_TOUCH, &touch_report(true, x, y))?;
        std::thread::sleep(Duration::from_millis(80));
        self.send(HID_TOUCH, &touch_report(false, x, y))?;
        std::thread::sleep(Duration::from_millis(20));
        self.send(HID_TOUCH, &[0, 0, 0, 0, 0]) // leave range
    }

    /// Press the left button, move by (dx, dy), release: a swipe/drag.
    pub fn mouse_drag(&self, dx: i32, dy: i32) -> Result<(), String> {
        self.send(HID_MOUSE, &mouse_report(1, 0, 0, 0))?;
        std::thread::sleep(Duration::from_millis(40));
        for (x, y) in mouse_steps(dx, dy) {
            self.send(HID_MOUSE, &mouse_report(1, x, y, 0))?;
            std::thread::sleep(Duration::from_millis(12));
        }
        std::thread::sleep(Duration::from_millis(40));
        self.send(HID_MOUSE, &mouse_report(0, 0, 0, 0))
    }
}

impl Drop for Aoa {
    fn drop(&mut self) {
        for id in std::mem::take(&mut self.registered) {
            let _ = self.out(REQ_UNREGISTER_HID, id, 0, &[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_letters_digits_and_shifted_symbols() {
        assert_eq!(ascii_to_hid('a'), Some((0, 0x04)));
        assert_eq!(ascii_to_hid('Z'), Some((MOD_SHIFT, 0x1D)));
        assert_eq!(ascii_to_hid('1'), Some((0, 0x1E)));
        assert_eq!(ascii_to_hid('0'), Some((0, 0x27)));
        assert_eq!(ascii_to_hid('@'), Some((MOD_SHIFT, 0x1F)));
        assert_eq!(ascii_to_hid('\n'), Some((0, KEY_ENTER)));
        assert_eq!(ascii_to_hid('中'), None);
    }

    #[test]
    fn keyboard_and_mouse_reports() {
        assert_eq!(keyboard_report(MOD_SHIFT, 0x04), [2, 0, 4, 0, 0, 0, 0, 0]);
        assert_eq!(mouse_report(1, -1, 127, 0), [1, 0xFF, 0x7F, 0]);
    }

    #[test]
    fn large_moves_are_split_into_hid_sized_steps() {
        let steps = mouse_steps(-1000, 300);
        assert!(steps.iter().all(
            |(x, y)| (-127..=127).contains(&(*x as i32)) && (-127..=127).contains(&(*y as i32))
        ));
        let (sx, sy) = steps.iter().fold((0i32, 0i32), |(a, b), (x, y)| {
            (a + *x as i32, b + *y as i32)
        });
        assert_eq!((sx, sy), (-1000, 300));
        assert!(mouse_steps(0, 0).is_empty());
    }

    /// Forward transform from AOSP for ROTATION_90: raw (x, y) → (y, 1−x).
    fn aosp_forward(raw: (f32, f32), rotation: u8) -> (f32, f32) {
        let (x, y) = raw;
        match rotation % 4 {
            0 => (x, y),
            1 => (y, 1.0 - x),
            2 => (1.0 - x, 1.0 - y),
            _ => (1.0 - y, x),
        }
    }

    #[test]
    fn raw_mapping_inverts_androids_rotation_for_every_orientation() {
        for rotation in 0..4 {
            for &(fx, fy) in &[(0.41, 0.61), (0.0, 0.0), (1.0, 0.25), (0.5, 0.5)] {
                let back = aosp_forward(raw_from_rotated(fx, fy, rotation), rotation);
                assert!(
                    (back.0 - fx).abs() < 1e-6 && (back.1 - fy).abs() < 1e-6,
                    "rot {rotation} ({fx},{fy})"
                );
            }
        }
        // Landscape (ROTATION_90): 连接办公套件 at (0.41, 0.61) → raw (0.39, 0.41).
        let (x, y) = raw_from_rotated(0.41, 0.61, 1);
        assert!((x - 0.39).abs() < 1e-6 && (y - 0.41).abs() < 1e-6);
    }

    #[test]
    fn pen_descriptor_uses_the_pen_application() {
        // Digitizer page, usage Pen (0x02) — the only digitizer application
        // hid-input marks INPUT_PROP_DIRECT.
        assert_eq!(&TOUCH_DESC[..6], &[0x05, 0x0D, 0x09, 0x02, 0xA1, 0x01]);
    }

    #[test]
    fn touch_reports_scale_fractions_to_the_logical_range() {
        assert_eq!(touch_report(true, 0.0, 0.0), [0b11, 0, 0, 0, 0]);
        assert_eq!(
            touch_report(false, 1.0, 1.0),
            [0b10, 0xFF, 0x7F, 0xFF, 0x7F]
        );
        let r = touch_report(true, 0.5, 0.25);
        assert_eq!(u16::from_le_bytes([r[1], r[2]]), 16384);
        assert_eq!(u16::from_le_bytes([r[3], r[4]]), 8192);
        assert_eq!(
            touch_report(true, 2.0, -1.0)[1..],
            [0xFF, 0x7F, 0, 0],
            "clamped"
        );
    }

    #[test]
    fn report_descriptors_are_balanced() {
        // Every Collection (0xA1) needs an End Collection (0xC0).
        for desc in [KEYBOARD_DESC, MOUSE_DESC, CONSUMER_DESC, TOUCH_DESC] {
            let opens = desc.windows(2).filter(|w| w[0] == 0xA1).count();
            let closes = desc.iter().filter(|b| **b == 0xC0).count();
            assert_eq!(opens, closes);
        }
    }

    /// Hardware probe (run manually with the tablet attached):
    /// `cargo test aoa_probe -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn aoa_probe() {
        let mut aoa = Aoa::open(None).expect("open");
        println!("found: {}", aoa.description);
        aoa.register_all().expect("register");
        println!("registered keyboard/mouse/consumer");
        aoa.tap_key(0, KEY_SPACE).expect("key");
        println!("sent SPACE (wakes the screen)");
    }
}
