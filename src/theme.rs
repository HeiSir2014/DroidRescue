//! "Instrument rail" theme: graphite (dark) or paper (light) surfaces, one
//! amber signal accent, Maple Mono CN everywhere (CJK : Latin = 2 : 1, so
//! readouts align).
//!
//! Colours are tokens in a [`Palette`]; [`c()`] returns the palette of the
//! theme egui is showing this frame (system / light / dark, chosen by the
//! user and persisted by egui). Device bodies in `skin` stay dark in both
//! themes — they depict real hardware.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Theme, Visuals,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, PartialEq, Eq)]
pub struct Palette {
    pub bg: Color32,
    pub surface: Color32,
    pub surface_hi: Color32,
    pub surface_hover: Color32,
    /// Recessed areas: stage, log, inputs.
    pub well: Color32,
    pub line: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    /// The single accent; also the primary button fill.
    pub amber: Color32,
    pub amber_hi: Color32,
    pub amber_lo: Color32,
    /// Text on an `amber` fill.
    pub on_amber: Color32,
    pub green: Color32,
    pub red: Color32,
}

pub const DARK: Palette = Palette {
    bg: Color32::from_rgb(0x0F, 0x10, 0x12),
    surface: Color32::from_rgb(0x16, 0x18, 0x1B),
    surface_hi: Color32::from_rgb(0x1E, 0x21, 0x25),
    surface_hover: Color32::from_rgb(0x27, 0x2B, 0x30),
    well: Color32::from_rgb(0x0A, 0x0B, 0x0C),
    line: Color32::from_rgb(0x2A, 0x2E, 0x33),
    text: Color32::from_rgb(0xE9, 0xE6, 0xDF),
    text_dim: Color32::from_rgb(0x8D, 0x92, 0x99),
    text_faint: Color32::from_rgb(0x6E, 0x73, 0x7A),
    amber: Color32::from_rgb(0xFF, 0xB2, 0x24),
    amber_hi: Color32::from_rgb(0xFF, 0xC6, 0x5C),
    amber_lo: Color32::from_rgb(0x3A, 0x2B, 0x0E),
    on_amber: Color32::from_rgb(0x0F, 0x10, 0x12),
    green: Color32::from_rgb(0x3D, 0xDC, 0x84),
    red: Color32::from_rgb(0xFF, 0x5C, 0x4D),
};

/// Warm paper surfaces; accent and status colours darkened so they still
/// read as text on the light background.
pub const LIGHT: Palette = Palette {
    bg: Color32::from_rgb(0xF3, 0xF1, 0xEC),
    surface: Color32::from_rgb(0xFB, 0xFA, 0xF7),
    surface_hi: Color32::from_rgb(0xEA, 0xE7, 0xE0),
    surface_hover: Color32::from_rgb(0xE0, 0xDC, 0xD3),
    well: Color32::from_rgb(0xE6, 0xE3, 0xDC),
    line: Color32::from_rgb(0xD0, 0xCB, 0xC1),
    text: Color32::from_rgb(0x1C, 0x1E, 0x22),
    text_dim: Color32::from_rgb(0x55, 0x5A, 0x61),
    text_faint: Color32::from_rgb(0x86, 0x8B, 0x92),
    amber: Color32::from_rgb(0x9A, 0x58, 0x00),
    amber_hi: Color32::from_rgb(0xB8, 0x6B, 0x00),
    amber_lo: Color32::from_rgb(0xF5, 0xE2, 0xC0),
    on_amber: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    green: Color32::from_rgb(0x15, 0x7A, 0x43),
    red: Color32::from_rgb(0xC0, 0x2E, 0x22),
};

static LIGHT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// The palette of the theme currently shown.
pub fn c() -> &'static Palette {
    if LIGHT_ACTIVE.load(Ordering::Relaxed) {
        &LIGHT
    } else {
        &DARK
    }
}

