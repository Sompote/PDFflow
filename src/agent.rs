//! The editor side of the MCP tools (`crate::mcp_tools` lists them). Calls arrive from the MCP
//! server thread and run here, on the UI thread between frames, through the same code paths as
//! the menus and tools, so each change shows at once and is one undo step.

use super::seltext::{Markup, markup_objects};
use super::signui::{ImageChoice, SignDialog};
use super::*;
use crate::adjust::{Adj, AdjKind, LevelsCh};
use crate::doc::NOTE_SIZE;
use crate::mcp::Call;
use crate::textlayer::PageText;
use serde_json::{Value, json};

type R = Result<Value, String>;

/// Largest picture side returned to an agent (larger ones are scaled down for the reply).
const REPLY_SIDE: u32 = 1600;

/// Typed access to a call's arguments.
struct Args<'a>(&'a Value);

impl Args<'_> {
    fn get(&self, k: &str) -> Option<&Value> {
        self.0.get(k).filter(|v| !v.is_null())
    }
    fn str(&self, k: &str) -> Result<&str, String> {
        self.opt_str(k).ok_or_else(|| format!("'{k}' (text) is required."))
    }
    fn opt_str(&self, k: &str) -> Option<&str> {
        self.get(k).and_then(Value::as_str)
    }
    fn num(&self, k: &str) -> Result<f32, String> {
        self.opt_num(k).ok_or_else(|| format!("'{k}' (number) is required."))
    }
    fn opt_num(&self, k: &str) -> Option<f32> {
        self.get(k).and_then(Value::as_f64).map(|v| v as f32).filter(|v| v.is_finite())
    }
    fn opt_int(&self, k: &str) -> Option<i64> {
        self.get(k).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f.round() as i64)))
    }
    fn bool(&self, k: &str) -> bool {
        self.get(k).and_then(Value::as_bool).unwrap_or(false)
    }
    fn opt_bool(&self, k: &str) -> Option<bool> {
        self.get(k).and_then(Value::as_bool)
    }
    fn nums(&self, k: &str, n: usize) -> Option<Result<Vec<f32>, String>> {
        let v = self.get(k)?;
        let a: Option<Vec<f32>> = v.as_array().map(|a| a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect());
        Some(match a {
            Some(a) if a.len() == n => Ok(a),
            _ => Err(format!("'{k}' must be a list of {n} numbers.")),
        })
    }
    fn opt_rect(&self, k: &str) -> Result<Option<Rect>, String> {
        self.nums(k, 4).transpose().map(|o| o.map(|a| Rect::from_two_pos(pos2(a[0], a[1]), pos2(a[2], a[3]))))
    }
    fn rect(&self, k: &str) -> Result<Rect, String> {
        self.opt_rect(k)?.ok_or_else(|| format!("'{k}' ([x0, y0, x1, y1] in points) is required."))
    }
    fn point(&self, k: &str) -> Result<Pos2, String> {
        let a = self.nums(k, 2).ok_or_else(|| format!("'{k}' ([x, y]) is required."))??;
        Ok(pos2(a[0], a[1]))
    }
    fn color(&self, k: &str) -> Result<Option<Color32>, String> {
        self.opt_str(k).map(parse_color).transpose()
    }
    fn points_of(v: &Value) -> Option<Vec<Pos2>> {
        v.as_array()?.iter().map(|p| Some(pos2(p.get(0)?.as_f64()? as f32, p.get(1)?.as_f64()? as f32))).collect()
    }
}

pub(crate) fn parse_color(s: &str) -> Result<Color32, String> {
    let named = match s.trim().to_ascii_lowercase().as_str() {
        "black" => Some([0, 0, 0]),
        "white" => Some([255, 255, 255]),
        "red" => Some([220, 30, 30]),
        "green" => Some([20, 150, 60]),
        "blue" => Some([30, 80, 220]),
        "yellow" => Some([255, 220, 0]),
        "orange" => Some([255, 140, 0]),
        "purple" => Some([130, 50, 180]),
        "gray" | "grey" => Some([128, 128, 128]),
        _ => None,
    };
    if let Some([r, g, b]) = named {
        return Ok(Color32::from_rgb(r, g, b));
    }
    let h = s.trim().trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2).unwrap_or("zz"), 16);
    match h.len() {
        6 => Ok(Color32::from_rgb(byte(0).map_err(|_| bad_color(s))?, byte(2).map_err(|_| bad_color(s))?, byte(4).map_err(|_| bad_color(s))?)),
        8 => Ok(Color32::from_rgba_unmultiplied(byte(0).map_err(|_| bad_color(s))?, byte(2).map_err(|_| bad_color(s))?, byte(4).map_err(|_| bad_color(s))?, byte(6).map_err(|_| bad_color(s))?)),
        _ => Err(bad_color(s)),
    }
}

fn bad_color(s: &str) -> String {
    format!("'{s}' is not a colour; use '#rrggbb' or a name such as red.")
}

fn hex(c: Color32) -> String {
    if c.a() == 255 { format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b()) } else { format!("#{:02x}{:02x}{:02x}{:02x}", c.r(), c.g(), c.b(), c.a()) }
}

fn r1(v: f32) -> f64 {
    (v as f64 * 10.0).round() / 10.0
}

fn bx(r: Rect) -> Value {
    json!([r1(r.min.x), r1(r.min.y), r1(r.max.x), r1(r.max.y)])
}

/// `~/x` and paths relative to the open document's folder (else the home folder).
fn resolve(p: &str, doc: &Doc) -> PathBuf {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_default();
    let path = match p.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(p),
    };
    if path.is_absolute() {
        return path;
    }
    doc.path.as_ref().and_then(|d| d.parent().map(|d| d.join(&path))).unwrap_or_else(|| home.join(path))
}

/// A picture for the reply: base64 PNG, at most `REPLY_SIDE` on its long side.
fn reply_png(img: &image::RgbaImage) -> Result<(String, u32, u32), String> {
    use base64::Engine as _;
    let small;
    let img = if img.width().max(img.height()) > REPLY_SIDE {
        small = image::DynamicImage::ImageRgba8(img.clone()).thumbnail(REPLY_SIDE, REPLY_SIDE).to_rgba8();
        &small
    } else {
        img
    };
    let png = crate::snapshot::encode(img, image::ImageFormat::Png)?;
    Ok((base64::engine::general_purpose::STANDARD.encode(png), img.width(), img.height()))
}

fn tool_of(name: &str) -> Option<Tool> {
    Some(match name {
        "move" => Tool::Move,
        "hand" => Tool::Hand,
        "select_text" => Tool::SelectText,
        "snapshot" => Tool::Snapshot,
        "text" => Tool::Text,
        "brush" => Tool::Brush,
        "highlighter" => Tool::Highlighter,
        "pen" => Tool::PenPath,
        "freeform_pen" => Tool::FreeformPen,
        "curvature_pen" => Tool::CurvaturePen,
        "add_anchor" => Tool::AddAnchor,
        "delete_anchor" => Tool::DeleteAnchor,
        "convert_point" => Tool::ConvertPoint,
        "line" => Tool::Line,
        "arrow" => Tool::Arrow,
        "rectangle" => Tool::Rect,
        "ellipse" => Tool::Ellipse,
        "whiteout" => Tool::Whiteout,
        "signature" => Tool::Signature,
        "comment" => Tool::Comment,
        "marquee" => Tool::Marquee,
        "lasso" => Tool::Lasso,
        "crop" => Tool::Crop,
        "eyedropper" => Tool::Eyedropper,
        "healing" => Tool::Heal,
        "paint_brush" => Tool::PaintBrush,
        "clone_stamp" => Tool::CloneStamp,
        "eraser" => Tool::Eraser,
        "gradient" => Tool::Gradient,
        "blur" => Tool::Blur,
        "dodge" => Tool::Dodge,
        _ => return None,
    })
}

/// What an object is, for agents.
fn kind_name(o: &Obj) -> &'static str {
    match &o.kind {
        Kind::Text { .. } => "text",
        Kind::Image(_) => "picture",
        Kind::Ink { highlighter: true, .. } => "highlight",
        Kind::Ink { .. } => "ink",
        Kind::Path { .. } => "path",
        Kind::Note { .. } => "comment",
        Kind::Shape { shape, fill, width, .. } => match shape {
            ShapeKind::Rect if *width == 0.0 && *fill == Some(Color32::WHITE) => "whiteout",
            ShapeKind::Rect => "rectangle",
            ShapeKind::Ellipse => "ellipse",
            ShapeKind::Line => "line",
            ShapeKind::Arrow => "arrow",
        },
    }
}

