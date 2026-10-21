//! Writes the edited document as a PDF. The source file is re-read with lopdf, the page tree
//! is rebuilt in the editor's order, and every page with placed objects gets one extra content
//! stream drawn after the original content (which is wrapped in q/Q so it cannot leak state).
//! Text is real, selectable text in an embedded Sarabun font; JPEGs are embedded unchanged.

use crate::doc::{Doc, ImageData, Kind, PageSrc, ShapeKind};
use crate::text;
use eframe::egui::{Color32, Pos2};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat, dictionary};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::Arc;

type Res<T> = Result<T, String>;

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

pub fn save(doc: &Doc) -> Res<Vec<u8>> {
    let mut pdf = match &doc.source {
        Some(bytes) => Document::load_mem(bytes).map_err(|e| format!("Cannot re-read the source PDF: {e}"))?,
        None => {
            let mut d = Document::with_version("1.7");
            let pages = d.add_object(dictionary! { "Type" => "Pages", "Kids" => Vec::<Object>::new(), "Count" => 0 });
            let cat = d.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
            d.trailer.set("Root", cat);
            d
        }
    };
    let root_pages = pdf.catalog().map_err(err)?.get(b"Pages").and_then(Object::as_reference).map_err(err)?;
    let originals: Vec<ObjectId> = pdf.get_pages().values().copied().collect();
    let mut w = Writer { font_ids: [None, None], glyphs: [BTreeMap::new(), BTreeMap::new()], images: HashMap::new(), gs_highlight: None };
    let mut kids = Vec::new();
    let mut reused = std::collections::HashSet::new();

    for entry in &doc.pages {
        let (mut page, display_box, base_rot, reuse_id) = match entry.src {
            PageSrc::Original(i) => {
                let id = *originals.get(i).ok_or("page index out of range")?;
                let mut d = pdf.get_dictionary(id).map_err(err)?.clone();
                let media = inherited(&pdf, id, b"MediaBox").and_then(|o| rect_of(&pdf, &o)).unwrap_or([0.0, 0.0, 595.0, 842.0]);
                let crop = inherited(&pdf, id, b"CropBox").and_then(|o| rect_of(&pdf, &o));
                let rot = inherited(&pdf, id, b"Rotate").and_then(|o| o.as_i64().ok()).unwrap_or(0).rem_euclid(360) as u16 / 90 * 90;
                let res = inherited(&pdf, id, b"Resources").map(|o| resolve_dict(&pdf, &o)).unwrap_or_default();
                d.set("MediaBox", rect_obj(media));
                if let Some(c) = crop {
                    d.set("CropBox", rect_obj(c));
                }
                d.set("Resources", res);
                // hayro shows the crop box clipped to the media box; overlays are placed in that box.
                let b = crop.map(|c| intersect(c, media)).filter(|b| b[2] > b[0] && b[3] > b[1]).unwrap_or(media);
                let reuse = reused.insert(id).then_some(id);
                (d, b, rot, reuse)
            }
            PageSrc::Blank => {
                let b = [0.0, 0.0, entry.size.x, entry.size.y];
                (dictionary! { "Type" => "Page", "MediaBox" => rect_obj(b), "Resources" => Dictionary::new() }, b, 0, None)
            }
        };
        page.set("Parent", root_pages);
        page.set("Rotate", ((base_rot + entry.rotation) % 360) as i64);

        // Comments become Text annotations; the source's own comments were imported into the
        // editor, so they are replaced rather than duplicated.
        let notes: Vec<&crate::doc::Obj> = entry.objects.iter().filter(|o| matches!(o.kind, Kind::Note { .. })).collect();
        let old_annots: Vec<Object> = match page.get(b"Annots") {
            Ok(o) => match pdf.dereference(o) {
                Ok((_, Object::Array(a))) => a.clone(),
                _ => Vec::new(),
            },
            Err(_) => Vec::new(),
        };
        let kept: Vec<Object> = old_annots.iter().filter(|a| !is_comment_annot(&pdf, a)).cloned().collect();
        if kept.len() != old_annots.len() || !notes.is_empty() {
            let m = display_matrix(display_box, base_rot);
            let mut annots = kept;
            for (k, o) in notes.iter().enumerate() {
                let Kind::Note { text, author, date, color } = &o.kind else { continue };
                let r = rect_to_user(m, [o.rect.min.x, o.rect.min.y, o.rect.max.x, o.rect.max.y]);
                let c = |v: u8| Object::Real(v as f32 / 255.0);
                let id = pdf.add_object(dictionary! {
                    "Type" => "Annot",
                    "Subtype" => "Text",
                    "Rect" => rect_obj(r),
                    "Contents" => Object::String(pdf_text(text), StringFormat::Literal),
                    "T" => Object::String(pdf_text(author), StringFormat::Literal),
                    "M" => Object::String(date.as_bytes().to_vec(), StringFormat::Literal),
                    "CreationDate" => Object::String(date.as_bytes().to_vec(), StringFormat::Literal),
                    "NM" => Object::String(format!("pdfflow-{}-{k}", kids.len()).into_bytes(), StringFormat::Literal),
                    "Name" => "Comment",
                    "C" => vec![c(color.r()), c(color.g()), c(color.b())],
                    "F" => 28,
                    "Open" => false,
                });
                annots.push(Object::Reference(id));
            }
            if annots.is_empty() {
                page.remove(b"Annots");
            } else {
                page.set("Annots", annots);
            }
        }

        if !entry.objects.is_empty() {
            let mut res = match page.get(b"Resources") {
                Ok(Object::Dictionary(d)) => d.clone(),
                _ => Dictionary::new(),
            };
            let content = w.overlay(&mut pdf, &mut res, entry, display_box, base_rot)?;
            page.set("Resources", res);
            let mut contents: Vec<Object> = match page.get(b"Contents") {
                Ok(Object::Array(a)) => a.clone(),
                Ok(o @ Object::Reference(_)) => vec![o.clone()],
                Ok(Object::Stream(s)) => vec![Object::Reference(pdf.add_object(s.clone()))],
                _ => Vec::new(),
            };
            let open = pdf.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
            let close = pdf.add_object(Stream::new(Dictionary::new(), b"\nQ\n".to_vec()));
            let ov = pdf.add_object(flate_stream(Dictionary::new(), &content));
            contents.insert(0, open.into());
            contents.push(close.into());
            contents.push(ov.into());
            page.set("Contents", contents);
        }

        let id = match reuse_id {
            Some(id) => {
                pdf.objects.insert(id, Object::Dictionary(page));
                id
            }
            None => pdf.add_object(page),
        };
        kids.push(Object::Reference(id));
    }

    w.write_fonts(&mut pdf);
    let count = kids.len() as i64;
    let root = pdf.get_dictionary_mut(root_pages).map_err(err)?;
    root.set("Kids", kids);
    root.set("Count", count);
    pdf.prune_objects();
    let mut out = Vec::new();
    pdf.save_modern(&mut out).map_err(err)?;
    Ok(out)
}

