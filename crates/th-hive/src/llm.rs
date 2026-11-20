//! The model seam (`ctx.llm`): adapters registered by provider, and the `llm/stream`
//! waterfall around every call.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};

use th_llm::{CallError, ChatOptions, Delta, Message, Provider, Reply, ToolSpec};

use crate::events::{Disposer, Waterfall};

/// One model request, frozen before it is sent.
#[derive(Debug, Clone)]
pub struct LlmRequest {
    pub provider: Provider,
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Thinking: "off", "low", "medium", "high"; None = the provider's default.
    pub reasoning: Option<String>,
    /// "agent" for a loop step, "compaction" for a summary, or a plugin's own purpose.
    pub purpose: String,
    /// The agent session the request belongs to.
    pub session: String,
}

impl LlmRequest {
    /// System prompt + history, as the wire wants it.
    pub fn wire_messages(&self) -> Vec<Message> {
        let mut m = Vec::with_capacity(self.messages.len() + 1);
        if !self.system.is_empty() {
            m.push(Message::system(self.system.clone()));
        }
        m.extend(self.messages.iter().cloned());
        m
    }
}

/// A model adapter (DeepSeek Harness `ctx.llm` adapter).
pub trait LlmAdapter: Send + Sync {
    fn id(&self) -> &str;
    /// Whether this adapter serves the provider.
    fn serves(&self, p: &Provider) -> bool;
    /// Whether thinking-mode reasoning must be sent back inside a turn.
    fn reasoning_passback(&self, _p: &Provider) -> bool {
        false
    }
    fn stream(&self, req: &LlmRequest, on: &mut dyn FnMut(Delta), cancel: &AtomicBool) -> Result<Reply, CallError>;
}

/// The argument of the `llm/stream` waterfall: listeners may change the request (or answer
/// it themselves, e.g. a recorded-replay plugin) before the adapter streams it.
pub struct StreamCall {
    pub req: LlmRequest,
}

pub struct LlmService {
    adapters: Arc<RwLock<Vec<Arc<dyn LlmAdapter>>>>,
    pub stream_hook: Waterfall<StreamCall, Result<Reply, CallError>>,
}

impl Default for LlmService {
    fn default() -> Self {
        LlmService { adapters: Arc::new(RwLock::new(vec![])), stream_hook: Waterfall::new("llm/stream") }
    }
}

impl LlmService {
    /// Registers an adapter; newer adapters win for providers they both serve.
    pub fn register(&self, a: Arc<dyn LlmAdapter>) -> Disposer {
        let id = a.id().to_string();
        if let Ok(mut l) = self.adapters.write() {
            l.insert(0, a);
        }
        let list = self.adapters.clone();
        Box::new(move || {
            if let Ok(mut l) = list.write() {
                if let Some(i) = l.iter().position(|a| a.id() == id) {
                    l.remove(i);
                }
            }
        })
    }

    pub fn adapter_for(&self, p: &Provider) -> Option<Arc<dyn LlmAdapter>> {
        self.adapters.read().ok()?.iter().find(|a| a.serves(p)).cloned()
    }

    pub fn adapters(&self) -> Vec<String> {
        self.adapters.read().map(|l| l.iter().map(|a| a.id().to_string()).collect()).unwrap_or_default()
    }

    /// Streams one request through the `llm/stream` waterfall and the matching adapter.
    pub fn stream(&self, req: LlmRequest, on: &mut dyn FnMut(Delta), cancel: &AtomicBool) -> Result<Reply, CallError> {
        let on = std::cell::RefCell::new(on);
        let mut call = StreamCall { req };
        self.stream_hook.dispatch(&mut call, &|c: &mut StreamCall| {
            let a = self.adapter_for(&c.req.provider).ok_or_else(|| CallError { message: format!("no model adapter serves provider '{}'", c.req.provider.name), status: None, overflow: false, cancelled: false })?;
            let mut on = on.borrow_mut();
            a.stream(&c.req, &mut **on, cancel)
        })
    }

    /// A whole call without streaming deltas (summaries, titles).
    pub fn complete(&self, req: LlmRequest, cancel: &AtomicBool) -> Result<Reply, CallError> {
        self.stream(req, &mut |_| {}, cancel)
    }
}

/// The OpenAI-compatible adapter (DeepSeek, OpenRouter, Ollama, OpenAI, gateways).
pub struct OpenAiCompatible;

impl LlmAdapter for OpenAiCompatible {
    fn id(&self) -> &str {
        "openai-compatible"
    }
    fn serves(&self, p: &Provider) -> bool {
        !p.is_cli()
    }
    fn reasoning_passback(&self, p: &Provider) -> bool {
        p.base_url.contains("deepseek.com")
    }
    fn stream(&self, req: &LlmRequest, on: &mut dyn FnMut(Delta), cancel: &AtomicBool) -> Result<Reply, CallError> {
        let opt = ChatOptions { temperature: req.temperature, max_tokens: req.max_tokens, reasoning: req.reasoning.clone(), ..Default::default() };
        th_llm::chat_stream(&req.provider, &req.model, &req.wire_messages(), &req.tools, &opt, on, cancel)
    }
}
