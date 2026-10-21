//! Round trips: build a document, save it, render the saved file with hayro and check that the
//! placed objects land where the editor showed them.

use crate::doc::{Doc, Kind, Obj, PageEntry, PageSrc, ShapeKind};
use crate::export;
use eframe::egui::{Color32, Rect, pos2, vec2};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::{RenderCache, RenderSettings};
use lopdf::{Document, Object, Stream, dictionary};
use std::sync::Arc;

fn red_box(r: Rect) -> Obj {
    Obj { rect: r, kind: Kind::Shape { shape: ShapeKind::Rect, stroke: Color32::RED, fill: Some(Color32::RED), width: 0.0, a: pos2(0.0, 0.0), b: pos2(1.0, 1.0) } }
}

/// Render page `i` of a PDF at 1 px per point; returns (width, height, rgba).
fn render(bytes: &[u8], i: usize) -> (usize, usize, Vec<u8>) {
    let pdf = Pdf::new(Arc::new(bytes.to_vec())).expect("saved PDF parses");
    let page = &pdf.pages()[i];
    let pix = hayro::render(page, &RenderCache::new(), &InterpreterSettings::default(), &RenderSettings {
        bg_color: hayro::vello_cpu::color::palette::css::WHITE,
        ..Default::default()
    });
    (pix.width() as usize, pix.height() as usize, pix.data_as_u8_slice().to_vec())
}

fn px(img: &(usize, usize, Vec<u8>), x: f32, y: f32) -> [u8; 3] {
    let i = (y as usize * img.0 + x as usize) * 4;
    [img.2[i], img.2[i + 1], img.2[i + 2]]
}

fn is_red(c: [u8; 3]) -> bool {
    c[0] > 200 && c[1] < 60 && c[2] < 60
}

fn is_white(c: [u8; 3]) -> bool {
    c.iter().all(|v| *v > 240)
}

/// A source PDF with an offset crop box and a /Rotate, inherited from the page tree.
pub(crate) fn source_pdf(rotate: i64) -> Vec<u8> {
    let mut d = Document::with_version("1.7");
    let pages = d.new_object_id();
    let content = d.add_object(Stream::new(dictionary! {}, b"0 0 1 rg 60 60 20 20 re f".to_vec()));
    let page = d.add_object(dictionary! { "Type" => "Page", "Parent" => pages, "Contents" => content, "CropBox" => vec![50.into(), 50.into(), 550.into(), 750.into()] });
    d.objects.insert(
        pages,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1,
            "MediaBox" => vec![0.into(), 0.into(), 600.into(), 800.into()], "Rotate" => rotate, "Resources" => dictionary! {},
        }),
    );
    let cat = d.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    d.trailer.set("Root", cat);
    let mut out = Vec::new();
    d.save_to(&mut out).unwrap();
    out
}

#[test]
fn blank_page_overlay_lands_in_place() {
    let mut doc = Doc::new_blank();
    doc.pages[0].objects.push(red_box(Rect::from_min_max(pos2(100.0, 100.0), pos2(200.0, 150.0))));
    let img = render(&export::save(&doc).unwrap(), 0);
    assert_eq!((img.0, img.1), (595, 842));
    assert!(is_red(px(&img, 150.0, 125.0)));
    assert!(is_white(px(&img, 150.0, 90.0)));
    assert!(is_white(px(&img, 90.0, 125.0)));
}

#[test]
fn editor_rotation_keeps_overlay_on_its_content() {
    let mut doc = Doc::new_blank();
    doc.pages[0].rotation = 90;
    doc.pages[0].objects.push(red_box(Rect::from_min_max(pos2(100.0, 100.0), pos2(200.0, 150.0))));
    let img = render(&export::save(&doc).unwrap(), 0);
    assert_eq!((img.0, img.1), (842, 595));
    let v = doc.pages[0].to_view(pos2(150.0, 125.0));
    assert!(is_red(px(&img, v.x, v.y)), "{:?}", px(&img, v.x, v.y));
}

#[test]
fn source_rotation_and_crop_box_are_respected() {
    for rot in [0, 90, 180, 270] {
        let src = source_pdf(rot);
        let pdf = Pdf::new(Arc::new(src.clone())).unwrap();
        let (w, h) = pdf.pages()[0].render_dimensions();
        let mut doc = Doc::from_pdf(None, Arc::new(src), &[vec2(w, h)]);
        doc.pages[0].objects.push(red_box(Rect::from_min_max(pos2(100.0, 100.0), pos2(200.0, 150.0))));
        let saved = export::save(&doc).unwrap();
        let img = render(&saved, 0);
        assert_eq!((img.0 as f32, img.1 as f32), (w, h), "rot {rot}");
        assert!(is_red(px(&img, 150.0, 125.0)), "rot {rot}: {:?}", px(&img, 150.0, 125.0));
        assert!(is_white(px(&img, 150.0, 160.0)), "rot {rot}");
        // The original blue square is still drawn.
        assert!(img.2.chunks(4).any(|p| p[2] > 200 && p[0] < 60), "rot {rot}: original content lost");
    }
}

