//! What the chat shows: the user's messages, the agent's replies (streamed, then whole),
//! one row per tool call with its result and duration, the todo list, and a token line per
//! message, built live from `Event`s.

use std::path::PathBuf;

use th_hive::session::TodoItem;

use super::engine::Event;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub prompt: u64,
    pub completion: u64,
    pub cached: u64,
    pub cost: Option<f64>,
    pub model: String,
    pub steps: u32,
}

impl Stats {
    /// "12.3k in · 1.1k out · 9.0k cached · $0.012 · 4 steps"
    pub fn line(&self) -> String {
        let k = |n: u64| if n >= 1000 { format!("{:.1}k", n as f64 / 1000.0) } else { n.to_string() };
        let mut v = vec![format!("{} in", k(self.prompt)), format!("{} out", k(self.completion))];
        if self.cached > 0 {
            v.push(format!("{} cached", k(self.cached)));
        }
        if let Some(c) = self.cost {
            v.push(format!("${c:.3}"));
        }
        if self.steps > 1 {
            v.push(format!("{} steps", self.steps));
        }
        v.join(" · ")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    User { text: String, files: Vec<PathBuf> },
    Assistant { text: String, reasoning: String, streaming: bool, pictures: Vec<PathBuf>, stats: Option<Stats> },
    Tool { id: String, name: String, args: String, result: Option<String>, is_error: bool, pictures: Vec<PathBuf>, started: u64, ms: Option<u64> },
    Todo(Vec<TodoItem>),
    Note(String),
}

#[derive(Debug, Clone, Default)]
pub struct Transcript {
    pub entries: Vec<Entry>,
    streaming: Option<usize>,
    todo_at: Option<usize>,
    turn: Stats,
}

/// One line of a tool's arguments: `level: Level 1, points: [[0,0],[4,0],…]`.
pub fn one_line_args(args: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(args).unwrap_or(serde_json::Value::Null);
    let s = match v.as_object() {
        Some(o) if !o.is_empty() => o.iter().map(|(k, v)| format!("{k}: {}", match v {
            serde_json::Value::String(s) => s.clone(),
            v => v.to_string(),
        })).collect::<Vec<_>>().join(", "),
        Some(_) => String::new(),
        None => args.trim().to_string(),
    };
    th_hive::clip(&s.replace('\n', " "), 140)
}

impl Transcript {
    pub fn begin_turn(&mut self) {
        self.streaming = None;
        self.todo_at = None;
        self.turn = Stats::default();
    }

    /// The user sent a message.
    pub fn push_user(&mut self, text: &str, files: Vec<PathBuf>) {
        self.entries.push(Entry::User { text: text.into(), files });
        self.begin_turn();
    }

    fn remove(&mut self, i: usize) {
        self.entries.remove(i);
        if let Some(t) = self.todo_at.as_mut() {
            if *t > i {
                *t -= 1;
            }
        }
    }

