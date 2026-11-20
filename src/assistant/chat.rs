//! The Assistant panel (ported from Aedifex's `chat.rs`): one agent on TellusHive's harness that
//! reviews and edits the document in this window with PDFflow's tools. The chat shows the
//! agent's text as it streams, its reasoning (folded), one row per tool call (name, arguments,
//! status, duration, result), its plan, and a token line per message. Every edit appears in the
//! window as the agent makes it; when the message ends, its edits become one undo step. The
//! provider is an OpenAI-compatible API (DeepSeek, OpenAI, OpenRouter, Ollama, vLLM) or Claude
//! Code / Codex, which get the same tools over MCP.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Stroke};

use super::cli::{Which, find_program};
use super::engine::{self, Editor, Event, Setup, Turn};
use super::settings::{self, AgentSettings, Keys, PROVIDERS, Store};
use super::transcript::{Entry, Transcript, one_line_args};

/// The chat's colours, from PDFflow's theme (Aedifex's chat tokens on the Photoshop greys).
#[derive(Clone, Copy, Debug)]
pub struct Tokens {
    pub dark: bool,
    pub card: Color32,
    pub field: Color32,
    pub field_border: Color32,
    pub separator: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    /// The user's bubble.
    pub accent_soft: Color32,
    pub accent_text: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub error: Color32,
}

impl Tokens {
    pub fn get(ctx: &egui::Context) -> Tokens {
        let t = crate::theme::Tokens::get(ctx);
        let mix = |a: Color32, b: Color32, k: f32| Color32::from_rgb((a.r() as f32 * (1.0 - k) + b.r() as f32 * k) as u8, (a.g() as f32 * (1.0 - k) + b.g() as f32 * k) as u8, (a.b() as f32 * (1.0 - k) + b.b() as f32 * k) as u8);
        Tokens {
            dark: true,
            card: t.card,
            field: t.field,
            field_border: t.field_border,
            separator: t.field_border,
            text: t.text,
            text_dim: t.text_dim,
            text_faint: t.text_faint,
            accent: t.accent,
            accent_soft: mix(t.field, t.accent, 0.28),
            accent_text: Color32::from_rgb(141, 184, 245),
            ok: Color32::from_rgb(110, 200, 120),
            warn: Color32::from_rgb(232, 170, 70),
            error: t.danger,
        }
    }
}

/// What the window does for the chat after a frame.
#[derive(Default)]
pub struct Effects {
    /// A message started: remember the document as it is, to fold the agent's edits into one
    /// undo step when it ends.
    pub started: bool,
    /// The message ended.
    pub finished: bool,
}

/// What the window tells the chat each frame.
pub struct Host {
    /// "[document 3: report.pdf, 12 pages, page 2 on screen, ...]"
    pub document: String,
    pub editor: Editor,
    pub mcp_addr: Option<String>,
}

pub struct ChatView {
    transcript: Transcript,
    draft: String,
    settings_open: bool,
    store: Store,
    settings: AgentSettings,
    keys: Keys,
    key_edit: String,
    pending: Option<Receiver<Event>>,
    cancel: Arc<AtomicBool>,
    started: Option<std::time::Instant>,
    /// What the agent is doing now ("Thinking", "find_text").
    activity: String,
    attachments: Vec<PathBuf>,
    thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    open_rows: HashSet<String>,
    session: String,
    /// Claude Code and Codex as found (looked up when Settings opens).
    found: Option<(Option<PathBuf>, Option<PathBuf>)>,
    /// A model in place of the provider (tests).
    pub(crate) adapter: Option<Arc<dyn th_hive::LlmAdapter>>,
    /// Send the draft in the next frame (a message asked for from outside the panel).
    send_next: bool,
}

const IMAGES: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];
const TEXTS: [&str; 7] = ["txt", "md", "csv", "tsv", "json", "xml", "html"];

