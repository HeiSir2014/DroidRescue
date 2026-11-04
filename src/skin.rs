//! Device skins: body geometry in millimetres, drawn in a canonical
//! orientation, rotated by quarter turns and fitted into the stage.
//! * Known models (vivo Pad3) use measured dimensions and key positions.
//! * Anything else gets a generic body whose screen has exactly the video's
//!   aspect — mirroring must work for every Android device, not just one.
//!
//! The video is always aspect-fitted into the screen ([`fit`]), so even a
//! skin/stream mismatch letterboxes instead of stretching.

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

/// How far the physical keys stand proud of the frame.
const KEY_DEPTH: f32 = 1.8;
/// Empty space around the device inside the stage.
const MARGIN_MM: f32 = 6.0;

// vivo Pad3 (PA2455): body 266.43 × 192 mm; 12.1" 2800×1968 @ 283 ppi.
const PAD3_MODELS: &[&str] = &["PA2455"];
const PAD3_BODY: Vec2 = vec2(266.43, 192.0);
const PAD3_SCREEN: Vec2 = vec2(2800.0 / 283.0 * 25.4, 1968.0 / 283.0 * 25.4);
/// Power: left short edge, measured from the top-left corner.
const POWER_MM: (f32, f32) = (16.0, 32.0);
/// Volume rocker: top long edge, measured from the top-left corner (vol+ first).
const VOL_UP_MM: (f32, f32) = (22.0, 37.0);
const VOL_DOWN_MM: (f32, f32) = (38.0, 53.0);

/// Generic body: screen's long side in mm (only proportions matter) and the
/// bezel as a fraction of the screen's short side.
const GENERIC_LONG_MM: f32 = 160.0;
const GENERIC_BEZEL: f32 = 0.045;

/// A device body in its canonical orientation (origin at the body centre).
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    body: Vec2,
    screen: Vec2,
    body_radius: f32,
    screen_radius: f32,
    camera: Pos2,
    camera_radius: f32,
    keys: Vec<(Part, Rect)>,
    /// Android display rotation the canonical drawing shows.
    rotation: u8,
}

impl Model {
    /// vivo Pad3, canonical = landscape (ROTATION_90), camera on the top long edge.
    pub fn pad3() -> Model {
        let (l, t) = (-PAD3_BODY.x / 2.0, -PAD3_BODY.y / 2.0);
        Model {
            body: PAD3_BODY,
            screen: PAD3_SCREEN,
            body_radius: 9.0,
            screen_radius: 4.0,
            camera: pos2(0.0, t + (PAD3_BODY.y - PAD3_SCREEN.y) / 4.0),
            camera_radius: 1.3,
            keys: vec![
                (
                    Part::Power,
                    mm(l - KEY_DEPTH, t + POWER_MM.0, l, t + POWER_MM.1),
                ),
                (
                    Part::VolUp,
                    mm(l + VOL_UP_MM.0, t - KEY_DEPTH, l + VOL_UP_MM.1, t),
                ),
                (
                    Part::VolDown,
                    mm(l + VOL_DOWN_MM.0, t - KEY_DEPTH, l + VOL_DOWN_MM.1, t),
                ),
            ],
            rotation: 1,
        }
    }

    /// Any device: a plain body around a screen of the given natural
    /// (ROTATION_0) pixel size. No drawn keys — positions are unknown; the
    /// rail's key buttons work for every device.
    pub fn generic(natural: (u32, u32)) -> Model {
        let (w, h) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
        let k = GENERIC_LONG_MM / w.max(h);
        let screen = vec2(w * k, h * k);
        let bezel = screen.min_elem() * GENERIC_BEZEL;
        let body = screen + Vec2::splat(2.0 * bezel);
        Model {
            body,
            screen,
            body_radius: body.min_elem() * 0.08,
            screen_radius: screen.min_elem() * 0.05,
            camera: pos2(0.0, -body.y / 2.0 + bezel / 2.0),
            camera_radius: bezel * 0.18,
            keys: Vec::new(),
            rotation: 0,
        }
    }

