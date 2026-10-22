//! Select Text and Snapshot. Select Text selects the text already in the PDF (drag along the
//! text, Alt-drag a column box, double-click a word, triple-click a line), copies it, and marks
//! it up as highlight, underline or strikethrough objects. Snapshot copies an area of a page,
//! edits included, as a picture: to the system clipboard, to PDFflow's own clipboard (⌘V pastes
//! it as a picture) and, on request, to a PNG or JPEG file.

use super::*;
use crate::textlayer::PageText;

/// Selected glyphs of one page's text (page = editor page index, glyphs in page order).
#[derive(Clone)]
pub(crate) struct TextSel {
    pub page: usize,
    pub glyphs: Vec<usize>,
}

/// The last snapshot.
pub(crate) struct Snap {
    pub img: Arc<image::RgbaImage>,
    pub page: usize,
    /// The captured area in page points.
    pub rect: Rect,
    pub dpi: f32,
    pub tex: Option<TextureHandle>,
    pub open: bool,
}

/// How selected text is marked up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Markup {
    Highlight,
    Underline,
    Strikeout,
}

pub(crate) const SNAP_DPI: [f32; 4] = [72.0, 150.0, 300.0, 600.0];

impl App {
    /// The text of editor page `page`, extracted the first time it is asked for.
    pub(crate) fn page_text(&mut self, page: usize) -> Option<Arc<PageText>> {
        let PageSrc::Original(src) = self.doc.pages.get(page)?.src else { return None };
        if let Some(t) = self.text_cache.get(&src) {
            return Some(t.clone());
        }
        let bytes = self.doc.source.clone()?;
        let t = Arc::new(crate::textlayer::extract(bytes, &[src]).ok()?.pop().map(|p| p.1).unwrap_or_default());
        self.text_cache.insert(src, t.clone());
        Some(t)
    }

    /// The text of a page only if it is already extracted (for hover feedback).
    pub(super) fn cached_text(&self, page: usize) -> Option<Arc<PageText>> {
        match self.doc.pages.get(page)?.src {
            PageSrc::Original(src) => self.text_cache.get(&src).cloned(),
            PageSrc::Blank => None,
        }
    }

    pub(crate) fn selected_text(&mut self) -> Option<String> {
        let sel = self.text_sel.clone()?;
        Some(self.page_text(sel.page)?.text_of(&sel.glyphs))
    }

    pub(super) fn text_press(&mut self, hit: Option<(usize, Pos2)>, column: bool, extend: bool) {
        let Some((page, p)) = hit else {
            self.text_sel = None;
            return;
        };
        let Some(t) = self.page_text(page) else {
            self.text_sel = None;
            self.status = "This page has no PDF text to select (a blank page).".into();
            return;
        };
        if t.is_empty() {
            self.text_sel = None;
            self.status = "This page has no selectable text; it may be a scan. Use Snapshot to copy it as a picture.".into();
            return;
        }
        // Shift-click extends the selection from its start.
        let anchor = match (&self.text_sel, extend) {
            (Some(s), true) if s.page == page && !column => s.glyphs.first().copied(),
            _ => t.caret_at(p),
        };
        if !extend {
            self.text_sel = None;
        }
        self.drag = Some(Drag::TextSel { page, anchor, start: p, column });
        if extend {
            self.text_drag(p);
        }
    }

    pub(super) fn text_drag(&mut self, p: Pos2) {
        let Some(Drag::TextSel { page, anchor, start, column }) = &self.drag else { return };
        let (page, anchor, start, column) = (*page, *anchor, *start, *column);
        let Some(t) = self.page_text(page) else { return };
        let glyphs = if column {
            t.in_rect(Rect::from_two_pos(start, p))
        } else {
            match (anchor, t.caret_at(p)) {
                (Some(a), Some(b)) if a != b => (a.min(b)..a.max(b)).collect(),
                _ => Vec::new(),
            }
        };
        self.text_sel = (!glyphs.is_empty()).then_some(TextSel { page, glyphs });
        self.note_selection();
    }

    pub(super) fn select_text_at(&mut self, page: usize, p: Pos2, line: bool) {
        let Some(t) = self.page_text(page) else { return };
        let glyphs = if line { t.line_at(p) } else { t.word_at(p) };
        self.text_sel = glyphs.map(|glyphs| TextSel { page, glyphs });
        self.note_selection();
    }

