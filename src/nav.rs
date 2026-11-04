//! Left navigation: one entry per page, icon + label; collapses to icons
//! while mirroring so the picture gets the room. Icons are drawn as
//! vectors (crisp at any DPI, themed) rather than taken from a font.

use crate::theme;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, pos2, vec2};

/// Sidebar width with labels / icons only.
pub const NAV_W: f32 = 196.0;
pub const NAV_COLLAPSED_W: f32 = 64.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Home,
    Mirror,
    Rescue,
    Script,
    Guide,
    Tools,
    Settings,
}

impl Page {
    /// Main entries (top) and utility entries (bottom).
    pub const MAIN: [Page; 5] = [
        Page::Home,
        Page::Mirror,
        Page::Rescue,
        Page::Script,
        Page::Guide,
    ];
    pub const UTILITY: [Page; 2] = [Page::Tools, Page::Settings];

    pub fn label(self) -> &'static str {
        match self {
            Page::Home => "首页",
            Page::Mirror => "投屏",
            Page::Rescue => "救援",
            Page::Script => "脚本调试",
            Page::Guide => "脚本说明",
            Page::Tools => "工具",
            Page::Settings => "设置",
        }
    }

    /// Stable key for persisting the last page.
    pub fn key(self) -> &'static str {
        match self {
            Page::Home => "home",
            Page::Mirror => "mirror",
            Page::Rescue => "rescue",
            Page::Script => "script",
            Page::Guide => "guide",
            Page::Tools => "tools",
            Page::Settings => "settings",
        }
    }

    pub fn from_key(key: &str) -> Page {
        Page::MAIN
            .into_iter()
            .chain(Page::UTILITY)
            .find(|p| p.key() == key)
            .unwrap_or(Page::Home)
    }
}

/// Draw the sidebar in `rect`; returns the page clicked, if any.
pub fn sidebar(ui: &mut egui::Ui, rect: Rect, current: Page, collapsed: bool) -> Option<Page> {
    let c = theme::c();
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, c.surface);
    p.line_segment(
        [rect.right_top(), rect.right_bottom()],
        Stroke::new(1.0, c.line),
    );
    let mut clicked = None;
    let item_h = 40.0;
    let pad = 10.0;
    let mut y = rect.top() + 12.0;
    for page in Page::MAIN {
        let r = Rect::from_min_size(
            pos2(rect.left() + pad, y),
            vec2(rect.width() - pad * 2.0, item_h),
        );
        if item(ui, r, page, page == current, collapsed) {
            clicked = Some(page);
        }
        y += item_h + 4.0;
    }
    // Utility entries pinned to the bottom.
    let mut y = rect.bottom() - 12.0 - (item_h + 4.0) * Page::UTILITY.len() as f32;
    p.line_segment(
        [
            pos2(rect.left() + pad + 6.0, y - 8.0),
            pos2(rect.right() - pad - 6.0, y - 8.0),
        ],
        Stroke::new(1.0, c.line),
    );
    for page in Page::UTILITY {
        let r = Rect::from_min_size(
            pos2(rect.left() + pad, y),
            vec2(rect.width() - pad * 2.0, item_h),
        );
        if item(ui, r, page, page == current, collapsed) {
            clicked = Some(page);
        }
        y += item_h + 4.0;
    }
    clicked
}

fn item(ui: &mut egui::Ui, r: Rect, page: Page, selected: bool, collapsed: bool) -> bool {
    let c = theme::c();
    let id = ui.id().with(("nav", page.key()));
    let resp = ui.interact(r, id, Sense::click());
    let resp = if collapsed {
        resp.on_hover_text(page.label())
    } else {
        resp
    };
    let p = ui.painter();
    let (fill, fg) = match (selected, resp.hovered()) {
        (true, _) => (c.amber_lo, c.amber),
        (false, true) => (c.surface_hover, c.text),
        (false, false) => (Color32::TRANSPARENT, c.text_dim),
    };
    p.rect_filled(r, 8.0, fill);
    if selected {
        // Indicator bar on the left edge.
        let bar = Rect::from_min_size(pos2(r.left(), r.center().y - 9.0), vec2(3.0, 18.0));
        p.rect_filled(bar, 2.0, c.amber);
    }
    let icon_center = if collapsed {
        r.center()
    } else {
        pos2(r.left() + 22.0, r.center().y)
    };
    icon(p, page, icon_center, 16.0, fg);
    if !collapsed {
        p.text(
            pos2(r.left() + 42.0, r.center().y),
            egui::Align2::LEFT_CENTER,
            page.label(),
            egui::FontId::proportional(13.5),
            if selected { c.amber } else { c.text },
        );
    }
    resp.clicked()
}

