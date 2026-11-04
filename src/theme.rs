//! "Instrument rail" theme: graphite surfaces, one amber signal accent,
//! Maple Mono CN everywhere (CJK : Latin = 2 : 1, so readouts align),
//! Chakra Petch only for the wordmark.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals,
};
use std::sync::Arc;

pub const BG: Color32 = Color32::from_rgb(0x0F, 0x10, 0x12);
pub const SURFACE: Color32 = Color32::from_rgb(0x16, 0x18, 0x1B);
pub const SURFACE_HI: Color32 = Color32::from_rgb(0x1E, 0x21, 0x25);
pub const SURFACE_HOVER: Color32 = Color32::from_rgb(0x27, 0x2B, 0x30);
pub const WELL: Color32 = Color32::from_rgb(0x0A, 0x0B, 0x0C);
pub const LINE: Color32 = Color32::from_rgb(0x2A, 0x2E, 0x33);
pub const TEXT: Color32 = Color32::from_rgb(0xE9, 0xE6, 0xDF);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8D, 0x92, 0x99);
pub const TEXT_FAINT: Color32 = Color32::from_rgb(0x5B, 0x60, 0x67);
pub const AMBER: Color32 = Color32::from_rgb(0xFF, 0xB2, 0x24);
pub const AMBER_HI: Color32 = Color32::from_rgb(0xFF, 0xC6, 0x5C);
pub const AMBER_LO: Color32 = Color32::from_rgb(0x3A, 0x2B, 0x0E);
pub const GREEN: Color32 = Color32::from_rgb(0x3D, 0xDC, 0x84);
pub const RED: Color32 = Color32::from_rgb(0xFF, 0x5C, 0x4D);

pub const RADIUS: u8 = 3;

pub fn display_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("display".into()))
}

pub fn strong_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("strong".into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    // Always dark, regardless of the OS light/dark setting.
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        use TextStyle::*;
        style.text_styles = [
            (Heading, FontId::new(17.0, FontFamily::Proportional)),
            (Body, FontId::new(13.0, FontFamily::Proportional)),
            (Button, FontId::new(13.0, FontFamily::Proportional)),
            (Small, FontId::new(10.5, FontFamily::Proportional)),
            (Monospace, FontId::new(11.5, FontFamily::Monospace)),
        ]
        .into();

        let s = &mut style.spacing;
        s.item_spacing = egui::vec2(8.0, 8.0);
        s.button_padding = egui::vec2(12.0, 6.0);
        s.interact_size.y = 30.0;
        s.window_margin = egui::Margin::same(0);
        s.scroll.bar_width = 6.0;
        s.scroll.floating = true;

        style.visuals = visuals();
    });
}

fn visuals() -> Visuals {
    let mut v = Visuals::dark();
    let r = CornerRadius::same(RADIUS);
    v.panel_fill = BG;
    v.window_fill = SURFACE;
    v.extreme_bg_color = WELL;
    v.faint_bg_color = SURFACE;
    v.window_stroke = Stroke::new(1.0, LINE);
    v.hyperlink_color = AMBER;
    v.selection.bg_fill = AMBER_LO;
    v.selection.stroke = Stroke::new(1.0, AMBER);
    v.text_cursor.stroke = Stroke::new(2.0, AMBER);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = SURFACE;
    w.noninteractive.weak_bg_fill = SURFACE;
    w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_DIM);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = SURFACE_HI;
    w.inactive.weak_bg_fill = SURFACE_HI;
    w.inactive.bg_stroke = Stroke::new(1.0, LINE);
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = SURFACE_HOVER;
    w.hovered.weak_bg_fill = SURFACE_HOVER;
    w.hovered.bg_stroke = Stroke::new(1.0, AMBER.gamma_multiply(0.55));
    w.hovered.fg_stroke = Stroke::new(1.0, TEXT);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = AMBER_LO;
    w.active.weak_bg_fill = AMBER_LO;
    w.active.bg_stroke = Stroke::new(1.0, AMBER);
    w.active.fg_stroke = Stroke::new(1.0, AMBER_HI);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;

    w.open = w.active;
    v
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let add = |fonts: &mut FontDefinitions, name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.into(), Arc::new(FontData::from_static(bytes)));
    };
    add(
        &mut fonts,
        "maple",
        include_bytes!("../assets/fonts/MapleMonoCN-Regular.ttf"),
    );
    add(
        &mut fonts,
        "maple-sb",
        include_bytes!("../assets/fonts/MapleMonoCN-SemiBold.ttf"),
    );
    add(
        &mut fonts,
        "chakra",
        include_bytes!("../assets/fonts/ChakraPetch-SemiBold.ttf"),
    );

    // Rare CJK outside the GB2312 subset falls back to the system font.
    let system_cjk = [r"C:\Windows\Fonts\msyh.ttc", r"C:\Windows\Fonts\simhei.ttf"]
        .iter()
        .find_map(|p| std::fs::read(p).ok());
    if let Some(bytes) = system_cjk {
        fonts
            .font_data
            .insert("system-cjk".into(), Arc::new(FontData::from_owned(bytes)));
    }

    // Chain: primary → Maple (CJK) → system CJK → egui's bundled symbol/emoji fonts.
    let egui_fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for (family, first) in [
        (FontFamily::Proportional, "maple"),
        (FontFamily::Monospace, "maple"),
        (FontFamily::Name("strong".into()), "maple-sb"),
        (FontFamily::Name("display".into()), "chakra"),
    ] {
        let mut chain: Vec<String> = vec![first.into(), "maple".into(), "system-cjk".into()];
        chain.extend(egui_fallbacks.iter().cloned());
        chain.dedup();
        chain.retain(|name| fonts.font_data.contains_key(name));
        fonts.families.insert(family, chain);
    }
    ctx.set_fonts(fonts);
}
