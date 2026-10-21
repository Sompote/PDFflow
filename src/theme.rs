//! Design tokens and egui style, ported from PhotoCraft's Pro themes
//! (github.com/storytold/photocraft, `crates/ui-egui/src/theme.rs`, MIT OR Apache-2.0).

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle, Visuals};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemeKind {
    Pro,
    ProMedium,
}

#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub chrome: Color32,
    pub canvas: Color32,
    pub dock: Color32,
    pub card: Color32,
    pub card_border: Color32,
    pub field: Color32,
    pub field_border: Color32,
    pub hover: Color32,
    pub pressed: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub icon: Color32,
    pub accent: Color32,
    pub separator: Color32,
    pub shadow: Color32,
    pub danger: Color32,
    pub tab_strip: Color32,
    pub row_selected: Color32,
    pub radius_sm: f32,
    pub radius: f32,
    pub radius_lg: f32,
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Self {
        let pro = Tokens {
            chrome: Color32::from_rgb(50, 50, 50),
            canvas: Color32::from_rgb(40, 40, 40),
            dock: Color32::from_rgb(30, 30, 30),
            card: Color32::from_rgb(50, 50, 50),
            card_border: Color32::from_rgb(30, 30, 30),
            field: Color32::from_rgb(36, 36, 36),
            field_border: Color32::from_rgb(74, 74, 74),
            hover: Color32::from_rgb(66, 66, 66),
            pressed: Color32::from_rgb(78, 78, 78),
            text: Color32::from_rgb(222, 222, 222),
            text_dim: Color32::from_rgb(178, 178, 178),
            text_faint: Color32::from_rgb(128, 128, 128),
            icon: Color32::from_rgb(200, 200, 200),
            accent: Color32::from_rgb(55, 142, 240),
            separator: Color32::from_rgb(30, 30, 30),
            shadow: Color32::from_black_alpha(150),
            danger: Color32::from_rgb(236, 91, 98),
            tab_strip: Color32::from_rgb(38, 38, 38),
            row_selected: Color32::from_rgb(82, 82, 82),
            radius_sm: 3.0,
            radius: 4.0,
            radius_lg: 6.0,
        };
        match kind {
            ThemeKind::Pro => pro,
            // Photoshop 2026's default brightness.
            ThemeKind::ProMedium => Tokens {
                chrome: Color32::from_rgb(83, 83, 83),
                dock: Color32::from_rgb(66, 66, 66),
                card: Color32::from_rgb(83, 83, 83),
                card_border: Color32::from_rgb(66, 66, 66),
                field: Color32::from_rgb(69, 69, 69),
                field_border: Color32::from_rgb(104, 104, 104),
                hover: Color32::from_rgb(98, 98, 98),
                pressed: Color32::from_rgb(112, 112, 112),
                text: Color32::from_rgb(238, 238, 238),
                text_dim: Color32::from_rgb(212, 212, 212),
                text_faint: Color32::from_rgb(160, 160, 160),
                icon: Color32::from_rgb(226, 226, 226),
                separator: Color32::from_rgb(62, 62, 62),
                tab_strip: Color32::from_rgb(74, 74, 74),
                row_selected: Color32::from_rgb(107, 107, 107),
                ..pro
            },
        }
    }

    pub fn get(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp(egui::Id::new("pdfeditor-theme"))).unwrap_or_else(|| Self::for_kind(ThemeKind::ProMedium))
    }
}

pub const SARABUN: &[u8] = include_bytes!("../assets/fonts/Sarabun-Regular.ttf");
pub const SARABUN_BOLD: &[u8] = include_bytes!("../assets/fonts/Sarabun-Bold.ttf");
const INTER: &[u8] = include_bytes!("../assets/fonts/Inter-Regular.ttf");

/// Font family used for document text (regular or bold).
pub fn doc_family(bold: bool) -> FontFamily {
    FontFamily::Name(if bold { "doc-bold" } else { "doc" }.into())
}

