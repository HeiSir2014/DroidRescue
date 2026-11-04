//! Custom-painted widgets for the instrument-rail look.

use crate::theme::{
    self, AMBER, AMBER_HI, BG, LINE, RED, SURFACE, SURFACE_HI, SURFACE_HOVER, TEXT, TEXT_DIM,
    TEXT_FAINT,
};
use eframe::egui::{
    self, Align2, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, pos2,
    vec2,
};

const R: CornerRadius = CornerRadius::same(theme::RADIUS);

/// `01  DEVICE ─────────── 设备`
pub fn section(ui: &mut Ui, index: &str, tag: &str, title: &str) {
    ui.add_space(4.0);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
    let p = ui.painter();
    let y = rect.center().y;
    let idx = p.text(
        pos2(rect.left(), y),
        Align2::LEFT_CENTER,
        index,
        theme::strong_font(10.5),
        AMBER,
    );
    let tag_r = p.text(
        pos2(idx.right() + 8.0, y),
        Align2::LEFT_CENTER,
        tag,
        theme::mono(10.0),
        TEXT_FAINT,
    );
    let title_r = p.text(
        pos2(rect.right(), y),
        Align2::RIGHT_CENTER,
        title,
        theme::mono(10.5),
        TEXT_DIM,
    );
    p.line_segment(
        [pos2(tag_r.right() + 8.0, y), pos2(title_r.left() - 8.0, y)],
        Stroke::new(1.0, LINE),
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CaptionIcon {
    Minimize,
    Maximize,
    Restore,
    Close,
    Swap,
}

/// Title-bar button, 44×caption_h; icons are drawn, not glyphs (crisp at any DPI).
pub fn caption_button(ui: &mut Ui, icon: CaptionIcon, height: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(44.0, height), Sense::click());
    let p = ui.painter();
    let hovered = resp.hovered();
    let (fill, fg) = match (icon, hovered, resp.is_pointer_button_down_on()) {
        (CaptionIcon::Close, true, _) => (RED, Color32::WHITE),
        (_, true, true) => (SURFACE_HOVER, AMBER_HI),
        (_, true, false) => (SURFACE_HI, TEXT),
        _ => (Color32::TRANSPARENT, TEXT_DIM),
    };
    p.rect_filled(rect, CornerRadius::ZERO, fill);
    let c = rect.center();
    let s = Stroke::new(1.2, fg);
    match icon {
        CaptionIcon::Minimize => {
            p.line_segment([c + vec2(-5.0, 0.5), c + vec2(5.0, 0.5)], s);
        }
        CaptionIcon::Maximize => {
            p.rect_stroke(
                Rect::from_center_size(c, Vec2::splat(9.0)),
                1.0,
                s,
                StrokeKind::Inside,
            );
        }
        CaptionIcon::Restore => {
            p.rect_stroke(
                Rect::from_center_size(c + vec2(-1.5, 1.5), Vec2::splat(7.5)),
                1.0,
                s,
                StrokeKind::Inside,
            );
            p.line_segment([c + vec2(-2.0, -4.5), c + vec2(4.5, -4.5)], s);
            p.line_segment([c + vec2(4.5, -4.5), c + vec2(4.5, 2.0)], s);
        }
        CaptionIcon::Close => {
            p.line_segment([c + vec2(-4.5, -4.5), c + vec2(4.5, 4.5)], s);
            p.line_segment([c + vec2(4.5, -4.5), c + vec2(-4.5, 4.5)], s);
        }
        CaptionIcon::Swap => {
            for (dy, dir) in [(-2.5_f32, 1.0_f32), (2.5, -1.0)] {
                let a = c + vec2(-5.0 * dir, dy);
                let b = c + vec2(5.0 * dir, dy);
                p.line_segment([a, b], s);
                p.line_segment([b, b + vec2(-2.5 * dir, -2.0)], s);
                p.line_segment([b, b + vec2(-2.5 * dir, 2.0)], s);
            }
        }
    }
    resp
}

/// Full-width amber call-to-action.
pub fn primary(ui: &mut Ui, text: &str, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), sense);
    let p = ui.painter();
    let fill = match (enabled, resp.hovered(), resp.is_pointer_button_down_on()) {
        (false, ..) => SURFACE_HI,
        (_, _, true) => AMBER.gamma_multiply(0.85),
        (_, true, _) => AMBER_HI,
        _ => AMBER,
    };
    p.rect_filled(rect, R, fill);
    let fg = if enabled { BG } else { TEXT_FAINT };
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

