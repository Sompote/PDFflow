//! Dock and dialog widgets ported from PhotoCraft's `crates/ui-egui/src/widgets.rs`
//! (github.com/storytold/photocraft, MIT OR Apache-2.0), reduced to the Pro theme and with
//! PhotoCraft's engine, i18n and tab-strip dependencies removed.

use crate::theme::Tokens;
use eframe::egui::{self, Color32, CornerRadius, Rect, Response, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

/// Height of a panel footer's bar under its 1 px line.
pub const FOOTER_BAR: f32 = 30.0;
const BODY_MARGIN: i8 = 8;

/// Photoshop-grammar panel group: dark tab strip with flat tabs and a flat body.
/// `height` is the whole group's height including the strip. Returns the selected tab.
pub fn card(ui: &mut Ui, id: &str, tabs: &[&str], selected: &mut usize, height: f32, body: impl FnOnce(&mut Ui, usize)) {
    let t = Tokens::get(ui.ctx());
    let width = ui.available_width();
    let (strip, _) = ui.allocate_exact_size(vec2(width, 26.0), Sense::hover());
    ui.painter().rect_filled(strip, CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 }, t.tab_strip);
    let mut x = strip.left();
    for (i, name) in tabs.iter().enumerate() {
        let galley = ui.painter().layout_no_wrap((*name).to_owned(), egui::FontId::proportional(12.0), t.text);
        let r = Rect::from_min_size(pos2(x, strip.top()), vec2(galley.size().x + 24.0, strip.height()));
        let resp = ui.interact(r, ui.id().with((id, i)), Sense::click());
        if resp.clicked() {
            *selected = i;
        }
        let on = *selected == i;
        if on {
            ui.painter().rect_filled(r, CornerRadius { nw: if i == 0 { 3 } else { 0 }, ne: 0, sw: 0, se: 0 }, t.card);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 0.0, t.hover.gamma_multiply(0.4));
        }
        let color = if on { t.text } else { t.text_faint };
        ui.painter().galley_with_override_text_color(r.center() - galley.size() / 2.0, galley, color);
        x = r.right();
    }
    // The body gets an exact rect, so a panel never grows past its share of the dock.
    let body_h = (height - 26.0 - 2.0).max(40.0);
    let (body_rect, _) = ui.allocate_exact_size(vec2(width, body_h), Sense::hover());
    ui.painter().rect_filled(body_rect, CornerRadius { nw: 0, ne: 0, sw: 3, se: 3 }, t.card);
    let inner = body_rect.shrink(BODY_MARGIN as f32);
    let mut child = ui.new_child(egui::UiBuilder::new().id_salt(id).max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
    child.set_clip_rect(body_rect.intersect(ui.clip_rect()));
    body(&mut child, *selected);
    ui.add_space(2.0);
}

/// The height a [`panel_footer`] takes from the panel body.
pub fn footer_height() -> f32 {
    1.0 + FOOTER_BAR - BODY_MARGIN as f32
}

/// Height left for a panel's list above its [`panel_footer`].
pub fn list_height(ui: &Ui) -> f32 {
    (ui.max_rect().bottom() - ui.cursor().top() - footer_height() - ui.spacing().item_spacing.y).max(20.0)
}

/// A panel footer as in Photoshop's Layers panel: a bar of buttons under an edge-to-edge line,
/// laid out from the right.
pub fn panel_footer<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let t = Tokens::get(ui.ctx());
    let m = BODY_MARGIN as f32;
    // Anchored to the body's bottom edge, whatever the list above it took.
    let max = ui.max_rect();
    let r = Rect::from_min_max(pos2(max.left(), max.bottom() - footer_height()), max.right_bottom());
    let y = r.top() + 0.5;
    ui.painter().line_segment([pos2(r.left() - m, y), pos2(r.right() + m, y)], Stroke::new(1.0, t.separator));
    let bar = Rect::from_min_size(pos2(r.left(), r.top() + 1.0), vec2(r.width(), FOOTER_BAR));
    let mut bar_ui = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::right_to_left(egui::Align::Center)));
    bar_ui.spacing_mut().item_spacing.x = 2.0;
    add(&mut bar_ui)
}

/// Spectrum primary button: filled accent pill.
pub fn primary_button(ui: &mut Ui, label: &str, min_width: f32) -> Response {
    button(ui, label, min_width, true)
}

