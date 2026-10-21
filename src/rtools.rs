//! Picture (raster) tools, as in PhotoCraft: Rectangular Marquee, Lasso, Crop, Eyedropper,
//! Spot Healing, Brush, Clone Stamp, Eraser, Gradient, Blur and Dodge/Burn. They work on the
//! selected picture only and are disabled otherwise.

use super::*;
use crate::raster::{MAX_EDIT_SIDE, Raster};

pub(super) const RASTER_TOOLS: [Tool; 11] = [
    Tool::Marquee,
    Tool::Lasso,
    Tool::Crop,
    Tool::Eyedropper,
    Tool::Heal,
    Tool::PaintBrush,
    Tool::CloneStamp,
    Tool::Eraser,
    Tool::Gradient,
    Tool::Blur,
    Tool::Dodge,
];

impl Tool {
    pub(super) fn is_raster(self) -> bool {
        RASTER_TOOLS.contains(&self)
    }

    /// Tools that paint dabs along the pointer.
    fn is_dab(self) -> bool {
        matches!(self, Tool::Heal | Tool::PaintBrush | Tool::CloneStamp | Tool::Eraser | Tool::Blur | Tool::Dodge)
    }
}

pub(super) struct RasterOpts {
    /// Brush diameter in picture pixels.
    pub size: f32,
    pub hardness: f32,
    pub opacity: f32,
}

impl Default for RasterOpts {
    fn default() -> Self {
        RasterOpts { size: 30.0, hardness: 0.6, opacity: 1.0 }
    }
}

/// A pixel selection, in picture pixel coordinates.
#[derive(Clone)]
pub(super) enum SelShape {
    Rect(Rect),
    Poly(Vec<Pos2>),
}

impl SelShape {
    pub fn contains(&self, p: Pos2) -> bool {
        match self {
            SelShape::Rect(r) => r.contains(p),
            SelShape::Poly(pts) => crate::path::point_in_polygon(pts, p),
        }
    }

    pub fn bounds(&self) -> Rect {
        match self {
            SelShape::Rect(r) => *r,
            SelShape::Poly(pts) => Rect::from_points(pts),
        }
    }
}

pub(super) struct PixSel {
    pub page: usize,
    pub idx: usize,
    pub shape: SelShape,
}

/// The picture being edited: full-resolution pixels and a matching screen texture that is
/// updated region by region while painting.
pub(super) struct Live {
    pub page: usize,
    pub idx: usize,
    /// Address of the committed `ImageData` these pixels belong to.
    pub base: usize,
    pub raster: Raster,
    pub tex: TextureHandle,
    pub dirty: Option<[u32; 4]>,
}

impl Live {
    fn touch(&mut self, x0: u32, y0: u32, x1: u32, y1: u32) {
        let d = self.dirty.get_or_insert([x0, y0, x1, y1]);
        *d = [d[0].min(x0), d[1].min(y0), d[2].max(x1), d[3].max(y1)];
    }
}

