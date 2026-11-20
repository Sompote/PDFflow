//! # th-hive: the TellusHive agent harness
//!
//! A Rust port of the architecture of DeepSeek Harness (`dsh`): *everything is a plugin*.
//!
//! - [`context`]: a Cordis-style plugin tree. Plugins provide services (`ctx.get("tools")`),
//!   wait for the services they inject, and record every registration as an effect that
//!   unwinds when they unload.
//! - [`events`]: typed hook points with the Cordis dispatch modes (emit, waterfall, bail).
//! - [`session`]: the append-only JSONL session log; the model's history is derived from it.
//! - [`facts`]: values a tool pins to its result, so they survive compaction exactly.
//! - [`agent`], [`agent_loop`]: live agents and the turn/step driver with its hooks
//!   (`agent/pre-step`, `agent/request`, `agent/request-error`, `agent/turn-stopping`).
//! - [`tools`]: the tool registry and its guarded execution pipeline.
//! - [`llm`]: the model seam with the `llm/stream` waterfall.
//! - [`prompt`]: prompt sections and the `system-prompt/assemble` waterfall.
//! - [`plugins`]: the base bundle (core services, OpenAI-compatible adapter, retry,
//!   compaction, repeat-tool reminder, continuation guard, todo, spill, subagents, team
//!   task board).
//!
//! An application composes a profile from the base bundle plus its own plugins and boots
//! it with [`Hive::boot`].

pub mod agent;
pub mod agent_loop;
pub mod context;
pub mod events;
pub mod facts;
pub mod llm;
pub mod plugins;
pub mod prompt;
pub mod session;
pub mod store;
pub mod tools;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub use agent::{Agent, AgentPreset, AgentRegistry, Inbound, Status, StreamEvent, StreamFrame, ToolFilter};
pub use agent_loop::{AgentLoop, ErrorAction, PreStep, RequestControls, RequestError, TurnOutcome, TurnStopping};
pub use context::{fn_plugin, Context, Plugin, PluginStatus};
pub use events::{Bail, Disposer, Emit, Waterfall};
pub use facts::{Fact, FactRecord, FactSet};
pub use llm::{LlmAdapter, LlmRequest, LlmService};
pub use plugins::BaseConfig;
pub use prompt::SystemPrompt;
pub use session::{Appended, EventBody, Session, SessionEvent, SessionMeta};
pub use store::SessionStore;
pub use tools::{Decision, ExecMode, ToolCtx, ToolDef, ToolOutput, ToolRegistry};

/// Service keys.
pub mod keys {
    pub use crate::agent_loop::{AGENTS, LLM, LOOP, PROMPT, TOOLS};
    pub const SESSIONS: &str = "sessions";
    pub const SUBAGENTS: &str = "subagents";
    pub const APPROVAL: &str = "approval";
    pub const TEAM_BOARD: &str = "teamBoard";
}

/// A booted plugin tree.
pub struct Hive {
    pub ctx: Context,
}

impl Hive {
    /// Mounts the plugins in order (each waits for the services it injects).
    pub fn boot(plugins: Vec<Arc<dyn Plugin>>) -> Hive {
        let ctx = Context::new();
        for p in plugins {
            ctx.plugin(p);
        }
        Hive { ctx }
    }

    pub fn agents(&self) -> Arc<AgentRegistry> {
        self.ctx.expect(keys::AGENTS).expect("core plugin")
    }
    pub fn sessions(&self) -> Arc<SessionStore> {
        self.ctx.expect(keys::SESSIONS).expect("core plugin")
    }
    pub fn tools(&self) -> Arc<ToolRegistry> {
        self.ctx.expect(keys::TOOLS).expect("core plugin")
    }

    /// Creates an agent on a session (new or resumed).
    pub fn agent(&self, preset: AgentPreset, session: Arc<Session>, cancel: Arc<AtomicBool>) -> Arc<Agent> {
        self.agents().create(preset, session, None, cancel)
    }

    /// Queues a message and runs one turn.
    pub fn send(&self, agent: &Arc<Agent>, msg: Inbound) -> Result<TurnOutcome, String> {
        agent.followup(msg);
        let lp = self.ctx.expect::<AgentLoop>(keys::LOOP)?;
        lp.run_turn(&self.ctx, agent)
    }

    /// The plugin tree with each plugin's state (`dsh --dump-config`).
    pub fn dump(&self) -> String {
        let mut s = String::new();
        for (name, st, d) in self.ctx.plugins() {
            let st = match st {
                PluginStatus::Active => "active".to_string(),
                PluginStatus::Pending(m) => format!("pending ({})", m.join(", ")),
                PluginStatus::Failed(e) => format!("failed: {e}"),
                PluginStatus::Disabled => "disabled".into(),
            };
            s.push_str(&format!("- {name:32} {st:12} {d}\n"));
        }
        s
    }
}

/// Clips text to `n` characters, marking the cut.
pub fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}
