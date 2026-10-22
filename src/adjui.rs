//! Image › Adjustments: the Image menu, the options-bar Adjustments button and the adjustment
//! dialogs (with live preview on the picture), as in PhotoCraft and Photoshop. The pixel
//! formulas are in `adjust.rs`. An adjustment works on the selected picture, inside the
//! marquee or lasso selection when there is one, and is one undo step.

use super::rtools::SelShape;
use super::*;
use crate::adjust::{self, AUTO, Accel, Adj, AdjKind, LevelsCh, MENU, PHOTO_FILTERS};
use crate::raster::Raster;

pub(super) struct AdjustDialog {
    adj: Adj,
    page: usize,
    idx: usize,
    /// The picture's pixels when the dialog opened.
    orig: Raster,
    sel: Option<SelShape>,
    before: Vec<PageEntry>,
    preview: bool,
    /// The settings the live picture currently shows (`None`: the original).
    shown: Option<Adj>,
    /// Levels/Curves channel, Color Balance tone, Channel Mixer output, Selective Color range.
    channel: usize,
    /// Selected Curves point.
    point: Option<usize>,
    /// Luminosity, red, green and blue histograms of the pixels being adjusted.
    hist: [[u64; 256]; 4],
    /// Photo Filter preset index (`None`: a custom colour).
    filter: Option<usize>,
    error: Option<String>,
}