/// Composite `s` over `d` with weight `w` (straight alpha).
fn blend(d: &mut [u8; 4], s: [u8; 4], w: f32) {
    let (sa, da) = (s[3] as f32 / 255.0 * w, d[3] as f32 / 255.0 * (1.0 - w));
    let a = sa + da;
    if a <= 1e-6 {
        d[3] = 0;
        return;
    }
    for i in 0..3 {
        d[i] = ((s[i] as f32 * sa + d[i] as f32 * da) / a).round().clamp(0.0, 255.0) as u8;
    }
    d[3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
}

fn rgba(c: Color32) -> [u8; 4] {
    [c.r(), c.g(), c.b(), 255]
}

impl App {
    /// The selected object when it is a picture.
    pub(super) fn picture_sel(&self) -> Option<(usize, usize, Arc<ImageData>)> {
        let (p, i) = self.sel?;
        match &self.doc.pages.get(p)?.objects.get(i)?.kind {
            Kind::Image(img) => Some((p, i, img.clone())),
            _ => None,
        }
    }

    pub(super) fn live_matches(&self, page: usize, idx: usize) -> bool {
        self.rlive.as_ref().is_some_and(|l| {
            l.page == page
                && l.idx == idx
                && matches!(self.doc.pages.get(page).and_then(|p| p.objects.get(idx)), Some(Obj { kind: Kind::Image(img), .. }) if Arc::as_ptr(img) as usize == l.base)
        })
    }

    /// Load the selected picture's pixels for editing (resampled to at most 2048 px).
    pub(super) fn ensure_live(&mut self, ctx: &egui::Context, page: usize, idx: usize) -> bool {
        if self.live_matches(page, idx) {
            return true;
        }
        let Some(Obj { kind: Kind::Image(img), .. }) = self.doc.pages.get(page).and_then(|p| p.objects.get(idx)) else { return false };
        let raster = match &img.raster {
            Some(r) => r.clone(),
            None => {
                let Some((w, h, data)) = img.rgba() else {
                    self.error = Some("This picture cannot be decoded for editing.".into());
                    return false;
                };
                if w.max(h) > MAX_EDIT_SIDE {
                    let im = image::RgbaImage::from_raw(w, h, data).expect("size");
                    let s = MAX_EDIT_SIDE as f32 / w.max(h) as f32;
                    let (nw, nh) = (((w as f32 * s).round() as u32).max(1), ((h as f32 * s).round() as u32).max(1));
                    let im = image::imageops::resize(&im, nw, nh, image::imageops::FilterType::Triangle);
                    self.status = format!("Picture resampled from {w}×{h} to {nw}×{nh} px for editing.");
                    Raster::from_rgba(nw, nh, im.as_raw())
                } else {
                    Raster::from_rgba(w, h, &data)
                }
            }
        };
        let ci = egui::ColorImage::from_rgba_unmultiplied([raster.w as usize, raster.h as usize], &raster.to_rgba());
        let tex = ctx.load_texture("picture-live", ci, TextureOptions::LINEAR);
        self.rlive = Some(Live { page, idx, base: Arc::as_ptr(img) as usize, raster, tex, dirty: None });
        true
    }

    pub(super) fn ensure_live_for_test(&mut self, ctx: &egui::Context) -> bool {
        let Some((p, i, _)) = self.picture_sel() else { return false };
        self.ensure_live(ctx, p, i)
    }

    /// Store the live pixels as a new version of the picture (one undo step).
    pub(super) fn commit_live(&mut self, before: Vec<PageEntry>) {
        let Some(l) = &mut self.rlive else { return };
        let Some(Obj { kind: Kind::Image(img), .. }) = self.doc.pages.get(l.page).and_then(|p| p.objects.get(l.idx)) else { return };
        let new = Arc::new(ImageData::edited(img.bytes.clone(), l.raster.clone()));
        l.base = Arc::as_ptr(&new) as usize;
        let (p, i) = (l.page, l.idx);
        self.doc.pages[p].objects[i].kind = Kind::Image(new);
        self.doc.push_undo(before);
    }

    /// Send the region painted since the last frame to the screen texture.
    pub(super) fn upload_dirty(&mut self) {
        let Some(l) = &mut self.rlive else { return };
        let Some([x0, y0, x1, y1]) = l.dirty.take() else { return };
        let (x1, y1) = (x1.min(l.raster.w), y1.min(l.raster.h));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let (w, h) = (x1 - x0, y1 - y0);
        let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &l.raster.region(x0, y0, w, h));
        l.tex.set_partial([x0 as usize, y0 as usize], ci, TextureOptions::LINEAR);
    }

    /// Picture size in pixels (the live raster when editing).
    fn picture_px(&self, page: usize, idx: usize) -> Vec2 {
        if self.live_matches(page, idx) {
            let l = self.rlive.as_ref().expect("live");
            return vec2(l.raster.w as f32, l.raster.h as f32);
        }
        match &self.doc.pages[page].objects[idx].kind {
            Kind::Image(img) => vec2(img.px[0] as f32, img.px[1] as f32),
            _ => vec2(1.0, 1.0),
        }
    }

    fn to_px(&self, rects: &[Rect], page: usize, idx: usize, s: Pos2) -> Pos2 {
        let r = self.doc.pages[page].objects[idx].rect;
        let pp = self.to_page(rects, page, s);
        let px = self.picture_px(page, idx);
        pos2((pp.x - r.min.x) / r.width() * px.x, (pp.y - r.min.y) / r.height() * px.y)
    }

    fn px_to_screen(&self, rects: &[Rect], page: usize, idx: usize, p: Pos2) -> Pos2 {
        let r = self.doc.pages[page].objects[idx].rect;
        let px = self.picture_px(page, idx);
        let pp = pos2(r.min.x + p.x / px.x * r.width(), r.min.y + p.y / px.y * r.height());
        rects[page].min + self.doc.pages[page].to_view(pp).to_vec2() * self.zoom
    }

    /// Pixel rect to page rect of the picture.
    fn px_rect_to_page(&self, page: usize, idx: usize, q: Rect) -> Rect {
        let r = self.doc.pages[page].objects[idx].rect;
        let px = self.picture_px(page, idx);
        let f = |p: Pos2| pos2(r.min.x + p.x / px.x * r.width(), r.min.y + p.y / px.y * r.height());
        Rect::from_two_pos(f(q.min), f(q.max))
    }

    /// Press with a picture tool.
    pub(super) fn raster_press(&mut self, ctx: &egui::Context, rects: &[Rect], s: Pos2, alt: bool) {
        let Some((page, idx, _)) = self.picture_sel() else {
            self.status = "Select a picture to use the picture tools.".into();
            return;
        };
        if !self.ensure_live(ctx, page, idx) {
            return;
        }
        let p = self.to_px(rects, page, idx, s);
        match self.tool {
            Tool::Marquee => {
                self.pix_sel = None;
                self.drag = Some(Drag::RSelect { page, idx, start: p });
            }
            Tool::Lasso => {
                self.pix_sel = None;
                self.drag = Some(Drag::RLasso { page, idx, pts: vec![p] });
            }
            Tool::Crop => self.drag = Some(Drag::RCrop { page, idx, start: p }),
            Tool::Gradient => self.drag = Some(Drag::RGradient { page, idx, start: p, cur: p }),
            Tool::Eyedropper => {
                let l = self.rlive.as_ref().expect("live");
                if p.x >= 0.0 && p.y >= 0.0 && p.x < l.raster.w as f32 && p.y < l.raster.h as f32 {
                    let c = l.raster.get(p.x as u32, p.y as u32);
                    self.opts.color = Color32::from_rgb(c[0], c[1], c[2]);
                    self.status = format!("Picked #{:02X}{:02X}{:02X}", c[0], c[1], c[2]);
                }
            }
            Tool::CloneStamp if alt => {
                self.clone_src = Some((page, idx, p));
                self.status = "Clone source set. Paint to copy from it.".into();
            }
            Tool::CloneStamp if !self.clone_src.is_some_and(|c| c.0 == page && c.1 == idx) => {
                self.status = "Alt-click (Option-click) the picture first to set the clone source.".into();
            }
            t if t.is_dab() => {
                let snapshot = self.rlive.as_ref().expect("live").raster.clone();
                let off = self.clone_src.map(|c| c.2 - p);
                let before = self.doc.pages.clone();
                self.apply_dab(p, &snapshot, off, alt);
                self.drag = Some(Drag::RPaint { page, idx, last: p, snapshot, before, off, alt });
            }
            _ => {}
        }
    }

    pub(super) fn raster_drag(&mut self, rects: &[Rect], s: Pos2) {
        let Some(drag) = self.drag.take() else { return };
        let drag = match drag {
            Drag::RPaint { page, idx, mut last, snapshot, before, off, alt } => {
                let p = self.to_px(rects, page, idx, s);
                let step = (self.ropts.size * 0.2).max(1.0);
                let dist = (p - last).length();
                let n = (dist / step).floor() as usize;
                for _ in 0..n {
                    last += (p - last).normalized() * step;
                    self.apply_dab(last, &snapshot, off, alt);
                }
                Drag::RPaint { page, idx, last, snapshot, before, off, alt }
            }
            Drag::RSelect { page, idx, start } => {
                let p = self.to_px(rects, page, idx, s);
                let px = self.picture_px(page, idx);
                let full = Rect::from_min_size(Pos2::ZERO, px);
                self.pix_sel = Some(PixSel { page, idx, shape: SelShape::Rect(Rect::from_two_pos(start, p).intersect(full)) });
                Drag::RSelect { page, idx, start }
            }
            Drag::RLasso { page, idx, mut pts } => {
                let p = self.to_px(rects, page, idx, s);
                if pts.last().is_none_or(|l| (p - *l).length() > 1.0) {
                    pts.push(p);
                }
                Drag::RLasso { page, idx, pts }
            }
            Drag::RCrop { page, idx, start } => {
                let p = self.to_px(rects, page, idx, s);
                let full = Rect::from_min_size(Pos2::ZERO, self.picture_px(page, idx));
                self.crop = Some((page, idx, Rect::from_two_pos(start, p).intersect(full)));
                Drag::RCrop { page, idx, start }
            }
            Drag::RGradient { page, idx, start, .. } => Drag::RGradient { page, idx, start, cur: self.to_px(rects, page, idx, s) },
            other => other,
        };
        self.drag = Some(drag);
    }

    pub(super) fn raster_release(&mut self, drag: Drag) {
        match drag {
            Drag::RPaint { before, .. } => self.commit_live(before),
            Drag::RSelect { .. } => {
                if self.pix_sel.as_ref().is_some_and(|s| s.shape.bounds().area() < 2.0) {
                    self.pix_sel = None;
                }
            }
            Drag::RLasso { page, idx, pts } => {
                self.pix_sel = (pts.len() >= 3).then(|| PixSel { page, idx, shape: SelShape::Poly(pts) });
            }
            Drag::RCrop { .. } => {
                if self.crop.is_some_and(|c| c.2.area() < 4.0) {
                    self.crop = None;
                }
            }
            Drag::RGradient { start, cur, .. } => {
                if (cur - start).length() > 2.0 {
                    let before = self.doc.pages.clone();
                    self.gradient(start, cur);
                    self.commit_live(before);
                }
            }
            _ => {}
        }
    }

    pub(super) fn sel_shape(&self) -> Option<SelShape> {
        let l = self.rlive.as_ref()?;
        self.pix_sel.as_ref().filter(|s| s.page == l.page && s.idx == l.idx).map(|s| s.shape.clone())
    }

    fn apply_dab(&mut self, c: Pos2, snap: &Raster, off: Option<Vec2>, alt: bool) {
        let sel = self.sel_shape();
        let (tool, color) = (self.tool, rgba(self.opts.color));
        let o = &self.ropts;
        let (size, hardness, opacity) = (o.size, o.hardness, o.opacity);
        let Some(l) = &mut self.rlive else { return };
        let r = (size / 2.0).max(0.5);
        let (w, h) = (l.raster.w as i32, l.raster.h as i32);
        let x0 = ((c.x - r).floor() as i32).max(0);
        let y0 = ((c.y - r).floor() as i32).max(0);
        let x1 = ((c.x + r).ceil() as i32).min(w);
        let y1 = ((c.y + r).ceil() as i32).min(h);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        // Spot Healing: the mean colour of a ring just outside the brush replaces the blemish.
        let ring = if tool == Tool::Heal {
            let mut acc = [0f32; 4];
            let n = 48;
            for k in 0..n {
                let a = k as f32 / n as f32 * std::f32::consts::TAU;
                let q = snap.get_clamped((c.x + a.cos() * r * 1.5) as i32, (c.y + a.sin() * r * 1.5) as i32);
                for i in 0..4 {
                    acc[i] += q[i] as f32;
                }
            }
            acc.map(|v| (v / n as f32) as u8)
        } else {
            [0; 4]
        };
        for y in y0..y1 {
            for x in x0..x1 {
                let q = pos2(x as f32 + 0.5, y as f32 + 0.5);
                let d = (q - c).length() / r;
                if d >= 1.0 {
                    continue;
                }
                if sel.as_ref().is_some_and(|s| !s.contains(q)) {
                    continue;
                }
                let fall = if d <= hardness { 1.0 } else { 1.0 - ((d - hardness) / (1.0 - hardness).max(1e-3)).powi(2) };
                let wgt = (fall * opacity).clamp(0.0, 1.0);
                let src = match tool {
                    Tool::CloneStamp => {
                        let v = off.unwrap_or(Vec2::ZERO);
                        let (sx, sy) = ((q.x + v.x).floor() as i32, (q.y + v.y).floor() as i32);
                        if sx < 0 || sy < 0 || sx >= w || sy >= h {
                            continue;
                        }
                        Some(snap.get(sx as u32, sy as u32))
                    }
                    Tool::Blur => {
                        let mut acc = [0u32; 4];
                        for dy in -2..=2 {
                            for dx in -2..=2 {
                                let p = snap.get_clamped(x + dx, y + dy);
                                for i in 0..4 {
                                    acc[i] += p[i] as u32;
                                }
                            }
                        }
                        Some(acc.map(|v| (v / 25) as u8))
                    }
                    Tool::Heal => {
                        // Keep a little of the original texture so the patch is not flat.
                        let o = snap.get(x as u32, y as u32);
                        Some([0, 1, 2, 3].map(|i| ((ring[i] as f32) * 0.85 + o[i] as f32 * 0.15) as u8))
                    }
                    _ => None,
                };
                let px = l.raster.px_mut(x as u32, y as u32);
                match tool {
                    Tool::PaintBrush => blend(px, color, wgt),
                    Tool::Eraser => px[3] = (px[3] as f32 * (1.0 - wgt)).round() as u8,
                    Tool::Dodge => {
                        for i in 0..3 {
                            let v = px[i] as f32;
                            px[i] = if alt { v * (1.0 - 0.25 * wgt) } else { v + (255.0 - v) * 0.25 * wgt }.round() as u8;
                        }
                    }
                    _ => {
                        if let Some(s) = src {
                            blend(px, s, wgt);
                        }
                    }
                }
            }
        }
        l.touch(x0 as u32, y0 as u32, x1 as u32, y1 as u32);
    }

    /// Linear gradient from the brush colour to white along a→b, inside the selection if any.
    fn gradient(&mut self, a: Pos2, b: Pos2) {
        let sel = self.sel_shape();
        let (fg, opacity) = (rgba(self.opts.color), self.ropts.opacity);
        let Some(l) = &mut self.rlive else { return };
        let ab = b - a;
        let len2 = ab.length_sq().max(1e-6);
        for y in 0..l.raster.h {
            for x in 0..l.raster.w {
                let q = pos2(x as f32 + 0.5, y as f32 + 0.5);
                if sel.as_ref().is_some_and(|s| !s.contains(q)) {
                    continue;
                }
                let t = ((q - a).dot(ab) / len2).clamp(0.0, 1.0);
                let c = [0, 1, 2].map(|i| (fg[i] as f32 + (255.0 - fg[i] as f32) * t).round() as u8);
                blend(l.raster.px_mut(x, y), [c[0], c[1], c[2], 255], opacity);
            }
        }
        let (w, h) = (l.raster.w, l.raster.h);
        l.touch(0, 0, w, h);
    }

    /// Run `f` on every selected pixel of the live picture, as one undo step.
    fn edit_selection(&mut self, f: impl Fn(&mut [u8; 4])) {
        let Some(shape) = self.sel_shape() else { return };
        let before = self.doc.pages.clone();
        let Some(l) = &mut self.rlive else { return };
        let b = shape.bounds();
        let (x0, y0) = (b.min.x.floor().max(0.0) as u32, b.min.y.floor().max(0.0) as u32);
        let (x1, y1) = ((b.max.x.ceil() as u32).min(l.raster.w), (b.max.y.ceil() as u32).min(l.raster.h));
        for y in y0..y1 {
            for x in x0..x1 {
                if shape.contains(pos2(x as f32 + 0.5, y as f32 + 0.5)) {
                    f(l.raster.px_mut(x, y));
                }
            }
        }
        l.touch(x0, y0, x1, y1);
        self.commit_live(before);
    }

    pub(super) fn delete_pixels(&mut self) {
        self.edit_selection(|p| p[3] = 0);
    }

    pub(super) fn fill_selection(&mut self) {
        let c = rgba(self.opts.color);
        self.edit_selection(move |p| *p = c);
    }

    pub(super) fn select_all_pixels(&mut self, ctx: &egui::Context) {
        let Some((page, idx, _)) = self.picture_sel() else { return };
        if self.ensure_live(ctx, page, idx) {
            let px = self.picture_px(page, idx);
            self.pix_sel = Some(PixSel { page, idx, shape: SelShape::Rect(Rect::from_min_size(Pos2::ZERO, px)) });
        }
    }

    /// Crop the live picture to a pixel rect; the object shrinks so the kept part stays put.
    pub(super) fn crop_picture(&mut self, q: Rect) {
        let Some(l) = &self.rlive else { return };
        let (page, idx) = (l.page, l.idx);
        let q = Rect::from_min_max(q.min.floor(), q.max.ceil()).intersect(Rect::from_min_size(Pos2::ZERO, vec2(l.raster.w as f32, l.raster.h as f32)));
        if q.width() < 1.0 || q.height() < 1.0 {
            return;
        }
        let before = self.doc.pages.clone();
        let new_rect = self.px_rect_to_page(page, idx, q);
        let raster = l.raster.crop(q.min.x as u32, q.min.y as u32, q.width() as u32, q.height() as u32);
        let Some(Obj { kind: Kind::Image(img), .. }) = self.doc.pages[page].objects.get(idx) else { return };
        let new = Arc::new(ImageData::edited(img.bytes.clone(), raster));
        let o = &mut self.doc.pages[page].objects[idx];
        o.kind = Kind::Image(new);
        o.rect = new_rect;
        self.doc.push_undo(before);
        self.rlive = None;
        self.pix_sel = None;
        self.crop = None;
        self.status = format!("Cropped to {}×{} px.", q.width() as u32, q.height() as u32);
    }

    /// Copy the selected pixels into a new picture above this one (⌘J).
    pub(super) fn picture_from_selection(&mut self) {
        let Some(shape) = self.sel_shape() else { return };
        let Some(l) = &self.rlive else { return };
        let (page, idx) = (l.page, l.idx);
        let b = shape.bounds();
        let q = Rect::from_min_max(b.min.floor(), b.max.ceil()).intersect(Rect::from_min_size(Pos2::ZERO, vec2(l.raster.w as f32, l.raster.h as f32)));
        if q.width() < 1.0 || q.height() < 1.0 {
            return;
        }
        let mut raster = l.raster.crop(q.min.x as u32, q.min.y as u32, q.width() as u32, q.height() as u32);
        for y in 0..raster.h {
            for x in 0..raster.w {
                if !shape.contains(pos2(q.min.x + x as f32 + 0.5, q.min.y + y as f32 + 0.5)) {
                    raster.px_mut(x, y)[3] = 0;
                }
            }
        }
        let rect = self.px_rect_to_page(page, idx, q);
        let bytes = match &self.doc.pages[page].objects[idx].kind {
            Kind::Image(img) => img.bytes.clone(),
            _ => return,
        };
        self.doc.checkpoint();
        let obj = Obj { rect, kind: Kind::Image(Arc::new(ImageData::edited(bytes, raster))) };
        self.doc.pages[page].objects.insert(idx + 1, obj);
        self.sel = Some((page, idx + 1));
        self.pix_sel = None;
        self.rlive = None;
        self.status = "New picture made from the selection.".into();
    }

    /// Drop editing state that no longer matches the selection (frees the full-size pixels).
    pub(super) fn raster_housekeeping(&mut self) {
        let pic = self.picture_sel().map(|(p, i, _)| (p, i));
        if self.tool.is_raster() && pic.is_none() {
            self.tool = Tool::Move;
        }
        if let Some(l) = &self.rlive {
            if pic != Some((l.page, l.idx)) || !self.live_matches(l.page, l.idx) {
                self.rlive = None;
            }
        }
        if self.pix_sel.as_ref().is_some_and(|s| pic != Some((s.page, s.idx))) {
            self.pix_sel = None;
        }
        if self.crop.is_some_and(|c| pic != Some((c.0, c.1))) {
            self.crop = None;
        }
    }

    /// Live texture for an object that is being edited, if any.
    pub(super) fn live_texture(&self, page: usize, idx: usize) -> Option<egui::TextureId> {
        self.live_matches(page, idx).then(|| self.rlive.as_ref().expect("live").tex.id())
    }

    /// Selection outline, crop frame, gradient line, lasso and brush cursor.
    pub(super) fn raster_overlay(&self, painter: &egui::Painter, rects: &[Rect], pointer: Option<Pos2>) {
        let t = Tokens::get(painter.ctx());
        let ants = |pts: Vec<Pos2>| {
            if pts.len() < 2 {
                return;
            }
            let mut closed = pts.clone();
            closed.push(pts[0]);
            painter.add(Shape::line(closed.clone(), Stroke::new(1.0, Color32::BLACK)));
            painter.extend(Shape::dashed_line(&closed, Stroke::new(1.0, Color32::WHITE), 4.0, 4.0));
        };
        let rect_pts = |page: usize, idx: usize, r: Rect| {
            [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()].map(|p| self.px_to_screen(rects, page, idx, p)).to_vec()
        };
        if let Some(s) = &self.pix_sel {
            if s.page < self.doc.pages.len() && s.idx < self.doc.pages[s.page].objects.len() {
                match &s.shape {
                    SelShape::Rect(r) => ants(rect_pts(s.page, s.idx, *r)),
                    SelShape::Poly(p) => ants(p.iter().map(|q| self.px_to_screen(rects, s.page, s.idx, *q)).collect()),
                }
            }
        }
        match &self.drag {
            Some(Drag::RLasso { page, idx, pts }) => {
                let pts: Vec<Pos2> = pts.iter().map(|q| self.px_to_screen(rects, *page, *idx, *q)).collect();
                painter.add(Shape::line(pts, Stroke::new(1.0, t.accent)));
            }
            Some(Drag::RGradient { page, idx, start, cur }) => {
                let (a, b) = (self.px_to_screen(rects, *page, *idx, *start), self.px_to_screen(rects, *page, *idx, *cur));
                painter.line_segment([a, b], Stroke::new(1.5, t.accent));
                painter.circle_filled(a, 3.5, self.opts.color);
                painter.circle_stroke(b, 3.5, Stroke::new(1.0, t.accent));
            }
            _ => {}
        }
        if let Some((page, idx, q)) = self.crop {
            if idx < self.doc.pages[page].objects.len() {
                let full = rect_pts(page, idx, Rect::from_min_size(Pos2::ZERO, self.picture_px(page, idx)));
                let fr = Rect::from_points(&full);
                let cr = Rect::from_points(&rect_pts(page, idx, q));
                let shade = Color32::from_black_alpha(140);
                for r in [
                    Rect::from_min_max(fr.min, pos2(fr.max.x, cr.min.y)),
                    Rect::from_min_max(pos2(fr.min.x, cr.max.y), fr.max),
                    Rect::from_min_max(pos2(fr.min.x, cr.min.y), pos2(cr.min.x, cr.max.y)),
                    Rect::from_min_max(pos2(cr.max.x, cr.min.y), pos2(fr.max.x, cr.max.y)),
                ] {
                    painter.rect_filled(r, 0.0, shade);
                }
                painter.rect_stroke(cr, 0.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Middle);
                for k in 1..3 {
                    let f = k as f32 / 3.0;
                    let x = cr.min.x + cr.width() * f;
                    let y = cr.min.y + cr.height() * f;
                    painter.line_segment([pos2(x, cr.min.y), pos2(x, cr.max.y)], Stroke::new(0.5, Color32::from_white_alpha(150)));
                    painter.line_segment([pos2(cr.min.x, y), pos2(cr.max.x, y)], Stroke::new(0.5, Color32::from_white_alpha(150)));
                }
            }
        }
        if let Some((page, idx, src)) = self.clone_src {
            if self.tool == Tool::CloneStamp && self.live_matches(page, idx) {
                let c = self.px_to_screen(rects, page, idx, src);
                painter.line_segment([c - vec2(6.0, 0.0), c + vec2(6.0, 0.0)], Stroke::new(1.5, t.accent));
                painter.line_segment([c - vec2(0.0, 6.0), c + vec2(0.0, 6.0)], Stroke::new(1.5, t.accent));
            }
        }
        // Brush outline at the pointer.
        if let (Some(s), true, Some((page, idx, _))) = (pointer, self.tool.is_dab(), self.picture_sel()) {
            let r = self.doc.pages[page].objects[idx].rect;
            let px = self.picture_px(page, idx);
            let radius = self.ropts.size / 2.0 * r.width() / px.x * self.zoom;
            painter.circle_stroke(s, radius.max(1.0), Stroke::new(1.0, Color32::BLACK));
            painter.circle_stroke(s, (radius - 1.0).max(0.5), Stroke::new(1.0, Color32::WHITE));
        }
    }

    /// Options bar for picture tools.
    pub(super) fn raster_options(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        if self.picture_sel().is_none() {
            ui.label(egui::RichText::new("Select a picture to use the picture tools.").color(t.text_faint));
            return;
        }
        let ctx = ui.ctx().clone();
        match self.tool {
            Tool::Marquee | Tool::Lasso => {
                let has = self.sel_shape().is_some();
                if widgets::secondary_button(ui, "Select All", 0.0).clicked() {
                    self.select_all_pixels(&ctx);
                }
                ui.add_enabled_ui(has, |ui| {
                    if widgets::secondary_button(ui, "Deselect", 0.0).clicked() {
                        self.pix_sel = None;
                    }
                    if widgets::secondary_button(ui, "Delete", 0.0).on_hover_text("Erase the selected pixels (⌫)").clicked() {
                        self.delete_pixels();
                    }
                    if widgets::secondary_button(ui, "Fill", 0.0).on_hover_text("Fill with the brush colour").clicked() {
                        self.fill_selection();
                    }
                    if widgets::secondary_button(ui, "Crop to Selection", 0.0).clicked() {
                        if let Some(s) = self.sel_shape() {
                            self.crop_picture(s.bounds());
                        }
                    }
                    if widgets::secondary_button(ui, "New Picture", 0.0).on_hover_text("Copy the selection to a new picture (⌘J)").clicked() {
                        self.picture_from_selection();
                    }
                });
            }
            Tool::Crop => {
                if let Some((_, _, q)) = self.crop {
                    ui.label(egui::RichText::new(format!("{} × {} px", q.width().round(), q.height().round())).color(t.text_dim));
                    if widgets::primary_button(ui, "Apply", 70.0).on_hover_text("Enter").clicked() {
                        self.crop_picture(q);
                    }
                    if widgets::secondary_button(ui, "Cancel", 70.0).on_hover_text("Esc").clicked() {
                        self.crop = None;
                    }
                } else {
                    ui.label(egui::RichText::new("Drag over the picture to choose what to keep.").color(t.text_faint));
                }
            }
            Tool::Eyedropper => {
                option_label(ui, "Sampled:");
                let (r, _) = ui.allocate_exact_size(vec2(36.0, 20.0), Sense::hover());
                ui.painter().rect_filled(r, 3.0, self.opts.color);
                let c = self.opts.color;
                ui.label(format!("#{:02X}{:02X}{:02X}", c.r(), c.g(), c.b()));
            }
            Tool::Gradient => {
                option_label(ui, "From:");
                color_row(ui, &mut self.opts.color);
                ui.label(egui::RichText::new("to white").color(t.text_dim));
                vline(ui, 22.0);
                option_label(ui, "Opacity:");
                let mut o = self.ropts.opacity * 100.0;
                if ui.add(egui::DragValue::new(&mut o).range(1.0..=100.0).suffix("%").max_decimals(0)).changed() {
                    self.ropts.opacity = o / 100.0;
                }
            }
            _ => {
                option_label(ui, "Size:");
                ui.add(egui::DragValue::new(&mut self.ropts.size).range(1.0..=500.0).speed(0.5).suffix(" px").max_decimals(0)).on_hover_text("[ and ] change the size");
                option_label(ui, "Hardness:");
                let mut hd = self.ropts.hardness * 100.0;
                if ui.add(egui::DragValue::new(&mut hd).range(0.0..=100.0).suffix("%").max_decimals(0)).changed() {
                    self.ropts.hardness = hd / 100.0;
                }
                option_label(ui, "Opacity:");
                let mut o = self.ropts.opacity * 100.0;
                if ui.add(egui::DragValue::new(&mut o).range(1.0..=100.0).suffix("%").max_decimals(0)).changed() {
                    self.ropts.opacity = o / 100.0;
                }
                vline(ui, 22.0);
                match self.tool {
                    Tool::PaintBrush => {
                        color_row(ui, &mut self.opts.color);
                    }
                    Tool::CloneStamp => {
                        ui.label(egui::RichText::new("Alt-click to set the source, then paint.").color(t.text_faint));
                    }
                    Tool::Dodge => {
                        ui.label(egui::RichText::new("Paint to lighten; hold Alt to darken (Burn).").color(t.text_faint));
                    }
                    Tool::Heal => {
                        ui.label(egui::RichText::new("Paint over a spot to blend it with its surroundings.").color(t.text_faint));
                    }
                    Tool::Eraser => {
                        ui.label(egui::RichText::new("Erases to transparent.").color(t.text_faint));
                    }
                    _ => {}
                }
            }
        }
        vline(ui, 22.0);
        self.adjust_button(ui);
    }
}