struct Writer {
    font_ids: [Option<ObjectId>; 2],
    glyphs: [BTreeMap<u16, String>; 2],
    images: HashMap<usize, ObjectId>,
    gs_highlight: Option<ObjectId>,
}

impl Writer {
    fn overlay(&mut self, pdf: &mut Document, res: &mut Dictionary, entry: &crate::doc::PageEntry, b: [f32; 4], rot: u16) -> Res<Vec<u8>> {
        let m = display_matrix(b, rot);
        let mut s = String::new();
        let _ = writeln!(s, "{} {} {} {} {} {} cm", m[0], m[1], m[2], m[3], n(m[4]), n(m[5]));
        for (k, obj) in entry.objects.iter().enumerate() {
            let r = obj.rect;
            match &obj.kind {
                Kind::Note { .. } => {}
                Kind::Text { text, size, color, bold } => {
                    let fi = *bold as usize;
                    let fid = *self.font_ids[fi].get_or_insert_with(|| pdf.new_object_id());
                    let name = format!("PEf{fi}");
                    add_resource(pdf, res, b"Font", &name, fid);
                    let l = text::layout(text, *size, *bold);
                    let _ = writeln!(s, "BT /{name} {} Tf {} rg", n(*size), rgb(*color));
                    for g in &l.glyphs {
                        let _ = writeln!(s, "1 0 0 -1 {} {} Tm <{:04X}> Tj", n(r.min.x + g.x), n(r.min.y + g.y), g.gid);
                        let e = self.glyphs[fi].entry(g.gid).or_default();
                        if e.is_empty() {
                            e.push_str(&g.text);
                        }
                    }
                    s.push_str("ET\n");
                }
                Kind::Image(img) => {
                    let key = Arc::as_ptr(img) as usize;
                    let id = match self.images.get(&key) {
                        Some(id) => *id,
                        None => {
                            let id = image_xobject(pdf, img)?;
                            self.images.insert(key, id);
                            id
                        }
                    };
                    let name = format!("PEim{k}");
                    add_resource(pdf, res, b"XObject", &name, id);
                    let _ = writeln!(s, "q {} 0 0 {} {} {} cm /{name} Do Q", n(r.width()), n(-r.height()), n(r.min.x), n(r.max.y));
                }
                Kind::Ink { strokes, color, width, highlighter } => {
                    s.push_str("q 1 J 1 j ");
                    if *highlighter {
                        let id = *self.gs_highlight.get_or_insert_with(|| {
                            pdf.add_object(dictionary! { "Type" => "ExtGState", "CA" => 0.45, "ca" => 0.45, "BM" => "Multiply" })
                        });
                        add_resource(pdf, res, b"ExtGState", "PEgsH", id);
                        s.push_str("/PEgsH gs ");
                    }
                    let _ = writeln!(s, "{} RG {} w", rgb(*color), n(*width));
                    for st in strokes.iter() {
                        for (i, p) in st.iter().enumerate() {
                            let p = obj.at(*p);
                            let _ = write!(s, "{} {} {} ", n(p.x), n(p.y), if i == 0 { "m" } else { "l" });
                        }
                        if st.len() == 1 {
                            let p = obj.at(st[0]);
                            let _ = write!(s, "{} {} l ", n(p.x), n(p.y));
                        }
                        s.push_str("S\n");
                    }
                    s.push_str("Q\n");
                }
                Kind::Path { stroke, fill, width, .. } => {
                    let (abs, closed) = obj.path_abs().unwrap_or_default();
                    let _ = write!(s, "q 1 j 1 J {} RG {} w ", rgb(*stroke), n(*width));
                    if let Some(f) = fill {
                        let _ = write!(s, "{} rg ", rgb(*f));
                    }
                    if let Some(first) = abs.first() {
                        let _ = write!(s, "{} {} m ", n(first.p.x), n(first.p.y));
                    }
                    for (_, c) in crate::path::segments(&abs, closed) {
                        let _ = write!(s, "{} {} {} {} {} {} c ", n(c[1].x), n(c[1].y), n(c[2].x), n(c[2].y), n(c[3].x), n(c[3].y));
                    }
                    let paint = match (fill.is_some() && closed, *width > 0.0) {
                        (true, true) => "b",
                        (true, false) => "f",
                        (false, true) if closed => "s",
                        (false, true) => "S",
                        (false, false) => "n",
                    };
                    let _ = writeln!(s, "{paint} Q");
                }
                Kind::Shape { shape, stroke, fill, width, a, b } => {
                    let _ = write!(s, "q 1 j {} RG {} w ", rgb(*stroke), n(*width));
                    if let Some(f) = fill {
                        let _ = write!(s, "{} rg ", rgb(*f));
                    }
                    let paint = match (fill.is_some(), *width > 0.0) {
                        (true, true) => "B",
                        (true, false) => "f",
                        (false, true) => "S",
                        (false, false) => "n",
                    };
                    match shape {
                        ShapeKind::Rect => {
                            let _ = writeln!(s, "{} {} {} {} re {paint}", n(r.min.x), n(r.min.y), n(r.width()), n(r.height()));
                        }
                        ShapeKind::Ellipse => {
                            let (cx, cy, rx, ry) = (r.center().x, r.center().y, r.width() / 2.0, r.height() / 2.0);
                            let (kx, ky) = (rx * 0.552_284_8, ry * 0.552_284_8);
                            let _ = writeln!(
                                s,
                                "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h {paint}",
                                n(cx + rx), n(cy),
                                n(cx + rx), n(cy + ky), n(cx + kx), n(cy + ry), n(cx), n(cy + ry),
                                n(cx - kx), n(cy + ry), n(cx - rx), n(cy + ky), n(cx - rx), n(cy),
                                n(cx - rx), n(cy - ky), n(cx - kx), n(cy - ry), n(cx), n(cy - ry),
                                n(cx + kx), n(cy - ry), n(cx + rx), n(cy - ky), n(cx + rx), n(cy)
                            );
                        }
                        ShapeKind::Line | ShapeKind::Arrow => {
                            let (pa, pb) = (obj.at(*a), obj.at(*b));
                            let tip = if *shape == ShapeKind::Arrow { arrow_head(pa, pb, *width) } else { None };
                            let end = tip.map(|t| t[3]).unwrap_or(pb);
                            let _ = writeln!(s, "1 J {} {} m {} {} l S", n(pa.x), n(pa.y), n(end.x), n(end.y));
                            if let Some([t, l, rr, _]) = tip {
                                let _ = writeln!(s, "{} rg {} {} m {} {} l {} {} l h f", rgb(*stroke), n(t.x), n(t.y), n(l.x), n(l.y), n(rr.x), n(rr.y));
                            }
                        }
                    }
                    s.push_str("Q\n");
                }
            }
        }
        Ok(s.into_bytes())
    }

