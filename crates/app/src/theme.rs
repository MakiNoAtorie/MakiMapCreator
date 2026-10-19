//! Look per `docs/design/ui.md`: warm neutrals, one ochre accent, 2 px radius, dense spacing.

use std::sync::Arc;

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Vec2,
};

/// Font files embedded from `crates/app/fonts/` at build time: (file name, bytes).
const EMBEDDED: &[(&str, &[u8])] = include!(concat!(env!("OUT_DIR"), "/fonts.rs"));

/// (file name, egui font family). Files in `fonts/` with other names are ignored.
const FONT_FILES: &[(&str, &str)] = &[
    ("IBMPlexSans-Regular.ttf", "sans"),
    ("SourceSerif4-Regular.ttf", "serif"),
    ("SourceSerif4-Bold.ttf", "serif-bold"),
    ("SourceSerif4-It.ttf", "serif-italic"),
];

pub struct Theme {
    pub canvas: Color32,
    pub panel: Color32,
    panel_alt: Color32,
    field: Color32,
    pub widget: Color32,
    pub widget_hi: Color32,
    pub line: Color32,
    pub text: Color32,
    pub text_weak: Color32,
    pub accent: Color32,
    pub accent_dim: Color32,
}

impl Theme {
    pub fn new(dark: bool) -> Theme {
        let c = Color32::from_rgb;
        if dark {
            Theme {
                canvas: c(26, 25, 23),
                panel: c(40, 38, 35),
                panel_alt: c(46, 44, 40),
                field: c(30, 29, 27),
                widget: c(54, 51, 47),
                widget_hi: c(66, 62, 57),
                line: c(60, 57, 52),
                text: c(222, 215, 202),
                text_weak: c(150, 142, 130),
                accent: c(206, 156, 72),
                accent_dim: Color32::from_rgba_unmultiplied(206, 156, 72, 70),
            }
        } else {
            Theme {
                canvas: c(208, 202, 190),
                panel: c(236, 231, 221),
                panel_alt: c(228, 222, 210),
                field: c(248, 245, 238),
                widget: c(222, 216, 204),
                widget_hi: c(210, 203, 189),
                line: c(196, 188, 174),
                text: c(42, 38, 34),
                text_weak: c(112, 104, 94),
                accent: c(150, 98, 24),
                accent_dim: Color32::from_rgba_unmultiplied(170, 112, 30, 70),
            }
        }
    }

    pub fn apply(&self, ctx: &egui::Context, dark: bool) {
        let mut v = if dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.panel_fill = self.panel;
        v.window_fill = self.panel;
        v.extreme_bg_color = self.field;
        v.faint_bg_color = self.panel_alt;
        v.code_bg_color = self.field;
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.accent_dim;
        v.selection.stroke = Stroke::new(1.0, self.accent);
        v.window_stroke = Stroke::new(1.0, self.line);
        v.window_shadow = egui::Shadow {
            offset: [0, 2],
            blur: 8,
            spread: 0,
            color: Color32::from_black_alpha(60),
        };
        v.popup_shadow = v.window_shadow;
        v.window_corner_radius = CornerRadius::same(2);
        v.menu_corner_radius = CornerRadius::same(2);
        v.override_text_color = Some(self.text);
        for (w, bg, stroke) in [
            (&mut v.widgets.noninteractive, self.panel, self.line),
            (&mut v.widgets.inactive, self.widget, self.line),
            (&mut v.widgets.hovered, self.widget_hi, self.text_weak),
            (&mut v.widgets.active, self.accent_dim, self.accent),
            (&mut v.widgets.open, self.widget_hi, self.line),
        ] {
            w.bg_fill = bg;
            w.weak_bg_fill = bg;
            w.bg_stroke = Stroke::new(1.0, stroke);
            w.fg_stroke = Stroke::new(1.0, self.text);
            w.corner_radius = CornerRadius::same(2);
            w.expansion = 0.0;
        }
        ctx.set_visuals(v);
        ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = Vec2::new(6.0, 4.0);
            s.spacing.button_padding = Vec2::new(6.0, 2.0);
            s.spacing.interact_size = Vec2::new(40.0, 22.0);
            s.spacing.menu_margin = egui::Margin::same(4);
            s.spacing.slider_width = 120.0;
            s.text_styles
                .insert(TextStyle::Body, FontId::proportional(13.0));
            s.text_styles
                .insert(TextStyle::Button, FontId::proportional(13.0));
            s.text_styles
                .insert(TextStyle::Small, FontId::proportional(11.0));
            s.text_styles
                .insert(TextStyle::Heading, FontId::proportional(15.0));
            s.text_styles
                .insert(TextStyle::Monospace, FontId::monospace(12.5));
        });
    }
}

/// Installs the embedded fonts; missing ones fall back to egui's default fonts.
/// Serif families are `FontFamily::Name("serif" | "serif-bold" | "serif-italic")`.
pub fn install_fonts(ctx: &egui::Context) {
    let mut f = FontDefinitions::default();
    let fallback = f.families[&FontFamily::Proportional].clone();
    for (file, family) in FONT_FILES {
        let mut chain = fallback.clone();
        if let Some((_, bytes)) = EMBEDDED.iter().find(|(n, _)| n == file) {
            f.font_data
                .insert((*family).into(), Arc::new(FontData::from_static(bytes)));
            chain.insert(0, (*family).into());
        }
        let key = if *family == "sans" {
            FontFamily::Proportional
        } else {
            FontFamily::Name((*family).into())
        };
        f.families.insert(key, chain);
    }
    ctx.set_fonts(f);
}