    pub(crate) fn select_all_text(&mut self, page: usize) {
        let Some(t) = self.page_text(page) else { return };
        self.text_sel = (!t.is_empty()).then(|| TextSel { page, glyphs: (0..t.glyphs.len()).collect() });
        self.note_selection();
    }

    fn note_selection(&mut self) {
        if let Some(s) = self.selected_text() {
            let n = s.chars().filter(|c| !c.is_whitespace()).count();
            self.status = format!("{n} characters selected. ⌘C copies them.");
        }
    }

    pub(super) fn copy_text_selection(&mut self, ctx: &egui::Context) {
        if let Some(s) = self.selected_text() {
            let n = s.chars().count();
            ctx.copy_text(s);
            self.status = format!("Copied {n} characters.");
        }
    }

    /// Mark the selected text up as objects (one undo step); returns how many were added.
    pub(crate) fn markup_selection(&mut self, kind: Markup, color: Color32) -> usize {
        let Some(sel) = self.text_sel.clone() else { return 0 };
        let Some(t) = self.page_text(sel.page) else { return 0 };
        let objs = markup_objects(&t, &sel.glyphs, kind, color);
        let n = objs.len();
        if n > 0 {
            self.doc.checkpoint();
            self.doc.pages[sel.page].objects.extend(objs);
            self.status = format!("{} added on {n} line{}.", match kind { Markup::Highlight => "Highlight", Markup::Underline => "Underline", Markup::Strikeout => "Strikethrough" }, if n == 1 { "" } else { "s" });
        }
        n
    }

    /// The selection, a column box being dragged and a snapshot box being dragged.
    pub(super) fn text_overlay(&self, painter: &egui::Painter, rects: &[Rect]) {
        let t = Tokens::get(painter.ctx());
        if let Some(sel) = &self.text_sel {
            if let (Some(text), Some(r), Some(pg)) = (self.cached_text(sel.page), rects.get(sel.page), self.doc.pages.get(sel.page)) {
                for (run, ..) in text.runs(&sel.glyphs) {
                    painter.rect_filled(screen_rect(pg, r.min, self.zoom, run), 1.0, Color32::from_rgba_unmultiplied(40, 120, 255, 70));
                }
            }
        }
        let boxed = match &self.drag {
            Some(Drag::TextSel { page, start, column: true, .. }) => {
                let cur = painter.ctx().input(|i| i.pointer.hover_pos()).map(|p| self.to_page(rects, *page, p));
                cur.map(|c| (*page, Rect::from_two_pos(*start, c)))
            }
            Some(Drag::Snap { page, start, cur }) => Some((*page, Rect::from_two_pos(*start, *cur))),
            _ => None,
        };
        if let Some((page, r)) = boxed {
            if let (Some(pr), Some(pg)) = (rects.get(page), self.doc.pages.get(page)) {
                let sr = screen_rect(pg, pr.min, self.zoom, r);
                painter.rect_filled(sr, 0.0, t.accent.gamma_multiply(0.10));
                painter.extend(Shape::dashed_line(&[sr.left_top(), sr.right_top(), sr.right_bottom(), sr.left_bottom(), sr.left_top()], Stroke::new(1.0, t.accent), 5.0, 3.0));
                if matches!(self.drag, Some(Drag::Snap { .. })) {
                    let px = r.size() * self.snap_dpi / 72.0;
                    painter.text(sr.right_bottom() + vec2(4.0, 4.0), egui::Align2::LEFT_TOP, format!("{:.0} × {:.0} px", px.x, px.y), FontId::proportional(11.0), t.accent);
                }
            }
        }
    }

    pub(super) fn text_options(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let has = self.text_sel.is_some();
        let ctx = ui.ctx().clone();
        if ui.add_enabled(has, egui::Button::new("Copy")).on_hover_text("Copy the selected text (⌘C)").clicked() {
            self.copy_text_selection(&ctx);
        }
        if ui.button("Select All").on_hover_text("All text on this page (⌘A)").clicked() {
            self.select_all_text(self.current);
        }
        vline(ui, 22.0);
        option_label(ui, "Mark up:");
        color_row(ui, &mut self.opts.hl_color);
        for (kind, label) in [(Markup::Highlight, "Highlight"), (Markup::Underline, "Underline"), (Markup::Strikeout, "Strikethrough")] {
            if ui.add_enabled(has, egui::Button::new(label)).clicked() {
                let c = if kind == Markup::Highlight { self.opts.hl_color } else { self.opts.color };
                self.markup_selection(kind, c);
            }
        }
        vline(ui, 22.0);
        ui.label(egui::RichText::new("Drag over text to select it, Alt-drag for a column, double-click a word.").color(t.text_faint));
    }