fn ext(p: &Path) -> String {
    p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

fn is_image(p: &Path) -> bool {
    IMAGES.contains(&ext(p).as_str())
}

impl ChatView {
    pub fn new() -> Self {
        Self::with_store(Store::user())
    }

    pub fn with_store(store: Store) -> Self {
        let settings = store.load();
        let keys = store.keys();
        let ready = settings.is_cli() || settings.llm_provider(&keys).ready();
        ChatView {
            transcript: Transcript::default(),
            draft: String::new(),
            settings_open: !ready,
            store,
            settings,
            keys,
            key_edit: String::new(),
            pending: None,
            cancel: Default::default(),
            started: None,
            activity: String::new(),
            attachments: vec![],
            thumbs: Default::default(),
            open_rows: Default::default(),
            session: engine::new_session_id(),
            found: None,
            adapter: None,
            send_next: false,
        }
    }

    #[cfg(test)]
    pub fn entries(&self) -> &[Entry] {
        &self.transcript.entries
    }

    /// Tests: puts a message in the box and sends it with the next frame.
    #[cfg(test)]
    pub fn ask(&mut self, s: &str) {
        self.draft = s.into();
        self.send_next = true;
    }

    #[cfg(test)]
    pub fn attachments(&self) -> &[PathBuf] {
        &self.attachments
    }

    #[cfg(test)]
    pub fn set_settings_open(&mut self, on: bool) {
        self.settings_open = on;
    }

    fn work_dir(&self) -> PathBuf {
        self.store.dir.join("assistant")
    }

    /// Adds files to the next message: pictures and text files.
    pub fn attach(&mut self, files: Vec<PathBuf>) {
        for p in files {
            let e = ext(&p);
            if !(IMAGES.contains(&e.as_str()) || TEXTS.contains(&e.as_str()) || e == "pdf") {
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                self.transcript.entries.push(Entry::Note(format!("{name}: attach pictures, PDFs or text files.")));
            } else if !self.attachments.contains(&p) {
                self.attachments.push(p);
            }
        }
    }

    fn texture(&mut self, ctx: &egui::Context, p: &Path, max: u32) {
        if !is_image(p) || self.thumbs.contains_key(p) {
            return;
        }
        let tex = image::open(p).ok().map(|i| {
            let i = i.thumbnail(max, max).to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied([i.width() as usize, i.height() as usize], i.as_raw());
            ctx.load_texture(format!("chat-{}", p.display()), ci, egui::TextureOptions::LINEAR)
        });
        self.thumbs.insert(p.to_path_buf(), tex);
    }

    /// Why a message cannot go yet.
    fn ready(&self) -> Result<(), String> {
        if self.adapter.is_some() {
            return Ok(());
        }
        let s = &self.settings;
        match s.provider.as_str() {
            "claude-code" => find_program("claude", &s.claude_path).map(|_| ()).ok_or_else(|| "Claude Code (claude) was not found: install it (npm install -g @anthropic-ai/claude-code) and log in, or set its path in Settings.".into()),
            "codex" => find_program("codex", &s.codex_path).map(|_| ()).ok_or_else(|| "Codex (codex) was not found: install it (npm install -g @openai/codex) and log in, or set its path in Settings.".into()),
            id => {
                if s.llm_provider(&self.keys).ready() {
                    Ok(())
                } else {
                    Err(format!("Add the {} API key in Settings.", settings::label(id)))
                }
            }
        }
    }

    /// Sends the draft; true when a message started.
    pub fn send(&mut self, ctx: &egui::Context, host: &Host) -> bool {
        let mut text = self.draft.trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return false;
        }
        if text.is_empty() {
            text = "Look at the attached file(s).".into();
        }
        if let Err(e) = self.ready() {
            self.transcript.entries.push(Entry::Note(e));
            self.settings_open = true;
            return false;
        }
        let work = self.work_dir();
        let files = std::mem::take(&mut self.attachments);
        let mut docs = String::new();
        let mut images = vec![];
        let mut notes = vec![];
        for (i, f) in files.iter().enumerate() {
            let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            if is_image(f) {
                // kept beside the session so it can be replayed
                let dir = work.join("attachments");
                let to = dir.join(format!("{}-{}-{name}", th_hive::session::now_ms(), i + 1));
                match std::fs::create_dir_all(&dir).and_then(|_| std::fs::copy(f, &to)) {
                    Ok(_) => images.push(to),
                    Err(e) => notes.push(format!("{name}: {e}")),
                }
            } else if ext(f) == "pdf" {
                match pdf_text(f) {
                    Ok(t) => docs += &format!("\n--- Attached file: {name} at {} (its text; open_pdf it with new_window: true to look at or edit its pages) ---\n{t}\n--- End of {name} ---", f.display()),
                    Err(e) => notes.push(format!("{name}: {e}")),
                }
            } else {
                match std::fs::read(f) {
                    Ok(b) => docs += &format!("\n--- Attached file: {name} ---\n{}\n--- End of {name} ---", th_hive::clip(&String::from_utf8_lossy(&b), 60_000)),
                    Err(e) => notes.push(format!("{name}: {e}")),
                }
            }
        }
        self.transcript.push_user(&text, files);
        self.transcript.entries.extend(notes.into_iter().map(Entry::Note));
        self.draft.clear();
        let setup = Setup {
            settings: self.settings.clone(),
            keys: self.keys.clone(),
            sessions_dir: work.join("sessions"),
            work_dir: work,
            editor: host.editor.clone(),
            mcp_addr: host.mcp_addr.clone(),
            exe: None,
            adapter: self.adapter.clone(),
        };
        let (tx, rx) = channel();
        self.pending = Some(rx);
        self.cancel = Arc::new(AtomicBool::new(false));
        self.started = Some(std::time::Instant::now());
        self.activity = "Starting".into();
        let ctx = ctx.clone();
        engine::start(setup, self.session.clone(), Turn { text, document: host.document.clone(), attachments: docs, images }, self.cancel.clone(), tx, move || ctx.request_repaint());
        true
    }

    /// Takes the agent's events; true when the message ended.
    fn poll(&mut self, ctx: &egui::Context) -> bool {
        let Some(rx) = &self.pending else { return false };
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        let mut done = None;
        let events: Vec<Event> = rx.try_iter().collect();
        for ev in events {
            if let Event::Done(r) = ev {
                done = Some(r);
                break;
            }
            match &ev {
                Event::Step => self.activity = "Thinking".into(),
                Event::Text(_) => self.activity = "Writing".into(),
                Event::Reasoning(_) => self.activity = "Reasoning".into(),
                Event::ToolCall { name, .. } => self.activity = name.clone(),
                _ => {}
            }
            self.transcript.apply(&ev);
        }
        let Some(r) = done else { return false };
        self.pending = None;
        self.started = None;
        match r {
            Ok(o) => {
                self.transcript.end_turn(Some(&o.reply), o.pictures);
                if o.reason == "max-steps" {
                    self.transcript.entries.push(Entry::Note(format!("The agent stopped at the limit of {} steps; raise it in Settings, or ask it to continue.", self.settings.max_steps)));
                }
            }
            Err(e) => {
                self.transcript.end_turn(None, vec![]);
                self.transcript.entries.push(Entry::Note(e));
            }
        }
        true
    }

    /// The panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, host: &Host) -> Effects {
        let ctx = ui.ctx().clone();
        let mut fx = Effects { finished: self.poll(&ctx), ..Default::default() };
        let t = Tokens::get(&ctx);
        self.header(ui, &t);
        if self.settings_open {
            egui::ScrollArea::vertical().id_salt("chat-settings-scroll").max_height(ui.available_height() * 0.62).show(ui, |ui| self.settings_ui(ui, &t));
            ui.add_space(6.0);
        }
        let wanted: Vec<(PathBuf, u32)> = self
            .transcript
            .entries
            .iter()
            .flat_map(|e| match e {
                Entry::User { files, .. } => files.iter().map(|f| (f.clone(), 160)).collect::<Vec<_>>(),
                Entry::Tool { pictures, .. } | Entry::Assistant { pictures, .. } => pictures.iter().map(|f| (f.clone(), 720)).collect(),
                _ => vec![],
            })
            .chain(self.attachments.iter().map(|f| (f.clone(), 160)))
            .collect();
        for (f, max) in &wanted {
            self.texture(&ctx, f, *max);
        }
        let thumbs = self.thumbs.clone();
        let bottom = if self.attachments.is_empty() { 104.0 } else { 162.0 };
        let mut toggled = None;
        egui::ScrollArea::vertical().id_salt("chat-scroll").stick_to_bottom(true).max_height((ui.available_height() - bottom).max(80.0)).auto_shrink([false, false]).show(ui, |ui| {
            if self.transcript.is_empty() {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new("Ask the agent to review or edit this PDF. It reads the pages, marks issues with comments, changes text, and rearranges or merges pages with PDFflow's tools. You watch every change in the window; when it finishes, Undo takes back the whole message.").color(t.text_dim)).wrap());
                ui.add_space(8.0);
                for ex in ["Review this document for technical errors and inconsistent units; comment each issue on its passage", "Summarise each section in three lines, with page numbers", "Replace every \"Draft\" with \"Final\" and check the pages still read well", "Move the appendix pages to the end and add a blank page before them"] {
                    if ui.add(egui::Button::new(RichText::new(ex).color(t.accent_text)).frame(false).wrap()).clicked() {
                        self.draft = ex.into();
                    }
                }
            }
            for e in &self.transcript.entries {
                match e {
                    Entry::User { text, files } => user_bubble(ui, &t, text, files, &thumbs),
                    Entry::Assistant { text, reasoning, streaming, pictures, stats } => {
                        if !reasoning.trim().is_empty() {
                            egui::CollapsingHeader::new(RichText::new("Reasoning").size(12.0).color(t.text_faint)).id_salt(("reasoning", reasoning.len(), text.len())).default_open(false).show(ui, |ui| {
                                ui.label(RichText::new(reasoning.trim()).size(12.0).color(t.text_dim));
                            });
                        }
                        if !text.trim().is_empty() {
                            super::markdown::show(ui, text, &t);
                        }
                        if *streaming {
                            ui.add(egui::Spinner::new().size(12.0).color(t.text_faint));
                        }
                        for p in pictures {
                            picture(ui, &t, p, &thumbs);
                        }
                        if let Some(s) = stats {
                            ui.label(RichText::new(s.line()).size(11.0).color(t.text_faint));
                        }
                        ui.add_space(8.0);
                    }
                    Entry::Tool { id, name, args, result, is_error, pictures, ms, .. } => {
                        if tool_row(ui, &t, id, name, args, result.as_deref(), *is_error, *ms, self.open_rows.contains(id)) {
                            toggled = Some(id.clone());
                        }
                        for p in pictures {
                            picture(ui, &t, p, &thumbs);
                        }
                    }
                    Entry::Todo(items) => todo_card(ui, &t, items),
                    Entry::Note(n) => {
                        ui.add(egui::Label::new(RichText::new(n).size(12.5).italics().color(t.text_dim)).wrap());
                        ui.add_space(4.0);
                    }
                }
            }
            if let Some(t0) = self.started {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(13.0).color(t.accent_text));
                    ui.label(RichText::new(format!("{} · {:.0} s", self.activity, t0.elapsed().as_secs_f64())).size(12.5).color(t.text_dim));
                });
            }
        });
        if let Some(id) = toggled {
            if !self.open_rows.remove(&id) {
                self.open_rows.insert(id);
            }
        }
        ui.separator();
        fx.started = self.composer(ui, &t, &thumbs, host);
        fx
    }

    fn header(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            let (r, _) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::hover());
            crate::icons::paint(ui.painter(), r, "bot", t.accent_text);
            ui.label(RichText::new("Assistant").size(16.0).strong().color(t.text));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(egui::Button::selectable(self.settings_open, "Settings").small()).clicked() {
                    self.settings_open = !self.settings_open;
                    self.found = None;
                }
                if !self.transcript.is_empty() && self.pending.is_none() && ui.small_button("Clear").on_hover_text("Start a new conversation (the old one stays in the session log)").clicked() {
                    self.transcript.clear();
                    self.open_rows.clear();
                    self.session = engine::new_session_id();
                }
            });
        });
        // the provider and model the agent runs on
        ui.horizontal(|ui| {
            let chip = self.settings.chip();
            let r = egui::Frame::new().fill(t.field).stroke(Stroke::new(1.0_f32, t.field_border)).corner_radius(CornerRadius::same(9)).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
                ui.label(RichText::new(&chip).size(11.5).color(t.text_dim));
            });
            if r.response.interact(egui::Sense::click()).on_hover_text(format!("{chip}: change it in Settings")).clicked() {
                self.settings_open = true;
                self.found = None;
            }
        });
        ui.add_space(4.0);
    }

    /// The message box; true when a message started.
    fn composer(&mut self, ui: &mut egui::Ui, t: &Tokens, thumbs: &HashMap<PathBuf, Option<egui::TextureHandle>>, host: &Host) -> bool {
        let busy = self.pending.is_some();
        if !self.attachments.is_empty() {
            let mut remove = None;
            ui.horizontal_wrapped(|ui| {
                for (i, f) in self.attachments.iter().enumerate() {
                    let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    egui::Frame::new().fill(t.card).corner_radius(6.0).inner_margin(Margin::symmetric(6, 4)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            if let Some(tex) = thumbs.get(f).cloned().flatten() {
                                let sz = tex.size_vec2();
                                ui.add(egui::Image::new((tex.id(), sz * (40.0 / sz.y.max(1.0)))).corner_radius(3.0));
                            }
                            ui.label(RichText::new(name).small());
                            if ui.small_button("×").on_hover_text("Remove").clicked() {
                                remove = Some(i);
                            }
                        });
                    });
                }
            });
            if let Some(i) = remove {
                self.attachments.remove(i);
            }
        }
        let edit = egui::TextEdit::multiline(&mut self.draft).id_salt("chat-draft").desired_rows(3).desired_width(f32::INFINITY).hint_text("Ask the agent to review or edit this PDF...");
        let r = ui.add_enabled(!busy, edit);
        let enter = (r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)) || (!busy && std::mem::take(&mut self.send_next));
        let mut started = false;
        ui.horizontal(|ui| {
            if ui.add_enabled(!busy, egui::Button::new("Attach…").small()).on_hover_text("Attach pictures (scans, sketches, photos), PDFs (their text) or text files, or drop them on the chat").clicked() {
                if let Some(files) = rfd::FileDialog::new().add_filter("Pictures, PDFs and text", &[&IMAGES[..], &TEXTS[..], &["pdf"]].concat()).pick_files() {
                    self.attach(files);
                }
            }
            ui.label(RichText::new("Enter sends, Shift+Enter a new line").small().color(t.text_faint));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if busy {
                    if ui.add(egui::Button::new(RichText::new("Stop").color(t.error))).on_hover_text("Stop the agent; the edits it made so far stay (Undo takes them back)").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.activity = "Stopping".into();
                    }
                } else if ui.add(egui::Button::new(RichText::new("Send").color(Color32::WHITE)).fill(t.accent)).clicked() || enter {
                    while self.draft.ends_with('\n') {
                        self.draft.pop();
                    }
                    let ctx = ui.ctx().clone();
                    started = self.send(&ctx, host);
                }
            });
        });
        started
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, t: &Tokens) {
        if self.found.is_none() {
            self.found = Some((find_program("claude", &self.settings.claude_path), find_program("codex", &self.settings.codex_path)));
        }
        let (claude, codex) = self.found.clone().unwrap_or_default();
        egui::Frame::new().fill(t.card).stroke(Stroke::new(1.0_f32, t.separator)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new("Model provider").strong().color(t.text));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                for (id, label) in PROVIDERS {
                    let on = self.settings.provider == id;
                    let txt = RichText::new(label).size(12.5).color(if on { t.text } else { t.text_dim });
                    if ui.add(egui::Button::selectable(on, txt).corner_radius(CornerRadius::same(7))).clicked() && !on {
                        self.settings.provider = id.into();
                        self.key_edit.clear();
                    }
                }
            });
            ui.add_space(6.0);
            let id = self.settings.provider.clone();
            let preset = settings::preset(&id);
            egui::Grid::new("agent-settings").num_columns(2).spacing([10.0, 6.0]).min_col_width(70.0).show(ui, |ui| {
                if let Some(which) = Which::of(&id) {
                    let claude_code = which == Which::ClaudeCode;
                    let found = if claude_code { &claude } else { &codex };
                    ui.label("Program");
                    ui.vertical(|ui| {
                        match found {
                            Some(p) => status_line(ui, t.ok, &format!("Found: {}", p.display())),
                            None => status_line(ui, t.warn, &format!("{} is not installed here (npm install -g {}), or set its path", which.program(), if claude_code { "@anthropic-ai/claude-code" } else { "@openai/codex" })),
                        }
                        let path = if claude_code { &mut self.settings.claude_path } else { &mut self.settings.codex_path };
                        if ui.add(egui::TextEdit::singleline(path).hint_text("path (empty: PATH and the usual places)").desired_width(f32::INFINITY)).changed() {
                            self.found = None;
                        }
                    });
                    ui.end_row();
                    ui.label("Model");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.current_mut().model).hint_text(if claude_code { "the program's default (sonnet, opus, haiku)" } else { "the program's default" }).desired_width(f32::INFINITY));
                    ui.end_row();
                    ui.label("Login");
                    ui.checkbox(&mut self.settings.subscription, RichText::new(if claude_code { "My Claude subscription (ignore ANTHROPIC_API_KEY)" } else { "My ChatGPT subscription (ignore OPENAI_API_KEY)" }).size(12.5));
                    ui.end_row();
                    ui.label("");
                    ui.add(egui::Label::new(RichText::new("The program is the model: it gets PDFflow's tools over MCP (pdfflow --mcp) and edits the document in this window.").size(11.5).color(t.text_faint)).wrap());
                    ui.end_row();
                } else {
                    ui.label("Base URL");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.current_mut().base_url).hint_text(preset.base_url.clone()).desired_width(f32::INFINITY));
                    ui.end_row();
                    ui.label("Model");
                    let hint = if preset.default_model.is_empty() { "the model the server serves".to_string() } else { preset.default_model.clone() };
                    ui.add(egui::TextEdit::singleline(&mut self.settings.current_mut().model).hint_text(hint).desired_width(f32::INFINITY));
                    ui.end_row();
                    ui.label("API key");
                    let hint = if self.keys.has(&id) { "saved (type to replace)".to_string() } else if preset.key_env.is_empty() { "not needed".to_string() } else { format!("the key, or set {}", preset.key_env) };
                    ui.add(egui::TextEdit::singleline(&mut self.key_edit).password(true).hint_text(hint).desired_width(f32::INFINITY));
                    ui.end_row();
                    ui.label("Images");
                    ui.horizontal(|ui| {
                        let v = &mut self.settings.current_mut().vision;
                        if v.is_empty() {
                            *v = "auto".into();
                        }
                        ui.selectable_value(v, "auto".to_string(), "Auto");
                        ui.selectable_value(v, "yes".to_string(), "Yes");
                        ui.selectable_value(v, "no".to_string(), "No");
                        ui.label(RichText::new("the model takes images").small().color(t.text_faint));
                    });
                    ui.end_row();
                }
                ui.label("Options");
                ui.vertical(|ui| {
                    ui.checkbox(&mut self.settings.subagents, RichText::new("Subagents: the agent may hand part of the work to a helper").size(12.5));
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.settings.max_steps).range(4..=400));
                        ui.label(RichText::new("steps per message at most").size(12.5).color(t.text_dim));
                    });
                });
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("Save").color(Color32::WHITE)).fill(t.accent)).clicked() {
                    let mut err = self.store.save(&self.settings).err();
                    if !self.key_edit.trim().is_empty() {
                        match self.store.save_key(&id, &self.key_edit) {
                            Ok(()) => {
                                self.keys.set(&id, &self.key_edit);
                                self.key_edit.clear();
                            }
                            Err(e) => err = Some(e),
                        }
                    }
                    match err {
                        Some(e) => self.transcript.entries.push(Entry::Note(e)),
                        None if self.ready().is_ok() => self.settings_open = false,
                        None => {}
                    }
                }
                let (c, s) = match self.ready() {
                    Ok(()) => (t.ok, format!("Ready: {}", self.settings.chip())),
                    Err(e) => (t.warn, e),
                };
                status_line(ui, c, &s);
            });
        });
    }
}