/// The adjusted copy of `orig`: `adj` on the selection's bounding box, kept inside the selection.
fn render(orig: &Raster, adj: &Adj, sel: Option<&SelShape>) -> Raster {
    let (x0, y0, x1, y1) = region(orig, sel);
    let (w, h) = ((x1 - x0) as usize, (y1 - y0) as usize);
    let mut out = orig.clone();
    if w == 0 || h == 0 {
        return out;
    }
    let (mut px, inside) = pixels(orig, sel, (x0, y0, x1, y1));
    adjust::apply(adj, &mut px, w, h, &inside);
    for (k, (p, ins)) in px.iter().zip(&inside).enumerate() {
        if *ins {
            let (x, y) = (x0 + (k % w) as u32, y0 + (k / w) as u32);
            let q = out.px_mut(x, y);
            for c in 0..3 {
                q[c] = (p[c] * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

/// Pixel bounds of the selection (or the whole picture).
fn region(r: &Raster, sel: Option<&SelShape>) -> (u32, u32, u32, u32) {
    match sel {
        Some(s) => {
            let b = s.bounds();
            let x0 = b.min.x.floor().clamp(0.0, r.w as f32) as u32;
            let y0 = b.min.y.floor().clamp(0.0, r.h as f32) as u32;
            let x1 = b.max.x.ceil().clamp(x0 as f32, r.w as f32) as u32;
            let y1 = b.max.y.ceil().clamp(y0 as f32, r.h as f32) as u32;
            (x0, y0, x1, y1)
        }
        None => (0, 0, r.w, r.h),
    }
}

/// The region as straight RGBA in 0..=1, and which of its pixels the selection covers.
fn pixels(r: &Raster, sel: Option<&SelShape>, (x0, y0, x1, y1): (u32, u32, u32, u32)) -> (Vec<[f32; 4]>, Vec<bool>) {
    let n = ((x1 - x0) * (y1 - y0)) as usize;
    let (mut px, mut inside) = (Vec::with_capacity(n), Vec::with_capacity(n));
    for y in y0..y1 {
        for x in x0..x1 {
            px.push(r.get(x, y).map(|v| v as f32 / 255.0));
            inside.push(sel.is_none_or(|s| s.contains(pos2(x as f32 + 0.5, y as f32 + 0.5))));
        }
    }
    (px, inside)
}

/// Luminosity and channel histograms of the pixels an adjustment will change.
fn histograms(r: &Raster, sel: Option<&SelShape>) -> [[u64; 256]; 4] {
    let (px, inside) = pixels(r, sel, region(r, sel));
    let rgb = adjust::histograms(&px, &inside);
    let mut lum = [0u64; 256];
    for (p, _) in px.iter().zip(&inside).filter(|(p, i)| **i && p[3] > 0.0) {
        lum[((0.3 * p[0] + 0.59 * p[1] + 0.11 * p[2]).clamp(0.0, 1.0) * 255.0).round() as usize] += 1;
    }
    [lum, rgb[0], rgb[1], rgb[2]]
}

/// Menu shortcut text: ⌥⇧⌘L on macOS, Ctrl+Alt+Shift+L elsewhere.
fn accel_text(a: Option<Accel>) -> String {
    let Some((alt, shift, key)) = a else { return String::new() };
    if cfg!(target_os = "macos") {
        format!("{}{}⌘{key}", if alt { "⌥" } else { "" }, if shift { "⇧" } else { "" })
    } else {
        format!("Ctrl+{}{}{key}", if alt { "Alt+" } else { "" }, if shift { "Shift+" } else { "" })
    }
}

fn accel_key(c: char) -> Key {
    match c {
        'L' => Key::L,
        'M' => Key::M,
        'U' => Key::U,
        'B' => Key::B,
        _ => Key::I,
    }
}

/// A labelled slider row in a two-column grid; true when the value changed.
fn slider(ui: &mut Ui, label: &str, v: &mut f32, range: std::ops::RangeInclusive<f32>, suffix: &str, decimals: usize) -> bool {
    ui.label(label);
    let c = ui.add(egui::Slider::new(v, range).suffix(suffix).max_decimals(decimals).min_decimals(decimals.min(2))).changed();
    ui.end_row();
    c
}

fn color3(ui: &mut Ui, c: &mut [f32; 3]) -> bool {
    let mut col = Color32::from_rgb((c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8);
    if egui::color_picker::color_edit_button_srgba(ui, &mut col, egui::color_picker::Alpha::Opaque).changed() {
        *c = adjust::rgb3([col.r(), col.g(), col.b()]);
        return true;
    }
    false
}

fn swatch(c: [f32; 3]) -> Color32 {
    Color32::from_rgb((c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8)
}

fn combo(ui: &mut Ui, id: &str, sel: &mut usize, items: &[&str]) -> bool {
    let before = *sel;
    egui::ComboBox::from_id_salt(id).selected_text(items[*sel]).width(150.0).show_ui(ui, |ui| {
        for (k, it) in items.iter().enumerate() {
            ui.selectable_value(sel, k, *it);
        }
    });
    *sel != before
}

/// A histogram filling `r` (square-root scaled, as Photoshop's Levels draws it).
fn paint_histogram(p: &egui::Painter, r: Rect, h: &[u64; 256], color: Color32) {
    let max = h.iter().copied().max().unwrap_or(0).max(1) as f32;
    let bw = r.width() / 256.0;
    for (k, &v) in h.iter().enumerate() {
        if v == 0 {
            continue;
        }
        let hh = (v as f32 / max).sqrt() * r.height();
        let x = r.min.x + k as f32 * bw;
        p.rect_filled(Rect::from_min_max(pos2(x, r.max.y - hh), pos2(x + bw.max(1.0), r.max.y)), 0.0, color);
    }
}

/// Triangle handles under a bar (Levels' input and output sliders); `vals` are positions in
/// 0..=1 and `fills` their colours. Returns the index of a moved handle.
fn handles(ui: &mut Ui, id: &str, width: f32, vals: &mut [f32], fills: &[Color32]) -> Option<usize> {
    let (r, resp) = ui.allocate_exact_size(vec2(width, 12.0), Sense::click_and_drag());
    let t = Tokens::get(ui.ctx());
    let key = Id::new(("levels-handle", id));
    let x = |v: f32| r.min.x + v * r.width();
    if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            // The nearest handle; the last one wins a tie so a black point at 1 can be pulled back.
            let k = (0..vals.len()).rev().min_by(|a, b| (x(vals[*a]) - p.x).abs().total_cmp(&(x(vals[*b]) - p.x).abs()));
            ui.data_mut(|d| d.insert_temp(key, k));
        }
    }
    let mut moved = None;
    if resp.dragged() {
        if let (Some(Some(k)), Some(p)) = (ui.data(|d| d.get_temp::<Option<usize>>(key)), resp.interact_pointer_pos()) {
            vals[k] = ((p.x - r.min.x) / r.width()).clamp(0.0, 1.0);
            moved = Some(k);
        }
    }
    for (v, f) in vals.iter().zip(fills) {
        let c = x(*v);
        let tri = vec![pos2(c, r.min.y + 1.0), pos2(c - 6.0, r.max.y), pos2(c + 6.0, r.max.y)];
        ui.painter().add(Shape::convex_polygon(tri, *f, Stroke::new(1.0, t.text_dim)));
    }
    moved
}

/// The Levels channel editor; true when the channel changed.
fn levels_ui(ui: &mut Ui, ch: &mut LevelsCh, hist: &[u64; 256], color: Color32) -> bool {
    let t = Tokens::get(ui.ctx());
    let w = 256.0;
    let (r, _) = ui.allocate_exact_size(vec2(w, 110.0), Sense::hover());
    ui.painter().rect_filled(r, 0.0, t.field);
    paint_histogram(ui.painter(), r.shrink2(vec2(0.0, 2.0)), hist, color);
    let mut changed = false;
    // Input: black, midtone (at m = 0.5^gamma between black and white), white.
    let m = 0.5f32.powf(ch.gamma);
    let mut v = [ch.in_black, ch.in_black + (ch.in_white - ch.in_black) * m, ch.in_white];
    match handles(ui, "in", w, &mut v, &[Color32::BLACK, Color32::GRAY, Color32::WHITE]) {
        Some(0) => {
            ch.in_black = v[0].min(ch.in_white - 2.0 / 255.0);
            changed = true;
        }
        Some(2) => {
            ch.in_white = v[2].max(ch.in_black + 2.0 / 255.0);
            changed = true;
        }
        Some(_) => {
            let f = ((v[1] - ch.in_black) / (ch.in_white - ch.in_black).max(1e-3)).clamp(0.01, 0.99);
            ch.gamma = (f.ln() / 0.5f32.ln()).clamp(0.1, 9.99);
            changed = true;
        }
        None => {}
    }
    ui.horizontal(|ui| {
        let mut b = (ch.in_black * 255.0).round();
        let mut g = ch.gamma;
        let mut wh = (ch.in_white * 255.0).round();
        option_label(ui, "Input Levels:");
        if ui.add(egui::DragValue::new(&mut b).range(0.0..=253.0).max_decimals(0)).changed() {
            ch.in_black = (b / 255.0).min(ch.in_white - 2.0 / 255.0);
            changed = true;
        }
        if ui.add(egui::DragValue::new(&mut g).range(0.1..=9.99).speed(0.01).fixed_decimals(2)).changed() {
            ch.gamma = g;
            changed = true;
        }
        if ui.add(egui::DragValue::new(&mut wh).range(2.0..=255.0).max_decimals(0)).changed() {
            ch.in_white = (wh / 255.0).max(ch.in_black + 2.0 / 255.0);
            changed = true;
        }
    });
    ui.add_space(6.0);
    let (g, _) = ui.allocate_exact_size(vec2(w, 10.0), Sense::hover());
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(g.left_top(), Color32::BLACK);
    mesh.colored_vertex(g.right_top(), Color32::WHITE);
    mesh.colored_vertex(g.right_bottom(), Color32::WHITE);
    mesh.colored_vertex(g.left_bottom(), Color32::BLACK);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
    let mut o = [ch.out_black, ch.out_white];
    match handles(ui, "out", w, &mut o, &[Color32::BLACK, Color32::WHITE]) {
        Some(0) => {
            ch.out_black = o[0];
            changed = true;
        }
        Some(_) => {
            ch.out_white = o[1];
            changed = true;
        }
        None => {}
    }
    ui.horizontal(|ui| {
        let mut b = (ch.out_black * 255.0).round();
        let mut wh = (ch.out_white * 255.0).round();
        option_label(ui, "Output Levels:");
        if ui.add(egui::DragValue::new(&mut b).range(0.0..=255.0).max_decimals(0)).changed() {
            ch.out_black = b / 255.0;
            changed = true;
        }
        if ui.add(egui::DragValue::new(&mut wh).range(0.0..=255.0).max_decimals(0)).changed() {
            ch.out_white = wh / 255.0;
            changed = true;
        }
    });
    changed
}

/// The Curves graph: click to add a point, drag to move it, drag it out of the graph to remove it.
fn curves_ui(ui: &mut Ui, pts: &mut Vec<[f32; 2]>, sel: &mut Option<usize>, hist: &[u64; 256], color: Color32) -> bool {
    let t = Tokens::get(ui.ctx());
    let side = 256.0;
    let (r, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
    let p = ui.painter_at(r.expand(6.0));
    p.rect_filled(r, 0.0, t.field);
    paint_histogram(&p, r, hist, t.text_faint.gamma_multiply(0.5));
    for k in 1..4 {
        let f = k as f32 / 4.0;
        let grid = Stroke::new(0.5, t.text_faint.gamma_multiply(0.6));
        p.line_segment([pos2(r.min.x + f * side, r.min.y), pos2(r.min.x + f * side, r.max.y)], grid);
        p.line_segment([pos2(r.min.x, r.min.y + f * side), pos2(r.max.x, r.min.y + f * side)], grid);
    }
    p.line_segment([r.left_bottom(), r.right_top()], Stroke::new(0.5, t.text_faint));
    let to_s = |q: [f32; 2]| pos2(r.min.x + q[0] * side, r.max.y - q[1] * side);
    let from_s = |s: Pos2| [((s.x - r.min.x) / side).clamp(0.0, 1.0), ((r.max.y - s.y) / side).clamp(0.0, 1.0)];
    let mut changed = false;
    if resp.drag_started() || resp.clicked() {
        if let Some(s) = resp.interact_pointer_pos() {
            let near = pts.iter().enumerate().map(|(k, q)| (k, (to_s(*q) - s).length())).filter(|(_, d)| *d < 8.0).min_by(|a, b| a.1.total_cmp(&b.1));
            *sel = match near {
                Some((k, _)) => Some(k),
                None if pts.len() < 16 => {
                    let q = from_s(s);
                    let k = pts.iter().position(|p| p[0] > q[0]).unwrap_or(pts.len());
                    if pts.iter().any(|p| (p[0] - q[0]).abs() < 0.01) {
                        None
                    } else {
                        pts.insert(k, q);
                        changed = true;
                        Some(k)
                    }
                }
                None => None,
            };
        }
    }
    if resp.dragged() {
        if let (Some(k), Some(s)) = (*sel, resp.interact_pointer_pos()) {
            let outside = !r.expand(24.0).contains(s);
            if outside && pts.len() > 2 && k > 0 && k + 1 < pts.len() {
                pts.remove(k);
                *sel = None;
            } else if k < pts.len() {
                let mut q = from_s(s);
                let lo = if k > 0 { pts[k - 1][0] + 0.01 } else { 0.0 };
                let hi = if k + 1 < pts.len() { pts[k + 1][0] - 0.01 } else { 1.0 };
                q[0] = q[0].clamp(lo, hi.max(lo));
                pts[k] = q;
            }
            changed = true;
        }
    }
    let table = adjust::curve_lut(pts);
    let line: Vec<Pos2> = (0..=128).map(|i| i as f32 / 128.0).map(|x| to_s([x, adjust::curve_at(&table, x)])).collect();
    p.add(Shape::line(line, Stroke::new(1.5, color)));
    for (k, q) in pts.iter().enumerate() {
        let c = to_s(*q);
        let rr = Rect::from_center_size(c, vec2(7.0, 7.0));
        if *sel == Some(k) {
            p.rect_filled(rr, 0.0, color);
        } else {
            p.rect_filled(rr, 0.0, t.field);
            p.rect_stroke(rr, 0.0, Stroke::new(1.0, color), StrokeKind::Middle);
        }
    }
    if let Some(k) = sel.filter(|k| *k < pts.len()) {
        ui.horizontal(|ui| {
            let mut i = (pts[k][0] * 255.0).round();
            let mut o = (pts[k][1] * 255.0).round();
            option_label(ui, "Input:");
            if ui.add(egui::DragValue::new(&mut i).range(0.0..=255.0).max_decimals(0)).changed() {
                let lo = if k > 0 { pts[k - 1][0] + 0.01 } else { 0.0 };
                let hi = if k + 1 < pts.len() { pts[k + 1][0] - 0.01 } else { 1.0 };
                pts[k][0] = (i / 255.0).clamp(lo, hi.max(lo));
                changed = true;
            }
            option_label(ui, "Output:");
            if ui.add(egui::DragValue::new(&mut o).range(0.0..=255.0).max_decimals(0)).changed() {
                pts[k][1] = o / 255.0;
                changed = true;
            }
        });
    } else {
        ui.label(egui::RichText::new("Click the curve to add a point; drag a point off the graph to remove it.").color(t.text_faint).small());
    }
    changed
}

const CHANNELS: [&str; 4] = ["RGB", "Red", "Green", "Blue"];
const CHANNEL_COLORS: [Color32; 4] = [Color32::from_gray(200), Color32::from_rgb(230, 70, 70), Color32::from_rgb(70, 200, 90), Color32::from_rgb(80, 130, 240)];

impl App {
    /// The Image › Adjustments items (shared by the menu bar and the options-bar button).
    pub(super) fn adjustment_items(&mut self, ui: &mut Ui) {
        let on = self.picture_sel().is_some();
        for m in MENU {
            match m {
                None => {
                    ui.separator();
                }
                Some((kind, label, accel)) => {
                    if ui.add_enabled(on, egui::Button::new(label).shortcut_text(accel_text(accel))).clicked() {
                        self.start_adjust(ui.ctx(), kind);
                        ui.close();
                    }
                }
            }
        }
        ui.separator();
        if ui.add_enabled(on, egui::Button::new("Equalize")).clicked() {
            self.start_adjust(ui.ctx(), AdjKind::Equalize);
            ui.close();
        }
    }

    /// The Image menu: Adjustments, then Auto Tone, Auto Contrast and Auto Color.
    pub(super) fn image_menu(&mut self, ui: &mut Ui) {
        let on = self.picture_sel().is_some();
        ui.add_enabled_ui(on, |ui| {
            ui.menu_button("Adjustments", |ui| self.adjustment_items(ui));
        });
        ui.separator();
        for (kind, label, accel) in AUTO {
            if ui.add_enabled(on, egui::Button::new(label).shortcut_text(accel_text(Some(accel)))).clicked() {
                self.start_adjust(ui.ctx(), kind);
                ui.close();
            }
        }
        if !on {
            ui.separator();
            ui.label(egui::RichText::new("Select a picture to adjust it.").color(Tokens::get(ui.ctx()).text_faint));
        }
    }

    /// The options-bar button shown while a picture is selected.
    pub(super) fn adjust_button(&mut self, ui: &mut Ui) {
        ui.menu_button("Adjustments…", |ui| {
            for (kind, label, accel) in AUTO {
                if ui.add(egui::Button::new(label).shortcut_text(accel_text(Some(accel)))).clicked() {
                    self.start_adjust(ui.ctx(), kind);
                    ui.close();
                }
            }
            ui.separator();
            self.adjustment_items(ui);
        })
        .response
        .on_hover_text("Image › Adjustments for the selected picture");
    }

    /// Image-menu shortcuts (Photoshop's), active while a picture is selected.
    pub(super) fn adjust_shortcuts(&mut self, ctx: &egui::Context) {
        if self.picture_sel().is_none() {
            return;
        }
        // The most specific combination first: a ⌘L shortcut also matches ⇧⌘L.
        let mut keys: Vec<(AdjKind, Accel)> = MENU.iter().flatten().filter_map(|m| m.2.map(|a| (m.0, a))).chain(AUTO.iter().map(|a| (a.0, a.2))).collect();
        keys.sort_by_key(|(_, (alt, shift, _))| std::cmp::Reverse(*alt as u8 + *shift as u8));
        for (kind, (alt, shift, key)) in keys {
            let mut m = Modifiers::COMMAND;
            if alt {
                m = m | Modifiers::ALT;
            }
            if shift {
                m = m | Modifiers::SHIFT;
            }
            if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, accel_key(key)))) {
                self.start_adjust(ctx, kind);
                return;
            }
        }
    }

    /// Apply `adj` to the whole of picture `idx` on `page` as one undo step (for agents).
    pub(crate) fn apply_adjustment(&mut self, ctx: &egui::Context, page: usize, idx: usize, adj: &Adj) -> Result<(), String> {
        if self.adjust_dlg.is_some() {
            return Err("An adjustment dialog is open in the window; close it first.".into());
        }
        if !self.ensure_live(ctx, page, idx) {
            return Err("The picture cannot be decoded for editing.".into());
        }
        let orig = self.rlive.as_ref().expect("live").raster.clone();
        let before = self.doc.pages.clone();
        self.set_live(render(&orig, adj, None));
        self.commit_live(before);
        Ok(())
    }

    pub(crate) fn adjusting(&self) -> bool {
        self.adjust_dlg.is_some()
    }

    /// Run a command at once, or open its dialog.
    pub(crate) fn start_adjust(&mut self, ctx: &egui::Context, kind: AdjKind) {
        let Some((page, idx, _)) = self.picture_sel() else {
            self.status = "Select a picture to adjust it.".into();
            return;
        };
        if self.adjust_dlg.is_some() || !self.ensure_live(ctx, page, idx) {
            return;
        }
        self.crop = None;
        let sel = self.sel_shape();
        let orig = self.rlive.as_ref().expect("live").raster.clone();
        let adj = Adj::new(kind);
        if kind.is_immediate() {
            let before = self.doc.pages.clone();
            self.set_live(render(&orig, &adj, sel.as_ref()));
            self.commit_live(before);
            self.status = format!("{} applied{}.", kind.title(), if sel.is_some() { " to the selection" } else { "" });
            return;
        }
        let hist = histograms(&orig, sel.as_ref());
        let mut d = AdjustDialog {
            adj,
            page,
            idx,
            orig,
            sel,
            before: self.doc.pages.clone(),
            preview: true,
            shown: None,
            channel: 0,
            point: None,
            hist,
            filter: Some(0),
            error: None,
        };
        if kind == AdjKind::ReplaceColor {
            let c = self.opts.color;
            d.adj = Adj::ReplaceColor { color: adjust::rgb3([c.r(), c.g(), c.b()]), fuzziness: 40.0, hue: 0.0, saturation: 0.0, lightness: 0.0 };
        }
        if kind == AdjKind::ColorBalance {
            d.channel = 1;
        }
        self.adjust_dlg = Some(d);
    }

    fn set_live(&mut self, raster: Raster) {
        let Some(l) = &mut self.rlive else { return };
        l.raster = raster;
        l.dirty = Some([0, 0, l.raster.w, l.raster.h]);
    }

    /// The open adjustment dialog; previews on the picture and commits on OK.
    pub(super) fn adjust_window(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.adjust_dlg.take() else { return };
        if !self.live_matches(d.page, d.idx) {
            return;
        }
        let kind = kind_of(&d.adj);
        let mut action: Option<bool> = None;
        let id = Id::new("adjust-dialog");
        let area = egui::Modal::default_area(id).anchor(egui::Align2::RIGHT_TOP, vec2(-14.0, 76.0));
        let eye = self.opts.color;
        egui::Modal::new(id).backdrop_color(Color32::TRANSPARENT).area(area).show(ctx, |ui| {
            ui.set_width(300.0);
            ui.spacing_mut().slider_width = 150.0;
            ui.heading(kind.title());
            ui.add_space(6.0);
            dialog_body(ui, &mut d, eye);
            if let Some(e) = &d.error {
                ui.colored_label(Color32::from_rgb(230, 90, 80), e);
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                widgets::checkbox(ui, &mut d.preview, "Preview");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::primary_button(ui, "OK", 70.0).clicked() {
                        action = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel", 70.0).clicked() {
                        action = Some(false);
                    }
                    if widgets::secondary_button(ui, "Reset", 60.0).on_hover_text("Back to the defaults").clicked() {
                        d.adj = Adj::new(kind);
                        d.point = None;
                    }
                });
            });
        });
        let typing = ctx.memory(|m| m.focused().is_some());
        if !typing && ctx.input(|i| i.key_pressed(Key::Enter)) {
            action = Some(true);
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            action = Some(false);
        }
        match action {
            Some(true) => {
                if d.shown.as_ref() != Some(&d.adj) {
                    self.set_live(render(&d.orig, &d.adj, d.sel.as_ref()));
                }
                self.commit_live(d.before);
                self.status = format!("{} applied{}.", kind.title(), if d.sel.is_some() { " to the selection" } else { "" });
            }
            Some(false) => self.set_live(d.orig),
            None => {
                // Spatial adjustments are slow on big pictures: preview them when the slider is let go.
                let heavy = matches!(kind, AdjKind::ShadowsHighlights | AdjKind::HdrToning) && ctx.input(|i| i.pointer.any_down());
                if d.preview && d.shown.as_ref() != Some(&d.adj) && !heavy {
                    self.set_live(render(&d.orig, &d.adj, d.sel.as_ref()));
                    d.shown = Some(d.adj.clone());
                } else if !d.preview && d.shown.is_some() {
                    self.set_live(d.orig.clone());
                    d.shown = None;
                }
                self.adjust_dlg = Some(d);
            }
        }
    }
}

