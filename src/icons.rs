//! Coloured feature icons and the brand mark, drawn as vectors (crisp at
//! any DPI, no image assets).
//!
//! Navigation stays monochrome (`nav::icon`, themed); colour is used where
//! it carries meaning: each feature has its own hue, shown as a tinted tile
//! with a line glyph — card titles, primary actions — and the brand mark
//! repeats the app icon (assets/icon/droidrescue.svg).

use crate::nav::{self, Page};
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2};
use std::f32::consts::{PI, TAU};

/// A feature, as an icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Usb,
    Mirror,
    Wifi,
    Export,
    Unlock,
    Keys,
    Rescue,
    Manual,
    Profile,
    Checkup,
    Bolt,
    Sliders,
    Palette,
    Folder,
    Info,
}

impl Glyph {
    /// Each feature's hue (readable on both the dark and light surfaces).
    pub fn hue(self) -> Color32 {
        match self {
            Glyph::Usb | Glyph::Mirror | Glyph::Sliders => BLUE,
            Glyph::Wifi | Glyph::Folder => TEAL,
            Glyph::Export | Glyph::Profile | Glyph::Palette => PURPLE,
            Glyph::Unlock | Glyph::Bolt => AMBER,
            Glyph::Rescue => ORANGE,
            Glyph::Checkup => GREEN,
            Glyph::Keys | Glyph::Manual => INDIGO,
            Glyph::Info => SLATE,
        }
    }
}

const BLUE: Color32 = Color32::from_rgb(0x4C, 0x8D, 0xF6);
const TEAL: Color32 = Color32::from_rgb(0x1F, 0xB0, 0xB8);
const PURPLE: Color32 = Color32::from_rgb(0x9B, 0x6C, 0xF2);
const AMBER: Color32 = Color32::from_rgb(0xE8, 0xA3, 0x3A);
const ORANGE: Color32 = Color32::from_rgb(0xF2, 0x68, 0x3C);
const GREEN: Color32 = Color32::from_rgb(0x2F, 0xB8, 0x6E);
const INDIGO: Color32 = Color32::from_rgb(0x6C, 0x7B, 0xF2);
const SLATE: Color32 = Color32::from_rgb(0x8A, 0x94, 0xA3);

/// Tinted rounded tile with the glyph in its hue, filling `rect`.
pub fn tile(p: &Painter, rect: Rect, glyph: Glyph) {
    let hue = glyph.hue();
    p.rect_filled(rect, rect.width() * 0.28, hue.gamma_multiply(0.16));
    draw(p, glyph, rect.center(), rect.width() * 0.6, hue);
}

