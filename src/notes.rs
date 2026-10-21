//! Comments: sticky notes anchored to a point on a page, saved as PDF Text annotations, so
//! Acrobat, Preview and other readers show them as comments.

use super::*;
use crate::doc::{NOTE_SIZE, note_key};
use lopdf::Object as PdfObject;

/// Colours offered for comments (the Acrobat set).
pub(super) const NOTE_COLORS: [Color32; 5] = [
    Color32::from_rgb(255, 214, 10),
    Color32::from_rgb(120, 200, 90),
    Color32::from_rgb(90, 170, 240),
    Color32::from_rgb(240, 120, 170),
    Color32::from_rgb(250, 150, 60),
];

pub(super) struct NoteEdit {
    pub page: usize,
    pub idx: usize,
    pub before: Vec<PageEntry>,
    pub original: (String, String),
    pub fresh: bool,
    pub focus: bool,
}

fn author_file() -> Option<PathBuf> {
    crate::sig::config_dir().map(|d| d.join("author.txt"))
}

/// The name written on new comments (saved between runs; the login name at first).
pub(super) fn default_author() -> String {
    author_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("USER").or_else(|_| std::env::var("USERNAME")).ok())
        .unwrap_or_else(|| "Me".into())
}

fn save_author(a: &str) {
    if let Some(p) = author_file() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, a);
    }
}

/// "D:YYYYMMDDHHmmSSZ" for now.
pub(super) fn now_pdf_date() -> String {
    crate::dsig::pdf_date(std::time::SystemTime::now()).trim_matches(|c| c == '(' || c == ')').to_owned()
}

/// Read the comments (Text annotations) of every page, in display coordinates.
pub(super) fn read_comments(bytes: &[u8]) -> Vec<Vec<Obj>> {
    let Ok(pdf) = lopdf::Document::load_mem(bytes) else { return Vec::new() };
    let mut out = Vec::new();
    for (_, page_id) in pdf.get_pages() {
        let mut notes = Vec::new();
        let (b, rot) = export::page_frame(&pdf, page_id);
        let m = export::display_matrix(b, rot);
        let annots = pdf.get_dictionary(page_id).ok().and_then(|d| d.get(b"Annots").ok()).and_then(|o| pdf.dereference(o).ok()).and_then(|(_, o)| o.as_array().ok().cloned()).unwrap_or_default();
        for a in annots {
            let Ok((_, PdfObject::Dictionary(d))) = pdf.dereference(&a) else { continue };
            if d.get(b"Subtype").and_then(PdfObject::as_name).ok() != Some(b"Text".as_slice()) {
                continue;
            }
            let Some(r) = d.get(b"Rect").ok().and_then(|o| export::rect_of(&pdf, o)) else { continue };
            let text_of = |k: &[u8]| d.get(k).ok().and_then(|o| o.as_str().ok()).map(crate::dsig::decode_text).unwrap_or_default();
            // The icon hangs from the annotation's top-left corner in display space.
            let corners = [export::point_to_display(m, r[0], r[1]), export::point_to_display(m, r[2], r[3]), export::point_to_display(m, r[0], r[3]), export::point_to_display(m, r[2], r[1])];
            let x = corners.iter().map(|c| c.0).fold(f32::MAX, f32::min);
            let y = corners.iter().map(|c| c.1).fold(f32::MAX, f32::min);
            let color = d
                .get(b"C")
                .ok()
                .and_then(|c| c.as_array().ok())
                .filter(|c| c.len() == 3)
                .map(|c| {
                    let v = |i: usize| (c[i].as_float().unwrap_or(1.0).clamp(0.0, 1.0) * 255.0) as u8;
                    Color32::from_rgb(v(0), v(1), v(2))
                })
                .unwrap_or(NOTE_COLORS[0]);
            let date = d.get(b"M").ok().and_then(|o| o.as_str().ok()).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
            notes.push(Obj {
                rect: Rect::from_min_size(pos2(x, y), vec2(NOTE_SIZE, NOTE_SIZE)),
                kind: Kind::Note { text: text_of(b"Contents").replace('\r', "\n"), author: text_of(b"T"), date, color },
            });
        }
        out.push(notes);
    }
    out
}

