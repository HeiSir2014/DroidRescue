//! 首页: the connected device at a glance — name, status tags, the common
//! actions and a drawing of the device — then unlocking and system keys.

use super::{ConnectTab, card, tag};
use crate::nav::{self, Page};
use crate::{PadApp, Snapshot, rescue, skin, theme, widgets};
use eframe::egui::{self, Align, Layout, Margin, Rect, RichText, Sense, vec2};

/// Height of the device drawing in the hero card.
const HERO_H: f32 = 250.0;

impl PadApp {
    pub(crate) fn home_page(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        egui::ScrollArea::vertical()
            .id_salt("home-page")
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 12.0;
                card(ui, None, |ui| self.hero(ui, snap));
                ui.columns(2, |cols| {
                    card(&mut cols[0], Some("解锁"), |ui| {
                        ui.label(
                            RichText::new("锁屏密码可提前输入；有 adb 时经 adb 输入，没有时经 USB 模拟键盘输入。")
                                .color(theme::c().text_dim),
                        );
                        self.unlock_controls(ui, snap);
                    });
                    card(&mut cols[1], Some("按键"), |ui| self.system_keys(ui, snap));
                });
            });
    }

    fn hero(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let c = theme::c();
        // Device chips: switch between attached adb devices.
        ui.horizontal_wrapped(|ui| {
            for d in &snap.devices {
                let selected = snap.selected.as_deref() == Some(d.serial.as_str());
                // Selected: accent; others: coloured by adb state.
                let color = match (selected, d.state.as_str()) {
                    (true, _) => c.amber,
                    (false, "device") => c.text_dim,
                    (false, "unauthorized") => c.red,
                    _ => c.text_faint,
                };
                let text = RichText::new(format!("●  {}", snap.name_of(d))).color(color);
                let resp = ui.add_enabled(d.is_ready(), egui::Button::selectable(selected, text));
                if resp.clicked() {
                    self.shared.lock().unwrap().selected = Some(d.serial.clone());
                }
            }
            if widgets::link(ui, "＋ 连接设备").clicked() {
                self.connect = Some(ConnectTab::Usb);
            }
        });
        ui.add_space(4.0);

        let selected = snap
            .devices
            .iter()
            .find(|d| Some(&d.serial) == snap.selected.as_ref());
        let ready = selected.is_some() && self.tools.is_some();
        let art_w = 200.0;
        ui.horizontal_top(|ui| {
            let text_w = (ui.available_width() - art_w - 16.0).max(200.0);
            ui.allocate_ui_with_layout(vec2(text_w, HERO_H), Layout::top_down(Align::Min), |ui| {
                ui.set_width(text_w);
                ui.add_space(12.0);
                match selected {
                    Some(d) => {
                        ui.label(
                            RichText::new(snap.name_of(d))
                                .font(theme::strong_font(30.0))
                                .color(c.text),
                        );
                        ui.horizontal_wrapped(|ui| {
                            let wifi = d.serial.contains(':');
                            tag(ui, if wifi { "无线连接" } else { "USB 连接" }, c.green);
                            match snap.awake {
                                Some(true) => tag(ui, "亮屏", c.green),
                                Some(false) => tag(ui, "熄屏", c.amber),
                                None => {}
                            }
                            match snap.locked {
                                Some(true) => tag(ui, "已锁屏", c.amber),
                                Some(false) => tag(ui, "已解锁", c.green),
                                None => {}
                            }
                            if self.mirroring() {
                                tag(ui, "投屏中", c.amber);
                            }
                        });
                        ui.label(
                            RichText::new(format!("{} · {}", d.model, d.serial))
                                .font(theme::mono(10.5))
                                .color(c.text_faint),
                        );
                    }
                    None => {
                        ui.label(
                            RichText::new("未连接设备")
                                .font(theme::strong_font(30.0))
                                .color(c.text),
                        );
                        if let Some(d) = snap.devices.iter().find(|d| d.state == "unauthorized") {
                            ui.horizontal(|ui| {
                                tag(ui, "未授权", c.red);
                                ui.label(
                                    RichText::new(format!("{} · 请在设备上允许 USB 调试", d.serial))
                                        .color(c.text_dim),
                                );
                            });
                        } else {
                            ui.label(
                                RichText::new("用数据线连接手机或平板，并在设备上允许 USB 调试。屏幕坏了或调试没开时，用「救援」。")
                                    .color(c.text_dim),
                            );
                        }
                    }
                }
                ui.add_space(14.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let size = vec2(132.0, 40.0);
                    if ready {
                        let (label, live) = if self.mirroring() {
                            ("查看投屏", true)
                        } else {
                            ("▶  手机投屏", false)
                        };
                        if action(ui, label, size, true, true).clicked() {
                            if !live {
                                self.start_mirror(ui.ctx());
                            }
                            self.page = Page::Mirror;
                        }
                        if action(ui, "无线 ADB", size, false, true).clicked() {
                            self.enable_wireless();
                        }
                        if action(ui, "导出照片/文件", size, false, true).clicked() {
                            self.backup();
                        }
                    } else {
                        if action(ui, "连接设备", size, true, true).clicked() {
                            self.connect = Some(ConnectTab::Usb);
                        }
                        if action(ui, "屏幕坏了？去救援", size, false, true).clicked() {
                            self.page = Page::Rescue;
                        }
                    }
                });
            });
            let (art, _) = ui.allocate_exact_size(vec2(art_w, HERO_H), Sense::hover());
            draw_device(ui, art, snap, ready);
        });
    }

    /// Password field, unlock (adb or USB keyboard), remember / forget.
    /// Works before anything is connected: the password is kept for later.
    pub(crate) fn unlock_controls(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ready = snap.selected.is_some() && self.tools.is_some();
        let gap = ui.spacing().item_spacing.x;
        let route = rescue::unlock_route(ready, &snap.usb_serials);
        let device_id = match &route {
            rescue::UnlockRoute::Adb => snap.device_id.clone(),
            rescue::UnlockRoute::Usb(serial) => Some(serial.clone()),
            rescue::UnlockRoute::None => None,
        };
        let device_id = device_id.as_deref();
        let saved = device_id.is_some_and(|id| self.passwords.contains(id));
        ui.horizontal(|ui| {
            let field_w = ui.available_width() - 72.0 - gap;
            let hint = match (&route, saved) {
                (_, true) => "已保存 · 留空直接解锁",
                (rescue::UnlockRoute::None, _) => "锁屏密码（可先输入，插上后使用）",
                _ => "锁屏密码",
            };
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.password)
                    .password(true)
                    .hint_text(hint)
                    .desired_width(field_w)
                    .margin(Margin::symmetric(8, 6)),
            );
            let can_unlock =
                route != rescue::UnlockRoute::None && (saved || !self.password.is_empty());
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let clicked = widgets::key(ui, "ENTER", "解锁", vec2(72.0, 34.0), can_unlock).clicked();
            if (clicked || enter) && can_unlock {
                let script = self
                    .profile(snap)
                    .map_or(rescue::SCRIPT_UNLOCK.to_string(), |p| p.unlock.clone());
                self.unlock(device_id, &route, &script);
            }
        });
        let via = match &route {
            rescue::UnlockRoute::Adb => "经 adb 输入",
            rescue::UnlockRoute::Usb(_) => "经 USB 模拟键盘（无需调试）",
            rescue::UnlockRoute::None if snap.usb_serials.len() > 1 => {
                "检测到多台设备，请只连接一台"
            }
            rescue::UnlockRoute::None => "未连接设备",
        };
        ui.label(
            RichText::new(via)
                .font(theme::mono(10.0))
                .color(theme::c().text_faint),
        );
        ui.horizontal(|ui| {
            widgets::switch_inline(ui, &mut self.remember_password, "记住此设备的密码");
            if saved && let Some(id) = device_id {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if widgets::link(ui, "忘记密码").clicked() {
                        self.forget_password(id);
                    }
                    ui.label(
                        RichText::new("● 已保存")
                            .font(theme::mono(10.5))
                            .color(theme::c().green),
                    );
                });
            }
        });
    }

    /// Android system keys over adb (also on the mirroring key strip).
    fn system_keys(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let ready = snap.selected.is_some() && self.tools.is_some();
        let gap = ui.spacing().item_spacing.x;
        let w3 = (ui.available_width() - gap * 2.0) / 3.0;
        ui.horizontal(|ui| {
            for (glyph, label, code) in [("◁", "返回", 4), ("○", "主页", 3), ("▢", "多任务", 187)]
            {
                if widgets::key(ui, glyph, label, vec2(w3, 48.0), ready).clicked() {
                    self.press_key(code);
                }
            }
        });
        let w4 = (ui.available_width() - gap * 3.0) / 4.0;
        ui.horizontal(|ui| {
            for (glyph, label, code) in [
                ("PWR", "电源", 26),
                ("VOL−", "音量", 25),
                ("VOL+", "音量", 24),
                ("WAKE", "唤醒", 224),
            ] {
                if widgets::key(ui, glyph, label, vec2(w4, 48.0), ready).clicked() {
                    self.press_key(code);
                }
            }
        });
        if !ready {
            ui.label(
                RichText::new("需要已授权的 adb 设备；没有调试时用「救援」里的手动操作。")
                    .color(theme::c().text_faint),
            );
        }
    }
}

