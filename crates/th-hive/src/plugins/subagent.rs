//! Subagents (dsh-subagent): one activation API over several backends. A provider either
//! starts a local child agent on its own session (a fresh context, linked to the parent's
//! session) or hands the task to an external program. The model-facing `delegate` tool is
//! concurrent, so several delegations in one reply run at the same time;
//! `delegate_parallel` gives the same in one call. The facts a child pinned go up with its
//! answer, so a coordinator holds the values of the work it delegated.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::agent::{Agent, AgentPreset, AgentRegistry, Inbound};
use crate::agent_loop::AgentLoop;
use crate::context::{Context, Plugin};
use crate::facts::FactSet;
use crate::keys;
use crate::session::SessionMeta;
use crate::store::SessionStore;
use crate::tools::{ToolDef, ToolOutput, ToolRegistry};

/// A delegation backend.
pub trait SubagentProvider: Send + Sync {
    /// The id the model names (`designer`, `claude_code`).
    fn id(&self) -> String;
    fn name(&self) -> String;
    fn role(&self) -> String;
    /// Runs the task to completion and returns the answer.
    fn run(&self, ctx: &Context, parent: &Arc<Agent>, task: &str) -> Result<String, String>;
    /// The answer and the facts the task pinned, which are promoted to the parent's session
    /// with the delegation's result. Providers without a session of their own pin nothing.
    fn run_facts(&self, ctx: &Context, parent: &Arc<Agent>, task: &str) -> Result<(String, Vec<FactSet>), String> {
        self.run(ctx, parent, task).map(|t| (t, vec![]))
    }
}

pub type ProviderSource = dyn Fn(&Agent) -> Vec<Arc<dyn SubagentProvider>> + Send + Sync;

/// `ctx.subagents`
pub struct Subagents {
    sources: Arc<RwLock<Vec<(String, Arc<ProviderSource>)>>>,
    pub max_depth: usize,
}

impl Subagents {
    /// Adds a source of providers for a parent agent (owned by `ctx`'s plugin).
    pub fn source(&self, ctx: &Context, f: impl Fn(&Agent) -> Vec<Arc<dyn SubagentProvider>> + Send + Sync + 'static) {
        let owner = ctx.owner().to_string();
        if let Ok(mut s) = self.sources.write() {
            s.push((owner.clone(), Arc::new(f)));
        }
        let list = self.sources.clone();
        ctx.effect(Box::new(move || {
            if let Ok(mut s) = list.write() {
                s.retain(|(o, _)| *o != owner);
            }
        }));
    }

    /// The providers this agent may delegate to.
    pub fn for_agent(&self, a: &Agent) -> Vec<Arc<dyn SubagentProvider>> {
        if a.depth + 1 > self.max_depth {
            return vec![];
        }
        let fs: Vec<Arc<ProviderSource>> = self.sources.read().map(|s| s.iter().map(|(_, f)| f.clone()).collect()).unwrap_or_default();
        let mut out: Vec<Arc<dyn SubagentProvider>> = vec![];
        for f in fs {
            for p in f(a) {
                if !out.iter().any(|x| x.id() == p.id()) {
                    out.push(p);
                }
            }
        }
        out
    }

    /// Runs a delegated task; returns the answer and the facts the child pinned.
    pub fn delegate(&self, ctx: &Context, parent: &Arc<Agent>, to: &str, task: &str) -> Result<(String, Vec<FactSet>), String> {
        if parent.depth + 1 > self.max_depth {
            return Err("delegation is too deep; do this task yourself".into());
        }
        let p = self.for_agent(parent).into_iter().find(|p| p.id() == to).ok_or_else(|| format!("{} may not delegate to '{to}'", parent.preset.name))?;
        p.run_facts(ctx, parent, task)
    }
}

/// A local child agent with its own preset, session and loop (dsh spawn-in-process).
pub struct ChildAgent {
    pub preset: AgentPreset,
}

impl SubagentProvider for ChildAgent {
    fn id(&self) -> String {
        self.preset.id.clone()
    }
    fn name(&self) -> String {
        self.preset.name.clone()
    }
    fn role(&self) -> String {
        self.preset.role.clone()
    }
    fn run(&self, ctx: &Context, parent: &Arc<Agent>, task: &str) -> Result<String, String> {
        self.run_facts(ctx, parent, task).map(|(t, _)| t)
    }