#[test]
fn pages_reorder_duplicate_blank_and_delete() {
    let src = source_pdf(0);
    let mut doc = Doc::from_pdf(None, Arc::new(src), &[vec2(500.0, 700.0)]);
    doc.pages.push(PageEntry::blank(vec2(300.0, 400.0)));
    let dup = doc.pages[0].clone();
    doc.pages.insert(0, dup);
    doc.pages[1].objects.push(red_box(Rect::from_min_max(pos2(10.0, 10.0), pos2(50.0, 50.0))));
    let saved = export::save(&doc).unwrap();
    let pdf = Pdf::new(Arc::new(saved.clone())).unwrap();
    assert_eq!(pdf.pages().len(), 3);
    assert_eq!(pdf.pages()[2].render_dimensions(), (300.0, 400.0));
    // Only the second copy carries the box.
    assert!(!is_red(px(&render(&saved, 0), 30.0, 30.0)));
    assert!(is_red(px(&render(&saved, 1), 30.0, 30.0)));
    assert_eq!(doc.pages[0].src, PageSrc::Original(0));
}

#[test]
fn text_is_embedded_and_extractable() {
    let mut doc = Doc::new_blank();
    let mut o = Obj { rect: Rect::from_min_size(pos2(72.0, 72.0), vec2(1.0, 1.0)), kind: Kind::Text { text: "Hello สวัสดี\nLine two".into(), size: 24.0, color: Color32::BLACK, bold: false } };
    o.refit_text();
    let r = o.rect;
    doc.pages[0].objects.push(o);
    let saved = export::save(&doc).unwrap();
    let img = render(&saved, 0);
    let dark = |x0: f32, y0: f32, x1: f32, y1: f32| {
        let mut n = 0;
        for y in y0 as usize..y1 as usize {
            for x in x0 as usize..x1 as usize {
                if px(&img, x as f32, y as f32)[0] < 100 {
                    n += 1;
                }
            }
        }
        n
    };
    assert!(dark(r.min.x, r.min.y, r.max.x, r.max.y) > 300, "text not drawn inside its box");
    assert_eq!(dark(r.max.x + 4.0, r.min.y, r.max.x + 200.0, r.max.y), 0, "text overflows its box");
    std::fs::write(std::env::temp_dir().join("pdfeditor-text-test.pdf"), &saved).unwrap();
}

#[test]
fn ink_and_picture_round_trip() {
    let ctx = eframe::egui::Context::default();
    let mut png = Vec::new();
    let im = image::RgbaImage::from_pixel(8, 8, image::Rgba([0, 160, 0, 255]));
    image::DynamicImage::ImageRgba8(im).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let tex = ctx.load_texture("t", eframe::egui::ColorImage::filled([2, 2], Color32::GREEN), Default::default());
    let mut doc = Doc::new_blank();
    let data = Arc::new(crate::doc::ImageData { bytes: Arc::new(png), px: [8, 8], tex });
    doc.pages[0].objects.push(Obj { rect: Rect::from_min_max(pos2(300.0, 300.0), pos2(400.0, 400.0)), kind: Kind::Image(data) });
    let strokes = vec![vec![pos2(0.0, 0.5), pos2(1.0, 0.5)]];
    doc.pages[0].objects.push(Obj {
        rect: Rect::from_min_max(pos2(50.0, 500.0), pos2(250.0, 520.0)),
        kind: Kind::Ink { strokes: Arc::new(strokes), color: Color32::BLUE, width: 4.0, highlighter: false },
    });
    let img = render(&export::save(&doc).unwrap(), 0);
    let g = px(&img, 350.0, 350.0);
    assert!(g[1] > 120 && g[0] < 40, "picture: {g:?}");
    let b = px(&img, 150.0, 510.0);
    assert!(b[2] > 200 && b[0] < 40, "ink: {b:?}");
}

#[test]
fn jpeg_marker_reader() {
    let mut jpg = Vec::new();
    let im = image::RgbImage::from_pixel(4, 4, image::Rgb([10, 20, 30]));
    image::DynamicImage::ImageRgb8(im).write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg).unwrap();
    assert_eq!(export::jpeg_components(&jpg), Some(3));
    assert_eq!(export::jpeg_components(b"not a jpeg"), None);
}