fn describe(i: usize, o: &Obj) -> Value {
    let mut v = json!({"index": i, "type": kind_name(o), "box": bx(o.rect)});
    match &o.kind {
        Kind::Text { text, size, color, bold } => {
            v["text"] = json!(text);
            v["size"] = json!(r1(*size));
            v["color"] = json!(hex(*color));
            v["bold"] = json!(bold);
        }
        Kind::Image(img) => v["pixels"] = json!(img.px),
        Kind::Ink { strokes, color, width, .. } => {
            v["color"] = json!(hex(*color));
            v["width"] = json!(r1(*width));
            v["strokes"] = json!(strokes.len());
        }
        Kind::Shape { shape, stroke, fill, width, a, b } => {
            v["stroke_color"] = json!(hex(*stroke));
            v["fill_color"] = json!(fill.map(hex));
            v["width"] = json!(r1(*width));
            if matches!(shape, ShapeKind::Line | ShapeKind::Arrow) {
                let (pa, pb) = (o.at(*a), o.at(*b));
                v["from"] = json!([r1(pa.x), r1(pa.y)]);
                v["to"] = json!([r1(pb.x), r1(pb.y)]);
            }
        }
        Kind::Path { nodes, closed, stroke, fill, width } => {
            v["anchors"] = json!(nodes.len());
            v["closed"] = json!(closed);
            v["stroke_color"] = json!(hex(*stroke));
            v["fill_color"] = json!(fill.map(hex));
            v["width"] = json!(r1(*width));
        }
        Kind::Note { text, author, date, color } => {
            v["text"] = json!(text);
            v["author"] = json!(author);
            v["date"] = json!(date);
            v["color"] = json!(hex(*color));
        }
    }
    v
}

impl App {
    /// Run the tool calls agents sent since the last frame (the first window holds the server
    /// and passes each call on to the document it names).
    pub(super) fn agent_poll(&mut self, ctx: &egui::Context) {
        if self.chat_rx.is_none() && self.agent.is_none() {
            return;
        }
        // The document whose Assistant started a message becomes the agents' default, so
        // Claude Code and Codex (which call over MCP) work on it.
        if std::mem::take(&mut self.want_agent_doc) {
            self.agent_doc = Some(self.doc_id);
        }
        for w in &mut self.others {
            if std::mem::take(&mut w.want_agent_doc) {
                self.agent_doc = Some(w.doc_id);
            }
        }
        let mut calls: Vec<Call> = self.chat_rx.as_ref().map(|r| r.try_iter().collect()).unwrap_or_default();
        if let Some(server) = &self.agent {
            calls.extend(server.rx.try_iter());
        }
        for c in calls {
            let result = self.agent_route(ctx, &c.name, &c.args);
            self.agent_note = Some((c.name.clone(), ctx.input(|i| i.time)));
            let _ = c.reply.send(result);
            ctx.request_repaint();
        }
    }

    /// What the Assistant is told about this window's document with each message.
    fn chat_context(&mut self) -> String {
        let mut s = format!(
            "[PDFflow document {}: {}{}, {} page{}, page {} on screen, {}",
            self.doc_id,
            self.doc.title(),
            self.doc.path.as_ref().map(|p| format!(" ({})", p.display())).unwrap_or_default(),
            self.doc.pages.len(),
            if self.doc.pages.len() == 1 { "" } else { "s" },
            self.current + 1,
            if self.doc.dirty { "unsaved changes" } else { "no unsaved changes" }
        );
        if let Some((p, i)) = self.sel.filter(|(p, i)| self.doc.pages.get(*p).is_some_and(|pg| *i < pg.objects.len())) {
            s.push_str(&format!("; selected: object {i} on page {} ({})", p + 1, kind_name(&self.doc.pages[p].objects[i])));
        }
        let page = self.text_sel.as_ref().map(|t| t.page + 1);
        if let (Some(text), Some(page)) = (self.selected_text().filter(|t| !t.trim().is_empty()), page) {
            s.push_str(&format!("; selected text on page {page}: \"{}\"", th_hive::clip(text.trim(), 600)));
        }
        s.push_str(". Tools work on this document unless you pass another 'document'.]");
        s
    }

    /// Runs the Assistant's tool calls in the editor, through the first window like MCP calls;
    /// a call that names no document works on this window's.
    fn chat_editor(&self, ctx: &egui::Context) -> crate::assistant::engine::Editor {
        let (tx, ctx, id) = (self.chat_tx.clone(), ctx.clone(), self.doc_id);
        Arc::new(move |name: &str, mut args: Value| {
            let new_window = args.get("new_window").and_then(Value::as_bool).unwrap_or(false);
            if name != "list_documents" && !new_window && args.get("document").is_none_or(Value::is_null) {
                args["document"] = json!(id);
            }
            let (reply, answer) = std::sync::mpsc::channel();
            tx.send(Call { name: name.to_owned(), args, reply }).map_err(|_| "PDFflow is closing.".to_owned())?;
            ctx.request_repaint();
            answer.recv_timeout(std::time::Duration::from_secs(120)).map_err(|_| "PDFflow did not answer in time (is a dialog open in the window?).".to_owned())?
        })
    }

    /// Tests: show the Assistant with settings in `dir` and a scripted model.
    #[cfg(test)]
    pub(crate) fn test_assistant(&mut self, dir: &std::path::Path, adapter: Arc<dyn th_hive::LlmAdapter>) -> &mut crate::assistant::chat::ChatView {
        let mut c = crate::assistant::chat::ChatView::with_store(crate::assistant::settings::Store::new(dir));
        c.adapter = Some(adapter);
        c.set_settings_open(false);
        self.chat_open = true;
        self.chat.insert(Box::new(c))
    }