    fn write_fonts(&self, pdf: &mut Document) {
        for fi in 0..2 {
            let Some(id) = self.font_ids[fi] else { continue };
            let face = text::face(fi == 1);
            let k = 1000.0 / face.upem;
            let file = {
                let mut st = flate_stream(Dictionary::new(), face.data);
                st.dict.set("Length1", face.data.len() as i64);
                pdf.add_object(st)
            };
            let bb = face.bbox;
            let desc = pdf.add_object(dictionary! {
                "Type" => "FontDescriptor",
                "FontName" => Object::Name(face.name.as_bytes().to_vec()),
                "Flags" => 32,
                "FontBBox" => vec![(bb[0] * k).into(), (bb[1] * k).into(), (bb[2] * k).into(), (bb[3] * k).into()],
                "ItalicAngle" => 0,
                "Ascent" => face.ascent * k,
                "Descent" => face.descent * k,
                "CapHeight" => face.ascent * k * 0.7,
                "StemV" => if fi == 1 { 140 } else { 80 },
                "FontFile2" => file,
            });
            let mut widths = Vec::new();
            for gid in self.glyphs[fi].keys() {
                widths.push(Object::Integer(*gid as i64));
                widths.push(Object::Array(vec![Object::Real(face.advance(*gid) * k)]));
            }
            let cid = pdf.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "CIDFontType2",
                "BaseFont" => Object::Name(face.name.as_bytes().to_vec()),
                "CIDSystemInfo" => dictionary! {
                    "Registry" => Object::String(b"Adobe".to_vec(), StringFormat::Literal),
                    "Ordering" => Object::String(b"Identity".to_vec(), StringFormat::Literal),
                    "Supplement" => 0,
                },
                "FontDescriptor" => desc,
                "DW" => 1000,
                "W" => widths,
                "CIDToGIDMap" => "Identity",
            });
            let cmap = pdf.add_object(flate_stream(Dictionary::new(), to_unicode(&self.glyphs[fi]).as_bytes()));
            pdf.objects.insert(
                id,
                Object::Dictionary(dictionary! {
                    "Type" => "Font",
                    "Subtype" => "Type0",
                    "BaseFont" => Object::Name(face.name.as_bytes().to_vec()),
                    "Encoding" => "Identity-H",
                    "DescendantFonts" => vec![Object::Reference(cid)],
                    "ToUnicode" => cmap,
                }),
            );
        }
    }
}

