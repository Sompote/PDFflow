//! One message of the chat: a th-hive agent turn on the document open in the window (ported
//! from Aedifex's `engine.rs`).
//!
//! The harness is TellusHive's base bundle (session log, step loop, tool pipeline, retry,
//! compaction, repeat-tool reminder, continuation guard, `todo_write`; `delegate` only when
//! subagents are on) plus the `pdfflow` plugin: every PDFflow MCP tool as a hive tool that runs
//! in the editor itself (`Editor` hands the call to the UI thread, as the MCP server does), so
//! the user watches each change and can undo it, and the working rules as the system prompt.
//! With Claude Code or Codex the program is the model and reaches the same tools over MCP
//! (`cli.rs`), through `pdfflow --mcp --connect` to this editor.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use th_hive::plugins::subagent::{ChildAgent, SubagentProvider, Subagents};
use th_hive::plugins::{base_bundle, BaseConfig};
use th_hive::session::{EventBody, TodoItem};
use th_hive::{fn_plugin, keys, AgentLoop, AgentPreset, Context, Inbound, LlmAdapter, LlmService, Plugin, SessionMeta, StreamFrame, SystemPrompt, ToolDef, ToolFilter, ToolOutput, ToolRegistry};

use super::cli::{self, CliAdapter, CliSetup, McpServer, Which};
use super::prompt;
use super::settings::{AgentSettings, Keys};

/// Runs one PDFflow tool in the editor and returns its value (as the MCP tools do).
pub type Editor = Arc<dyn Fn(&str, Value) -> Result<Value, String> + Send + Sync>;

/// What the chat hears while a message runs.
#[derive(Debug, Clone)]
pub enum Event {
    /// A model request started.
    Step,
    Text(String),
    Reasoning(String),
    ToolCall { id: String, name: String, args: String, ts: u64 },
    ToolResult { id: String, text: String, is_error: bool, pictures: Vec<PathBuf>, ts: u64 },
    Todo(Vec<TodoItem>),
    /// A whole assistant message (the streamed text it replaces, and its tokens).
    Assistant { text: String, reasoning: Option<String>, prompt: u64, completion: u64, cached: u64, model: String },
    Cost(f64),
    Note(String),
    Done(Result<Outcome, String>),
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub reply: String,
    /// completed | max-steps | cancelled | stuck | truncated | rejected
    pub reason: String,
    /// Pictures a Claude Code / Codex run made (in-process ones are on their tool rows).
    pub pictures: Vec<PathBuf>,
}

/// One message.
#[derive(Debug, Clone, Default)]
pub struct Turn {
    /// The user's words.
    pub text: String,
    /// "[document 1: report.pdf, 12 pages, page 3 on screen, ...]"
    pub document: String,
    /// The attached documents' text.
    pub attachments: String,
    pub images: Vec<PathBuf>,
}

/// Where and how the agent runs.
#[derive(Clone)]
pub struct Setup {
    pub settings: AgentSettings,
    pub keys: Keys,
    /// The chat's session logs.
    pub sessions_dir: PathBuf,
    /// The agent's folder: pictures, attachments, the working folder of Claude Code / Codex.
    pub work_dir: PathBuf,
    pub editor: Editor,
    /// Where this editor serves MCP (Claude Code / Codex reach it there); None when it does not.
    pub mcp_addr: Option<String>,
    /// The PDFflow program that bridges MCP for Claude Code / Codex (default: this one).
    pub exe: Option<PathBuf>,
    /// A model in place of the provider (tests).
    pub adapter: Option<Arc<dyn LlmAdapter>>,
}

impl Setup {
    pub fn pictures_dir(&self) -> PathBuf {
        self.work_dir.join("pictures")
    }
}

/// A fresh session id.
pub fn new_session_id() -> String {
    format!("chat-{}", th_hive::session::now_ms())
}

