//! Custom-painted widgets for the instrument-rail look. Colours come from
//! the active palette (`theme::c()`), so every widget follows light / dark.

use crate::icons::{self, KeyIcon};
use crate::theme;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
};

const R: CornerRadius = CornerRadius::same(theme::RADIUS);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CaptionIcon {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// Title-bar button, 44×caption_h; icons are drawn, not glyphs (crisp at any DPI).
pub fn caption_button(ui: &mut Ui, icon: CaptionIcon, height: f32) -> Response {
    let c = theme::c();
    let (rect, resp) = ui.allocate_exact_size(vec2(44.0, height), Sense::click());
    let p = ui.painter();
    let hovered = resp.hovered();
    let (fill, fg) = match (icon, hovered, resp.is_pointer_button_down_on()) {
        // Windows convention: close turns red with a white glyph in both themes.
        (CaptionIcon::Close, true, _) => (c.red, Color32::WHITE),
        (_, true, true) => (c.surface_hover, c.amber_hi),
        (_, true, false) => (c.surface_hi, c.text),
        _ => (Color32::TRANSPARENT, c.text_dim),
    };
    p.rect_filled(rect, CornerRadius::ZERO, fill);
    let m = rect.center();
    let s = Stroke::new(1.2, fg);
    match icon {
        CaptionIcon::Minimize => {
            p.line_segment([m + vec2(-5.0, 0.5), m + vec2(5.0, 0.5)], s);
        }
        CaptionIcon::Maximize => {
            p.rect_stroke(
                Rect::from_center_size(m, Vec2::splat(9.0)),
                1.0,
                s,
                StrokeKind::Inside,
            );
        }
        CaptionIcon::Restore => {
            p.rect_stroke(
                Rect::from_center_size(m + vec2(-1.5, 1.5), Vec2::splat(7.5)),
                1.0,
                s,
                StrokeKind::Inside,
            );
            p.line_segment([m + vec2(-2.0, -4.5), m + vec2(4.5, -4.5)], s);
            p.line_segment([m + vec2(4.5, -4.5), m + vec2(4.5, 2.0)], s);
        }
        CaptionIcon::Close => {
            p.line_segment([m + vec2(-4.5, -4.5), m + vec2(4.5, 4.5)], s);
            p.line_segment([m + vec2(4.5, -4.5), m + vec2(-4.5, 4.5)], s);
        }
    }
    resp
}

/// Full-width amber call-to-action.
pub fn primary(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let c = theme::c();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), sense);
    let p = ui.painter();
    let fill = match (enabled, resp.hovered(), resp.is_pointer_button_down_on()) {
        (false, ..) => c.surface_hi,
        (_, _, true) => c.amber.gamma_multiply(0.85),
        (_, true, _) => c.amber_hi,
        _ => c.amber,
    };
    p.rect_filled(rect, R, fill);
    let fg = if enabled { c.on_amber } else { c.text_faint };
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        theme::strong_font(14.0),
        fg,
    );
    resp.on_hover_cursor(if enabled {
        egui::CursorIcon::PointingHand
    } else {
        egui::CursorIcon::NotAllowed
    })
}

/// Hardware-style key: icon over a caption (tall keys) or icon beside it
/// (short keys next to a text field).
pub fn key(ui: &mut Ui, icon: KeyIcon, label: &str, size: Vec2, enabled: bool) -> Response {
    key_with_hint(ui, icon, label, None, size, enabled)
}

/// [`key`] with its keyboard shortcut ("F5") shown in the top-right corner.
pub fn key_with_hint(
    ui: &mut Ui,
    icon: KeyIcon,
    label: &str,
    hint: Option<&str>,
    size: Vec2,
    enabled: bool,
) -> Response {
    let c = theme::c();
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let p = ui.painter();
    let down = resp.is_pointer_button_down_on();
    let (fill, stroke, fg) = match (enabled, resp.hovered(), down) {
        (false, ..) => (c.surface, c.line, c.text_faint),
        (_, _, true) => (c.amber_lo, c.amber, c.amber_hi),
        (_, true, _) => (c.surface_hover, c.amber.gamma_multiply(0.55), c.text),
        _ => (c.surface_hi, c.line, c.text),
    };
    p.rect_filled(rect, R, fill);
    p.rect_stroke(rect, R, Stroke::new(1.0, stroke), StrokeKind::Inside);
    let label_color = if enabled { c.text_dim } else { c.text_faint };
    if size.y >= 40.0 {
        // Icon over caption, centred together by the caption's real height.
        let icon_size = 18.0;
        let label = p.layout_no_wrap(label.to_string(), theme::ui(11.5), label_color);
        let gap = 4.0;
        let top = rect.center().y - (icon_size + gap + label.size().y) / 2.0;
        icons::draw_key(
            p,
            icon,
            pos2(rect.center().x, top + icon_size / 2.0),
            icon_size,
            fg,
        );
        let label_pos = pos2(
            rect.center().x - label.size().x / 2.0,
            top + icon_size + gap,
        );
        p.galley(label_pos, label, label_color);
    } else {
        // Icon beside the label.
        let icon_size = 14.0;
        let label = p.layout_no_wrap(label.to_string(), theme::ui(12.5), fg);
        let gap = 6.0;
        let left = rect.center().x - (icon_size + gap + label.size().x) / 2.0;
        icons::draw_key(
            p,
            icon,
            pos2(left + icon_size / 2.0, rect.center().y),
            icon_size,
            fg,
        );
        let label_pos = pos2(
            left + icon_size + gap,
            rect.center().y - label.size().y / 2.0,
        );
        p.galley(label_pos, label, fg);
    }
    if let Some(hint) = hint {
        let hint = p.layout_no_wrap(hint.to_string(), theme::mono(9.5), c.text_faint);
        let pos = pos2(rect.right() - hint.size().x - 6.0, rect.top() + 4.0);
        p.galley(pos, hint, c.text_faint);
    }
    resp
}

