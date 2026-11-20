//! Agents (`ctx.agents`): a live handle on one session with its inbox, preset, status and
//! cancel flag, plus the registry and the live `agent/*` events.

use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde_json::Value;
use th_llm::Provider;

use crate::events::Emit;
use crate::session::Session;

/// Which registered tools an agent may see.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolFilter {
    All,
    Only(Vec<String>),
}

impl ToolFilter {
    pub fn allows(&self, name: &str) -> bool {
        match self {
            ToolFilter::All => true,
            ToolFilter::Only(v) => v.iter().any(|t| t == name || t == "*"),
        }
    }
}

/// An agent composition (DeepSeek Harness preset): who it is, which model it uses and
/// which tools it may see. `meta` carries the application's own data about the agent.
#[derive(Debug, Clone)]
pub struct AgentPreset {
    /// Stable id ("designer").
    pub id: String,
    /// Display name ("Designer").
    pub name: String,
    /// One line for whoever delegates to it.
    pub role: String,
    pub provider: Provider,
    pub model: String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Thinking: "off", "low", "medium", "high"; None = the provider's default.
    pub reasoning: Option<String>,
    /// Steps per turn before the loop stops it.
    pub max_steps: usize,
    /// Wall-clock limit of one delegated task; None = no limit. When it passes the agent is
    /// stopped and its parent gets what it has so far.
    pub time_limit: Option<std::time::Duration>,
    /// Failures of one tool in a row with the same error that end the turn; 0 = never.
    pub fail_limit: usize,
    pub tools: ToolFilter,
    pub meta: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Running,
    Done,
    Failed,
}

/// One message waiting in an agent's inbox.
#[derive(Debug, Clone)]
pub struct Inbound {
    pub text: String,
    pub images: Vec<PathBuf>,
    /// "user", "parent", "plugin:<name>".
    pub source: String,
}

pub struct Agent {
    pub id: String,
    /// Short running number for display.
    pub run: u64,
    pub preset: AgentPreset,
    pub session: Arc<Session>,
    pub depth: usize,
    pub parent: Option<String>,
    /// Cancels this agent and every child that shares it.
    pub cancel: Arc<AtomicBool>,
    inbox: Mutex<VecDeque<Inbound>>,
    injected: Mutex<Vec<Inbound>>,
    status: Mutex<Status>,
    halt: Mutex<Option<String>>,
    state: Mutex<HashMap<&'static str, Box<dyn Any + Send>>>,
}

impl Agent {
    /// Queues a message that wakes the agent (a user follow-up or a task).
    pub fn followup(&self, m: Inbound) {
        if let Ok(mut q) = self.inbox.lock() {
            q.push_back(m);
        }
    }

    /// Adds context for the next request without waking the agent (`agent.inject`).
    pub fn inject(&self, source: &str, text: impl Into<String>) {
        if let Ok(mut q) = self.injected.lock() {
            q.push(Inbound { text: text.into(), images: vec![], source: source.into() });
        }
    }

    pub fn has_pending(&self) -> bool {
        self.inbox.lock().map(|q| !q.is_empty()).unwrap_or(false) || self.injected.lock().map(|q| !q.is_empty()).unwrap_or(false)
    }

    /// Claims the next-step input: injected context plus one queued message.
    pub fn claim(&self) -> Vec<Inbound> {
        let mut out: Vec<Inbound> = self.injected.lock().map(|mut q| q.drain(..).collect()).unwrap_or_default();
        if let Some(m) = self.inbox.lock().ok().and_then(|mut q| q.pop_front()) {
            out.push(m);
        }
        out
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|s| *s).unwrap_or(Status::Idle)
    }

    /// Asks the loop to end the turn after the current tool batch, with this reason.
    pub fn halt(&self, why: impl Into<String>) {
        if let Ok(mut h) = self.halt.lock() {
            h.get_or_insert(why.into());
        }
    }

    pub fn take_halt(&self) -> Option<String> {
        self.halt.lock().ok().and_then(|mut h| h.take())
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Per-agent plugin state, created on first use.
    pub fn with_state<T: Default + Send + 'static, R>(&self, key: &'static str, f: impl FnOnce(&mut T) -> R) -> R {
        let mut m = self.state.lock().expect("agent state");
        let slot = m.entry(key).or_insert_with(|| Box::new(T::default()));
        if slot.downcast_ref::<T>().is_none() {
            *slot = Box::new(T::default());
        }
        f(slot.downcast_mut::<T>().expect("state type"))
    }
}