    #[cfg(test)]
    pub(crate) fn chat_view(&self) -> Option<&crate::assistant::chat::ChatView> {
        self.chat.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn chat_area(&self) -> Option<Rect> {
        self.chat_rect
    }

    #[cfg(test)]
    pub(crate) fn chat_entries(&self) -> Vec<crate::assistant::transcript::Entry> {
        self.chat.as_ref().map(|c| c.entries().to_vec()).unwrap_or_default()
    }

    /// The Assistant panel.
    pub(super) fn assistant_ui(&mut self, ui: &mut Ui) {
        let host = crate::assistant::chat::Host { document: self.chat_context(), editor: self.chat_editor(ui.ctx()), mcp_addr: self.mcp_addr.clone() };
        let area = ui.max_rect();
        self.chat_rect = Some(area);
        let chat = self.chat.get_or_insert_with(|| Box::new(crate::assistant::chat::ChatView::new()));
        let fx = chat.ui(ui, &host);
        // Files dragged over the panel: show that a drop attaches them.
        let (hovering, pointer) = ui.input(|i| (!i.raw.hovered_files.is_empty(), i.pointer.latest_pos()));
        if hovering && pointer.is_some_and(|p| area.contains(p)) {
            let t = Tokens::get(ui.ctx());
            let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, self.wid("chat-drop")));
            painter.rect_filled(area, 6.0, t.accent.gamma_multiply(0.12));
            painter.rect_stroke(area.shrink(2.0), 6.0, Stroke::new(2.0, t.accent), StrokeKind::Inside);
            painter.text(area.center(), egui::Align2::CENTER_CENTER, "Drop to attach to the chat", egui::FontId::proportional(15.0), t.text);
        }
        if fx.started {
            self.end_edit();
            self.chat_mark = Some((self.doc.mark(), self.doc.pages.clone(), self.doc_epoch));
            self.want_agent_doc = true;
        }
        if fx.finished {
            // The message's edits become one undo step (unless another document was opened).
            if let Some((mark, before, epoch)) = self.chat_mark.take() {
                if epoch == self.doc_epoch {
                    self.doc.squash(mark, before);
                }
            }
        }
    }

    /// The window holding document `id`.
    fn window_of(&mut self, id: u64) -> Option<&mut App> {
        if self.doc_id == id { Some(self) } else { self.others.iter_mut().find(|w| w.doc_id == id) }
    }

    fn documents(&self) -> Value {
        let default = self.agent_target_id();
        let docs: Vec<Value> = std::iter::once(&*self)
            .chain(self.others.iter())
            .map(|w| {
                json!({
                    "document": w.doc_id,
                    "file": w.doc.path.as_ref().map(|p| p.display().to_string()),
                    "title": w.doc.title(),
                    "page_count": w.doc.pages.len(),
                    "unsaved_changes": w.doc.dirty,
                    "agent_default": w.doc_id == default,
                })
            })
            .collect();
        json!({"documents": docs})
    }

    /// The document a call without `document` works on: the one used last, while it is open.
    fn agent_target_id(&self) -> u64 {
        self.agent_doc.filter(|id| *id == self.doc_id || self.others.iter().any(|w| w.doc_id == *id)).unwrap_or(self.doc_id)
    }

    /// Run one tool call for an agent: on the document it names (or the default one), in a new
    /// window for `new_window`, with errors returned instead of shown in the window.
    pub(crate) fn agent_route(&mut self, ctx: &egui::Context, name: &str, args: &Value) -> R {
        let a = Args(args);
        if name == "list_documents" {
            return Ok(self.documents());
        }
        if matches!(name, "open_pdf" | "new_document") && a.bool("new_window") {
            if a.get("document").is_some() {
                return Err("Give either 'document' or new_window, not both.".into());
            }
            let next = self.others.iter().map(|w| w.win).max().unwrap_or(self.win).max(self.win) + 1;
            let mut w = self.child(ctx, next, None);
            let mut args = args.clone();
            if let Some(o) = args.as_object_mut() {
                o.remove("new_window");
            }
            let v = w.agent_run(ctx, name, &args)?;
            self.agent_doc = Some(w.doc_id);
            self.others.push(w);
            return Ok(v);
        }
        let id = match a.get("document") {
            Some(v) => v.as_u64().ok_or("'document' must be a document id from list_documents.")?,
            None => self.agent_target_id(),
        };
        let count = 1 + self.others.len();
        let w = self.window_of(id).ok_or_else(|| format!("There is no open document {id}; list_documents shows the {count} open."))?;
        if name == "close_document" {
            if count == 1 {
                return Err("This is the only window, so it cannot be closed; open_pdf or new_document replace its document.".into());
            }
            if w.doc.dirty && !a.bool("discard_changes") {
                return Err("The document has unsaved changes. Save it first (save_pdf) or pass discard_changes: true.".into());
            }
            // The window closes in the next frame, as if its close button were pressed.
            w.allow_close = true;
            let title = w.doc.title();
            if self.agent_doc == Some(id) {
                self.agent_doc = None;
            }
            return Ok(json!({"closed": id, "title": title}));
        }
        let r = w.agent_run(ctx, name, args);
        self.agent_doc = Some(id);
        r
    }

    /// Run a tool on this window's document; an error the editor would show in a dialog is
    /// returned to the agent instead.
    fn agent_run(&mut self, ctx: &egui::Context, name: &str, args: &Value) -> R {
        let shown = self.error.take();
        let mut result = self.agent_call(ctx, name, args);
        if let Some(e) = self.error.take() {
            result = Err(e);
        }
        self.error = shown;
        result
    }

    /// The agent indicator in the status bar: lit while tools are being called.
    pub(super) fn agent_badge(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let Some(server) = &self.agent else { return };
        let now = ui.input(|i| i.time);
        let recent = self.agent_note.as_ref().filter(|(_, at)| now - at < 2.5);
        let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
        icons::paint(ui.painter(), r.shrink(1.0), "bot", if recent.is_some() { t.accent } else { t.text_faint });
        let tip = format!(
            "AI agents can drive PDFflow over MCP:\n  http://{}/mcp\nor run `pdfflow --mcp` as a stdio server.\nClick to copy the command that connects Claude Code.{}",
            server.addr,
            self.agent_note.as_ref().map(|(n, _)| format!("\nLast tool: {n}")).unwrap_or_default()
        );
        if resp.clicked() {
            ui.ctx().copy_text(crate::mcp::claude_code_command());
            self.status = "Copied the Claude Code command; paste it in a terminal to connect PDFflow.".into();
        }
        resp.on_hover_text(tip);
        if recent.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    fn page_arg(&self, a: &Args, key: &str) -> Result<usize, String> {
        let n = a.opt_int(key).ok_or_else(|| format!("'{key}' (page number from 1) is required."))?;
        let count = self.doc.pages.len() as i64;
        if n < 1 || n > count {
            return Err(format!("Page {n} does not exist; the document has {count} page{}.", if count == 1 { "" } else { "s" }));
        }
        Ok(n as usize - 1)
    }

    fn obj_arg(&self, a: &Args) -> Result<(usize, usize), String> {
        let p = self.page_arg(a, "page")?;
        let i = a.opt_int("index").ok_or("'index' is required.")?;
        let n = self.doc.pages[p].objects.len() as i64;
        if i < 0 || i >= n {
            return Err(format!("Page {} has {n} object{}; there is no object {i}.", p + 1, if n == 1 { "" } else { "s" }));
        }
        Ok((p, i as usize))
    }

    /// Show the page an agent works on.
    fn focus(&mut self, page: usize) {
        if page != self.current || self.grid {
            self.grid = false;
            self.current = page;
            self.scroll_to_page(page);
            self.scroll_thumbs_to = Some(page);
        }
    }

    /// Add an object (one undo step) and return its index.
    fn push_obj(&mut self, page: usize, o: Obj) -> Value {
        self.end_edit();
        self.doc.checkpoint();
        self.doc.pages[page].objects.push(o);
        let i = self.doc.pages[page].objects.len() - 1;
        self.focus(page);
        json!({"page": page + 1, "index": i, "object": describe(i, &self.doc.pages[page].objects[i])})
    }

    fn info(&self) -> Value {
        let pages: Vec<Value> = self
            .doc
            .pages
            .iter()
            .enumerate()
            .take(300)
            .map(|(i, p)| {
                let vs = p.view_size();
                json!({"page": i + 1, "width": r1(vs.x), "height": r1(vs.y), "rotation": p.rotation, "blank": p.src == PageSrc::Blank, "objects": p.objects.len()})
            })
            .collect();
        json!({
            "document": self.doc_id,
            "file": self.doc.path.as_ref().map(|p| p.display().to_string()),
            "title": self.doc.title(),
            "page_count": self.doc.pages.len(),
            "current_page": self.current + 1,
            "unsaved_changes": self.doc.dirty,
            "can_undo": self.doc.can_undo(),
            "can_redo": self.doc.can_redo(),
            "digital_signatures": self.sig_reports.len(),
            "tool": self.tool.name(),
            "clipboard": self.clipboard.as_ref().map(kind_name),
            "pages": pages,
        })
    }

    fn texts(&mut self, pages: impl Iterator<Item = usize>) -> Vec<(usize, Arc<PageText>)> {
        pages.filter_map(|p| self.page_text(p).map(|t| (p, t))).collect()
    }

    pub(crate) fn agent_call(&mut self, ctx: &egui::Context, name: &str, args: &Value) -> R {
        let a = Args(args);
        match name {
            // --------------------------------------------------------------------- document
            "open_pdf" => {
                if self.doc.dirty && !a.bool("discard_changes") {
                    return Err("The open document has unsaved changes. Save it first (save_pdf) or pass discard_changes: true.".into());
                }
                let path = resolve(a.str("path")?, &self.doc);
                if !path.is_file() {
                    return Err(format!("There is no file at {}.", path.display()));
                }
                self.open_path(path);
                Ok(self.info())
            }
            "new_document" => {
                if self.doc.dirty && !a.bool("discard_changes") {
                    return Err("The open document has unsaved changes. Save it first or pass discard_changes: true.".into());
                }
                self.doc = Doc::new_blank();
                let size = vec2(a.opt_num("width").unwrap_or(A4.x), a.opt_num("height").unwrap_or(A4.y));
                if size.x < 10.0 || size.y < 10.0 || size.x > 14400.0 || size.y > 14400.0 {
                    return Err("Page sizes must be between 10 and 14400 points.".into());
                }
                self.doc.pages[0].size = size;
                self.reset_view();
                Ok(self.info())
            }
            "save_pdf" => {
                self.end_edit();
                self.close_note();
                if !self.sig_reports.is_empty() && !self.doc.is_pristine() && !a.bool("remove_signatures") {
                    return Err("This file is digitally signed and has been edited; saving removes the signatures. Pass remove_signatures: true to save anyway, or sign again afterwards.".into());
                }
                let mut path = match a.opt_str("path") {
                    Some(p) => resolve(p, &self.doc),
                    None => self.doc.path.clone().ok_or("The document has no file yet; give a path.")?,
                };
                if path.extension().is_none() {
                    path.set_extension("pdf");
                }
                let n = self.write_pdf(path.clone())?;
                self.status = format!("Saved {} ({:.1} MB)", self.doc.title(), n as f32 / 1.0e6);
                Ok(json!({"saved": path.display().to_string(), "bytes": n}))
            }
            "export_pages" => {
                let list = a.get("pages").and_then(Value::as_array).ok_or("'pages' (a list of page numbers) is required.")?;
                let mut idx = Vec::new();
                for v in list {
                    idx.push(self.page_arg(&Args(&json!({"p": v})), "p")?);
                }
                if idx.is_empty() {
                    return Err("Give at least one page.".into());
                }
                let mut path = resolve(a.str("path")?, &self.doc);
                if path.extension().is_none() {
                    path.set_extension("pdf");
                }
                self.end_edit();
                let bytes = export::save(&self.doc.subset(&idx))?;
                std::fs::write(&path, &bytes).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
                Ok(json!({"saved": path.display().to_string(), "pages": idx.len(), "bytes": bytes.len()}))
            }
            "document_info" => Ok(self.info()),
            "undo" | "redo" => {
                self.end_edit();
                self.close_note();
                self.pen = None;
                let done = if name == "undo" { self.doc.undo() } else { self.doc.redo() };
                if !done {
                    return Err(format!("Nothing to {name}."));
                }
                self.sel = None;
                self.text_sel = None;
                self.current = self.current.min(self.doc.pages.len() - 1);
                Ok(json!({"done": name, "can_undo": self.doc.can_undo(), "can_redo": self.doc.can_redo(), "page_count": self.doc.pages.len()}))
            }
            // ------------------------------------------------------------------------- view
            "go_to_page" => {
                let p = self.page_arg(&a, "page")?;
                self.grid = false;
                self.current = p;
                self.scroll_to_page(p);
                self.scroll_thumbs_to = Some(p);
                Ok(json!({"current_page": p + 1}))
            }
            "set_view" => {
                if let Some(g) = a.opt_bool("grid") {
                    if g != self.grid {
                        self.toggle_grid();
                    }
                }
                match a.opt_str("fit") {
                    Some("page") => self.fit_page(),
                    Some("width") => self.fit_width(),
                    Some(o) => return Err(format!("fit must be 'page' or 'width', not '{o}'.")),
                    None => {}
                }
                if let Some(z) = a.opt_num("zoom_percent") {
                    self.zoom_center((z / 100.0).clamp(0.1, 8.0));
                }
                match a.opt_str("theme") {
                    Some("dark") => self.theme = ThemeKind::Pro,
                    Some("medium") => self.theme = ThemeKind::ProMedium,
                    Some(o) => return Err(format!("theme must be 'medium' or 'dark', not '{o}'.")),
                    None => {}
                }
                theme::apply(ctx, self.theme);
                Ok(json!({"zoom_percent": (self.zoom * 100.0).round(), "grid": self.grid}))
            }
            "set_tool" => {
                let n = a.str("tool")?;
                let tool = tool_of(n).ok_or_else(|| format!("Unknown tool '{n}'."))?;
                self.set_tool(tool);
                if self.tool != tool {
                    return Err(std::mem::take(&mut self.status));
                }
                Ok(json!({"tool": tool.name()}))
            }
            "render_page" => {
                let p = self.page_arg(&a, "page")?;
                let dpi = a.opt_num("dpi").unwrap_or(96.0).clamp(10.0, 1200.0);
                let rect = a.opt_rect("rect")?.unwrap_or(Rect::from_min_size(Pos2::ZERO, self.doc.pages[p].size));
                self.end_edit();
                let img = self.snapshot_image(p, rect, dpi)?;
                let mut out = json!({"page": p + 1, "dpi": dpi, "width": img.width(), "height": img.height()});
                if let Some(path) = a.opt_str("save_path") {
                    let path = resolve(path, &self.doc);
                    let b = crate::snapshot::encode(&img, crate::snapshot::format_for(&path))?;
                    std::fs::write(&path, b).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
                    out["saved"] = json!(path.display().to_string());
                }
                let (png, w, h) = reply_png(&img)?;
                if (w, h) != (img.width(), img.height()) {
                    out["reply_picture"] = json!(format!("scaled to {w} × {h} px"));
                }
                out["image_png"] = json!(png);
                Ok(out)
            }
            // ------------------------------------------------------------------------- text
            "get_text" => {
                let n = self.doc.pages.len();
                let range: Vec<usize> = if a.get("page").is_some() {
                    vec![self.page_arg(&a, "page")?]
                } else {
                    let first = if a.get("first_page").is_some() { self.page_arg(&a, "first_page")? } else { 0 };
                    let last = if a.get("last_page").is_some() { self.page_arg(&a, "last_page")? } else { n - 1 };
                    (first..=last).collect()
                };
                let pages: Vec<Value> = range
                    .into_iter()
                    .map(|p| {
                        let text = self.page_text(p).map(|t| t.all_text()).unwrap_or_default();
                        json!({"page": p + 1, "text": text})
                    })
                    .collect();
                let empty = pages.iter().all(|p| p["text"].as_str().is_some_and(str::is_empty));
                let mut out = json!({"pages": pages});
                if empty {
                    out["note"] = json!("No text layer on these pages (blank or scanned). render_page shows what is on them.");
                }
                Ok(out)
            }
            "get_text_lines" => {
                let p = self.page_arg(&a, "page")?;
                let lines: Vec<Value> = self.page_text(p).map(|t| t.lines().into_iter().map(|l| json!({"text": l.text, "box": bx(l.rect)})).collect()).unwrap_or_default();
                Ok(json!({"page": p + 1, "lines": lines}))
            }
            "find_text" => {
                let q = a.str("query")?;
                let cs = a.bool("case_sensitive");
                let pages: Vec<usize> = if a.get("page").is_some() { vec![self.page_arg(&a, "page")?] } else { (0..self.doc.pages.len()).collect() };
                let mut matches = Vec::new();
                let mut total = 0;
                for (p, t) in self.texts(pages.into_iter()) {
                    for m in t.find(q, cs) {
                        total += 1;
                        if matches.len() < 300 {
                            matches.push(json!({"page": p + 1, "text": t.text_of(&m), "boxes": t.runs(&m).iter().map(|r| bx(r.0)).collect::<Vec<_>>()}));
                        }
                    }
                }
                Ok(json!({"query": q, "count": total, "matches": matches}))
            }
            "select_text" => {
                let p = self.page_arg(&a, "page")?;
                let t = self.page_text(p).filter(|t| !t.is_empty()).ok_or("This page has no text layer to select.")?;
                let glyphs = if let Some(q) = a.opt_str("query") {
                    let k = a.opt_int("occurrence").unwrap_or(1).max(1) as usize;
                    let hits = t.find(q, false);
                    hits.get(k - 1).cloned().ok_or_else(|| format!("'{q}' occurs {} time(s) on page {}.", hits.len(), p + 1))?
                } else if let Some(r) = a.opt_rect("rect")? {
                    t.in_rect(r)
                } else {
                    (0..t.glyphs.len()).collect()
                };
                if glyphs.is_empty() {
                    return Err("Nothing to select there.".into());
                }
                self.set_tool(Tool::SelectText);
                self.focus(p);
                self.text_sel = Some(TextSel { page: p, glyphs: glyphs.clone() });
                let text = t.text_of(&glyphs);
                if a.bool("copy") {
                    ctx.copy_text(text.clone());
                }
                Ok(json!({"page": p + 1, "text": text, "boxes": t.runs(&glyphs).iter().map(|r| bx(r.0)).collect::<Vec<_>>()}))
            }
            "copy_text" => {
                let text = match a.opt_str("text") {
                    Some(t) => t.to_owned(),
                    None => self.selected_text().ok_or("No text is selected; give 'text' or select some first.")?,
                };
                ctx.copy_text(text.clone());
                Ok(json!({"copied": text}))
            }
            "markup_text" => {
                let style = match a.str("style")? {
                    "highlight" => Markup::Highlight,
                    "underline" => Markup::Underline,
                    "strikeout" | "strikethrough" => Markup::Strikeout,
                    o => return Err(format!("style must be highlight, underline or strikeout, not '{o}'.")),
                };
                let color = a.color("color")?.unwrap_or(if style == Markup::Highlight { self.opts.hl_color } else { self.opts.color });
                // Targets: (page, glyphs) groups.
                let mut targets: Vec<(usize, Vec<usize>)> = Vec::new();
                if let Some(q) = a.opt_str("query") {
                    let pages: Vec<usize> = if a.get("page").is_some() { vec![self.page_arg(&a, "page")?] } else { (0..self.doc.pages.len()).collect() };
                    for (p, t) in self.texts(pages.into_iter()) {
                        targets.extend(t.find(q, false).into_iter().map(|m| (p, m)));
                    }
                    if let Some(k) = a.opt_int("occurrence") {
                        let k = k.max(1) as usize;
                        let n = targets.len();
                        targets = targets.into_iter().nth(k - 1).into_iter().collect();
                        if targets.is_empty() {
                            return Err(format!("'{q}' occurs {n} time(s); there is no occurrence {k}."));
                        }
                    }
                    if targets.is_empty() {
                        return Err(format!("'{q}' was not found."));
                    }
                } else if let Some(r) = a.opt_rect("rect")? {
                    let p = self.page_arg(&a, "page").map_err(|_| "Give 'page' with 'rect'.")?;
                    let t = self.page_text(p).ok_or("This page has no text layer.")?;
                    targets.push((p, t.in_rect(r)));
                } else {
                    let s = self.text_sel.clone().ok_or("Give 'query' or 'rect', or select text first.")?;
                    targets.push((s.page, s.glyphs));
                }
                self.end_edit();
                let mut added: Vec<(usize, Obj)> = Vec::new();
                for (p, g) in &targets {
                    if let Some(t) = self.page_text(*p) {
                        added.extend(markup_objects(&t, g, style, color).into_iter().map(|o| (*p, o)));
                    }
                }
                if added.is_empty() {
                    return Err("There is no text there to mark.".into());
                }
                // A review comment at the end of each marked passage.
                let mut notes = 0;
                if let Some(text) = a.opt_str("comment").filter(|t| !t.trim().is_empty()) {
                    let author = a.opt_str("author").map(str::to_owned).unwrap_or_else(|| self.author.clone());
                    for (p, g) in &targets {
                        let Some(t) = self.page_text(*p) else { continue };
                        let Some((r, ..)) = t.runs(g).last().copied() else { continue };
                        let size = self.doc.pages[*p].size;
                        let min = pos2((r.max.x + 2.0).clamp(0.0, size.x - NOTE_SIZE), (r.min.y - 4.0).clamp(0.0, size.y - NOTE_SIZE));
                        let o = Obj { rect: Rect::from_min_size(min, vec2(NOTE_SIZE, NOTE_SIZE)), kind: Kind::Note { text: text.to_owned(), author: author.clone(), date: notes::now_pdf_date(), color: self.note_color } };
                        added.push((*p, o));
                        notes += 1;
                    }
                }
                self.close_note();
                self.doc.checkpoint();
                let mut pages = Vec::new();
                for (p, o) in added.iter().cloned() {
                    self.doc.pages[p].objects.push(o);
                    if !pages.contains(&(p + 1)) {
                        pages.push(p + 1);
                    }
                }
                self.focus(added[0].0);
                Ok(json!({"marked": targets.len(), "objects_added": added.len() - notes, "comments_added": notes, "pages": pages}))
            }
            // ------------------------------------------------------------ snapshot, clipboard
            "snapshot" => {
                let p = self.page_arg(&a, "page")?;
                let dpi = a.opt_num("dpi").unwrap_or(150.0).clamp(10.0, 1200.0);
                let rect = a.opt_rect("rect")?.unwrap_or(Rect::from_min_size(Pos2::ZERO, self.doc.pages[p].size));
                self.end_edit();
                let img = Arc::new(self.snapshot_image(p, rect, dpi)?);
                ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()));
                self.clip_picture(ctx, &img, rect);
                self.snap = Some(Snap { img: img.clone(), page: p, rect, dpi, tex: None, open: false });
                self.status = format!("Snapshot of page {} copied ({} × {} px).", p + 1, img.width(), img.height());
                let mut out = json!({"page": p + 1, "width": img.width(), "height": img.height(), "dpi": dpi, "copied_to": "system clipboard and PDFflow clipboard"});
                if let Some(path) = a.opt_str("save_path") {
                    let path = resolve(path, &self.doc);
                    let b = crate::snapshot::encode(&img, crate::snapshot::format_for(&path))?;
                    std::fs::write(&path, b).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
                    out["saved"] = json!(path.display().to_string());
                }
                if a.opt_bool("return_image").unwrap_or(true) {
                    out["image_png"] = json!(reply_png(&img)?.0);
                }
                Ok(out)
            }
            "paste" => {
                let mut o = self.clipboard.clone().ok_or("PDFflow's clipboard is empty; take a snapshot or copy an object first.")?;
                let p = if a.get("page").is_some() { self.page_arg(&a, "page")? } else { self.current };
                match (a.opt_num("x"), a.opt_num("y")) {
                    (Some(x), Some(y)) => o.rect = Rect::from_center_size(pos2(x, y), o.rect.size()),
                    _ => o.rect = o.rect.translate(vec2(12.0, 12.0)),
                }
                let v = self.push_obj(p, o);
                self.sel = Some((p, self.doc.pages[p].objects.len() - 1));
                Ok(v)
            }
            "copy_object" => {
                let (p, i) = self.obj_arg(&a)?;
                self.clipboard = Some(self.doc.pages[p].objects[i].clone());
                Ok(json!({"copied": describe(i, &self.doc.pages[p].objects[i])}))
            }
            // ------------------------------------------------------------------------ pages
            "insert_blank_page" => {
                let at = match a.opt_int("after") {
                    Some(n) if n < 0 || n > self.doc.pages.len() as i64 => return Err(format!("'after' must be 0 to {}.", self.doc.pages.len())),
                    Some(n) => n as usize,
                    None => self.current + 1,
                };
                let base = self.doc.pages.get(self.current).map(|p| p.view_size()).unwrap_or(A4);
                let size = vec2(a.opt_num("width").unwrap_or(base.x), a.opt_num("height").unwrap_or(base.y));
                if size.x < 10.0 || size.y < 10.0 || size.x > 14400.0 || size.y > 14400.0 {
                    return Err("Page sizes must be between 10 and 14400 points.".into());
                }
                self.page_op(|pages, cur| {
                    pages.insert(at, PageEntry::blank(size));
                    *cur = at;
                });
                self.scroll_to_page(at);
                Ok(json!({"inserted_page": at + 1, "page_count": self.doc.pages.len()}))
            }
            "duplicate_page" => {
                let p = self.page_arg(&a, "page")?;
                self.duplicate_page(p);
                Ok(json!({"new_page": p + 2, "page_count": self.doc.pages.len()}))
            }
            "rotate_page" => {
                let p = self.page_arg(&a, "page")?;
                let d = a.opt_int("degrees").ok_or("'degrees' is required.")?.rem_euclid(360);
                if d % 90 != 0 {
                    return Err("degrees must be a multiple of 90.".into());
                }
                if d != 0 {
                    self.rotate_page(p, d as u16);
                }
                Ok(json!({"page": p + 1, "rotation": self.doc.pages[p].rotation}))
            }
            "move_page" => {
                let p = self.page_arg(&a, "page")?;
                let to = self.page_arg(&a, "to")?;
                if to != p {
                    self.move_page_to(p, if to > p { to + 1 } else { to });
                }
                self.scroll_to_page(to);
                Ok(json!({"page": to + 1}))
            }
            "delete_page" => {
                let p = self.page_arg(&a, "page")?;
                if self.doc.pages.len() <= 1 {
                    return Err("A document needs at least one page.".into());
                }
                self.delete_page(p);
                Ok(json!({"deleted": p + 1, "page_count": self.doc.pages.len()}))
            }
            // ---------------------------------------------------------------------- objects
            "list_objects" => {
                let p = self.page_arg(&a, "page")?;
                let pg = &self.doc.pages[p];
                let objs: Vec<Value> = pg.objects.iter().enumerate().map(|(i, o)| describe(i, o)).collect();
                Ok(json!({"page": p + 1, "size": [r1(pg.size.x), r1(pg.size.y)], "objects": objs}))
            }
            "add_text" => {
                let p = self.page_arg(&a, "page")?;
                let text = a.str("text")?.to_owned();
                if text.trim().is_empty() {
                    return Err("The text is empty.".into());
                }
                let size = a.opt_num("size").unwrap_or(14.0).clamp(4.0, 300.0);
                let color = a.color("color")?.unwrap_or(Color32::BLACK);
                let mut o = Obj { rect: Rect::from_min_size(pos2(a.num("x")?, a.num("y")?), vec2(1.0, 1.0)), kind: Kind::Text { text, size, color, bold: a.bool("bold") } };
                o.refit_text();
                Ok(self.push_obj(p, o))
            }
            "add_shape" => {
                let p = self.page_arg(&a, "page")?;
                let stroke = a.color("stroke_color")?.unwrap_or(self.opts.color);
                let fill = a.color("fill_color")?;
                let width = a.opt_num("width").unwrap_or(2.0).clamp(0.0, 60.0);
                let o = match a.str("shape")? {
                    s @ ("rectangle" | "ellipse") => {
                        let r = a.rect("rect")?;
                        let shape = if s == "rectangle" { ShapeKind::Rect } else { ShapeKind::Ellipse };
                        Obj { rect: r, kind: Kind::Shape { shape, stroke, fill, width, a: pos2(0.0, 0.0), b: pos2(1.0, 1.0) } }
                    }
                    s @ ("line" | "arrow") => {
                        let shape = if s == "line" { ShapeKind::Line } else { ShapeKind::Arrow };
                        let mut o = Obj { rect: Rect::NOTHING, kind: Kind::Shape { shape, stroke, fill: None, width: width.max(0.1), a: Pos2::ZERO, b: Pos2::ZERO } };
                        set_line(&mut o, a.point("from")?, a.point("to")?);
                        o
                    }
                    s => return Err(format!("Unknown shape '{s}'.")),
                };
                Ok(self.push_obj(p, o))
            }
            "add_whiteout" => {
                let p = self.page_arg(&a, "page")?;
                let r = a.rect("rect")?;
                Ok(self.push_obj(p, Obj { rect: r, kind: Kind::Shape { shape: ShapeKind::Rect, stroke: Color32::WHITE, fill: Some(Color32::WHITE), width: 0.0, a: pos2(0.0, 0.0), b: pos2(1.0, 1.0) } }))
            }
            "add_ink" => {
                let p = self.page_arg(&a, "page")?;
                let strokes: Vec<Vec<Pos2>> = a.get("strokes").and_then(Value::as_array).ok_or("'strokes' is required.")?.iter().map(Args::points_of).collect::<Option<_>>().ok_or("Each stroke must be a list of [x, y] points.")?;
                if strokes.iter().all(Vec::is_empty) {
                    return Err("The strokes have no points.".into());
                }
                let hl = a.bool("highlighter");
                let color = a.color("color")?.unwrap_or(if hl { self.opts.hl_color } else { self.opts.color });
                let width = a.opt_num("width").unwrap_or(if hl { 12.0 } else { 2.0 }).clamp(0.1, 60.0);
                let (rect, unit) = ink_from_points(&strokes, width / 2.0);
                Ok(self.push_obj(p, Obj { rect, kind: Kind::Ink { strokes: Arc::new(unit), color, width, highlighter: hl } }))
            }
            "add_path" => {
                let p = self.page_arg(&a, "page")?;
                let pts = a.get("points").and_then(Args::points_of).ok_or("'points' must be a list of [x, y].")?;
                if pts.len() < 2 {
                    return Err("A path needs at least two points.".into());
                }
                let closed = a.bool("closed");
                let mut nodes: Vec<crate::path::Node> = pts.iter().map(|q| crate::path::Node::corner(*q)).collect();
                if a.bool("smooth") {
                    crate::path::auto_smooth(&mut nodes, &vec![false; pts.len()], closed);
                }
                let stroke = a.color("stroke_color")?.unwrap_or(self.opts.color);
                let fill = a.color("fill_color")?;
                let width = a.opt_num("width").unwrap_or(2.0).clamp(0.0, 60.0);
                let mut o = Obj { rect: Rect::NOTHING, kind: Kind::Path { nodes: Arc::new(Vec::new()), closed, stroke, fill, width } };
                o.set_path_abs(&nodes, closed);
                Ok(self.push_obj(p, o))
            }
            "add_image" => {
                let p = self.page_arg(&a, "page")?;
                let path = resolve(a.str("path")?, &self.doc);
                let bytes = std::fs::read(&path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
                self.end_edit();
                let n = self.doc.pages[p].objects.len();
                self.insert_image(ctx, bytes, p, None);
                if self.doc.pages[p].objects.len() == n {
                    return Err(self.error.take().unwrap_or_else(|| "The picture could not be placed.".into()));
                }
                let i = n;
                let r0 = self.doc.pages[p].objects[i].rect;
                let rect = match (a.opt_rect("rect")?, a.opt_num("x"), a.opt_num("y")) {
                    (Some(r), ..) => Some(r),
                    (None, Some(x), Some(y)) => {
                        let w = a.opt_num("width").unwrap_or(r0.width());
                        Some(Rect::from_min_size(pos2(x, y), vec2(w, w * r0.height() / r0.width().max(1e-3))))
                    }
                    _ => a.opt_num("width").map(|w| Rect::from_center_size(r0.center(), vec2(w, w * r0.height() / r0.width().max(1e-3)))),
                };
                if let Some(r) = rect {
                    self.doc.pages[p].objects[i].rect = r;
                }
                self.focus(p);
                Ok(json!({"page": p + 1, "index": i, "object": describe(i, &self.doc.pages[p].objects[i])}))
            }
            "add_comment" => {
                let p = self.page_arg(&a, "page")?;
                let text = a.str("text")?.to_owned();
                let size = self.doc.pages[p].size;
                let min = pos2(a.num("x")?.clamp(0.0, size.x - NOTE_SIZE), a.num("y")?.clamp(0.0, size.y - NOTE_SIZE));
                let author = a.opt_str("author").map(str::to_owned).unwrap_or_else(|| self.author.clone());
                let color = a.color("color")?.unwrap_or(self.note_color);
                self.close_note();
                Ok(self.push_obj(p, Obj { rect: Rect::from_min_size(min, vec2(NOTE_SIZE, NOTE_SIZE)), kind: Kind::Note { text, author, date: notes::now_pdf_date(), color } }))
            }
            "place_signature" => {
                let p = self.page_arg(&a, "page")?;
                if self.sigs.is_empty() {
                    return Err("The user has no saved signatures yet; they draw one in the Signatures panel.".into());
                }
                let k = a.opt_int("signature").map(|k| k as usize).unwrap_or(self.sig_sel);
                let s = self.sigs.get(k).cloned().ok_or_else(|| format!("There are {} saved signatures (0 to {}).", self.sigs.len(), self.sigs.len() - 1))?;
                let w = a.opt_num("width").unwrap_or(150.0).clamp(10.0, 2000.0);
                let rect = Rect::from_min_size(pos2(a.num("x")?, a.num("y")?), vec2(w, w / s.aspect.max(0.05)));
                let color = a.color("color")?.unwrap_or(self.opts.sig_color);
                Ok(self.push_obj(p, Obj { rect, kind: Kind::Ink { strokes: s.strokes, color, width: 1.6, highlighter: false } }))
            }
            "update_object" => {
                let (p, i) = self.obj_arg(&a)?;
                let mut o = self.doc.pages[p].objects[i].clone();
                if let Some(r) = a.opt_rect("rect")? {
                    // Text sizes itself, so only its position follows the box.
                    o.rect = if matches!(o.kind, Kind::Text { .. }) { Rect::from_min_size(r.min, o.rect.size()) } else { r };
                }
                if a.opt_num("x").is_some() || a.opt_num("y").is_some() {
                    let to = pos2(a.opt_num("x").unwrap_or(o.rect.min.x), a.opt_num("y").unwrap_or(o.rect.min.y));
                    o.rect = o.rect.translate(to - o.rect.min);
                }
                let text = a.opt_str("text").map(str::to_owned);
                let color = a.color("color")?;
                let stroke_c = a.color("stroke_color")?;
                let fill_c = match a.opt_str("fill_color") {
                    Some("none") => Some(None),
                    Some(c) => Some(Some(parse_color(c)?)),
                    None => None,
                };
                let w = a.opt_num("width");
                match &mut o.kind {
                    Kind::Text { text: t, size, color: c, bold } => {
                        if let Some(x) = text {
                            *t = x;
                        }
                        if let Some(s) = a.opt_num("size") {
                            *size = s.clamp(4.0, 300.0);
                        }
                        if let Some(b) = a.opt_bool("bold") {
                            *bold = b;
                        }
                        if let Some(x) = color {
                            *c = x;
                        }
                    }
                    Kind::Note { text: t, color: c, author, .. } => {
                        if let Some(x) = text {
                            *t = x;
                        }
                        if let Some(x) = color {
                            *c = x;
                        }
                        if let Some(x) = a.opt_str("author") {
                            *author = x.to_owned();
                        }
                    }
                    Kind::Ink { color: c, width, .. } => {
                        if let Some(x) = color.or(stroke_c) {
                            *c = x;
                        }
                        if let Some(x) = w {
                            *width = x.clamp(0.1, 60.0);
                        }
                    }
                    Kind::Shape { stroke, fill, width, .. } | Kind::Path { stroke, fill, width, .. } => {
                        if let Some(x) = stroke_c.or(color) {
                            *stroke = x;
                        }
                        if let Some(f) = fill_c {
                            *fill = f;
                        }
                        if let Some(x) = w {
                            *width = x.clamp(0.0, 60.0);
                        }
                    }
                    Kind::Image(_) => {}
                }
                o.refit_text();
                self.end_edit();
                self.doc.checkpoint();
                self.doc.pages[p].objects[i] = o;
                self.focus(p);
                Ok(json!({"page": p + 1, "object": describe(i, &self.doc.pages[p].objects[i])}))
            }
            "delete_object" => {
                let (p, i) = self.obj_arg(&a)?;
                self.end_edit();
                self.close_note();
                self.doc.checkpoint();
                let o = self.doc.pages[p].objects.remove(i);
                self.sel = None;
                self.rlive = None;
                Ok(json!({"deleted": describe(i, &o), "objects_left": self.doc.pages[p].objects.len()}))
            }
            "arrange_object" => {
                let (p, i) = self.obj_arg(&a)?;
                let n = self.doc.pages[p].objects.len();
                let j = match a.str("to")? {
                    "front" => n - 1,
                    "back" => 0,
                    "forward" => (i + 1).min(n - 1),
                    "backward" => i.saturating_sub(1),
                    o => return Err(format!("'to' must be front, back, forward or backward, not '{o}'.")),
                };
                if j != i {
                    self.end_edit();
                    self.doc.checkpoint();
                    let o = self.doc.pages[p].objects.remove(i);
                    self.doc.pages[p].objects.insert(j, o);
                    self.sel = None;
                    self.rlive = None;
                }
                Ok(json!({"page": p + 1, "index": j}))
            }
            "duplicate_object" => {
                let (p, i) = self.obj_arg(&a)?;
                let mut o = self.doc.pages[p].objects[i].clone();
                o.rect = o.rect.translate(vec2(a.opt_num("dx").unwrap_or(12.0), a.opt_num("dy").unwrap_or(12.0)));
                Ok(self.push_obj(p, o))
            }
            "select_object" => {
                let (p, i) = self.obj_arg(&a)?;
                self.end_edit();
                self.set_tool(Tool::Move);
                self.focus(p);
                self.sel = Some((p, i));
                Ok(json!({"selected": describe(i, &self.doc.pages[p].objects[i])}))
            }
            // --------------------------------------------------------------------- pictures
            "adjust_picture" => {
                let (p, i) = self.obj_arg(&a)?;
                if !matches!(self.doc.pages[p].objects[i].kind, Kind::Image(_)) {
                    return Err(format!("Object {i} on page {} is not a picture.", p + 1));
                }
                let adj = adjustment(&a)?;
                self.end_edit();
                self.focus(p);
                self.apply_adjustment(ctx, p, i, &adj)?;
                Ok(json!({"page": p + 1, "index": i, "applied": a.str("adjustment")?}))
            }
            "crop_picture" => {
                let (p, i) = self.obj_arg(&a)?;
                let r = a.rect("rect")?;
                let o = self.doc.pages[p].objects[i].clone();
                if !matches!(o.kind, Kind::Image(_)) {
                    return Err(format!("Object {i} on page {} is not a picture.", p + 1));
                }
                self.end_edit();
                if !self.ensure_live(ctx, p, i) {
                    return Err("The picture cannot be decoded for editing.".into());
                }
                let l = self.rlive.as_ref().expect("live");
                let s = vec2(l.raster.w as f32 / o.rect.width(), l.raster.h as f32 / o.rect.height());
                let q = Rect::from_min_max(((r.min - o.rect.min) * s).to_pos2(), ((r.max - o.rect.min) * s).to_pos2());
                self.crop_picture(q);
                let o = &self.doc.pages[p].objects[i];
                Ok(json!({"page": p + 1, "object": describe(i, o)}))
            }
            "export_picture" => {
                let (p, i) = self.obj_arg(&a)?;
                let Kind::Image(img) = &self.doc.pages[p].objects[i].kind else { return Err(format!("Object {i} on page {} is not a picture.", p + 1)) };
                let (w, h, data) = img.rgba().ok_or("The picture cannot be decoded.")?;
                let im = image::RgbaImage::from_raw(w, h, data).ok_or("picture size")?;
                let path = resolve(a.str("path")?, &self.doc);
                let b = crate::snapshot::encode(&im, crate::snapshot::format_for(&path))?;
                std::fs::write(&path, b).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
                Ok(json!({"saved": path.display().to_string(), "pixels": [w, h]}))
            }
            // ------------------------------------------------------------- comments, signing
            "list_comments" => {
                let mut out = Vec::new();
                for (p, pg) in self.doc.pages.iter().enumerate() {
                    for (i, o) in pg.objects.iter().enumerate() {
                        if matches!(o.kind, Kind::Note { .. }) {
                            let mut d = describe(i, o);
                            d["page"] = json!(p + 1);
                            out.push(d);
                        }
                    }
                }
                Ok(json!({"comments": out}))
            }
            "list_signatures" => {
                let sigs: Vec<Value> = self
                    .sig_reports
                    .iter()
                    .map(|r| {
                        json!({
                            "field": r.field, "signer": r.signer, "issuer": r.issuer, "signed": r.when, "reason": r.reason,
                            "valid": r.valid(), "content_unchanged": r.intact, "signature_matches": r.crypto_ok,
                            "covers_whole_file": r.whole_file, "self_signed": r.self_signed, "certificate_current": r.cert_current,
                            "trusted": r.trusted, "chain": r.chain, "certificate_expires": r.expires, "problem": r.problem,
                        })
                    })
                    .collect();
                Ok(json!({"signatures": sigs, "note": "Revocation (CRL/OCSP) is not checked."}))
            }
            "list_digital_ids" => {
                let ids: Vec<Value> = self.ids.iter().enumerate().map(|(i, d)| json!({"digital_id": i, "name": d.name, "detail": d.detail, "expires": d.expires})).collect();
                Ok(json!({"digital_ids": ids}))
            }
            "sign_pdf" => {
                let id = a.opt_int("digital_id").ok_or("'digital_id' is required.")?;
                if id < 0 || id as usize >= self.ids.len() {
                    return Err(format!("There are {} Digital IDs; use list_digital_ids.", self.ids.len()));
                }
                let rect = a.opt_rect("rect")?;
                let page = match (rect, a.get("page")) {
                    (Some(_), Some(_)) => self.page_arg(&a, "page")?,
                    (Some(_), None) => return Err("Give 'page' for the signature box.".into()),
                    _ => self.current,
                };
                let reason = a.opt_str("reason").unwrap_or("").to_owned();
                let d = SignDialog {
                    id: id as usize,
                    password: a.str("password")?.to_owned(),
                    reason: reason.clone(),
                    location: a.opt_str("location").unwrap_or("").to_owned(),
                    visible: rect.is_some(),
                    place: rect.map(|r| (page, r)),
                    image: ImageChoice::None,
                    show_name: true,
                    show_date: true,
                    show_reason: !reason.is_empty(),
                    error: None,
                };
                self.end_edit();
                let saved_sel = self.sel.take();
                let res = self.sign_document(&d);
                self.sel = saved_sel;
                let (signed, signer) = res?;
                let mut path = resolve(a.str("output_path")?, &self.doc);
                if path.extension().is_none() {
                    path.set_extension("pdf");
                }
                std::fs::write(&path, &signed).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
                self.doc.dirty = false;
                self.open_path(path.clone());
                self.status = format!("Signed by {signer} and saved to {}.", path.display());
                Ok(json!({"signed_by": signer, "saved": path.display().to_string(), "signatures": self.sig_reports.len()}))
            }
            "list_documents" | "close_document" => Err("This tool is answered by PDFflow's first window.".into()),
            "get_outline" => {
                let Some(src) = self.doc.source.clone() else { return Ok(json!({"outline": [], "metadata": {}, "note": "A new document has no outline or metadata."})) };
                let pdf = lopdf::Document::load_mem(&src).map_err(|e| format!("Cannot read the PDF structure: {e}"))?;
                // Outline pages are source pages; show where they are in the edited document.
                let page_of = |n: usize| self.doc.pages.iter().position(|p| p.src == PageSrc::Original(n.saturating_sub(1))).map(|i| i + 1);
                let outline: Vec<Value> = pdf.get_toc().map(|t| t.toc).unwrap_or_default().into_iter().take(3000).map(|e| json!({"level": e.level, "title": e.title, "page": page_of(e.page)})).collect();
                let mut meta = serde_json::Map::new();
                if let Ok(info) = pdf.trailer.get(b"Info").and_then(|o| pdf.dereference(o)).and_then(|(_, o)| o.as_dict()) {
                    for (key, name) in [("Title", "title"), ("Author", "author"), ("Subject", "subject"), ("Keywords", "keywords"), ("Creator", "creator"), ("Producer", "producer"), ("CreationDate", "created"), ("ModDate", "modified")] {
                        if let Some(v) = info.get(key.as_bytes()).ok().and_then(|o| pdf.dereference(o).ok()).and_then(|(_, o)| lopdf::decode_text_string(o).ok()).filter(|v| !v.trim().is_empty()) {
                            meta.insert(name.into(), json!(v));
                        }
                    }
                }
                let mut out = json!({"outline": outline, "metadata": meta});
                if out["outline"].as_array().is_some_and(Vec::is_empty) {
                    out["note"] = json!("The document has no bookmarks; get_text and find_text show its headings.");
                }
                Ok(out)
            }
            "insert_pdf" => {
                let path = resolve(a.str("path")?, &self.doc);
                let bytes = Arc::new(std::fs::read(&path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?);
                let n = lopdf::Document::load_mem(&bytes).map_err(|e| format!("{} is not a readable PDF: {e}", path.display()))?.get_pages().len();
                let idx: Vec<usize> = match a.get("pages").and_then(Value::as_array) {
                    Some(list) => list
                        .iter()
                        .map(|v| match v.as_i64() {
                            Some(k) if k >= 1 && k as usize <= n => Ok(k as usize - 1),
                            _ => Err(format!("{} has pages 1 to {n}; {v} is not one of them.", path.display())),
                        })
                        .collect::<Result<_, _>>()?,
                    None => (0..n).collect(),
                };
                if idx.is_empty() {
                    return Err("No pages to insert.".into());
                }
                let len = self.doc.pages.len();
                let at = match a.opt_int("after") {
                    Some(k) if k < 0 || k as usize > len => return Err(format!("'after' must be 0 to {len}.")),
                    Some(k) => k as usize,
                    None => len,
                };
                // The other file's comments come along with its pages.
                let notes = notes::read_comments(&bytes);
                let entries = idx.iter().map(|&i| PageEntry { src: PageSrc::Original(i), size: A4, rotation: 0, objects: notes.get(i).cloned().unwrap_or_default() }).collect();
                self.end_edit();
                let added = self.import_pages(entries, Some(bytes), at)?;
                Ok(json!({"inserted": added, "first_page": at + 1, "page_count": self.doc.pages.len()}))
            }
            "replace_text" => {
                let q = a.str("query")?;
                let new = a.opt_str("replacement").ok_or("'replacement' is required (empty to delete).")?.to_owned();
                let pages: Vec<usize> = if a.get("page").is_some() { vec![self.page_arg(&a, "page")?] } else { (0..self.doc.pages.len()).collect() };
                let mut targets: Vec<(usize, Arc<PageText>, Vec<usize>)> = Vec::new();
                for (p, t) in self.texts(pages.into_iter()) {
                    for m in t.find(q, a.bool("case_sensitive")) {
                        targets.push((p, t.clone(), m));
                    }
                }
                let total = targets.len();
                if let Some(k) = a.opt_int("occurrence") {
                    let k = k.max(1) as usize;
                    targets = targets.into_iter().nth(k - 1).into_iter().collect();
                    if targets.is_empty() {
                        return Err(format!("'{q}' occurs {total} time(s); there is no occurrence {k}."));
                    }
                }
                if targets.is_empty() {
                    return Err(format!("'{q}' was not found (find_text shows the text as PDFflow reads it)."));
                }
                let color = a.color("color")?.unwrap_or(Color32::BLACK);
                let cover = a.color("cover_color")?.unwrap_or(Color32::WHITE);
                let bold = a.bool("bold");
                let face = crate::text::face(bold);
                let mut added: Vec<(usize, Obj)> = Vec::new();
                let mut done = Vec::new();
                let mut skipped = 0;
                for (p, t, g) in &targets {
                    let runs = t.runs(g);
                    let Some(&(first, origin, end, size0)) = runs.first() else { continue };
                    // Only horizontal text: a rotated line would need rotated text.
                    if (end.y - origin.y).abs() > 0.5 + 0.02 * (end.x - origin.x).abs() {
                        skipped += 1;
                        continue;
                    }
                    for (r, ..) in &runs {
                        added.push((*p, Obj { rect: r.expand2(vec2(0.6, 0.4)), kind: Kind::Shape { shape: ShapeKind::Rect, stroke: cover, fill: Some(cover), width: 0.0, a: pos2(0.0, 0.0), b: pos2(1.0, 1.0) } }));
                    }
                    let size = a.opt_num("size").unwrap_or(size0).clamp(2.0, 300.0);
                    let mut item = json!({"page": p + 1, "was": t.text_of(g), "box": bx(first)});
                    if !new.is_empty() {
                        let top = origin.y - face.ascent / face.upem * size;
                        let mut o = Obj { rect: Rect::from_min_size(pos2(origin.x, top), vec2(1.0, 1.0)), kind: Kind::Text { text: new.clone(), size, color, bold } };
                        o.refit_text();
                        let old_w: f32 = runs.iter().map(|(r, ..)| r.width()).fold(0.0, f32::max);
                        if o.rect.width() > old_w + 1.0 {
                            item["overflow_pt"] = json!(r1(o.rect.width() - old_w));
                        }
                        added.push((*p, o));
                    }
                    done.push(item);
                }
                if added.is_empty() {
                    return Err("The matches are on rotated lines, which replace_text cannot retype; use add_whiteout and add_text.".into());
                }
                self.end_edit();
                self.doc.checkpoint();
                for (p, o) in added.iter().cloned() {
                    self.doc.pages[p].objects.push(o);
                }
                self.focus(added[0].0);
                let mut out = json!({"replaced": done.len(), "matches": done, "objects_added": added.len()});
                if skipped > 0 {
                    out["skipped_rotated"] = json!(skipped);
                }
                Ok(out)
            }
            "batch" => {
                const NOT_IN_BATCH: [&str; 8] = ["open_pdf", "new_document", "close_document", "save_pdf", "sign_pdf", "undo", "redo", "batch"];
                let calls = a.get("calls").and_then(Value::as_array).ok_or("'calls' (a list of {tool, arguments}) is required.")?.clone();
                if calls.is_empty() || calls.len() > 500 {
                    return Err("A batch takes 1 to 500 calls.".into());
                }
                // Check every call before running any.
                for (k, c) in calls.iter().enumerate() {
                    let tool = c.get("tool").and_then(Value::as_str).ok_or_else(|| format!("Call {} has no 'tool'.", k + 1))?;
                    if NOT_IN_BATCH.contains(&tool) || tool == "list_documents" {
                        return Err(format!("Call {} ({tool}) cannot run inside a batch.", k + 1));
                    }
                    let args = c.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    if args.get("document").is_some_and(|d| !d.is_null()) {
                        return Err(format!("Call {} names a document; the calls of a batch work on the batch's document.", k + 1));
                    }
                    crate::mcp_tools::check(tool, &args).map_err(|e| format!("Call {}: {e}", k + 1))?;
                }
                let atomic = a.opt_bool("atomic").unwrap_or(true);
                self.end_edit();
                self.close_note();
                let (before, mark, dirty) = (self.doc.pages.clone(), self.doc.mark(), self.doc.dirty);
                let mut results = Vec::new();
                let mut failed = None;
                for (k, c) in calls.iter().enumerate() {
                    let tool = c["tool"].as_str().unwrap_or("");
                    let args = c.get("arguments").cloned().unwrap_or_else(|| json!({}));
                    match self.agent_run(ctx, tool, &args) {
                        Ok(mut v) => {
                            if let Some(o) = v.as_object_mut() {
                                o.remove("image_png");
                            }
                            results.push(json!({"tool": tool, "result": v}));
                        }
                        Err(e) => {
                            failed = Some(json!({"call": k + 1, "tool": tool, "error": e}));
                            break;
                        }
                    }
                }
                match failed {
                    Some(f) if atomic => {
                        self.doc.rollback(mark, before, dirty);
                        self.sel = None;
                        self.text_sel = None;
                        self.rlive = None;
                        self.current = self.current.min(self.doc.pages.len() - 1);
                        Err(format!("Call {} ({}) failed: {}. The batch was rolled back, so nothing changed.", f["call"], f["tool"].as_str().unwrap_or(""), f["error"].as_str().unwrap_or("")))
                    }
                    f => {
                        self.doc.squash(mark, before);
                        let mut out = json!({"completed": results.len(), "results": results, "undo_steps": 1});
                        if let Some(f) = f {
                            out["failed"] = f;
                        }
                        Ok(out)
                    }
                }
            }
            _ => Err(format!("Unknown tool '{name}'.")),
        }
    }
}

