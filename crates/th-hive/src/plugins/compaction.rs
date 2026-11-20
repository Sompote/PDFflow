//! Context compaction (dsh-compaction-basic): under pressure before a step, or after a
//! context-overflow error, old tool results are pruned first; if that is not enough, the
//! older part of the surface is summarised by the agent's own model and replaced by one
//! `user/message` carrying the summary, followed by the values the shadowed tool results
//! pinned (`facts`), copied exactly rather than left to the summary. The lock (`compaction/start` … `compaction/end`)
//! and the summary are logged, so the replacement is reconstructable from the log.

use std::sync::Arc;

use crate::agent::Agent;
use crate::agent_loop::{AgentLoop, ErrorAction};
use crate::context::{Context, Plugin};
use crate::facts::{self, FactRecord};
use crate::keys;
use crate::llm::{LlmRequest, LlmService};
use crate::session::{EventBody, SurfaceOp};

pub struct Compaction {
    pub budget_tokens: usize,
    pub keep_recent: usize,
    pub prune_min_chars: usize,
    /// Most characters of pinned values that follow a summary.
    pub pin_chars: usize,
}

const SUMMARY_PROMPT: &str = "You compress the earlier part of an engineering agent's working conversation so the agent can continue without it. Write a faithful summary that keeps: the person's requests and decisions; all input data, numbers with units, design standards and clause numbers; results that tools returned (values, PASS/FAIL, utilisation ratios); files written (paths); open questions and what remains to do. Do not invent anything. Use short headed lists.";

/// Rough token estimate from characters (DeepSeek's tokenizer is near 3.5 characters per token).
pub fn tokens(chars: usize) -> usize {
    chars * 2 / 7
}

impl Compaction {
    /// Compacts the agent's session when needed (`force` ignores the budget). Returns
    /// whether the surface changed.
    pub fn run(&self, ctx: &Context, agent: &Arc<Agent>, force: bool) -> bool {
        let s = &agent.session;
        let est = tokens(s.estimate_chars());
        if !force && est <= self.budget_tokens {
            return false;
        }
        let sizes = s.surface_sizes();
        let n = sizes.len();
        if n <= self.keep_recent + 1 {
            return false;
        }
        // 1. prune old tool results
        let old = &sizes[..n - self.keep_recent];
        let prune: Vec<u64> = old.iter().filter(|(_, k, sz)| *k == "result" && *sz > self.prune_min_chars).map(|(s, _, _)| *s).collect();
        let mut changed = false;
        if !prune.is_empty() {
            s.append(EventBody::ToolPrune { seqs: prune });
            changed = true;
            if tokens(s.estimate_chars()) <= self.budget_tokens * 4 / 5 {
                return true;
            }
        }
        // 2. summarise the older surface; never split a call from its results
        let mut end = n - self.keep_recent - 1;
        while end > 0 && (sizes[end].1 == "assistant-calls" || sizes.get(end + 1).is_some_and(|x| x.1 == "result")) {
            end -= 1;
        }
        if end < 1 {
            return changed;
        }
        let (first, last) = (sizes[0].0, sizes[end].0);
        let shadowed: Vec<u64> = sizes[..=end].iter().map(|x| x.0).collect();
        let shadowed_tokens = tokens(sizes[..=end].iter().map(|x| x.2).sum());
        let turn = s.open_turn();
        s.append(EventBody::CompactionStart { turn });
        let transcript = crate::clip(&s.render_surface(last, 3000), 240_000);
        let Ok(llm) = ctx.expect::<LlmService>(keys::LLM) else {
            s.append(EventBody::CompactionEnd { turn, error: Some("no model service".into()) });
            return changed;
        };
        let p = &agent.preset;
        let req = LlmRequest {
            provider: p.provider.clone(),
            model: p.model.clone(),
            system: SUMMARY_PROMPT.into(),
            messages: vec![th_llm::Message::user(format!("Summarise this conversation:\n\n{transcript}"))],
            tools: vec![],
            temperature: Some(0.0),
            max_tokens: Some(4000),
            reasoning: Some("off".into()),
            purpose: "compaction".into(),
            session: s.id().to_string(),
        };
        match llm.complete(req, &agent.cancel) {
            Ok(r) => {
                let summary = r.message.text();
                // the values the shadowed tool results pinned follow the summary verbatim
                let records = s.fact_records();
                let pinned: Vec<&FactRecord> = facts::current(&records).into_iter().filter(|r| r.source <= last).collect();
                let block = facts::render_block(&pinned, self.pin_chars);
                let pinned_text = if block.is_empty() { String::new() } else { format!("\n\n{block}") };
                s.append(EventBody::CompactionSummary { summary: summary.clone(), shadowed, shadowed_tokens, provider: p.provider.name.clone(), model: p.model.clone() });
                s.append(EventBody::UserMessage {
                    text: format!("[Summary of the earlier conversation, made to save context]\n{summary}{pinned_text}"),
                    images: vec![],
                    source: "plugin:compaction".into(),
                    surface_op: Some(SurfaceOp { start: first, end: last }),
                });
                s.append(EventBody::CompactionEnd { turn, error: None });
                true
            }
            Err(e) => {
                s.append(EventBody::CompactionEnd { turn, error: Some(e.message) });
                changed
            }
        }
    }
}

struct Shared(Compaction);

impl Plugin for Compaction {
    fn name(&self) -> String {
        "hive-compaction".into()
    }
    fn describe(&self) -> String {
        format!("prune, then summarise above ~{}k tokens; keeps the last {} nodes", self.budget_tokens / 1000, self.keep_recent)
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::LOOP, keys::LLM]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let lp = ctx.expect::<AgentLoop>(keys::LOOP)?;
        let me = Arc::new(Shared(Compaction { budget_tokens: self.budget_tokens, keep_recent: self.keep_recent, prune_min_chars: self.prune_min_chars, pin_chars: self.pin_chars }));
        let (c1, m1) = (ctx.clone(), me.clone());
        ctx.effect(lp.pre_step.on(false, move |ps, next| {
            m1.0.run(&c1, &ps.agent, false);
            next(ps)
        }));
        let (c2, m2) = (ctx.clone(), me);
        ctx.effect(lp.request_error.on(true, move |re, next| {
            if re.error.overflow && re.attempt <= 2 && m2.0.run(&c2, &re.agent, true) {
                return ErrorAction::Retry(0);
            }
            next(re)
        }));
        Ok(())
    }
}
