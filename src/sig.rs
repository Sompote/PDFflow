//! Saved signatures: drawn once in a pad, stored as unit-square strokes, reused on any page.

use crate::theme::Tokens;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct Signature {
    /// Width / height of the drawn box.
    pub aspect: f32,
    pub strokes: Arc<Vec<Vec<Pos2>>>,
}

fn store_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/PDFEditor"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("PDFEditor"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|c| c.join("pdfeditor"))
    };
    base.map(|b| b.join("signatures.txt"))
}

/// One signature per line: `aspect|x,y x,y;x,y ...` with strokes separated by `;`.
pub fn load() -> Vec<Signature> {
    let Some(text) = store_path().and_then(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let (aspect, rest) = line.split_once('|')?;
            let strokes = rest
                .split(';')
                .map(|s| {
                    s.split_whitespace()
                        .filter_map(|p| {
                            let (x, y) = p.split_once(',')?;
                            Some(pos2(x.parse().ok()?, y.parse().ok()?))
                        })
                        .collect::<Vec<_>>()
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            (!strokes.is_empty()).then(|| Signature { aspect: aspect.parse().unwrap_or(3.0), strokes: Arc::new(strokes) })
        })
        .collect()
}

pub fn save(sigs: &[Signature]) {
    let Some(path) = store_path() else { return };
    let mut out = String::new();
    for s in sigs {
        out.push_str(&format!("{:.4}|", s.aspect));
        let parts: Vec<String> =
            s.strokes.iter().map(|st| st.iter().map(|p| format!("{:.4},{:.4}", p.x, p.y)).collect::<Vec<_>>().join(" ")).collect();
        out.push_str(&parts.join(";"));
        out.push('\n');
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, out);
}

/// Paint a signature's strokes into a screen rect, keeping its aspect ratio.
pub fn paint(painter: &egui::Painter, rect: Rect, sig: &Signature, color: Color32, width: f32) {
    let r = fit(rect, sig.aspect);
    for st in sig.strokes.iter() {
        let pts: Vec<Pos2> = st.iter().map(|p| pos2(r.min.x + p.x * r.width(), r.min.y + p.y * r.height())).collect();
        if pts.len() == 1 {
            painter.circle_filled(pts[0], width * 0.5, color);
        } else {
            painter.add(Shape::line(pts, Stroke::new(width, color)));
        }
    }
}

fn fit(rect: Rect, aspect: f32) -> Rect {
    let w = rect.width().min(rect.height() * aspect);
    Rect::from_center_size(rect.center(), vec2(w, w / aspect))
}

/// The drawing pad shown in the "New Signature" dialog.
#[derive(Default)]
pub struct Pad {
    strokes: Vec<Vec<Pos2>>,
    current: Vec<Pos2>,
}

impl Pad {
    pub fn ui(&mut self, ui: &mut Ui, color: Color32) {
        let t = Tokens::get(ui.ctx());
        let (rect, resp) = ui.allocate_exact_size(vec2(520.0, 200.0), Sense::drag());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, t.radius, Color32::WHITE);
        let base = rect.bottom() - 48.0;
        p.line_segment([pos2(rect.left() + 24.0, base), pos2(rect.right() - 24.0, base)], Stroke::new(1.0, Color32::from_gray(200)));
        p.text(pos2(rect.left() + 24.0, base + 6.0), egui::Align2::LEFT_TOP, "Sign above the line", egui::FontId::proportional(11.0), Color32::from_gray(150));
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.dragged() || resp.drag_started() {
                let local = pos - rect.min;
                if self.current.last().is_none_or(|l| (local - l.to_vec2()).length() > 0.8) {
                    self.current.push(local.to_pos2());
                }
            }
        }
        if resp.drag_stopped() && !self.current.is_empty() {
            self.strokes.push(std::mem::take(&mut self.current));
        }
        for st in self.strokes.iter().chain(std::iter::once(&self.current)) {
            let pts: Vec<Pos2> = st.iter().map(|q| rect.min + q.to_vec2()).collect();
            if pts.len() == 1 {
                p.circle_filled(pts[0], 1.4, color);
            } else if pts.len() > 1 {
                p.add(Shape::line(pts, Stroke::new(2.6, color)));
            }
        }
        if self.is_empty() {
            p.text(rect.center() - vec2(0.0, 20.0), egui::Align2::CENTER_CENTER, "Draw your signature here", egui::FontId::proportional(15.0), Color32::from_gray(170));
        }
        resp.on_hover_cursor(egui::CursorIcon::Crosshair);
    }

    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.current.is_empty()
    }

    pub fn clear(&mut self) {
        self.strokes.clear();
        self.current.clear();
    }

    /// Crop to the drawn strokes and normalise them to a unit square.
    pub fn finish(&self) -> Option<Signature> {
        let (r, strokes) = crate::doc::ink_from_points(&self.strokes, 2.0);
        if self.strokes.is_empty() || !r.is_finite() {
            return None;
        }
        let size: Vec2 = r.size().max(vec2(1.0, 1.0));
        Some(Signature { aspect: size.x / size.y, strokes: Arc::new(strokes) })
    }
}