/// The same PDF without its comments, for the page renderer: the editor draws (and moves)
/// comments itself, so the original icons must not stay painted underneath.
pub(super) fn without_comments(bytes: &[u8]) -> Option<Vec<u8>> {
    let pdf = lopdf::Document::load_mem(bytes).ok()?;
    if pdf.is_encrypted() {
        return None;
    }
    let mut objects = Vec::new();
    for (_, page_id) in pdf.get_pages() {
        let Ok(page) = pdf.get_dictionary(page_id) else { continue };
        let Ok(a) = page.get(b"Annots") else { continue };
        let Ok((_, PdfObject::Array(arr))) = pdf.dereference(a) else { continue };
        let kept: Vec<PdfObject> = arr.iter().filter(|x| !export::is_comment_annot(&pdf, x)).cloned().collect();
        if kept.len() == arr.len() {
            continue;
        }
        let mut p = page.clone();
        p.set("Annots", kept);
        objects.push((page_id, crate::dsig::ser(&PdfObject::Dictionary(p)).ok()?));
    }
    if objects.is_empty() {
        return None;
    }
    crate::dsig::append_update(bytes, &pdf, objects).ok()
}

impl App {
    /// Load the source's comments as editable notes and hide them from the page renderer.
    pub(super) fn import_comments(&mut self, bytes: &[u8]) {
        let pages = read_comments(bytes);
        for (i, notes) in pages.into_iter().enumerate() {
            if let Some(p) = self.doc.pages.get_mut(i) {
                self.doc.source_notes[i] = notes.iter().filter_map(note_key).collect();
                p.objects = notes;
            }
        }
    }

    /// Comment tool: drop a note where clicked and open it for typing.
    pub(super) fn place_note(&mut self, page: usize, at: Pos2) {
        let before = self.doc.pages.clone();
        let size = self.doc.pages[page].size;
        let min = pos2(at.x.clamp(0.0, size.x - NOTE_SIZE), at.y.clamp(0.0, size.y - NOTE_SIZE));
        let o = Obj { rect: Rect::from_min_size(min, vec2(NOTE_SIZE, NOTE_SIZE)), kind: Kind::Note { text: String::new(), author: self.author.clone(), date: now_pdf_date(), color: self.note_color } };
        self.doc.pages[page].objects.push(o);
        let idx = self.doc.pages[page].objects.len() - 1;
        self.sel = Some((page, idx));
        self.note_edit = Some(NoteEdit { page, idx, before, original: (String::new(), self.author.clone()), fresh: true, focus: true });
    }

    pub(super) fn open_note(&mut self, page: usize, idx: usize) {
        self.close_note();
        let Some(Obj { kind: Kind::Note { text, author, .. }, .. }) = self.doc.pages.get(page).and_then(|p| p.objects.get(idx)) else { return };
        let original = (text.clone(), author.clone());
        self.sel = Some((page, idx));
        self.note_edit = Some(NoteEdit { page, idx, before: self.doc.pages.clone(), original, fresh: false, focus: true });
    }

    /// Close the note pop-up: an empty new note disappears, a changed one is one undo step.
    pub(super) fn close_note(&mut self) {
        let Some(ed) = self.note_edit.take() else { return };
        let Some(Obj { kind: Kind::Note { text, author, .. }, .. }) = self.doc.pages.get(ed.page).and_then(|p| p.objects.get(ed.idx)) else { return };
        let (text, author) = (text.clone(), author.clone());
        if text.trim().is_empty() && ed.fresh {
            self.doc.pages[ed.page].objects.remove(ed.idx);
            self.sel = None;
            return;
        }
        if ed.fresh || (text, author.clone()) != ed.original {
            self.doc.push_undo(ed.before);
        }
        if author != self.author && !author.trim().is_empty() {
            self.author = author;
            save_author(&self.author);
        }
    }