/// Display space (top-left origin, y down, the page's /Rotate applied) to PDF user space, as a
/// `cm` matrix [a b c d e f] for the visible box `b` and rotation `rot`.
pub(crate) fn display_matrix(b: [f32; 4], rot: u16) -> [f32; 6] {
    let [x0, y0, x1, y1] = b;
    match rot {
        90 => [0.0, 1.0, 1.0, 0.0, x0, y0],
        180 => [-1.0, 0.0, 0.0, 1.0, x1, y0],
        270 => [0.0, -1.0, -1.0, 0.0, x1, y1],
        _ => [1.0, 0.0, 0.0, -1.0, x0, y1],
    }
}

/// A display-space rect in user space.
pub(crate) fn rect_to_user(m: [f32; 6], r: [f32; 4]) -> [f32; 4] {
    let t = |u: f32, v: f32| (m[0] * u + m[2] * v + m[4], m[1] * u + m[3] * v + m[5]);
    let (ax, ay) = t(r[0], r[1]);
    let (bx, by) = t(r[2], r[3]);
    [ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)]
}

/// A user-space point in display space (inverse of `display_matrix`).
pub(crate) fn point_to_display(m: [f32; 6], x: f32, y: f32) -> (f32, f32) {
    let det = m[0] * m[3] - m[1] * m[2];
    let (dx, dy) = (x - m[4], y - m[5]);
    ((m[3] * dx - m[2] * dy) / det, (-m[1] * dx + m[0] * dy) / det)
}