/// Full-width outlined "stop" action.
pub fn danger(ui: &mut Ui, text: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
    let p = ui.painter();
    let fill = if resp.hovered() {
        RED.gamma_multiply(0.14)
    } else {
        Color32::TRANSPARENT
    };
    p.rect_filled(rect, R, fill);
    p.rect_stroke(
        rect,
        R,
        Stroke::new(1.0, RED.gamma_multiply(0.8)),
        StrokeKind::Inside,
    );
    p.text(
        rect.center(),
        Align2::CENTER_CENTER,
        text,
        theme::strong_font(14.0),
        RED,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Hardware-style key: glyph on top, small caption below.
pub fn key(ui: &mut Ui, glyph: &str, label: &str, size: Vec2, enabled: bool) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let p = ui.painter();
    let down = resp.is_pointer_button_down_on();
    let (fill, stroke, fg) = match (enabled, resp.hovered(), down) {
        (false, ..) => (SURFACE, LINE, TEXT_FAINT),
        (_, _, true) => (theme::AMBER_LO, AMBER, AMBER_HI),
        (_, true, _) => (SURFACE_HOVER, AMBER.gamma_multiply(0.55), TEXT),
        _ => (SURFACE_HI, LINE, TEXT),
    };
    p.rect_filled(rect, R, fill);
    p.rect_stroke(rect, R, Stroke::new(1.0, stroke), StrokeKind::Inside);
    let glyph_pos = pos2(rect.center().x, rect.top() + rect.height() * 0.42);
    p.text(
        glyph_pos,
        Align2::CENTER_CENTER,
        glyph,
        theme::mono(17.0),
        fg,
    );
    let label_pos = pos2(rect.center().x, rect.bottom() - 9.0);
    p.text(
        label_pos,
        Align2::CENTER_CENTER,
        label,
        theme::mono(9.5),
        if enabled { TEXT_DIM } else { TEXT_FAINT },
    );
    resp
}

/// Small readout cell: `TAG` above a coloured value.
pub fn readout(ui: &mut Ui, width: f32, tag: &str, value: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 44.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, R, SURFACE);
    p.rect_stroke(rect, R, Stroke::new(1.0, LINE), StrokeKind::Inside);
    p.text(
        rect.left_top() + vec2(9.0, 8.0),
        Align2::LEFT_TOP,
        tag,
        theme::mono(9.5),
        TEXT_FAINT,
    );
    p.circle_filled(pos2(rect.left() + 12.0, rect.bottom() - 13.0), 2.5, color);
    p.text(
        pos2(rect.left() + 20.0, rect.bottom() - 13.0),
        Align2::LEFT_CENTER,
        value,
        theme::strong_font(12.5),
        color,
    );
}

/// Full-width row: label left, pill switch right. `changed()` on toggle.
pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::click());
    toggle_on_click(&mut resp, on);
    let track = Rect::from_min_size(
        pos2(rect.right() - 34.0, rect.center().y - 9.0),
        vec2(34.0, 18.0),
    );
    paint_switch(ui, &resp, track, *on);
    let fg = if resp.hovered() { TEXT } else { TEXT_DIM };
    ui.painter().text(
        pos2(rect.left(), rect.center().y),
        Align2::LEFT_CENTER,
        label,
        theme::mono(12.0),
        fg,
    );
    resp
}

/// Compact: small switch then label, sized to content.
pub fn switch_inline(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), theme::mono(11.0), TEXT_DIM);
    let size = vec2(28.0 + 8.0 + galley.size().x, 22.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    toggle_on_click(&mut resp, on);
    let track = Rect::from_min_size(pos2(rect.left(), rect.center().y - 7.5), vec2(28.0, 15.0));
    paint_switch(ui, &resp, track, *on);
    let fg = if resp.hovered() { TEXT } else { TEXT_DIM };
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
    let p = ui.painter();
    let t = ui.ctx().animate_bool_responsive(resp.id, on);
    let radius = CornerRadius::same((track.height() / 2.0) as u8);
    p.rect_filled(track, radius, lerp_color(SURFACE_HI, theme::AMBER_LO, t));
    p.rect_stroke(
        track,
        radius,
        Stroke::new(1.0, lerp_color(LINE, AMBER, t)),
        StrokeKind::Inside,
    );
    let inset = track.height() / 2.0;
    let knob_x = egui::lerp((track.left() + inset)..=(track.right() - inset), t);
    p.circle_filled(
        pos2(knob_x, track.center().y),
        inset - 3.0,
        lerp_color(TEXT_DIM, AMBER, t),
    );
}

/// Quiet text action: dim, amber + underline on hover.
pub fn link(ui: &mut Ui, text: &str) -> Response {
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), theme::mono(10.5), TEXT_DIM);
    let (rect, resp) = ui.allocate_exact_size(galley.size() + vec2(4.0, 6.0), Sense::click());
    let color = if resp.hovered() { AMBER } else { TEXT_DIM };
    let pos = pos2(rect.left() + 2.0, rect.center().y - galley.size().y / 2.0);
    if resp.hovered() {
        let y = pos.y + galley.size().y + 0.5;
        ui.painter().line_segment(
            [pos2(pos.x, y), pos2(pos.x + galley.size().x, y)],
            Stroke::new(1.0, AMBER),
        );
    }
    ui.painter().galley(pos, galley, color);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Hairline frame around the whole window (the drawn "border").
pub fn window_frame(ui: &Ui, active: bool) {
    let rect = ui.ctx().content_rect();
    let color = if active {
        Color32::from_rgb(0x38, 0x3D, 0x44)
    } else {
        LINE
    };
    ui.painter().rect_stroke(
        rect,
        CornerRadius::ZERO,
        Stroke::new(1.0, color),
        StrokeKind::Inside,
    );
}