    pub(super) fn snapshot_options(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        option_label(ui, "Resolution:");
        egui::ComboBox::from_id_salt("snap-dpi").selected_text(format!("{:.0} dpi", self.snap_dpi)).width(84.0).show_ui(ui, |ui| {
            for d in SNAP_DPI {
                ui.selectable_value(&mut self.snap_dpi, d, format!("{d:.0} dpi"));
            }
        });
        vline(ui, 22.0);
        if let Some(s) = &self.snap {
            ui.label(egui::RichText::new(format!("Last: {} × {} px", s.img.width(), s.img.height())).color(t.text_dim));
            if ui.button("Save…").clicked() {
                self.save_snapshot_dialog();
            }
            if ui.button("Show").clicked() {
                if let Some(s) = &mut self.snap {
                    s.open = true;
                }
            }
            vline(ui, 22.0);
        }
        ui.label(egui::RichText::new("Drag around an area (or click for the whole page) to copy it as a picture; ⌘V pastes it.").color(t.text_faint));
    }

    /// Capture `rect` (page points) of `page` with its edits, put it on both clipboards and
    /// show it.
    pub(crate) fn take_snapshot(&mut self, ctx: &egui::Context, page: usize, rect: Rect) -> Option<()> {
        let dpi = self.snap_dpi;
        let img = match self.snapshot_image(page, rect, dpi) {
            Ok(i) => i,
            Err(e) => {
                self.error = Some(format!("Snapshot failed: {e}"));
                return None;
            }
        };
        let img = Arc::new(img);
        ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()));
        self.clip_picture(ctx, &img, rect);
        self.snap = Some(Snap { img: img.clone(), page, rect, dpi, tex: None, open: true });
        self.status = format!("Snapshot copied ({} × {} px at {dpi:.0} dpi). ⌘V pastes it as a picture.", img.width(), img.height());
        Some(())
    }

    /// The picture of `rect` (page points) on `page` at `dpi`, edits included.
    pub(crate) fn snapshot_image(&self, page: usize, rect: Rect, dpi: f32) -> Result<image::RgbaImage, String> {
        let pg = self.doc.pages.get(page).ok_or("No such page.")?;
        let rect = rect.intersect(Rect::from_min_size(Pos2::ZERO, pg.size));
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return Err("The area is empty.".into());
        }
        let view = Rect::from_two_pos(pg.to_view(rect.min), pg.to_view(rect.max));
        let (full, scale) = crate::snapshot::render_page(&self.doc, page, dpi / 72.0)?;
        crate::snapshot::crop(&full, view, scale).ok_or_else(|| "The area is empty.".to_owned())
    }

    /// Put a picture on PDFflow's clipboard, sized as the area it came from.
    pub(crate) fn clip_picture(&mut self, ctx: &egui::Context, img: &image::RgbaImage, rect: Rect) {
        let Ok(png) = crate::snapshot::encode(img, image::ImageFormat::Png) else { return };
        let preview = if img.width().max(img.height()) > 1024 { image::DynamicImage::ImageRgba8(img.clone()).thumbnail(1024, 1024).to_rgba8() } else { img.clone() };
        let ci = egui::ColorImage::from_rgba_unmultiplied([preview.width() as usize, preview.height() as usize], preview.as_raw());
        let tex = ctx.load_texture("picture", ci, TextureOptions::LINEAR);
        let data = Arc::new(ImageData::new(Arc::new(png), [img.width(), img.height()], Some(tex)));
        // Paste offsets by 12 pt, so start 12 pt up and left to land on the captured area.
        self.clipboard = Some(Obj { rect: rect.translate(vec2(-12.0, -12.0)), kind: Kind::Image(data) });
    }

    pub(super) fn save_snapshot_dialog(&mut self) {
        let Some(s) = &self.snap else { return };
        let stem = self.doc.title().trim_end_matches(".pdf").to_owned();
        let name = format!("{stem}-page{}-snapshot.png", s.page + 1);
        let Some(mut path) = rfd::FileDialog::new().add_filter("PNG", &["png"]).add_filter("JPEG", &["jpg", "jpeg"]).set_file_name(name).save_file() else { return };
        if path.extension().is_none() {
            path.set_extension("png");
        }
        let img = s.img.clone();
        match crate::snapshot::encode(&img, crate::snapshot::format_for(&path)).and_then(|b| std::fs::write(&path, b).map_err(|e| e.to_string())) {
            Ok(()) => self.status = format!("Snapshot saved to {}.", path.display()),
            Err(e) => self.error = Some(format!("Could not save the snapshot: {e}")),
        }
    }

    /// A small window with the last snapshot and what can be done with it.
    pub(super) fn snapshot_window(&mut self, ctx: &egui::Context) {
        let Some(s) = &mut self.snap else { return };
        if !s.open {
            return;
        }
        let tex = s
            .tex
            .get_or_insert_with(|| {
                let img = &s.img;
                let p = if img.width().max(img.height()) > 640 { image::DynamicImage::ImageRgba8(img.as_ref().clone()).thumbnail(640, 640).to_rgba8() } else { img.as_ref().clone() };
                ctx.load_texture("snapshot", egui::ColorImage::from_rgba_unmultiplied([p.width() as usize, p.height() as usize], p.as_raw()), TextureOptions::LINEAR)
            })
            .clone();
        let (w, h, dpi, page) = (s.img.width(), s.img.height(), s.dpi, s.page);
        let mut open = true;
        let mut action = None;
        egui::Window::new("Snapshot").open(&mut open).collapsible(false).resizable(false).default_pos(ctx.content_rect().right_top() + vec2(-300.0, 90.0)).show(ctx, |ui| {
            let t = Tokens::get(ui.ctx());
            let fit = (260.0 / w as f32).min(200.0 / h as f32).min(1.0);
            let size = vec2(w as f32, h as f32) * fit;
            let (r, _) = ui.allocate_exact_size(size, Sense::hover());
            ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, t.separator), StrokeKind::Outside);
            ui.label(egui::RichText::new(format!("Page {} · {w} × {h} px · {dpi:.0} dpi\nCopied to the clipboard; ⌘V pastes it here as a picture.", page + 1)).color(t.text_dim).size(11.0));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if widgets::primary_button(ui, "Save…", 70.0).clicked() {
                    action = Some(0);
                }
                if widgets::secondary_button(ui, "Copy", 60.0).clicked() {
                    action = Some(1);
                }
                if widgets::secondary_button(ui, "Place on Page", 100.0).clicked() {
                    action = Some(2);
                }
            });
        });
        match action {
            Some(0) => self.save_snapshot_dialog(),
            Some(1) => {
                if let Some(s) = &self.snap {
                    let img = s.img.clone();
                    let rect = s.rect;
                    ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()));
                    self.clip_picture(ctx, &img, rect);
                    self.status = "Snapshot copied again.".into();
                }
            }
            Some(2) => {
                self.paste();
                if let Some(s) = &mut self.snap {
                    s.open = false;
                }
                return;
            }
            _ => {}
        }
        if !open {
            if let Some(s) = &mut self.snap {
                s.open = false;
            }
        }
    }
}