/// The glyph alone, `size` px square, centred on `at` (16 px design grid).
pub fn draw(p: &Painter, glyph: Glyph, at: Pos2, size: f32, color: Color32) {
    let s = size / 16.0;
    let pt = |x: f32, y: f32| at + vec2((x - 8.0) * s, (y - 8.0) * s);
    let stroke = Stroke::new(1.6 * s, color);
    let line = |pts: &[(f32, f32)]| {
        p.add(Shape::line(
            pts.iter().map(|&(x, y)| pt(x, y)).collect(),
            stroke,
        ));
    };
    let arc = |cx: f32, cy: f32, r: f32, a0: f32, a1: f32| {
        let n = 16;
        let pts = (0..=n)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / n as f32;
                pt(cx + r * a.cos(), cy + r * a.sin())
            })
            .collect();
        p.add(Shape::line(pts, stroke));
    };
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        p.rect_stroke(
            Rect::from_min_max(pt(x0, y0), pt(x1, y1)),
            r * s,
            stroke,
            StrokeKind::Middle,
        );
    };
    match glyph {
        Glyph::Usb => {
            // USB trident: stem, two branches, round foot.
            line(&[(8.0, 1.5), (8.0, 12.0)]);
            line(&[(6.2, 3.6), (8.0, 1.5), (9.8, 3.6)]);
            line(&[(8.0, 9.0), (4.0, 6.5), (4.0, 5.0)]);
            line(&[(8.0, 7.5), (12.0, 5.5), (12.0, 4.0)]);
            p.circle_filled(pt(4.0, 4.2), 1.2 * s, color);
            p.rect_filled(
                Rect::from_center_size(pt(12.0, 3.4), vec2(2.2 * s, 2.2 * s)),
                0.0,
                color,
            );
            p.circle_filled(pt(8.0, 13.0), 1.8 * s, color);
        }
        Glyph::Mirror => nav::icon(p, Page::Mirror, at, size, color),
        Glyph::Rescue => nav::icon(p, Page::Rescue, at, size, color),
        Glyph::Wifi => {
            for r in [3.0, 6.0, 9.0] {
                arc(8.0, 13.0, r, -PI * 0.78, -PI * 0.22);
            }
            p.circle_filled(pt(8.0, 13.0), 1.3 * s, color);
        }
        Glyph::Export => {
            line(&[(8.0, 2.0), (8.0, 10.0)]);
            line(&[(4.8, 7.0), (8.0, 10.2), (11.2, 7.0)]);
            line(&[(2.0, 10.0), (2.0, 14.0), (14.0, 14.0), (14.0, 10.0)]);
        }
        Glyph::Unlock => {
            rect(3.0, 7.5, 13.0, 14.5, 1.5);
            // Shackle open on the right.
            line(&[(5.2, 7.5), (5.2, 5.0)]);
            arc(8.0, 5.0, 2.8, PI, TAU);
            line(&[(10.8, 5.0), (10.8, 5.6)]);
            p.circle_filled(pt(8.0, 11.0), 1.2 * s, color);
        }
        Glyph::Keys => {
            rect(1.5, 4.0, 14.5, 12.5, 1.5);
            for x in [4.5, 8.0, 11.5] {
                p.circle_filled(pt(x, 7.0), 0.9 * s, color);
            }
            line(&[(5.0, 10.0), (11.0, 10.0)]);
        }
        Glyph::Manual => {
            // Four-way arrows (direction keys).
            line(&[(8.0, 2.0), (8.0, 14.0)]);
            line(&[(2.0, 8.0), (14.0, 8.0)]);
            line(&[(6.0, 4.0), (8.0, 2.0), (10.0, 4.0)]);
            line(&[(6.0, 12.0), (8.0, 14.0), (10.0, 12.0)]);
            line(&[(4.0, 6.0), (2.0, 8.0), (4.0, 10.0)]);
            line(&[(12.0, 6.0), (14.0, 8.0), (12.0, 10.0)]);
        }
        Glyph::Profile => {
            // Phone with script lines.
            rect(3.5, 1.5, 12.5, 14.5, 2.0);
            line(&[(6.0, 5.0), (10.0, 5.0)]);
            line(&[(6.0, 8.0), (10.0, 8.0)]);
            line(&[(6.0, 11.0), (8.5, 11.0)]);
        }
        Glyph::Checkup => {
            line(&[
                (1.5, 8.5),
                (4.5, 8.5),
                (6.2, 4.0),
                (9.2, 13.0),
                (10.8, 8.5),
                (14.5, 8.5),
            ]);
        }
        Glyph::Bolt => {
            line(&[
                (9.5, 1.5),
                (3.5, 9.0),
                (7.5, 9.0),
                (6.5, 14.5),
                (12.5, 7.0),
                (8.5, 7.0),
                (9.5, 1.5),
            ]);
        }
        Glyph::Sliders => {
            for (y, knob) in [(4.0, 10.5), (8.0, 5.0), (12.0, 9.0)] {
                line(&[(2.0, y), (14.0, y)]);
                p.circle_filled(pt(knob, y), 1.8 * s, color);
            }
        }
        Glyph::Palette => {
            // Half-filled disc: light / dark.
            p.circle_stroke(pt(8.0, 8.0), 6.0 * s, stroke);
            let half: Vec<Pos2> = (0..=16)
                .map(|i| {
                    let a = -PI / 2.0 + PI * i as f32 / 16.0;
                    pt(8.0 + 6.0 * a.cos(), 8.0 + 6.0 * a.sin())
                })
                .collect();
            p.add(Shape::convex_polygon(half, color, Stroke::NONE));
        }
        Glyph::Folder => {
            line(&[
                (1.5, 4.0),
                (6.0, 4.0),
                (7.5, 5.5),
                (14.5, 5.5),
                (14.5, 13.0),
                (1.5, 13.0),
                (1.5, 4.0),
            ]);
        }
        Glyph::Info => {
            p.circle_stroke(pt(8.0, 8.0), 6.2 * s, stroke);
            line(&[(8.0, 7.0), (8.0, 11.5)]);
            p.circle_filled(pt(8.0, 4.8), 1.0 * s, color);
        }
    }
}