/// Inter for the interface (as in PhotoCraft), Sarabun as fallback so Thai file names render,
/// and Sarabun on its own for document text so the screen matches the saved PDF.
pub fn install_fonts(ctx: &egui::Context) {
    let mut f = FontDefinitions::empty();
    f.font_data.insert("inter".into(), Arc::new(FontData::from_static(INTER)));
    f.font_data.insert("sarabun".into(), Arc::new(FontData::from_static(SARABUN)));
    f.font_data.insert("sarabun-bold".into(), Arc::new(FontData::from_static(SARABUN_BOLD)));
    let ui = vec!["inter".to_owned(), "sarabun".to_owned()];
    f.families.insert(FontFamily::Proportional, ui.clone());
    f.families.insert(FontFamily::Monospace, ui);
    f.families.insert(doc_family(false), vec!["sarabun".into()]);
    f.families.insert(doc_family(true), vec!["sarabun-bold".into()]);
    ctx.set_fonts(f);
}

pub fn apply(ctx: &egui::Context, kind: ThemeKind) {
    let t = Tokens::for_kind(kind);
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("pdfeditor-theme"), t));
    let mut v = Visuals::dark();
    v.panel_fill = t.chrome;
    v.window_fill = t.card;
    v.window_stroke = Stroke::new(1.0, t.card_border);
    v.extreme_bg_color = t.field;
    v.faint_bg_color = t.card;
    v.code_bg_color = t.field;
    v.override_text_color = Some(t.text);
    v.hyperlink_color = t.accent;
    v.error_fg_color = t.danger;
    v.window_corner_radius = CornerRadius::same(t.radius_lg as u8);
    v.menu_corner_radius = CornerRadius::same(t.radius as u8);
    v.window_shadow = egui::Shadow { offset: [0, 10], blur: 32, spread: 0, color: t.shadow };
    v.popup_shadow = egui::Shadow { offset: [0, 6], blur: 20, spread: 0, color: t.shadow };
    v.selection.bg_fill = t.accent;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.slider_trailing_fill = true;
    v.handle_shape = egui::style::HandleShape::Circle;
    let r = CornerRadius::same(t.radius_sm as u8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.card;
    w.noninteractive.weak_bg_fill = t.card;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.separator);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text_dim);
    w.noninteractive.corner_radius = r;
    for (wv, bg, stroke) in [
        (&mut w.inactive, t.field, t.field_border),
        (&mut w.hovered, t.hover, t.field_border),
        (&mut w.active, t.pressed, t.accent),
        (&mut w.open, t.hover, t.field_border),
    ] {
        wv.bg_fill = bg;
        wv.weak_bg_fill = bg;
        wv.bg_stroke = Stroke::new(1.0, stroke);
        wv.fg_stroke = Stroke::new(1.0, t.text);
        wv.corner_radius = r;
        wv.expansion = 0.0;
    }
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(10.5)),
            (TextStyle::Body, FontId::proportional(12.0)),
            (TextStyle::Button, FontId::proportional(12.0)),
            (TextStyle::Heading, FontId::proportional(15.0)),
            (TextStyle::Monospace, FontId::monospace(12.0)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        s.spacing.interact_size = egui::vec2(24.0, 24.0);
        s.spacing.slider_width = 110.0;
        s.spacing.combo_width = 90.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.window_margin = egui::Margin::same(16);
        s.spacing.icon_width = 14.0;
        s.interaction.tooltip_delay = 0.35;
        let mut scroll = egui::style::ScrollStyle::thin();
        scroll.interact_handle_opacity = 1.0;
        scroll.interact_background_opacity = 0.4;
        scroll.floating_width = 4.0;
        s.spacing.scroll = scroll;
    });
    let style = ctx.global_style();
    ctx.set_style_of(egui::Theme::Light, Arc::clone(&style));
    ctx.set_style_of(egui::Theme::Dark, style);
}