fn kind_of(adj: &Adj) -> AdjKind {
    match adj {
        Adj::BrightnessContrast { .. } => AdjKind::BrightnessContrast,
        Adj::Levels { .. } => AdjKind::Levels,
        Adj::Curves { .. } => AdjKind::Curves,
        Adj::Exposure { .. } => AdjKind::Exposure,
        Adj::Vibrance { .. } => AdjKind::Vibrance,
        Adj::HueSaturation { .. } => AdjKind::HueSaturation,
        Adj::ColorBalance { .. } => AdjKind::ColorBalance,
        Adj::BlackWhite { .. } => AdjKind::BlackWhite,
        Adj::PhotoFilter { .. } => AdjKind::PhotoFilter,
        Adj::ChannelMixer { .. } => AdjKind::ChannelMixer,
        Adj::ColorLookup { .. } => AdjKind::ColorLookup,
        Adj::Invert => AdjKind::Invert,
        Adj::Posterize { .. } => AdjKind::Posterize,
        Adj::Threshold { .. } => AdjKind::Threshold,
        Adj::GradientMap { .. } => AdjKind::GradientMap,
        Adj::SelectiveColor { .. } => AdjKind::SelectiveColor,
        Adj::ShadowsHighlights(_) => AdjKind::ShadowsHighlights,
        Adj::HdrToning(_) => AdjKind::HdrToning,
        Adj::Desaturate => AdjKind::Desaturate,
        Adj::MatchColor { .. } => AdjKind::MatchColor,
        Adj::ReplaceColor { .. } => AdjKind::ReplaceColor,
        Adj::Equalize => AdjKind::Equalize,
        Adj::AutoTone => AdjKind::AutoTone,
        Adj::AutoContrast => AdjKind::AutoContrast,
        Adj::AutoColor => AdjKind::AutoColor,
    }
}