/// The text layer of an attached PDF, page by page.
fn pdf_text(p: &Path) -> Result<String, String> {
    let bytes = Arc::new(std::fs::read(p).map_err(|e| e.to_string())?);
    let n = lopdf::Document::load_mem(&bytes).map_err(|e| format!("not a readable PDF: {e}"))?.get_pages().len();
    let pages = crate::textlayer::extract(bytes, &(0..n.min(300)).collect::<Vec<_>>())?;
    let mut s = String::new();
    for (i, t) in pages {
        let text = t.all_text();
        if !text.trim().is_empty() {
            s.push_str(&format!("[page {}]\n{}\n", i + 1, text.trim()));
        }
    }
    if s.trim().is_empty() {
        return Err(format!("it has no text layer ({n} pages, perhaps scanned); open it in PDFflow and ask the agent to look at its pages"));
    }
    Ok(th_hive::clip(&s, 60_000))
}

fn status_line(ui: &mut egui::Ui, colour: Color32, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (r, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().circle_filled(r.center(), 3.5, colour);
        ui.add(egui::Label::new(RichText::new(text).size(12.0)).wrap());
    });
}

fn user_bubble(ui: &mut egui::Ui, t: &Tokens, text: &str, files: &[PathBuf], thumbs: &HashMap<PathBuf, Option<egui::TextureHandle>>) {
    ui.with_layout(egui::Layout::top_down(egui::Align::Max), |ui| {
        egui::Frame::new().fill(t.accent_soft).corner_radius(10.0).inner_margin(Margin::symmetric(10, 7)).show(ui, |ui| {
            ui.set_max_width(ui.available_width() * 0.88);
            ui.add(egui::Label::new(RichText::new(text).color(t.text)).wrap());
            if !files.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    for f in files {
                        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        match thumbs.get(f).cloned().flatten() {
                            Some(tex) => {
                                let sz = tex.size_vec2();
                                ui.add(egui::Image::new((tex.id(), sz * (72.0 / sz.y.max(1.0)))).corner_radius(4.0)).on_hover_text(&name);
                            }
                            None => {
                                ui.label(RichText::new(name).small().color(t.text_dim));
                            }
                        }
                    }
                });
            }
        });
    });
    ui.add_space(8.0);
}