/// Starts a message on its own thread; events (ending with `Done`) go to `tx`.
pub fn start(setup: Setup, session: String, turn: Turn, cancel: Arc<AtomicBool>, tx: Sender<Event>, wake: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let r = run_turn(&setup, &session, turn, &cancel, &tx);
        let _ = tx.send(Event::Done(r));
        wake();
    });
}

const PICS: &str = "pdfflow-pictures";
const AGENT_ID: &str = "assistant";

/// Runs one message to its end.
pub fn run_turn(setup: &Setup, session_id: &str, turn: Turn, cancel: &Arc<AtomicBool>, tx: &Sender<Event>) -> Result<Outcome, String> {
    let st = &setup.settings;
    let which = Which::of(&st.provider);
    let provider = st.llm_provider(&setup.keys);
    if which.is_none() && setup.adapter.is_none() && !provider.ready() {
        return Err(format!("Add the {} API key in the Assistant's Settings (or set {}).", super::settings::label(&st.provider), provider.key_env));
    }
    let vision = which.is_none()
        && match st.current().vision.as_str() {
            "yes" => true,
            "no" => false,
            _ if setup.adapter.is_none() && provider.base_url.contains("openrouter.ai") => super::settings::openrouter_takes_images(&st.model()).unwrap_or(provider.vision),
            _ => provider.vision,
        };
    let pictures_dir = setup.pictures_dir();
    let _ = std::fs::create_dir_all(&pictures_dir);
    let started = std::time::SystemTime::now();

    // Claude Code / Codex: PDFflow's tools through the stdio bridge to this editor.
    let mut cli_setup = None;
    if let Some(w) = which {
        let addr = setup.mcp_addr.clone().ok_or("Claude Code and Codex reach PDFflow through its MCP server, which is off in this window (another PDFflow holds the port, or PDFFLOW_MCP=0). Quit the other PDFflow or choose an API provider.")?;
        let exe = match &setup.exe {
            Some(e) => e.clone(),
            None => std::env::current_exe().map_err(|e| format!("cannot find the PDFflow program: {e}"))?,
        };
        let workdir = setup.work_dir.join("agent");
        std::fs::create_dir_all(&workdir).map_err(|e| format!("{}: {e}", workdir.display()))?;
        let path = if w == Which::ClaudeCode { &st.claude_path } else { &st.codex_path };
        cli_setup = Some(CliSetup { configured_path: path.clone(), subscription: st.subscription, timeout: Duration::from_secs(st.cli_timeout_min.max(1) * 60), workdir, mcp: McpServer::pdfflow(&exe, &addr), images: turn.images.clone() });
    }

    let mut cfg = BaseConfig { sessions_dir: Some(setup.sessions_dir.clone()), pin_facts: false, ..Default::default() };
    if !st.subagents {
        cfg.disabled.extend(["hive-subagent".to_string(), "hive-team-board".to_string()]);
    }
    if which.is_some() {
        // the program keeps its own context and loop: one run per message
        cfg.disabled.extend(["hive-compaction", "hive-continuation-guard", "hive-repeat-tool-reminder"].map(String::from));
    }
    let mut plugins = base_bundle(&cfg);
    plugins.push(Arc::new(PdfflowPlugin { editor: setup.editor.clone(), vision, in_process: which.is_none(), pictures_dir: pictures_dir.clone() }));
    if let Some(cs) = cli_setup.clone() {
        plugins.push(fn_plugin("pdfflow-cli-models", vec![keys::LLM, keys::AGENTS], move |ctx| {
            let llm = ctx.expect::<LlmService>(keys::LLM)?;
            ctx.effect(llm.register(Arc::new(CliAdapter { ctx: ctx.clone(), setup: cs.clone() })));
            Ok(())
        }));
    }
    if let Some(a) = setup.adapter.clone() {
        plugins.push(fn_plugin("pdfflow-given-model", vec![keys::LLM], move |ctx| {
            let llm = ctx.expect::<LlmService>(keys::LLM)?;
            ctx.effect(llm.register(a.clone()));
            Ok(())
        }));
    }
    let hive = th_hive::Hive::boot(plugins);
    for (name, status, _) in hive.ctx.plugins() {
        if let th_hive::PluginStatus::Failed(e) = status {
            return Err(format!("the agent harness could not start {name}: {e}"));
        }
    }
    let store = hive.sessions();
    let session = if store.path_of(session_id).is_some_and(|p| p.is_file()) {
        store.open(session_id)?
    } else {
        store.create(SessionMeta { id: session_id.into(), agent: AGENT_ID.into(), label: "PDFflow chat".into(), parent: None, depth: 0 })?
    };
    let preset = AgentPreset {
        id: AGENT_ID.into(),
        name: "PDFflow Assistant".into(),
        role: "reviews and edits PDF documents in PDFflow".into(),
        provider: provider.clone(),
        model: if which.is_some() { st.current().model.trim().to_string() } else { st.model() },
        temperature: if which.is_some() { None } else { Some(0.2) },
        max_tokens: if which.is_some() { None } else { Some(8192) },
        reasoning: None,
        max_steps: st.max_steps.max(4),
        time_limit: None,
        fail_limit: 3,
        tools: ToolFilter::All,
        meta: json!({}),
    };
    if st.subagents {
        if let Some(subs) = hive.ctx.get::<Subagents>(keys::SUBAGENTS) {
            let helper = AgentPreset { id: "helper".into(), name: "Helper".into(), role: "a second PDF reviewer for a self-contained part of the work (a chapter, a range of pages, the references); give it the pages and what to check".into(), max_steps: 30, ..preset.clone() };
            subs.source(&hive.ctx, move |a| if a.depth == 0 { vec![Arc::new(ChildAgent { preset: helper.clone() }) as Arc<dyn SubagentProvider>] } else { vec![] });
        }
    }

    // the session's events and the model's stream go to the chat
    let sid = session_id.to_string();
    let t2 = tx.clone();
    let bus_off = store.bus.on(move |ap| {
        if ap.session != sid {
            return;
        }
        let ts = ap.event.ts;
        let ev = match &ap.event.body {
            EventBody::AssistantMessage { text, reasoning, usage, model, .. } => Event::Assistant { text: text.clone(), reasoning: reasoning.clone(), prompt: usage.prompt, completion: usage.completion, cached: usage.cached, model: model.clone() },
            EventBody::ToolCall { id, name, arguments } => Event::ToolCall { id: id.clone(), name: name.clone(), args: arguments.clone(), ts },
            EventBody::ToolResult { id, text, is_error, meta, .. } => Event::ToolResult { id: id.clone(), text: text.clone(), is_error: *is_error, pictures: pictures_of(meta), ts },
            EventBody::TodoWrite { todos } => Event::Todo(todos.clone()),
            EventBody::Custom { kind, data } if kind == cli::COST_KIND => Event::Cost(data["usd"].as_f64().unwrap_or(0.0)),
            EventBody::CompactionStart { .. } => Event::Note("Compacting the conversation…".into()),
            _ => return,
        };
        let _ = t2.send(ev);
    });
    let sid = session_id.to_string();
    let t3 = tx.clone();
    let stream_off = hive.agents().stream.on(move |ev| {
        if ev.agent != sid {
            return;
        }
        let e = match &ev.frame {
            StreamFrame::Start => Event::Step,
            StreamFrame::Text(t) => Event::Text(t.clone()),
            StreamFrame::Reasoning(t) => Event::Reasoning(t.clone()),
            _ => return,
        };
        let _ = t3.send(e);
    });

    let mut text = prompt::compose(&turn.text, &turn.document, &turn.attachments);
    let mut images = vec![];
    if !turn.images.is_empty() {
        if which.is_some() {
            let list: Vec<String> = turn.images.iter().map(|p| p.display().to_string()).collect();
            text.push_str(&format!("\nAttached images (read them): {}", list.join(", ")));
        } else if vision {
            images = turn.images.clone();
        } else {
            let _ = tx.send(Event::Note(format!("{} takes text only: the {} attached image(s) went as file names, not pictures. Choose a model that takes images, or set Images to Yes in Settings if it does.", st.model(), turn.images.len())));
            text.push_str(&format!("\n[{} image(s) were attached, but you cannot see them: say so]", turn.images.len()));
        }
    }
    let agent = hive.agent(preset, session, cancel.clone());
    let r = hive.send(&agent, Inbound { text, images, source: "user".into() });
    bus_off();
    stream_off();
    hive.ctx.dispose();
    let out = r?;
    let pictures = if which.is_some() { new_files(&pictures_dir, started) } else { vec![] };
    if out.reason == "cancelled" {
        return Ok(Outcome { reply: "Stopped. The changes made so far stay in the document; Undo takes them back.".into(), reason: out.reason, pictures });
    }
    let reply = if out.text.trim().is_empty() { "The agent gave no answer.".to_string() } else { out.text };
    Ok(Outcome { reply, reason: out.reason, pictures })
}

