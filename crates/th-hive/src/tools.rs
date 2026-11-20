//! The tool seam (`ctx.tools`): a registry, per-agent tool sources, and the guarded
//! execution pipeline of DeepSeek Harness:
//!
//! ```text
//! tool/call (logged) → tools/pre-execute waterfall (allow | deny | ask → ctx.approval)
//!   → monotonic guards (deny or abstain) → tools/execute waterfall around the tool body
//!   → tools/post-execute waterfall (in model order; may replace the result or add context)
//!   → finalize (clip) → tools/result notification → tool/result (logged)
//!   → facts/set (logged, one per fact set the tool pinned to its result)
//! ```
//!
//! Calls are scheduled by execution mode: consecutive `Concurrent` calls run together on a
//! bounded pool, an `Exclusive` call is a barrier. Context that post-execute listeners add
//! is injected as `user/message` events after every result of the batch is recorded, so
//! call/result adjacency holds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use serde_json::{json, Value};
use th_llm::ToolSpec;

use crate::agent::Agent;
use crate::context::Context;
use crate::events::{Emit, Waterfall};
use crate::facts::{self, FactRecord, FactSet};
use crate::session::{CallRecord, EventBody};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMode {
    /// May run beside other concurrent calls of the same reply.
    Concurrent,
    /// Runs alone, in order.
    Exclusive,
}

#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub text: String,
    pub is_error: bool,
    /// Structured data for the application (artifacts, saved models); not sent to the model.
    pub meta: Value,
    /// (source, text) context injected after the batch's results.
    pub additional_context: Vec<(String, String)>,
    /// Values pinned to this result: logged with it and shown again, exactly, once the
    /// result is pruned or summarised away.
    pub facts: Vec<FactSet>,
}

impl ToolOutput {
    pub fn ok(text: impl Into<String>) -> Self {
        ToolOutput { text: text.into(), ..Default::default() }
    }
    pub fn error(text: impl Into<String>) -> Self {
        ToolOutput { text: text.into(), is_error: true, ..Default::default() }
    }
}

/// What a running tool can reach.
pub struct ToolCtx {
    pub ctx: Context,
    pub agent: Arc<Agent>,
    pub call_id: String,
}

pub type ExecFn = Arc<dyn Fn(&ToolCtx, Value) -> Result<ToolOutput, String> + Send + Sync>;
pub type AvailFn = Arc<dyn Fn(&Agent) -> bool + Send + Sync>;

pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub mode: ExecMode,
    /// The plugin that registered it.
    pub owner: String,
    /// Harness tools (todo, delegation, task board) are not subject to the preset's tool
    /// list; `available` still decides.
    pub always: bool,
    pub available: Option<AvailFn>,
    pub execute: ExecFn,
}

impl ToolDef {
    pub fn new(name: &str, description: &str, parameters: Value, execute: impl Fn(&ToolCtx, Value) -> Result<ToolOutput, String> + Send + Sync + 'static) -> ToolDef {
        ToolDef { name: name.into(), description: description.into(), parameters, mode: ExecMode::Exclusive, owner: String::new(), always: false, available: None, execute: Arc::new(execute) }
    }
    pub fn concurrent(mut self) -> Self {
        self.mode = ExecMode::Concurrent;
        self
    }
    pub fn always(mut self) -> Self {
        self.always = true;
        self
    }
    pub fn when(mut self, f: impl Fn(&Agent) -> bool + Send + Sync + 'static) -> Self {
        self.available = Some(Arc::new(f));
        self
    }
    pub fn spec(&self) -> ToolSpec {
        let mut p = self.parameters.clone();
        if p.get("type").is_none() {
            p["type"] = json!("object");
        }
        ToolSpec { name: self.name.clone(), description: self.description.clone(), parameters: p }
    }
    fn admitted(&self, a: &Agent) -> bool {
        (self.always || a.preset.tools.allows(&self.name)) && self.available.as_ref().map(|f| f(a)).unwrap_or(true)
    }
}