/// Spectrum secondary button: outlined pill.
pub fn secondary_button(ui: &mut Ui, label: &str, min_width: f32) -> Response {
    button(ui, label, min_width, false)
}

fn button(ui: &mut Ui, label: &str, min_width: f32, primary: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let fg = if primary { Color32::WHITE } else { t.text };
    let galley = ui.painter().layout_no_wrap(label.to_owned(), egui::FontId::proportional(12.0), fg);
    let h = 28.0;
    let size = vec2((galley.size().x + 28.0).max(min_width), h);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let down = resp.is_pointer_button_down_on();
    let r = h / 2.0;
    if primary {
        let fill = if down {
            t.accent.gamma_multiply(0.8)
        } else if resp.hovered() {
            t.accent.gamma_multiply(0.9)
        } else {
            t.accent
        };
        ui.painter().rect_filled(rect, r, fill);
    } else {
        if resp.hovered() || down {
            ui.painter().rect_filled(rect, r, t.hover);
        }
        ui.painter().rect_stroke(rect, r, Stroke::new(1.5, if resp.hovered() { t.text } else { t.text_dim }), StrokeKind::Inside);
    }
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, fg);
    resp
}

/// Photoshop check box with its label; clicking either toggles it.
pub fn checkbox(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let mut resp = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let (rect, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            let l = ui
                .add(egui::Label::new(egui::RichText::new(label).color(t.text_dim)).selectable(false).sense(Sense::click()))
                .on_hover_cursor(egui::CursorIcon::PointingHand);
            let pressed = resp.is_pointer_button_down_on() || l.is_pointer_button_down_on();
            check_box(ui.painter(), rect, *on, resp.hovered() || l.hovered(), pressed, &t);
            resp.union(l)
        })
        .inner;
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    resp
}

fn check_box(p: &egui::Painter, rect: Rect, on: bool, hovered: bool, pressed: bool, t: &Tokens) {
    let fill = match (on, pressed) {
        (true, false) => t.accent,
        (true, true) => t.accent.gamma_multiply(0.75),
        (false, false) => t.field,
        (false, true) => t.field.lerp_to_gamma(Color32::BLACK, 0.18),
    };
    p.rect_filled(rect, 2.0, fill);
    if on {
        let a = rect.left_center() + vec2(3.0, 0.5);
        let b = rect.center_bottom() + vec2(-1.0, -3.5);
        let c = rect.right_top() + vec2(-3.0, 3.5);
        let tick = Stroke::new(rect.width() * 0.125, Color32::WHITE);
        p.line_segment([a, b], tick);
        p.line_segment([b, c], tick);
    } else {
        p.rect_stroke(rect, 2.0, Stroke::new(1.5, if hovered || pressed { t.text_dim } else { t.text_faint }), StrokeKind::Inside);
    }
}

/// Vertical hairline for horizontal layouts (options bar separators).
pub fn vline(ui: &mut Ui, height: f32) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(9.0, height), Sense::hover());
    ui.painter().line_segment([r.center_top(), r.center_bottom()], Stroke::new(1.0, t.separator));
}

/// Options-bar label ending with a colon, as in Photoshop ("Size:").
pub fn option_label(ui: &mut Ui, text: &str) {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(text).color(t.text_dim));
}

/// A colour chip that opens egui's picker, followed by quick swatches. Returns true on change.
pub fn color_row(ui: &mut Ui, color: &mut Color32) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut changed = egui::color_picker::color_edit_button_srgba(ui, color, egui::color_picker::Alpha::Opaque).changed();
    for c in [
        Color32::BLACK,
        Color32::from_rgb(20, 50, 140),
        Color32::from_rgb(200, 30, 30),
        Color32::from_rgb(20, 130, 60),
        Color32::from_rgb(255, 220, 0),
        Color32::WHITE,
    ] {
        let (r, resp) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::click());
        ui.painter().rect_filled(r, 2.0, c);
        let edge = if *color == c { t.accent } else { t.field_border };
        ui.painter().rect_stroke(r, 2.0, Stroke::new(if *color == c { 2.0 } else { 1.0 }, edge), StrokeKind::Outside);
        if resp.clicked() {
            *color = c;
            changed = true;
        }
    }
    changed
}