/// A hero action button: primary = amber fill.
fn action(
    ui: &mut egui::Ui,
    label: &str,
    size: egui::Vec2,
    primary: bool,
    enabled: bool,
) -> egui::Response {
    let c = theme::c();
    let (fill, text) = if primary {
        (c.amber, c.on_amber)
    } else {
        (c.surface_hi, c.text)
    };
    ui.add_enabled(
        enabled,
        egui::Button::new(RichText::new(label).color(text))
            .fill(fill)
            .corner_radius(8)
            .min_size(size),
    )
}

/// The selected device (or a generic one) as drawn hardware.
fn draw_device(ui: &egui::Ui, rect: Rect, snap: &Snapshot, connected: bool) {
    let model = skin::Model::for_device(snap.selected_model(), (0, 0), snap.orientation);
    let turns = skin::turns_for(&model, snap.orientation, None);
    let layout = skin::layout(rect.shrink(8.0), &model, turns);
    let p = ui.painter_at(rect);
    skin::paint::back(&p, &layout, None, None, false);
    skin::paint::front(&p, &layout);
    let color = if connected {
        theme::c().amber
    } else {
        theme::c().text_faint
    };
    let size = (layout.screen.width().min(layout.screen.height()) * 0.28).clamp(16.0, 48.0);
    nav::icon(&p, Page::Rescue, layout.screen.center(), size, color);
}