/// The controls of each dialog. `eye` is the Eyedropper (brush) colour.
fn dialog_body(ui: &mut Ui, d: &mut AdjustDialog, eye: Color32) {
    let t = Tokens::get(ui.ctx());
    let grid = |ui: &mut Ui, id: &str, body: &mut dyn FnMut(&mut Ui)| {
        egui::Grid::new(id).num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| body(ui));
    };
    let note = |ui: &mut Ui, s: &str| {
        ui.label(egui::RichText::new(s).color(t.text_faint).small());
    };
    match &mut d.adj {
        Adj::BrightnessContrast { brightness, contrast, legacy } => {
            grid(ui, "bc", &mut |ui| {
                slider(ui, "Brightness:", brightness, -150.0..=150.0, "", 0);
                let lo = if *legacy { -100.0 } else { -50.0 };
                slider(ui, "Contrast:", contrast, lo..=100.0, "", 0);
            });
            if widgets::checkbox(ui, legacy, "Use Legacy").changed() && !*legacy {
                *contrast = contrast.max(-50.0);
            }
        }
        Adj::Levels { ch } => {
            ui.horizontal(|ui| {
                option_label(ui, "Channel:");
                combo(ui, "levels-ch", &mut d.channel, &CHANNELS);
            });
            let c = d.channel;
            levels_ui(ui, &mut ch[c], &d.hist[c], CHANNEL_COLORS[c]);
            ui.add_space(4.0);
            if widgets::secondary_button(ui, "Auto", 60.0).on_hover_text("Auto Tone: stretch each channel, clipping 0.1 %").clicked() {
                let (px, inside) = pixels(&d.orig, d.sel.as_ref(), region(&d.orig, d.sel.as_ref()));
                *ch = adjust::auto_levels(&Adj::AutoTone, &px, &inside);
            }
        }
        Adj::Curves { ch } => {
            ui.horizontal(|ui| {
                option_label(ui, "Channel:");
                if combo(ui, "curves-ch", &mut d.channel, &CHANNELS) {
                    d.point = None;
                }
            });
            let c = d.channel;
            curves_ui(ui, &mut ch[c], &mut d.point, &d.hist[c], CHANNEL_COLORS[c]);
        }
        Adj::Exposure { exposure, offset, gamma } => grid(ui, "exp", &mut |ui| {
            slider(ui, "Exposure:", exposure, -20.0..=20.0, "", 2);
            slider(ui, "Offset:", offset, -0.5..=0.5, "", 4);
            slider(ui, "Gamma Correction:", gamma, 0.01..=9.99, "", 2);
        }),
        Adj::Vibrance { vibrance, saturation } => grid(ui, "vib", &mut |ui| {
            slider(ui, "Vibrance:", vibrance, -100.0..=100.0, "", 0);
            slider(ui, "Saturation:", saturation, -100.0..=100.0, "", 0);
        }),
        Adj::HueSaturation { hue, saturation, lightness, colorize } => {
            let c = *colorize;
            grid(ui, "hs", &mut |ui| {
                if c {
                    slider(ui, "Hue:", hue, 0.0..=360.0, "°", 0);
                    slider(ui, "Saturation:", saturation, 0.0..=100.0, "", 0);
                } else {
                    slider(ui, "Hue:", hue, -180.0..=180.0, "°", 0);
                    slider(ui, "Saturation:", saturation, -100.0..=100.0, "", 0);
                }
                slider(ui, "Lightness:", lightness, -100.0..=100.0, "", 0);
            });
            if widgets::checkbox(ui, colorize, "Colorize").changed() {
                if *colorize {
                    *hue = hue.rem_euclid(360.0);
                    *saturation = if *saturation <= 0.0 { 25.0 } else { *saturation };
                } else {
                    *hue = 0.0;
                    *saturation = 0.0;
                }
            }
        }
        Adj::ColorBalance { tones, preserve } => {
            ui.horizontal(|ui| {
                option_label(ui, "Tone:");
                for (k, name) in ["Shadows", "Midtones", "Highlights"].iter().enumerate() {
                    ui.radio_value(&mut d.channel, k, *name);
                }
            });
            let k = d.channel.min(2);
            grid(ui, "cb", &mut |ui| {
                slider(ui, "Cyan · Red:", &mut tones[k][0], -100.0..=100.0, "", 0);
                slider(ui, "Magenta · Green:", &mut tones[k][1], -100.0..=100.0, "", 0);
                slider(ui, "Yellow · Blue:", &mut tones[k][2], -100.0..=100.0, "", 0);
            });
            widgets::checkbox(ui, preserve, "Preserve Luminosity");
        }
        Adj::BlackWhite { weights, tint } => {
            grid(ui, "bw", &mut |ui| {
                for (k, name) in ["Reds:", "Yellows:", "Greens:", "Cyans:", "Blues:", "Magentas:"].iter().enumerate() {
                    slider(ui, name, &mut weights[k], -200.0..=300.0, "%", 0);
                }
            });
            ui.horizontal(|ui| {
                let mut on = tint.is_some();
                if widgets::checkbox(ui, &mut on, "Tint").changed() {
                    *tint = on.then_some(adjust::rgb3([225, 211, 179]));
                }
                if let Some(c) = tint {
                    color3(ui, c);
                }
            });
        }
        Adj::PhotoFilter { color, density, preserve } => {
            let mut k = d.filter.unwrap_or(PHOTO_FILTERS.len());
            ui.horizontal(|ui| {
                option_label(ui, "Filter:");
                let mut names: Vec<&str> = PHOTO_FILTERS.iter().map(|f| f.0).collect();
                names.push("Custom Colour");
                if combo(ui, "pf", &mut k, &names) && k < PHOTO_FILTERS.len() {
                    *color = adjust::rgb3(PHOTO_FILTERS[k].1);
                }
                if color3(ui, color) {
                    k = PHOTO_FILTERS.len();
                }
            });
            d.filter = (k < PHOTO_FILTERS.len()).then_some(k);
            let mut pct = *density * 100.0;
            grid(ui, "pfd", &mut |ui| {
                if slider(ui, "Density:", &mut pct, 1.0..=100.0, "%", 0) {
                    *density = pct / 100.0;
                }
            });
            widgets::checkbox(ui, preserve, "Preserve Luminosity");
        }
        Adj::ChannelMixer { matrix, mono } => {
            ui.horizontal(|ui| {
                option_label(ui, "Output Channel:");
                if *mono {
                    ui.label("Gray");
                    d.channel = 0;
                } else {
                    combo(ui, "cm", &mut d.channel, &["Red", "Green", "Blue"]);
                }
            });
            let row = &mut matrix[d.channel.min(2)];
            let mut pct = row.map(|v| v * 100.0);
            let mut changed = false;
            grid(ui, "cmg", &mut |ui| {
                changed |= slider(ui, "Red:", &mut pct[0], -200.0..=200.0, "%", 0);
                changed |= slider(ui, "Green:", &mut pct[1], -200.0..=200.0, "%", 0);
                changed |= slider(ui, "Blue:", &mut pct[2], -200.0..=200.0, "%", 0);
                changed |= slider(ui, "Constant:", &mut pct[3], -200.0..=200.0, "%", 0);
            });
            if changed {
                *row = pct.map(|v| v / 100.0);
            }
            note(ui, &format!("Total: {:+.0} %", pct[0] + pct[1] + pct[2]));
            if widgets::checkbox(ui, mono, "Monochrome").changed() {
                matrix[0] = if *mono { [0.4, 0.4, 0.2, 0.0] } else { [1.0, 0.0, 0.0, 0.0] };
            }
        }
        Adj::ColorLookup { lut, name } => {
            ui.horizontal(|ui| {
                if widgets::secondary_button(ui, "Load 3D LUT…", 110.0).clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("3D LUT", &["cube", "CUBE"]).pick_file() {
                        match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|s| adjust::parse_cube(&s)) {
                            Ok(t) => {
                                *lut = Some(Arc::new(t));
                                *name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                d.error = None;
                            }
                            Err(e) => d.error = Some(e),
                        }
                    }
                }
                ui.label(if name.is_empty() { "No table loaded" } else { name.as_str() });
            });
            note(ui, "Reads .cube files (Adobe, DaVinci Resolve).");
        }
        Adj::Posterize { levels } => {
            ui.horizontal(|ui| {
                option_label(ui, "Levels:");
                ui.add(egui::Slider::new(levels, 2..=255));
            });
        }
        Adj::Threshold { level } => {
            let (r, _) = ui.allocate_exact_size(vec2(256.0, 90.0), Sense::hover());
            ui.painter().rect_filled(r, 0.0, t.field);
            paint_histogram(ui.painter(), r, &d.hist[0], CHANNEL_COLORS[0]);
            let x = r.min.x + *level / 255.0 * r.width();
            ui.painter().line_segment([pos2(x, r.min.y), pos2(x, r.max.y)], Stroke::new(1.0, t.accent));
            ui.horizontal(|ui| {
                option_label(ui, "Threshold Level:");
                ui.add(egui::Slider::new(level, 1.0..=255.0).max_decimals(0));
            });
        }
        Adj::GradientMap { from, to, reverse } => {
            let (r, _) = ui.allocate_exact_size(vec2(256.0, 18.0), Sense::hover());
            let (a, b) = if *reverse { (*to, *from) } else { (*from, *to) };
            let mut mesh = egui::Mesh::default();
            mesh.colored_vertex(r.left_top(), swatch(a));
            mesh.colored_vertex(r.right_top(), swatch(b));
            mesh.colored_vertex(r.right_bottom(), swatch(b));
            mesh.colored_vertex(r.left_bottom(), swatch(a));
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            ui.painter().add(mesh);
            ui.horizontal(|ui| {
                option_label(ui, "Shadows:");
                color3(ui, from);
                option_label(ui, "Highlights:");
                color3(ui, to);
            });
            widgets::checkbox(ui, reverse, "Reverse");
        }
        Adj::SelectiveColor { relative, adj } => {
            ui.horizontal(|ui| {
                option_label(ui, "Colors:");
                combo(ui, "sc", &mut d.channel, &["Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas", "Whites", "Neutrals", "Blacks"]);
            });
            let a = &mut adj[d.channel.min(8)];
            grid(ui, "scg", &mut |ui| {
                slider(ui, "Cyan:", &mut a[0], -100.0..=100.0, "%", 0);
                slider(ui, "Magenta:", &mut a[1], -100.0..=100.0, "%", 0);
                slider(ui, "Yellow:", &mut a[2], -100.0..=100.0, "%", 0);
                slider(ui, "Black:", &mut a[3], -100.0..=100.0, "%", 0);
            });
            ui.horizontal(|ui| {
                option_label(ui, "Method:");
                ui.radio_value(relative, true, "Relative");
                ui.radio_value(relative, false, "Absolute");
            });
        }
        Adj::ShadowsHighlights(p) => {
            ui.strong("Shadows");
            grid(ui, "sh-s", &mut |ui| {
                slider(ui, "Amount:", &mut p.shadow_amount, 0.0..=100.0, "%", 0);
                slider(ui, "Tone:", &mut p.shadow_tone, 0.0..=100.0, "%", 0);
                slider(ui, "Radius:", &mut p.shadow_radius, 0.0..=500.0, " px", 0);
            });
            ui.strong("Highlights");
            grid(ui, "sh-h", &mut |ui| {
                slider(ui, "Amount:", &mut p.highlight_amount, 0.0..=100.0, "%", 0);
                slider(ui, "Tone:", &mut p.highlight_tone, 0.0..=100.0, "%", 0);
                slider(ui, "Radius:", &mut p.highlight_radius, 0.0..=500.0, " px", 0);
            });
            ui.strong("Adjustments");
            grid(ui, "sh-a", &mut |ui| {
                slider(ui, "Color:", &mut p.color, -100.0..=100.0, "", 0);
                slider(ui, "Midtone:", &mut p.midtone, -100.0..=100.0, "", 0);
                slider(ui, "Black Clip:", &mut p.black_clip, 0.0..=50.0, "%", 2);
                slider(ui, "White Clip:", &mut p.white_clip, 0.0..=50.0, "%", 2);
            });
        }
        Adj::HdrToning(p) => {
            ui.strong("Edge Glow");
            grid(ui, "hdr-e", &mut |ui| {
                slider(ui, "Radius:", &mut p.radius, 1.0..=500.0, " px", 0);
                slider(ui, "Strength:", &mut p.strength, 0.1..=4.0, "", 2);
            });
            ui.strong("Tone and Detail");
            grid(ui, "hdr-t", &mut |ui| {
                slider(ui, "Gamma:", &mut p.gamma, 0.1..=2.0, "", 2);
                slider(ui, "Exposure:", &mut p.exposure, -5.0..=5.0, "", 2);
                slider(ui, "Detail:", &mut p.detail, -100.0..=300.0, "%", 0);
            });
            ui.strong("Advanced");
            grid(ui, "hdr-a", &mut |ui| {
                slider(ui, "Shadow:", &mut p.shadow, -100.0..=100.0, "%", 0);
                slider(ui, "Highlight:", &mut p.highlight, -100.0..=100.0, "%", 0);
                slider(ui, "Vibrance:", &mut p.vibrance, -100.0..=100.0, "%", 0);
                slider(ui, "Saturation:", &mut p.saturation, -100.0..=100.0, "%", 0);
            });
        }
        Adj::MatchColor { luminance, intensity, fade, neutralize } => {
            grid(ui, "mc", &mut |ui| {
                slider(ui, "Luminance:", luminance, 1.0..=200.0, "", 0);
                slider(ui, "Color Intensity:", intensity, 1.0..=200.0, "", 0);
                slider(ui, "Fade:", fade, 0.0..=100.0, "", 0);
            });
            widgets::checkbox(ui, neutralize, "Neutralize");
            note(ui, "Source: this picture's own colours (Neutralize removes a colour cast).");
        }
        Adj::ReplaceColor { color, fuzziness, hue, saturation, lightness } => {
            ui.horizontal(|ui| {
                option_label(ui, "Color:");
                color3(ui, color);
                if widgets::secondary_button(ui, "Eyedropper Colour", 0.0).on_hover_text("Use the colour picked with the Eyedropper tool (I)").clicked() {
                    *color = adjust::rgb3([eye.r(), eye.g(), eye.b()]);
                }
            });
            grid(ui, "rc", &mut |ui| {
                slider(ui, "Fuzziness:", fuzziness, 0.0..=200.0, "", 0);
                slider(ui, "Hue:", hue, -180.0..=180.0, "°", 0);
                slider(ui, "Saturation:", saturation, -100.0..=100.0, "", 0);
                slider(ui, "Lightness:", lightness, -100.0..=100.0, "", 0);
            });
            ui.horizontal(|ui| {
                option_label(ui, "Result:");
                let (r, _) = ui.allocate_exact_size(vec2(36.0, 18.0), Sense::hover());
                ui.painter().rect_filled(r, 3.0, swatch(adjust::replace_color_result(*color, *hue, *saturation, *lightness)));
            });
        }
        Adj::Invert | Adj::Desaturate | Adj::Equalize | Adj::AutoTone | Adj::AutoContrast | Adj::AutoColor => {}
    }
    if d.sel.is_some() {
        ui.add_space(4.0);
        note(ui, "Applies inside the pixel selection.");
    }
}

