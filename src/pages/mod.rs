//! The app's pages (one per sidebar entry) and the connect dialog. Each is
//! an `impl PadApp` block: child modules of the crate root may use its
//! private state, so pages stay thin views over the app's existing logic.

mod connect;
mod home;
mod mirror;
mod rescue;
mod script;
mod settings;
mod tools;

pub use connect::{ConnectForm, ConnectTab};
pub use mirror::KEYSTRIP_W;

use crate::icons::{self, Glyph};
use crate::theme;
use eframe::egui::{self, Margin, RichText, Sense, vec2};

/// Card radius (widgets keep the smaller `theme::RADIUS`).
pub const CARD_RADIUS: u8 = 12;

/// A white (dark: graphite) rounded card with an optional title row: the
/// feature's coloured icon tile, then the title.
pub fn card<R>(
    ui: &mut egui::Ui,
    title: Option<(Glyph, &str)>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    egui::Frame::NONE
        .fill(theme::c().surface)
        .stroke(egui::Stroke::new(1.0, theme::c().line))
        .corner_radius(CARD_RADIUS)
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            if let Some((glyph, title)) = title {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    let (rect, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
                    icons::tile(ui.painter(), rect, glyph);
                    ui.label(
                        RichText::new(title)
                            .font(theme::strong_font(16.0))
                            .color(theme::c().text),
                    );
                });
                ui.add_space(2.0);
            }
            add(ui)
        })
        .inner
}

/// A small rounded status tag ("USB 连接", "已授权" …).
pub fn tag(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::NONE
        .fill(color.gamma_multiply(0.14))
        .corner_radius(6)
        .inner_margin(Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).font(theme::ui(11.5)).color(color));
        });
}

/// A page heading and one-line description.
pub fn page_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(
        RichText::new(title)
            .font(theme::strong_font(22.0))
            .color(theme::c().text),
    );
    ui.label(RichText::new(subtitle).color(theme::c().text_dim));
    ui.add_space(6.0);
}