fn new_files(dir: &Path, since: std::time::SystemTime) -> Vec<PathBuf> {
    let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut v: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let p = e.path();
                    let m = mtime(&p)?;
                    (m >= since && p.extension().is_some_and(|x| x == "png")).then_some((m, p))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v.into_iter().map(|(_, p)| p).take(6).collect()
}

pub fn pictures_of(meta: &Value) -> Vec<PathBuf> {
    meta["pictures"].as_array().map(|a| a.iter().filter_map(|p| p.as_str().map(PathBuf::from)).collect()).unwrap_or_default()
}

/// PDFflow's tools and working rules on the hive.
struct PdfflowPlugin {
    editor: Editor,
    vision: bool,
    /// The tools run here (an API model); off when Claude Code / Codex get them over MCP.
    in_process: bool,
    pictures_dir: PathBuf,
}

impl Plugin for PdfflowPlugin {
    fn name(&self) -> String {
        "pdfflow".into()
    }
    fn describe(&self) -> String {
        "PDFflow's tools on the document in the window, and the working rules".into()
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::TOOLS, keys::PROMPT, keys::LOOP]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let tools = ctx.expect::<ToolRegistry>(keys::TOOLS)?;
        let sp = ctx.expect::<SystemPrompt>(keys::PROMPT)?;
        ctx.effect(sp.section(10, "pdfflow-rules", |_, _| Some(prompt::RULES.to_string())));
        ctx.effect(sp.section(20, "pdfflow-tools", |_, _| Some(format!("# PDFflow\n{}", crate::mcp::INSTRUCTIONS))));
        if !self.in_process {
            return Ok(());
        }
        for t in crate::mcp_tools::list() {
            let name = t["name"].as_str().unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let (editor, dir, vision, n2) = (self.editor.clone(), self.pictures_dir.clone(), self.vision, name.clone());
            let def = ToolDef::new(&name, t["description"].as_str().unwrap_or(""), t["inputSchema"].clone(), move |tc, args: Value| {
                let args = if args.is_object() { args } else { json!({}) };
                crate::mcp_tools::check(&n2, &args)?;
                let mut v = editor(&n2, args)?;
                // A picture of the page goes to the chat (and to a vision model next step).
                let mut pictures = vec![];
                if let Some(png) = v.as_object_mut().and_then(|o| o.remove("image_png")) {
                    if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(png.as_str().unwrap_or("")) {
                        let safe: String = tc.call_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
                        let f = dir.join(format!("{}-{n2}-{safe}.png", th_hive::session::now_ms()));
                        if std::fs::write(&f, bytes).is_ok() {
                            pictures.push(f);
                        }
                    }
                }
                if vision && !pictures.is_empty() {
                    let p2 = pictures.clone();
                    tc.agent.with_state::<Vec<PathBuf>, _>(PICS, move |v| v.extend(p2));
                }
                let mut text = match &v {
                    Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_default(),
                };
                if !pictures.is_empty() && !vision {
                    text.push_str("\n(The picture is shown to the user in the chat; this model cannot see images, so read the page with get_text_lines instead.)");
                }
                Ok(ToolOutput { text, meta: json!({"pictures": pictures}), ..Default::default() })
            });
            let _ = tools.register(ctx, def);
        }
        // the pictures of a step's tools go to a vision model with its next request
        let lp = ctx.expect::<AgentLoop>(keys::LOOP)?;
        ctx.effect(lp.pre_step.on(false, |p, next| {
            let pics: Vec<PathBuf> = p.agent.with_state::<Vec<PathBuf>, _>(PICS, std::mem::take);
            if !pics.is_empty() {
                let n = pics.len();
                p.messages.push(Inbound { text: format!("The page picture{} your tools made: look at {} and check the document.", if n > 1 { "s" } else { "" }, if n > 1 { "them" } else { "it" }), images: pics, source: "plugin:pdfflow-pictures".into() });
            }
            next(p)
        }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use th_llm::{CallError, Delta, FunctionCall, Message, Provider, Reply, ToolCall, Usage};

    /// A model that calls add_text once, then answers.
    struct Scripted {
        n: AtomicUsize,
    }

    impl LlmAdapter for Scripted {
        fn id(&self) -> &str {
            "scripted"
        }
        fn serves(&self, _: &Provider) -> bool {
            true
        }
        fn stream(&self, req: &th_hive::LlmRequest, on: &mut dyn FnMut(Delta), _: &AtomicBool) -> Result<Reply, CallError> {
            let usage = Usage { prompt_tokens: 100, completion_tokens: 10, cached_tokens: 0 };
            if self.n.fetch_add(1, Ordering::SeqCst) == 0 {
                assert!(req.system.contains("PDFflow") && req.tools.iter().any(|t| t.name == "replace_text"));
                let mut m = Message::assistant("");
                m.tool_calls = Some(vec![ToolCall { id: "c1".into(), kind: "function".into(), function: FunctionCall { name: "add_text".into(), arguments: r#"{"page":1,"x":72,"y":72,"text":"Checked"}"#.into() } }]);
                return Ok(Reply { message: m, reasoning: None, finish_reason: "tool_calls".into(), usage });
            }
            on(Delta::Text("Added the note on page 1.".into()));
            Ok(Reply { message: Message::assistant("Added the note on page 1."), reasoning: None, finish_reason: "stop".into(), usage })
        }
    }

    #[test]
    fn a_turn_runs_the_tools_in_the_editor() {
        let dir = std::env::temp_dir().join(format!("pdfflow-engine-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let seen: Arc<Mutex<Vec<(String, Value)>>> = Default::default();
        let s2 = seen.clone();
        let editor: Editor = Arc::new(move |name: &str, args: Value| {
            s2.lock().unwrap().push((name.to_owned(), args));
            Ok(json!({"page": 1, "index": 0}))
        });
        let setup = Setup { settings: AgentSettings::default(), keys: Keys::default(), sessions_dir: dir.join("sessions"), work_dir: dir.clone(), editor, mcp_addr: None, exe: None, adapter: Some(Arc::new(Scripted { n: AtomicUsize::new(0) })) };
        let (tx, rx) = std::sync::mpsc::channel();
        let out = run_turn(&setup, "chat-test", Turn { text: "Mark page 1 as checked".into(), document: "[document 1]".into(), ..Default::default() }, &Arc::new(AtomicBool::new(false)), &tx).unwrap();
        assert_eq!(out.reply, "Added the note on page 1.");
        assert_eq!(seen.lock().unwrap()[0].0, "add_text");
        assert_eq!(seen.lock().unwrap()[0].1["text"], "Checked");
        let events: Vec<Event> = rx.try_iter().collect();
        assert!(events.iter().any(|e| matches!(e, Event::ToolResult { id, is_error: false, .. } if id == "c1")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
