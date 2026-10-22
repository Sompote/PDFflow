//! The text already in a PDF page, for selecting, copying and searching it. hayro's interpreter
//! runs the page into a device that keeps only the glyphs: their Unicode text (from ToUnicode,
//! glyph names or the standard encodings) and where they sit, in the page's display space
//! (points, top-left origin, y down, the source page's /Rotate applied), the same space the
//! editor's objects use.

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};
use hayro::hayro_interpret::font::Glyph as HGlyph;
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterCache, InterpreterSettings, Paint, PathDrawMode, SoftMask, interpret_page,
};
use hayro::hayro_syntax::Pdf;
use kurbo::{Affine, BezPath, Point};
use std::sync::Arc;

/// Glyph-space units per em in hayro's glyph transform.
const EM: f64 = 1000.0;

/// One glyph of page text.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub text: String,
    /// Axis-aligned box of the glyph's em box (descender to ascender, origin to advance).
    pub rect: Rect,
    /// Baseline start and end (origin and origin + advance).
    pub origin: Pos2,
    pub end: Pos2,
    /// Font size in points (the em height on the page).
    pub size: f32,
}

impl Glyph {
    fn dir(&self) -> Vec2 {
        let d = self.end - self.origin;
        if d.length() > 1e-3 { d.normalized() } else { vec2(1.0, 0.0) }
    }

    fn center(&self) -> Pos2 {
        self.rect.center()
    }

    fn is_space(&self) -> bool {
        self.text.chars().all(char::is_whitespace)
    }

    fn is_word(&self) -> bool {
        self.text.chars().any(|c| c.is_alphanumeric() || c == '_' || c == '\'' || c == '-')
    }
}

/// The text of one page in content-stream order, which for almost every PDF is reading order.
#[derive(Clone, Debug, Default)]
pub struct PageText {
    pub glyphs: Vec<Glyph>,
}

/// A line of text and its box.
#[derive(Clone, Debug)]
pub struct Line {
    pub text: String,
    pub rect: Rect,
    /// Glyph indices `start..end`.
    pub start: usize,
    pub end: usize,
}

struct Collect {
    glyphs: Vec<Glyph>,
}

fn pt(p: Point) -> Pos2 {
    pos2(p.x as f32, p.y as f32)
}

impl<'a> Device<'a> for Collect {
    fn set_soft_mask(&mut self, _: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _: BlendMode) {}
    fn draw_path(&mut self, _: &BezPath, _: Affine, _: &Paint<'a>, _: &PathDrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: Affine) {}
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}

    fn draw_glyph(&mut self, glyph: &HGlyph<'a>, transform: Affine, glyph_transform: Affine, _: &Paint<'a>, _: &GlyphDrawMode) {
        let text = match glyph.as_unicode() {
            Some(BfString::Char(c)) => c.to_string(),
            Some(BfString::String(s)) => s,
            None => return,
        };
        // Ligatures and other presentation forms come back as their plain letters.
        let text: String = text.chars().flat_map(expand_ligature).collect();
        if text.is_empty() || text.chars().all(|c| c.is_control() && c != '\t') {
            return;
        }
        let m = transform * glyph_transform;
        let adv = match glyph {
            HGlyph::Outline(o) => o.advance_width().filter(|w| *w > 0.0).map_or(EM * 0.5, |w| w as f64),
            HGlyph::Type3(_) => EM * 0.5,
        };
        let (o, e) = (m * Point::new(0.0, 0.0), m * Point::new(adv, 0.0));
        let up = m * Point::new(0.0, EM);
        let size = (pt(up) - pt(o)).length();
        if !(size.is_finite() && size > 0.05) {
            return;
        }
        let corners = [Point::new(0.0, -0.22 * EM), Point::new(adv, -0.22 * EM), Point::new(adv, 0.9 * EM), Point::new(0.0, 0.9 * EM)];
        let rect = Rect::from_points(&corners.map(|c| pt(m * c)));
        self.glyphs.push(Glyph { text, rect, origin: pt(o), end: pt(e), size });
    }
}

fn expand_ligature(c: char) -> Vec<char> {
    match c {
        '\u{FB00}' => vec!['f', 'f'],
        '\u{FB01}' => vec!['f', 'i'],
        '\u{FB02}' => vec!['f', 'l'],
        '\u{FB03}' => vec!['f', 'f', 'i'],
        '\u{FB04}' => vec!['f', 'f', 'l'],
        '\u{FB05}' | '\u{FB06}' => vec!['s', 't'],
        '\u{00A0}' => vec![' '],
        '\u{0000}' => vec![],
        c => vec![c],
    }
}