/// Call once per frame, before painting: follows the user's preference
/// (system / light / dark) as resolved by egui.
pub fn sync(ctx: &egui::Context) {
    LIGHT_ACTIVE.store(ctx.theme() == Theme::Light, Ordering::Relaxed);
}

pub const RADIUS: u8 = 3;

pub fn strong_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("strong".into()))
}

pub fn mono(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    // The theme preference itself (default: follow the system) is left to
    // egui, which persists the user's choice across launches.
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
    });
    for (theme, palette) in [(Theme::Dark, &DARK), (Theme::Light, &LIGHT)] {
        let mut style = (*ctx.style_of(theme)).clone();
        style.visuals = visuals(theme, palette);
        ctx.set_style_of(theme, style);
    }
}

fn visuals(theme: Theme, p: &Palette) -> Visuals {
    let mut v = theme.default_visuals();
    let r = CornerRadius::same(RADIUS);
    v.panel_fill = p.bg;
    v.window_fill = p.surface;
    v.extreme_bg_color = p.well;
    v.faint_bg_color = p.surface;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.hyperlink_color = p.amber;
    v.selection.bg_fill = p.amber_lo;
    v.selection.stroke = Stroke::new(1.0, p.amber);
    v.text_cursor.stroke = Stroke::new(2.0, p.amber);

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.surface;
    w.noninteractive.weak_bg_fill = p.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text_dim);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.surface_hi;
    w.inactive.weak_bg_fill = p.surface_hi;
    w.inactive.bg_stroke = Stroke::new(1.0, p.line);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.surface_hover;
    w.hovered.weak_bg_fill = p.surface_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.amber.gamma_multiply(0.55));
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.amber_lo;
    w.active.weak_bg_fill = p.amber_lo;
    w.active.bg_stroke = Stroke::new(1.0, p.amber);
    w.active.fg_stroke = Stroke::new(1.0, p.amber_hi);
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
    ] {
        let mut chain: Vec<String> = vec![first.into(), "maple".into(), "system-cjk".into()];
        chain.extend(egui_fallbacks.iter().cloned());
        chain.dedup();
        chain.retain(|name| fonts.font_data.contains_key(name));
        fonts.families.insert(family, chain);
    }
    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.x relative luminance.
    fn luminance(c: Color32) -> f64 {
        let ch = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    fn palettes() -> [(&'static str, &'static Palette); 2] {
        [("dark", &DARK), ("light", &LIGHT)]
    }

    /// Body text and labels: WCAG AA (4.5:1) on every surface they sit on.
    #[test]
    fn text_meets_wcag_aa_on_all_surfaces() {
        for (name, p) in palettes() {
            for (fg_name, fg) in [("text", p.text), ("text_dim", p.text_dim)] {
                for (bg_name, bg) in [
                    ("bg", p.bg),
                    ("surface", p.surface),
                    ("surface_hi", p.surface_hi),
                    ("well", p.well),
                ] {
                    let ratio = contrast(fg, bg);
                    assert!(ratio >= 4.5, "{name}: {fg_name} on {bg_name} = {ratio:.2}");
                }
            }
        }
    }

    /// Accent/status colours are used as text (links, readouts, log lines).
    #[test]
    fn signal_colours_are_readable_as_text() {
        for (name, p) in palettes() {
            for (c_name, c) in [("amber", p.amber), ("green", p.green), ("red", p.red)] {
                let ratio = contrast(c, p.surface);
                assert!(ratio >= 4.5, "{name}: {c_name} on surface = {ratio:.2}");
            }
            let ratio = contrast(p.on_amber, p.amber);
            assert!(ratio >= 4.5, "{name}: on_amber on amber = {ratio:.2}");
        }
    }

    /// Hints / disabled text: at least 3:1 (WCAG AA for large / incidental text).
    #[test]
    fn faint_text_is_still_visible() {
        for (name, p) in palettes() {
            let ratio = contrast(p.text_faint, p.surface);
            assert!(ratio >= 3.0, "{name}: text_faint on surface = {ratio:.2}");
        }
    }
}
