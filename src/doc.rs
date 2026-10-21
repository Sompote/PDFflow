//! The editable document: an ordered page list over an untouched source PDF, plus the objects
//! (text, images, ink, shapes) placed on each page. Coordinates are PDF points in the page's
//! own display space (top-left origin, y down, the source page's /Rotate already applied).

use eframe::egui::{Color32, Pos2, Rect, TextureHandle, Vec2, pos2, vec2};
use std::path::PathBuf;
use std::sync::Arc;

pub const A4: Vec2 = Vec2::new(595.0, 842.0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageSrc {
    /// Index of the page in the source PDF.
    Original(usize),
    Blank,
}

#[derive(Clone)]
pub struct PageEntry {
    pub src: PageSrc,
    /// Display size in points with the source page's own rotation applied.
    pub size: Vec2,
    /// Extra clockwise rotation added in the editor: 0, 90, 180 or 270.
    pub rotation: u16,
    pub objects: Vec<Obj>,
}

impl PageEntry {
    pub fn blank(size: Vec2) -> Self {
        PageEntry { src: PageSrc::Blank, size, rotation: 0, objects: Vec::new() }
    }

    /// Size as shown on screen, after the editor's rotation.
    pub fn view_size(&self) -> Vec2 {
        if self.rotation % 180 == 90 { vec2(self.size.y, self.size.x) } else { self.size }
    }

    /// Page point to view point (before zoom), applying the editor rotation.
    pub fn to_view(&self, p: Pos2) -> Pos2 {
        let (w, h) = (self.size.x, self.size.y);
        match self.rotation {
            90 => pos2(h - p.y, p.x),
            180 => pos2(w - p.x, h - p.y),
            270 => pos2(p.y, w - p.x),
            _ => p,
        }
    }

    pub fn from_view(&self, v: Pos2) -> Pos2 {
        let (w, h) = (self.size.x, self.size.y);
        match self.rotation {
            90 => pos2(v.y, h - v.x),
            180 => pos2(w - v.x, h - v.y),
            270 => pos2(w - v.y, v.x),
            _ => v,
        }
    }
}

/// A picture placed on a page. The original file bytes are kept (JPEGs go into the PDF
/// unchanged); the screen only holds a downscaled preview texture.
pub struct ImageData {
    pub bytes: Arc<Vec<u8>>,
    pub px: [u32; 2],
    pub tex: TextureHandle,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ShapeKind {
    Rect,
    Ellipse,
    Line,
    Arrow,
}

#[derive(Clone)]
pub enum Kind {
    Text { text: String, size: f32, color: Color32, bold: bool },
    Image(Arc<ImageData>),
    /// Freehand strokes, points in unit coordinates of the object rect.
    Ink { strokes: Arc<Vec<Vec<Pos2>>>, color: Color32, width: f32, highlighter: bool },
    /// Lines and arrows run from `a` to `b`, both in unit coordinates of the object rect.
    Shape { shape: ShapeKind, stroke: Color32, fill: Option<Color32>, width: f32, a: Pos2, b: Pos2 },
}

#[derive(Clone)]
pub struct Obj {
    pub rect: Rect,
    pub kind: Kind,
}

impl Obj {
    pub fn label(&self) -> String {
        match &self.kind {
            Kind::Text { text, .. } => {
                let first = text.lines().next().unwrap_or("");
                let s: String = first.chars().take(24).collect();
                if s.is_empty() { "Text".into() } else { s }
            }
            Kind::Image(_) => "Picture".into(),
            Kind::Ink { highlighter: true, .. } => "Highlight".into(),
            Kind::Ink { .. } => "Ink".into(),
            Kind::Shape { shape, fill, width, .. } => match shape {
                ShapeKind::Rect if *width == 0.0 && *fill == Some(Color32::WHITE) => "Whiteout".into(),
                ShapeKind::Rect => "Rectangle".into(),
                ShapeKind::Ellipse => "Ellipse".into(),
                ShapeKind::Line => "Line".into(),
                ShapeKind::Arrow => "Arrow".into(),
            },
        }
    }

    pub fn icon(&self) -> &'static str {
        match &self.kind {
            Kind::Text { .. } => "type",
            Kind::Image(_) => "image",
            Kind::Ink { highlighter: true, .. } => "highlighter",
            Kind::Ink { .. } => "pencil",
            Kind::Shape { shape, .. } => match shape {
                ShapeKind::Rect => "square",
                ShapeKind::Ellipse => "circle",
                ShapeKind::Line => "slash",
                ShapeKind::Arrow => "move-up-right",
            },
        }
    }

    /// Recompute a text object's box from its content.
    pub fn refit_text(&mut self) {
        if let Kind::Text { text, size, bold, .. } = &self.kind {
            let l = crate::text::layout(text, *size, *bold);
            self.rect = Rect::from_min_size(self.rect.min, vec2(l.width.max(*size * 0.5), l.height));
        }
    }

    /// Unit-coordinate point to page point.
    pub fn at(&self, u: Pos2) -> Pos2 {
        pos2(self.rect.min.x + u.x * self.rect.width(), self.rect.min.y + u.y * self.rect.height())
    }

    pub fn hit(&self, p: Pos2, tol: f32) -> bool {
        if let Kind::Shape { shape: ShapeKind::Line | ShapeKind::Arrow, a, b, width, .. } = &self.kind {
            let (a, b) = (self.at(*a), self.at(*b));
            return seg_dist(p, a, b) <= tol + width * 0.5;
        }
        self.rect.expand(tol).contains(p)
    }
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
    (a + ab * t - p).length()
}

/// Normalise absolute stroke points into the bounding rect of all strokes.
pub fn ink_from_points(strokes: &[Vec<Pos2>], pad: f32) -> (Rect, Vec<Vec<Pos2>>) {
    let mut r = Rect::NOTHING;
    for s in strokes {
        for p in s {
            r.extend_with(*p);
        }
    }
    let r = r.expand(pad);
    let (w, h) = (r.width().max(1e-3), r.height().max(1e-3));
    let unit = strokes.iter().map(|s| s.iter().map(|p| pos2((p.x - r.min.x) / w, (p.y - r.min.y) / h)).collect()).collect();
    (r, unit)
}

pub struct Doc {
    pub path: Option<PathBuf>,
    /// The source PDF, never modified; `None` for a document started from scratch.
    pub source: Option<Arc<Vec<u8>>>,
    pub pages: Vec<PageEntry>,
    pub dirty: bool,
    undo: Vec<Vec<PageEntry>>,
    redo: Vec<Vec<PageEntry>>,
}

const UNDO_LIMIT: usize = 60;

impl Doc {
    pub fn new_blank() -> Self {
        Doc { path: None, source: None, pages: vec![PageEntry::blank(A4)], dirty: false, undo: Vec::new(), redo: Vec::new() }
    }

    pub fn from_pdf(path: Option<PathBuf>, bytes: Arc<Vec<u8>>, sizes: &[Vec2]) -> Self {
        let pages = sizes
            .iter()
            .enumerate()
            .map(|(i, s)| PageEntry { src: PageSrc::Original(i), size: *s, rotation: 0, objects: Vec::new() })
            .collect();
        Doc { path, source: Some(bytes), pages, dirty: false, undo: Vec::new(), redo: Vec::new() }
    }

    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled.pdf".into())
    }

    /// Record the current state before a change.
    pub fn checkpoint(&mut self) {
        self.push_undo(self.pages.clone());
    }

    /// Record a state captured earlier (at the start of a drag).
    pub fn push_undo(&mut self, state: Vec<PageEntry>) {
        self.undo.push(state);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else { return false };
        self.redo.push(std::mem::replace(&mut self.pages, prev));
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else { return false };
        self.undo.push(std::mem::replace(&mut self.pages, next));
        self.dirty = true;
        true
    }
}
