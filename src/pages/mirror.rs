//! 投屏 page. Mirroring: the picture (stage) and a vertical key strip; the
//! sidebar collapses and the window fits the picture. Not mirroring: start
//! card with the mirroring options.

use super::{card, page_header};
use crate::icons::{Glyph, KeyIcon};
use crate::{PadApp, Snapshot, combo_row, control, theme, widgets};
use eframe::egui::{self, Rect, RichText, vec2};
use std::sync::atomic::Ordering;

/// Width of the key strip beside the picture.
pub const KEYSTRIP_W: f32 = 92.0;

impl PadApp {
    /// While mirroring: picture in `rect` minus the strip on its right.
    pub(crate) fn mirror_view(&mut self, ui: &mut egui::Ui, rect: Rect, snap: &Snapshot) {
        let strip = Rect::from_min_max(egui::pos2(rect.right() - KEYSTRIP_W, rect.top()), rect.max);
        let stage = Rect::from_min_max(rect.min, egui::pos2(strip.left(), rect.bottom()));
        let before = self.last_extent;
        self.stage(ui, stage, snap);
        if let Some(extent) = self.last_extent
            && before.is_some()
            && before != self.last_extent
        {
            self.fit_window(ui.ctx(), extent);
        }
        ui.painter().line_segment(
            [strip.left_top(), strip.left_bottom()],
            egui::Stroke::new(1.0, theme::c().line),
        );
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(strip.shrink2(vec2(8.0, 10.0))),
            |ui| {
                ui.set_clip_rect(strip);
                egui::ScrollArea::vertical()
                    .id_salt("keystrip")
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 6.0;
                        self.key_strip(ui);
                    });
            },
        );
    }

    fn key_strip(&mut self, ui: &mut egui::Ui) {
        let size = vec2(ui.available_width(), 50.0);
        if widgets::key(ui, KeyIcon::Stop, "停止投屏", size, true).clicked() {
            self.stop_mirror();
            return;
        }
        ui.add_space(4.0);
        for (icon, label, code) in [
            (KeyIcon::Back, "返回", 4),
            (KeyIcon::Home, "主页", 3),
            (KeyIcon::Recents, "多任务", 187),
            (KeyIcon::Power, "电源", 26),
            (KeyIcon::VolDown, "音量 −", 25),
            (KeyIcon::VolUp, "音量 +", 24),
        ] {
            if widgets::key(ui, icon, label, size, true).clicked() {
                self.press_key(code);
            }
        }
        ui.add_space(4.0);
        let screen_off = self
            .session()
            .is_some_and(|s| s.shared.screen_off.load(Ordering::Relaxed));
        let (icon, label) = if screen_off {
            (KeyIcon::ScreenOn, "点亮屏幕")
        } else {
            (KeyIcon::ScreenOff, "熄灭屏幕")
        };
        if widgets::key(ui, icon, label, size, true).clicked()
            && let Some(s) = self.session()
        {
            s.set_screen_off(!screen_off);
        }
        if widgets::key(ui, KeyIcon::Notifications, "通知栏", size, true).clicked()
            && let Some(s) = self.session()
        {
            s.send(&control::expand_notification_panel());
        }
        if widgets::key(ui, KeyIcon::Rotate, "旋转", size, true).clicked()
            && let Some(s) = self.session()
        {
            s.send(&control::rotate_device());
        }
        ui.add_space(4.0);
        widgets::switch(ui, &mut self.show_hud, "HUD");
    }

    /// Not mirroring: start card + options.
    pub(crate) fn mirror_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ready = snap.selected.is_some() && self.tools.is_some();
        egui::ScrollArea::vertical()
            .id_salt("mirror-page")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                page_header(
                    ui,
                    "投屏",
                    "低延迟投屏：鼠标 = 触摸，滚轮 = 滑动，右键 = 返回，中键 = 主页，点击画面后键盘直接输入。",
                );
                card(ui, None, |ui| {
                    if matches!(self.mirror, crate::MirrorState::Connecting(_)) {
                        ui.label(RichText::new("正在连接…").color(theme::c().amber));
                    } else if widgets::primary(ui, "▶  开始投屏", ready).clicked() {
                        self.start_mirror(ui.ctx());
                    }
                    if !ready {
                        ui.label(
                            RichText::new("需要已授权的 adb 设备：先在首页连接设备，或用「救援」打开 USB 调试。")
                                .color(theme::c().text_faint),
                        );
                    }
                });
                card(ui, Some((Glyph::Sliders, "投屏参数")), |ui| {
                    self.mirror_options(ui)
                });
            });
    }

    fn mirror_options(&mut self, ui: &mut egui::Ui) {
        combo_row(
            ui,
            "最大分辨率",
            "size",
            &mut self.opts.max_size,
            &[1280, 1600, 1920, 2400, 2800],
            |v| format!("{v}p"),
        );
        combo_row(
            ui,
            "码率",
            "bitrate",
            &mut self.opts.bit_rate,
            &[4_000_000, 8_000_000, 12_000_000, 20_000_000, 32_000_000],
            |v| format!("{} Mbps", v / 1_000_000),
        );
        combo_row(
            ui,
            "帧率上限",
            "fps",
            &mut self.opts.max_fps,
            &[0, 30, 60, 90, 120, 144],
            |v| {
                if v == 0 {
                    "自动".into()
                } else {
                    format!("{v} fps")
                }
            },
        );
        widgets::switch(ui, &mut self.opts.audio, "声音转到电脑（设备静音）");
        widgets::switch(ui, &mut self.opts.stay_awake, "保持唤醒");
        widgets::switch(ui, &mut self.opts.turn_screen_off, "投屏时关闭设备屏幕");
        widgets::switch(ui, &mut self.show_hud, "显示延迟 / 帧率");
    }
}