/// Brand colours (assets/icon/droidrescue.svg).
const BRAND_TILE: Color32 = Color32::from_rgb(0x1F, 0x9D, 0x5A);
const BRAND_BAND: Color32 = Color32::from_rgb(0xF2, 0x68, 0x3C);

/// The app icon (green tile, lifebuoy, Android head), filling `rect` —
/// the same geometry as assets/icon/droidrescue.svg on its 1024 grid.
pub fn brand_mark(p: &Painter, rect: Rect) {
    let size = rect.width().min(rect.height());
    // The SVG's tile spans 100…924; map that span onto `rect`.
    let k = size / 824.0;
    let c = rect.center();
    let at = |x: f32, y: f32| pos2(c.x + (x - 512.0) * k, c.y + (y - 512.0) * k);
    let tile = BRAND_TILE;
    p.rect_filled(Rect::from_center_size(c, vec2(size, size)), 185.0 * k, tile);
    // Ring r 188…330: white disc, orange-red bands on the axes (annular
    // sectors filled as thin convex slices — thick strokes don't follow the
    // arc), then the hole on top.
    let (inner, outer) = (188.0 * k, 330.0 * k);
    p.circle_filled(c, outer, Color32::WHITE);
    let half_band = 11.5_f32.to_radians();
    let dir = |t: f32| vec2(t.cos(), t.sin());
    for axis in 0..4 {
        let a = axis as f32 * PI / 2.0;
        let slices = 6;
        for i in 0..slices {
            let t0 = a - half_band + 2.0 * half_band * i as f32 / slices as f32;
            let t1 = a - half_band + 2.0 * half_band * (i + 1) as f32 / slices as f32;
            let quad = vec![
                c + dir(t0) * inner,
                c + dir(t0) * outer,
                c + dir(t1) * outer,
                c + dir(t1) * inner,
            ];
            p.add(Shape::convex_polygon(quad, BRAND_BAND, Stroke::NONE));
        }
    }
    p.circle_filled(c, inner, tile);
    // Head: half disc r 122 on y 588, antennae, eyes.
    let head = Color32::WHITE;
    let base = at(512.0, 588.0);
    let dome: Vec<Pos2> = (0..=24)
        .map(|i| {
            let t = PI + PI * i as f32 / 24.0;
            base + vec2(t.cos(), t.sin()) * 122.0 * k
        })
        .collect();
    p.add(Shape::convex_polygon(dome, head, Stroke::NONE));
    let antenna = Stroke::new(16.0 * k, head);
    p.line_segment([at(456.4, 499.0), at(419.8, 440.4)], antenna);
    p.line_segment([at(567.6, 499.0), at(604.2, 440.4)], antenna);
    p.circle_filled(at(462.0, 546.0), 15.0 * k, tile);
    p.circle_filled(at(562.0, 546.0), 15.0 * k, tile);
}

/// `brand_mark` in an allocated square of `size`.
pub fn brand_widget(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), egui::Sense::hover());
    brand_mark(ui.painter(), rect);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_has_a_visible_hue() {
        use Glyph::*;
        for g in [
            Usb, Mirror, Wifi, Export, Unlock, Keys, Rescue, Manual, Profile, Checkup, Bolt,
            Sliders, Palette, Folder, Info,
        ] {
            assert_ne!(g.hue().a(), 0, "{g:?}");
        }
    }
}