fn picture(ui: &mut egui::Ui, t: &Tokens, p: &Path, thumbs: &HashMap<PathBuf, Option<egui::TextureHandle>>) {
    match thumbs.get(p).cloned().flatten() {
        Some(tex) => {
            let sz = tex.size_vec2();
            let w = ui.available_width().min(460.0);
            ui.add_space(2.0);
            ui.add(egui::Image::new((tex.id(), egui::vec2(w, w * sz.y / sz.x.max(1.0)))).corner_radius(6.0)).on_hover_text(p.display().to_string());
            ui.add_space(4.0);
        }
        None if !p.is_file() => {
            ui.label(RichText::new("(the picture is no longer on disk)").size(11.0).color(t.text_faint));
        }
        None => {}
    }
}

/// A tool call: status, name, arguments on one line, duration; the result when open.
/// Returns true when clicked (open or close it).
#[allow(clippy::too_many_arguments)]
fn tool_row(ui: &mut egui::Ui, t: &Tokens, id: &str, name: &str, args: &str, result: Option<&str>, is_error: bool, ms: Option<u64>, open: bool) -> bool {
    let fill = if open { t.card } else { Color32::TRANSPARENT };
    let r = egui::Frame::new().fill(fill).stroke(Stroke::new(1.0_f32, t.separator)).corner_radius(CornerRadius::same(7)).inner_margin(Margin::symmetric(8, 4)).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            match (result, is_error) {
                (None, _) => {
                    ui.add(egui::Spinner::new().size(12.0).color(t.accent_text));
                }
                (Some(_), err) => {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                    crate::icons::paint(ui.painter(), r, if err { "x" } else { "check" }, if err { t.error } else { t.ok });
                }
            }
            ui.label(RichText::new(name).monospace().size(12.0).color(if is_error { t.error } else { t.text }));
            let dur = match ms {
                Some(ms) if ms >= 1000 => format!("{:.1} s", ms as f64 / 1000.0),
                Some(ms) => format!("{ms} ms"),
                None => String::new(),
            };
            let reserve = if dur.is_empty() { 4.0 } else { 54.0 };
            let w = (ui.available_width() - reserve).max(10.0);
            ui.allocate_ui(egui::vec2(w, 16.0), |ui| ui.add(egui::Label::new(RichText::new(one_line_args(args)).size(11.5).color(t.text_faint)).truncate()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(dur).size(11.0).color(t.text_faint));
            });
        });
        if let (true, Some(res)) = (open, result) {
            ui.add_space(2.0);
            egui::ScrollArea::vertical().id_salt(("tool-result", id)).max_height(180.0).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(th_hive::clip(res, 6000)).monospace().size(11.0).color(if is_error { t.error } else { t.text_dim })).wrap());
            });
        }
    });
    let clicked = r.response.interact(egui::Sense::click()).on_hover_text("Show or hide the result").clicked();
    ui.add_space(3.0);
    clicked
}