/// The visible box (crop ∩ media) and /Rotate of a page, with inheritance.
pub(crate) fn page_frame(pdf: &Document, id: ObjectId) -> ([f32; 4], u16) {
    let media = inherited(pdf, id, b"MediaBox").and_then(|o| rect_of(pdf, &o)).unwrap_or([0.0, 0.0, 595.0, 842.0]);
    let crop = inherited(pdf, id, b"CropBox").and_then(|o| rect_of(pdf, &o));
    let b = crop.map(|c| intersect(c, media)).filter(|b| b[2] > b[0] && b[3] > b[1]).unwrap_or(media);
    let rot = inherited(pdf, id, b"Rotate").and_then(|o| o.as_i64().ok()).unwrap_or(0).rem_euclid(360) as u16 / 90 * 90;
    (b, rot)
}

/// Is this annotation a comment (Text) or the pop-up of one? Those are replaced by the editor's
/// comments when saving.
pub(crate) fn is_comment_annot(pdf: &Document, o: &Object) -> bool {
    let Ok((_, Object::Dictionary(d))) = pdf.dereference(o) else { return false };
    match d.get(b"Subtype").and_then(Object::as_name) {
        Ok(b"Text") => true,
        Ok(b"Popup") => d.get(b"Parent").is_ok_and(|p| is_comment_annot(pdf, p)),
        _ => false,
    }
}

/// Arrow head for a line from `a` to `b`: [tip, left, right, where the shaft should stop].
pub fn arrow_head(a: Pos2, b: Pos2, width: f32) -> Option<[Pos2; 4]> {
    let d = b - a;
    let len = d.length();
    if len < 1e-3 {
        return None;
    }
    let u = d / len;
    let size = (width * 4.0).max(8.0).min(len);
    let base = b - u * size;
    let perp = eframe::egui::vec2(-u.y, u.x) * size * 0.5;
    Some([b, base + perp, base - perp, b - u * size * 0.6])
}

/// A PDF text string: PDFDocEncoding-compatible ASCII, else UTF-16BE with a byte-order mark.
fn pdf_text(s: &str) -> Vec<u8> {
    if s.is_ascii() {
        return s.as_bytes().to_vec();
    }
    let mut out = vec![0xFE, 0xFF];
    for u in s.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

fn to_unicode(map: &BTreeMap<u16, String>) -> String {
    let mut s = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
    );
    let entries: Vec<_> = map.iter().filter(|(_, t)| !t.is_empty()).collect();
    for chunk in entries.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", chunk.len());
        for (gid, t) in chunk {
            let hex: String = t.encode_utf16().map(|u| format!("{u:04X}")).collect();
            let _ = writeln!(s, "<{gid:04X}> <{hex}>");
        }
        s.push_str("endbfchar\n");
    }
    s.push_str("endcmap\nCMapName currentdict /CIDInit /ProcSet findresource exch defineresource pop\nend\nend\n");
    s
}

