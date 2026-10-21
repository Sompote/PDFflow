//! Draws the flattened Lucide icons from `icon_data.rs` as strokes, so no SVG renderer is needed.

use crate::icon_data::ICONS;
use crate::theme::Tokens;
use eframe::egui::{self, Color32, Painter, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, pos2};

pub fn paint(painter: &Painter, rect: Rect, name: &str, color: Color32) {
    let Some((_, polys)) = ICONS.iter().find(|(n, _)| *n == name) else { return };
    let s = rect.width().min(rect.height()) / 24.0;
    let o = rect.center() - Vec2::splat(12.0 * s);
    let stroke = Stroke::new((2.0 * s).max(1.0), color);
    for (pts, closed) in polys.iter() {
        let pts: Vec<Pos2> = pts.iter().map(|p| pos2(o.x + p[0] * s, o.y + p[1] * s)).collect();
        if *closed {
            painter.add(Shape::closed_line(pts, stroke));
        } else {
            painter.add(Shape::line(pts, stroke));
        }
    }
}

/// A square icon button in the Photoshop toolbar style; `on` draws it as the active tool.
pub fn button(ui: &mut Ui, name: &str, on: bool, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 28.0), Sense::click());
    let bg = if on {
        t.pressed
    } else if resp.is_pointer_button_down_on() {
        t.pressed
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, t.radius_sm, bg);
    let color = if on { Color32::WHITE } else { t.icon };
    paint(ui.painter(), rect.shrink2(egui::vec2(7.0, 6.0)), name, color);
    resp.on_hover_text(tip)
}

/// A tool button that can be disabled (dimmed, no click), as PhotoCraft does for tools that
/// need a raster layer.
pub fn button_state(ui: &mut Ui, name: &str, on: bool, enabled: bool, tip: &str) -> Response {
    if enabled {
        return button(ui, name, on, tip);
    }
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(30.0, 28.0), Sense::hover());
    paint(ui.painter(), rect.shrink2(egui::vec2(7.0, 6.0)), name, t.text_faint.gamma_multiply(0.45));
    resp.on_hover_text(tip)
}

/// A smaller borderless icon button for panel footers and the options bar.
pub fn small(ui: &mut Ui, name: &str, tip: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(24.0, 22.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, t.radius_sm, t.hover);
    }
    paint(ui.painter(), rect.shrink2(egui::vec2(5.0, 4.0)), name, t.icon);
    resp.on_hover_text(tip)
}