/// A live frame of a model call in flight (`agent/assistant-stream`).
#[derive(Debug, Clone)]
pub enum StreamFrame {
    Start,
    Text(String),
    Reasoning(String),
    ToolCall(String),
    End,
}

#[derive(Debug, Clone)]
pub struct StreamEvent {
    pub agent: String,
    pub frame: StreamFrame,
}

pub struct AgentRegistry {
    agents: RwLock<HashMap<String, Arc<Agent>>>,
    runs: AtomicU64,
    /// `agent/created`
    pub created: Emit<Arc<Agent>>,
    /// `agent/status`
    pub status: Emit<(Arc<Agent>, Status)>,
    /// `agent/assistant-stream`
    pub stream: Emit<StreamEvent>,
}

impl Default for AgentRegistry {
    fn default() -> Self {
        AgentRegistry { agents: RwLock::new(HashMap::new()), runs: AtomicU64::new(1), created: Emit::new("agent/created"), status: Emit::new("agent/status"), stream: Emit::new("agent/assistant-stream") }
    }
}

impl AgentRegistry {
    pub fn create(&self, preset: AgentPreset, session: Arc<Session>, parent: Option<&Agent>, cancel: Arc<AtomicBool>) -> Arc<Agent> {
        self.create_at(preset, session, parent.map(|p| p.depth + 1).unwrap_or(0), parent.map(|p| p.id.clone()), cancel)
    }

    /// An agent whose parent lives elsewhere (another process): its depth and parent id are
    /// given, so delegation limits carry across the process boundary.
    pub fn create_at(&self, preset: AgentPreset, session: Arc<Session>, depth: usize, parent: Option<String>, cancel: Arc<AtomicBool>) -> Arc<Agent> {
        let a = Arc::new(Agent {
            id: session.id().to_string(),
            run: self.runs.fetch_add(1, Ordering::Relaxed),
            preset,
            depth,
            parent,
            session,
            cancel,
            inbox: Mutex::new(VecDeque::new()),
            injected: Mutex::new(vec![]),
            status: Mutex::new(Status::Idle),
            halt: Mutex::new(None),
            state: Mutex::new(HashMap::new()),
        });
        if let Ok(mut m) = self.agents.write() {
            m.insert(a.id.clone(), a.clone());
        }
        self.created.emit(&a);
        a
    }

    pub fn get(&self, id: &str) -> Option<Arc<Agent>> {
        self.agents.read().ok()?.get(id).cloned()
    }

    pub fn list(&self) -> Vec<Arc<Agent>> {
        self.agents.read().map(|m| m.values().cloned().collect()).unwrap_or_default()
    }

    /// The agent's children, by parent id.
    pub fn children(&self, id: &str) -> Vec<Arc<Agent>> {
        self.list().into_iter().filter(|a| a.parent.as_deref() == Some(id)).collect()
    }

    pub fn set_status(&self, a: &Arc<Agent>, s: Status) {
        if let Ok(mut st) = a.status.lock() {
            *st = s;
        }
        self.status.emit(&(a.clone(), s));
    }

    /// Forgets a finished agent (its session stays on disk).
    pub fn remove(&self, id: &str) {
        if let Ok(mut m) = self.agents.write() {
            m.remove(id);
        }
    }
}
