//! 连接设备 dialog: how to get a device onto adb — USB, wireless (connect /
//! Android 11+ pairing) — and, for broken screens, the way to 救援.

use crate::nav::Page;
use crate::{PadApp, Snapshot, log_result, theme, widgets};
use eframe::egui::{
    self, Color32, Margin, Painter, Pos2, Rect, RichText, Stroke, StrokeKind, pos2, vec2,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectTab {
    Usb,
    Wireless,
    Broken,
}

/// Inputs of the wireless tab.
#[derive(Default)]
pub struct ConnectForm {
    pub address: String,
    pub pair_address: String,
    pub pair_code: String,
}

/// `host:port` with a numeric port (adb's own syntax).
fn valid_address(text: &str) -> bool {
    let Some((host, port)) = text.trim().rsplit_once(':') else {
        return false;
    };
    !host.is_empty() && port.parse::<u16>().is_ok_and(|p| p > 0)
}

impl PadApp {
    pub(crate) fn connect_dialog(&mut self, ctx: &egui::Context, snap: &Snapshot) {
        let Some(mut tab) = self.connect else { return };
        let c = theme::c();
        let modal = egui::Modal::new(egui::Id::new("connect-dialog"))
            .backdrop_color(Color32::from_black_alpha(90))
            .frame(
                egui::Frame::NONE
                    .fill(c.surface)
                    .stroke(Stroke::new(1.0, c.line))
                    .corner_radius(14)
                    .inner_margin(Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_width(560.0);
                ui.spacing_mut().item_spacing.y = 10.0;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("连接设备")
                            .font(theme::strong_font(18.0))
                            .color(c.text),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::link(ui, "关闭").clicked() {
                            self.connect = None;
                        }
                    });
                });
                ui.allocate_ui(vec2(ui.available_width(), 30.0), |ui| {
                    widgets::segmented(
                        ui,
                        &mut tab,
                        &[
                            (ConnectTab::Usb, "USB 连接"),
                            (ConnectTab::Wireless, "无线连接"),
                            (ConnectTab::Broken, "屏幕坏了 / 调试没开"),
                        ],
                    );
                });
                ui.add_space(6.0);
                match tab {
                    ConnectTab::Usb => self.connect_usb(ui, snap),
                    ConnectTab::Wireless => self.connect_wireless(ui),
                    ConnectTab::Broken => self.connect_broken(ui),
                }
            });
        if self.connect.is_some() {
            self.connect = Some(tab);
        }
        if modal.should_close() {
            self.connect = None;
        }
    }

    fn connect_usb(&mut self, ui: &mut egui::Ui, snap: &Snapshot) {
        let c = theme::c();
        let (art, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), 120.0), egui::Sense::hover());
        draw_usb_illustration(ui.painter(), art);
        ui.label(
            RichText::new("用 USB 数据线把手机或平板连到电脑，解锁屏幕，在设备上打开「开发者选项 → USB 调试」，弹出「允许 USB 调试吗？」时点「允许」。")
                .color(c.text),
        );
        let (status, color) = match (
            snap.devices.iter().find(|d| d.is_ready()),
            snap.devices.iter().find(|d| d.state == "unauthorized"),
        ) {
            (Some(d), _) => (format!("✔ 已连接：{}", snap.name_of(d)), c.green),
            (None, Some(d)) => (
                format!("● {} 等待授权：请在设备上点「允许」", d.serial),
                c.amber,
            ),
            (None, None) => ("● 等待设备…".to_string(), c.text_dim),
        };
        ui.label(RichText::new(status).font(theme::mono(11.0)).color(color));
        egui::CollapsingHeader::new(RichText::new("接入后未连接？").color(c.amber))
            .id_salt("connect-usb-help")
            .show(ui, |ui| {
                for line in [
                    "换一根能传数据的数据线，或换一个 USB 口（有的线只能充电）。",
                    "设备弹出「USB 用途」时选「传输文件」，有的设备在这个模式下才会询问调试授权。",
                    "「允许 USB 调试吗？」勾选「一律允许使用这台计算机进行调试」，再点「允许」。",
                    "还是没有：在开发者选项里点「撤销 USB 调试授权」，重新插拔。",
                    "Windows 识别不到：安装设备厂商的 USB 驱动或 Google USB Driver 后重新插拔。",
                ] {
                    ui.horizontal_top(|ui| {
                        ui.label(RichText::new("·").color(c.amber));
                        ui.add(egui::Label::new(RichText::new(line).color(c.text_dim)).wrap());
                    });
                }
            });
    }

    fn connect_wireless(&mut self, ui: &mut egui::Ui) {
        let c = theme::c();
        ui.label(
            RichText::new("设备和电脑在同一局域网。已用 USB 连过：在「工具 → 无线 ADB」一键开启。Android 11 以上也可在「开发者选项 → 无线调试」里配对：")
                .color(c.text),
        );
        let field = |ui: &mut egui::Ui, text: &mut String, hint: &str, w: f32| {
            ui.add(
                egui::TextEdit::singleline(text)
                    .hint_text(hint)
                    .font(theme::mono(12.0))
                    .desired_width(w)
                    .margin(Margin::symmetric(8, 6)),
            );
        };
        ui.label(RichText::new("1. 配对（只需一次）").color(c.text_dim));
        ui.horizontal(|ui| {
            field(
                ui,
                &mut self.connect_form.pair_address,
                "配对地址 192.168.1.8:37099",
                230.0,
            );
            field(ui, &mut self.connect_form.pair_code, "配对码", 110.0);
            let ok = valid_address(&self.connect_form.pair_address)
                && self.connect_form.pair_code.trim().len() == 6;
            if ui.add_enabled(ok, egui::Button::new("配对")).clicked() {
                let (addr, code) = (
                    self.connect_form.pair_address.trim().to_string(),
                    self.connect_form.pair_code.trim().to_string(),
                );
                self.run_task("无线配对", move |t, _, sh| {
                    log_result(
                        sh,
                        &format!("adb pair {addr}"),
                        t.run(None, &["pair", &addr, &code]),
                    );
                });
            }
        });
        ui.label(RichText::new("2. 连接").color(c.text_dim));
        ui.horizontal(|ui| {
            field(
                ui,
                &mut self.connect_form.address,
                "IP:端口，如 192.168.1.8:5555",
                350.0,
            );
            let ok = valid_address(&self.connect_form.address);
            if ui.add_enabled(ok, egui::Button::new("连接")).clicked() {
                let addr = self.connect_form.address.trim().to_string();
                self.run_task("无线连接", move |t, _, sh| {
                    log_result(
                        sh,
                        &format!("adb connect {addr}"),
                        t.run(None, &["connect", &addr]),
                    );
                });
            }
        });
        ui.label(RichText::new("结果显示在日志里；连上后设备会出现在首页。").color(c.text_faint));
    }

    fn connect_broken(&mut self, ui: &mut egui::Ui) {
        let c = theme::c();
        ui.label(
            RichText::new("屏幕碎了、看不见或摸不了，USB 调试也没开？不用拆机：安卓救援宝把电脑模拟成 USB 键盘和触控笔，按机型脚本盲操作——解锁、选 USB 模式、允许调试，完成后就能投屏操作。")
                .color(c.text),
        );
        ui.label(
            RichText::new("内置 vivo Pad3 的完整流程；其他机型可在「脚本调试」里编写自己的流程。")
                .color(c.text_dim),
        );
        if widgets::primary(ui, "去救援", true).clicked() {
            self.connect = None;
            self.page = Page::Rescue;
        }
    }
}

