//! 坐标拾取板: a pad shaped like the device screen beside the script editor.
//! The mouse over it reads out the normalised x,y (the script's
//! coordinates for the chosen `rot:`), optionally hovers the pen there on
//! the device in real time, and a click pins the point to insert as
//! `move:` / `click:`.

use crate::rescue::{self, Pointer};
use crate::{theme, widgets};
use eframe::egui::{self, Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, Vec2, pos2, vec2};
use std::time::{Duration, Instant};

/// Screen shapes, as long side : short side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aspect {
    Phone,
    Tablet,
    Tablet43,
}

impl Aspect {
    const ALL: [(Aspect, &'static str); 3] = [
        (Aspect::Phone, "手机 20:9"),
        (Aspect::Tablet, "平板 16:10"),
        (Aspect::Tablet43, "平板 4:3"),
    ];

    fn long_over_short(self) -> f32 {
        match self {
            Aspect::Phone => 20.0 / 9.0,
            Aspect::Tablet => 16.0 / 10.0,
            Aspect::Tablet43 => 4.0 / 3.0,
        }
    }

    /// A guess from a model / profile name.
    pub fn guess(name: &str) -> Aspect {
        let lower = name.to_lowercase();
        if lower.contains("pad") || lower.contains("tab") || name.contains("平板") {
            Aspect::Tablet
        } else {
            Aspect::Phone
        }
    }
}

/// `rot:` values with what they look like.
const ROTATIONS: [(u8, &str); 4] = [(0, "竖屏 0"), (1, "横屏 1"), (2, "倒竖 2"), (3, "反横 3")];

/// Width / height of the screen as displayed with `rotation`.
pub fn displayed_ratio(aspect: Aspect, rotation: u8) -> f32 {
    let r = aspect.long_over_short();
    if rotation.is_multiple_of(2) {
        1.0 / r
    } else {
        r
    }
}

/// The largest rect with `ratio` (w/h) fitting in `avail`, centred.
pub fn fit(avail: Rect, ratio: f32) -> Rect {
    let size = if avail.width() / avail.height().max(1.0) > ratio {
        vec2(avail.height() * ratio, avail.height())
    } else {
        vec2(avail.width(), avail.width() / ratio)
    };
    Rect::from_center_size(avail.center(), size)
}

/// `pos` inside `screen` as 0..1 fractions (clamped).
pub fn normalise(screen: Rect, pos: Pos2) -> (f32, f32) {
    let f = |v: f32, lo: f32, len: f32| ((v - lo) / len.max(1.0)).clamp(0.0, 1.0);
    (
        f(pos.x, screen.left(), screen.width()),
        f(pos.y, screen.top(), screen.height()),
    )
}

/// 0.512 / 0.88 / 0 / 1: three decimals, trailing zeros dropped.
pub fn fmt_coord(v: f32) -> String {
    let s = format!("{:.3}", v.clamp(0.0, 1.0));
    let s = s.trim_end_matches('0').trim_end_matches('.');
    s.to_string()
}

/// "click:0.512,0.88".
pub fn command(kind: &str, (x, y): (f32, f32)) -> String {
    format!("{kind}:{},{}", fmt_coord(x), fmt_coord(y))
}

/// Fractions → the script's 1/10000 units.
fn units(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 10_000.0).round() as u16
}

/// Rate limit for live hovering (the device takes one HID report each).
const HOVER_INTERVAL: Duration = Duration::from_millis(33);

pub struct Pad {
    aspect: Aspect,
    rotation: u8,
    /// Hover the pen on the device as the mouse moves.
    sync: bool,
    pinned: Option<(f32, f32)>,
    last_sent: Option<((u16, u16, u8), Instant)>,
    /// Aspect chosen by hand (no more guessing from the profile).
    aspect_chosen: bool,
}

impl Default for Pad {
    fn default() -> Self {
        Pad {
            aspect: Aspect::Phone,
            rotation: rescue::DEFAULT_ROTATION,
            sync: true,
            pinned: None,
            last_sent: None,
            aspect_chosen: false,
        }
    }
}

/// What the pad asks of the editor.
pub enum PadAction {
    /// Insert this line after the cursor's line.
    Insert(String),
}

pub struct PadContext<'a> {
    pub rescue: &'a rescue::Rescue,
    /// A device is held (hovering reaches it).
    pub held: bool,
    /// A script is running: no live hovering in between its steps.
    pub busy: bool,
    /// Model / profile name, to guess the screen shape.
    pub name_hint: &'a str,
}

