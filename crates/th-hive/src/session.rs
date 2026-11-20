//! The append-only session log: the source of everything the model sees.
//!
//! As in DeepSeek Harness, "model-visible means logged": every request is derived from the
//! log by [`Session::derive_messages`], so a session can be replayed, resumed, forked or
//! audited from its JSONL file. Plugins add durable facts with `custom` events, and the
//! two plugin-owned projections (`tool/prune` and a `user/message` that replaces a surface
//! range) are applied while deriving.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use th_llm::{Content, FunctionCall, Message, Part, ToolCall};

use crate::events::Emit;
use crate::facts::{self, FactRecord};

pub type Seq = u64;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    pub prompt: u64,
    pub completion: u64,
    #[serde(default)]
    pub cached: u64,
}

/// Replaces the surface nodes from `start` through `end` (by position) with this message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SurfaceOp {
    pub start: Seq,
    pub end: Seq,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoItem {
    pub content: String,
    /// pending | in_progress | completed
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CallRecord {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum EventBody {
    #[serde(rename = "session/start")]
    SessionStart { id: String, agent: String, label: String, parent: Option<String>, depth: usize },
    #[serde(rename = "turn/start")]
    TurnStart { turn: u64 },
    #[serde(rename = "turn/end")]
    TurnEnd { turn: u64, reason: String },
    #[serde(rename = "step/start")]
    StepStart { turn: u64, step: u64 },
    #[serde(rename = "step/end")]
    StepEnd { turn: u64, step: u64 },
    #[serde(rename = "system/message")]
    SystemMessage { text: String },
    /// `source` is "user" for the person, "parent" for a delegated task, or "plugin:<name>"
    /// for injected context.
    #[serde(rename = "user/message")]
    UserMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<PathBuf>,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        surface_op: Option<SurfaceOp>,
    },
    #[serde(rename = "assistant/message")]
    AssistantMessage {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        calls: Vec<CallRecord>,
        finish: String,
        provider: String,
        model: String,
        usage: Usage,
    },
    /// A model call that failed or was cancelled; kept for the record, not for the model.
    #[serde(rename = "assistant/attempt")]
    AssistantAttempt { error: String, provider: String, model: String },
    #[serde(rename = "tool/call")]
    ToolCall { id: String, name: String, arguments: String },
    #[serde(rename = "tool/result")]
    ToolResult {
        id: String,
        name: String,
        text: String,
        is_error: bool,
        #[serde(default, skip_serializing_if = "Value::is_null")]
        meta: Value,
    },
    /// Older tool results whose bodies the model no longer sees (compaction pruning).
    #[serde(rename = "tool/prune")]
    ToolPrune { seqs: Vec<Seq> },
    #[serde(rename = "todo/write")]
    TodoWrite { todos: Vec<TodoItem> },
    #[serde(rename = "compaction/start")]
    CompactionStart { turn: Option<u64> },
    #[serde(rename = "compaction/summary")]
    CompactionSummary { summary: String, shadowed: Vec<Seq>, shadowed_tokens: usize, provider: String, model: String },
    #[serde(rename = "compaction/end")]
    CompactionEnd {
        turn: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// A plugin's own durable fact (DeepSeek Harness extends `SessionEventMap` the same way).
    #[serde(rename = "custom")]
    Custom { kind: String, data: Value },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionEvent {
    pub seq: Seq,
    /// Milliseconds since the Unix epoch.
    pub ts: u64,
    #[serde(flatten)]
    pub body: EventBody,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SessionMeta {
    pub id: String,
    pub agent: String,
    pub label: String,
    pub parent: Option<String>,
    pub depth: usize,
}

/// A notification of one appended event (`session/event`).
#[derive(Debug, Clone)]
pub struct Appended {
    pub session: String,
    pub agent: String,
    pub event: SessionEvent,
}

pub struct Session {
    pub meta: SessionMeta,
    events: RwLock<Vec<SessionEvent>>,
    file: Option<Mutex<File>>,
    pub path: Option<PathBuf>,
    bus: Arc<Emit<Appended>>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// How derived history is shaped for one provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeriveOptions {
    /// Send `reasoning_content` back for the assistant messages of the open turn
    /// (DeepSeek thinking mode with tool calls).
    pub reasoning_passback: bool,
}

/// One message-producing node of the surface.
#[derive(Debug, Clone)]
enum Node {
    User { text: String, images: Vec<PathBuf> },
    Assistant { text: String, reasoning: Option<String>, calls: Vec<CallRecord>, turn: u64 },
    Result { id: String, text: String },
}

impl Session {
    /// A session kept in memory only.
    pub fn memory(meta: SessionMeta, bus: Arc<Emit<Appended>>) -> Arc<Session> {
        let s = Arc::new(Session { meta: meta.clone(), events: RwLock::new(vec![]), file: None, path: None, bus });
        s.append(EventBody::SessionStart { id: meta.id, agent: meta.agent, label: meta.label, parent: meta.parent, depth: meta.depth });
        s
    }

    /// Creates a session file (JSONL) and writes its `session/start`.
    pub fn create(path: &Path, meta: SessionMeta, bus: Arc<Emit<Appended>>) -> Result<Arc<Session>, String> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
        }
        let f = OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let s = Arc::new(Session { meta: meta.clone(), events: RwLock::new(vec![]), file: Some(Mutex::new(f)), path: Some(path.to_path_buf()), bus });
        s.append(EventBody::SessionStart { id: meta.id, agent: meta.agent, label: meta.label, parent: meta.parent, depth: meta.depth });
        Ok(s)
    }

    /// Opens a session file. An interrupted tail (a turn without `turn/end`) is sealed so
    /// the next turn starts clean.
    pub fn open(path: &Path, bus: Arc<Emit<Appended>>) -> Result<Arc<Session>, String> {
        let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut events = vec![];
        for line in BufReader::new(f).lines() {
            let line = line.map_err(|e| e.to_string())?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<SessionEvent>(&line) {
                Ok(e) => events.push(e),
                Err(e) => return Err(format!("{}: line {}: {e}", path.display(), events.len() + 1)),
            }
        }
        let meta = match events.first().map(|e| &e.body) {
            Some(EventBody::SessionStart { id, agent, label, parent, depth }) => SessionMeta { id: id.clone(), agent: agent.clone(), label: label.clone(), parent: parent.clone(), depth: *depth },
            _ => return Err(format!("{}: no session/start", path.display())),
        };
        let f = OpenOptions::new().append(true).open(path).map_err(|e| e.to_string())?;
        let s = Arc::new(Session { meta, events: RwLock::new(events), file: Some(Mutex::new(f)), path: Some(path.to_path_buf()), bus });
        if let Some(turn) = s.open_turn() {
            s.append(EventBody::TurnEnd { turn, reason: "interrupted".into() });
        }
        Ok(s)
    }

    pub fn id(&self) -> &str {
        &self.meta.id
    }

    /// Appends one durable event, writes it to the file and notifies `session/event`.
    pub fn append(&self, body: EventBody) -> Seq {
        let ev = {
            let Ok(mut evs) = self.events.write() else { return 0 };
            let seq = evs.last().map(|e| e.seq + 1).unwrap_or(1);
            let ev = SessionEvent { seq, ts: now_ms(), body };
            if let Some(f) = &self.file {
                if let (Ok(mut f), Ok(line)) = (f.lock(), serde_json::to_string(&ev)) {
                    let _ = writeln!(f, "{line}");
                }
            }
            evs.push(ev.clone());
            ev
        };
        let seq = ev.seq;
        self.bus.emit(&Appended { session: self.meta.id.clone(), agent: self.meta.agent.clone(), event: ev });
        seq
    }

    pub fn events(&self) -> Vec<SessionEvent> {
        self.events.read().map(|e| e.clone()).unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.events.read().map(|e| e.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The turn that started and has not ended.
    pub fn open_turn(&self) -> Option<u64> {
        let evs = self.events.read().ok()?;
        let mut open = None;
        for e in evs.iter() {
            match &e.body {
                EventBody::TurnStart { turn } => open = Some(*turn),
                EventBody::TurnEnd { .. } => open = None,
                _ => {}
            }
        }
        open
    }

    pub fn next_turn(&self) -> u64 {
        self.events
            .read()
            .map(|e| e.iter().filter_map(|e| if let EventBody::TurnStart { turn } = e.body { Some(turn) } else { None }).max().unwrap_or(0) + 1)
            .unwrap_or(1)
    }

    /// The latest system prompt in the log.
    pub fn system_prompt(&self) -> Option<String> {
        let evs = self.events.read().ok()?;
        evs.iter().rev().find_map(|e| if let EventBody::SystemMessage { text } = &e.body { Some(text.clone()) } else { None })
    }

    /// The latest todo list (`todo/write` replays as last-write-wins).
    pub fn todos(&self) -> Vec<TodoItem> {
        self.events.read().ok().and_then(|evs| evs.iter().rev().find_map(|e| if let EventBody::TodoWrite { todos } = &e.body { Some(todos.clone()) } else { None })).unwrap_or_default()
    }

    /// The latest custom event of a kind.
    pub fn last_custom(&self, kind: &str) -> Option<Value> {
        let evs = self.events.read().ok()?;
        evs.iter().rev().find_map(|e| match &e.body {
            EventBody::Custom { kind: k, data } if k == kind => Some(data.clone()),
            _ => None,
        })
    }

    /// Every pinned fact set in the log, oldest first (`custom` events of kind `facts/set`).
    pub fn fact_records(&self) -> Vec<FactRecord> {
        let Ok(evs) = self.events.read() else { return vec![] };
        evs.iter()
            .filter_map(|e| match &e.body {
                EventBody::Custom { kind, data } if kind == facts::KIND => serde_json::from_value(data.clone()).ok(),
                _ => None,
            })
            .collect()
    }

    /// The surface: message-producing nodes after replacements and pruning, with their seqs.
    fn surface(&self) -> Vec<(Seq, Node)> {
        let evs = self.events.read().map(|e| e.clone()).unwrap_or_default();
        let mut surface: Vec<(Seq, Node)> = vec![];
        let mut pruned: std::collections::HashSet<Seq> = Default::default();
        let mut turn = 0;
        for e in &evs {
            match &e.body {
                EventBody::TurnStart { turn: t } => turn = *t,
                EventBody::UserMessage { text, images, surface_op, .. } => {
                    let node = (e.seq, Node::User { text: text.clone(), images: images.clone() });
                    match surface_op {
                        Some(op) => {
                            let a = surface.iter().position(|(s, _)| *s == op.start);
                            let b = surface.iter().position(|(s, _)| *s == op.end);
                            match (a, b) {
                                (Some(a), Some(b)) if a <= b => {
                                    surface.splice(a..=b, [node]);
                                }
                                _ => surface.push(node),
                            }
                        }
                        None => surface.push(node),
                    }
                }
                EventBody::AssistantMessage { text, reasoning, calls, .. } => {
                    surface.push((e.seq, Node::Assistant { text: text.clone(), reasoning: reasoning.clone(), calls: calls.clone(), turn }));
                }
                EventBody::ToolResult { id, text, .. } => surface.push((e.seq, Node::Result { id: id.clone(), text: text.clone() })),
                EventBody::ToolPrune { seqs } => pruned.extend(seqs.iter().copied()),
                _ => {}
            }
        }
        // a pruned result keeps the values it pinned, unless a later run replaced them
        let records = if pruned.is_empty() { vec![] } else { self.fact_records() };
        for (seq, n) in surface.iter_mut() {
            if let Node::Result { text, .. } = n {
                if pruned.contains(seq) {
                    *text = format!("{}\n…[older tool output removed to save context]", crate::clip(text, 400));
                    for r in records.iter().filter(|r| r.source == *seq) {
                        if facts::superseded(&records, r) {
                            text.push_str(&format!("\n[Its values were replaced by a later run of {}]", r.set.group));
                        } else {
                            text.push_str(&format!("\n[Pinned values, copied exactly from this output]\n{}", facts::render(r)));
                        }
                    }
                }
            }
        }
        surface
    }

    /// Surface seqs with a rough size, oldest first (for compaction).
    pub fn surface_sizes(&self) -> Vec<(Seq, &'static str, usize)> {
        self.surface()
            .into_iter()
            .map(|(s, n)| match n {
                Node::User { text, images } => (s, "user", text.len() + images.len() * 3000),
                Node::Assistant { text, calls, .. } => (s, if calls.is_empty() { "assistant" } else { "assistant-calls" }, text.len() + calls.iter().map(|c| c.arguments.len() + c.name.len()).sum::<usize>()),
                Node::Result { text, .. } => (s, "result", text.len()),
            })
            .collect()
    }

    /// The surface from its start through the node `upto`, as a plain transcript (the
    /// input of a compaction summary).
    pub fn render_surface(&self, upto: Seq, max_result_chars: usize) -> String {
        let mut out = String::new();
        for (seq, n) in self.surface() {
            match n {
                Node::User { text, images } => {
                    out.push_str(&format!("USER: {text}\n"));
                    for p in images {
                        out.push_str(&format!("[image {}]\n", p.display()));
                    }
                }
                Node::Assistant { text, calls, .. } => {
                    if !text.trim().is_empty() {
                        out.push_str(&format!("ASSISTANT: {text}\n"));
                    }
                    for c in calls {
                        out.push_str(&format!("ASSISTANT CALLED {}({})\n", c.name, crate::clip(&c.arguments, 1500)));
                    }
                }
                Node::Result { text, .. } => out.push_str(&format!("RESULT: {}\n", crate::clip(&text, max_result_chars))),
            }
            out.push('\n');
            if seq == upto {
                break;
            }
        }
        out
    }

    /// The model history (without the system prompt) derived from the log. Tool results
    /// follow the assistant message that asked for them; a call without a result (an
    /// interrupted step) gets a synthetic error result so the history stays valid.
    pub fn derive_messages(&self, opt: DeriveOptions) -> Vec<Message> {
        let surface = self.surface();
        let open = self.open_turn();
        let mut results: HashMap<String, String> = HashMap::new();
        for (_, n) in &surface {
            if let Node::Result { id, text } = n {
                results.insert(id.clone(), text.clone());
            }
        }
        let mut out = vec![];
        for (_, n) in surface {
            match n {
                Node::User { text, images } => {
                    if images.is_empty() {
                        out.push(Message::user(text));
                    } else {
                        let mut parts = vec![Part::Text { text }];
                        for p in &images {
                            match th_llm::image_part(p) {
                                Ok(part) => parts.push(part),
                                Err(e) => parts.push(Part::Text { text: format!("[image {} could not be read: {e}]", p.display()) }),
                            }
                        }
                        out.push(Message::user_parts(parts));
                    }
                }
                Node::Assistant { text, reasoning, calls, turn } => {
                    if text.trim().is_empty() && calls.is_empty() {
                        continue;
                    }
                    let mut m = Message::assistant(text.clone());
                    if text.is_empty() {
                        m.content = None;
                    }
                    if !calls.is_empty() {
                        m.tool_calls = Some(calls.iter().map(|c| ToolCall { id: c.id.clone(), kind: "function".into(), function: FunctionCall { name: c.name.clone(), arguments: c.arguments.clone() } }).collect());
                    }
                    if opt.reasoning_passback && Some(turn) == open {
                        m.reasoning_content = reasoning.clone();
                    }
                    out.push(m);
                    for c in &calls {
                        let r = results.remove(&c.id).unwrap_or_else(|| "ERROR: no result (the step was interrupted before this call finished)".into());
                        out.push(Message::tool(&c.id, r));
                    }
                }
                Node::Result { .. } => {}
            }
        }
        out
    }

    /// The plain text of the derived history, for size estimates.
    pub fn estimate_chars(&self) -> usize {
        let mut n = self.system_prompt().map(|s| s.len()).unwrap_or(0);
        for m in self.derive_messages(DeriveOptions::default()) {
            n += match &m.content {
                Some(Content::Parts(p)) => p.iter().map(|p| if let Part::Text { text } = p { text.len() } else { 3000 }).sum(),
                Some(c) => c.text().len(),
                None => 0,
            };
            n += m.tool_calls.as_ref().map(|c| c.iter().map(|c| c.function.arguments.len() + 20).sum()).unwrap_or(0);
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s() -> Arc<Session> {
        Session::memory(SessionMeta { id: "s1".into(), agent: "a".into(), ..Default::default() }, Arc::new(Emit::new("session/event")))
    }

    fn user(t: &str) -> EventBody {
        EventBody::UserMessage { text: t.into(), images: vec![], source: "user".into(), surface_op: None }
    }

    fn asst(t: &str, calls: Vec<CallRecord>, reasoning: Option<&str>) -> EventBody {
        EventBody::AssistantMessage { text: t.into(), reasoning: reasoning.map(String::from), calls, finish: "stop".into(), provider: "p".into(), model: "m".into(), usage: Usage::default() }
    }

    #[test]
    fn derives_history_with_results_after_calls() {
        let s = s();
        s.append(EventBody::TurnStart { turn: 1 });
        s.append(user("hi"));
        s.append(asst("", vec![CallRecord { id: "c1".into(), name: "f".into(), arguments: "{}".into() }, CallRecord { id: "c2".into(), name: "g".into(), arguments: "{}".into() }], Some("think")));
        s.append(EventBody::ToolResult { id: "c2".into(), name: "g".into(), text: "two".into(), is_error: false, meta: Value::Null });
        let m = s.derive_messages(DeriveOptions { reasoning_passback: true });
        assert_eq!(m.len(), 4);
        assert_eq!(m[1].reasoning_content.as_deref(), Some("think"));
        assert_eq!(m[2].tool_call_id.as_deref(), Some("c1"));
        assert!(m[2].text().starts_with("ERROR"));
        assert_eq!(m[3].text(), "two");
        s.append(EventBody::TurnEnd { turn: 1, reason: "completed".into() });
        // a closed turn's reasoning is not sent back
        assert!(s.derive_messages(DeriveOptions { reasoning_passback: true })[1].reasoning_content.is_none());
    }

    #[test]
    fn surface_replace_and_prune() {
        let s = s();
        let a = s.append(user("one"));
        s.append(asst("r1", vec![], None));
        let b = s.append(user("two"));
        s.append(asst("r2", vec![CallRecord { id: "c".into(), name: "f".into(), arguments: "{}".into() }], None));
        let r = s.append(EventBody::ToolResult { id: "c".into(), name: "f".into(), text: "x".repeat(2000), is_error: false, meta: Value::Null });
        s.append(EventBody::ToolPrune { seqs: vec![r] });
        let m = s.derive_messages(DeriveOptions::default());
        assert!(m[4].text().contains("removed to save context"));
        s.append(EventBody::UserMessage { text: "summary".into(), images: vec![], source: "plugin:compaction".into(), surface_op: Some(SurfaceOp { start: a, end: b - 0 }) });
        let m = s.derive_messages(DeriveOptions::default());
        assert_eq!(m[0].text(), "summary");
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn pruned_result_keeps_its_pinned_values() {
        use crate::facts::{Fact, FactRecord, FactSet, KIND};
        let s = s();
        s.append(user("design B1"));
        let fact = |source, tool: &str, v: &str| EventBody::Custom {
            kind: KIND.into(),
            data: serde_json::to_value(FactRecord { source, tool: tool.into(), set: FactSet { group: "calcs/B1".into(), origin: "calcs/B1.json".into(), headline: String::new(), facts: vec![Fact::new("result", "M_u", v, "kN*m")] } }).unwrap(),
        };
        s.append(asst("", vec![CallRecord { id: "c1".into(), name: "calc_sheet".into(), arguments: "{}".into() }], None));
        let r1 = s.append(EventBody::ToolResult { id: "c1".into(), name: "calc_sheet".into(), text: format!("{} M_u = 120.5", "x".repeat(2000)), is_error: false, meta: Value::Null });
        s.append(fact(r1, "calc_sheet", "120.5"));
        // while the result is whole, nothing is added to it
        assert!(!s.derive_messages(DeriveOptions::default())[2].text().contains("Pinned values"));
        s.append(EventBody::ToolPrune { seqs: vec![r1] });
        let t = s.derive_messages(DeriveOptions::default())[2].text();
        assert!(t.contains("removed to save context") && t.ends_with("calcs/B1.json [calc_sheet]\n  results: M_u = 120.5 kN*m"), "{t}");
        // a later run replaces the values, and the old pruned result says so
        s.append(asst("", vec![CallRecord { id: "c2".into(), name: "calc_sheet".into(), arguments: "{}".into() }], None));
        let r2 = s.append(EventBody::ToolResult { id: "c2".into(), name: "calc_sheet".into(), text: "M_u = 131.2".into(), is_error: false, meta: Value::Null });
        s.append(fact(r2, "calc_sheet", "131.2"));
        let m = s.derive_messages(DeriveOptions::default());
        assert!(m[2].text().ends_with("[Its values were replaced by a later run of calcs/B1]") && !m[2].text().contains("M_u = 120.5 kN*m"), "{}", m[2].text());
        assert_eq!(m[4].text(), "M_u = 131.2");
    }

    #[test]
    fn file_round_trip_seals_open_turn() {
        let dir = std::env::temp_dir().join(format!("hive_sess_{}", now_ms()));
        let p = dir.join("s.jsonl");
        let bus = Arc::new(Emit::new("session/event"));
        {
            let s = Session::create(&p, SessionMeta { id: "x".into(), agent: "a".into(), ..Default::default() }, bus.clone()).unwrap();
            s.append(EventBody::TurnStart { turn: 1 });
            s.append(user("hello"));
        }
        let s = Session::open(&p, bus).unwrap();
        assert_eq!(s.meta.id, "x");
        assert_eq!(s.open_turn(), None);
        assert_eq!(s.next_turn(), 2);
        assert_eq!(s.derive_messages(DeriveOptions::default())[0].text(), "hello");
        let _ = std::fs::remove_dir_all(dir);
    }
}