/// PC ⇄ device line drawing for the USB tab.
fn draw_usb_illustration(p: &Painter, r: Rect) {
    let c = theme::c();
    let stroke = Stroke::new(2.0, c.text_dim);
    let mid = r.center();
    // Monitor.
    let screen = Rect::from_center_size(pos2(mid.x - 120.0, mid.y - 8.0), vec2(110.0, 72.0));
    p.rect_stroke(screen, 6.0, stroke, StrokeKind::Middle);
    p.line_segment(
        [
            pos2(screen.center().x, screen.bottom()),
            pos2(screen.center().x, screen.bottom() + 16.0),
        ],
        stroke,
    );
    p.line_segment(
        [
            pos2(screen.center().x - 26.0, screen.bottom() + 16.0),
            pos2(screen.center().x + 26.0, screen.bottom() + 16.0),
        ],
        stroke,
    );
    // Phone.
    let phone = Rect::from_center_size(pos2(mid.x + 120.0, mid.y), vec2(52.0, 96.0));
    p.rect_stroke(phone, 9.0, stroke, StrokeKind::Middle);
    p.line_segment(
        [
            pos2(phone.center().x - 8.0, phone.bottom() - 8.0),
            pos2(phone.center().x + 8.0, phone.bottom() - 8.0),
        ],
        stroke,
    );
    // Link: two rounded halves in the accent colour.
    let link = |p: &Painter, at: Pos2, color| {
        p.rect_stroke(
            Rect::from_center_size(at, vec2(26.0, 14.0)),
            7.0,
            Stroke::new(3.0, color),
            StrokeKind::Middle,
        );
    };
    link(p, pos2(mid.x - 10.0, mid.y), c.amber);
    link(p, pos2(mid.x + 10.0, mid.y), c.green);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wireless_addresses_need_host_and_port() {
        assert!(valid_address("192.168.1.8:5555"));
        assert!(valid_address(" 10.0.0.2:37099 "));
        assert!(!valid_address("192.168.1.8"));
        assert!(!valid_address(":5555"));
        assert!(!valid_address("192.168.1.8:0"));
        assert!(!valid_address("192.168.1.8:port"));
    }
}