/// The text of every page of `bytes` listed in `pages` (source page indices).
pub fn extract(bytes: Arc<Vec<u8>>, pages: &[usize]) -> Result<Vec<(usize, PageText)>, String> {
    let pdf = Pdf::new(bytes).map_err(|_| "The PDF cannot be read for text.".to_owned())?;
    let all = pdf.pages();
    let cache = InterpreterCache::new();
    // Annotations (form fields, comments) are not page text.
    let settings = InterpreterSettings { render_annotations: false, ..Default::default() };
    let mut out = Vec::new();
    for &i in pages {
        let Some(page) = all.get(i) else { continue };
        let (w, h) = page.render_dimensions();
        let mut ctx = Context::new(page.initial_transform(true).to_kurbo(), kurbo::Rect::new(0.0, 0.0, w as f64, h as f64), &cache, page.xref(), settings.clone());
        let mut dev = Collect { glyphs: Vec::new() };
        interpret_page(page, &mut ctx, &mut dev);
        out.push((i, PageText { glyphs: merge_accents(dev.glyphs) }));
    }
    Ok(out)
}

use hayro::hayro_interpret::util::TransformExt as _;

/// The combining mark for a spacing accent that TeX-made PDFs draw as its own glyph.
fn combining(accent: &str) -> Option<char> {
    Some(match accent {
        "\u{B4}" | "\u{2CA}" => '\u{301}',
        "`" | "\u{2CB}" => '\u{300}',
        "\u{A8}" => '\u{308}',
        "\u{2C6}" => '\u{302}',
        "\u{2DC}" => '\u{303}',
        "\u{B8}" => '\u{327}',
        "\u{2C7}" => '\u{30C}',
        "\u{2DA}" => '\u{30A}',
        "\u{AF}" | "\u{2C9}" => '\u{304}',
        "\u{2D8}" => '\u{306}',
        "\u{2D9}" => '\u{307}',
        "\u{2DD}" => '\u{30B}',
        _ => return None,
    })
}

/// A letter and a combining mark as one precomposed letter where Latin has one.
fn compose(letter: char, mark: char) -> Option<char> {
    const TABLE: &[(char, &str, &str)] = &[
        ('\u{301}', "aeiouyAEIOUYcnszCNSZ", "áéíóúýÁÉÍÓÚÝćńśźĆŃŚŹ"),
        ('\u{300}', "aeiouAEIOU", "àèìòùÀÈÌÒÙ"),
        ('\u{308}', "aeiouyAEIOUY", "äëïöüÿÄËÏÖÜŸ"),
        ('\u{302}', "aeiouAEIOU", "âêîôûÂÊÎÔÛ"),
        ('\u{303}', "anoANO", "ãñõÃÑÕ"),
        ('\u{327}', "cCsStT", "çÇşŞţŢ"),
        ('\u{30C}', "csznrezCSZNREZ", "čšžňřěžČŠŽŇŘĚŽ"),
        ('\u{30A}', "auAU", "åůÅŮ"),
    ];
    let (_, from, to) = TABLE.iter().find(|t| t.0 == mark)?;
    from.chars().position(|c| c == letter).and_then(|k| to.chars().nth(k))
}

/// Fold accents drawn as separate glyphs into the letter they sit over, so "Tom´as" reads,
/// copies and searches as "Tomás".
fn merge_accents(glyphs: Vec<Glyph>) -> Vec<Glyph> {
    let over = |a: &Glyph, b: &Glyph| {
        let (lo, hi) = (a.rect.min.x.max(b.rect.min.x), a.rect.max.x.min(b.rect.max.x));
        hi - lo > 0.25 * (a.rect.width().min(b.rect.width())) && (a.origin.y - b.origin.y).abs() < 0.5 * a.size.max(b.size)
    };
    let mut out: Vec<Glyph> = Vec::with_capacity(glyphs.len());
    let mut pending: Option<char> = None;
    let mut it = glyphs.into_iter().peekable();
    while let Some(g) = it.next() {
        if let Some(mark) = combining(&g.text) {
            if it.peek().is_some_and(|n| over(&g, n) && n.text.chars().count() == 1) {
                pending = Some(mark);
                continue;
            }
            if let Some(prev) = out.last_mut().filter(|p| over(p, &g) && p.text.chars().count() == 1) {
                let l = prev.text.chars().next().unwrap_or(' ');
                prev.text = compose(l, mark).map_or_else(|| format!("{l}{mark}"), String::from);
                continue;
            }
        }
        let mut g = g;
        if let Some(mark) = pending.take() {
            let l = g.text.chars().next().unwrap_or(' ');
            g.text = compose(l, mark).map_or_else(|| format!("{l}{mark}"), String::from);
        }
        out.push(g);
    }
    out
}

