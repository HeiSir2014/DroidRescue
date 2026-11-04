//! 工具 page: device utilities that need adb.

use super::{card, page_header};
use crate::icons::{Glyph, KeyIcon};
use crate::{PadApp, Snapshot, theme, widgets};
use eframe::egui::{self, Margin, RichText, vec2};

impl PadApp {
    pub(crate) fn tools_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ready = snap.selected.is_some() && self.tools.is_some();
        egui::ScrollArea::vertical()
            .id_salt("tools-page")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                page_header(ui, "工具", "需要已授权的 adb 设备。");
                ui.columns(2, |cols| {
                    card(&mut cols[0], Some((Glyph::Wifi, "无线 ADB")), |ui| {
                        ui.label(
                            RichText::new("在设备上开启 5555 端口的无线调试并自动连接，之后可以拔掉数据线（需在同一局域网）。")
                                .color(theme::c().text_dim),
                        );
                        if widgets::key(ui, KeyIcon::Wifi, "开启无线 ADB", vec2(180.0, 36.0), ready)
                            .clicked()
                        {
                            self.enable_wireless();
                        }
                    });
                    card(&mut cols[1], Some((Glyph::Bolt, "插电常亮")), |ui| {
                        ui.label(
                            RichText::new("连着电源时屏幕不自动锁定，适合长时间投屏或调试。")
                                .color(theme::c().text_dim),
                        );
                        ui.add_enabled_ui(ready, |ui| {
                            if widgets::switch(ui, &mut self.stay_on, "插电常亮（不锁屏）").changed() {
                                let value = if self.stay_on { 7 } else { 0 };
                                self.shell_task(
                                    "插电常亮",
                                    format!("settings put global stay_on_while_plugged_in {value}"),
                                );
                            }
                        });
                    });
                });
                card(ui, Some((Glyph::Export, "导出照片 / 文件")), |ui| {
                    ui.label(
                        RichText::new("把设备上的相册、下载、文档等目录复制到电脑上的这个文件夹：")
                            .color(theme::c().text_dim),
                    );
                    ui.add(
                        egui::TextEdit::singleline(&mut self.backup_dir)
                            .desired_width(f32::INFINITY)
                            .margin(Margin::symmetric(8, 6))
                            .font(theme::mono(11.0)),
                    );
                    if widgets::key(ui, KeyIcon::Save, "开始导出", vec2(180.0, 36.0), ready)
                        .clicked()
                    {
                        self.backup();
                    }
                });
            });
    }
}