    /// The skin for a device: its own model if known, else a generic body
    /// matching the stream. `rotation` (current Android display rotation)
    /// recovers the natural size from the rotated video.
    pub fn for_device(adb_model: Option<&str>, video: (u32, u32), rotation: Option<u8>) -> Model {
        if adb_model.is_some_and(|m| PAD3_MODELS.contains(&m)) {
            return Model::pad3();
        }
        let natural = match (video, rotation) {
            ((0, _) | (_, 0), _) => (1080, 2400), // no stream yet: a typical phone
            ((w, h), Some(r)) if r % 2 == 1 => (h, w),
            (wh, _) => wh,
        };
        Model::generic(natural)
    }

    fn landscape(&self) -> bool {
        self.screen.x >= self.screen.y
    }

    /// Device + keys + margin in mm after `turns` quarter turns.
    pub fn extent(&self, turns: u8) -> Vec2 {
        let key = if self.keys.is_empty() { 0.0 } else { KEY_DEPTH };
        let e = self.body + Vec2::splat(2.0 * (key + MARGIN_MM));
        if turns % 2 == 1 { vec2(e.y, e.x) } else { e }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Part {
    Power,
    VolUp,
    VolDown,
}

impl Part {
    pub const fn keycode(self) -> u32 {
        match self {
            Part::Power => 26,
            Part::VolUp => 24,
            Part::VolDown => 25,
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Part::Power => "电源",
            Part::VolUp => "音量 +",
            Part::VolDown => "音量 −",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Layout {
    /// Pixels per millimetre.
    pub scale: f32,
    pub body: Rect,
    pub body_radius: f32,
    pub screen: Rect,
    pub screen_radius: f32,
    pub camera: Pos2,
    pub camera_radius: f32,
    pub keys: Vec<(Part, Rect)>,
}

impl Layout {
    pub fn key_at(&self, p: Pos2) -> Option<Part> {
        // Keys are thin: give them a slightly larger hit area.
        let pad = 0.8 * self.scale;
        self.keys
            .iter()
            .find(|(_, r)| r.expand(pad).contains(p))
            .map(|(k, _)| *k)
    }
}

/// Canonical rect (mm, origin at body centre, y down).
fn mm(left: f32, top: f32, right: f32, bottom: f32) -> Rect {
    Rect::from_min_max(pos2(left, top), pos2(right, bottom))
}

/// Rotate a canonical point clockwise by `turns` quarter turns.
fn rotate(p: Pos2, turns: u8) -> Pos2 {
    (0..turns % 4).fold(p, |p, _| pos2(-p.y, p.x))
}

fn rotate_rect(r: Rect, turns: u8) -> Rect {
    Rect::from_two_pos(rotate(r.min, turns), rotate(r.max, turns))
}

fn centered(size: Vec2) -> Rect {
    Rect::from_center_size(Pos2::ZERO, size)
}

/// Fit the device (rotated by `turns`) into `stage`.
pub fn layout(stage: Rect, model: &Model, turns: u8) -> Layout {
    let extent = model.extent(turns);
    let scale = (stage.width() / extent.x)
        .min(stage.height() / extent.y)
        .max(0.01);
    let center = stage.center();
    let place = |r: Rect| {
        let r = rotate_rect(r, turns);
        Rect::from_min_max(
            center + r.min.to_vec2() * scale,
            center + r.max.to_vec2() * scale,
        )
    };
    Layout {
        scale,
        body: place(centered(model.body)),
        body_radius: model.body_radius * scale,
        screen: place(centered(model.screen)),
        screen_radius: model.screen_radius * scale,
        camera: center + rotate(model.camera, turns).to_vec2() * scale,
        camera_radius: model.camera_radius * scale,
        keys: model.keys.iter().map(|(k, r)| (*k, place(*r))).collect(),
    }
}

/// Quarter turns (clockwise) from the model's canonical drawing to the
/// current display rotation; the video aspect (once known) wins over a
/// stale rotation that disagrees with the stream.
pub fn turns_for(model: &Model, orientation: Option<u8>, video_landscape: Option<bool>) -> u8 {
    let landscape_at = |t: u8| model.landscape() == t.is_multiple_of(2);
    match (
        orientation.map(|r| (model.rotation + 4 - r % 4) % 4),
        video_landscape,
    ) {
        (Some(t), None) => t,
        (Some(t), Some(v)) if landscape_at(t) == v => t,
        (_, Some(v)) if landscape_at(0) != v => 1,
        _ => 0,
    }
}

/// The largest rect of the video's aspect centred in `screen` (letterbox).
pub fn fit(screen: Rect, video: (u32, u32)) -> Rect {
    if video.0 == 0 || video.1 == 0 {
        return screen;
    }
    let aspect = video.0 as f32 / video.1 as f32;
    let size = if screen.width() / screen.height() > aspect {
        vec2(screen.height() * aspect, screen.height())
    } else {
        vec2(screen.width(), screen.width() / aspect)
    };
    Rect::from_center_size(screen.center(), size)
}

/// Paint everything except the video: shadow, keys, frame, glass, screen
/// mask and camera. Call `paint_back` before the video and `paint_front` after.
pub mod paint {
    use super::{Layout, Part};
    use crate::theme;
    use eframe::egui::{Color32, CornerRadius, Painter, Stroke, StrokeKind};

    const FRAME: Color32 = Color32::from_rgb(0x2A, 0x2D, 0x32);
    const FRAME_EDGE: Color32 = Color32::from_rgb(0x4A, 0x4F, 0x57);
    const GLASS: Color32 = Color32::from_rgb(0x05, 0x05, 0x06);
    const KEY: Color32 = Color32::from_rgb(0x3A, 0x3E, 0x45);
    const NO_SIGNAL: Color32 = Color32::from_rgb(0x0B, 0x0C, 0x0E);

    fn radius(r: f32) -> CornerRadius {
        CornerRadius::same(r.round().clamp(0.0, 255.0) as u8)
    }

    pub fn back(
        p: &Painter,
        l: &Layout,
        hot: Option<Part>,
        pressed: Option<Part>,
        has_video: bool,
    ) {
        // Soft drop shadow.
        for i in 1..=6 {
            let grow = i as f32 * 0.9 * l.scale;
            let shadow = l
                .body
                .translate(eframe::egui::vec2(0.0, 1.2 * l.scale))
                .expand(grow);
            p.rect_filled(
                shadow,
                radius(l.body_radius + grow),
                Color32::from_black_alpha(14),
            );
        }
        // Keys sit under the frame so only their proud part shows.
        let accent = theme::c();
        for (part, rect) in &l.keys {
            let (fill, edge) = match (Some(*part) == pressed, Some(*part) == hot) {
                (true, _) => (accent.amber, accent.amber_hi),
                (_, true) => (KEY.gamma_multiply(1.4), accent.amber),
                _ => (KEY, FRAME_EDGE),
            };
            let r = radius(0.9 * l.scale);
            p.rect_filled(*rect, r, fill);
            p.rect_stroke(*rect, r, Stroke::new(1.0, edge), StrokeKind::Inside);
        }
        // Aluminium frame with a bright chamfer line, then the black glass.
        p.rect_filled(l.body, radius(l.body_radius), FRAME);
        p.rect_stroke(
            l.body,
            radius(l.body_radius),
            Stroke::new(1.0, FRAME_EDGE),
            StrokeKind::Inside,
        );
        let rim = 1.1 * l.scale;
        p.rect_filled(l.body.shrink(rim), radius(l.body_radius - rim), GLASS);
        if !has_video {
            p.rect_filled(l.screen, radius(l.screen_radius), NO_SIGNAL);
        }
    }

    pub fn front(p: &Painter, l: &Layout) {
        // Round the video's corners by stroking glass over them: a stroke of
        // width w around screen.expand(w/2) has inner corner radius screen_radius.
        let w = l.screen_radius.max(2.0) * 2.0;
        p.rect_stroke(
            l.screen.expand(w / 2.0),
            radius(l.screen_radius + w / 2.0),
            Stroke::new(w, GLASS),
            StrokeKind::Middle,
        );
        // Front camera: lens + faint ring + glint.
        p.circle_filled(
            l.camera,
            l.camera_radius * 1.6,
            Color32::from_rgb(0x0E, 0x10, 0x14),
        );
        p.circle_filled(
            l.camera,
            l.camera_radius,
            Color32::from_rgb(0x12, 0x18, 0x24),
        );
        p.circle_filled(
            l.camera + eframe::egui::vec2(-0.35, -0.35) * l.camera_radius,
            l.camera_radius * 0.3,
            Color32::from_white_alpha(40),
        );
    }
}

/// Map a point on the drawn screen to video pixel coordinates (clamped).
pub fn screen_to_video(p: Pos2, screen: Rect, video: (u32, u32)) -> (i32, i32) {
    let u = ((p.x - screen.left()) / screen.width()).clamp(0.0, 1.0);
    let v = ((p.y - screen.top()) / screen.height()).clamp(0.0, 1.0);
    let x = (u * video.0 as f32).min(video.0.saturating_sub(1) as f32);
    let y = (v * video.1 as f32).min(video.1.saturating_sub(1) as f32);
    (x as i32, y as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAGE: Rect = Rect {
        min: pos2(0.0, 0.0),
        max: pos2(1600.0, 1000.0),
    };

    fn aspect(r: Rect) -> f32 {
        r.width() / r.height()
    }

    fn pad3_layout(turns: u8) -> Layout {
        layout(STAGE, &Model::pad3(), turns)
    }

    #[test]
    fn landscape_screen_matches_panel_aspect() {
        let l = pad3_layout(0);
        assert!((aspect(l.screen) - 2800.0 / 1968.0).abs() < 0.005);
    }

    #[test]
    fn portrait_screen_is_inverse_aspect() {
        let l = pad3_layout(1);
        assert!((aspect(l.screen) - 1968.0 / 2800.0).abs() < 0.005);
    }

    #[test]
    fn pad3_is_chosen_by_model() {
        assert_eq!(
            Model::for_device(Some("PA2455"), (1080, 2400), Some(0)),
            Model::pad3()
        );
        assert_ne!(
            Model::for_device(Some("V2405A"), (1080, 2400), Some(0)),
            Model::pad3()
        );
    }

    /// The reported bug: a portrait phone was drawn into the Pad3 screen.
    #[test]
    fn generic_screen_has_the_video_aspect_in_every_rotation() {
        let phone = (1080u32, 2400u32); // natural portrait
        for r in 0..4u8 {
            let video = if r % 2 == 1 {
                (phone.1, phone.0)
            } else {
                phone
            };
            let model = Model::for_device(Some("V2405A"), video, Some(r));
            let turns = turns_for(&model, Some(r), Some(video.0 >= video.1));
            let l = layout(STAGE, &model, turns);
            let want = video.0 as f32 / video.1 as f32;
            assert!(
                (aspect(l.screen) - want).abs() < 0.005,
                "rotation {r}: {} vs {want}",
                aspect(l.screen)
            );
            assert!(STAGE.contains_rect(l.body));
            assert!(l.keys.is_empty());
        }
    }

    #[test]
    fn generic_survives_a_stale_rotation() {
        // Rotation says portrait, stream is already landscape.
        let video = (2400, 1080);
        let model = Model::for_device(None, video, Some(0));
        let turns = turns_for(&model, Some(0), Some(true));
        let l = layout(STAGE, &model, turns);
        assert!((aspect(l.screen) - 2400.0 / 1080.0).abs() < 0.005);
    }

    #[test]
    fn generic_camera_is_on_the_natural_top_bezel() {
        let l = layout(STAGE, &Model::generic((1080, 2400)), 0);
        assert!(l.camera.y > l.body.top() && l.camera.y < l.screen.top());
    }

    #[test]
    fn video_is_letterboxed_not_stretched() {
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 700.0));
        let tall = fit(screen, (1080, 2400));
        assert!((aspect(tall) - 1080.0 / 2400.0).abs() < 0.001);
        assert_eq!(tall.height(), 700.0);
        assert_eq!(tall.center(), screen.center());
        let wide = fit(screen, (2400, 1080));
        assert_eq!(wide.width(), 1000.0);
        assert_eq!(fit(screen, (0, 0)), screen);
    }

    #[test]
    fn screen_sits_inside_body_and_device_fits_stage() {
        for turns in 0..4 {
            let l = pad3_layout(turns);
            assert!(l.body.contains_rect(l.screen), "turns {turns}");
            for (_, k) in &l.keys {
                assert!(STAGE.contains_rect(*k), "turns {turns}");
            }
            assert!(STAGE.contains_rect(l.body));
        }
    }

    #[test]
    fn keys_protrude_from_the_expected_edges() {
        let l = pad3_layout(0);
        let key = |p| l.keys.iter().find(|(k, _)| *k == p).unwrap().1;
        assert!(
            key(Part::Power).right() <= l.body.left() + 0.01,
            "power on left edge"
        );
        assert!(
            key(Part::VolUp).bottom() <= l.body.top() + 0.01,
            "volume on top edge"
        );
        assert!(key(Part::VolUp).center().x < key(Part::VolDown).center().x);

        // One clockwise turn: left edge → top, top edge → right.
        let l = pad3_layout(1);
        let key = |p| l.keys.iter().find(|(k, _)| *k == p).unwrap().1;
        assert!(key(Part::Power).bottom() <= l.body.top() + 0.01);
        assert!(key(Part::VolUp).left() >= l.body.right() - 0.01);
    }

    #[test]
    fn camera_is_on_top_bezel_in_canonical_landscape() {
        let l = pad3_layout(0);
        assert!(l.camera.y > l.body.top() && l.camera.y < l.screen.top());
        assert!((l.camera.x - l.body.center().x).abs() < 0.01);
    }

    #[test]
    fn key_hit_testing() {
        let l = pad3_layout(0);
        let power = l.keys[0].1;
        assert_eq!(l.key_at(power.center()), Some(Part::Power));
        assert_eq!(l.key_at(l.screen.center()), None);
    }

    #[test]
    fn rotation_to_turns() {
        let pad = Model::pad3();
        assert_eq!(turns_for(&pad, Some(1), Some(true)), 0);
        assert_eq!(turns_for(&pad, Some(3), Some(true)), 2);
        assert_eq!(turns_for(&pad, Some(0), Some(false)), 1);
        assert_eq!(turns_for(&pad, Some(2), Some(false)), 3);
        // Stale rotation that contradicts the video: trust the video.
        assert_eq!(turns_for(&pad, Some(1), Some(false)), 1);
        assert_eq!(turns_for(&pad, None, Some(true)), 0);
        // No stream yet: rotation alone.
        assert_eq!(turns_for(&pad, Some(0), None), 1);
        assert_eq!(turns_for(&pad, None, None), 0);
    }

    #[test]
    fn maps_screen_points_to_video_pixels() {
        let screen = Rect::from_min_size(pos2(100.0, 50.0), vec2(1000.0, 700.0));
        assert_eq!(
            screen_to_video(pos2(100.0, 50.0), screen, (2000, 1400)),
            (0, 0)
        );
        assert_eq!(
            screen_to_video(pos2(600.0, 400.0), screen, (2000, 1400)),
            (1000, 700)
        );
        assert_eq!(
            screen_to_video(pos2(5000.0, 5000.0), screen, (2000, 1400)),
            (1999, 1399)
        );
    }
}