/// `tools/pre-execute`
pub struct PreExecute {
    pub agent: Arc<Agent>,
    pub tool: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Allow,
    Deny(String),
    /// Ask the person (through `ctx.approval`); no approver means deny.
    Ask(String),
}

/// `tools/execute`
pub struct Execute {
    pub agent: Arc<Agent>,
    pub tool: Arc<ToolDef>,
    pub args: Value,
    pub call_id: String,
}

/// `tools/post-execute`
pub struct PostExecute {
    pub agent: Arc<Agent>,
    pub tool: String,
    pub args: Value,
    pub call_id: String,
    pub output: ToolOutput,
}

/// `tools/result`: the frozen outcome.
#[derive(Clone)]
pub struct ToolResultNote {
    pub agent: Arc<Agent>,
    pub call_id: String,
    pub tool: String,
    pub args: Value,
    pub output: ToolOutput,
}

/// One-shot approval of an `Ask` decision (`ctx.approval`).
pub trait Approver: Send + Sync {
    fn approve(&self, agent: &Agent, tool: &str, args: &Value, reason: &str) -> bool;
}

pub type SourceFn = dyn Fn(&Arc<Agent>, &Context) -> Vec<Arc<ToolDef>> + Send + Sync;
pub type GuardFn = dyn Fn(&PreExecute) -> Option<String> + Send + Sync;

static IDS: AtomicU64 = AtomicU64::new(1);

pub struct ToolRegistry {
    tools: Arc<RwLock<Vec<Arc<ToolDef>>>>,
    sources: Arc<RwLock<Vec<(u64, String, Arc<SourceFn>)>>>,
    guards: Arc<RwLock<Vec<(u64, Arc<GuardFn>)>>>,
    pub pre_execute: Waterfall<PreExecute, Decision>,
    pub execute_hook: Waterfall<Execute, Result<ToolOutput, String>>,
    pub post_execute: Waterfall<PostExecute, ()>,
    pub result: Emit<ToolResultNote>,
    /// Calls of one reply that may run at the same time.
    pub max_parallel: usize,
    /// Longest tool result the model sees (characters).
    pub max_result_chars: usize,
    /// Log the facts tools pin to their results.
    pub pin_facts: bool,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        ToolRegistry {
            tools: Arc::new(RwLock::new(vec![])),
            sources: Arc::new(RwLock::new(vec![])),
            guards: Arc::new(RwLock::new(vec![])),
            pre_execute: Waterfall::new("tools/pre-execute"),
            execute_hook: Waterfall::new("tools/execute"),
            post_execute: Waterfall::new("tools/post-execute"),
            result: Emit::new("tools/result"),
            max_parallel: 8,
            max_result_chars: 20_000,
            pin_facts: true,
        }
    }
}

impl ToolRegistry {
    /// Registers a tool owned by `ctx`'s plugin; unloading the plugin removes it.
    pub fn register(&self, ctx: &Context, mut def: ToolDef) -> Result<(), String> {
        def.owner = ctx.owner().to_string();
        let name = def.name.clone();
        {
            let mut t = self.tools.write().map_err(|_| "poisoned")?;
            if t.iter().any(|x| x.name == name) {
                return Err(format!("tool '{name}' is already registered"));
            }
            t.push(Arc::new(def));
        }
        let list = self.tools.clone();
        ctx.effect(Box::new(move || {
            if let Ok(mut t) = list.write() {
                t.retain(|x| x.name != name);
            }
        }));
        Ok(())
    }

