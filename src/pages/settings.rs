//! 设置 page: appearance and where things live.

use super::{card, page_header};
use crate::{PadApp, bundle, log, open_in_text_editor, profiles, theme, widgets};
use eframe::egui::{self, RichText};

impl PadApp {
    pub(crate) fn settings_page(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("settings-page")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                page_header(ui, "设置", "外观与数据位置。投屏参数在「投屏」页。");
                card(ui, Some("外观"), |ui| {
                    // egui persists the choice.
                    let mut preference = ui.ctx().options(|o| o.theme_preference);
                    let width = ui.available_width().min(420.0);
                    let changed = ui
                        .allocate_ui(egui::vec2(width, 28.0), |ui| {
                            widgets::segmented(
                                ui,
                                &mut preference,
                                &[
                                    (egui::ThemePreference::System, "跟随系统"),
                                    (egui::ThemePreference::Light, "亮色"),
                                    (egui::ThemePreference::Dark, "暗色"),
                                ],
                            )
                        })
                        .inner;
                    if changed {
                        ui.ctx().set_theme(preference);
                    }
                });
                card(ui, Some("数据"), |ui| {
                    let base = bundle::default_base();
                    ui.label(
                        RichText::new(
                            "锁屏密码（加密）、机型配置、已知设备、窗口位置和日志都保存在：",
                        )
                        .color(theme::c().text_dim),
                    );
                    ui.label(
                        RichText::new(base.display().to_string())
                            .font(theme::mono(11.0))
                            .color(theme::c().text),
                    );
                    ui.horizontal(|ui| {
                        if widgets::link(ui, "打开文件夹").clicked()
                            && let Err(e) = open_folder(&base)
                        {
                            log(&self.shared, format!("✘ 打开 {} 失败: {e}", base.display()));
                        }
                        if widgets::link(ui, "打开机型配置").clicked()
                            && let Err(e) = open_in_text_editor(&profiles::path())
                        {
                            log(&self.shared, format!("✘ 打开机型配置失败: {e}"));
                        }
                    });
                });
                card(ui, Some("关于"), |ui| {
                    ui.label(
                        RichText::new(format!(
                            "安卓救援宝 DroidRescue {}",
                            env!("CARGO_PKG_VERSION")
                        ))
                        .color(theme::c().text),
                    );
                    ui.hyperlink_to(
                        "github.com/HeiSir2014/DroidRescue",
                        "https://github.com/HeiSir2014/DroidRescue",
                    );
                });
            });
    }
}

/// Explorer on Windows, Finder on macOS.
fn open_folder(path: &std::path::Path) -> std::io::Result<std::process::Child> {
    let program = if cfg!(windows) { "explorer" } else { "open" };
    std::process::Command::new(program).arg(path).spawn()
}
