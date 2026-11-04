//! 脚本调试 and 脚本说明 pages (the workbench and the guide modules).

use crate::icons::{self, Glyph};
use crate::nav::Page;
use crate::{PadApp, Snapshot, guide, log, profiles, theme, widgets, workbench};
use eframe::egui::{self, Margin, RichText, vec2};

impl PadApp {
    pub(crate) fn script_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        super::page_header(
            ui,
            "脚本调试",
            "编写机型的解锁 / 一键救援脚本：实时检查语法，运行、单步、运行当前行，保存到机型配置。",
        );
        let status = self.rescue.status.lock().unwrap().clone();
        self.script_password_row(ui, snap, status.serial.as_deref());
        ui.add_space(4.0);
        let current = self.profile(snap).cloned();
        // Looked up only when a script runs, not every frame.
        let (typed, store) = (&self.password, &self.passwords);
        let password = || crate::rescue_password_for(typed, store, snap, status.serial.as_deref());
        let cx = workbench::Context {
            rescue: &self.rescue,
            status: &status,
            profiles: &self.profiles,
            current: current.as_ref(),
            password: &password,
            adb_model: snap.selected_model(),
        };
        for action in self.workbench.ui(ui, &cx) {
            match action {
                workbench::Action::Save(profile) => self.save_profile(&profile),
                workbench::Action::Log(line) => log(&self.shared, line),
                workbench::Action::OpenGuide => self.page = Page::Guide,
            }
        }
    }

    /// What `{pw}` will be: a field for the lock-screen password, whether
    /// one is saved for this device, and save / forget. The same password
    /// as the 解锁 cards (typed field shared, saved per device).
    fn script_password_row(&mut self, ui: &mut egui::Ui, snap: &Snapshot, held: Option<&str>) {
        let c = theme::c();
        let device = crate::password_device_id(held, &snap.usb_serials, snap.device_id.as_deref());
        let saved = device
            .as_deref()
            .is_some_and(|id| self.passwords.contains(id));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            let (icon, _) = ui.allocate_exact_size(vec2(22.0, 22.0), egui::Sense::hover());
            icons::tile(ui.painter(), icon, Glyph::Unlock);
            ui.label(RichText::new("{pw} 锁屏密码").font(theme::mono(12.5)).color(c.text))
                .on_hover_text("脚本里的 {pw} 在运行时替换成这里的密码，再由设备输入；脚本和日志里都不会出现明文。");
            let hint = if saved {
                "已保存，留空即用已保存的"
            } else {
                "输入锁屏密码"
            };
            ui.add(
                egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .hint_text(hint)
                    .desired_width(200.0)
                    .margin(Margin::symmetric(8, 5)),
            );
            let typed = !self.password.is_empty();
            match (&device, typed, saved) {
                (Some(id), true, _) => {
                    if ui
                        .button(RichText::new("保存到此设备").color(c.amber))
                        .on_hover_text(format!("加密保存为设备 {id} 的密码"))
                        .clicked()
                    {
                        let typed = std::mem::take(&mut self.password);
                        match self.passwords.set(id, &typed) {
                            Ok(()) => log(&self.shared, format!("✔ 已保存 {id} 的锁屏密码（加密）")),
                            Err(e) => log(&self.shared, format!("✘ 保存密码失败: {e}")),
                        }
                    }
                }
                (Some(id), false, true) => {
                    ui.label(RichText::new("● 已保存").font(theme::ui(12.0)).color(c.green))
                        .on_hover_text(format!("设备 {id}"));
                    if widgets::link(ui, "忘记").clicked() {
                        let id = id.clone();
                        self.forget_password(&id);
                    }
                }
                (None, true, _) => {
                    ui.label(
                        RichText::new("本次运行直接使用；插上设备后可保存")
                            .font(theme::ui(12.0))
                            .color(c.text_dim),
                    );
                }
                // Nothing typed, nothing saved (`saved` needs a device).
                _ => {
                    ui.label(
                        RichText::new("未设置：运行到 {pw} 时会提示")
                            .font(theme::ui(12.0))
                            .color(c.text_faint),
                    );
                }
            }
        });
    }

    pub(crate) fn guide_page(&mut self, ui: &mut egui::Ui) {
        super::page_header(
            ui,
            "脚本说明",
            "救援脚本的概念、命令参考、开发流程、调试方法和示例。",
        );
        if let Some(guide::Action::Insert(text)) = guide::ui(ui) {
            self.workbench.insert(text);
            self.page = Page::Script;
        }
    }

    /// Write one profile into rescue-profiles.txt and reload everything.
    pub(crate) fn save_profile(&mut self, profile: &profiles::Profile) {
        let path = profiles::path();
        match profiles::save(&path, profile) {
            Ok(()) => {
                self.reload_profiles();
                self.workbench.mark_saved(profile);
                log(
                    &self.shared,
                    format!("✔ 已保存机型「{}」到 {}", profile.name, path.display()),
                );
            }
            Err(e) => log(&self.shared, format!("✘ 保存机型配置失败: {e}")),
        }
    }
}