    /// The open note's pop-up, next to its icon.
    pub(super) fn note_popup(&mut self, ctx: &egui::Context, rects: &[Rect]) {
        let Some(ed) = &mut self.note_edit else { return };
        let (page, idx) = (ed.page, ed.idx);
        let Some(obj) = self.doc.pages.get(page).and_then(|p| p.objects.get(idx)) else {
            self.note_edit = None;
            return;
        };
        let Kind::Note { text, author, date, color } = &obj.kind else { return };
        let (mut text, mut author, date, color) = (text.clone(), author.clone(), date.clone(), *color);
        let pg = &self.doc.pages[page];
        let icon = screen_rect(pg, rects[page].min, self.zoom, obj.rect);
        let t = Tokens::get(ctx);
        let focus = std::mem::take(&mut ed.focus);
        let mut close = false;
        let mut delete = false;
        let area = egui::Area::new(Id::new("note-popup")).order(egui::Order::Foreground).fixed_pos(if icon.right() + 300.0 > self.view.right() { icon.left_top() - vec2(298.0, 0.0) } else { icon.right_top() + vec2(8.0, 0.0) }).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).fill(t.card).show(ui, |ui| {
                ui.set_width(270.0);
                let (bar, _) = ui.allocate_exact_size(vec2(270.0, 4.0), Sense::hover());
                ui.painter().rect_filled(bar, 2.0, color);
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut author).desired_width(130.0).hint_text("Author"));
                    ui.label(egui::RichText::new(readable_date(&date)).color(t.text_faint).size(10.5));
                });
                let r = ui.add(egui::TextEdit::multiline(&mut text).desired_rows(4).desired_width(270.0).hint_text("Add a comment…").font(FontId::new(13.0, theme::doc_family(false))));
                if focus {
                    r.request_focus();
                }
                ui.horizontal(|ui| {
                    if icons::small(ui, "trash", "Delete comment").clicked() {
                        delete = true;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::primary_button(ui, "Done", 64.0).clicked() {
                            close = true;
                        }
                    });
                });
            });
        });
        if let Some(Obj { kind: Kind::Note { text: t0, author: a0, .. }, .. }) = self.doc.pages[page].objects.get_mut(idx) {
            if *t0 != text || *a0 != author {
                *t0 = text;
                *a0 = author;
                self.doc.dirty = true;
            }
        }
        if delete {
            let ed = self.note_edit.take().expect("open note");
            self.doc.pages[page].objects.remove(idx);
            self.sel = None;
            if !ed.fresh {
                self.doc.push_undo(ed.before);
            }
            return;
        }
        let clicked_out = ctx.input(|i| i.pointer.any_pressed()) && ctx.pointer_interact_pos().is_some_and(|p| !area.response.rect.contains(p) && !icon.contains(p));
        if close || clicked_out || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.close_note();
        }
    }

    /// The Comments tab: every comment in the document, in page order.
    pub(super) fn comments_panel(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let mut open = None;
        let count: usize = self.doc.pages.iter().map(|p| p.objects.iter().filter(|o| matches!(o.kind, Kind::Note { .. })).count()).sum();
        egui::ScrollArea::vertical().id_salt("comments").auto_shrink([false, false]).show(ui, |ui| {
            if count == 0 {
                ui.label(egui::RichText::new("No comments yet. Choose the Comment tool (Z) and click a page.").color(t.text_faint));
            }
            for (pi, page) in self.doc.pages.iter().enumerate() {
                for (oi, o) in page.objects.iter().enumerate() {
                    let Kind::Note { text, author, date, color } = &o.kind else { continue };
                    let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
                    if self.sel == Some((pi, oi)) {
                        ui.painter().rect_filled(row, t.radius_sm, t.row_selected);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(row, t.radius_sm, t.hover);
                    }
                    let ir = Rect::from_min_size(row.min + vec2(4.0, 6.0), vec2(18.0, 18.0));
                    paint_note_icon(ui.painter(), ir, *color);
                    let head = format!("p.{} · {} · {}", pi + 1, if author.is_empty() { "—" } else { author }, readable_date(date));
                    ui.painter().text(pos2(row.left() + 28.0, row.top() + 10.0), egui::Align2::LEFT_CENTER, head, FontId::proportional(10.5), t.text_faint);
                    let snippet: String = text.replace('\n', " ").chars().take(46).collect();
                    let painter = ui.painter_at(row);
                    painter.text(pos2(row.left() + 28.0, row.top() + 29.0), egui::Align2::LEFT_CENTER, if snippet.is_empty() { "(empty)".into() } else { snippet }, FontId::new(12.5, theme::doc_family(false)), t.text);
                    if resp.clicked() {
                        open = Some((pi, oi));
                    }
                }
            }
        });
        if let Some((p, i)) = open {
            self.end_edit();
            self.current = p;
            let y = self.doc.pages[p].objects[i].rect.min.y;
            self.scroll_to_page(p);
            self.scroll.y += (y - 60.0).max(0.0) * self.zoom;
            self.clamp_scroll();
            self.open_note(p, i);
        }
    }
}

fn readable_date(d: &str) -> String {
    let s: String = d.trim_start_matches("D:").chars().take(12).collect();
    if s.len() == 12 && s.chars().all(|c| c.is_ascii_digit()) { format!("{}-{}-{} {}:{}", &s[0..4], &s[4..6], &s[6..8], &s[8..10], &s[10..12]) } else { d.to_owned() }
}

/// A speech-bubble comment icon in the note's colour.
pub(super) fn paint_note_icon(painter: &egui::Painter, r: Rect, color: Color32) {
    painter.rect_filled(r, r.width() * 0.18, color);
    painter.rect_stroke(r, r.width() * 0.18, Stroke::new((r.width() / 20.0).max(0.6), Color32::from_black_alpha(140)), StrokeKind::Inside);
    icons::paint(painter, r.shrink(r.width() * 0.16), "message-square-text", Color32::from_rgb(40, 40, 40));
}
