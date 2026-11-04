//! 救援 page: drive the device as a USB keyboard / pen (AOA HID) — works
//! with USB debugging off, on the lock screen, with no screen.

use super::{card, page_header};
use crate::nav::Page;
use crate::{
    PadApp, Snapshot, aoa, checkup, log, open_in_text_editor, profiles, rescue, theme, usbhost,
    widgets,
};
use eframe::egui::{self, Margin, RichText, vec2};

impl PadApp {
    pub(crate) fn rescue_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let status = self.rescue.status.lock().unwrap().clone();
        egui::ScrollArea::vertical()
            .id_salt("rescue-page")
            .auto_shrink(false)
            .show(ui, |ui| {
                page_header(
                    ui,
                    "救援",
                    "屏幕坏了、USB 调试没开也能操作：电脑模拟 USB 键盘和触控笔，按机型脚本盲操作。",
                );
                ui.columns(2, |cols| {
                    cols[0].spacing_mut().item_spacing.y = 12.0;
                    cols[1].spacing_mut().item_spacing.y = 12.0;
                    card(&mut cols[0], Some("一键救援"), |ui| {
                        self.rescue_actions(ui, snap, &status);
                    });
                    card(&mut cols[0], Some("手动操作"), |ui| {
                        self.rescue_manual(ui, &status);
                    });
                    card(&mut cols[1], Some("机型配置"), |ui| {
                        self.rescue_profiles_card(ui, &status);
                    });
                    card(&mut cols[1], Some("设备体检"), |ui| {
                        self.checkup_view(ui, snap);
                    });
                });
            });
    }

    fn rescue_actions(&mut self, ui: &mut egui::Ui, snap: &Snapshot, status: &rescue::Status) {
        let gap = ui.spacing().item_spacing.x;
        let (text, color) = match (&status.device, status.busy) {
            (Some(d), true) => (format!("● {d} · 执行中…"), theme::c().amber),
            (Some(d), false) => (format!("● {d}"), theme::c().green),
            (None, true) => ("连接设备中…".to_string(), theme::c().amber),
            (None, false) => ("插上设备即可操作".to_string(), theme::c().text_dim),
        };
        ui.label(RichText::new(text).font(theme::mono(10.5)).color(color));
        if status.needs_setup {
            self.winusb_setup_link(ui);
        }

        // Per-model workflows (profiles.rs): automatic by model, or picked.
        let profile = self.profile(snap).cloned();
        let auto = self.profile_choice.is_none();
        ui.horizontal(|ui| {
            ui.label(RichText::new("机型").color(theme::c().text_dim));
            let shown = match (&profile, auto) {
                (Some(p), true) => format!("自动 · {}", p.name),
                (Some(p), false) => p.name.clone(),
                (None, _) => "—".into(),
            };
            // Truncate: in a horizontal row the default (Extend) lets a long
            // profile name push the page wider than the window.
            egui::ComboBox::from_id_salt("rescue-profile")
                .width(ui.available_width())
                .truncate()
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.profile_choice, None, "自动（按型号）");
                    for p in &self.profiles {
                        let label = if p.models.is_empty() {
                            p.name.clone()
                        } else {
                            format!("{}  [{}]", p.name, p.models.join(", "))
                        };
                        ui.selectable_value(&mut self.profile_choice, Some(p.name.clone()), label);
                    }
                });
        });

        let enable = profile.as_ref().and_then(|p| p.enable.clone());
        if widgets::primary(ui, "一键救援：解锁并开启 USB 调试", !status.busy && enable.is_some())
            .on_hover_text(if enable.is_some() {
                "按所选机型的流程盲操作：解锁 → 选 USB 模式 → 允许 USB 调试。无需在电脑上安装其他软件，完成后可直接投屏。"
            } else {
                "该机型还没有「一键救援」流程：在「脚本调试」页为它编写 enable 脚本"
            })
            .clicked()
            && let Some(script) = &enable
        {
            match self.rescue_password(snap, status.serial.as_deref()) {
                Some(pw) => self.rescue_script("开启调试", script, &[("pw", &pw)]),
                None => log(&self.shared, "✘ 救援: 没有锁屏密码，请先在首页「解锁」里输入"),
            }
        }

        let unlock = profile
            .as_ref()
            .map_or(rescue::SCRIPT_UNLOCK.to_string(), |p| p.unlock.clone());
        let w2 = (ui.available_width() - gap) / 2.0;
        ui.horizontal(|ui| {
            if widgets::key(ui, "WAKE", "唤醒", vec2(w2, 44.0), !status.busy).clicked() {
                self.rescue_script("唤醒", rescue::SCRIPT_WAKE, &[]);
            }
            if widgets::key(ui, "UNLOCK", "解锁", vec2(w2, 44.0), !status.busy).clicked() {
                match self.rescue_password(snap, status.serial.as_deref()) {
                    Some(pw) => self.rescue_script("解锁", &unlock, &[("pw", &pw)]),
                    None => log(
                        &self.shared,
                        "✘ 救援: 没有锁屏密码，请先在首页「解锁」里输入",
                    ),
                }
            }
        });
        ui.separator();
        self.unlock_controls(ui, snap);
    }

    /// Keys, text and keyboard passthrough by hand.
    fn rescue_manual(&mut self, ui: &mut egui::Ui, status: &rescue::Status) {
        use rescue::Step;
        let gap = ui.spacing().item_spacing.x;
        let w4 = (ui.available_width() - gap * 3.0) / 4.0;
        let nav: [[(&str, &str, Step); 4]; 2] = [
            [
                ("↑", "上", Step::Key(0, aoa::KEY_UP)),
                ("↓", "下", Step::Key(0, aoa::KEY_DOWN)),
                ("←", "左", Step::Key(0, aoa::KEY_LEFT)),
                ("→", "右", Step::Key(0, aoa::KEY_RIGHT)),
            ],
            [
                ("TAB", "焦点", Step::Key(0, aoa::KEY_TAB)),
                ("ENTER", "确认", Step::Key(0, aoa::KEY_ENTER)),
                ("BACK", "返回", Step::Consumer(aoa::USAGE_AC_BACK)),
                ("HOME", "主页", Step::Consumer(aoa::USAGE_AC_HOME)),
            ],
        ];
        for row in nav {
            ui.horizontal(|ui| {
                for (glyph, label, step) in row {
                    if widgets::key(ui, glyph, label, vec2(w4, 40.0), !status.busy).clicked() {
                        self.rescue.run("按键", vec![step]);
                    }
                }
            });
        }
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.rescue_text)
                    .hint_text("文字发送到设备（英文/数字/符号）")
                    .desired_width(ui.available_width() - 72.0 - gap)
                    .margin(Margin::symmetric(8, 6)),
            );
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (widgets::key(
                ui,
                "SEND",
                "发送",
                vec2(72.0, 34.0),
                !self.rescue_text.is_empty(),
            )
            .clicked()
                || enter)
                && !self.rescue_text.is_empty()
            {
                self.rescue.run(
                    "输入文字",
                    vec![Step::Text(std::mem::take(&mut self.rescue_text))],
                );
            }
        });
        widgets::switch(
            ui,
            &mut self.rescue_passthrough,
            "键盘直通（电脑键盘 → 设备）",
        );
    }

    /// Profile file, script workbench, driver setup.
    fn rescue_profiles_card(&mut self, ui: &mut egui::Ui, status: &rescue::Status) {
        ui.label(
            RichText::new(
                "每个机型的解锁 / 一键救援流程是一段脚本，可在软件里编写、单步调试并保存。",
            )
            .color(theme::c().text_dim),
        );
        if ui
            .button(RichText::new("编写 / 调试机型脚本 →").color(theme::c().amber))
            .clicked()
        {
            self.page = Page::Script;
        }
        ui.horizontal_wrapped(|ui| {
            if widgets::link(ui, "用文本编辑器打开配置文件")
                .on_hover_text(profiles::path().display().to_string())
                .clicked()
                && let Err(e) = open_in_text_editor(&profiles::path())
            {
                log(&self.shared, format!("✘ 打开机型配置失败: {e}"));
            }
            if widgets::link(ui, "重新加载").clicked() {
                self.reload_profiles();
                log(
                    &self.shared,
                    format!("✔ 机型配置: {} 个", self.profiles.len()),
                );
            }
            if status.device.is_some() && widgets::link(ui, "释放设备").clicked() {
                self.rescue.release();
            }
        });
        self.winusb_setup_link(ui);
    }

    /// USB functions as the OS sees them, and whether the rescue can use them.
    pub(crate) fn checkup_view(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let c = theme::c();
        let adb: Vec<String> = snap
            .devices
            .iter()
            .map(|d| format!("{} {} {}", d.serial, d.state, d.model))
            .collect();
        ui.horizontal(|ui| {
            if widgets::link(ui, "扫描").clicked() || self.checkup.is_none() {
                let targets = profiles::usb_targets(&self.profiles);
                self.checkup = Some(checkup::rows(&usbhost::survey(), &targets));
            }
            if let Some(rows) = &self.checkup
                && widgets::link(ui, "复制报告").clicked()
            {
                ui.ctx().copy_text(checkup::report(rows, &adb));
                log(&self.shared, "✔ 设备体检报告已复制");
            }
        });
        let Some(rows) = &self.checkup else { return };
        ui.label(RichText::new(checkup::summary(rows)).color(c.text_dim));
        for r in rows {
            let color = match r.role {
                checkup::Role::Ready => c.green,
                checkup::Role::NeedsDriver => c.amber,
                checkup::Role::Adb | checkup::Role::Mtp => c.text,
                checkup::Role::Other => c.text_faint,
            };
            let service = if r.service.is_empty() {
                "无驱动"
            } else {
                &r.service
            };
            ui.label(
                RichText::new(format!(
                    "{:<12} {:<4} {}",
                    r.target.to_string(),
                    checkup::role_label(r.role),
                    service
                ))
                .font(theme::mono(10.5))
                .color(color),
            )
            .on_hover_text(&r.description);
        }
        let adb_line = if adb.is_empty() {
            "adb: 无设备".to_string()
        } else {
            format!("adb: {}", adb.join(" · "))
        };
        ui.label(
            RichText::new(adb_line)
                .font(theme::mono(10.5))
                .color(c.text_dim),
        );
    }

    fn winusb_setup_link(&self, ui: &mut egui::Ui) {
        if !cfg!(windows) {
            return; // macOS needs no driver
        }
        if widgets::link(ui, "为救援接口安装 WinUSB（需管理员）")
            .on_hover_text("使用 Windows 自带、微软签名的 WinUsb 驱动，只作用于机型配置里声明的救援接口（如 Pad3 的虚拟光驱接口）；MTP 文件传输不受影响。设备需插着。开始救援时也会自动请求。")
            .clicked()
        {
            // Waits for the UAC prompt and the install: off the UI thread.
            let targets = profiles::usb_targets(&self.profiles);
            let shared = self.shared.clone();
            log(&shared, "… 正在请求管理员权限安装 WinUSB —— 请在电脑上点“是”");
            std::thread::spawn(move || match usbhost::install_elevated(&targets) {
                Ok(()) => log(&shared, "✔ WinUSB 已安装，可以开始救援"),
                Err(e) => log(&shared, format!("✘ {e}")),
            });
        }
    }
}