    /// Adds a per-agent tool source (tools that depend on the agent, e.g. its MCP plugins).
    pub fn source(&self, ctx: &Context, f: impl Fn(&Arc<Agent>, &Context) -> Vec<Arc<ToolDef>> + Send + Sync + 'static) {
        let id = IDS.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut s) = self.sources.write() {
            s.push((id, ctx.owner().to_string(), Arc::new(f)));
        }
        let list = self.sources.clone();
        ctx.effect(Box::new(move || {
            if let Ok(mut s) = list.write() {
                s.retain(|(i, _, _)| *i != id);
            }
        }));
    }

    /// Adds a monotonic guard: it may deny a call, never allow one another listener denied.
    pub fn guard(&self, ctx: &Context, f: impl Fn(&PreExecute) -> Option<String> + Send + Sync + 'static) {
        let id = IDS.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut g) = self.guards.write() {
            g.push((id, Arc::new(f)));
        }
        let list = self.guards.clone();
        ctx.effect(Box::new(move || {
            if let Ok(mut g) = list.write() {
                g.retain(|(i, _)| *i != id);
            }
        }));
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.read().map(|t| t.iter().map(|d| d.name.clone()).collect()).unwrap_or_default()
    }

    pub fn get(&self, name: &str) -> Option<Arc<ToolDef>> {
        self.tools.read().ok()?.iter().find(|d| d.name == name).cloned()
    }

    /// The tools one step of this agent admits (registered + its sources), in a stable order.
    pub fn resolve(&self, agent: &Arc<Agent>, ctx: &Context) -> Vec<Arc<ToolDef>> {
        let mut out: Vec<Arc<ToolDef>> = self.tools.read().map(|t| t.iter().filter(|d| d.admitted(agent)).cloned().collect()).unwrap_or_default();
        let sources: Vec<Arc<SourceFn>> = self.sources.read().map(|s| s.iter().map(|(_, _, f)| f.clone()).collect()).unwrap_or_default();
        for s in sources {
            for d in s(agent, ctx) {
                if !out.iter().any(|x| x.name == d.name) && d.available.as_ref().map(|f| f(agent)).unwrap_or(true) {
                    out.push(d);
                }
            }
        }
        out
    }

    /// Runs one reply's tool calls through the pipeline, logging `tool/call` before each
    /// starts and `tool/result` in model order, then injects any added context.
    pub fn run_batch(&self, ctx: &Context, agent: &Arc<Agent>, calls: &[CallRecord], toolset: &[Arc<ToolDef>]) {
        let find = |n: &str| toolset.iter().find(|d| d.name == n).cloned();
        // groups: consecutive concurrent calls together (bounded), exclusive alone
        let mut groups: Vec<Vec<usize>> = vec![];
        for (i, c) in calls.iter().enumerate() {
            let conc = find(&c.name).map(|d| d.mode == ExecMode::Concurrent).unwrap_or(false);
            match groups.last_mut() {
                Some(g) if conc && g.len() < self.max_parallel.max(1) && g.iter().all(|&j| find(&calls[j].name).map(|d| d.mode == ExecMode::Concurrent).unwrap_or(false)) => g.push(i),
                _ => groups.push(vec![i]),
            }
        }
        let mut context: Vec<(String, String)> = vec![];
        for g in groups {
            for &i in &g {
                let c = &calls[i];
                agent.session.append(EventBody::ToolCall { id: c.id.clone(), name: c.name.clone(), arguments: c.arguments.clone() });
            }
            let raw: Vec<(Value, ToolOutput)> = if g.len() == 1 {
                vec![self.invoke(ctx, agent, &calls[g[0]], find(&calls[g[0]].name))]
            } else {
                std::thread::scope(|sc| {
                    let hs: Vec<_> = g.iter().map(|&i| {
                        let def = find(&calls[i].name);
                        sc.spawn(move || self.invoke(ctx, agent, &calls[i], def))
                    }).collect();
                    hs.into_iter().map(|h| h.join().unwrap_or_else(|_| (Value::Null, ToolOutput::error("ERROR: the tool panicked")))).collect()
                })
            };
            // post-execute and logging in model order
            for (&i, (args, out)) in g.iter().zip(raw) {
                let c = &calls[i];
                let mut post = PostExecute { agent: agent.clone(), tool: c.name.clone(), args: args.clone(), call_id: c.id.clone(), output: out };
                self.post_execute.dispatch(&mut post, &|_| ());
                let mut out = post.output;
                if out.text.chars().count() > self.max_result_chars {
                    out.text = crate::clip(&out.text, self.max_result_chars);
                }
                context.append(&mut out.additional_context);
                self.result.emit(&ToolResultNote { agent: agent.clone(), call_id: c.id.clone(), tool: c.name.clone(), args, output: out.clone() });
                let source = agent.session.append(EventBody::ToolResult { id: c.id.clone(), name: c.name.clone(), text: out.text, is_error: out.is_error, meta: out.meta });
                if !out.is_error && self.pin_facts {
                    for set in out.facts.into_iter().filter(|s| !s.facts.is_empty()) {
                        if let Ok(data) = serde_json::to_value(FactRecord { source, tool: c.name.clone(), set }) {
                            agent.session.append(EventBody::Custom { kind: facts::KIND.into(), data });
                        }
                    }
                }
            }
        }
        for (source, text) in context {
            agent.session.append(EventBody::UserMessage { text, images: vec![], source, surface_op: None });
        }
    }

    /// pre-execute → guards → execute. Returns the parsed arguments and the raw output.
    fn invoke(&self, ctx: &Context, agent: &Arc<Agent>, call: &CallRecord, def: Option<Arc<ToolDef>>) -> (Value, ToolOutput) {
        let Some(def) = def else {
            return (Value::Null, ToolOutput::error(format!("ERROR: tool '{}' is not available to {}", call.name, agent.preset.name)));
        };
        if agent.cancelled() {
            return (Value::Null, ToolOutput::error("ERROR: cancelled"));
        }
        let args: Value = if call.arguments.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(&call.arguments) {
                Ok(v) => v,
                Err(e) if e.is_eof() => return (Value::Null, ToolOutput::error(format!("ERROR: the call was cut off at the output limit ({e}). Make it smaller: split the work into several calls"))),
                Err(e) => return (Value::Null, ToolOutput::error(format!("ERROR: arguments are not valid JSON ({e}); resend the call"))),
            }
        };
        let mut pre = PreExecute { agent: agent.clone(), tool: call.name.clone(), args };
        let decision = self.pre_execute.dispatch(&mut pre, &|_| Decision::Allow);
        let decision = match decision {
            Decision::Ask(reason) => match ctx.get::<Box<dyn Approver>>("approval") {
                Some(a) if a.approve(agent, &pre.tool, &pre.args, &reason) => Decision::Allow,
                Some(_) => Decision::Deny(format!("the person refused: {reason}")),
                None => Decision::Deny(format!("needs approval and no one can approve here: {reason}")),
            },
            d => d,
        };
        if let Decision::Deny(why) = decision {
            return (pre.args, ToolOutput::error(format!("ERROR: denied: {why}")));
        }
        let guards: Vec<Arc<GuardFn>> = self.guards.read().map(|g| g.iter().map(|(_, f)| f.clone()).collect()).unwrap_or_default();
        if let Some(why) = guards.iter().find_map(|g| g(&pre)) {
            return (pre.args, ToolOutput::error(format!("ERROR: denied: {why}")));
        }
        let args = pre.args;
        let mut ex = Execute { agent: agent.clone(), tool: def, args: args.clone(), call_id: call.id.clone() };
        let r = self.execute_hook.dispatch(&mut ex, &|e: &mut Execute| {
            let tc = ToolCtx { ctx: ctx.clone(), agent: e.agent.clone(), call_id: e.call_id.clone() };
            (e.tool.execute)(&tc, e.args.clone())
        });
        let out = match r {
            Ok(o) => o,
            Err(e) => ToolOutput::error(format!("ERROR: {e}")),
        };
        (args, out)
    }
}