/// Full-width row: label left, pill switch right. `changed()` on toggle.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let c = theme::c();
    let (rect, mut resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    toggle_on_click(&mut resp, on);
    let track = Rect::from_min_size(
        pos2(rect.right() - 34.0, rect.center().y - 9.0),
        vec2(34.0, 18.0),
    );
    paint_switch(ui, &resp, track, *on);
    let fg = if resp.hovered() { c.text } else { c.text_dim };
    ui.painter().text(
        pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        label,
        theme::ui(12.5),
        fg,
    );
    resp
}

/// Compact: small switch then label, sized to content.
pub fn switch_inline(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let c = theme::c();
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), theme::ui(12.0), c.text_dim);
    let size = vec2(28.0 + 8.0 + galley.size().x, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    toggle_on_click(&mut resp, on);
    let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - 7.5), vec2(28.0, 15.0));
    paint_switch(ui, &resp, track, *on);
    let fg = if resp.hovered() { c.text } else { c.text_dim };
    ui.painter().galley(
        pos2(track.right() + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        fg,
    );
    resp
}

fn toggle_on_click(resp: &mut Response, on: &mut bool) {
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
}

fn paint_switch(ui: &Ui, resp: &Response, track: Rect, on: bool) {
    let c = theme::c();
    let p = ui.painter();
    let t = ui.ctx().animate_bool_responsive(resp.id, on);
    let radius = CornerRadius::same((track.height() / 2.0) as u8);
    p.rect_filled(track, radius, lerp_color(c.surface_hi, c.amber_lo, t));
    p.rect_stroke(
        track,
        radius,
        Stroke::new(1.0, lerp_color(c.line, c.amber, t)),
        StrokeKind::Inside,
    );
    let inset = track.height() / 2.0;
    let knob_x = egui::lerp((track.left() + inset)..=(track.right() - inset), t);
    p.circle_filled(
        pos2(knob_x, track.center().y),
        inset - 3.0,
        lerp_color(c.text_dim, c.amber, t),
    );
}

/// Quiet text action: dim, amber + underline on hover.
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let c = theme::c();
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), theme::ui(11.5), c.text_dim);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(4.0, 6.0), Sense::click());
    let color = if resp.hovered() { c.amber } else { c.text_dim };
    let pos = pos2(rect.left() + 2.0, rect.center().y - galley.size().y / 2.0);
    if resp.hovered() {
        let y = pos.y + galley.size().y + 0.5;
        ui.painter().line_segment(
            [pos2(pos.x, y), pos2(pos.x + galley.size().x, y)],
            Stroke::new(1.0, c.amber),
        );
    }
    ui.painter().galley(pos, galley, color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Segmented control: one of `options`, the selected one highlighted.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let c = theme::c();
    let gap = 4.0;
    // Never negative: a window mid-resize can momentarily have no room.
    let w = ((ui.available_width() - gap * (options.len() as f32 - 1.0)) / options.len() as f32)
        .max(0.0);
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (option, label) in options {
            let (rect, resp) = ui.allocate_exact_size(vec2(w, 28.0), Sense::click());
            let selected = *value == *option;
            let (fill, stroke, fg) = match (selected, resp.hovered()) {
                (true, _) => (c.amber_lo, c.amber, c.amber),
                (false, true) => (c.surface_hover, c.line, c.text),
                (false, false) => (c.surface_hi, c.line, c.text_dim),
            };
            let p = ui.painter();
            p.rect_filled(rect, R, fill);
            p.rect_stroke(rect, R, Stroke::new(1.0, stroke), StrokeKind::Inside);
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                *label,
                theme::ui(12.0),
                fg,
            );
            if resp
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
                && !selected
            {
                *value = *option;
                changed = true;
            }
        }
    });
    changed
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// A small rounded version tag ("v0.2.0") whose left edge is at
/// `left_center`; returns its rect.
pub fn version_tag(p: &egui::Painter, left_center: egui::Pos2, version: &str) -> Rect {
    let c = theme::c();
    let galley = p.layout_no_wrap(version.to_string(), theme::mono(10.5), c.text_dim);
    let rect = Rect::from_min_size(
        pos2(left_center.x, left_center.y - 9.0),
        vec2(galley.size().x + 12.0, 18.0),
    );
    p.rect_filled(rect, 9.0, c.surface_hi);
    p.rect_stroke(rect, 9.0, Stroke::new(1.0, c.line), StrokeKind::Inside);
    p.galley(
        pos2(rect.left() + 6.0, rect.center().y - galley.size().y / 2.0),
        galley,
        c.text_dim,
    );
    rect
}

/// Hairline frame around the whole window (the drawn "border").
pub fn window_frame(ui: &Ui, active: bool) {
    let c = theme::c();
    let rect = ui.ctx().content_rect();
    let color = if active { c.text_faint } else { c.line };
    ui.painter().rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, color),
        StrokeKind::Inside,
    );
}
