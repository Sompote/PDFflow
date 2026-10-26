//! The editor window: Photoshop layout (menu bar, options bar, tool column, canvas, dock).

use crate::doc::{A4, Doc, ImageData, Kind, Obj, PageEntry, PageSrc, ShapeKind, ink_from_points};
use crate::render::{Job, Renderer, clamp_scale};
use crate::sig::{self, Signature};
use crate::theme::{self, ThemeKind, Tokens, doc_family};
use crate::widgets::{self, color_row, option_label, vline};
use crate::{export, icons, text};
use eframe::egui::{
    self, Color32, CursorIcon, Event, FontId, Id, Key, KeyboardShortcut, Modifiers, Pos2, Rect, Sense, Shape, Stroke, StrokeKind,
    TextureHandle, TextureOptions, Ui, Vec2, ViewportBuilder, ViewportCommand, ViewportId, pos2, vec2,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

#[path = "pen.rs"]
mod pen;
#[path = "rtools.rs"]
mod rtools;
#[path = "signui.rs"]
mod signui;
#[path = "notes.rs"]
mod notes;
#[path = "adjui.rs"]
mod adjui;
#[path = "seltext.rs"]
mod seltext;
#[path = "agent.rs"]
mod agent;
use adjui::AdjustDialog;
use pen::{Part, PenDraft};
use rtools::{Live, PixSel, RasterOpts};
use signui::{CreateDialog, ImportDialog, SignDialog, TrustDialog};
use notes::NoteEdit;
#[cfg(test)]
pub(crate) use seltext::Markup;
use seltext::{Snap, TextSel};
#[cfg(test)]
pub(crate) fn notes_without_comments_for_test(b: &[u8]) -> Option<Vec<u8>> {
    notes::without_comments(b)
}
#[cfg(test)]
pub(crate) mod signui_dialog {
    pub(crate) use super::signui::{ImageChoice, SignDialog};
}

/// Space between pages and around the page column, in PDF points.
const GAP: f32 = 16.0;
const MARGIN: f32 = 24.0;
const MAX_THUMBS: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Move,
    Hand,
    SelectText,
    Snapshot,
    Text,
    Brush,
    Highlighter,
    PenPath,
    FreeformPen,
    CurvaturePen,
    AddAnchor,
    DeleteAnchor,
    ConvertPoint,
    Marquee,
    Lasso,
    Crop,
    Eyedropper,
    Heal,
    PaintBrush,
    CloneStamp,
    Eraser,
    Gradient,
    Blur,
    Dodge,
    Line,
    Arrow,
    Rect,
    Ellipse,
    Whiteout,
    Signature,
    Comment,
}

const TOOLS: &[(Tool, &str, &str, Option<Key>)] = &[
    (Tool::Move, "mouse-pointer-2", "Move (V)", Some(Key::V)),
    (Tool::Hand, "hand", "Hand (H)", Some(Key::H)),
    (Tool::SelectText, "text-cursor", "Select Text (D)", Some(Key::D)),
    (Tool::Snapshot, "camera", "Snapshot: copy an area as a picture", None),
    (Tool::Text, "type", "Text (T)", Some(Key::T)),
    (Tool::Brush, "pencil", "Brush (B)", Some(Key::B)),
    (Tool::Highlighter, "highlighter", "Highlighter (K)", Some(Key::K)),
    (Tool::PenPath, "pen-tool", "Pen (P)", Some(Key::P)),
    (Tool::FreeformPen, "pen-freeform", "Freeform Pen (P)", Some(Key::P)),
    (Tool::CurvaturePen, "pen-curvature", "Curvature Pen (P)", Some(Key::P)),
    (Tool::AddAnchor, "pen-add-anchor", "Add Anchor Point (P)", Some(Key::P)),
    (Tool::DeleteAnchor, "pen-delete-anchor", "Delete Anchor Point (P)", Some(Key::P)),
    (Tool::ConvertPoint, "convert-point", "Convert Point (P)", Some(Key::P)),
    (Tool::Line, "slash", "Line (L)", Some(Key::L)),
    (Tool::Arrow, "move-up-right", "Arrow (A)", Some(Key::A)),
    (Tool::Rect, "square", "Rectangle (R)", Some(Key::R)),
    (Tool::Ellipse, "circle", "Ellipse (E)", Some(Key::E)),
    (Tool::Whiteout, "eraser", "Whiteout: cover content with white (W)", Some(Key::W)),
    (Tool::Signature, "signature", "Signature (S)", Some(Key::S)),
    (Tool::Comment, "message-square-text", "Comment (Z)", Some(Key::Z)),
    (Tool::Marquee, "square-dashed", "Rectangular Marquee (M)", Some(Key::M)),
    (Tool::Lasso, "lasso", "Lasso (Q)", Some(Key::Q)),
    (Tool::Crop, "crop", "Crop (C)", Some(Key::C)),
    (Tool::Eyedropper, "pipette", "Eyedropper (I)", Some(Key::I)),
    (Tool::Heal, "bandage", "Spot Healing Brush (J)", Some(Key::J)),
    (Tool::PaintBrush, "brush", "Brush (N)", Some(Key::N)),
    (Tool::CloneStamp, "stamp", "Clone Stamp (Y)", Some(Key::Y)),
    (Tool::Eraser, "eraser-background", "Eraser (X)", Some(Key::X)),
    (Tool::Gradient, "blend", "Gradient (F)", Some(Key::F)),
    (Tool::Blur, "droplet", "Blur (U)", Some(Key::U)),
    (Tool::Dodge, "lollipop", "Dodge / Burn (O)", Some(Key::O)),
];

impl Tool {
    fn name(self) -> &'static str {
        TOOLS.iter().find(|t| t.0 == self).map(|t| t.2.split([':', '(']).next().unwrap_or("").trim_end()).unwrap_or("")
    }
    fn icon(self) -> &'static str {
        TOOLS.iter().find(|t| t.0 == self).map(|t| t.1).unwrap_or("")
    }
}

#[derive(Clone)]
struct Opts {
    color: Color32,
    width: f32,
    fill: Option<Color32>,
    text_color: Color32,
    text_size: f32,
    bold: bool,
    hl_color: Color32,
    hl_width: f32,
    sig_color: Color32,
}

enum Drag {
    Pan,
    Move { page: usize, idx: usize, start: Pos2, orig: Rect, before: Vec<PageEntry>, moved: bool },
    Resize { page: usize, idx: usize, handle: usize, orig: Obj, before: Vec<PageEntry> },
    Draw { page: usize, pts: Vec<Pos2> },
    Create { page: usize, start: Pos2, cur: Pos2 },
    VScroll { grab: f32 },
    SignBox { page: usize, start: Pos2, cur: Pos2 },
    PenPull { page: usize },
    Freeform { page: usize, pts: Vec<Pos2> },
    PathPoint { page: usize, idx: usize, node: usize, part: Part, symmetric: bool, before: Vec<PageEntry>, orig: Vec<crate::path::Node>, closed: bool, moved: bool },
    RPaint { page: usize, idx: usize, last: Pos2, snapshot: crate::raster::Raster, before: Vec<PageEntry>, off: Option<Vec2>, alt: bool },
    RSelect { page: usize, idx: usize, start: Pos2 },
    RLasso { page: usize, idx: usize, pts: Vec<Pos2> },
    RCrop { page: usize, idx: usize, start: Pos2 },
    RGradient { page: usize, idx: usize, start: Pos2, cur: Pos2 },
    /// Select Text: `anchor` is the caret where the drag began; Alt drags a column box.
    TextSel { page: usize, anchor: Option<usize>, start: Pos2, column: bool },
    Snap { page: usize, start: Pos2, cur: Pos2 },
}

struct Editing {
    page: usize,
    idx: usize,
    before: Vec<PageEntry>,
    original: String,
    focus: bool,
}

/// What waits for the "Save changes?" answer: closing this window.
enum Pending {
    Close,
}

/// A page thumbnail being dragged, seen by every window: the source window updates it each
/// frame, the window under the pointer offers an insertion point, and the root window copies
/// the page when the button is released over another window.
#[derive(Default)]
pub(crate) struct Shared {
    drag: Option<LiveDrag>,
    /// The window under the pointer and the page index to insert before.
    target: Option<(u64, usize)>,
    /// Every open window: number, viewport and title, for the Window menu.
    windows: Vec<(u64, ViewportId, String)>,
    /// Frame counter of the root window, to drop a stale drag.
    frame: u64,
}

struct LiveDrag {
    from: u64,
    page: usize,
    /// Pointer in screen points (the OS coordinate space shared by all windows).
    screen: Pos2,
    released: bool,
    /// The source window dropped the page inside itself (a reorder), so no copy follows.
    handled: bool,
    frame: u64,
}

/// Where this window accepts a dropped page: its Pages panel or its Page Grid, in window
/// coordinates, recorded each frame.
enum DropZone {
    List { area: Rect, rows: Vec<(usize, Rect)> },
    Grid { area: Rect, left: f32, cols: usize, cell: Vec2, n: usize },
}