impl Pad {
    pub fn ui(&mut self, ui: &mut egui::Ui, cx: &PadContext) -> Option<PadAction> {
        let c = theme::c();
        if !self.aspect_chosen {
            self.aspect = Aspect::guess(cx.name_hint);
        }
        let mut action = None;
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(
            RichText::new("坐标拾取板")
                .font(theme::strong_font(14.0))
                .color(c.text),
        );
        let before = self.aspect;
        widgets::segmented(ui, &mut self.aspect, &Aspect::ALL);
        if self.aspect != before {
            self.aspect_chosen = true;
        }
        if widgets::segmented(ui, &mut self.rotation, &ROTATIONS) {
            self.pinned = None; // a point means nothing in another rotation
        }

        // The pad: as much room as is left above the readout and actions.
        let reserve = 150.0;
        let avail = Rect::from_min_size(
            ui.cursor().min,
            vec2(
                ui.available_width(),
                (ui.available_height() - reserve).max(160.0),
            ),
        );
        let screen = fit(
            avail.shrink(10.0),
            displayed_ratio(self.aspect, self.rotation),
        );
        let resp = ui.allocate_rect(avail, Sense::click());
        let hover = resp.hover_pos().filter(|p| screen.contains(*p));
        let hovered = hover.map(|p| normalise(screen, p));
        if resp.clicked()
            && let Some(p) = resp.interact_pointer_pos().filter(|p| screen.contains(*p))
        {
            self.pinned = Some(normalise(screen, p));
        }
        self.paint(ui.painter(), screen, hovered);
        if hover.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }

        // Live hover on the device: rate limited, only on change.
        if self.sync
            && cx.held
            && !cx.busy
            && let Some((x, y)) = hovered
        {
            let key = (units(x), units(y), self.rotation);
            let due = self
                .last_sent
                .is_none_or(|(last, at)| last != key && at.elapsed() >= HOVER_INTERVAL);
            if due {
                cx.rescue.pointer(Pointer {
                    x: key.0,
                    y: key.1,
                    rotation: self.rotation,
                    connect: false,
                });
                self.last_sent = Some((key, Instant::now()));
            } else if self.last_sent.is_some_and(|(last, _)| last != key) {
                // The last position must still reach the device.
                ui.ctx().request_repaint_after(HOVER_INTERVAL);
            }
        }