#[cfg(test)]
impl App {
    pub(crate) fn adjust_settings_for_test(&mut self) -> Option<&mut Adj> {
        self.adjust_dlg.as_mut().map(|d| &mut d.adj)
    }
    pub(crate) fn live_pixel_for_test(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.rlive.as_ref().map(|l| l.raster.get(x, y))
    }
}

impl App {
    /// Debug screenshots (PDFFLOW_SHOT_ADJUST): open a dialog with sample settings.
    pub(super) fn shot_adjustment(&mut self, ctx: &egui::Context, which: &str) {
        let kind = match which {
            "levels" => AdjKind::Levels,
            "hue" => AdjKind::HueSaturation,
            _ => AdjKind::Curves,
        };
        self.start_adjust(ctx, kind);
        if let Some(d) = &mut self.adjust_dlg {
            match &mut d.adj {
                Adj::Curves { ch } => {
                    ch[0] = vec![[0.0, 0.0], [0.25, 0.18], [0.75, 0.86], [1.0, 1.0]];
                    d.point = Some(2);
                }
                Adj::Levels { ch } => ch[0] = LevelsCh { in_black: 30.0 / 255.0, in_white: 225.0 / 255.0, gamma: 1.25, ..Default::default() },
                Adj::HueSaturation { hue, saturation, .. } => {
                    *hue = 40.0;
                    *saturation = 30.0;
                }
                _ => {}
            }
        }
    }
}