fn todo_card(ui: &mut egui::Ui, t: &Tokens, items: &[th_hive::session::TodoItem]) {
    let done = items.iter().filter(|i| i.status == "completed").count();
    egui::Frame::new().fill(t.card).stroke(Stroke::new(1.0_f32, t.separator)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new("Plan").strong().size(12.5).color(t.text));
            ui.label(RichText::new(format!("{done} of {}", items.len())).size(11.5).color(t.text_faint));
        });
        for it in items {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (r, _) = ui.allocate_exact_size(egui::vec2(13.0, 13.0), egui::Sense::hover());
                match it.status.as_str() {
                    "completed" => crate::icons::paint(ui.painter(), r, "check", t.ok),
                    "in_progress" => {
                        ui.painter().circle_stroke(r.center(), 5.0, Stroke::new(1.5_f32, t.accent_text));
                        ui.painter().circle_filled(r.center(), 2.5, t.accent_text);
                    }
                    _ => {
                        ui.painter().circle_stroke(r.center(), 5.0, Stroke::new(1.2_f32, t.text_faint));
                    }
                }
                let c = match it.status.as_str() {
                    "completed" => t.text_faint,
                    "in_progress" => t.text,
                    _ => t.text_dim,
                };
                let mut txt = RichText::new(&it.content).size(12.5).color(c);
                if it.status == "completed" {
                    txt = txt.strikethrough();
                }
                ui.add(egui::Label::new(txt).wrap());
            });
        }
    });
    ui.add_space(4.0);
}