impl PageText {
    pub fn is_empty(&self) -> bool {
        self.glyphs.is_empty()
    }

    /// Whether glyph `b` starts a new line after glyph `a`.
    fn breaks(a: &Glyph, b: &Glyph) -> bool {
        let dir = a.dir();
        let off = b.origin - a.origin;
        let across = (off.x * dir.y - off.y * dir.x).abs();
        let along = (b.origin - a.end).dot(dir);
        // Measured against the larger size, so superscripts and subscripts stay on their line.
        across > 0.5 * a.size.max(b.size) || along < -1.5 * a.size || along > 6.0 * a.size
    }

    /// Whether a space separates glyph `b` from glyph `a` on the same line.
    fn spaced(a: &Glyph, b: &Glyph) -> bool {
        !a.is_space() && !b.is_space() && (b.origin - a.end).dot(a.dir()) > 0.15 * a.size.min(b.size)
    }

    /// The text of the glyphs `idx` (in page order), with spaces and line breaks restored.
    pub fn text_of(&self, idx: &[usize]) -> String {
        let mut s = String::new();
        let mut prev: Option<usize> = None;
        for &i in idx {
            let Some(g) = self.glyphs.get(i) else { continue };
            if let Some(p) = prev {
                let a = &self.glyphs[p];
                // A gap in the selection (column selection) or a break in the page is a new line.
                if i != p + 1 || Self::breaks(a, g) {
                    s.truncate(s.trim_end_matches(' ').len());
                    s.push('\n');
                } else if Self::spaced(a, g) {
                    s.push(' ');
                }
            }
            s.push_str(&g.text);
            prev = Some(i);
        }
        s
    }

    pub fn all_text(&self) -> String {
        self.text_of(&(0..self.glyphs.len()).collect::<Vec<_>>())
    }

    /// The page split into lines.
    pub fn lines(&self) -> Vec<Line> {
        let mut out = Vec::new();
        let mut start = 0;
        for i in 1..=self.glyphs.len() {
            if i == self.glyphs.len() || Self::breaks(&self.glyphs[i - 1], &self.glyphs[i]) {
                if start < i {
                    let idx: Vec<usize> = (start..i).collect();
                    let text = self.text_of(&idx);
                    if !text.trim().is_empty() {
                        let rect = self.bounds(&idx);
                        out.push(Line { text: text.trim().to_owned(), rect, start, end: i });
                    }
                }
                start = i;
            }
        }
        out
    }

    fn bounds(&self, idx: &[usize]) -> Rect {
        idx.iter().filter(|i| !self.glyphs[**i].is_space()).fold(Rect::NOTHING, |r, i| r.union(self.glyphs[*i].rect))
    }

    /// One box per line run of the glyphs `idx`, for painting a selection or marking it up.
    pub fn runs(&self, idx: &[usize]) -> Vec<(Rect, Pos2, Pos2, f32)> {
        let mut out = Vec::new();
        let mut run: Vec<usize> = Vec::new();
        let flush = |run: &mut Vec<usize>, out: &mut Vec<(Rect, Pos2, Pos2, f32)>| {
            let solid: Vec<usize> = run.iter().copied().filter(|i| !self.glyphs[*i].is_space()).collect();
            if let (Some(&a), Some(&b)) = (solid.first(), solid.last()) {
                let size = solid.iter().map(|i| self.glyphs[*i].size).fold(0.0, f32::max);
                out.push((self.bounds(&solid), self.glyphs[a].origin, self.glyphs[b].end, size));
            }
            run.clear();
        };
        for &i in idx {
            if let Some(&p) = run.last() {
                if i != p + 1 || Self::breaks(&self.glyphs[p], &self.glyphs[i]) {
                    flush(&mut run, &mut out);
                }
            }
            run.push(i);
        }
        flush(&mut run, &mut out);
        out
    }

