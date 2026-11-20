//! The default agent driver (`ctx.agentLoop`), following DeepSeek Harness's turn flow:
//!
//! ```text
//! turn/start
//!   claim next-step input (injected context + one queued message)
//!   resolve the step's tools; assemble the system prompt
//!   agent/pre-step waterfall → enter or reject (compaction runs here)
//!   step/start; system/message if changed; user/message per entered input
//!   agent/request waterfall (adjust model controls)
//!   derive history from the log → llm/stream → agent/assistant-stream frames
//!     failure → assistant/attempt → agent/request-error waterfall → retry or fail
//!   assistant/message
//!   tool batch (tools pipeline) → tool/result*
//!   step/end
//!   tools owe another request, or input arrived → next step
//!   natural stop → agent/turn-stopping (bail: a listener may continue the turn)
//! turn/end
//! ```

use std::sync::Arc;

use th_llm::{CallError, Delta, Reply};

use crate::agent::{Agent, AgentRegistry, Inbound, Status, StreamEvent, StreamFrame};
use crate::context::Context;
use crate::events::{Bail, Waterfall};
use crate::llm::{LlmRequest, LlmService};
use crate::prompt::SystemPrompt;
use crate::session::{CallRecord, DeriveOptions, EventBody, Usage};
use crate::tools::ToolRegistry;

/// `agent/pre-step`: listeners may rewrite the entered messages or reject the step.
pub struct PreStep {
    pub agent: Arc<Agent>,
    pub turn: u64,
    pub step: u64,
    pub messages: Vec<Inbound>,
    pub reject: bool,
}

/// `agent/request`: the request before history is derived.
pub struct RequestControls {
    pub agent: Arc<Agent>,
    pub req: LlmRequest,
}

/// `agent/request-error`
pub struct RequestError {
    pub agent: Arc<Agent>,
    pub error: CallError,
    /// 1 for the first failure of this step.
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ErrorAction {
    Fail,
    /// Retry the same step after waiting (milliseconds); history is derived again, so a
    /// listener that compacted the log changes the retried request.
    Retry(u64),
}

/// `agent/turn-stopping`: the model stopped without tool calls.
pub struct TurnStopping {
    pub agent: Arc<Agent>,
    pub turn: u64,
    pub step: u64,
    pub text: String,
    pub finish: String,
}

/// The result of one turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnOutcome {
    pub text: String,
    /// completed | max-steps | cancelled | error | rejected | stuck | truncated
    pub reason: String,
}

pub struct AgentLoop {
    pub pre_step: Waterfall<PreStep, ()>,
    pub request: Waterfall<RequestControls, ()>,
    pub request_error: Waterfall<RequestError, ErrorAction>,
    pub turn_stopping: Bail<TurnStopping, String>,
}

impl Default for AgentLoop {
    fn default() -> Self {
        AgentLoop { pre_step: Waterfall::new("agent/pre-step"), request: Waterfall::new("agent/request"), request_error: Waterfall::new("agent/request-error"), turn_stopping: Bail::new("agent/turn-stopping") }
    }
}

pub const AGENTS: &str = "agents";
pub const LOOP: &str = "agentLoop";
pub const LLM: &str = "llm";
pub const TOOLS: &str = "tools";
pub const PROMPT: &str = "systemPrompt";