pub struct App {
    /// Window number: 0 for the first window, then one more for each window opened.
    win: u64,
    /// The other windows (each a document of its own), kept by the first window.
    others: Vec<App>,
    /// Asks the first window to stop showing this one.
    closed: bool,
    /// Close this window as if its close button were pressed (⌘W).
    close_me: bool,
    /// The window may go now (unsaved changes settled); the first window decides what follows.
    want_close: bool,
    /// Quit the whole application (File › Quit).
    quit: bool,
    quitting: bool,
    quit_cancelled: bool,
    /// Windows to open: a file, or `None` for an empty document.
    spawn: Vec<Option<PathBuf>>,
    /// Position and size the window is created with.
    spawn_at: Option<(Pos2, Vec2)>,
    first_frame: bool,
    shared: Shared,
    /// This window's inner and outer rect on screen, as the OS last reported them.
    inner_rect: Rect,
    outer_rect: Rect,
    /// A page being dragged from the Pages panel.
    thumb_drag: Option<usize>,
    /// Set in the frame a page drag ended: the page and whether it was dropped in this window.
    drag_released: Option<(usize, bool)>,
    drop_zone: Option<DropZone>,
    /// Every open window, for the Window menu.
    window_list: Vec<(u64, ViewportId, String)>,
    pub(crate) doc: Doc,
    renderer: Renderer,
    page_tex: HashMap<usize, (f32, TextureHandle)>,
    thumbs: HashMap<usize, (f32, TextureHandle)>,
    thumb_want: Vec<Job>,
    thumb_seen: HashSet<usize>,
    last_want: Vec<Job>,
    pub(crate) zoom: f32,
    scroll: Vec2,
    fit_on_next: bool,
    view: Rect,
    pub(crate) tool: Tool,
    opts: Opts,
    current: usize,
    sel: Option<(usize, usize)>,
    drag: Option<Drag>,
    editing: Option<Editing>,
    prop_session: Option<(usize, usize)>,
    sigs: Vec<Signature>,
    sig_sel: usize,
    sig_pad: Option<sig::Pad>,
    clipboard: Option<Obj>,
    pending: Option<Pending>,
    allow_close: bool,
    error: Option<String>,
    status: String,
    theme: ThemeKind,
    dock_tabs: [usize; 3],
    last_title: String,
    scroll_thumbs_to: Option<usize>,
    frames: u32,
    grid: bool,
    grid_size: f32,
    grid_drag: Option<usize>,
    pen: Option<PenDraft>,
    pen_last: Tool,
    flyout: Option<Pos2>,
    flyout_frame: u64,
    rlive: Option<Live>,
    pix_sel: Option<PixSel>,
    crop: Option<(usize, usize, Rect)>,
    clone_src: Option<(usize, usize, Pos2)>,
    ropts: RasterOpts,
    two_cols: bool,
    brand: Option<TextureHandle>,
    ids: Vec<crate::sig::IdEntry>,
    id_sel: usize,
    sign_dlg: Option<SignDialog>,
    create_dlg: Option<CreateDialog>,
    import_dlg: Option<ImportDialog>,
    delete_id: Option<usize>,
    sig_reports: Vec<crate::dsig::SigReport>,
    sig_panel: bool,
    confirm_unsign: Option<bool>,
    unsign_ok: bool,
    note_edit: Option<NoteEdit>,
    author: String,
    note_color: Color32,
    trusted: Vec<(PathBuf, x509_cert::Certificate)>,
    trust_dlg: Option<TrustDialog>,
    sign_placing: Option<SignDialog>,
    adjust_dlg: Option<AdjustDialog>,
    /// Text of the source pages, extracted when first needed (key: source page index).
    text_cache: HashMap<usize, Arc<crate::textlayer::PageText>>,
    text_sel: Option<TextSel>,
    snap: Option<Snap>,
    snap_dpi: f32,
    /// The MCP server agents drive the editor through, when it could start.
    agent: Option<crate::mcp::Server>,
    agent_note: Option<(String, f64)>,
    /// Full Screen (View menu): the window fills the screen and the chrome collapses to one
    /// bar of tools and tool options above the canvas, so drawing continues as before.
    fullscreen: bool,
    /// The window's full-screen state as the OS last reported it, so the green button and
    /// the View menu stay in step.
    os_fullscreen: Option<bool>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, ThemeKind::ProMedium);
        let mut app = Self::create(&cc.egui_ctx, 0, true);
        #[cfg(target_os = "macos")]
        crate::macopen::set_context(&cc.egui_ctx);
        if let Some(p) = path {
            app.open_path(p);
        }
        app
    }

    /// Another window, opened from `self`: it starts with the same tool settings, author,
    /// signatures and identities, and is placed a little below and to the right of `self`.
    fn child(&self, ctx: &egui::Context, win: u64, path: Option<PathBuf>) -> App {
        let mut w = Self::create(ctx, win, false);
        w.theme = self.theme;
        w.opts = self.opts.clone();
        w.ropts = self.ropts.clone();
        w.author = self.author.clone();
        w.note_color = self.note_color;
        w.sigs = self.sigs.clone();
        w.sig_sel = self.sig_sel;
        w.ids = self.ids.clone();
        w.id_sel = self.id_sel;
        w.trusted = self.trusted.clone();
        w.snap_dpi = self.snap_dpi;
        w.dock_tabs = self.dock_tabs;
        let size = if self.inner_rect.is_positive() { self.inner_rect.size() } else { vec2(1280.0, 820.0) };
        let at = if self.outer_rect.is_positive() { self.outer_rect.min + vec2(30.0, 30.0) } else { pos2(60.0, 60.0) };
        w.spawn_at = Some((at, size));
        if let Some(p) = path {
            w.open_path(p);
        }
        w
    }

    fn create(ctx: &egui::Context, win: u64, root: bool) -> Self {
        App {
            win,
            others: Vec::new(),
            closed: false,
            close_me: false,
            want_close: false,
            quit: false,
            quitting: false,
            quit_cancelled: false,
            spawn: Vec::new(),
            spawn_at: None,
            first_frame: true,
            shared: Shared::default(),
            inner_rect: Rect::NOTHING,
            outer_rect: Rect::NOTHING,
            thumb_drag: None,
            drag_released: None,
            drop_zone: None,
            window_list: Vec::new(),
            doc: Doc::new_blank(),
            renderer: Renderer::new(ctx.clone()),
            page_tex: HashMap::new(),
            thumbs: HashMap::new(),
            thumb_want: Vec::new(),
            thumb_seen: HashSet::new(),
            last_want: Vec::new(),
            zoom: 1.0,
            scroll: Vec2::ZERO,
            fit_on_next: true,
            view: Rect::NOTHING,
            tool: Tool::Move,
            opts: Opts {
                color: Color32::from_rgb(200, 30, 30),
                width: 2.0,
                fill: None,
                text_color: Color32::BLACK,
                text_size: 14.0,
                bold: false,
                hl_color: Color32::from_rgb(255, 220, 0),
                hl_width: 12.0,
                sig_color: Color32::from_rgb(20, 50, 140),
            },
            current: 0,
            sel: None,
            drag: None,
            editing: None,
            prop_session: None,
            sigs: sig::load(),
            sig_sel: 0,
            sig_pad: None,
            clipboard: None,
            pending: None,
            allow_close: false,
            error: None,
            status: "Open a PDF (⌘O) or drop one on the window.".into(),
            theme: ThemeKind::ProMedium,
            dock_tabs: [0; 3],
            last_title: String::new(),
            scroll_thumbs_to: None,
            frames: 0,
            grid: false,
            grid_size: 150.0,
            grid_drag: None,
            pen: None,
            pen_last: Tool::PenPath,
            flyout: None,
            flyout_frame: 0,
            rlive: None,
            pix_sel: None,
            crop: None,
            clone_src: None,
            ropts: RasterOpts::default(),
            two_cols: false,
            brand: None,
            ids: crate::sig::load_ids(),
            id_sel: 0,
            sign_dlg: None,
            create_dlg: None,
            import_dlg: None,
            delete_id: None,
            sig_reports: Vec::new(),
            sig_panel: false,
            confirm_unsign: None,
            unsign_ok: false,
            note_edit: None,
            author: notes::default_author(),
            note_color: notes::NOTE_COLORS[0],
            trusted: crate::sig::load_trusted().into_iter().filter_map(|(p, b)| <x509_cert::Certificate as der::Decode>::from_der(&b).ok().map(|c| (p, c))).collect(),
            trust_dlg: None,
            sign_placing: None,
            adjust_dlg: None,
            text_cache: HashMap::new(),
            text_sel: None,
            snap: None,
            fullscreen: false,
            os_fullscreen: None,
            snap_dpi: 150.0,
            agent: if cfg!(test) || !root { None } else { crate::mcp::Server::start(ctx.clone()) },
            agent_note: None,
        }
    }

    /// An id for a widget of this window, so two windows never share widget state.
    fn wid(&self, salt: impl std::hash::Hash + std::fmt::Debug) -> Id {
        Id::new(("pdfflow-window", self.win)).with(salt)
    }

    fn viewport_id(&self) -> ViewportId {
        if self.win == 0 { ViewportId::ROOT } else { ViewportId::from_hash_of(("pdfflow-window", self.win)) }
    }

    /// An empty, untouched document (a new window's start), which an opened file may replace.
    fn is_fresh(&self) -> bool {
        self.doc.path.is_none() && self.doc.source.is_none() && !self.doc.dirty && !self.doc.can_undo() && self.doc.pages.len() == 1 && self.doc.pages[0].objects.is_empty()
    }

    /// Open a file (asking for one when `path` is `None`): into this window while it still
    /// shows an untouched empty document, else into a new window.
    fn open_or_spawn(&mut self, path: Option<PathBuf>) {
        let path = match path {
            Some(p) => p,
            None => match rfd::FileDialog::new().add_filter("PDF", &["pdf"]).pick_file() {
                Some(p) => p,
                None => return,
            },
        };
        if self.is_fresh() {
            self.open_path(path);
        } else {
            self.spawn.push(Some(path));
        }
    }

    // ----------------------------------------------------------------------------------- files

    pub(crate) fn trusted_certs(&self) -> Vec<x509_cert::Certificate> {
        self.trusted.iter().map(|(_, c)| c.clone()).collect()
    }

    fn reset_view(&mut self) {
        self.page_tex.clear();
        self.thumbs.clear();
        self.last_want.clear();
        self.sel = None;
        self.editing = None;
        self.drag = None;
        self.current = 0;
        self.scroll = Vec2::ZERO;
        self.fit_on_next = true;
        self.prop_session = None;
        self.grid = false;
        self.grid_drag = None;
        self.pen = None;
        self.adjust_dlg = None;
        self.rlive = None;
        self.pix_sel = None;
        self.crop = None;
        self.clone_src = None;
        self.sig_reports.clear();
        self.sig_panel = false;
        self.note_edit = None;
        self.text_cache.clear();
        self.text_sel = None;
    }

    fn open_path(&mut self, path: PathBuf) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => Arc::new(b),
            Err(e) => return self.error = Some(format!("Cannot read {}: {e}", path.display())),
        };
        // The renderer gets the file without its comments; the editor draws those itself.
        let render = notes::without_comments(&bytes).map(Arc::new).unwrap_or_else(|| bytes.clone());
        match self.renderer.open(render) {
            Ok(sizes) if !sizes.is_empty() => {
                self.doc = Doc::from_pdf(Some(path), bytes.clone(), &sizes);
                self.reset_view();
                self.sig_reports = crate::dsig::verify(&bytes, &self.trusted_certs());
                self.import_comments(&bytes);
                self.status = format!("Opened {} ({} pages)", self.doc.title(), self.doc.pages.len());
            }
            Ok(_) => self.error = Some("The PDF has no pages.".into()),
            Err(e) => self.error = Some(e),
        }
    }

    fn run_pending(&mut self, action: Pending) {
        match action {
            Pending::Close => {
                self.allow_close = true;
            }
        }
    }

    /// Copy pages from another document in before page `at`: pages of its source PDF are
    /// appended to this document's source (see `merge`), blank pages are taken as they are,
    /// and the placed objects come along. One undo step; returns how many pages were added.
    pub(crate) fn import_pages(&mut self, mut entries: Vec<PageEntry>, src: Option<Arc<Vec<u8>>>, at: usize) -> Result<usize, String> {
        if entries.is_empty() {
            return Ok(0);
        }
        let originals: Vec<usize> = entries.iter().filter_map(|e| if let PageSrc::Original(i) = e.src { Some(i) } else { None }).collect();
        if !originals.is_empty() {
            let src = src.ok_or("The other document has no source PDF.")?;
            let base = self.doc.source.as_ref().map(|b| b.as_slice());
            let (merged, offset) = crate::merge::merge_pages(base, &src, &originals)?;
            let merged = Arc::new(merged);
            let render = notes::without_comments(&merged).map(Arc::new).unwrap_or_else(|| merged.clone());
            let sizes = match self.renderer.open(render) {
                Ok(s) if s.len() == offset + originals.len() => s,
                other => {
                    // Put the renderer back on the document it had.
                    if let Some(old) = &self.doc.source {
                        let r = notes::without_comments(old).map(Arc::new).unwrap_or_else(|| old.clone());
                        let _ = self.renderer.open(r);
                    }
                    return Err(match other {
                        Err(e) => e,
                        Ok(_) => "The merged PDF has an unexpected number of pages.".into(),
                    });
                }
            };
            let mut k = 0;
            for e in &mut entries {
                if let PageSrc::Original(_) = e.src {
                    e.src = PageSrc::Original(offset + k);
                    e.size = sizes[offset + k];
                    k += 1;
                }
            }
            self.doc.source = Some(merged);
            self.doc.source_pages = sizes.len();
            self.doc.source_notes.resize(offset, Vec::new());
            for e in &entries {
                if let PageSrc::Original(_) = e.src {
                    self.doc.source_notes.push(e.objects.iter().filter_map(crate::doc::note_key).collect());
                }
            }
            self.page_tex.clear();
            self.thumbs.clear();
            self.last_want.clear();
        }
        let n = entries.len();
        let at = at.min(self.doc.pages.len());
        self.page_op(|pages, cur| {
            for (k, e) in entries.into_iter().enumerate() {
                pages.insert(at + k, e);
            }
            *cur = at;
        });
        self.scroll_thumbs_to = Some(at);
        self.scroll_to_page(at);
        Ok(n)
    }

    fn save(&mut self, save_as: bool) -> bool {
        self.end_edit();
        // Edits rewrite the file, which drops existing digital signatures: ask first.
        if !self.sig_reports.is_empty() && !self.doc.is_pristine() && !self.unsign_ok {
            self.confirm_unsign = Some(save_as);
            return false;
        }
        let path = match (&self.doc.path, save_as) {
            (Some(p), false) => p.clone(),
            _ => {
                let name = self.doc.title();
                match rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(name).save_file() {
                    Some(mut p) => {
                        if p.extension().is_none() {
                            p.set_extension("pdf");
                        }
                        p
                    }
                    None => return false,
                }
            }
        };
        match self.write_pdf(path) {
            Ok(n) => {
                self.status = format!("Saved {} ({:.1} MB)", self.doc.title(), n as f32 / 1.0e6);
                true
            }
            Err(e) => {
                self.error = Some(format!("Could not save: {e}"));
                false
            }
        }
    }

    /// Write the document to `path` (through a temporary file) and make it the document's file.
    pub(crate) fn write_pdf(&mut self, path: PathBuf) -> Result<usize, String> {
        // An untouched document is written back byte for byte (signatures stay valid).
        let bytes = match (&self.doc.source, self.doc.is_pristine()) {
            (Some(src), true) => src.as_ref().clone(),
            _ => export::save(&self.doc)?,
        };
        let tmp = path.with_extension("pdf.tmp");
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
        self.doc.path = Some(path);
        self.doc.dirty = false;
        Ok(bytes.len())
    }

    fn insert_image_dialog(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Pictures", &["png", "jpg", "jpeg", "gif", "bmp", "webp"]).pick_file() {
            match std::fs::read(&p) {
                Ok(b) => self.insert_image(ctx, b, self.current, None),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
    }

    /// Place a picture on a page, centred on `at` (page points) or the page centre.
    pub(crate) fn insert_image(&mut self, ctx: &egui::Context, bytes: Vec<u8>, page: usize, at: Option<Pos2>) {
        let img = match image::load_from_memory(&bytes) {
            Ok(i) => i,
            Err(e) => return self.error = Some(format!("Cannot read picture: {e}")),
        };
        let (w, h) = (img.width(), img.height());
        // The screen only needs a preview; the original bytes go into the PDF.
        let preview = if w.max(h) > 1024 { img.thumbnail(1024, 1024) } else { img };
        let rgba = preview.to_rgba8();
        let ci = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
        drop(rgba);
        let tex = ctx.load_texture("picture", ci, TextureOptions::LINEAR);
        let Some(pe) = self.doc.pages.get(page) else { return };
        let natural = vec2(w as f32, h as f32) * 0.75; // 96 dpi pixels to points
        let fit = (pe.size.x * 0.6 / natural.x).min(pe.size.y * 0.6 / natural.y).min(1.0);
        let size = natural * fit;
        let c = at.unwrap_or(Rect::from_min_size(Pos2::ZERO, pe.size).center());
        let rect = Rect::from_center_size(c, size);
        let data = Arc::new(ImageData::new(Arc::new(bytes), [w, h], Some(tex)));
        self.doc.checkpoint();
        let objs = &mut self.doc.pages[page].objects;
        objs.push(Obj { rect, kind: Kind::Image(data) });
        self.sel = Some((page, objs.len() - 1));
        self.current = page;
        self.tool = Tool::Move;
        self.status = "Picture placed. Drag the corners to resize (Shift frees the proportions).".into();
    }

    // ---------------------------------------------------------------------------------- pages

    fn page_op(&mut self, f: impl FnOnce(&mut Vec<PageEntry>, &mut usize)) {
        self.end_edit();
        self.doc.checkpoint();
        f(&mut self.doc.pages, &mut self.current);
        self.current = self.current.min(self.doc.pages.len() - 1);
        self.sel = None;
        self.prop_session = None;
        self.text_sel = None;
    }

    pub(crate) fn add_blank(&mut self, at: usize) {
        let size = self.doc.pages.get(self.current).map(|p| p.view_size()).unwrap_or(A4);
        self.page_op(|pages, cur| {
            let at = at.min(pages.len());
            pages.insert(at, PageEntry::blank(size));
            *cur = at;
        });
        self.scroll_to_page(self.current);
    }

    pub(crate) fn duplicate_page(&mut self, i: usize) {
        self.page_op(|pages, cur| {
            let p = pages[i].clone();
            pages.insert(i + 1, p);
            *cur = i + 1;
        });
        self.scroll_to_page(self.current);
    }

    pub(crate) fn rotate_page(&mut self, i: usize, delta: u16) {
        self.page_op(|pages, _| pages[i].rotation = (pages[i].rotation + delta) % 360);
    }

    fn move_page(&mut self, i: usize, down: bool) {
        let n = self.doc.pages.len();
        if (down && i + 1 >= n) || (!down && i == 0) {
            return;
        }
        self.page_op(|pages, cur| {
            let j = if down { i + 1 } else { i - 1 };
            pages.swap(i, j);
            *cur = j;
        });
        self.scroll_to_page(self.current);
    }

    /// Move page `from` so it lands before the page now at `to` (`to` may equal the length).
    pub(crate) fn move_page_to(&mut self, from: usize, to: usize) {
        let n = self.doc.pages.len();
        if from >= n || to > n || to == from || to == from + 1 {
            return;
        }
        self.page_op(|pages, cur| {
            let p = pages.remove(from);
            let at = if to > from { to - 1 } else { to };
            pages.insert(at, p);
            *cur = at;
        });
    }

    pub(crate) fn delete_page(&mut self, i: usize) {
        if self.doc.pages.len() <= 1 {
            self.status = "A document needs at least one page.".into();
            return;
        }
        self.page_op(|pages, cur| {
            pages.remove(i);
            *cur = i.min(pages.len() - 1);
        });
    }

    // ----------------------------------------------------------------------------------- view

    fn content_size(&self) -> Vec2 {
        let w = self.doc.pages.iter().map(|p| p.view_size().x).fold(0.0, f32::max) + 2.0 * MARGIN;
        let h = self.doc.pages.iter().map(|p| p.view_size().y + GAP).sum::<f32>() - GAP + 2.0 * MARGIN;
        vec2(w, h)
    }

    fn offset_x(&self, zoom: f32) -> f32 {
        ((self.view.width() - self.content_size().x * zoom) / 2.0).max(0.0)
    }

    fn page_top(&self, i: usize) -> f32 {
        MARGIN + self.doc.pages[..i].iter().map(|p| p.view_size().y + GAP).sum::<f32>()
    }

    /// Screen rect of every page at the current zoom and scroll.
    fn page_rects(&self) -> Vec<Rect> {
        let z = self.zoom;
        let cw = self.content_size().x;
        let ox = self.offset_x(z);
        let mut y = MARGIN;
        self.doc
            .pages
            .iter()
            .map(|p| {
                let vs = p.view_size();
                let x = (cw - vs.x) / 2.0;
                let min = pos2(self.view.min.x + ox + x * z - self.scroll.x, self.view.min.y + y * z - self.scroll.y);
                y += vs.y + GAP;
                Rect::from_min_size(min, vs * z)
            })
            .collect()
    }

    fn clamp_scroll(&mut self) {
        let c = self.content_size() * self.zoom;
        self.scroll.x = self.scroll.x.clamp(0.0, (c.x - self.view.width()).max(0.0));
        self.scroll.y = self.scroll.y.clamp(0.0, (c.y - self.view.height()).max(0.0));
    }

    fn zoom_at(&mut self, p: Pos2, zoom: f32) {
        let zoom = zoom.clamp(0.1, 8.0);
        let z = self.zoom;
        let cx = (p.x - self.view.min.x - self.offset_x(z) + self.scroll.x) / z;
        let cy = (p.y - self.view.min.y + self.scroll.y) / z;
        self.zoom = zoom;
        self.scroll.x = cx * zoom - (p.x - self.view.min.x - self.offset_x(zoom));
        self.scroll.y = cy * zoom - (p.y - self.view.min.y);
        self.clamp_scroll();
    }

    fn zoom_center(&mut self, zoom: f32) {
        if self.view.is_positive() {
            self.zoom_at(self.view.center(), zoom);
        } else {
            self.zoom = zoom;
        }
    }

    fn fit_width(&mut self) {
        let w = self.doc.pages.iter().map(|p| p.view_size().x).fold(0.0, f32::max) + 2.0 * MARGIN;
        if self.view.is_positive() {
            self.zoom = (self.view.width() / w).clamp(0.1, 8.0);
            self.scroll_to_page(self.current);
        }
    }

    fn fit_page(&mut self) {
        if let Some(p) = self.doc.pages.get(self.current) {
            let vs = p.view_size() + Vec2::splat(2.0 * GAP);
            if self.view.is_positive() {
                self.zoom = (self.view.width() / vs.x).min(self.view.height() / vs.y).clamp(0.1, 8.0);
                self.scroll_to_page(self.current);
            }
        }
    }

    fn scroll_to_page(&mut self, i: usize) {
        if i < self.doc.pages.len() {
            self.scroll.y = (self.page_top(i) - GAP) * self.zoom;
            self.clamp_scroll();
        }
    }

    fn to_page(&self, rects: &[Rect], i: usize, s: Pos2) -> Pos2 {
        let v = ((s - rects[i].min) / self.zoom).to_pos2();
        self.doc.pages[i].from_view(v)
    }

    fn page_at(&self, rects: &[Rect], s: Pos2) -> Option<(usize, Pos2)> {
        rects.iter().position(|r| r.contains(s)).map(|i| (i, self.to_page(rects, i, s)))
    }

    // ------------------------------------------------------------------------------- editing

    fn start_edit(&mut self, page: usize, idx: usize, before: Vec<PageEntry>) {
        let original = match &self.doc.pages[page].objects[idx].kind {
            Kind::Text { text, .. } => text.clone(),
            _ => return,
        };
        self.sel = Some((page, idx));
        self.editing = Some(Editing { page, idx, before, original, focus: true });
    }

    /// Commit text editing: empty text boxes are removed, real changes become one undo step.
    fn end_edit(&mut self) {
        let Some(ed) = self.editing.take() else { return };
        let Some(obj) = self.doc.pages.get(ed.page).and_then(|p| p.objects.get(ed.idx)) else { return };
        let Kind::Text { text, .. } = &obj.kind else { return };
        let empty = text.trim().is_empty();
        let new = ed.before.get(ed.page).is_none_or(|p| p.objects.len() < self.doc.pages[ed.page].objects.len());
        if empty {
            self.doc.pages[ed.page].objects.remove(ed.idx);
            self.sel = None;
            if !new {
                self.doc.push_undo(ed.before);
            }
        } else if new || *text != ed.original {
            self.doc.push_undo(ed.before);
        }
    }

    fn selected(&self) -> Option<&Obj> {
        self.sel.and_then(|(p, i)| self.doc.pages.get(p)?.objects.get(i))
    }

    fn delete_selected(&mut self) {
        if let Some((p, i)) = self.sel.take() {
            if i < self.doc.pages[p].objects.len() {
                self.doc.checkpoint();
                self.doc.pages[p].objects.remove(i);
            }
        }
    }

    fn restack(&mut self, up: bool) {
        let Some((p, i)) = self.sel else { return };
        let n = self.doc.pages[p].objects.len();
        let j = if up { i + 1 } else { i.wrapping_sub(1) };
        if j >= n {
            return;
        }
        self.doc.checkpoint();
        self.doc.pages[p].objects.swap(i, j);
        self.sel = Some((p, j));
    }

    fn paste(&mut self) {
        let Some(mut o) = self.clipboard.clone() else { return };
        let page = self.current.min(self.doc.pages.len() - 1);
        o.rect = o.rect.translate(vec2(12.0, 12.0));
        self.clipboard = Some(o.clone());
        self.doc.checkpoint();
        self.doc.pages[page].objects.push(o);
        self.sel = Some((page, self.doc.pages[page].objects.len() - 1));
        self.tool = Tool::Move;
    }

    fn place_signature(&mut self, page: usize, at: Pos2) {
        let Some(s) = self.sigs.get(self.sig_sel.min(self.sigs.len().saturating_sub(1))).cloned() else {
            self.sig_pad = Some(sig::Pad::default());
            return;
        };
        let w = 150.0f32.min(self.doc.pages[page].size.x * 0.5);
        let rect = Rect::from_center_size(at, vec2(w, w / s.aspect));
        self.doc.checkpoint();
        let objs = &mut self.doc.pages[page].objects;
        objs.push(Obj { rect, kind: Kind::Ink { strokes: s.strokes, color: self.opts.sig_color, width: 1.6, highlighter: false } });
        self.sel = Some((page, objs.len() - 1));
        self.tool = Tool::Move;
        self.status = "Signature placed. Drag to move, drag a corner to resize.".into();
    }

    // ------------------------------------------------------------------------------ main loop

    fn receive_renders(&mut self, ctx: &egui::Context) {
        while let Ok(done) = self.renderer.rx.try_recv() {
            let tex = ctx.load_texture(format!("page{}", done.job.page), done.image, TextureOptions::LINEAR);
            if done.job.thumb {
                self.thumbs.insert(done.job.page, (done.job.scale, tex));
            } else {
                self.page_tex.insert(done.job.page, (done.job.scale, tex));
            }
        }
    }

    fn handle_drops(&mut self, ctx: &egui::Context, rects: &[Rect]) {
        let (files, pointer) = ctx.input(|i| (i.raw.dropped_files.clone(), i.pointer.latest_pos()));
        for f in files {
            let path = f.path().to_path_buf();
            let bytes = match f.bytes() {
                Ok(b) => b,
                Err(e) => {
                    self.error = Some(e);
                    continue;
                }
            };
            if bytes.starts_with(b"%PDF") || path.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf")) {
                self.open_or_spawn(Some(path));
                continue;
            }
            let (page, at) = match pointer.and_then(|p| self.page_at(rects, p)) {
                Some((i, p)) => (i, Some(p)),
                None => (self.current, None),
            };
            self.insert_image(ctx, bytes, page, at);
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.egui_wants_keyboard_input();
        let sc = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(m, k)));
        let cmd_shift = Modifiers::COMMAND | Modifiers::SHIFT;
        if sc(Modifiers::COMMAND, Key::O) {
            self.open_or_spawn(None);
        }
        if sc(cmd_shift, Key::S) {
            self.save(true);
        }
        if sc(Modifiers::COMMAND, Key::S) {
            self.save(false);
        }
        if sc(Modifiers::COMMAND, Key::N) || sc(cmd_shift, Key::N) {
            self.spawn.push(None);
        }
        if sc(Modifiers::COMMAND, Key::W) {
            self.close_me = true;
        }
        if sc(Modifiers::COMMAND, Key::Q) {
            self.quit = true;
        }
        if sc(Modifiers::COMMAND, Key::Equals) || sc(Modifiers::COMMAND, Key::Plus) {
            self.zoom_center(self.zoom * 1.25);
        }
        if sc(Modifiers::COMMAND, Key::Minus) {
            self.zoom_center(self.zoom / 1.25);
        }
        if sc(Modifiers::COMMAND, Key::Num0) {
            self.fit_page();
        }
        if sc(Modifiers::COMMAND, Key::Num1) {
            self.zoom_center(1.0);
        }
        if sc(Modifiers::COMMAND | Modifiers::CTRL, Key::F) || sc(Modifiers::NONE, Key::F11) {
            self.toggle_fullscreen(ctx);
        }
        if typing {
            return;
        }
        if sc(cmd_shift, Key::Z) {
            self.end_edit();
            self.note_edit = None;
            self.pen = None;
            if self.doc.redo() {
                self.sel = None;
            }
        }
        if sc(Modifiers::COMMAND, Key::Z) {
            self.end_edit();
            self.note_edit = None;
            self.pen = None;
            if self.doc.undo() {
                self.sel = None;
            }
        }
        self.adjust_shortcuts(ctx);
        if self.tool == Tool::SelectText && sc(Modifiers::COMMAND, Key::A) {
            self.select_all_text(self.current);
        }
        if self.tool.is_raster() {
            if sc(Modifiers::COMMAND, Key::J) {
                self.picture_from_selection();
            }
            if sc(Modifiers::COMMAND, Key::A) {
                self.select_all_pixels(ctx);
            }
        }
        if sc(Modifiers::COMMAND, Key::D) && self.pix_sel.take().is_none() {
            if let Some(o) = self.selected().cloned() {
                self.clipboard = Some(o);
                self.paste();
            }
        }
        let events = ctx.input(|i| i.events.clone());
        for e in events {
            match e {
                Event::Copy if self.tool == Tool::SelectText && self.text_sel.is_some() => self.copy_text_selection(ctx),
                Event::Copy => self.clipboard = self.selected().cloned(),
                Event::Cut => {
                    self.clipboard = self.selected().cloned();
                    self.delete_selected();
                }
                Event::Paste(_) => self.paste(),
                _ => {}
            }
        }
        let mods = ctx.input(|i| i.modifiers);
        if mods.command || mods.alt {
            return;
        }
        if ctx.input(|i| i.key_pressed(Key::G)) {
            self.toggle_grid();
            return;
        }
        if self.grid {
            if ctx.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Enter)) {
                self.toggle_grid();
            } else if ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
                self.delete_page(self.current);
            } else if ctx.input(|i| i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowDown)) {
                self.current = (self.current + 1).min(self.doc.pages.len() - 1);
                self.scroll_thumbs_to = Some(self.current);
            } else if ctx.input(|i| i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowUp)) {
                self.current = self.current.saturating_sub(1);
                self.scroll_thumbs_to = Some(self.current);
            }
            return;
        }
        if self.sign_placing.is_some() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.sign_dlg = self.sign_placing.take();
            return;
        }
        if self.pen.is_some() {
            if ctx.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Escape)) {
                self.finish_pen(false);
                return;
            }
            if ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
                self.pen_backspace();
                return;
            }
        }
        for (tool, _, _, key) in TOOLS {
            if let Some(key) = key
                && !tool.is_pen()
                && ctx.input(|i| i.key_pressed(*key))
            {
                self.set_tool(*tool);
            }
        }
        if ctx.input(|i| i.key_pressed(Key::P)) {
            self.pen_key(mods.shift);
        }
        if self.tool.is_raster() {
            if ctx.input(|i| i.key_pressed(Key::OpenBracket)) {
                self.ropts.size = (self.ropts.size / 1.2).max(1.0);
            }
            if ctx.input(|i| i.key_pressed(Key::CloseBracket)) {
                self.ropts.size = (self.ropts.size * 1.2).min(500.0);
            }
            if let Some((_, _, q)) = self.crop {
                if ctx.input(|i| i.key_pressed(Key::Enter)) {
                    self.crop_picture(q);
                }
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace)) {
            if self.pix_sel.is_some() && self.tool.is_raster() {
                self.delete_pixels();
            } else {
                self.delete_selected();
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if self.text_sel.take().is_none() && self.crop.take().is_none() && self.pix_sel.take().is_none() && self.sel.take().is_none() && self.fullscreen {
                self.toggle_fullscreen(ctx);
            }
        }
        let step = if mods.shift { 10.0 } else { 1.0 };
        let mut nudge = Vec2::ZERO;
        ctx.input(|i| {
            if i.key_pressed(Key::ArrowLeft) {
                nudge.x -= step;
            }
            if i.key_pressed(Key::ArrowRight) {
                nudge.x += step;
            }
            if i.key_pressed(Key::ArrowUp) {
                nudge.y -= step;
            }
            if i.key_pressed(Key::ArrowDown) {
                nudge.y += step;
            }
        });
        if nudge != Vec2::ZERO {
            if let Some((p, i)) = self.sel {
                if self.prop_session != Some((p, i)) {
                    self.doc.checkpoint();
                    self.prop_session = Some((p, i));
                }
                // Nudges move on screen; map the screen direction into page space.
                let page = &self.doc.pages[p];
                let d = page.from_view(page.to_view(Pos2::ZERO) + nudge) - Pos2::ZERO;
                let o = &mut self.doc.pages[p].objects[i];
                o.rect = o.rect.translate(d);
            } else if self.current + 1 < self.doc.pages.len() && nudge.y > 0.0 && step > 1.0 {
                self.scroll_to_page(self.current + 1);
            }
        }
    }

    /// View › Full Screen (⌃⌘F, F11): asks the OS for a full-screen window and switches to the
    /// one-bar layout. Escape leaves it once there is nothing else left to cancel.
    pub(crate) fn toggle_fullscreen(&mut self, ctx: &egui::Context) {
        self.fullscreen = !self.fullscreen;
        self.flyout = None;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(self.fullscreen));
    }

    fn toggle_grid(&mut self) {
        self.end_edit();
        self.grid = !self.grid;
        self.grid_drag = None;
        self.sel = None;
        if self.grid {
            self.scroll_thumbs_to = Some(self.current);
        } else {
            self.scroll_to_page(self.current);
        }
    }

    pub(crate) fn set_tool(&mut self, tool: Tool) {
        if tool.is_raster() && self.picture_sel().is_none() {
            self.status = "Select a picture to use the picture tools.".into();
            return;
        }
        self.end_edit();
        if self.pen.is_some() && !matches!(tool, Tool::PenPath | Tool::CurvaturePen) {
            self.finish_pen(false);
        }
        if tool.is_pen() {
            self.pen_last = tool;
        }
        if self.grid && tool != Tool::Move {
            self.toggle_grid();
        }
        self.tool = tool;
        if tool == Tool::Signature && self.sigs.is_empty() {
            self.sig_pad = Some(sig::Pad::default());
        }
    }

    // ------------------------------------------------------------------------------- panels

    fn menu_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            let t = Tokens::get(ui.ctx());
            let (r, _) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::hover());
            let tex = self.brand.get_or_insert_with(|| {
                let img = image::load_from_memory(crate::ICON_PNG).expect("bundled icon").thumbnail(96, 96).to_rgba8();
                let ci = egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
                ui.ctx().load_texture("brand", ci, TextureOptions::LINEAR)
            });
            ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            let cmd = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" };
            let item = |ui: &mut Ui, label: &str, sc: &str| ui.add(egui::Button::new(label).shortcut_text(if sc.is_empty() { String::new() } else { format!("{cmd}{sc}") })).clicked();
            ui.menu_button("File", |ui| {
                if item(ui, "New", "N") {
                    self.spawn.push(None);
                }
                if item(ui, "Open…", "O") {
                    self.open_or_spawn(None);
                }
                ui.separator();
                if item(ui, "Save", "S") {
                    self.save(false);
                }
                if item(ui, "Save As…", "⇧S") {
                    self.save(true);
                }
                ui.separator();
                if item(ui, "Sign with Digital ID…", "") {
                    self.open_sign_dialog();
                }
                if item(ui, "Place Picture…", "") {
                    self.insert_image_dialog(&ctx);
                }
                ui.separator();
                if item(ui, "Close Window", "W") {
                    self.close_me = true;
                }
                if item(ui, "Quit", "Q") {
                    self.quit = true;
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(self.doc.can_undo(), egui::Button::new("Undo").shortcut_text(format!("{cmd}Z"))).clicked() {
                    self.end_edit();
                    self.doc.undo();
                    self.sel = None;
                }
                if ui.add_enabled(self.doc.can_redo(), egui::Button::new("Redo").shortcut_text(format!("{cmd}⇧Z"))).clicked() {
                    self.doc.redo();
                    self.sel = None;
                }
                ui.separator();
                let has_sel = self.sel.is_some();
                if ui.add_enabled(has_sel, egui::Button::new("Copy").shortcut_text(format!("{cmd}C"))).clicked() {
                    self.clipboard = self.selected().cloned();
                }
                if ui.add_enabled(self.clipboard.is_some(), egui::Button::new("Paste").shortcut_text(format!("{cmd}V"))).clicked() {
                    self.paste();
                }
                if ui.add_enabled(has_sel, egui::Button::new("Duplicate").shortcut_text(format!("{cmd}D"))).clicked() {
                    self.clipboard = self.selected().cloned();
                    self.paste();
                }
                if ui.add_enabled(has_sel, egui::Button::new("Delete").shortcut_text("⌫")).clicked() {
                    self.delete_selected();
                }
                ui.separator();
                if ui.add_enabled(has_sel, egui::Button::new("Bring Forward")).clicked() {
                    self.restack(true);
                }
                if ui.add_enabled(has_sel, egui::Button::new("Send Backward")).clicked() {
                    self.restack(false);
                }
                ui.separator();
                if ui.add_enabled(self.text_sel.is_some(), egui::Button::new("Copy Text").shortcut_text(format!("{cmd}C"))).clicked() {
                    self.copy_text_selection(&ctx);
                }
                if ui.button("Select All Text on Page").clicked() {
                    self.set_tool(Tool::SelectText);
                    self.select_all_text(self.current);
                }
                if ui.button("Snapshot of Page").on_hover_text("Copy the current page, edits included, as a picture").clicked() {
                    let page = self.current.min(self.doc.pages.len() - 1);
                    let size = self.doc.pages[page].size;
                    self.take_snapshot(&ctx, page, Rect::from_min_size(Pos2::ZERO, size));
                }
            });
            ui.menu_button("Image", |ui| self.image_menu(ui));
            ui.menu_button("Page", |ui| {
                let c = self.current;
                if ui.button("Insert Blank Page After").clicked() {
                    self.add_blank(c + 1);
                }
                if ui.button("Insert Blank Page Before").clicked() {
                    self.add_blank(c);
                }
                if ui.button("Duplicate Page").clicked() {
                    self.duplicate_page(c);
                }
                ui.separator();
                if ui.button("Rotate Right 90°").clicked() {
                    self.rotate_page(c, 90);
                }
                if ui.button("Rotate Left 90°").clicked() {
                    self.rotate_page(c, 270);
                }
                ui.separator();
                if ui.button("Move Page Up").clicked() {
                    self.move_page(c, false);
                }
                if ui.button("Move Page Down").clicked() {
                    self.move_page(c, true);
                }
                ui.separator();
                if ui.add_enabled(self.doc.pages.len() > 1, egui::Button::new("Delete Page")).clicked() {
                    self.delete_page(c);
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Signature Panel…").clicked() {
                    self.sig_panel = true;
                    ui.close();
                }
                if ui.add(egui::Button::selectable(self.grid, "Page Grid").shortcut_text("G")).clicked() {
                    self.toggle_grid();
                    ui.close();
                }
                if ui.add(egui::Button::selectable(self.fullscreen, "Full Screen").shortcut_text(if cfg!(target_os = "macos") { "⌃⌘F" } else { "F11" })).clicked() {
                    self.toggle_fullscreen(&ctx);
                    ui.close();
                }
                ui.separator();
                if item(ui, "Zoom In", "+") {
                    self.zoom_center(self.zoom * 1.25);
                }
                if item(ui, "Zoom Out", "-") {
                    self.zoom_center(self.zoom / 1.25);
                }
                if item(ui, "Fit Page", "0") {
                    self.fit_page();
                }
                if ui.button("Fit Width").clicked() {
                    self.fit_width();
                }
                if item(ui, "Actual Size", "1") {
                    self.zoom_center(1.0);
                }
                ui.separator();
                for (k, name) in [(ThemeKind::ProMedium, "Medium Gray Theme"), (ThemeKind::Pro, "Dark Theme")] {
                    if ui.radio(self.theme == k, name).clicked() {
                        self.theme = k;
                        theme::apply(ui.ctx(), k);
                    }
                }
            });
            ui.menu_button("Window", |ui| {
                if item(ui, "New Window", "N") {
                    self.spawn.push(None);
                }
                if item(ui, "Open in New Window…", "O") {
                    self.open_or_spawn(None);
                }
                ui.separator();
                ui.label(egui::RichText::new("Drag a page thumbnail from one window onto another window's Pages panel to copy it there.").color(t.text_faint).size(11.0));
                ui.separator();
                for (win, vp, title) in self.window_list.clone() {
                    if ui.radio(win == self.win, title).clicked() {
                        ctx.send_viewport_cmd_to(vp, ViewportCommand::Focus);
                        ui.close();
                    }
                }
            });
            ui.add_space(16.0);
            let title = format!("{}{} @ {:.0}%", self.doc.title(), if self.doc.dirty { "*" } else { "" }, self.zoom * 100.0);
            ui.label(egui::RichText::new(title).color(t.text_dim));
        });
    }

    fn options_bar(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        ui.horizontal_centered(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
            ui.painter().rect_filled(r, t.radius_sm, t.field);
            icons::paint(ui.painter(), r.shrink(5.0), if self.grid { "layout-grid" } else { self.tool.icon() }, t.icon);
            ui.label(if self.grid { "Page Grid" } else { self.tool.name() });
            vline(ui, 22.0);
            if self.tool.is_raster() {
                self.raster_options(ui);
                return;
            }
            if self.grid {
                option_label(ui, "Thumbnail size:");
                ui.add(egui::Slider::new(&mut self.grid_size, 80.0..=400.0).show_value(false));
                vline(ui, 22.0);
                ui.label(egui::RichText::new(format!("{} pages. Double-click a page to open it, drag to reorder, right-click for page commands. G or Esc returns.", self.doc.pages.len())).color(t.text_faint));
                return;
            }
            // With the Move tool, the bar edits the selected object (Photoshop's Properties).
            if self.tool == Tool::Move || (self.tool == Tool::Text && self.editing.is_some()) {
                self.object_options(ui);
                return;
            }
            match self.tool {
                Tool::Text => {
                    text_controls(ui, &mut self.opts.text_size, &mut self.opts.bold, &mut self.opts.text_color);
                    vline(ui, 22.0);
                    ui.label(egui::RichText::new("Click on a page to type. Font: Sarabun (Thai and Latin).").color(t.text_faint));
                }
                Tool::PenPath | Tool::FreeformPen | Tool::CurvaturePen | Tool::AddAnchor | Tool::DeleteAnchor | Tool::ConvertPoint => {
                    stroke_controls(ui, &mut self.opts.color, &mut self.opts.width);
                    vline(ui, 22.0);
                    let mut on = self.opts.fill.is_some();
                    if widgets::checkbox(ui, &mut on, "Fill").changed() {
                        self.opts.fill = on.then_some(Color32::from_rgb(255, 240, 160));
                    }
                    if let Some(f) = &mut self.opts.fill {
                        egui::color_picker::color_edit_button_srgba(ui, f, egui::color_picker::Alpha::Opaque);
                    }
                    vline(ui, 22.0);
                    let hint = match self.tool {
                        Tool::PenPath => "Click for corners, drag for curves. Click the first point to close; Enter ends.",
                        Tool::FreeformPen => "Drag to draw; the stroke becomes an editable path.",
                        Tool::CurvaturePen => "Click points on a smooth curve; Alt-click for a corner. Enter ends.",
                        Tool::AddAnchor => "Click a path segment to add an anchor.",
                        Tool::DeleteAnchor => "Click an anchor to remove it.",
                        _ => "Click an anchor: corner ⇄ smooth. Drag a handle to break its pair.",
                    };
                    ui.label(egui::RichText::new(hint).color(t.text_faint));
                }
                Tool::Brush | Tool::Line | Tool::Arrow => {
                    stroke_controls(ui, &mut self.opts.color, &mut self.opts.width);
                }
                Tool::Highlighter => {
                    stroke_controls(ui, &mut self.opts.hl_color, &mut self.opts.hl_width);
                }
                Tool::Rect | Tool::Ellipse => {
                    stroke_controls(ui, &mut self.opts.color, &mut self.opts.width);
                    vline(ui, 22.0);
                    let mut on = self.opts.fill.is_some();
                    if widgets::checkbox(ui, &mut on, "Fill").changed() {
                        self.opts.fill = on.then_some(Color32::from_rgb(255, 240, 160));
                    }
                    if let Some(f) = &mut self.opts.fill {
                        egui::color_picker::color_edit_button_srgba(ui, f, egui::color_picker::Alpha::Opaque);
                    }
                }
                Tool::Comment => {
                    option_label(ui, "Author:");
                    if ui.add(egui::TextEdit::singleline(&mut self.author).desired_width(130.0)).lost_focus() {
                        let a = self.author.clone();
                        let _ = crate::sig::config_dir().map(|d| std::fs::write(d.join("author.txt"), a));
                    }
                    option_label(ui, "Colour:");
                    for nc in notes::NOTE_COLORS {
                        let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                        ui.painter().rect_filled(r, 3.0, nc);
                        if self.note_color == nc {
                            ui.painter().rect_stroke(r, 3.0, Stroke::new(2.0, t.accent), StrokeKind::Outside);
                        }
                        if resp.clicked() {
                            self.note_color = nc;
                        }
                    }
                    vline(ui, 22.0);
                    ui.label(egui::RichText::new("Click a page to add a comment; it is saved as a PDF comment other readers show.").color(t.text_faint));
                }
                Tool::Whiteout => {
                    ui.label(egui::RichText::new("Drag over content to cover it with white, then type on top with the Text tool.").color(t.text_faint));
                }
                Tool::Signature => {
                    option_label(ui, "Ink:");
                    color_row(ui, &mut self.opts.sig_color);
                    vline(ui, 22.0);
                    if widgets::secondary_button(ui, "New Signature…", 0.0).clicked() {
                        self.sig_pad = Some(sig::Pad::default());
                    }
                    if !self.sigs.is_empty() {
                        ui.label(egui::RichText::new("Click on a page to place the selected signature.").color(t.text_faint));
                    }
                }
                Tool::Marquee | Tool::Lasso | Tool::Crop | Tool::Eyedropper | Tool::Heal | Tool::PaintBrush | Tool::CloneStamp | Tool::Eraser | Tool::Gradient | Tool::Blur | Tool::Dodge => {}
                Tool::SelectText => self.text_options(ui),
                Tool::Snapshot => self.snapshot_options(ui),
                Tool::Hand | Tool::Move => {
                    ui.label(egui::RichText::new("Drag to scroll. Hold Space with any tool to pan; ⌘-scroll or pinch to zoom.").color(t.text_faint));
                }
            }
        });
    }

    /// Properties of the selected object, editable in place (one undo step per object).
    fn object_options(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let Some((p, i)) = self.sel.or(self.editing.as_ref().map(|e| (e.page, e.idx))) else {
            ui.label(egui::RichText::new("Click an object to select it. Drag pictures or PDFs from Finder onto the window.").color(t.text_faint));
            return;
        };
        let Some(mut obj) = self.doc.pages.get(p).and_then(|pg| pg.objects.get(i)).cloned() else { return };
        ui.label(egui::RichText::new(obj.label()).color(t.text));
        vline(ui, 22.0);
        let changed = match &mut obj.kind {
            Kind::Text { size, bold, color, .. } => text_controls(ui, size, bold, color),
            Kind::Ink { color, width, .. } => stroke_controls(ui, color, width),
            Kind::Note { color, .. } => {
                option_label(ui, "Colour:");
                let mut c = false;
                for nc in notes::NOTE_COLORS {
                    let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                    ui.painter().rect_filled(r, 3.0, nc);
                    if *color == nc {
                        ui.painter().rect_stroke(r, 3.0, Stroke::new(2.0, t.accent), StrokeKind::Outside);
                    }
                    if resp.clicked() {
                        *color = nc;
                        c = true;
                    }
                }
                c
            }
            Kind::Path { stroke, width, fill, .. } => {
                let mut c = stroke_controls(ui, stroke, width);
                vline(ui, 22.0);
                let mut on = fill.is_some();
                if widgets::checkbox(ui, &mut on, "Fill").changed() {
                    *fill = on.then_some(Color32::from_rgb(255, 240, 160));
                    c = true;
                }
                if let Some(f) = fill {
                    c |= egui::color_picker::color_edit_button_srgba(ui, f, egui::color_picker::Alpha::Opaque).changed();
                }
                c
            }
            Kind::Shape { stroke, width, fill, shape, .. } => {
                let mut c = stroke_controls(ui, stroke, width);
                if matches!(shape, ShapeKind::Rect | ShapeKind::Ellipse) {
                    vline(ui, 22.0);
                    let mut on = fill.is_some();
                    if widgets::checkbox(ui, &mut on, "Fill").changed() {
                        *fill = on.then_some(Color32::WHITE);
                        c = true;
                    }
                    if let Some(f) = fill {
                        c |= egui::color_picker::color_edit_button_srgba(ui, f, egui::color_picker::Alpha::Opaque).changed();
                    }
                }
                c
            }
            Kind::Image(img) => {
                ui.label(egui::RichText::new(format!("{} × {} px", img.px[0], img.px[1])).color(t.text_dim));
                if self.editing.is_none() {
                    vline(ui, 22.0);
                    self.adjust_button(ui);
                }
                false
            }
        };
        vline(ui, 22.0);
        let mut w = obj.rect.width();
        let mut h = obj.rect.height();
        option_label(ui, "W:");
        let cw = ui.add(egui::DragValue::new(&mut w).range(1.0..=5000.0).speed(0.5).suffix(" pt").max_decimals(1)).changed();
        option_label(ui, "H:");
        let ch = ui.add(egui::DragValue::new(&mut h).range(1.0..=5000.0).speed(0.5).suffix(" pt").max_decimals(1)).changed();
        let resized = (cw || ch) && !matches!(obj.kind, Kind::Text { .. });
        if resized {
            obj.rect = Rect::from_min_size(obj.rect.min, vec2(w, h));
        }
        if changed || resized {
            if self.editing.is_none() && self.prop_session != Some((p, i)) {
                self.doc.checkpoint();
                self.prop_session = Some((p, i));
            }
            obj.refit_text();
            self.doc.pages[p].objects[i] = obj;
            self.doc.dirty = true;
        }
        if self.editing.is_none() {
            vline(ui, 22.0);
            if icons::small(ui, "chevron-up", "Bring forward").clicked() {
                self.restack(true);
            }
            if icons::small(ui, "chevron-down", "Send backward").clicked() {
                self.restack(false);
            }
            if icons::small(ui, "trash", "Delete (⌫)").clicked() {
                self.delete_selected();
            }
        }
    }

    /// Tool groups as laid out in the column (separators between groups).
    fn tool_groups() -> Vec<Vec<Tool>> {
        vec![
            vec![Tool::Move, Tool::Hand, Tool::SelectText, Tool::Snapshot],
            vec![Tool::Text, Tool::Brush, Tool::Highlighter, Tool::PenPath],
            vec![Tool::Line, Tool::Arrow, Tool::Rect, Tool::Ellipse],
            vec![Tool::Whiteout, Tool::Signature, Tool::Comment],
            rtools::RASTER_TOOLS.to_vec(),
        ]
    }

    /// One column when it fits, two (as PhotoCraft does) when the window is short.
    fn tool_columns_needed(height: f32) -> bool {
        let buttons: usize = Self::tool_groups().iter().map(Vec::len).sum::<usize>() + 2;
        let needed = buttons as f32 * 30.0 + 6.0 * 9.0 + 16.0;
        needed > height
    }

    fn tool_column(&mut self, ui: &mut Ui) {
        let cols = if self.two_cols { 2 } else { 1 };
        let picture = self.picture_sel().is_some();
        ui.add_space(6.0);
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
            for (g, group) in Self::tool_groups().into_iter().enumerate() {
                if g > 0 {
                    ui.add_space(3.0);
                    widgets_hline(ui);
                    ui.add_space(3.0);
                }
                for row in group.chunks(cols) {
                    ui.horizontal(|ui| {
                        if cols == 2 && row.len() == 1 {
                            ui.add_space(0.0);
                        }
                        for tool in row {
                            if tool.is_pen() {
                                self.pen_slot(ui);
                                continue;
                            }
                            let tip = TOOLS.iter().find(|x| x.0 == *tool).map(|x| x.2).unwrap_or("");
                            let enabled = !tool.is_raster() || picture;
                            let tip = if enabled { tip.to_owned() } else { format!("{tip} · select a picture first") };
                            if icons::button_state(ui, tool.icon(), self.tool == *tool, enabled, &tip).clicked() {
                                self.set_tool(*tool);
                            }
                        }
                    });
                }
            }
            ui.add_space(3.0);
            widgets_hline(ui);
            ui.add_space(3.0);
            ui.horizontal(|ui| {
                if icons::button(ui, "image", false, "Place picture… (or drag a picture onto a page)").clicked() {
                    let ctx = ui.ctx().clone();
                    self.insert_image_dialog(&ctx);
                }
                if cols == 1 {
                    return;
                }
                if icons::button(ui, "file-plus", false, "Insert blank page after the current page").clicked() {
                    self.add_blank(self.current + 1);
                }
            });
            if cols == 1 && icons::button(ui, "file-plus", false, "Insert blank page after the current page").clicked() {
                self.add_blank(self.current + 1);
            }
        });
    }

    fn dock(&mut self, ui: &mut Ui) {
        let h = ui.available_height() - 6.0;
        ui.add_space(4.0);
        let mut tab = self.dock_tabs[0];
        widgets::card(ui, "pages", &["Pages"], &mut tab, h * 0.46, |ui, _| self.pages_panel(ui));
        let mut tab = self.dock_tabs[1];
        widgets::card(ui, "layers", &["Layers", "Comments"], &mut tab, h * 0.24, |ui, sel| {
            if sel == 0 {
                self.layers_panel(ui)
            } else {
                self.comments_panel(ui)
            }
        });
        self.dock_tabs[1] = tab;
        let mut tab = self.dock_tabs[2];
        let rest = ui.available_height() - 4.0;
        widgets::card(ui, "sigs", &["Signatures", "Digital IDs"], &mut tab, rest, |ui, sel| {
            if sel == 0 {
                self.sigs_panel(ui)
            } else {
                self.ids_panel(ui)
            }
        });
        self.dock_tabs[2] = tab;
    }

    fn pages_panel(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let ppp = ui.ctx().pixels_per_point();
        let list_h = widgets::list_height(ui);
        let mut clicked = None;
        let mut menu_action: Option<(usize, u8)> = None;
        let mut reorder: Option<(usize, usize)> = None;
        self.thumb_seen.clear();
        let scroll_to = self.scroll_thumbs_to.take();
        let pointer = ui.input(|i| i.pointer.interact_pos());
        let dragging = self.thumb_drag;
        let mut rows: Vec<(usize, Rect)> = Vec::new();
        let out = egui::ScrollArea::vertical().id_salt(self.wid("thumbs")).max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
            let w = (ui.available_width() - 40.0).clamp(60.0, 150.0);
            let mut started = None;
            let mut stopped = None;
            for (i, page) in self.doc.pages.iter().enumerate() {
                let vs = page.view_size();
                let th = w * vs.y / vs.x;
                let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), th + 26.0), Sense::click_and_drag());
                if scroll_to == Some(i) {
                    ui.scroll_to_rect(row, None);
                }
                if resp.drag_started() {
                    started = Some(i);
                }
                if resp.drag_stopped() {
                    stopped = Some(i);
                }
                if !ui.is_rect_visible(row) {
                    continue;
                }
                rows.push((i, row));
                let r = Rect::from_min_size(pos2(row.center().x - w / 2.0, row.top() + 4.0), vec2(w, th));
                if i == self.current {
                    ui.painter().rect_filled(row.shrink(1.0), t.radius_sm, t.row_selected);
                } else if resp.hovered() && dragging.is_none() {
                    ui.painter().rect_filled(row.shrink(1.0), t.radius_sm, t.hover);
                }
                paint_thumb(ui.painter(), page, r, ppp, &self.thumbs, &mut self.thumb_want, &mut self.thumb_seen);
                if dragging == Some(i) {
                    ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(120));
                }
                ui.painter().rect_stroke(r, 0.0, Stroke::new(if i == self.current { 2.0 } else { 1.0 }, if i == self.current { t.accent } else { t.separator }), StrokeKind::Outside);
                ui.painter().text(pos2(row.center().x, r.bottom() + 11.0), egui::Align2::CENTER_CENTER, format!("{}", i + 1), FontId::proportional(11.0), t.text_dim);
                if resp.clicked() {
                    clicked = Some(i);
                }
                resp.context_menu(|ui| {
                    for (k, label) in [(0, "Insert Blank Page After"), (1, "Duplicate Page"), (2, "Rotate Right 90°"), (3, "Rotate Left 90°"), (4, "Delete Page")] {
                        if ui.button(label).clicked() {
                            menu_action = Some((i, k));
                            ui.close();
                        }
                    }
                });
            }
            (started, stopped)
        });
        let (started, stopped) = out.inner;
        let area = out.inner_rect;
        // Where a dragged page would land in this list (its own page, or one from another window).
        let slot_at = |p: Pos2| -> Option<usize> {
            if !area.contains(p) {
                return None;
            }
            Some(rows.iter().find(|(_, r)| r.center().y > p.y).map(|(i, _)| *i).unwrap_or_else(|| rows.last().map(|(i, _)| i + 1).unwrap_or(0)))
        };
        if let Some(i) = started {
            self.end_edit();
            self.thumb_drag = Some(i);
            self.current = i;
        }
        if let (Some(from), Some(p)) = (self.thumb_drag, pointer) {
            if let Some(to) = slot_at(p) {
                if to != from && to != from + 1 {
                    paint_insert_marker(ui, &rows, area, to, t.accent);
                }
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
        }
        if let Some(i) = stopped {
            let inside = pointer.and_then(slot_at);
            if let Some(to) = inside {
                reorder = Some((i, to));
            }
            self.drag_released = Some((i, inside.is_some()));
            self.thumb_drag = None;
        }
        self.drop_zone = Some(DropZone::List { area, rows });
        if let Some(i) = clicked {
            self.end_edit();
            self.current = i;
            self.scroll_to_page(i);
        }
        if let Some((from, to)) = reorder {
            self.move_page_to(from, to);
        }
        if let Some((i, k)) = menu_action {
            self.current = i;
            match k {
                0 => self.add_blank(i + 1),
                1 => self.duplicate_page(i),
                2 => self.rotate_page(i, 90),
                3 => self.rotate_page(i, 270),
                _ => self.delete_page(i),
            }
        }
        let c = self.current;
        widgets::panel_footer(ui, |ui| {
            if icons::small(ui, "trash", "Delete page").clicked() {
                self.delete_page(c);
            }
            if icons::small(ui, "chevron-down", "Move page down").clicked() {
                self.move_page(c, true);
            }
            if icons::small(ui, "chevron-up", "Move page up").clicked() {
                self.move_page(c, false);
            }
            if icons::small(ui, "rotate-cw", "Rotate right").clicked() {
                self.rotate_page(c, 90);
            }
            if icons::small(ui, "rotate-ccw", "Rotate left").clicked() {
                self.rotate_page(c, 270);
            }
            if icons::small(ui, "copy", "Duplicate page").clicked() {
                self.duplicate_page(c);
            }
            if icons::small(ui, "file-plus", "Insert blank page after").clicked() {
                self.add_blank(c + 1);
            }
        });
    }

    fn layers_panel(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let list_h = widgets::list_height(ui);
        let page = self.current.min(self.doc.pages.len() - 1);
        let mut pick = None;
        egui::ScrollArea::vertical().id_salt(self.wid("layers")).max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
            let objs = &self.doc.pages[page].objects;
            if objs.is_empty() {
                ui.label(egui::RichText::new(format!("Page {} has no added objects.", page + 1)).color(t.text_faint));
            }
            for (i, o) in objs.iter().enumerate().rev() {
                let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
                let on = self.sel == Some((page, i));
                if on {
                    ui.painter().rect_filled(row, 0.0, t.row_selected);
                } else if resp.hovered() {
                    ui.painter().rect_filled(row, 0.0, t.hover);
                }
                icons::paint(ui.painter(), Rect::from_center_size(pos2(row.left() + 14.0, row.center().y), vec2(15.0, 15.0)), o.icon(), t.icon);
                ui.painter().text(pos2(row.left() + 30.0, row.center().y), egui::Align2::LEFT_CENTER, o.label(), FontId::proportional(12.0), t.text);
                if resp.clicked() {
                    pick = Some(i);
                }
            }
        });
        if let Some(i) = pick {
            self.end_edit();
            self.sel = Some((page, i));
            self.tool = Tool::Move;
        }
        widgets::panel_footer(ui, |ui| {
            if icons::small(ui, "trash", "Delete object").clicked() {
                self.delete_selected();
            }
            if icons::small(ui, "chevron-down", "Send backward").clicked() {
                self.restack(false);
            }
            if icons::small(ui, "chevron-up", "Bring forward").clicked() {
                self.restack(true);
            }
        });
    }

    fn sigs_panel(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let list_h = widgets::list_height(ui);
        let mut remove = None;
        let mut pick = None;
        egui::ScrollArea::vertical().id_salt(self.wid("sigs")).max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
            if self.sigs.is_empty() {
                ui.label(egui::RichText::new("No saved signatures yet. Press + to draw one.").color(t.text_faint));
            }
            for (i, s) in self.sigs.iter().enumerate() {
                let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 48.0), Sense::click());
                let on = i == self.sig_sel;
                ui.painter().rect_filled(row.shrink(2.0), t.radius_sm, Color32::WHITE);
                if on {
                    ui.painter().rect_stroke(row.shrink(2.0), t.radius_sm, Stroke::new(2.0, t.accent), StrokeKind::Inside);
                }
                sig::paint(ui.painter(), row.shrink2(vec2(30.0, 6.0)), s, self.opts.sig_color, 1.4);
                let x = Rect::from_center_size(pos2(row.right() - 14.0, row.top() + 14.0), vec2(16.0, 16.0));
                let xr = ui.interact(x, ui.id().with(("sigx", i)), Sense::click());
                icons::paint(ui.painter(), x, "x", if xr.hovered() { t.danger } else { Color32::from_gray(150) });
                if xr.clicked() {
                    remove = Some(i);
                } else if resp.clicked() {
                    pick = Some(i);
                }
            }
        });
        if let Some(i) = pick {
            self.sig_sel = i;
            self.set_tool(Tool::Signature);
            self.status = "Click on a page to place the signature.".into();
        }
        if let Some(i) = remove {
            self.sigs.remove(i);
            self.sig_sel = self.sig_sel.min(self.sigs.len().saturating_sub(1));
            sig::save(&self.sigs);
        }
        widgets::panel_footer(ui, |ui| {
            if icons::small(ui, "plus", "New signature").clicked() {
                self.sig_pad = Some(sig::Pad::default());
            }
        });
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        ui.horizontal_centered(|ui| {
            ui.label(egui::RichText::new(format!("Page {} of {}", self.current + 1, self.doc.pages.len())).color(t.text_dim));
            vline(ui, 16.0);
            if icons::small(ui, "zoom-out", "Zoom out (⌘−)").clicked() {
                self.zoom_center(self.zoom / 1.25);
            }
            let mut pct = self.zoom * 100.0;
            if ui.add(egui::DragValue::new(&mut pct).range(10.0..=800.0).suffix("%").max_decimals(0).speed(1.0)).changed() {
                self.zoom_center(pct / 100.0);
            }
            if icons::small(ui, "zoom-in", "Zoom in (⌘+)").clicked() {
                self.zoom_center(self.zoom * 1.25);
            }
            if icons::small(ui, "maximize", "Fit page (⌘0)").clicked() {
                self.grid = false;
                self.fit_page();
            }
            vline(ui, 16.0);
            let tip = if self.grid { "Back to the page view (G)" } else { "Page grid: see all pages (G)" };
            let r = icons::small(ui, "layout-grid", tip);
            if self.grid {
                ui.painter().rect_stroke(r.rect, 3.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
            }
            if r.clicked() {
                self.toggle_grid();
            }
            if icons::small(ui, "expand", if cfg!(target_os = "macos") { "Full screen (⌃⌘F)" } else { "Full screen (F11)" }).clicked() {
                let ctx = ui.ctx().clone();
                self.toggle_fullscreen(&ctx);
            }
            if self.agent.is_some() {
                vline(ui, 16.0);
                self.agent_badge(ui);
            }
            vline(ui, 16.0);
            ui.label(egui::RichText::new(&self.status).color(t.text_faint));
        });
    }

    /// The one bar of Full Screen: every tool in a row with the page and zoom controls, then the
    /// options of the current tool (colour, width, fill, …) exactly as in the options bar.
    fn fullscreen_bar(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let picture = self.picture_sel().is_some();
        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
        ui.horizontal(|ui| {
            for (g, group) in Self::tool_groups().into_iter().enumerate() {
                if g > 0 {
                    vline(ui, 22.0);
                }
                for tool in group {
                    if tool.is_pen() {
                        self.pen_slot(ui);
                        continue;
                    }
                    let tip = TOOLS.iter().find(|x| x.0 == tool).map(|x| x.2).unwrap_or("");
                    let enabled = !tool.is_raster() || picture;
                    let tip = if enabled { tip.to_owned() } else { format!("{tip} · select a picture first") };
                    if icons::button_state(ui, tool.icon(), self.tool == tool, enabled, &tip).clicked() {
                        self.set_tool(tool);
                    }
                }
            }
            vline(ui, 22.0);
            if icons::button(ui, "image", false, "Place picture… (or drag a picture onto a page)").clicked() {
                let ctx = ui.ctx().clone();
                self.insert_image_dialog(&ctx);
            }
            if icons::button(ui, "file-plus", false, "Insert blank page after the current page").clicked() {
                self.add_blank(self.current + 1);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 2.0);
                if icons::button(ui, "minimize", false, if cfg!(target_os = "macos") { "Leave full screen (⌃⌘F, Esc)" } else { "Leave full screen (F11, Esc)" }).clicked() {
                    let ctx = ui.ctx().clone();
                    self.toggle_fullscreen(&ctx);
                }
                vline(ui, 22.0);
                let tip = if self.grid { "Back to the page view (G)" } else { "Page grid: see all pages (G)" };
                let r = icons::small(ui, "layout-grid", tip);
                if self.grid {
                    ui.painter().rect_stroke(r.rect, 3.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
                }
                if r.clicked() {
                    self.toggle_grid();
                }
                if icons::small(ui, "maximize", "Fit page (⌘0)").clicked() {
                    self.grid = false;
                    self.fit_page();
                }
                if icons::small(ui, "zoom-in", "Zoom in (⌘+)").clicked() {
                    self.zoom_center(self.zoom * 1.25);
                }
                let mut pct = self.zoom * 100.0;
                if ui.add(egui::DragValue::new(&mut pct).range(10.0..=800.0).suffix("%").max_decimals(0).speed(1.0)).changed() {
                    self.zoom_center(pct / 100.0);
                }
                if icons::small(ui, "zoom-out", "Zoom out (⌘−)").clicked() {
                    self.zoom_center(self.zoom / 1.25);
                }
                vline(ui, 16.0);
                ui.label(egui::RichText::new(format!("Page {} of {}", self.current + 1, self.doc.pages.len())).color(t.text_dim));
            });
        });
        widgets_hline(ui);
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 30.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 2.0);
            self.options_bar(ui);
        });
    }

    // ------------------------------------------------------------------------------- canvas

    fn canvas(&mut self, ui: &mut Ui) {
        if self.grid {
            self.grid_view(ui);
            let jobs = std::mem::take(&mut self.thumb_want);
            self.send_jobs(jobs);
            return;
        }
        self.upload_dirty();
        let t = Tokens::get(ui.ctx());
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        self.view = rect;
        if self.fit_on_next {
            self.fit_on_next = false;
            let w = self.content_size().x;
            self.zoom = (rect.width() / w).clamp(0.1, 1.5);
        }
        let resp = ui.interact(rect, Id::new("canvas"), Sense::click_and_drag());
        let (pointer, zoom_delta, scroll_delta) = ui.input(|i| (i.pointer.interact_pos().or(i.pointer.hover_pos()), i.zoom_delta(), i.smooth_scroll_delta));
        let old_scroll = self.scroll;
        if resp.contains_pointer() {
            if zoom_delta != 1.0 {
                if let Some(p) = pointer {
                    self.zoom_at(p, self.zoom * zoom_delta);
                }
            } else {
                self.scroll -= scroll_delta;
            }
        }
        self.clamp_scroll();
        let mut rects = self.page_rects();

        // Page under the view centre becomes current while scrolling.
        if self.scroll != old_scroll && self.drag.is_none() {
            let cy = rect.center().y;
            if let Some(i) = rects.iter().position(|r| r.bottom() + GAP * self.zoom > cy) {
                if i != self.current {
                    self.current = i;
                    self.scroll_thumbs_to = Some(i);
                }
            }
        }

        self.handle_drops(&ctx, &rects);
        self.canvas_input(ui, &resp, &rects, pointer);
        rects = self.page_rects();

        // Paint pages.
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.canvas);
        let ppp = ctx.pixels_per_point();
        let mut jobs = Vec::new();
        let mut visible = HashSet::new();
        let editing = self.editing.as_ref().map(|e| (e.page, e.idx));
        for (i, (page, r)) in self.doc.pages.iter().zip(&rects).enumerate() {
            if !r.intersects(rect) {
                continue;
            }
            painter.rect_filled(r.translate(vec2(0.0, 2.0)).expand(1.0), 0.0, Color32::from_black_alpha(60));
            painter.rect_filled(*r, 0.0, Color32::WHITE);
            if let PageSrc::Original(src) = page.src {
                visible.insert(src);
                let want = quantize(clamp_scale(page.size, self.zoom * ppp));
                match self.page_tex.get(&src) {
                    Some((s, tex)) => {
                        paint_page_texture(&painter, page, *r, tex);
                        if (*s - want).abs() > 1e-3 {
                            jobs.push(Job { page: src, scale: want, thumb: false });
                        }
                    }
                    None => jobs.push(Job { page: src, scale: want, thumb: false }),
                }
            }
            let clip = painter.with_clip_rect(r.intersect(rect));
            for (k, o) in page.objects.iter().enumerate() {
                if editing != Some((i, k)) {
                    match self.live_texture(i, k) {
                        Some(tid) => paint_image(&clip, page, r.min, self.zoom, o.rect, tid),
                        None => paint_obj(&clip, page, r.min, self.zoom, o),
                    }
                }
            }
        }

        // Live previews of the current drag.
        if let Some(d) = &self.drag {
            match d {
                Drag::Draw { page, pts } => {
                    let (hl, color, width) = self.ink_style();
                    let o = Obj { rect: Rect::from_min_size(Pos2::ZERO, vec2(1.0, 1.0)), kind: Kind::Ink { strokes: Arc::new(vec![pts.clone()]), color, width, highlighter: hl } };
                    paint_obj(&painter, &self.doc.pages[*page], rects[*page].min, self.zoom, &o);
                }
                Drag::SignBox { page, start, cur } => {
                    let sr = screen_rect(&self.doc.pages[*page], rects[*page].min, self.zoom, Rect::from_two_pos(*start, *cur));
                    painter.rect_filled(sr, 2.0, t.accent.gamma_multiply(0.15));
                    painter.rect_stroke(sr, 2.0, Stroke::new(1.5, t.accent), StrokeKind::Middle);
                }
                Drag::Freeform { page, pts } => {
                    let o = Obj { rect: Rect::from_min_size(Pos2::ZERO, vec2(1.0, 1.0)), kind: Kind::Ink { strokes: Arc::new(vec![pts.clone()]), color: self.opts.color, width: self.opts.width, highlighter: false } };
                    paint_obj(&painter, &self.doc.pages[*page], rects[*page].min, self.zoom, &o);
                }
                Drag::Create { page, start, cur } => {
                    if let Some(o) = self.shape_from_drag(*start, *cur, ui.input(|i| i.modifiers.shift)) {
                        paint_obj(&painter, &self.doc.pages[*page], rects[*page].min, self.zoom, &o);
                    }
                }
                _ => {}
            }
        }

        // The signature box chosen in the sign dialog.
        if let Some((p, r)) = self.sign_dlg.as_ref().and_then(|d| d.place).or_else(|| self.sign_placing.as_ref().and_then(|d| d.place)) {
            if p < rects.len() {
                let sr = screen_rect(&self.doc.pages[p], rects[p].min, self.zoom, r);
                painter.rect_filled(sr, 2.0, t.accent.gamma_multiply(0.12));
                painter.extend(Shape::dashed_line(&[sr.left_top(), sr.right_top(), sr.right_bottom(), sr.left_bottom(), sr.left_top()], Stroke::new(1.5, t.accent), 6.0, 4.0));
                painter.text(sr.left_top() + vec2(4.0, 3.0), egui::Align2::LEFT_TOP, "Digital signature", FontId::proportional(11.0), t.accent);
            }
        }
        if self.sign_placing.is_some() {
            ctx.set_cursor_icon(CursorIcon::Crosshair);
        }

        // Selection frame and handles.
        if let Some((p, i)) = self.sel {
            if let Some(o) = self.doc.pages.get(p).and_then(|pg| pg.objects.get(i)) {
                let pg = &self.doc.pages[p];
                let sr = screen_rect(pg, rects[p].min, self.zoom, o.rect);
                let pen_edit = self.tool.is_pen() && matches!(o.kind, Kind::Path { .. });
                if !is_line(o) && !pen_edit {
                    painter.rect_stroke(sr, 0.0, Stroke::new(1.0, t.accent), StrokeKind::Outside);
                }
                if editing.is_none() && !pen_edit && !self.tool.is_raster() {
                    for (_, hp) in handles(o) {
                        let s = rects[p].min + pg.to_view(hp).to_vec2() * self.zoom;
                        let hr = Rect::from_center_size(s, vec2(8.0, 8.0));
                        painter.rect_filled(hr, 1.0, Color32::WHITE);
                        painter.rect_stroke(hr, 1.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
                    }
                }
            }
        }

        self.text_overlay(&painter, &rects);
        self.pen_overlay(&painter, &rects, pointer);
        self.raster_overlay(&painter, &rects, pointer);
        self.text_editor(ui, &rects);

        // Drop hint.
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            painter.rect_filled(rect, 0.0, Color32::from_black_alpha(90));
            painter.rect_stroke(rect.shrink(8.0), 8.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, "Drop a picture on a page to place it, or a PDF to open it", FontId::proportional(16.0), Color32::WHITE);
        }

        self.scrollbar(ui, rect);

        // Free textures of pages far from view; keep a few for quick scroll-back.
        if self.page_tex.len() > visible.len() + 4 {
            self.page_tex.retain(|k, _| visible.contains(k));
        }
        if self.thumbs.len() > MAX_THUMBS.max(self.thumb_seen.len() + 8) {
            let seen = &self.thumb_seen;
            self.thumbs.retain(|k, _| seen.contains(k));
        }
        jobs.extend(std::mem::take(&mut self.thumb_want));
        self.send_jobs(jobs);
    }

    fn send_jobs(&mut self, jobs: Vec<Job>) {
        if jobs != self.last_want {
            self.renderer.want(jobs.clone());
            self.last_want = jobs;
        }
    }

    /// Every page as a thumbnail. Click selects, double-click opens the page, drag reorders,
    /// right-click offers the page commands.
    fn grid_view(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let rect = ui.max_rect();
        self.view = rect;
        ui.painter().rect_filled(rect, 0.0, t.canvas);
        // ⌘-scroll or pinch changes the thumbnail size.
        let zd = ui.input(|i| i.zoom_delta());
        if zd != 1.0 && ui.rect_contains_pointer(rect) {
            self.grid_size = (self.grid_size * zd).clamp(80.0, 400.0);
        }
        let w = self.grid_size;
        let cell = vec2(w + 28.0, w * 1.3 + 44.0);
        let n = self.doc.pages.len();
        let mut open = None;
        let mut select = None;
        let mut menu_action: Option<(usize, u8)> = None;
        let mut reorder: Option<(usize, usize)> = None;
        let scroll_to = self.scroll_thumbs_to.take();
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        egui::ScrollArea::vertical().id_salt(self.wid("grid")).auto_shrink([false, false]).show(&mut child, |ui| {
            let avail = ui.available_width();
            let cols = ((avail - 24.0) / cell.x).floor().max(1.0) as usize;
            let rows = n.div_ceil(cols);
            let left = ((avail - cols as f32 * cell.x) / 2.0).max(12.0);
            let (area, _) = ui.allocate_exact_size(vec2(avail, rows as f32 * cell.y + 24.0), Sense::hover());
            let cell_rect = |i: usize| Rect::from_min_size(area.min + vec2(left + (i % cols) as f32 * cell.x, 12.0 + (i / cols) as f32 * cell.y), cell);
            // Insertion slot under the pointer while dragging a page (none once it leaves the grid).
            let visible = ui.clip_rect().intersect(area);
            let slot = self.grid_drag.and(pointer).filter(|p| visible.contains(*p)).map(|p| grid_slot(p, area, left, cols, cell, n));
            self.drop_zone = Some(DropZone::Grid { area: visible, left, cols, cell, n });
            for i in 0..n {
                let cr = cell_rect(i);
                if scroll_to == Some(i) {
                    ui.scroll_to_rect(cr, Some(egui::Align::Center));
                }
                if !ui.is_rect_visible(cr) && self.grid_drag != Some(i) {
                    continue;
                }
                let page = &self.doc.pages[i];
                let vs = page.view_size();
                let fit = (w / vs.x).min(w * 1.3 / vs.y);
                let tr = Rect::from_center_size(pos2(cr.center().x, cr.top() + 8.0 + w * 0.65), vs * fit);
                let resp = ui.interact(cr, Id::new(("grid-page", i)), Sense::click_and_drag());
                let on = i == self.current;
                if on {
                    ui.painter().rect_filled(cr.shrink(3.0), t.radius, t.row_selected);
                } else if resp.hovered() {
                    ui.painter().rect_filled(cr.shrink(3.0), t.radius, t.hover);
                }
                ui.painter().rect_filled(tr.translate(vec2(0.0, 2.0)).expand(1.0), 0.0, Color32::from_black_alpha(60));
                let faded = self.grid_drag == Some(i);
                paint_thumb(ui.painter(), page, tr, ppp, &self.thumbs, &mut self.thumb_want, &mut self.thumb_seen);
                if faded {
                    ui.painter().rect_filled(tr, 0.0, Color32::from_black_alpha(120));
                }
                ui.painter().rect_stroke(tr, 0.0, Stroke::new(if on { 2.0 } else { 1.0 }, if on { t.accent } else { t.separator }), StrokeKind::Outside);
                let label = if page.objects.is_empty() { format!("{}", i + 1) } else { format!("{}  ·  {} edits", i + 1, page.objects.len()) };
                ui.painter().text(pos2(cr.center().x, cr.bottom() - 16.0), egui::Align2::CENTER_CENTER, label, FontId::proportional(12.0), if on { t.text } else { t.text_dim });
                if resp.double_clicked() {
                    open = Some(i);
                } else if resp.clicked() {
                    select = Some(i);
                }
                if resp.drag_started() {
                    self.grid_drag = Some(i);
                    select = Some(i);
                }
                if resp.drag_stopped() {
                    if let Some(to) = slot {
                        reorder = Some((i, to));
                    }
                    self.drag_released = Some((i, slot.is_some()));
                    self.grid_drag = None;
                }
                resp.context_menu(|ui| {
                    for (k, label) in [(0, "Insert Blank Page After"), (1, "Duplicate Page"), (2, "Rotate Right 90°"), (3, "Rotate Left 90°"), (4, "Delete Page"), (5, "Open Page")] {
                        if ui.button(label).clicked() {
                            menu_action = Some((i, k));
                            ui.close();
                        }
                    }
                });
            }
            if let (Some(to), Some(from)) = (slot, self.grid_drag) {
                if to != from && to != from + 1 {
                    paint_grid_marker(ui.painter(), to, area, left, cols, cell, n, t.accent);
                }
                ctx.set_cursor_icon(CursorIcon::Grabbing);
            }
        });
        if let Some(i) = select {
            self.current = i;
        }
        if let Some((from, to)) = reorder {
            self.move_page_to(from, to);
        }
        if let Some((i, k)) = menu_action {
            self.current = i;
            match k {
                0 => self.add_blank(i + 1),
                1 => self.duplicate_page(i),
                2 => self.rotate_page(i, 90),
                3 => self.rotate_page(i, 270),
                4 => self.delete_page(i),
                _ => open = Some(i),
            }
        }
        if let Some(i) = open {
            self.current = i;
            self.grid = false;
            self.scroll_to_page(i);
        }
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            ui.painter().rect_filled(rect, 0.0, Color32::from_black_alpha(90));
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "Drop a PDF to open it", FontId::proportional(16.0), Color32::WHITE);
        }
        self.handle_drops(&ctx, &[]);
    }

    fn ink_style(&self) -> (bool, Color32, f32) {
        if self.tool == Tool::Highlighter { (true, self.opts.hl_color, self.opts.hl_width) } else { (false, self.opts.color, self.opts.width) }
    }

    /// The object a shape tool would create for a drag from `a` to `b` (page points).
    fn shape_from_drag(&self, a: Pos2, mut b: Pos2, shift: bool) -> Option<Obj> {
        let (shape, stroke, fill, width) = match self.tool {
            Tool::Rect => (ShapeKind::Rect, self.opts.color, self.opts.fill, self.opts.width),
            Tool::Ellipse => (ShapeKind::Ellipse, self.opts.color, self.opts.fill, self.opts.width),
            Tool::Whiteout => (ShapeKind::Rect, Color32::WHITE, Some(Color32::WHITE), 0.0),
            Tool::Line => (ShapeKind::Line, self.opts.color, None, self.opts.width),
            Tool::Arrow => (ShapeKind::Arrow, self.opts.color, None, self.opts.width),
            _ => return None,
        };
        if matches!(shape, ShapeKind::Line | ShapeKind::Arrow) {
            if shift {
                let d = b - a;
                let ang = (d.y.atan2(d.x) / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
                b = a + vec2(ang.cos(), ang.sin()) * d.length();
            }
            let mut o = Obj { rect: Rect::NOTHING, kind: Kind::Shape { shape, stroke, fill, width, a: Pos2::ZERO, b: Pos2::ZERO } };
            set_line(&mut o, a, b);
            return Some(o);
        }
        if shift {
            let d = b - a;
            let s = d.x.abs().max(d.y.abs());
            b = a + vec2(s.copysign(d.x), s.copysign(d.y));
        }
        Some(Obj { rect: Rect::from_two_pos(a, b), kind: Kind::Shape { shape, stroke, fill, width, a: pos2(0.0, 0.0), b: pos2(1.0, 1.0) } })
    }

    fn canvas_input(&mut self, ui: &mut Ui, resp: &egui::Response, rects: &[Rect], pointer: Option<Pos2>) {
        let (pressed, middle, any_down, released, shift, delta) = ui.input(|i| {
            (i.pointer.primary_pressed(), i.pointer.button_pressed(egui::PointerButton::Middle), i.pointer.any_down(), i.pointer.any_released(), i.modifiers.shift, i.pointer.delta())
        });
        let space = ui.input(|i| i.key_down(Key::Space)) && !ui.ctx().egui_wants_keyboard_input();
        let z = self.zoom;

        // Cursor feedback.
        if resp.hovered() {
            let icon = if matches!(self.drag, Some(Drag::Pan)) {
                CursorIcon::Grabbing
            } else if space || self.tool == Tool::Hand {
                CursorIcon::Grab
            } else {
                match self.tool {
                    Tool::Text => CursorIcon::Text,
                    Tool::SelectText => {
                        let over = pointer.and_then(|p| self.page_at(rects, p)).is_some_and(|(pi, pp)| self.cached_text(pi).is_some_and(|t| t.over_text(pp, 2.0)));
                        if over || matches!(self.drag, Some(Drag::TextSel { .. })) { CursorIcon::Text } else { CursorIcon::Default }
                    }
                    Tool::Move => {
                        let over_handle = pointer.zip(self.sel).is_some_and(|(p, (pi, oi))| {
                            self.doc.pages.get(pi).and_then(|pg| pg.objects.get(oi)).is_some_and(|o| {
                                handles(o).iter().any(|(_, hp)| (rects[pi].min + self.doc.pages[pi].to_view(*hp).to_vec2() * z - p).length() < 7.0)
                            })
                        });
                        if over_handle { CursorIcon::Crosshair } else { CursorIcon::Default }
                    }
                    _ => CursorIcon::Crosshair,
                }
            };
            ui.ctx().set_cursor_icon(icon);
        }

        if (pressed || middle) && resp.hovered() && self.drag.is_none() {
            let Some(p) = pointer else { return };
            if middle || space || self.tool == Tool::Hand {
                self.drag = Some(Drag::Pan);
                return;
            }
            self.end_edit();
            let hit = self.page_at(rects, p);
            if self.sign_placing.is_some() {
                if let Some((pi, pp)) = hit {
                    self.drag = Some(Drag::SignBox { page: pi, start: pp, cur: pp });
                }
                return;
            }
            if let Some((pi, _)) = hit {
                if pi != self.current {
                    self.current = pi;
                    self.scroll_thumbs_to = Some(pi);
                }
            }
            match self.tool {
                Tool::Move => {
                    // Handles of the selected object first.
                    if let Some((sp, si)) = self.sel {
                        if let Some(o) = self.doc.pages.get(sp).and_then(|pg| pg.objects.get(si)) {
                            for (h, hp) in handles(o) {
                                let s = rects[sp].min + self.doc.pages[sp].to_view(hp).to_vec2() * z;
                                if (s - p).length() < 7.0 {
                                    self.drag = Some(Drag::Resize { page: sp, idx: si, handle: h, orig: o.clone(), before: self.doc.pages.clone() });
                                    return;
                                }
                            }
                        }
                    }
                    self.prop_session = None;
                    let Some((pi, pp)) = hit else {
                        self.sel = None;
                        return;
                    };
                    match self.doc.pages[pi].objects.iter().rposition(|o| o.hit(pp, 4.0 / z)) {
                        Some(k) => {
                            self.sel = Some((pi, k));
                            let orig = self.doc.pages[pi].objects[k].rect;
                            self.drag = Some(Drag::Move { page: pi, idx: k, start: pp, orig, before: self.doc.pages.clone(), moved: false });
                        }
                        None => self.sel = None,
                    }
                }
                Tool::Text => {
                    let Some((pi, pp)) = hit else { return };
                    let before = self.doc.pages.clone();
                    if let Some(k) = self.doc.pages[pi].objects.iter().rposition(|o| matches!(o.kind, Kind::Text { .. }) && o.hit(pp, 2.0 / z)) {
                        self.start_edit(pi, k, before);
                    } else {
                        let size = self.opts.text_size;
                        let asc = text::face(self.opts.bold).ascent / text::face(self.opts.bold).upem * size;
                        let mut o = Obj {
                            rect: Rect::from_min_size(pos2(pp.x, pp.y - asc * 0.7), vec2(1.0, 1.0)),
                            kind: Kind::Text { text: String::new(), size, color: self.opts.text_color, bold: self.opts.bold },
                        };
                        o.refit_text();
                        self.doc.pages[pi].objects.push(o);
                        let k = self.doc.pages[pi].objects.len() - 1;
                        self.start_edit(pi, k, before);
                    }
                }
                Tool::PenPath | Tool::FreeformPen | Tool::CurvaturePen | Tool::AddAnchor | Tool::DeleteAnchor | Tool::ConvertPoint => {
                    let alt = ui.input(|i| i.modifiers.alt);
                    self.pen_press(rects, p, hit, alt);
                }
                Tool::Marquee
                | Tool::Lasso
                | Tool::Crop
                | Tool::Eyedropper
                | Tool::Heal
                | Tool::PaintBrush
                | Tool::CloneStamp
                | Tool::Eraser
                | Tool::Gradient
                | Tool::Blur
                | Tool::Dodge => {
                    let alt = ui.input(|i| i.modifiers.alt);
                    let ctx = ui.ctx().clone();
                    self.raster_press(&ctx, rects, p, alt);
                }
                Tool::Brush | Tool::Highlighter => {
                    if let Some((pi, pp)) = hit {
                        self.drag = Some(Drag::Draw { page: pi, pts: vec![pp] });
                    }
                }
                Tool::Line | Tool::Arrow | Tool::Rect | Tool::Ellipse | Tool::Whiteout => {
                    if let Some((pi, pp)) = hit {
                        self.drag = Some(Drag::Create { page: pi, start: pp, cur: pp });
                    }
                }
                Tool::Signature => {
                    if let Some((pi, pp)) = hit {
                        self.place_signature(pi, pp);
                    }
                }
                Tool::Comment => {
                    self.close_note();
                    if let Some((pi, pp)) = hit {
                        match self.doc.pages[pi].objects.iter().rposition(|o| matches!(o.kind, Kind::Note { .. }) && o.hit(pp, 2.0 / z)) {
                            Some(k) => self.open_note(pi, k),
                            None => self.place_note(pi, pp),
                        }
                    }
                }
                Tool::SelectText => {
                    let alt = ui.input(|i| i.modifiers.alt);
                    self.text_press(hit, alt, shift);
                }
                Tool::Snapshot => {
                    if let Some((pi, pp)) = hit {
                        self.drag = Some(Drag::Snap { page: pi, start: pp, cur: pp });
                    }
                }
                Tool::Hand => {}
            }
            return;
        }

        // Double-click selects a word, triple-click a line.
        if self.tool == Tool::SelectText && (resp.double_clicked() || resp.triple_clicked()) {
            if let Some((pi, pp)) = pointer.and_then(|p| self.page_at(rects, p)) {
                self.select_text_at(pi, pp, resp.triple_clicked());
            }
        }

        // Double-click a text object with the Move tool to edit it.
        if resp.double_clicked() && self.tool == Tool::Move {
            if let Some((pi, k)) = self.sel {
                if matches!(self.doc.pages[pi].objects[k].kind, Kind::Text { .. }) {
                    let before = self.doc.pages.clone();
                    self.start_edit(pi, k, before);
                }
            }
        }

        let Some(drag) = &mut self.drag else { return };
        if any_down {
            let pp_of = |page: usize| pointer.map(|p| self.doc.pages[page].from_view(((p - rects[page].min) / z).to_pos2()));
            match drag {
                Drag::Pan => {
                    self.scroll -= delta;
                    self.clamp_scroll();
                }
                Drag::VScroll { .. } => {}
                Drag::SignBox { page, cur, .. } => {
                    if let Some(pp) = pp_of(*page) {
                        *cur = pp;
                    }
                }
                Drag::PenPull { page } | Drag::Freeform { page, .. } | Drag::PathPoint { page, .. } => {
                    if let Some(pp) = pp_of(*page) {
                        self.pen_drag(pp);
                    }
                }
                Drag::Move { page, idx, start, orig, moved, .. } => {
                    if let Some(pp) = pp_of(*page) {
                        let d = pp - *start;
                        if d.length() > 0.0 {
                            *moved = true;
                        }
                        self.doc.pages[*page].objects[*idx].rect = orig.translate(d);
                    }
                }
                Drag::Resize { page, idx, handle, orig, .. } => {
                    if let Some(pp) = pp_of(*page) {
                        self.doc.pages[*page].objects[*idx] = resize(orig, *handle, pp, shift);
                    }
                }
                Drag::Draw { page, pts } => {
                    if let Some(pp) = pp_of(*page) {
                        if pts.last().is_none_or(|l| (pp - *l).length() > 0.75 / z) {
                            pts.push(pp);
                        }
                    }
                }
                Drag::Create { page, cur, .. } | Drag::Snap { page, cur, .. } => {
                    if let Some(pp) = pp_of(*page) {
                        *cur = pp;
                    }
                }
                Drag::TextSel { page, .. } => {
                    if let Some(pp) = pp_of(*page) {
                        self.text_drag(pp);
                    }
                }
                Drag::RPaint { .. } | Drag::RSelect { .. } | Drag::RLasso { .. } | Drag::RCrop { .. } | Drag::RGradient { .. } => {
                    if let Some(p) = pointer {
                        self.raster_drag(rects, p);
                    }
                }
            }
        }
        if released || !any_down {
            let drag = self.drag.take().expect("drag");
            match drag {
                Drag::Move { before, moved: true, .. } | Drag::Resize { before, .. } => self.doc.push_undo(before),
                Drag::SignBox { page, start, cur } => {
                    let size = self.doc.pages[page].size;
                    let r = if (cur - start).length() * z < 6.0 { Rect::from_center_size(start, vec2(180.0, 60.0)) } else { Rect::from_two_pos(start, cur) };
                    let r = Rect::from_min_size(r.min, r.size().max(vec2(40.0, 20.0))).intersect(Rect::from_min_size(Pos2::ZERO, size));
                    if let Some(mut d) = self.sign_placing.take() {
                        d.place = Some((page, r));
                        d.error = None;
                        self.sign_dlg = Some(d);
                        self.status = "Signature box placed.".into();
                    }
                }
                Drag::Move { page, idx, moved: false, .. } => {
                    if matches!(self.doc.pages.get(page).and_then(|p| p.objects.get(idx)).map(|o| &o.kind), Some(Kind::Note { .. })) {
                        self.open_note(page, idx);
                    }
                }
                Drag::Draw { page, pts } => {
                    let (hl, color, width) = self.ink_style();
                    let (rect, unit) = ink_from_points(&[pts], width / 2.0);
                    self.doc.checkpoint();
                    self.doc.pages[page].objects.push(Obj { rect, kind: Kind::Ink { strokes: Arc::new(unit), color, width, highlighter: hl } });
                }
                Drag::Create { page, start, cur } => {
                    let tiny = (cur - start).length() < 3.0 / z;
                    let o = if tiny && matches!(self.tool, Tool::Rect | Tool::Ellipse | Tool::Whiteout) {
                        self.shape_from_drag(start - vec2(60.0, 30.0), start + vec2(60.0, 30.0), false)
                    } else if tiny {
                        None
                    } else {
                        self.shape_from_drag(start, cur, shift)
                    };
                    if let Some(o) = o {
                        self.doc.checkpoint();
                        self.doc.pages[page].objects.push(o);
                        self.sel = Some((page, self.doc.pages[page].objects.len() - 1));
                    }
                }
                Drag::Snap { page, start, cur } => {
                    // A click without a drag takes the whole page.
                    let size = self.doc.pages[page].size;
                    let r = if (cur - start).length() * z < 4.0 { Rect::from_min_size(Pos2::ZERO, size) } else { Rect::from_two_pos(start, cur) };
                    let ctx = ui.ctx().clone();
                    self.take_snapshot(&ctx, page, r);
                }
                d @ (Drag::Freeform { .. } | Drag::PathPoint { .. }) => self.pen_release(d),
                d @ (Drag::RPaint { .. } | Drag::RSelect { .. } | Drag::RLasso { .. } | Drag::RCrop { .. } | Drag::RGradient { .. }) => self.raster_release(d),
                _ => {}
            }
        }
    }

    /// The in-place text editor over the object being typed into.
    fn text_editor(&mut self, ui: &mut Ui, rects: &[Rect]) {
        let edit_id = self.wid("pdf-text-edit");
        let Some(ed) = &mut self.editing else { return };
        let (p, i) = (ed.page, ed.idx);
        let Some(obj) = self.doc.pages.get(p).and_then(|pg| pg.objects.get(i)) else {
            self.editing = None;
            return;
        };
        let Kind::Text { text, size, color, bold } = &obj.kind else { return };
        let (size, color, bold) = (*size, *color, *bold);
        let z = self.zoom;
        let pg = &self.doc.pages[p];
        let sr = screen_rect(pg, rects[p].min, z, obj.rect);
        let font = FontId::new(size * z, doc_family(bold));
        let lh = text::face(bold).line_height(size) * z;
        let mut buf = text.clone();
        let mut layouter = |ui: &Ui, b: &dyn egui::TextBuffer, _w: f32| {
            let mut job = egui::text::LayoutJob::single_section(
                b.as_str().to_owned(),
                egui::TextFormat { font_id: font.clone(), color, line_height: Some(lh), ..Default::default() },
            );
            job.wrap.max_width = f32::INFINITY;
            ui.ctx().fonts_mut(|f| f.layout_job(job))
        };
        let area = Rect::from_min_size(sr.min, vec2(sr.width() + size * z * 2.0, sr.height().max(lh)));
        let te = egui::TextEdit::multiline(&mut buf)
            .id(edit_id)
            .frame(egui::Frame::NONE)
            .margin(egui::Margin::ZERO)
            .desired_width(area.width())
            .desired_rows(1)
            .background_color(Color32::TRANSPARENT)
            .layouter(&mut layouter);
        let r = ui.put(area, te);
        let t = Tokens::get(ui.ctx());
        ui.painter().rect_stroke(area.expand(3.0), 2.0, Stroke::new(1.0, t.accent), StrokeKind::Outside);
        if ed.focus {
            r.request_focus();
            ed.focus = false;
        }
        let lost = r.lost_focus();
        if buf != *text {
            if let Kind::Text { text, .. } = &mut self.doc.pages[p].objects[i].kind {
                *text = buf;
            }
            self.doc.pages[p].objects[i].refit_text();
            self.doc.dirty = true;
        }
        if lost {
            self.end_edit();
        }
    }

    fn scrollbar(&mut self, ui: &mut Ui, rect: Rect) {
        let t = Tokens::get(ui.ctx());
        let content = self.content_size().y * self.zoom;
        if content <= rect.height() {
            return;
        }
        let track = Rect::from_min_max(pos2(rect.right() - 10.0, rect.top() + 2.0), pos2(rect.right() - 2.0, rect.bottom() - 2.0));
        let frac = rect.height() / content;
        let th = (track.height() * frac).max(24.0);
        let max_scroll = content - rect.height();
        let ty = track.top() + (track.height() - th) * (self.scroll.y / max_scroll);
        let thumb = Rect::from_min_size(pos2(track.left(), ty), vec2(track.width(), th));
        let resp = ui.interact(track, Id::new("vscroll"), Sense::drag());
        if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                let grab = if thumb.contains(p) { p.y - thumb.top() } else { th / 2.0 };
                self.drag = Some(Drag::VScroll { grab });
            }
        }
        if let (Some(Drag::VScroll { grab }), Some(p)) = (&self.drag, resp.interact_pointer_pos()) {
            let f = ((p.y - grab - track.top()) / (track.height() - th)).clamp(0.0, 1.0);
            self.scroll.y = f * max_scroll;
        }
        if resp.drag_stopped() {
            self.drag = None;
        }
        let c = if resp.hovered() || resp.dragged() { t.text_dim } else { t.text_faint.gamma_multiply(0.7) };
        ui.painter().rect_filled(thumb, 4.0, c);
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(pad) = &mut self.sig_pad {
            let mut close = false;
            let mut save = false;
            let color = self.opts.sig_color;
            egui::Modal::new(Id::new("sigpad")).show(ctx, |ui| {
                ui.heading("New Signature");
                ui.add_space(8.0);
                pad.ui(ui, color);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if widgets::secondary_button(ui, "Clear", 80.0).clicked() {
                        pad.clear();
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::primary_button(ui, "Save Signature", 120.0).clicked() {
                            save = true;
                        }
                        if widgets::secondary_button(ui, "Cancel", 80.0).clicked() {
                            close = true;
                        }
                    });
                });
            });
            if save {
                match pad.finish() {
                    Some(s) => {
                        self.sigs.push(s);
                        self.sig_sel = self.sigs.len() - 1;
                        sig::save(&self.sigs);
                        self.tool = Tool::Signature;
                        self.status = "Signature saved. Click on a page to place it.".into();
                        close = true;
                    }
                    None => self.status = "Draw a signature first.".into(),
                }
            }
            if close {
                self.sig_pad = None;
            }
        }

        if self.pending.is_some() {
            let mut choice = None;
            egui::Modal::new(Id::new("unsaved")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading(format!("Save changes to “{}”?", self.doc.title()));
                ui.add_space(6.0);
                ui.label("Your changes will be lost if you don't save them.");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if widgets::secondary_button(ui, "Don't Save", 90.0).clicked() {
                        choice = Some(1);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::primary_button(ui, "Save", 80.0).clicked() {
                            choice = Some(0);
                        }
                        if widgets::secondary_button(ui, "Cancel", 80.0).clicked() {
                            choice = Some(2);
                        }
                    });
                });
            });
            match choice {
                Some(0) => {
                    if self.save(false) {
                        let a = self.pending.take().expect("pending");
                        self.run_pending(a);
                    }
                }
                Some(1) => {
                    let a = self.pending.take().expect("pending");
                    self.run_pending(a);
                }
                Some(_) => {
                    self.pending = None;
                    self.quit_cancelled = true;
                }
                None => {}
            }
        }

        if let Some(msg) = self.error.clone() {
            egui::Modal::new(Id::new("error")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading("Something went wrong");
                ui.add_space(6.0);
                ui.label(msg);
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::primary_button(ui, "OK", 80.0).clicked() {
                        self.error = None;
                    }
                });
            });
        }
    }
}

