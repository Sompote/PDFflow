//! Background page rasteriser. One worker thread owns the parsed PDF (hayro, pure Rust) and
//! renders only the pages the canvas asks for, newest request set first, so scrolling never
//! blocks the UI and only visible pages occupy memory.

use eframe::egui::{self, ColorImage, Vec2, vec2};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::{RenderCache, RenderSettings};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Largest rendered page side and area, which caps the memory of one page texture.
pub const MAX_SIDE: f32 = 4096.0;
pub const MAX_PIXELS: f32 = 12.0e6;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Job {
    pub page: usize,
    /// Pixels per PDF point.
    pub scale: f32,
    pub thumb: bool,
}

pub struct Done {
    pub job: Job,
    pub image: ColorImage,
}

enum Req {
    Open(Arc<Vec<u8>>, Sender<Result<Vec<Vec2>, String>>),
    Want(Vec<Job>),
}

pub struct Renderer {
    tx: Sender<Req>,
    pub rx: Receiver<Done>,
}

impl Renderer {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, req_rx) = channel();
        let (done_tx, rx) = channel();
        std::thread::Builder::new()
            .name("pdf-render".into())
            .spawn(move || worker(req_rx, done_tx, ctx))
            .expect("spawn render thread");
        Renderer { tx, rx }
    }

    /// Parse a PDF on the worker and return each page's display size in points.
    pub fn open(&self, bytes: Arc<Vec<u8>>) -> Result<Vec<Vec2>, String> {
        let (reply, answer) = channel();
        self.tx.send(Req::Open(bytes, reply)).map_err(|e| e.to_string())?;
        answer.recv().map_err(|e| e.to_string())?
    }

    /// Replace the queue of wanted renders (the first job is rendered first).
    pub fn want(&self, jobs: Vec<Job>) {
        let _ = self.tx.send(Req::Want(jobs));
    }
}

/// Clamp a render scale so one page stays inside the texture memory budget.
pub fn clamp_scale(size: Vec2, scale: f32) -> f32 {
    let side = size.x.max(size.y) * scale;
    let mut s = scale;
    if side > MAX_SIDE {
        s *= MAX_SIDE / side;
    }
    let area = size.x * size.y * s * s;
    if area > MAX_PIXELS {
        s *= (MAX_PIXELS / area).sqrt();
    }
    s
}

fn worker(rx: Receiver<Req>, tx: Sender<Done>, ctx: egui::Context) {
    let mut next: Option<(Arc<Vec<u8>>, Sender<Result<Vec<Vec2>, String>>)> = None;
    loop {
        let (bytes, reply) = match next.take() {
            Some(n) => n,
            None => match rx.recv() {
                Ok(Req::Open(b, r)) => (b, r),
                Ok(Req::Want(_)) => continue,
                Err(_) => return,
            },
        };
        let pdf = match Pdf::new(bytes) {
            Ok(p) => p,
            Err(e) => {
                let msg = match e {
                    hayro::hayro_syntax::LoadPdfError::Decryption(_) => "This PDF is encrypted (password protected).".to_owned(),
                    _ => "The file is not a readable PDF.".to_owned(),
                };
                let _ = reply.send(Err(msg));
                continue;
            }
        };
        let pages = pdf.pages();
        let sizes = pages
            .iter()
            .map(|p| {
                let (w, h) = p.render_dimensions();
                vec2(w, h)
            })
            .collect();
        let _ = reply.send(Ok(sizes));
        let cache = RenderCache::new();
        let settings = InterpreterSettings::default();
        let mut queue: Vec<Job> = Vec::new();
        loop {
            // Block only when idle; otherwise just pick up whatever arrived meanwhile.
            let msg = if queue.is_empty() { rx.recv().ok() } else { rx.try_recv().ok() };
            match msg {
                Some(Req::Want(j)) => {
                    queue = j;
                    continue;
                }
                Some(Req::Open(b, r)) => {
                    next = Some((b, r));
                    break;
                }
                None if queue.is_empty() => return,
                None => {}
            }
            let job = queue.remove(0);
            let Some(page) = pages.get(job.page) else { continue };
            let pix = hayro::render(
                page,
                &cache,
                &settings,
                &RenderSettings { x_scale: job.scale, y_scale: job.scale, bg_color: hayro::vello_cpu::color::palette::css::WHITE, ..Default::default() },
            );
            let size = [pix.width() as usize, pix.height() as usize];
            let image = ColorImage::from_rgba_premultiplied(size, pix.data_as_u8_slice());
            if tx.send(Done { job, image }).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    }
}