        // Readout: live position, pinned point.
        let mut readout = |label: &str, p: Option<(f32, f32)>, color: Color32| {
            let text = p.map_or("—".to_string(), |(x, y)| {
                format!("x {}   y {}", fmt_coord(x), fmt_coord(y))
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new(label).font(theme::ui(12.0)).color(c.text_dim));
                ui.label(RichText::new(text).font(theme::mono(13.0)).color(color));
            });
        };
        readout("鼠标", hovered, c.text);
        readout("已固定", self.pinned, c.amber);

        ui.horizontal(|ui| {
            let enabled = self.pinned.is_some();
            for kind in ["move", "click"] {
                if ui
                    .add_enabled(enabled, egui::Button::new(format!("插入 {kind}")))
                    .on_hover_text("插入到编辑器光标所在行的下一行")
                    .on_disabled_hover_text("先在拾取板上点一下固定坐标")
                    .clicked()
                    && let Some(p) = self.pinned
                {
                    action = Some(PadAction::Insert(command(kind, p)));
                }
            }
            if ui.add_enabled(enabled, egui::Button::new("复制")).clicked()
                && let Some(p) = self.pinned
            {
                ui.ctx().copy_text(command("click", p));
            }
        });
        ui.label(
            RichText::new(format!(
                "坐标按 rot:{} 计算：脚本里这一行之前要有 rot:{}",
                self.rotation, self.rotation
            ))
            .font(theme::ui(11.0))
            .color(c.text_faint),
        );

        ui.horizontal(|ui| {
            widgets::switch_inline(ui, &mut self.sync, "实时同步到设备（悬停，不点击）");
        });
        if self.sync && !cx.held {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("设备未接管")
                        .font(theme::ui(11.5))
                        .color(c.text_faint),
                );
                if ui
                    .add_enabled(!cx.busy, egui::Button::new("接管设备"))
                    .clicked()
                {
                    let (x, y) = self.pinned.unwrap_or((0.5, 0.5));
                    cx.rescue.pointer(Pointer {
                        x: units(x),
                        y: units(y),
                        rotation: self.rotation,
                        connect: true,
                    });
                }
            });
        }
        action
    }

    /// The screen (with a camera dot showing which edge is the device's
    /// top), a crosshair at the mouse, the pinned point.
    fn paint(&self, p: &egui::Painter, screen: Rect, hovered: Option<(f32, f32)>) {
        let c = theme::c();
        let bezel = screen.expand(6.0);
        p.rect_filled(bezel, 14.0, Color32::from_rgb(0x15, 0x18, 0x1C));
        p.rect_filled(screen, 8.0, c.well);
        // Grid at quarters, for orientation.
        let grid = Stroke::new(1.0, c.line);
        for i in 1..4 {
            let t = i as f32 / 4.0;
            let x = screen.left() + screen.width() * t;
            let y = screen.top() + screen.height() * t;
            p.line_segment([pos2(x, screen.top()), pos2(x, screen.bottom())], grid);
            p.line_segment([pos2(screen.left(), y), pos2(screen.right(), y)], grid);
        }
        // Camera: the device's natural top edge as displayed with `rot`
        // (1 = turned so that edge is on the left).
        let cam = match self.rotation {
            0 => pos2(screen.center().x, bezel.top() + 3.0),
            1 => pos2(bezel.left() + 3.0, screen.center().y),
            2 => pos2(screen.center().x, bezel.bottom() - 3.0),
            _ => pos2(bezel.right() - 3.0, screen.center().y),
        };
        p.circle_filled(cam, 2.0, Color32::from_gray(0x55));

        let at = |(x, y): (f32, f32)| {
            pos2(
                screen.left() + screen.width() * x,
                screen.top() + screen.height() * y,
            )
        };
        if let Some(point) = self.pinned {
            let q = at(point);
            p.circle_filled(q, 6.0, c.amber.gamma_multiply(0.3));
            p.circle_filled(q, 3.0, c.amber);
        }
        if let Some(point) = hovered {
            let q = at(point);
            let hair = Stroke::new(1.0, c.text_dim);
            p.line_segment([pos2(screen.left(), q.y), pos2(screen.right(), q.y)], hair);
            p.line_segment([pos2(q.x, screen.top()), pos2(q.x, screen.bottom())], hair);
            p.circle_stroke(q, 5.0, Stroke::new(1.5, c.text));
            // Live label beside the point, kept inside the screen.
            let text = format!("{}, {}", fmt_coord(point.0), fmt_coord(point.1));
            let galley = p.layout_no_wrap(text, theme::mono(11.0), c.text);
            let size = galley.size() + Vec2::splat(8.0);
            let mut pos = q + vec2(10.0, -size.y - 6.0);
            pos.x = pos.x.min(screen.right() - size.x);
            pos.y = pos.y.max(screen.top());
            let bg = Rect::from_min_size(pos, size);
            p.rect_filled(bg, 4.0, c.surface_hi);
            p.galley(bg.min + Vec2::splat(4.0), galley, c.text);
        } else {
            p.text(
                screen.center(),
                Align2::CENTER_CENTER,
                "把鼠标移到这里",
                theme::ui(12.0),
                c.text_faint,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coordinates_are_fractions_of_the_screen_and_clamped() {
        let screen = Rect::from_min_size(pos2(100.0, 50.0), vec2(200.0, 400.0));
        assert_eq!(normalise(screen, pos2(100.0, 50.0)), (0.0, 0.0));
        assert_eq!(normalise(screen, pos2(200.0, 350.0)), (0.5, 0.75));
        assert_eq!(normalise(screen, pos2(0.0, 999.0)), (0.0, 1.0), "clamped");
    }

    #[test]
    fn the_pad_takes_the_displayed_shape() {
        // Phone 20:9 upright is tall; rot:1 lies on its side.
        assert!((displayed_ratio(Aspect::Phone, 0) - 0.45).abs() < 1e-3);
        assert!((displayed_ratio(Aspect::Phone, 1) - 20.0 / 9.0).abs() < 1e-3);
        assert_eq!(
            displayed_ratio(Aspect::Tablet, 2),
            displayed_ratio(Aspect::Tablet, 0)
        );
        let avail = Rect::from_min_size(Pos2::ZERO, vec2(300.0, 300.0));
        let tall = fit(avail, 0.5);
        assert_eq!(tall.size(), vec2(150.0, 300.0));
        assert_eq!(tall.center(), avail.center());
        let wide = fit(avail, 2.0);
        assert_eq!(wide.size(), vec2(300.0, 150.0));
    }

    #[test]
    fn coordinates_print_like_the_scripts_write_them() {
        assert_eq!(fmt_coord(0.5), "0.5");
        assert_eq!(fmt_coord(0.88), "0.88");
        assert_eq!(fmt_coord(0.51234), "0.512");
        assert_eq!(fmt_coord(0.0), "0");
        assert_eq!(fmt_coord(1.0), "1");
        assert_eq!(command("click", (0.5, 0.88)), "click:0.5,0.88");
    }

    #[test]
    fn inserted_commands_parse_back_to_the_same_point() {
        let line = command("move", (0.512, 0.88));
        assert_eq!(
            rescue::parse(&line, &[]).unwrap(),
            vec![rescue::Step::Hover(5120, 8800)]
        );
        assert_eq!(units(0.512), 5120);
    }

    #[test]
    fn the_shape_is_guessed_from_the_model_name() {
        assert_eq!(Aspect::guess("vivo Pad3"), Aspect::Tablet);
        assert_eq!(Aspect::guess("vivo X200 Ultra"), Aspect::Phone);
        assert_eq!(Aspect::guess("Galaxy Tab S9"), Aspect::Tablet);
    }
}