impl eframe::App for App {
    /// The first window draws itself, then every other window as an immediate viewport of
    /// the same egui context, and finally settles what the windows asked for: new windows,
    /// closed ones, quitting, and a page dropped from one window onto another.
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut shared = std::mem::take(&mut self.shared);
        shared.frame += 1;
        shared.windows = std::iter::once((self.win, self.viewport_id(), self.window_title())).chain(self.others.iter().map(|w| (w.win, w.viewport_id(), w.window_title()))).collect();
        // A PDF double-clicked in Finder (or dropped on the Dock icon) opens in a window of its own.
        #[cfg(target_os = "macos")]
        for p in crate::macopen::take() {
            self.open_or_spawn(Some(p));
        }
        self.window_ui(ui, &mut shared);
        let mut others = std::mem::take(&mut self.others);
        for w in &mut others {
            let (at, size) = w.spawn_at.unwrap_or((pos2(60.0, 60.0), vec2(1280.0, 820.0)));
            let builder = ViewportBuilder::default()
                .with_title(format!("{} — PDFflow", w.doc.title()))
                .with_position(at)
                .with_inner_size(size)
                .with_min_inner_size([760.0, 480.0])
                .with_drag_and_drop(true);
            ctx.show_viewport_immediate(w.viewport_id(), builder, |ui, _class| w.window_ui(ui, &mut shared));
        }
        self.others = others;
        self.after_windows(&ctx, &mut shared);
        self.shared = shared;
    }
}

