//! The in-app log: entries with a level and a time, and the painting the
//! bottom status bar shares (level icons, relative times).
//!
//! Messages keep their historical prefixes (`✔` done, `✘` failed, `…` in
//! progress, `▶`/`■` start/stop) — the log file shows them as written; the
//! bar turns them into a coloured icon and drops the character.

use crate::theme;
use eframe::egui::{self, Color32, Painter, Pos2, Response, Sense, Stroke, Ui, pos2, vec2};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Error,
    Progress,
    Info,
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// Increases for every entry ever logged (survives trimming): "unseen
    /// errors" compare against it.
    pub seq: u64,
    pub at: Instant,
    pub level: Level,
    /// The message without its level character.
    pub text: String,
}

impl Entry {
    pub fn new(message: &str) -> Entry {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let (level, text) = parse(message);
        Entry {
            seq: SEQ.fetch_add(1, Ordering::Relaxed),
            at: Instant::now(),
            level,
            text: text.to_string(),
        }
    }

    /// As written to the clipboard / file.
    pub fn line(&self) -> String {
        let mark = match self.level {
            Level::Ok => "✔ ",
            Level::Error => "✘ ",
            Level::Progress => "… ",
            Level::Info => "",
        };
        format!("{mark}{}", self.text)
    }
}

/// Level from the leading character; the rest, trimmed.
pub fn parse(message: &str) -> (Level, &str) {
    let mut chars = message.chars();
    let level = match chars.next() {
        Some('✔') => Level::Ok,
        Some('✘') => Level::Error,
        Some('…' | '▶' | '●') => Level::Progress,
        Some('■') => Level::Info,
        _ => return (Level::Info, message.trim()),
    };
    (level, chars.as_str().trim())
}

/// "刚刚", "12 秒前", "3 分钟前", "2 小时前".
pub fn ago(elapsed: Duration) -> String {
    let s = elapsed.as_secs();
    match s {
        0..10 => "刚刚".into(),
        10..60 => format!("{s} 秒前"),
        60..3600 => format!("{} 分钟前", s / 60),
        _ => format!("{} 小时前", s / 3600),
    }
}

pub fn color(level: Level) -> Color32 {
    let c = theme::c();
    match level {
        Level::Ok => c.green,
        Level::Error => c.red,
        Level::Progress => c.amber,
        Level::Info => c.text_faint,
    }
}

/// Level icon, `size` px, centred on `at`: ✓ / ✕ in a filled disc, a ring
/// for progress, a dot for information.
pub fn paint_level(p: &Painter, level: Level, at: Pos2, size: f32) {
    let color = color(level);
    let r = size / 2.0;
    let mark = Stroke::new(size * 0.13, theme::c().surface);
    match level {
        Level::Ok => {
            p.circle_filled(at, r, color);
            p.line_segment(
                [at + vec2(-r * 0.45, 0.0), at + vec2(-r * 0.1, r * 0.35)],
                mark,
            );
            p.line_segment(
                [at + vec2(-r * 0.1, r * 0.35), at + vec2(r * 0.5, -r * 0.35)],
                mark,
            );
        }
        Level::Error => {
            p.circle_filled(at, r, color);
            let d = r * 0.38;
            p.line_segment([at + vec2(-d, -d), at + vec2(d, d)], mark);
            p.line_segment([at + vec2(d, -d), at + vec2(-d, d)], mark);
        }
        Level::Progress => {
            p.circle_stroke(at, r * 0.8, Stroke::new(size * 0.14, color));
        }
        Level::Info => {
            p.circle_filled(at, r * 0.35, color);
        }
    }
}

/// Red pill "✕ N 个错误" (clickable).
pub fn error_badge(ui: &mut Ui, count: usize) -> Response {
    let c = theme::c();
    let galley = ui
        .painter()
        .layout_no_wrap(format!("{count} 个错误"), theme::ui(11.5), c.red);
    let size = vec2(galley.size().x + 32.0, 20.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("查看日志");
    let p = ui.painter();
    let alpha = if resp.hovered() { 0.24 } else { 0.14 };
    p.rect_filled(rect, rect.height() / 2.0, c.red.gamma_multiply(alpha));
    paint_level(
        p,
        Level::Error,
        pos2(rect.left() + 13.0, rect.center().y),
        11.0,
    );
    p.galley(
        pos2(rect.left() + 23.0, rect.center().y - galley.size().y / 2.0),
        galley,
        c.red,
    );
    resp
}

/// One row of the expanded log: how long ago, level icon, the message
/// (selectable, wrapped; errors in red).
pub fn row(ui: &mut Ui, entry: &Entry, now: Instant) {
    let c = theme::c();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (time_rect, _) = ui.allocate_exact_size(vec2(64.0, 18.0), Sense::hover());
        ui.painter().text(
            pos2(time_rect.right(), time_rect.center().y),
            egui::Align2::RIGHT_CENTER,
            ago(now.saturating_duration_since(entry.at)),
            theme::ui(11.0),
            c.text_faint,
        );
        let (icon_rect, _) = ui.allocate_exact_size(vec2(12.0, 18.0), Sense::hover());
        paint_level(ui.painter(), entry.level, icon_rect.center(), 11.0);
        let color = match entry.level {
            Level::Error => c.red,
            Level::Info => c.text_dim,
            Level::Ok | Level::Progress => c.text,
        };
        ui.add(
            egui::Label::new(
                egui::RichText::new(&entry.text)
                    .font(theme::ui(12.0))
                    .color(color),
            )
            .wrap()
            .selectable(true),
        );
    });
}

/// A spinner: a three-quarter arc turning with `time` (seconds).
pub fn paint_spinner(p: &Painter, at: Pos2, size: f32, time: f64, color: Color32) {
    use std::f32::consts::TAU;
    let start = (time as f32 * TAU * 0.9) % TAU;
    let r = size / 2.0 * 0.8;
    let pts = (0..=18)
        .map(|i| {
            let a = start + TAU * 0.75 * i as f32 / 18.0;
            at + vec2(a.cos(), a.sin()) * r
        })
        .collect();
    p.add(eframe::egui::Shape::line(
        pts,
        Stroke::new(size * 0.14, color),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_come_from_the_prefix_and_are_stripped() {
        assert_eq!(parse("✔ 已连接 Pad"), (Level::Ok, "已连接 Pad"));
        assert_eq!(parse("✘ 投屏失败: x"), (Level::Error, "投屏失败: x"));
        assert_eq!(
            parse("… 导出 /sdcard/DCIM"),
            (Level::Progress, "导出 /sdcard/DCIM")
        );
        assert_eq!(parse("▶ 正在连接"), (Level::Progress, "正在连接"));
        assert_eq!(parse("■ 投屏已停止"), (Level::Info, "投屏已停止"));
        assert_eq!(parse("server 5.0.1"), (Level::Info, "server 5.0.1"));
    }

    #[test]
    fn entries_round_trip_to_their_log_line() {
        assert_eq!(Entry::new("✘ 失败").line(), "✘ 失败");
        assert_eq!(Entry::new("普通信息").line(), "普通信息");
    }

    #[test]
    fn sequence_numbers_increase() {
        let a = Entry::new("a");
        let b = Entry::new("b");
        assert!(b.seq > a.seq);
    }

    #[test]
    fn relative_times_read_naturally() {
        assert_eq!(ago(Duration::from_secs(3)), "刚刚");
        assert_eq!(ago(Duration::from_secs(42)), "42 秒前");
        assert_eq!(ago(Duration::from_secs(185)), "3 分钟前");
        assert_eq!(ago(Duration::from_secs(7300)), "2 小时前");
    }
}