    /// The glyph nearest to `p` and its distance (0 inside the glyph box).
    fn nearest(&self, p: Pos2) -> Option<(usize, f32)> {
        let mut best: Option<(usize, f32)> = None;
        for (i, g) in self.glyphs.iter().enumerate() {
            let r = g.rect;
            let dx = (r.min.x - p.x).max(p.x - r.max.x).max(0.0);
            // Vertical distance counts more, so a point beside a line picks that line.
            let dy = (r.min.y - p.y).max(p.y - r.max.y).max(0.0) * 2.0;
            let d = (dx * dx + dy * dy).sqrt();
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best
    }

    /// Caret position (0..=glyph count) for a point: before or after the nearest glyph.
    pub fn caret_at(&self, p: Pos2) -> Option<usize> {
        let (i, _) = self.nearest(p)?;
        let g = &self.glyphs[i];
        let after = (p - g.center()).dot(g.dir()) > 0.0;
        Some(if after { i + 1 } else { i })
    }

    /// Whether `p` is on (or within `tol` points of) any glyph.
    pub fn over_text(&self, p: Pos2, tol: f32) -> bool {
        self.nearest(p).is_some_and(|(_, d)| d <= tol)
    }

    /// The word around `p`.
    pub fn word_at(&self, p: Pos2) -> Option<Vec<usize>> {
        let (i, d) = self.nearest(p)?;
        if d > 4.0 {
            return None;
        }
        if !self.glyphs[i].is_word() {
            return Some(vec![i]);
        }
        let joined = |a: usize, b: usize| self.glyphs[a].is_word() && self.glyphs[b].is_word() && !Self::breaks(&self.glyphs[a], &self.glyphs[b]) && !Self::spaced(&self.glyphs[a], &self.glyphs[b]);
        let (mut a, mut b) = (i, i);
        while a > 0 && joined(a - 1, a) {
            a -= 1;
        }
        while b + 1 < self.glyphs.len() && joined(b, b + 1) {
            b += 1;
        }
        Some((a..=b).collect())
    }

    /// The line around `p`.
    pub fn line_at(&self, p: Pos2) -> Option<Vec<usize>> {
        let (i, d) = self.nearest(p)?;
        if d > 4.0 {
            return None;
        }
        self.lines().into_iter().find(|l| (l.start..l.end).contains(&i)).map(|l| (l.start..l.end).collect())
    }

    /// Glyphs whose centre lies in `r` (column selection).
    pub fn in_rect(&self, r: Rect) -> Vec<usize> {
        (0..self.glyphs.len()).filter(|i| r.contains(self.glyphs[*i].center())).collect()
    }

    /// Every occurrence of `query` as glyph index lists. Whitespace in the query matches any
    /// run of spaces or line breaks in the page, so phrases that wrap are found.
    pub fn find(&self, query: &str, case_sensitive: bool) -> Vec<Vec<usize>> {
        let norm = |s: &str| if case_sensitive { s.to_owned() } else { s.to_lowercase() };
        let words: Vec<String> = query.split_whitespace().map(norm).collect();
        if words.is_empty() {
            return Vec::new();
        }
        // The page as characters, each tagged with its glyph (None for restored separators).
        let all: Vec<usize> = (0..self.glyphs.len()).collect();
        let mut chars: Vec<(char, Option<usize>)> = Vec::new();
        let mut prev: Option<usize> = None;
        for &i in &all {
            let g = &self.glyphs[i];
            if let Some(p) = prev {
                if Self::breaks(&self.glyphs[p], g) || Self::spaced(&self.glyphs[p], g) {
                    chars.push((' ', None));
                }
            }
            for c in norm(&g.text).chars() {
                chars.push((if c.is_whitespace() { ' ' } else { c }, Some(i)));
            }
            prev = Some(i);
        }
        let mut out = Vec::new();
        let mut k = 0;
        while k < chars.len() {
            if let Some(end) = match_words(&chars, k, &words) {
                let mut idx: Vec<usize> = chars[k..end].iter().filter_map(|c| c.1).collect();
                idx.dedup();
                if !idx.is_empty() {
                    out.push(idx);
                }
                k = end.max(k + 1);
            } else {
                k += 1;
            }
        }
        out
    }
}

/// End of a match of `words` (separated by any whitespace) starting at `k`.
fn match_words(chars: &[(char, Option<usize>)], mut k: usize, words: &[String]) -> Option<usize> {
    for (w, word) in words.iter().enumerate() {
        if w > 0 {
            let s = k;
            while k < chars.len() && chars[k].0 == ' ' {
                k += 1;
            }
            if k == s {
                return None;
            }
        }
        for c in word.chars() {
            if chars.get(k)?.0 != c {
                return None;
            }
            k += 1;
        }
    }
    Some(k)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use lopdf::{Document, Object, Stream, dictionary};

    /// A page with two lines of Helvetica text and a word set with a kerning gap.
    pub(crate) fn text_pdf() -> Vec<u8> {
        let mut d = Document::with_version("1.7");
        let pages = d.new_object_id();
        let font = d.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" });
        let content = b"BT /F1 20 Tf 72 700 Td (Hello world) Tj 0 -30 Td [(Soil) -300 (mechanics)] TJ ET".to_vec();
        let content = d.add_object(Stream::new(dictionary! {}, content));
        let page = d.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages, "Contents" => content,
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
        });
        d.objects.insert(
            pages,
            Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1, "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()] }),
        );
        let cat = d.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        d.trailer.set("Root", cat);
        let mut out = Vec::new();
        d.save_to(&mut out).unwrap();
        out
    }

    #[test]
    fn extracts_text_with_spaces_lines_and_positions() {
        let pages = extract(Arc::new(text_pdf()), &[0]).unwrap();
        let t = &pages[0].1;
        assert_eq!(t.all_text(), "Hello world\nSoil mechanics");
        let lines = t.lines();
        assert_eq!(lines.len(), 2);
        // Baseline 700 pt from the bottom of a 792 pt page is 92 pt from the top.
        let h = &t.glyphs[0];
        assert!((h.origin.x - 72.0).abs() < 0.5 && (h.origin.y - 92.0).abs() < 0.5, "{h:?}");
        assert!((h.size - 20.0).abs() < 0.1);
        assert!(lines[0].rect.min.y < 92.0 && lines[0].rect.max.y > 92.0);
        assert!(lines[1].rect.min.y > lines[0].rect.max.y - 5.0);
    }

    fn glyph(text: &str, x: f32, w: f32, y: f32, size: f32) -> Glyph {
        Glyph { text: text.into(), rect: Rect::from_min_max(pos2(x, y - size * 0.9), pos2(x + w, y + size * 0.22)), origin: pos2(x, y), end: pos2(x + w, y), size }
    }

    #[test]
    fn accents_merge_and_superscripts_stay_on_the_line() {
        // "Tom´as" with the accent drawn before the a it sits over, and "Li" with a superscript "a".
        let g = vec![glyph("T", 0.0, 6.0, 100.0, 10.0), glyph("o", 6.0, 5.0, 100.0, 10.0), glyph("m", 11.0, 8.0, 100.0, 10.0), glyph("\u{B4}", 20.0, 3.0, 100.0, 10.0), glyph("a", 19.0, 5.0, 100.0, 10.0), glyph("s", 24.0, 4.0, 100.0, 10.0)];
        let t = PageText { glyphs: merge_accents(g) };
        assert_eq!(t.all_text(), "Tomás");
        assert_eq!(t.find("tomás", false).len(), 1);
        let sup = PageText { glyphs: vec![glyph("L", 0.0, 6.0, 100.0, 10.0), glyph("i", 6.0, 3.0, 100.0, 10.0), glyph("a", 9.5, 3.0, 96.0, 6.0), glyph(",", 12.6, 3.0, 100.0, 10.0), glyph("Y", 0.0, 6.0, 112.0, 10.0)] };
        assert_eq!(sup.all_text(), "Lia,\nY");
    }

    #[test]
    fn carets_words_search_and_column_selection() {
        let pages = extract(Arc::new(text_pdf()), &[0]).unwrap();
        let t = &pages[0].1;
        // From the start of "Hello" to just after "wor".
        let a = t.caret_at(pos2(70.0, 85.0)).unwrap();
        let w = t.word_at(pos2(140.0, 85.0)).unwrap();
        assert_eq!(t.text_of(&w), "world");
        assert_eq!(a, 0);
        let hits = t.find("SOIL   mechanics", false);
        assert_eq!(hits.len(), 1);
        assert_eq!(t.text_of(&hits[0]), "Soil mechanics");
        assert!(t.find("hello", true).is_empty());
        let both = t.find("world soil", false);
        assert_eq!(both.len(), 1, "a phrase across the line break is found");
        let col = t.in_rect(Rect::from_min_max(pos2(60.0, 70.0), pos2(110.0, 130.0)));
        assert_eq!(t.text_of(&col), "Hell\nSoil");
        assert_eq!(t.runs(&w).len(), 1);
    }
}
