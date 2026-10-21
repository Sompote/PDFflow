//! Text shaping for the Text tool. egui draws the screen text with harfrust; this module runs the
//! same shaper on the same font so the glyphs written into the PDF land where the screen shows them.

use crate::theme::{SARABUN, SARABUN_BOLD};
use skrifa::instance::{LocationRef, Size};
use skrifa::{FontRef, GlyphId, MetadataProvider};
use std::sync::OnceLock;

pub struct Face {
    pub data: &'static [u8],
    pub font: FontRef<'static>,
    shaper: harfrust::ShaperData,
    pub upem: f32,
    pub ascent: f32,
    pub descent: f32,
    pub line_gap: f32,
    pub bbox: [f32; 4],
    pub name: &'static str,
}

impl Face {
    fn new(data: &'static [u8], name: &'static str) -> Self {
        let font = FontRef::new(data).expect("bundled font");
        let m = font.metrics(Size::unscaled(), LocationRef::default());
        let b = m.bounds.map(|b| [b.x_min, b.y_min, b.x_max, b.y_max]).unwrap_or([0.0, m.descent, 1000.0, m.ascent]);
        Face {
            data,
            shaper: harfrust::ShaperData::new(&font),
            font,
            upem: m.units_per_em as f32,
            ascent: m.ascent,
            descent: m.descent,
            line_gap: m.leading,
            bbox: b,
            name,
        }
    }

    /// Advance width of a glyph in font units.
    pub fn advance(&self, gid: u16) -> f32 {
        self.font.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(GlyphId::new(gid as u32)).unwrap_or(0.0)
    }

    /// Line pitch in points for a font size.
    pub fn line_height(&self, size: f32) -> f32 {
        (self.ascent - self.descent + self.line_gap) / self.upem * size
    }
}

pub fn face(bold: bool) -> &'static Face {
    static FACES: OnceLock<[Face; 2]> = OnceLock::new();
    &FACES.get_or_init(|| [Face::new(SARABUN, "Sarabun-Regular"), Face::new(SARABUN_BOLD, "Sarabun-Bold")])[bold as usize]
}

/// One positioned glyph, in points relative to the text box's top-left corner (y down).
pub struct Glyph {
    pub gid: u16,
    pub x: f32,
    pub y: f32,
    /// The characters this glyph starts (empty for the 2nd+ glyph of a cluster).
    pub text: String,
}

pub struct Layout {
    pub glyphs: Vec<Glyph>,
    pub width: f32,
    pub height: f32,
}

pub fn layout(text: &str, size: f32, bold: bool) -> Layout {
    let f = face(bold);
    let scale = size / f.upem;
    let line_height = f.line_height(size);
    let ascent = f.ascent * scale;
    let shaper = f.shaper.shaper(&f.font).build();
    let mut glyphs = Vec::new();
    let mut width: f32 = 0.0;
    let lines: Vec<&str> = text.split('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        let baseline = ascent + i as f32 * line_height;
        if line.is_empty() {
            continue;
        }
        let mut buf = harfrust::UnicodeBuffer::new();
        buf.push_str(line);
        buf.guess_segment_properties();
        let out = shaper.shape(buf, harfrust::ShapeOptions::new());
        let mut x = 0.0;
        let mut last_cluster = u32::MAX;
        for (info, pos) in out.glyph_infos().iter().zip(out.glyph_positions()) {
            let text = if info.cluster != last_cluster {
                let start = info.cluster as usize;
                let end = out.glyph_infos().iter().map(|g| g.cluster as usize).filter(|&c| c > start).min().unwrap_or(line.len());
                line.get(start..end).unwrap_or("").to_owned()
            } else {
                String::new()
            };
            last_cluster = info.cluster;
            glyphs.push(Glyph {
                gid: info.glyph_id as u16,
                x: x + pos.x_offset as f32 * scale,
                y: baseline - pos.y_offset as f32 * scale,
                text,
            });
            x += pos.x_advance as f32 * scale;
        }
        width = width.max(x);
    }
    Layout { glyphs, width, height: lines.len() as f32 * line_height }
}
