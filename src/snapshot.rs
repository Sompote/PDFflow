//! Pictures of pages as the editor shows them, edits included: the Snapshot tool, the agent's
//! page renders and page exports. A page with placed objects is first written as a one-page PDF,
//! so the picture is exactly what Save would produce.

use crate::doc::{Doc, Kind, PageSrc};
use eframe::egui::{Rect, Vec2};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::{RenderCache, RenderSettings};
use image::RgbaImage;
use std::sync::Arc;

/// Largest picture the renderer makes, in pixels and per side.
const MAX_PIXELS: f32 = 60.0e6;
const MAX_SIDE: f32 = 12000.0;

/// The highest scale (pixels per point) not above `scale` that keeps a `size` page in budget.
pub fn fit_scale(size: Vec2, scale: f32) -> f32 {
    let mut s = scale.max(0.05);
    let side = size.x.max(size.y) * s;
    if side > MAX_SIDE {
        s *= MAX_SIDE / side;
    }
    let area = size.x * size.y * s * s;
    if area > MAX_PIXELS {
        s *= (MAX_PIXELS / area).sqrt();
    }
    s
}

/// Page `page` of `doc` in its view orientation (editor rotation applied) at `scale` pixels
/// per point, on white. Returns the picture and the scale actually used.
pub fn render_page(doc: &Doc, page: usize, scale: f32) -> Result<(RgbaImage, f32), String> {
    let entry = doc.pages.get(page).ok_or("No such page.")?;
    let scale = fit_scale(entry.view_size(), scale);
    let drawn = entry.objects.iter().any(|o| !matches!(o.kind, Kind::Note { .. }));
    let (bytes, index) = match (&doc.source, entry.src) {
        (Some(src), PageSrc::Original(i)) if !drawn && entry.rotation == 0 => (src.clone(), i),
        (_, PageSrc::Blank) if !drawn => {
            let vs = entry.view_size() * scale;
            return Ok((RgbaImage::from_pixel((vs.x as u32).max(1), (vs.y as u32).max(1), image::Rgba([255, 255, 255, 255])), scale));
        }
        _ => (Arc::new(crate::export::save(&doc.subset(&[page]))?), 0),
    };
    let pdf = Pdf::new(bytes).map_err(|_| "The page cannot be rendered.".to_owned())?;
    let pages = pdf.pages();
    let p = pages.get(index).ok_or("No such page.")?;
    let pix = hayro::render(
        p,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings { x_scale: scale, y_scale: scale, bg_color: hayro::vello_cpu::color::palette::css::WHITE, ..Default::default() },
    );
    // The background is opaque white, so premultiplied and straight RGBA are the same.
    let img = RgbaImage::from_raw(pix.width() as u32, pix.height() as u32, pix.data_as_u8_slice().to_vec()).ok_or("render size")?;
    Ok((img, scale))
}

/// The part `view` (points, in the page's view space) of a page rendered at `scale`.
pub fn crop(img: &RgbaImage, view: Rect, scale: f32) -> Option<RgbaImage> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let x0 = (view.min.x * scale).floor().clamp(0.0, w);
    let y0 = (view.min.y * scale).floor().clamp(0.0, h);
    let x1 = (view.max.x * scale).ceil().clamp(0.0, w);
    let y1 = (view.max.y * scale).ceil().clamp(0.0, h);
    if x1 - x0 < 1.0 || y1 - y0 < 1.0 {
        return None;
    }
    Some(image::imageops::crop_imm(img, x0 as u32, y0 as u32, (x1 - x0) as u32, (y1 - y0) as u32).to_image())
}

pub fn encode(img: &RgbaImage, format: image::ImageFormat) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let res = if format == image::ImageFormat::Jpeg {
        image::DynamicImage::ImageRgba8(img.clone()).to_rgb8().write_to(&mut std::io::Cursor::new(&mut out), format)
    } else {
        img.write_to(&mut std::io::Cursor::new(&mut out), format)
    };
    res.map_err(|e| e.to_string())?;
    Ok(out)
}

/// The picture format for a file name (PNG unless it ends in .jpg or .jpeg).
pub fn format_for(path: &std::path::Path) -> image::ImageFormat {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("jpg" | "jpeg") => image::ImageFormat::Jpeg,
        _ => image::ImageFormat::Png,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Obj, ShapeKind};
    use eframe::egui::{Color32, pos2};

    #[test]
    fn renders_edits_and_crops_in_view_space() {
        let mut doc = Doc::new_blank();
        doc.pages[0].objects.push(Obj {
            rect: Rect::from_min_max(pos2(100.0, 100.0), pos2(200.0, 150.0)),
            kind: Kind::Shape { shape: ShapeKind::Rect, stroke: Color32::RED, fill: Some(Color32::RED), width: 0.0, a: pos2(0.0, 0.0), b: pos2(1.0, 1.0) },
        });
        let (img, s) = render_page(&doc, 0, 2.0).unwrap();
        assert_eq!(s, 2.0);
        assert_eq!((img.width(), img.height()), (1190, 1684));
        let red = |p: &image::Rgba<u8>| p[0] > 200 && p[1] < 60 && p[2] < 60;
        assert!(red(img.get_pixel(300, 250)));
        let part = crop(&img, Rect::from_min_max(pos2(90.0, 90.0), pos2(210.0, 160.0)), s).unwrap();
        assert_eq!((part.width(), part.height()), (240, 140));
        assert!(red(part.get_pixel(120, 70)) && !red(part.get_pixel(5, 5)));
        // Rotated a quarter turn, the red box moves to the top right of the view.
        doc.pages[0].rotation = 90;
        let (img, _) = render_page(&doc, 0, 1.0).unwrap();
        assert_eq!((img.width(), img.height()), (842, 595));
        assert!(red(img.get_pixel(842 - 125, 150)));
        assert!(encode(&part, image::ImageFormat::Png).unwrap().starts_with(b"\x89PNG"));
    }
}