    pub fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Step => self.turn.steps += 1,
            Event::Text(s) | Event::Reasoning(s) => {
                let i = match self.streaming {
                    Some(i) => i,
                    None => {
                        self.entries.push(Entry::Assistant { text: String::new(), reasoning: String::new(), streaming: true, pictures: vec![], stats: None });
                        self.streaming = Some(self.entries.len() - 1);
                        self.entries.len() - 1
                    }
                };
                if let Some(Entry::Assistant { text, reasoning, .. }) = self.entries.get_mut(i) {
                    if matches!(ev, Event::Text(_)) {
                        text.push_str(s)
                    } else {
                        reasoning.push_str(s)
                    }
                }
            }
            Event::Assistant { text, reasoning, prompt, completion, cached, model } => {
                let mut kept_reasoning = reasoning.clone().unwrap_or_default();
                if let Some(i) = self.streaming.take() {
                    if let Some(Entry::Assistant { reasoning: r, .. }) = self.entries.get(i) {
                        if kept_reasoning.is_empty() {
                            kept_reasoning = r.clone();
                        }
                    }
                    self.remove(i);
                }
                self.turn.prompt += prompt;
                self.turn.completion += completion;
                self.turn.cached += cached;
                if !model.is_empty() {
                    self.turn.model = model.clone();
                }
                if !text.trim().is_empty() {
                    self.entries.push(Entry::Assistant { text: text.trim().to_string(), reasoning: kept_reasoning, streaming: false, pictures: vec![], stats: None });
                }
            }
            Event::ToolCall { id, name, args, ts } => {
                if name == "todo_write" {
                    return;
                }
                self.entries.push(Entry::Tool { id: id.clone(), name: name.clone(), args: args.clone(), result: None, is_error: false, pictures: vec![], started: *ts, ms: None });
            }
            Event::ToolResult { id: rid, text, is_error: err, pictures: pics, ts, .. } => {
                for e in self.entries.iter_mut().rev() {
                    if let Entry::Tool { id, result, is_error, pictures, started, ms, .. } = e {
                        if id == rid {
                            *result = Some(text.clone());
                            *is_error = *err;
                            *pictures = pics.clone();
                            *ms = Some(ts.saturating_sub(*started));
                            break;
                        }
                    }
                }
            }
            Event::Todo(items) => match self.todo_at {
                Some(i) if i < self.entries.len() => self.entries[i] = Entry::Todo(items.clone()),
                _ => {
                    self.entries.push(Entry::Todo(items.clone()));
                    self.todo_at = Some(self.entries.len() - 1);
                }
            },
            Event::Cost(c) => self.turn.cost = Some(self.turn.cost.unwrap_or(0.0) + c),
            Event::Note(n) => self.entries.push(Entry::Note(n.clone())),
            Event::Done(_) => {}
        }
    }

    /// The message ended: the token line goes under its last reply; `reply` is added when the
    /// agent's answer is not in the transcript yet (an error, a stop, a stuck turn's note).
    pub fn end_turn(&mut self, reply: Option<&str>, pictures: Vec<PathBuf>) {
        if let Some(i) = self.streaming.take() {
            self.remove(i);
        }
        let last_text = self.entries.iter().rev().take_while(|e| !matches!(e, Entry::User { .. })).find_map(|e| if let Entry::Assistant { text, .. } = e { Some(text.clone()) } else { None });
        if let Some(r) = reply.map(str::trim).filter(|r| !r.is_empty()) {
            if last_text.as_deref() != Some(r) && !last_text.as_deref().is_some_and(|l| r.starts_with(l)) {
                self.entries.push(Entry::Assistant { text: r.to_string(), reasoning: String::new(), streaming: false, pictures: vec![], stats: None });
            } else if let Some(l) = &last_text {
                if r.len() > l.len() {
                    // a note added to the answer ("stopped after 60 steps")
                    if let Some(Entry::Assistant { text, .. }) = self.entries.iter_mut().rev().find(|e| matches!(e, Entry::Assistant { .. })) {
                        *text = r.to_string();
                    }
                }
            }
        }
        let stats = self.turn.clone();
        let has_tokens = stats.prompt + stats.completion > 0 || stats.cost.is_some();
        for e in self.entries.iter_mut().rev() {
            match e {
                Entry::User { .. } => break,
                Entry::Assistant { stats: s, pictures: p, .. } => {
                    if has_tokens {
                        *s = Some(stats);
                    }
                    p.extend(pictures);
                    break;
                }
                _ => {}
            }
        }
        self.turn = Stats::default();
        self.todo_at = None;
    }

    pub fn clear(&mut self) {
        *self = Transcript::default();
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_text_gives_way_to_the_whole_message_after_the_tools() {
        let mut t = Transcript::default();
        t.push_user("mark the units", vec![]);
        t.apply(&Event::Step);
        t.apply(&Event::Text("I'll add".into()));
        t.apply(&Event::Assistant { text: String::new(), reasoning: None, prompt: 100, completion: 10, cached: 50, model: "m".into() });
        t.apply(&Event::ToolCall { id: "c1".into(), name: "markup_text".into(), args: r#"{"style":"highlight"}"#.into(), ts: 1000 });
        t.apply(&Event::Todo(vec![TodoItem { content: "units".into(), status: "in_progress".into() }]));
        t.apply(&Event::ToolResult { id: "c1".into(), text: "ok".into(), is_error: false, pictures: vec![], ts: 1250 });
        t.apply(&Event::Todo(vec![TodoItem { content: "units".into(), status: "completed".into() }]));
        t.apply(&Event::Step);
        t.apply(&Event::Text("Done".into()));
        t.apply(&Event::Assistant { text: "Done: 4 marks.".into(), reasoning: None, prompt: 200, completion: 20, cached: 150, model: "m".into() });
        t.end_turn(Some("Done: 4 marks."), vec![]);
        let kinds: Vec<&str> = t.entries.iter().map(|e| match e {
            Entry::User { .. } => "user",
            Entry::Assistant { .. } => "assistant",
            Entry::Tool { .. } => "tool",
            Entry::Todo(_) => "todo",
            Entry::Note(_) => "note",
        }).collect();
        assert_eq!(kinds, ["user", "tool", "todo", "assistant"]);
        match &t.entries[1] {
            Entry::Tool { ms, result, .. } => assert_eq!((*ms, result.as_deref()), (Some(250), Some("ok"))),
            _ => unreachable!(),
        }
        match &t.entries[2] {
            Entry::Todo(items) => assert_eq!(items[0].status, "completed"),
            _ => unreachable!(),
        }
        match &t.entries[3] {
            Entry::Assistant { stats: Some(s), .. } => assert_eq!(s.line(), "300 in · 30 out · 200 cached · 2 steps"),
            e => panic!("{e:?}"),
        }
        assert_eq!(one_line_args(r#"{"page":1,"rect":[0,0,4,0]}"#), "page: 1, rect: [0,0,4,0]");
    }
}