/// Line icon for a page, `size` px square, centred on `at`.
pub fn icon(p: &Painter, page: Page, at: Pos2, size: f32, color: Color32) {
    let s = size / 16.0; // icons are designed on a 16 px grid
    let pt = |x: f32, y: f32| at + vec2((x - 8.0) * s, (y - 8.0) * s);
    let stroke = Stroke::new(1.6 * s, color);
    let rect = |x0: f32, y0: f32, x1: f32, y1: f32| Rect::from_min_max(pt(x0, y0), pt(x1, y1));
    match page {
        Page::Home => {
            p.line_segment([pt(1.5, 8.0), pt(8.0, 2.0)], stroke);
            p.line_segment([pt(8.0, 2.0), pt(14.5, 8.0)], stroke);
            p.rect_stroke(
                rect(3.5, 7.0, 12.5, 14.0),
                1.0 * s,
                stroke,
                StrokeKind::Middle,
            );
            p.rect_stroke(
                rect(6.8, 10.0, 9.2, 14.0),
                0.5 * s,
                stroke,
                StrokeKind::Middle,
            );
        }
        Page::Mirror => {
            p.rect_stroke(
                rect(1.5, 2.5, 14.5, 11.5),
                1.5 * s,
                stroke,
                StrokeKind::Middle,
            );
            p.line_segment([pt(5.5, 14.5), pt(10.5, 14.5)], stroke);
            p.line_segment([pt(8.0, 11.5), pt(8.0, 14.5)], stroke);
            p.add(egui::Shape::convex_polygon(
                vec![pt(6.8, 5.0), pt(10.2, 7.0), pt(6.8, 9.0)],
                color,
                Stroke::NONE,
            ));
        }
        Page::Rescue => {
            // Lifebuoy: ring + four bands.
            p.circle_stroke(pt(8.0, 8.0), 6.2 * s, stroke);
            p.circle_stroke(pt(8.0, 8.0), 2.6 * s, stroke);
            for (dx, dy) in [(1.0, 1.0), (-1.0, 1.0), (1.0, -1.0), (-1.0, -1.0)] {
                let a = pt(8.0 + dx * 1.9, 8.0 + dy * 1.9);
                let b = pt(8.0 + dx * 4.4, 8.0 + dy * 4.4);
                p.line_segment([a, b], stroke);
            }
        }
        Page::Script => {
            p.line_segment([pt(5.0, 4.0), pt(1.5, 8.0)], stroke);
            p.line_segment([pt(1.5, 8.0), pt(5.0, 12.0)], stroke);
            p.line_segment([pt(11.0, 4.0), pt(14.5, 8.0)], stroke);
            p.line_segment([pt(14.5, 8.0), pt(11.0, 12.0)], stroke);
            p.line_segment([pt(9.2, 2.5), pt(6.8, 13.5)], stroke);
        }
        Page::Guide => {
            // Open book.
            p.line_segment([pt(8.0, 3.5), pt(8.0, 13.5)], stroke);
            p.line_segment([pt(8.0, 3.5), pt(1.5, 2.5)], stroke);
            p.line_segment([pt(1.5, 2.5), pt(1.5, 12.5)], stroke);
            p.line_segment([pt(1.5, 12.5), pt(8.0, 13.5)], stroke);
            p.line_segment([pt(8.0, 3.5), pt(14.5, 2.5)], stroke);
            p.line_segment([pt(14.5, 2.5), pt(14.5, 12.5)], stroke);
            p.line_segment([pt(14.5, 12.5), pt(8.0, 13.5)], stroke);
        }
        Page::Tools => {
            // Four tiles.
            for (x, y) in [(1.5, 1.5), (9.0, 1.5), (1.5, 9.0), (9.0, 9.0)] {
                p.rect_stroke(
                    rect(x, y, x + 5.5, y + 5.5),
                    1.2 * s,
                    stroke,
                    StrokeKind::Middle,
                );
            }
        }
        Page::Settings => {
            // Gear: ring + eight teeth.
            p.circle_stroke(pt(8.0, 8.0), 4.4 * s, stroke);
            p.circle_stroke(pt(8.0, 8.0), 1.8 * s, stroke);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                let (sin, cos) = a.sin_cos();
                p.line_segment(
                    [
                        pt(8.0 + cos * 4.4, 8.0 + sin * 4.4),
                        pt(8.0 + cos * 6.8, 8.0 + sin * 6.8),
                    ],
                    stroke,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_round_trip_through_storage_keys() {
        for page in Page::MAIN.into_iter().chain(Page::UTILITY) {
            assert_eq!(Page::from_key(page.key()), page);
        }
        assert_eq!(Page::from_key("unknown"), Page::Home);
        assert_eq!(
            Page::from_key("workspace-tab-from-older-version"),
            Page::Home
        );
    }
}