    fn run_facts(&self, ctx: &Context, parent: &Arc<Agent>, task: &str) -> Result<(String, Vec<FactSet>), String> {
        let store = ctx.expect::<SessionStore>(keys::SESSIONS)?;
        let agents = ctx.expect::<AgentRegistry>(keys::AGENTS)?;
        let lp = ctx.expect::<AgentLoop>(keys::LOOP)?;
        let id = store.new_id(&self.preset.id);
        let session = store.create(SessionMeta { id, agent: self.preset.id.clone(), label: crate::clip(task, 120), parent: Some(parent.id.clone()), depth: parent.depth + 1 })?;
        // the child has its own Stop flag, set by the parent's Stop or by its time limit
        let cancel = Arc::new(AtomicBool::new(parent.cancel.load(Ordering::Relaxed)));
        let child = agents.create(self.preset.clone(), session, Some(parent), cancel.clone());
        child.followup(Inbound { text: format!("Task from {}:\n{task}", parent.preset.name), images: vec![], source: "parent".into() });
        let (done, timed_out) = (AtomicBool::new(false), AtomicBool::new(false));
        let start = Instant::now();
        let r = std::thread::scope(|sc| {
            sc.spawn(|| {
                while !done.load(Ordering::Relaxed) {
                    if parent.cancel.load(Ordering::Relaxed) {
                        cancel.store(true, Ordering::Relaxed);
                    } else if self.preset.time_limit.is_some_and(|l| start.elapsed() >= l) && !cancel.load(Ordering::Relaxed) {
                        timed_out.store(true, Ordering::Relaxed);
                        cancel.store(true, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            let r = lp.run_turn(ctx, &child);
            done.store(true, Ordering::Relaxed);
            r
        });
        agents.remove(&child.id);
        let out = r?;
        // the values the child pinned and still holds go up with its answer
        let records = child.session.fact_records();
        let facts: Vec<FactSet> = crate::facts::current(&records).into_iter().map(|r| r.set.clone()).collect();
        match out.reason.as_str() {
            "completed" | "max-steps" => Ok((out.text, facts)),
            "cancelled" if timed_out.load(Ordering::Relaxed) => {
                let min = self.preset.time_limit.map(|l| l.as_secs_f64() / 60.0).unwrap_or_default();
                let got = if out.text.trim().is_empty() { "no answer yet".to_string() } else { format!("its answer so far:\n{}", out.text) };
                Err(format!("{} stopped at its time limit of {min:.0} min, {got}", self.preset.name))
            }
            "cancelled" => Err("cancelled".into()),
            other => Err(format!("{} stopped ({other}): {}", self.preset.name, out.text)),
        }
    }
}

pub struct SubagentPlugin {
    pub max_depth: usize,
}

fn delegate_tool(subs: Arc<Subagents>, ids: Vec<String>) -> ToolDef {
    ToolDef::new(
        "delegate",
        "Give a task to one team member and wait for its answer. Several delegate calls in the same reply run at the same time.",
        json!({"type":"object","properties":{"agent":{"type":"string","enum":ids},"task":{"type":"string","description":"Complete, self-contained task with all data needed"}},"required":["agent","task"]}),
        move |t, a: Value| {
            let to = a.get("agent").and_then(|v| v.as_str()).ok_or("missing 'agent'")?;
            let task = a.get("task").and_then(|v| v.as_str()).ok_or("missing 'task'")?;
            subs.delegate(&t.ctx, &t.agent, to, task).map(|(text, facts)| ToolOutput { text, facts, ..Default::default() })
        },
    )
    .concurrent()
    .always()
}

fn parallel_tool(subs: Arc<Subagents>, ids: Vec<String>) -> ToolDef {
    ToolDef::new(
        "delegate_parallel",
        "Give independent tasks to several team members at once; returns all answers.",
        json!({"type":"object","properties":{"tasks":{"type":"array","items":{"type":"object","properties":{"agent":{"type":"string","enum":ids},"task":{"type":"string"}},"required":["agent","task"]}}},"required":["tasks"]}),
        move |t, a: Value| {
            let tasks: Vec<(String, String)> = a
                .get("tasks")
                .and_then(|t| t.as_array())
                .ok_or("missing 'tasks'")?
                .iter()
                .map(|t| (t.get("agent").and_then(|v| v.as_str()).unwrap_or("").to_string(), t.get("task").and_then(|v| v.as_str()).unwrap_or("").to_string()))
                .collect();
            let subs = &subs;
            let results: Vec<(String, Result<(String, Vec<FactSet>), String>)> = std::thread::scope(|sc| {
                let hs: Vec<_> = tasks.iter().map(|(to, task)| (to.clone(), sc.spawn(move || subs.delegate(&t.ctx, &t.agent, to, task)))).collect();
                hs.into_iter().map(|(to, h)| (to, h.join().unwrap_or_else(|_| Err("agent thread panicked".into())))).collect()
            });
            let mut facts = vec![];
            let mut parts = vec![];
            for (to, r) in results {
                match r {
                    Ok((text, f)) => {
                        facts.extend(f);
                        parts.push(format!("## {to}\n{text}"));
                    }
                    Err(e) => parts.push(format!("## {to}\nERROR: {e}")),
                }
            }
            Ok(ToolOutput { text: parts.join("\n\n"), facts, ..Default::default() })
        },
    )
    .concurrent()
    .always()
}

impl Plugin for SubagentPlugin {
    fn name(&self) -> String {
        "hive-subagent".into()
    }
    fn describe(&self) -> String {
        format!("delegate / delegate_parallel, depth ≤ {}", self.max_depth)
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::TOOLS, keys::LOOP, keys::SESSIONS]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let subs = Arc::new(Subagents { sources: Arc::new(RwLock::new(vec![])), max_depth: self.max_depth });
        ctx.provide(keys::SUBAGENTS, subs.clone())?;
        let tools = ctx.expect::<ToolRegistry>(keys::TOOLS)?;
        // the tools' enum lists the providers each agent may reach, so they come from a source
        tools.source(ctx, move |a, _| {
            let ids: Vec<String> = subs.for_agent(a).iter().map(|p| p.id()).collect();
            if ids.is_empty() {
                return vec![];
            }
            vec![Arc::new(delegate_tool(subs.clone(), ids.clone())), Arc::new(parallel_tool(subs.clone(), ids))]
        });
        Ok(())
    }
}
