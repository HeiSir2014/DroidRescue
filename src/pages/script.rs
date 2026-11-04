//! 脚本调试 and 脚本说明 pages (the workbench and the guide modules).

use crate::nav::Page;
use crate::{PadApp, Snapshot, guide, log, profiles, workbench};
use eframe::egui;

impl PadApp {
    pub(crate) fn script_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        super::page_header(
            ui,
            "脚本调试",
            "编写机型的解锁 / 一键救援脚本：实时检查语法，运行、单步、运行当前行，保存到机型配置。",
        );
        let status = self.rescue.status.lock().unwrap().clone();
        let password = self.rescue_password(snap, status.serial.as_deref());
        let current = self.profile(snap).cloned();
        let cx = workbench::Context {
            rescue: &self.rescue,
            status: &status,
            profiles: &self.profiles,
            current: current.as_ref(),
            password,
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
            Err(e) => log(
                &self.shared,
                format!("✘ 保存机型配置失败 {}: {e}", path.display()),
            ),
        }
    }
}