impl AgentLoop {
    /// Runs one turn of the agent until nothing is owed. Returns the final answer.
    pub fn run_turn(&self, ctx: &Context, agent: &Arc<Agent>) -> Result<TurnOutcome, String> {
        let agents = ctx.expect::<AgentRegistry>(AGENTS)?;
        let llm = ctx.expect::<LlmService>(LLM)?;
        let tools = ctx.expect::<ToolRegistry>(TOOLS)?;
        let prompt = ctx.expect::<SystemPrompt>(PROMPT)?;
        let s = &agent.session;
        let turn = s.next_turn();
        s.append(EventBody::TurnStart { turn });
        agents.set_status(agent, Status::Running);
        let end = |reason: &str, text: String| {
            s.append(EventBody::TurnEnd { turn, reason: reason.into() });
            agents.set_status(agent, if reason == "error" { Status::Failed } else { Status::Done });
            TurnOutcome { text, reason: reason.into() }
        };
        let mut last_text = String::new();
        let mut step: u64 = 0;
        let mut cut_off = false;
        loop {
            if agent.cancelled() {
                return Ok(end("cancelled", last_text));
            }
            if step as usize >= agent.preset.max_steps.max(1) {
                let note = format!("{last_text}\n\n(stopped after {} steps)", agent.preset.max_steps);
                return Ok(end("max-steps", note));
            }
            step += 1;
            let toolset = tools.resolve(agent, ctx);
            let mut pre = PreStep { agent: agent.clone(), turn, step, messages: agent.claim(), reject: false };
            self.pre_step.dispatch(&mut pre, &|_| ());
            if pre.reject {
                return Ok(end("rejected", last_text));
            }
            s.append(EventBody::StepStart { turn, step });
            let system = prompt.assemble(agent, &toolset);
            if s.system_prompt().as_deref() != Some(system.as_str()) {
                s.append(EventBody::SystemMessage { text: system.clone() });
            }
            for m in pre.messages {
                s.append(EventBody::UserMessage { text: m.text, images: m.images, source: m.source, surface_op: None });
            }
            let p = &agent.preset;
            let mut rc = RequestControls {
                agent: agent.clone(),
                req: LlmRequest { provider: p.provider.clone(), model: p.model.clone(), system, messages: vec![], tools: toolset.iter().map(|d| d.spec()).collect(), temperature: p.temperature, max_tokens: p.max_tokens, reasoning: p.reasoning.clone(), purpose: "agent".into(), session: s.id().to_string() },
            };
            self.request.dispatch(&mut rc, &|_| ());
            let passback = llm.adapter_for(&rc.req.provider).map(|a| a.reasoning_passback(&rc.req.provider)).unwrap_or(false);
            let mut attempt = 0;
            let reply: Reply = loop {
                let mut req = rc.req.clone();
                req.messages = s.derive_messages(DeriveOptions { reasoning_passback: passback });
                agents.stream.emit(&StreamEvent { agent: agent.id.clone(), frame: StreamFrame::Start });
                let mut on = |d: Delta| {
                    let frame = match d {
                        Delta::Text(t) => StreamFrame::Text(t),
                        Delta::Reasoning(t) => StreamFrame::Reasoning(t),
                        Delta::ToolCall { name, .. } => StreamFrame::ToolCall(name),
                    };
                    agents.stream.emit(&StreamEvent { agent: agent.id.clone(), frame });
                };
                let r = llm.stream(req, &mut on, &agent.cancel);
                agents.stream.emit(&StreamEvent { agent: agent.id.clone(), frame: StreamFrame::End });
                match r {
                    Ok(r) => break r,
                    Err(e) => {
                        s.append(EventBody::AssistantAttempt { error: e.message.clone(), provider: rc.req.provider.name.clone(), model: rc.req.model.clone() });
                        if e.cancelled || agent.cancelled() {
                            s.append(EventBody::StepEnd { turn, step });
                            return Ok(end("cancelled", last_text));
                        }
                        attempt += 1;
                        let mut re = RequestError { agent: agent.clone(), error: e.clone(), attempt };
                        match self.request_error.dispatch(&mut re, &|_| ErrorAction::Fail) {
                            ErrorAction::Retry(ms) => {
                                if ms > 0 {
                                    std::thread::sleep(std::time::Duration::from_millis(ms));
                                }
                                continue;
                            }
                            ErrorAction::Fail => {
                                s.append(EventBody::StepEnd { turn, step });
                                end("error", last_text);
                                return Err(e.message);
                            }
                        }
                    }
                }
            };
            let text = clean_markup(&reply.message.text());
            let calls: Vec<CallRecord> = reply.message.tool_calls.clone().unwrap_or_default().into_iter().map(|c| CallRecord { id: c.id, name: c.function.name, arguments: c.function.arguments }).collect();
            s.append(EventBody::AssistantMessage {
                text: text.clone(),
                reasoning: reply.reasoning.clone(),
                calls: calls.clone(),
                finish: reply.finish_reason.clone(),
                provider: rc.req.provider.name.clone(),
                model: [rc.req.model.as_str(), rc.req.provider.default_model.as_str(), rc.req.provider.name.as_str()].into_iter().find(|m| !m.is_empty()).unwrap_or_default().to_string(),
                usage: Usage { prompt: reply.usage.prompt_tokens, completion: reply.usage.completion_tokens, cached: reply.usage.cached_tokens },
            });
            if !text.trim().is_empty() {
                last_text = text.clone();
            }
            if !calls.is_empty() {
                tools.run_batch(ctx, agent, &calls, &toolset);
            }
            s.append(EventBody::StepEnd { turn, step });
            if let Some(why) = agent.take_halt() {
                let note = if last_text.trim().is_empty() { format!("({why})") } else { format!("{last_text}\n\n({why})") };
                return Ok(end("stuck", note));
            }
            if calls.is_empty() && text.trim().is_empty() && reply.finish_reason == "length" {
                // the reply ran out of output tokens (usually in its reasoning) before any answer
                let limit = p.max_tokens.map(|n| format!(" of {n} tokens")).unwrap_or_default();
                if !cut_off {
                    cut_off = true;
                    agent.inject("plugin:output-limit", format!("Your last reply reached the output limit{limit} before you wrote anything. Do not reason at length: give the answer, or the next tool call, now and keep it short."));
                    continue;
                }
                return Ok(end("truncated", format!("(the model reached its output limit{limit} twice before it wrote an answer)")));
            }
            if !calls.is_empty() || agent.has_pending() {
                continue;
            }
            let ts = TurnStopping { agent: agent.clone(), turn, step, text: text.clone(), finish: reply.finish_reason.clone() };
            if let Some(more) = self.turn_stopping.bail(&ts) {
                agent.inject("plugin:turn-stopping", more);
                continue;
            }
            return Ok(end("completed", text));
        }
    }
}

/// Removes a model's raw tool-call markup that leaks into its text (DeepSeek's DSML tags).
pub fn clean_markup(s: &str) -> String {
    match s.find("<｜DSML｜").or_else(|| s.find("<|DSML|")) {
        Some(i) => s[..i].trim_end().to_string(),
        None => s.to_string(),
    }
}