fn image_xobject(pdf: &mut Document, img: &ImageData) -> Res<ObjectId> {
    let [w, h] = img.px;
    let original = img.raster.is_none();
    if let (true, Some(comps @ (1 | 3))) = (original, jpeg_components(&img.bytes)) {
        let mut st = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64,
                "ColorSpace" => if comps == 1 { "DeviceGray" } else { "DeviceRGB" },
                "BitsPerComponent" => 8, "Filter" => "DCTDecode",
            },
            img.bytes.to_vec(),
        );
        st.allows_compression = false;
        return Ok(pdf.add_object(st));
    }
    let (w, h, data) = img.rgba().ok_or("Cannot decode picture")?;
    let rgba = image::RgbaImage::from_raw(w, h, data).ok_or("Bad picture size")?;
    let mut rgb = Vec::with_capacity((w * h * 3) as usize);
    let mut alpha = Vec::with_capacity((w * h) as usize);
    for p in rgba.pixels() {
        rgb.extend_from_slice(&p.0[..3]);
        alpha.push(p.0[3]);
    }
    let mut dict = dictionary! {
        "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64,
        "ColorSpace" => "DeviceRGB", "BitsPerComponent" => 8,
    };
    if alpha.iter().any(|a| *a < 255) {
        let mask = pdf.add_object(flate_stream(
            dictionary! { "Type" => "XObject", "Subtype" => "Image", "Width" => w as i64, "Height" => h as i64, "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8 },
            &alpha,
        ));
        dict.set("SMask", mask);
    }
    Ok(pdf.add_object(flate_stream(dict, &rgb)))
}

/// Number of colour components of a baseline or progressive JPEG, read from its SOF marker.
pub fn jpeg_components(b: &[u8]) -> Option<u8> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if matches!(marker, 0xC0 | 0xC1 | 0xC2) {
            return b.get(i + 9).copied();
        }
        if matches!(marker, 0xC3 | 0xC5..=0xCF) && marker != 0xC8 && marker != 0xCC {
            return None; // lossless / hierarchical / arithmetic JPEGs: re-encode instead
        }
        i += 2 + len;
    }
    None
}

fn flate_stream(mut dict: Dictionary, data: &[u8]) -> Stream {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = enc.write_all(data);
    let bytes = enc.finish().unwrap_or_default();
    dict.set("Filter", "FlateDecode");
    let mut st = Stream::new(dict, bytes);
    st.allows_compression = false;
    st
}

fn add_resource(pdf: &Document, res: &mut Dictionary, cat: &[u8], name: &str, id: ObjectId) {
    let mut sub = match res.get(cat) {
        Ok(o) => resolve_dict(pdf, o),
        Err(_) => Dictionary::new(),
    };
    sub.set(name.as_bytes().to_vec(), Object::Reference(id));
    res.set(cat.to_vec(), Object::Dictionary(sub));
}

fn resolve_dict(pdf: &Document, o: &Object) -> Dictionary {
    match pdf.dereference(o) {
        Ok((_, Object::Dictionary(d))) => d.clone(),
        _ => Dictionary::new(),
    }
}

pub(crate) fn inherited(pdf: &Document, mut id: ObjectId, key: &[u8]) -> Option<Object> {
    for _ in 0..64 {
        let d = pdf.get_dictionary(id).ok()?;
        if let Ok(v) = d.get(key) {
            return Some(v.clone());
        }
        id = d.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

pub(crate) fn rect_of(pdf: &Document, o: &Object) -> Option<[f32; 4]> {
    let (_, o) = pdf.dereference(o).ok()?;
    let a = o.as_array().ok()?;
    if a.len() != 4 {
        return None;
    }
    let mut v = [0.0f32; 4];
    for (i, x) in a.iter().enumerate() {
        v[i] = pdf.dereference(x).ok()?.1.as_float().ok()?;
    }
    Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

pub(crate) fn intersect(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

fn rect_obj(r: [f32; 4]) -> Vec<Object> {
    r.iter().map(|v| Object::Real(*v)).collect()
}

fn rgb(c: Color32) -> String {
    format!("{} {} {}", n(c.r() as f32 / 255.0), n(c.g() as f32 / 255.0), n(c.b() as f32 / 255.0))
}

/// Compact number formatting for content streams.
fn n(v: f32) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}