/// The adjustment an `adjust_picture` call asks for.
fn adjustment(a: &Args) -> Result<Adj, String> {
    let kind = a.str("adjustment")?;
    let c3 = |k: &str, d: [f32; 3]| -> Result<[f32; 3], String> { Ok(a.color(k)?.map_or(d, |c| [c.r(), c.g(), c.b()].map(|v| v as f32 / 255.0))) };
    Ok(match kind {
        "brightness_contrast" => Adj::BrightnessContrast { brightness: a.opt_num("brightness").unwrap_or(0.0).clamp(-150.0, 150.0), contrast: a.opt_num("contrast").unwrap_or(0.0).clamp(-50.0, 100.0), legacy: false },
        "levels" => {
            let mut ch = [LevelsCh::default(); 4];
            ch[0].in_black = a.opt_num("black").unwrap_or(0.0).clamp(0.0, 253.0) / 255.0;
            ch[0].in_white = a.opt_num("white").unwrap_or(255.0).clamp(2.0, 255.0) / 255.0;
            ch[0].gamma = a.opt_num("gamma").unwrap_or(1.0).clamp(0.1, 9.99);
            if ch[0].in_white <= ch[0].in_black {
                return Err("white must be above black.".into());
            }
            Adj::Levels { ch }
        }
        "exposure" => Adj::Exposure { exposure: a.opt_num("exposure").unwrap_or(0.0).clamp(-20.0, 20.0), offset: a.opt_num("offset").unwrap_or(0.0).clamp(-0.5, 0.5), gamma: a.opt_num("gamma").unwrap_or(1.0).clamp(0.01, 9.99) },
        "vibrance" => Adj::Vibrance { vibrance: a.opt_num("vibrance").unwrap_or(0.0).clamp(-100.0, 100.0), saturation: a.opt_num("saturation").unwrap_or(0.0).clamp(-100.0, 100.0) },
        "hue_saturation" => Adj::HueSaturation {
            hue: a.opt_num("hue").unwrap_or(0.0).clamp(-180.0, 180.0),
            saturation: a.opt_num("saturation").unwrap_or(0.0).clamp(-100.0, 100.0),
            lightness: a.opt_num("lightness").unwrap_or(0.0).clamp(-100.0, 100.0),
            colorize: a.bool("colorize"),
        },
        "photo_filter" => Adj::PhotoFilter { color: c3("color", crate::adjust::rgb3(crate::adjust::PHOTO_FILTERS[0].1))?, density: a.opt_num("density").unwrap_or(25.0).clamp(0.0, 100.0) / 100.0, preserve: true },
        "posterize" => Adj::Posterize { levels: a.opt_int("levels").unwrap_or(4).clamp(2, 255) as u32 },
        "threshold" => Adj::Threshold { level: a.opt_num("level").unwrap_or(128.0).clamp(1.0, 255.0) },
        "gradient_map" => Adj::GradientMap { from: c3("from", [0.0; 3])?, to: c3("to", [1.0; 3])?, reverse: false },
        "shadows_highlights" => {
            let mut s = crate::adjust::ShadowsHighlights::default();
            if let Some(v) = a.opt_num("shadows") {
                s.shadow_amount = v.clamp(0.0, 100.0);
            }
            if let Some(v) = a.opt_num("highlights") {
                s.highlight_amount = v.clamp(0.0, 100.0);
            }
            Adj::ShadowsHighlights(s)
        }
        "black_white" => Adj::new(AdjKind::BlackWhite),
        "hdr_toning" => Adj::new(AdjKind::HdrToning),
        "invert" => Adj::Invert,
        "desaturate" => Adj::Desaturate,
        "equalize" => Adj::Equalize,
        "auto_tone" => Adj::AutoTone,
        "auto_contrast" => Adj::AutoContrast,
        "auto_color" => Adj::AutoColor,
        o => return Err(format!("Unknown adjustment '{o}'.")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse() {
        assert_eq!(parse_color("#ff0000").unwrap(), Color32::from_rgb(255, 0, 0));
        assert_eq!(parse_color("Yellow").unwrap(), Color32::from_rgb(255, 220, 0));
        assert_eq!(parse_color("#00ff0080").unwrap(), Color32::from_rgba_unmultiplied(0, 255, 0, 128));
        assert!(parse_color("#12").is_err() && parse_color("chartreuse").is_err());
        assert_eq!(hex(Color32::from_rgb(1, 2, 255)), "#0102ff");
    }

    #[test]
    fn every_listed_tool_is_implemented() {
        // Unknown tools fall through to the last arm; every listed tool must have its own.
        let src = include_str!("agent.rs");
        for t in crate::mcp_tools::list() {
            let name = t["name"].as_str().unwrap();
            assert!(src.contains(&format!("\"{name}\" =>")) || src.contains(&format!("\"{name}\" |")) || src.contains(&format!("| \"{name}\"")), "{name} has no implementation");
        }
    }
}