impl App {
    fn window_title(&self) -> String {
        format!("{}{}", self.doc.title(), if self.doc.dirty { " •" } else { "" })
    }

    /// Windows other than the first are created here, next to the window that asked.
    fn after_windows(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        // New windows.
        let next = self.others.iter().map(|w| w.win).max().unwrap_or(self.win).max(self.win) + 1;
        let mut n = 0;
        let mut spawns: Vec<(usize, Option<PathBuf>)> = self.spawn.drain(..).map(|p| (0, p)).collect();
        for (k, w) in self.others.iter_mut().enumerate() {
            spawns.extend(w.spawn.drain(..).map(|p| (k + 1, p)));
        }
        for (k, path) in spawns {
            let from = if k == 0 { &*self } else { &self.others[k - 1] };
            let w = from.child(ctx, next + n, path);
            n += 1;
            self.others.push(w);
        }

        // A page dropped on another window.
        if let Some(d) = shared.drag.as_ref().filter(|d| d.released || d.frame != shared.frame).map(|d| (d.from, d.page, d.released && !d.handled)) {
            shared.drag = None;
            let (from, page, copy) = d;
            if let Some((to, at)) = shared.target.take().filter(|(to, _)| copy && from != *to) {
                self.copy_page_between(from, page, to, at);
                if let Some(vp) = self.window(to).map(|w| w.viewport_id()) {
                    ctx.send_viewport_cmd_to(vp, ViewportCommand::Focus);
                }
            }
        }

        // Quit: ask about unsaved changes window by window, then leave.
        if self.quit || self.others.iter().any(|w| w.quit) {
            self.quit = false;
            self.quitting = true;
            self.quit_cancelled = false;
            for w in &mut self.others {
                w.quit = false;
                w.quit_cancelled = false;
            }
        }
        if self.quitting && (self.quit_cancelled || self.others.iter().any(|w| w.quit_cancelled)) {
            self.quitting = false;
            self.quit_cancelled = false;
            for w in &mut self.others {
                w.quit_cancelled = false;
            }
        }
        if self.quitting {
            if let Some(w) = self.others.iter_mut().find(|w| w.doc.dirty && !w.allow_close) {
                if w.pending.is_none() {
                    w.pending = Some(Pending::Close);
                }
            } else {
                self.others.clear();
                if self.doc.dirty && !self.allow_close {
                    if self.pending.is_none() {
                        self.pending = Some(Pending::Close);
                    }
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }

        // `PDFFLOW_SHOT_WINDOWS=other.pdf` (with `PDFFLOW_SHOT`): open that file in a second
        // window and copy its first page in here, as a drop between windows would.
        if let Some(other) = std::env::var_os("PDFFLOW_SHOT_WINDOWS") {
            if self.frames == 5 {
                self.spawn.push(Some(PathBuf::from(other)));
            }
            if self.frames == 40 {
                if let Some(from) = self.others.first().map(|w| w.win) {
                    self.copy_page_between(from, 0, self.win, 1);
                }
            }
        }

        // Windows that closed, and this window, which hands itself to another when closed.
        self.others.retain(|w| !w.closed);
        if self.want_close && !self.quitting {
            self.want_close = false;
            if self.others.is_empty() {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else {
                self.promote(ctx);
            }
        }
        shared.windows = std::iter::once((self.win, self.viewport_id(), self.window_title())).chain(self.others.iter().map(|w| (w.win, w.viewport_id(), w.window_title()))).collect();
    }

    /// The first window is closing while others are open: the newest other window takes
    /// over the first window (its viewport cannot be closed without quitting), which moves to
    /// where that window was.
    fn promote(&mut self, ctx: &egui::Context) {
        let mut w = self.others.pop().expect("another window");
        let (at, size) = (w.outer_rect, w.inner_rect.size());
        std::mem::swap(self, &mut w);
        // `w` is now the old first window; keep what belongs to the application.
        self.others = std::mem::take(&mut w.others);
        self.agent = w.agent.take();
        self.quitting = w.quitting;
        self.win = w.win;
        self.closed = false;
        self.allow_close = false;
        self.want_close = false;
        self.spawn_at = None;
        self.os_fullscreen = None;
        self.fullscreen = w.fullscreen;
        self.last_title.clear();
        if at.is_positive() {
            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(at.min));
            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
        }
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    fn window(&self, win: u64) -> Option<&App> {
        if self.win == win { Some(self) } else { self.others.iter().find(|w| w.win == win) }
    }

    fn window_mut(&mut self, win: u64) -> Option<&mut App> {
        if self.win == win { Some(self) } else { self.others.iter_mut().find(|w| w.win == win) }
    }

    /// Copy page `page` of window `from` into window `to` before page `at`.
    fn copy_page_between(&mut self, from: u64, page: usize, to: u64, at: usize) {
        let Some(src) = self.window(from) else { return };
        let Some(entry) = src.doc.pages.get(page).cloned() else { return };
        let (bytes, title) = (src.doc.source.clone(), src.doc.title());
        let Some(dst) = self.window_mut(to) else { return };
        match dst.import_pages(vec![entry], bytes, at) {
            Ok(n) => dst.status = format!("Copied {n} page{} from {title} (page {}).", if n == 1 { "" } else { "s" }, page + 1),
            Err(e) => dst.error = Some(format!("Could not copy the page: {e}")),
        }
    }

    /// Where a page dragged from another window would land here, in this window's coordinates.
    fn drop_slot(&self, p: Pos2) -> Option<usize> {
        match self.drop_zone.as_ref()? {
            DropZone::List { area, rows } => {
                if !area.contains(p) {
                    return None;
                }
                Some(rows.iter().find(|(_, r)| r.center().y > p.y).map(|(i, _)| *i).unwrap_or_else(|| rows.last().map(|(i, _)| i + 1).unwrap_or(0)))
            }
            DropZone::Grid { area, left, cols, cell, n } => area.contains(p).then(|| grid_slot(p, *area, *left, *cols, *cell, *n)),
        }
    }

    /// The marker for a page dragged over this window from another one, and this window's
    /// own drag for the others to see.
    fn cross_window_drag(&mut self, ctx: &egui::Context, shared: &mut Shared) {
        let t = Tokens::get(ctx);
        let origin = self.inner_rect.min;
        if let Some(d) = shared.drag.as_ref().filter(|d| d.from != self.win && !d.released) {
            let p = d.screen - origin.to_vec2();
            match self.drop_slot(p) {
                Some(slot) => {
                    shared.target = Some((self.win, slot));
                    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, self.wid("drop-marker")));
                    match self.drop_zone.as_ref() {
                        Some(DropZone::List { area, rows }) => {
                            painter.rect_stroke(area.shrink(1.0), 3.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
                            let y = match rows.iter().find(|(i, _)| *i == slot) {
                                Some((_, r)) => r.top() + 1.0,
                                None => rows.last().map(|(_, r)| r.bottom() - 1.0).unwrap_or(area.top() + 4.0),
                            };
                            painter.line_segment([pos2(area.left() + 8.0, y), pos2(area.right() - 8.0, y)], Stroke::new(3.0, t.accent));
                        }
                        Some(DropZone::Grid { area, left, cols, cell, n }) => {
                            painter.rect_stroke(area.shrink(1.0), 3.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
                            paint_grid_marker(&painter, slot, *area, *left, *cols, *cell, *n, t.accent);
                        }
                        None => {}
                    }
                }
                None => {
                    if shared.target.is_some_and(|(w, _)| w == self.win) {
                        shared.target = None;
                    }
                }
            }
            ctx.request_repaint();
        }
        let pointer = ctx.input(|i| i.pointer.latest_pos().or(i.pointer.interact_pos()));
        if let Some((page, handled)) = self.drag_released.take() {
            let screen = shared.drag.as_ref().filter(|d| d.from == self.win).map(|d| d.screen).or_else(|| pointer.map(|p| origin + p.to_vec2()));
            shared.drag = Some(LiveDrag { from: self.win, page, screen: screen.unwrap_or(Pos2::ZERO), released: true, handled, frame: shared.frame });
        } else if let (Some(page), Some(p)) = (self.thumb_drag.or(self.grid_drag), pointer) {
            shared.drag = Some(LiveDrag { from: self.win, page, screen: origin + p.to_vec2(), released: false, handled: false, frame: shared.frame });
            ctx.request_repaint();
        }
    }

    /// One window: Photoshop layout (menu bar, options bar, tool column, canvas, dock).
    fn window_ui(&mut self, ui: &mut Ui, shared: &mut Shared) {
        let ctx = ui.ctx().clone();
        let t = Tokens::get(&ctx);
        self.receive_renders(&ctx);
        self.raster_housekeeping();
        self.two_cols = Self::tool_columns_needed(ctx.content_rect().height() - 110.0);
        if let Some(r) = ctx.input(|i| i.viewport().inner_rect) {
            self.inner_rect = r;
        }
        if let Some(r) = ctx.input(|i| i.viewport().outer_rect) {
            self.outer_rect = r;
        }
        if self.first_frame {
            self.first_frame = false;
            if self.win != 0 {
                ctx.send_viewport_cmd(ViewportCommand::Icon(Some(crate::app_icon())));
            }
        }
        self.drop_zone = None;
        self.window_list = shared.windows.clone();

        // Closing: unsaved changes are asked about first; the first window's viewport is the
        // application's, so it is only closed after the other windows have been settled.
        let close_req = ctx.input(|i| i.viewport().close_requested()) || std::mem::take(&mut self.close_me);
        let siblings = shared.windows.len() > 1;
        if close_req && !self.allow_close {
            if self.doc.dirty {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.pending = Some(Pending::Close);
            } else if self.win == 0 && siblings {
                // Another window takes this viewport over (see `promote`).
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                self.want_close = true;
            } else if self.win != 0 {
                self.closed = true;
            }
            // The first window with no others: eframe closes it, and the application ends.
        }
        if self.allow_close {
            if self.win != 0 {
                self.closed = true;
            } else if siblings {
                self.want_close = true;
            } else {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
        let modal = self.trust_dlg.is_some() || self.sign_dlg.is_some() || self.create_dlg.is_some() || self.import_dlg.is_some() || self.delete_id.is_some() || self.confirm_unsign.is_some() || self.adjusting();
        if self.pending.is_none() && self.sig_pad.is_none() && self.error.is_none() && !modal {
            self.shortcuts(&ctx);
        }

        // The green button (or a gesture) changes the window too; follow it.
        let os = ctx.input(|i| i.viewport().fullscreen);
        if os.is_some() && os != self.os_fullscreen {
            self.os_fullscreen = os;
            if let Some(f) = os {
                self.fullscreen = f;
            }
        }

        let bar = |fill| egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::symmetric(8, 4));
        if self.fullscreen {
            egui::Panel::top(self.wid("fullscreen-bar")).frame(bar(t.chrome).inner_margin(egui::Margin::symmetric(8, 3))).show(ui, |ui| self.fullscreen_bar(ui));
        } else {
            egui::Panel::top(self.wid("menu")).frame(bar(t.chrome)).show(ui, |ui| self.menu_bar(ui));
            egui::Panel::top(self.wid("options")).frame(bar(t.chrome).inner_margin(egui::Margin::symmetric(8, 3))).exact_size(36.0).show(ui, |ui| self.options_bar(ui));
            egui::Panel::bottom(self.wid("status")).frame(bar(t.chrome)).exact_size(26.0).show(ui, |ui| self.status_bar(ui));
            egui::Panel::left(self.wid("tools")).frame(bar(t.chrome).inner_margin(egui::Margin::symmetric(4, 2))).exact_size(if self.two_cols { 72.0 } else { 42.0 }).resizable(false).show(ui, |ui| self.tool_column(ui));
            egui::Panel::right(self.wid("dock"))
                .frame(egui::Frame::NONE.fill(t.dock).inner_margin(egui::Margin::symmetric(4, 0)))
                .default_size(250.0)
                .size_range(200.0..=380.0)
                .show(ui, |ui| self.dock(ui));
        }
        egui::CentralPanel::no_frame().show(ui, |ui| {
            self.sig_banner(ui);
            let r = ui.available_rect_before_wrap();
            let mut c = ui.new_child(egui::UiBuilder::new().max_rect(r));
            self.canvas(&mut c);
        });
        self.pen_flyout(&ctx);
        if self.grid {
            self.close_note();
        } else {
            let rects = self.page_rects();
            self.note_popup(&ctx, &rects);
        }
        self.adjust_window(&ctx);
        self.snapshot_window(&ctx);
        self.agent_poll(&ctx);
        self.dialogs(&ctx);
        self.sign_dialogs(&ctx);
        self.cross_window_drag(&ctx, shared);

        self.debug_screenshot(&ctx);

        let title = format!("{}{} — PDFflow", self.doc.title(), if self.doc.dirty { " •" } else { "" });
        if title != self.last_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }
}

#[cfg(test)]
impl App {
    pub(crate) fn screen_of(&self, page: usize, p: Pos2) -> Pos2 {
        self.page_rects()[page].min + self.doc.pages[page].to_view(p).to_vec2() * self.zoom
    }
    pub(crate) fn editing_text(&self) -> bool {
        self.editing.is_some()
    }
    pub(crate) fn set_signatures(&mut self, s: Vec<Signature>) {
        self.sigs = s;
        self.sig_sel = 0;
    }
    pub(crate) fn grid_on(&self) -> bool {
        self.grid
    }
    pub(crate) fn fullscreen_on(&self) -> bool {
        self.fullscreen
    }
    pub(crate) fn canvas_rect(&self) -> Rect {
        self.view
    }
    pub(crate) fn pen_drafting(&self) -> bool {
        self.pen.is_some()
    }
    pub(crate) fn flyout_open(&self) -> bool {
        self.flyout.is_some()
    }
    pub(crate) fn has_pixel_selection(&self) -> bool {
        self.pix_sel.is_some()
    }
    pub(crate) fn opts_color_for_test(&mut self, c: Color32) {
        self.opts.color = c;
    }
    pub(crate) fn brush_color(&self) -> Color32 {
        self.opts.color
    }
    pub(crate) fn select(&mut self, page: usize, idx: usize) {
        self.sel = Some((page, idx));
    }
    pub(crate) fn add_test_id(&mut self, file: PathBuf) {
        self.ids.push(crate::sig::IdEntry { file, name: "Test".into(), detail: String::new(), expires: String::new() });
    }
    pub(crate) fn signature_reports(&self) -> &[crate::dsig::SigReport] {
        &self.sig_reports
    }
    pub(crate) fn try_save(&mut self) -> bool {
        self.save(false)
    }
    pub(crate) fn unsign_prompt(&self) -> bool {
        self.confirm_unsign.is_some()
    }
    pub(crate) fn open_file(&mut self, p: PathBuf) {
        self.open_path(p);
    }
    pub(crate) fn note_open(&self) -> bool {
        self.note_edit.is_some()
    }
    pub(crate) fn start_placing_for_test(&mut self, d: SignDialog) {
        self.sign_placing = Some(d);
    }
    pub(crate) fn take_sign_dialog_for_test(&mut self) -> Option<SignDialog> {
        self.sign_dlg.take()
    }
    pub(crate) fn rendered_counts(&self) -> (usize, usize) {
        (self.page_tex.len(), self.thumbs.len())
    }
    pub(crate) fn snapshot_size(&self) -> Option<(u32, u32)> {
        self.snap.as_ref().map(|s| (s.img.width(), s.img.height()))
    }
    pub(crate) fn clipboard_is_picture(&self) -> bool {
        matches!(self.clipboard.as_ref().map(|o| &o.kind), Some(Kind::Image(_)))
    }
}

impl App {
    /// `PDFFLOW_SHOT=out.png` saves a window capture after start-up and quits (for checks).
    fn debug_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = std::env::var_os("PDFFLOW_SHOT") else { return };
        self.frames += 1;
        if self.frames == 5 && std::env::var_os("PDFFLOW_SHOT_GRID").is_some() {
            self.toggle_grid();
        }
        if self.frames == 5 {
            if let Some(pic) = std::env::var_os("PDFFLOW_SHOT_PICTURE") {
                if let Ok(b) = std::fs::read(pic) {
                    let objs = &mut self.doc.pages[0].objects;
                    let (rect, unit) = ink_from_points(&[vec![pos2(38.0, 173.0), pos2(413.0, 173.0)]], 7.0);
                    objs.push(Obj { rect, kind: Kind::Ink { strokes: Arc::new(unit), color: Color32::from_rgb(255, 220, 0), width: 14.0, highlighter: true } });
                    let mut note = Obj {
                        rect: Rect::from_min_size(pos2(205.0, 140.0), vec2(1.0, 1.0)),
                        kind: Kind::Text { text: "Key reference · อ่านแล้ว".into(), size: 13.0, color: Color32::from_rgb(200, 30, 30), bold: true },
                    };
                    note.refit_text();
                    objs.push(note);
                    self.insert_image(ctx, b, 0, Some(pos2(300.0, 330.0)));
                    self.set_tool(Tool::Marquee);
                    if let Some((p, i, _)) = self.picture_sel() {
                        if self.ensure_live_for_test(ctx) {
                            let l = self.rlive.as_ref().expect("live");
                            let (w, h) = (l.raster.w as f32, l.raster.h as f32);
                            self.pix_sel = Some(PixSel { page: p, idx: i, shape: rtools::SelShape::Rect(Rect::from_min_max(pos2(w * 0.3, h * 0.25), pos2(w * 0.7, h * 0.75))) });
                        }
                    }
                }
            }
        }
        if self.frames == 6 {
            if let Some(k) = std::env::var_os("PDFFLOW_SHOT_ADJUST") {
                self.pix_sel = None;
                self.set_tool(Tool::Move);
                self.shot_adjustment(ctx, &k.to_string_lossy());
            }
        }
        if self.frames == 5 && std::env::var_os("PDFFLOW_SHOT_SIGNDLG").is_some() {
            if self.ids.is_empty() {
                self.ids.push(crate::sig::IdEntry { file: PathBuf::from("/dev/null"), name: "สมโพธิ อยู่ไว".into(), detail: "Issued by Thai University Consortium Certification Authority".into(), expires: "2027-10-21".into() });
            }
            self.open_sign_dialog();
            if let Some(d) = &mut self.sign_dlg {
                d.place = Some((0, Rect::from_min_size(pos2(330.0, 600.0), vec2(200.0, 64.0))));
            }
        }
        if self.frames == 5 && std::env::var_os("PDFFLOW_SHOT_NOTE").is_some() {
            self.set_tool(Tool::Comment);
            self.dock_tabs[1] = 1;
            self.place_note(0, pos2(470.0, 150.0));
            if let Some(Obj { kind: Kind::Note { text, .. }, .. }) = self.doc.pages[0].objects.last_mut() {
                *text = "Please cite the original hypoplastic model here.\nตรวจสอบสมการที่ 3 อีกครั้ง".into();
            }
            self.close_note();
            self.place_note(0, pos2(40.0, 330.0));
            if let Some(Obj { kind: Kind::Note { text, color, .. }, .. }) = self.doc.pages[0].objects.last_mut() {
                *text = "Good abstract.".into();
                *color = notes::NOTE_COLORS[1];
            }
            self.close_note();
            self.open_note(0, 0);
        }
        if self.frames == 5 && std::env::var_os("PDFFLOW_SHOT_FULLSCREEN").is_some() {
            // The layout only: the window itself stays as it is, so the shot has a known size.
            self.fullscreen = true;
            self.set_tool(Tool::Brush);
        }
        if self.frames == 5 && std::env::var_os("PDFFLOW_SHOT_SIGS").is_some() {
            self.sig_panel = true;
            self.dock_tabs[2] = 1;
        }
        if self.frames == 5 && std::env::var_os("PDFFLOW_SHOT_PEN").is_some() {
            use crate::path::Node;
            let pts = [pos2(330.0, 130.0), pos2(470.0, 190.0), pos2(420.0, 330.0), pos2(300.0, 300.0)];
            let mut nodes: Vec<Node> = pts.iter().map(|p| Node::corner(*p)).collect();
            crate::path::auto_smooth(&mut nodes, &[false, false, true, false], true);
            let mut o = Obj { rect: Rect::NOTHING, kind: Kind::Path { nodes: Arc::new(vec![]), closed: true, stroke: Color32::from_rgb(200, 30, 30), fill: Some(Color32::from_rgba_unmultiplied(255, 220, 0, 255)), width: 2.0 } };
            o.set_path_abs(&nodes, true);
            self.doc.pages[0].objects.push(o);
            self.sel = Some((0, self.doc.pages[0].objects.len() - 1));
            self.set_tool(Tool::CurvaturePen);
        }
        let at: u32 = std::env::var("PDFFLOW_SHOT_FRAME").ok().and_then(|v| v.parse().ok()).unwrap_or(90);
        if self.frames == at {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        if self.frames < at + 110 {
            ctx.request_repaint();
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| if let Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                let _ = buf.save(&path);
            }
            self.doc.dirty = false;
            self.quit = true;
        }
    }
}

fn widgets_hline(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let (r, _) = ui.allocate_exact_size(vec2(26.0, 1.0), Sense::hover());
    ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, t.separator));
}

fn text_controls(ui: &mut Ui, size: &mut f32, bold: &mut bool, color: &mut Color32) -> bool {
    option_label(ui, "Size:");
    let mut c = ui.add(egui::DragValue::new(size).range(4.0..=300.0).speed(0.2).suffix(" pt").max_decimals(1)).changed();
    c |= widgets::checkbox(ui, bold, "Bold").changed();
    vline(ui, 22.0);
    option_label(ui, "Color:");
    c |= color_row(ui, color);
    c
}

fn stroke_controls(ui: &mut Ui, color: &mut Color32, width: &mut f32) -> bool {
    option_label(ui, "Color:");
    let mut c = color_row(ui, color);
    vline(ui, 22.0);
    option_label(ui, "Width:");
    c |= ui.add(egui::DragValue::new(width).range(0.0..=60.0).speed(0.1).suffix(" pt").max_decimals(1)).changed();
    c
}

/// The grid cell boundary nearest to `p`: the index a dragged page would be inserted before.
fn grid_slot(p: Pos2, area: Rect, left: f32, cols: usize, cell: Vec2, n: usize) -> usize {
    let col = ((p.x - area.min.x - left) / cell.x).clamp(0.0, cols as f32 - 0.01);
    let row = ((p.y - area.min.y - 12.0) / cell.y).max(0.0) as usize;
    let after = col.fract() > 0.5;
    (row * cols + col as usize + after as usize).min(n)
}

/// Accent bar at a grid insertion point.
#[allow(clippy::too_many_arguments)]
fn paint_grid_marker(painter: &egui::Painter, to: usize, area: Rect, left: f32, cols: usize, cell: Vec2, n: usize, color: Color32) {
    if n == 0 {
        return;
    }
    let cell_rect = |i: usize| Rect::from_min_size(area.min + vec2(left + (i % cols) as f32 * cell.x, 12.0 + (i / cols) as f32 * cell.y), cell);
    let (r, x) = if to < n { (cell_rect(to), 0.0) } else { (cell_rect(n - 1), cell.x) };
    let x = r.left() + x;
    painter.line_segment([pos2(x, r.top() + 8.0), pos2(x, r.bottom() - 28.0)], Stroke::new(3.0, color));
}

/// Accent bar above the thumbnail row a dragged page would be inserted before (or below the
/// last row).
fn paint_insert_marker(ui: &Ui, rows: &[(usize, Rect)], area: Rect, to: usize, color: Color32) {
    let y = match rows.iter().find(|(i, _)| *i == to) {
        Some((_, r)) => r.top() + 1.0,
        None => match rows.last() {
            Some((_, r)) => r.bottom() - 1.0,
            None => return,
        },
    };
    let painter = ui.painter_at(area);
    painter.line_segment([pos2(area.left() + 8.0, y), pos2(area.right() - 8.0, y)], Stroke::new(3.0, color));
}

/// Render scales snap to quarter octaves so small zoom changes reuse the texture.
fn quantize(scale: f32) -> f32 {
    2f32.powf((scale.max(0.01).log2() * 4.0).round() / 4.0)
}

/// Paint a Bezier path given in page points; `ts` maps page points to the screen.
fn paint_path(painter: &egui::Painter, abs: &[crate::path::Node], closed: bool, stroke: Color32, fill: Option<Color32>, width: f32, z: f32, ts: impl Fn(Pos2) -> Pos2) {
    let mut pts: Vec<Pos2> = crate::path::flatten(abs, closed, 24).into_iter().map(&ts).collect();
    if closed && pts.len() > 2 && (pts[0] - pts[pts.len() - 1]).length() < 1e-3 {
        pts.pop();
    }
    if pts.len() < 2 {
        return;
    }
    if let (Some(f), true) = (fill, closed) {
        let mut mesh = egui::Mesh::default();
        for p in &pts {
            mesh.colored_vertex(*p, f);
        }
        mesh.indices = crate::path::triangulate(&pts);
        painter.add(Shape::mesh(mesh));
    }
    if width > 0.0 {
        let st = Stroke::new((width * z).max(0.5), stroke);
        painter.add(if closed { Shape::closed_line(pts, st) } else { Shape::line(pts, st) });
    }
}

/// Draw a page thumbnail into `r`: the rendered page (requested when missing or too coarse)
/// with the placed objects on top.
fn paint_thumb(
    painter: &egui::Painter,
    page: &PageEntry,
    r: Rect,
    ppp: f32,
    thumbs: &HashMap<usize, (f32, TextureHandle)>,
    want: &mut Vec<Job>,
    seen: &mut HashSet<usize>,
) {
    painter.rect_filled(r, 0.0, Color32::WHITE);
    let vs = page.view_size();
    if let PageSrc::Original(src) = page.src {
        seen.insert(src);
        let need = quantize(clamp_scale(page.size, r.width() * ppp / vs.x));
        let have = thumbs.get(&src);
        if let Some((_, tex)) = have {
            paint_page_texture(painter, page, r, tex);
        }
        if have.is_none_or(|(s, _)| *s < need * 0.7) {
            let job = Job { page: src, scale: need, thumb: true };
            if !want.contains(&job) {
                want.push(job);
            }
        }
    }
    let clip = painter.with_clip_rect(r.intersect(painter.clip_rect()));
    let z = r.width() / vs.x;
    for o in &page.objects {
        paint_obj(&clip, page, r.min, z, o);
    }
}

/// Draw a rendered page texture into its view rect, rotated by the editor's page rotation.
fn paint_page_texture(painter: &egui::Painter, page: &PageEntry, view: Rect, tex: &TextureHandle) {
    let z = view.width() / page.view_size().x;
    let (w, h) = (page.size.x, page.size.y);
    let mut mesh = egui::Mesh::with_texture(tex.id());
    for (p, uv) in [(pos2(0.0, 0.0), pos2(0.0, 0.0)), (pos2(w, 0.0), pos2(1.0, 0.0)), (pos2(w, h), pos2(1.0, 1.0)), (pos2(0.0, h), pos2(0.0, 1.0))] {
        mesh.vertices.push(egui::epaint::Vertex { pos: view.min + page.to_view(p).to_vec2() * z, uv, color: Color32::WHITE });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(Shape::mesh(mesh));
}

fn screen_rect(page: &PageEntry, origin: Pos2, z: f32, r: Rect) -> Rect {
    Rect::from_two_pos(origin + page.to_view(r.min).to_vec2() * z, origin + page.to_view(r.max).to_vec2() * z)
}

fn is_line(o: &Obj) -> bool {
    matches!(o.kind, Kind::Shape { shape: ShapeKind::Line | ShapeKind::Arrow, .. })
}

fn set_line(o: &mut Obj, pa: Pos2, pb: Pos2) {
    let r = Rect::from_two_pos(pa, pb);
    let unit = |p: Pos2| pos2(if r.width() > 0.0 { (p.x - r.min.x) / r.width() } else { 0.0 }, if r.height() > 0.0 { (p.y - r.min.y) / r.height() } else { 0.0 });
    o.rect = r;
    if let Kind::Shape { a, b, .. } = &mut o.kind {
        *a = unit(pa);
        *b = unit(pb);
    }
}

/// Resize handles in page points: line end points, corners only for text, else 8 handles.
fn handles(o: &Obj) -> Vec<(usize, Pos2)> {
    if matches!(o.kind, Kind::Note { .. }) {
        return Vec::new();
    }
    if let Kind::Shape { shape: ShapeKind::Line | ShapeKind::Arrow, a, b, .. } = &o.kind {
        return vec![(0, o.at(*a)), (1, o.at(*b))];
    }
    let r = o.rect;
    let all = [r.left_top(), r.center_top(), r.right_top(), r.right_center(), r.right_bottom(), r.center_bottom(), r.left_bottom(), r.left_center()];
    all.into_iter().enumerate().filter(|(i, _)| !matches!(o.kind, Kind::Text { .. }) || i % 2 == 0).collect()
}

/// Drag handle `h` of `orig` to `p`. Pictures, text and ink keep their proportions at the
/// corners (Shift frees them); shapes are free (Shift locks them), as in Photoshop.
fn resize(orig: &Obj, h: usize, p: Pos2, shift: bool) -> Obj {
    let mut o = orig.clone();
    if let Kind::Shape { shape: ShapeKind::Line | ShapeKind::Arrow, a, b, .. } = &orig.kind {
        let (mut pa, mut pb) = (orig.at(*a), orig.at(*b));
        if h == 0 {
            pa = p;
        } else {
            pb = p;
        }
        set_line(&mut o, pa, pb);
        return o;
    }
    let r = orig.rect;
    let (mut x0, mut y0, mut x1, mut y1) = (r.min.x, r.min.y, r.max.x, r.max.y);
    match h {
        0 => (x0, y0) = (p.x, p.y),
        1 => y0 = p.y,
        2 => (x1, y0) = (p.x, p.y),
        3 => x1 = p.x,
        4 => (x1, y1) = (p.x, p.y),
        5 => y1 = p.y,
        6 => (x0, y1) = (p.x, p.y),
        _ => x0 = p.x,
    }
    let proportional = matches!(orig.kind, Kind::Image(_) | Kind::Ink { .. } | Kind::Text { .. }) != shift;
    if proportional && h % 2 == 0 && r.width() > 0.0 && r.height() > 0.0 {
        let s = ((x1 - x0).abs() / r.width()).max((y1 - y0).abs() / r.height());
        let (nw, nh) = (r.width() * s, r.height() * s);
        match h {
            0 => (x0, y0) = (x1 - nw, y1 - nh),
            2 => (x1, y0) = (x0 + nw, y1 - nh),
            4 => (x1, y1) = (x0 + nw, y0 + nh),
            _ => (x0, y1) = (x1 - nw, y0 + nh),
        }
    }
    let mut nr = Rect::from_two_pos(pos2(x0, y0), pos2(x1, y1));
    nr.set_width(nr.width().max(2.0));
    nr.set_height(nr.height().max(2.0));
    o.rect = nr;
    match &mut o.kind {
        Kind::Text { size, .. } => {
            let Kind::Text { size: s0, .. } = orig.kind else { unreachable!() };
            *size = (s0 * nr.height() / r.height()).clamp(4.0, 300.0);
            o.refit_text();
            // Keep the corner opposite the dragged one in place.
            let sz = o.rect.size();
            o.rect = match h {
                0 => Rect::from_min_size(r.max - sz, sz),
                2 => Rect::from_min_size(pos2(r.min.x, r.max.y - sz.y), sz),
                6 => Rect::from_min_size(pos2(r.max.x - sz.x, r.min.y), sz),
                _ => Rect::from_min_size(r.min, sz),
            };
        }
        Kind::Ink { width, highlighter: false, .. } => {
            let Kind::Ink { width: w0, .. } = orig.kind else { unreachable!() };
            *width = (w0 * ((nr.width() / r.width()) * (nr.height() / r.height())).sqrt()).clamp(0.3, 20.0);
        }
        _ => {}
    }
    o
}

/// Draw a picture texture into its object rect (rotated with the page).
fn paint_image(painter: &egui::Painter, page: &PageEntry, origin: Pos2, z: f32, r: Rect, tex: egui::TextureId) {
    let ts = |p: Pos2| origin + page.to_view(p).to_vec2() * z;
    let mut mesh = egui::Mesh::with_texture(tex);
    for (p, uv) in [(r.left_top(), pos2(0.0, 0.0)), (r.right_top(), pos2(1.0, 0.0)), (r.right_bottom(), pos2(1.0, 1.0)), (r.left_bottom(), pos2(0.0, 1.0))] {
        mesh.vertices.push(egui::epaint::Vertex { pos: ts(p), uv, color: Color32::WHITE });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(Shape::mesh(mesh));
}

/// Paint one placed object; `origin` is the page's top-left on screen and `z` the zoom.
pub fn paint_obj(painter: &egui::Painter, page: &PageEntry, origin: Pos2, z: f32, obj: &Obj) {
    let ts = |p: Pos2| origin + page.to_view(p).to_vec2() * z;
    match &obj.kind {
        Kind::Text { text, size, color, bold } => {
            let face = text::face(*bold);
            let lh = face.line_height(*size);
            let asc = face.ascent / face.upem * size;
            let angle = page.rotation as f32 * std::f32::consts::PI / 180.0;
            let rot = egui::emath::Rot2::from_angle(angle);
            for (i, line) in text.split('\n').enumerate() {
                if line.is_empty() {
                    continue;
                }
                let galley = painter.layout_no_wrap(line.to_owned(), FontId::new(size * z, doc_family(*bold)), *color);
                let gb = galley.rows.first().and_then(|r| r.row.glyphs.first().map(|g| r.pos.y + g.pos.y)).unwrap_or(asc * z);
                let anchor = ts(pos2(obj.rect.min.x, obj.rect.min.y + asc + i as f32 * lh));
                let pos = anchor + rot * vec2(0.0, -gb);
                painter.add(egui::epaint::TextShape::new(pos, galley, *color).with_angle(angle));
            }
        }
        Kind::Image(img) => {
            if let Some(tex) = img.texture(painter.ctx()) {
                paint_image(painter, page, origin, z, obj.rect, tex.id());
            }
        }
        Kind::Ink { strokes, color, width, highlighter } => {
            let c = if *highlighter { Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 110) } else { *color };
            let w = (width * z).max(0.5);
            for st in strokes.iter() {
                let pts: Vec<Pos2> = st.iter().map(|u| ts(obj.at(*u))).collect();
                if *highlighter && pts.len() == 2 && pts[0] != pts[1] {
                    // A straight highlight with the round caps the saved PDF has, in one shape
                    // so the translucent ends do not overlap.
                    let d = (pts[1] - pts[0]).normalized() * (w / 2.0);
                    let n = vec2(-d.y, d.x);
                    // Each cap bulges outwards: from one side through the end to the other side.
                    let cap = |c: Pos2, out: Vec2, side: Vec2| {
                        (0..=12).map(move |k| {
                            let t = std::f32::consts::PI * k as f32 / 12.0;
                            c + side * t.cos() + out * t.sin()
                        })
                    };
                    let ring: Vec<Pos2> = cap(pts[1], d, n).chain(cap(pts[0], -d, -n)).collect();
                    painter.add(Shape::convex_polygon(ring, c, Stroke::NONE));
                } else if pts.len() == 1 {
                    painter.circle_filled(pts[0], w / 2.0, c);
                } else {
                    painter.add(Shape::line(pts, Stroke::new(w, c)));
                }
            }
        }
        Kind::Note { color, .. } => {
            notes::paint_note_icon(painter, screen_rect(page, origin, z, obj.rect), *color);
        }
        Kind::Path { stroke, fill, width, .. } => {
            let (abs, closed) = obj.path_abs().unwrap_or_default();
            paint_path(painter, &abs, closed, *stroke, *fill, *width, z, ts);
        }
        Kind::Shape { shape, stroke, fill, width, a, b } => {
            let st = Stroke::new(if *width > 0.0 { (width * z).max(0.5) } else { 0.0 }, *stroke);
            let sr = screen_rect(page, origin, z, obj.rect);
            match shape {
                ShapeKind::Rect => {
                    painter.rect(sr, 0.0, fill.unwrap_or(Color32::TRANSPARENT), st, StrokeKind::Middle);
                }
                ShapeKind::Ellipse => {
                    if let Some(f) = fill {
                        painter.add(Shape::ellipse_filled(sr.center(), sr.size() / 2.0, *f));
                    }
                    if *width > 0.0 {
                        painter.add(Shape::ellipse_stroke(sr.center(), sr.size() / 2.0, st));
                    }
                }
                ShapeKind::Line | ShapeKind::Arrow => {
                    let (pa, pb) = (obj.at(*a), obj.at(*b));
                    let tip = if *shape == ShapeKind::Arrow { export::arrow_head(pa, pb, *width) } else { None };
                    let end = tip.map(|t| t[3]).unwrap_or(pb);
                    painter.line_segment([ts(pa), ts(end)], st);
                    if let Some([t, l, r, _]) = tip {
                        painter.add(Shape::convex_polygon(vec![ts(t), ts(l), ts(r)], *stroke, Stroke::NONE));
                    }
                }
            }
        }
    }
}