/// Highlight, underline or strikethrough objects for the glyphs `idx`, one per line run.
pub(crate) fn markup_objects(t: &PageText, idx: &[usize], kind: Markup, color: Color32) -> Vec<Obj> {
    let mut out = Vec::new();
    for (rect, a, b, size) in t.runs(idx) {
        let dir = if (b - a).length() > 1e-3 { (b - a).normalized() } else { vec2(1.0, 0.0) };
        // Up from the baseline, on the page (y grows downwards).
        let up = vec2(dir.y, -dir.x);
        match kind {
            Markup::Highlight => {
                // The stroke runs along the middle of the line, as wide as the line is tall.
                let mid = up * (size * 0.34);
                let w = (size * 1.12).max(rect.height().min(size * 1.6));
                // The stroke has round caps (as in the saved PDF); pull its ends in so the caps
                // stop just past the first and last letters.
                let inset = dir * (w * 0.35).min((b - a).length() * 0.45);
                let (r, unit) = ink_from_points(&[vec![a + mid + inset, b + mid - inset]], w / 2.0);
                out.push(Obj { rect: r, kind: Kind::Ink { strokes: Arc::new(unit), color, width: w, highlighter: true } });
            }
            Markup::Underline | Markup::Strikeout => {
                let off = if kind == Markup::Underline { -up * (size * 0.12) } else { up * (size * 0.3) };
                let width = (size * 0.06).clamp(0.5, 3.0);
                let mut o = Obj { rect: Rect::NOTHING, kind: Kind::Shape { shape: ShapeKind::Line, stroke: color, fill: None, width, a: Pos2::ZERO, b: Pos2::ZERO } };
                set_line(&mut o, a + off, b + off);
                out.push(o);
            }
        }
    }
    out
}
