//! The OpenAI-compatible model adapter and the request retry policy.

use std::sync::Arc;

use crate::agent_loop::{AgentLoop, ErrorAction};
use crate::context::{Context, Plugin};
use crate::keys;
use crate::llm::{LlmService, OpenAiCompatible};

pub struct OpenAiAdapter;

impl Plugin for OpenAiAdapter {
    fn name(&self) -> String {
        "hive-llm-openai-compatible".into()
    }
    fn describe(&self) -> String {
        "DeepSeek, OpenRouter, Ollama, OpenAI (streamed chat completions)".into()
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::LLM]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let llm = ctx.expect::<LlmService>(keys::LLM)?;
        ctx.effect(llm.register(Arc::new(OpenAiCompatible)));
        Ok(())
    }
}

/// Retries rate limits, server errors and broken streams with a growing wait.
pub struct Retry {
    pub attempts: u32,
}

/// Whether a failed call is worth repeating unchanged.
pub fn transient(e: &th_llm::CallError) -> bool {
    if e.cancelled || e.overflow {
        return false;
    }
    match e.status {
        Some(s) => s == 408 || s == 429 || s >= 500,
        None => true,
    }
}

impl Plugin for Retry {
    fn name(&self) -> String {
        "hive-llm-retry".into()
    }
    fn describe(&self) -> String {
        format!("{} retries on 408/429/5xx and transport errors", self.attempts)
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::LOOP]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let lp = ctx.expect::<AgentLoop>(keys::LOOP)?;
        let attempts = self.attempts;
        ctx.effect(lp.request_error.on(false, move |re, next| {
            if transient(&re.error) && re.attempt <= attempts {
                let wait = [2_000u64, 5_000, 12_000, 30_000][(re.attempt as usize - 1).min(3)];
                return ErrorAction::Retry(wait);
            }
            next(re)
        }));
        Ok(())
    }
}
