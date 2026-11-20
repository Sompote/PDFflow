//! OpenAI-compatible chat-completions client with tool calls, image inputs and embeddings.
//!
//! One client talks to every provider that speaks the OpenAI wire format: DeepSeek (the
//! default brain of the harness), OpenRouter, Ollama, OpenAI, vLLM (on this machine or a
//! rented GPU such as Vast.ai) and any self-hosted gateway.
//! The calls are blocking (ureq) because every agent already runs on its own thread.

use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Where a model lives and how to reach it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Provider {
    /// Display name, also the key agents use to pick a provider ("deepseek", "openrouter").
    pub name: String,
    pub base_url: String,
    /// Key stored in the settings file; empty means "read `key_env`".
    pub api_key: String,
    pub key_env: String,
    /// Model used when an agent does not name one.
    pub default_model: String,
    /// Whether the provider's models accept image parts. DeepSeek's API does not.
    pub vision: bool,
    /// How long its models think before each reply for agents left on "provider default":
    /// "" (the model's own default), "off", "low", "medium" or "high". Sent only to OpenRouter,
    /// whose unified `reasoning` field maps it to each model's own switch.
    pub thinking: String,
    /// The server software when it needs its own handling: "vllm", or "" for any other
    /// OpenAI-compatible API.
    pub kind: String,
}

impl Default for Provider {
    fn default() -> Self {
        Provider::deepseek()
    }
}

impl Provider {
    pub fn deepseek() -> Self {
        Provider {
            name: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            api_key: String::new(),
            key_env: "DEEPSEEK_API_KEY".into(),
            default_model: "deepseek-chat".into(),
            vision: false,
            thinking: "off".into(),
            kind: String::new(),
        }
    }
    pub fn openrouter() -> Self {
        Provider {
            name: "openrouter".into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: String::new(),
            key_env: "OPENROUTER_API_KEY".into(),
            default_model: "deepseek/deepseek-v4.1-flash-1017".into(),
            vision: true,
            thinking: "off".into(),
            kind: String::new(),
        }
    }
    pub fn ollama() -> Self {
        Provider {
            name: "ollama".into(),
            base_url: "http://localhost:11434/v1".into(),
            api_key: String::new(),
            key_env: String::new(),
            default_model: "qwen2.5:14b".into(),
            vision: false,
            thinking: "off".into(),
            kind: String::new(),
        }
    }
    pub fn openai() -> Self {
        Provider {
            name: "openai".into(),
            base_url: "https://api.openai.com/v1".into(),
            api_key: String::new(),
            key_env: "OPENAI_API_KEY".into(),
            default_model: "gpt-4.1-mini".into(),
            vision: true,
            thinking: "off".into(),
            kind: String::new(),
        }
    }
    /// vLLM on this machine (`vllm serve <model> --enable-auto-tool-choice
    /// --tool-call-parser <parser>`), or reached through an SSH tunnel to a rented GPU.
    /// An empty default model means "the model the server serves".
    pub fn vllm() -> Self {
        Provider {
            name: "vllm".into(),
            base_url: "http://localhost:8000/v1".into(),
            api_key: String::new(),
            key_env: "VLLM_API_KEY".into(),
            default_model: String::new(),
            vision: false,
            thinking: "off".into(),
            kind: "vllm".into(),
        }
    }
    /// vLLM on a rented Vast.ai GPU, reached at the instance's public IP and the external
    /// port Vast maps to vLLM's port. The key is vLLM's `--api-key`, or the instance's
    /// `OPEN_BUTTON_TOKEN` when the request goes through Vast's authenticating proxy.
    pub fn vllm_vast() -> Self {
        Provider { name: "vllm-vast".into(), base_url: VAST_PLACEHOLDER.into(), key_env: "VAST_VLLM_API_KEY".into(), ..Provider::vllm() }
    }

    pub fn is_vllm(&self) -> bool {
        self.kind.eq_ignore_ascii_case("vllm")
    }

    /// A server on this machine (no key needed).
    pub fn is_local(&self) -> bool {
        let u = self.base_url.to_ascii_lowercase();
        ["://localhost", "://127.", "://[::1]", "://0.0.0.0"].iter().any(|h| u.contains(h))
    }

    /// Whether a call cannot go out without a key. Local servers and vLLM (which runs open
    /// unless started with `--api-key`) can; a missing key then shows as the server's 401.
    pub fn needs_key(&self) -> bool {
        !self.is_cli() && !self.is_local() && !self.is_vllm()
    }

    /// Ready to call: a coding-agent program, or a key when one is needed.
    pub fn ready(&self) -> bool {
        !self.needs_key() || self.key().is_some()
    }

    /// The base URL still holds the preset's placeholder.
    pub fn unconfigured(&self) -> bool {
        self.base_url.contains('<')
    }

    /// Fits the request body to this provider's API. `reasoning` is OpenRouter's (an API such
    /// as OpenAI's rejects an argument it does not know). vLLM switches thinking through the
    /// model's chat template instead (`enable_thinking` for Qwen3, `thinking` for DeepSeek
    /// V3.1; a template ignores a name it does not use) and `reasoning_effort` (gpt-oss).
    pub fn keep_only_known(&self, body: &mut Value) {
        if self.base_url.contains("openrouter.ai") {
            return;
        }
        let Some(o) = body.as_object_mut() else { return };
        let reasoning = o.remove("reasoning");
        if !self.is_vllm() {
            return;
        }
        if let Some(r) = reasoning {
            let on = r.get("enabled") != Some(&Value::Bool(false));
            o.insert("chat_template_kwargs".into(), json!({"enable_thinking": on, "thinking": on}));
            if let Some(e) = r.get("effort").filter(|_| on) {
                o.insert("reasoning_effort".into(), e.clone());
            }
        }
    }
    /// Claude Code run as the model (`claude -p`), on the user's Claude subscription.
    pub fn claude_code() -> Self {
        Provider { name: "claude-code".into(), base_url: "cli://claude-code".into(), api_key: String::new(), key_env: String::new(), default_model: String::new(), vision: false, thinking: String::new(), kind: String::new() }
    }
    /// Codex run as the model (`codex exec`), on the user's ChatGPT subscription.
    pub fn codex() -> Self {
        Provider { name: "codex".into(), base_url: "cli://codex".into(), api_key: String::new(), key_env: String::new(), default_model: String::new(), vision: false, thinking: String::new(), kind: String::new() }
    }
    pub fn presets() -> Vec<Provider> {
        vec![Provider::deepseek(), Provider::openrouter(), Provider::ollama(), Provider::openai(), Provider::vllm(), Provider::vllm_vast(), Provider::claude_code(), Provider::codex()]
    }

    /// A coding-agent program run in place of an API (no key, no HTTP).
    pub fn is_cli(&self) -> bool {
        self.base_url.starts_with("cli://")
    }

    /// The key to send: the stored one, else the environment variable.
    pub fn key(&self) -> Option<String> {
        if !self.api_key.trim().is_empty() {
            return Some(self.api_key.trim().to_string());
        }
        if !self.key_env.is_empty() {
            if let Ok(k) = std::env::var(&self.key_env) {
                if !k.trim().is_empty() {
                    return Some(k.trim().to_string());
                }
            }
        }
        None
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url.trim_end_matches('/'), path)
    }
}

/// Base URL of the Vast.ai preset until the engineer enters the instance's address.
pub const VAST_PLACEHOLDER: &str = "http://<vast-ip>:<port>/v1";

/// The model a server serves, asked once per base URL (vLLM serves one model, under the
/// name given by `--served-model-name`).
fn served_model(p: &Provider) -> Result<String, String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(m) = cache.lock().ok().and_then(|c| c.get(&p.base_url).cloned()) {
        return Ok(m);
    }
    let first = list_models_raw(p).map_err(|e| format!("{}: no model named, and the server's model list failed: {e}", p.name))?.into_iter().next();
    let m = first.ok_or_else(|| format!("{}: no model named, and the server lists none", p.name))?;
    if let Ok(mut c) = cache.lock() {
        c.insert(p.base_url.clone(), m.clone());
    }
    Ok(m)
}

/// The model to call: the one asked for, else the provider's default, else the served one.
pub fn resolve_model(p: &Provider, model: &str) -> Result<String, String> {
    if p.unconfigured() {
        return Err(format!("Set the Base URL of provider '{}' in Settings (for Vast.ai: http://<instance IP>:<external port mapped to vLLM>/v1).", p.name));
    }
    match [model, p.default_model.as_str()].into_iter().map(str::trim).find(|m| !m.is_empty()) {
        Some(m) => Ok(m.to_string()),
        None => served_model(p),
    }
}

/// Adds the likely fix to a vLLM error.
fn explain(p: &Provider, status: Option<u16>, msg: String) -> String {
    if !p.is_vllm() {
        return msg;
    }
    let low = msg.to_ascii_lowercase();
    let hint = if status == Some(401) {
        " (set this provider's API key: vLLM's --api-key, or the instance's OPEN_BUTTON_TOKEN behind Vast's proxy)"
    } else if low.contains("enable-auto-tool-choice") || low.contains("tool-call-parser") || low.contains("tool choice") {
        " (start vLLM with --enable-auto-tool-choice --tool-call-parser <hermes for Qwen, llama3_json, mistral, openai for gpt-oss, ...>)"
    } else if status == Some(404) && low.contains("model") {
        " (the model must match vLLM's --served-model-name; leave Default model empty to use the one it serves)"
    } else if low.contains("connection refused") || low.contains("timed out") || low.contains("dns") {
        " (is the vLLM server up? On Vast.ai use the instance's public IP and the external port mapped to vLLM's port)"
    } else {
        ""
    };
    format!("{msg}{hint}")
}

// ------------------------------------------------------------------ messages

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text { text: String },
    /// A data URI (`data:image/png;base64,...`) or an http(s) URL.
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Part>),
}

impl Content {
    pub fn text(&self) -> String {
        match self {
            Content::Text(s) => s.clone(),
            Content::Parts(p) => p
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
    pub fn has_images(&self) -> bool {
        matches!(self, Content::Parts(p) if p.iter().any(|p| matches!(p, Part::ImageUrl { .. })))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: FunctionCall,
}

fn function_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    /// JSON text as the model wrote it (may be malformed; the harness reports that back).
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// DeepSeek thinking-mode reasoning, passed back inside the turn that produced it
    /// (the harness sets it only for providers that ask for it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl Message {
    pub fn system(s: impl Into<String>) -> Self {
        Message { role: "system".into(), content: Some(Content::Text(s.into())), tool_calls: None, tool_call_id: None, name: None, reasoning_content: None }
    }
    pub fn user(s: impl Into<String>) -> Self {
        Message { role: "user".into(), content: Some(Content::Text(s.into())), tool_calls: None, tool_call_id: None, name: None, reasoning_content: None }
    }
    pub fn user_parts(parts: Vec<Part>) -> Self {
        Message { role: "user".into(), content: Some(Content::Parts(parts)), tool_calls: None, tool_call_id: None, name: None, reasoning_content: None }
    }
    pub fn assistant(s: impl Into<String>) -> Self {
        Message { role: "assistant".into(), content: Some(Content::Text(s.into())), tool_calls: None, tool_call_id: None, name: None, reasoning_content: None }
    }
    pub fn tool(id: &str, s: impl Into<String>) -> Self {
        Message { role: "tool".into(), content: Some(Content::Text(s.into())), tool_calls: None, tool_call_id: Some(id.into()), name: None, reasoning_content: None }
    }
    pub fn text(&self) -> String {
        self.content.as_ref().map(|c| c.text()).unwrap_or_default()
    }
}

/// An image file as an image part (data URI).
pub fn image_part(path: &std::path::Path) -> Result<Part, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(Part::ImageUrl { image_url: ImageUrl { url: format!("data:{mime};base64,{b64}") } })
}

/// A function the model may call.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments (an object schema).
    pub parameters: Value,
}

impl ToolSpec {
    fn wire(&self) -> Value {
        json!({"type": "function", "function": {"name": self.name, "description": self.description, "parameters": self.parameters}})
    }
}

#[derive(Debug, Clone, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Prompt tokens served from the provider's prefix cache (DeepSeek `prompt_cache_hit_tokens`).
    pub cached_tokens: u64,
}

/// The assistant turn the model returned.
#[derive(Debug, Clone)]
pub struct Reply {
    pub message: Message,
    /// DeepSeek-reasoner's thinking, kept for display only (never sent back).
    pub reasoning: Option<String>,
    pub finish_reason: String,
    pub usage: Usage,
}

#[derive(Debug, Clone)]
pub struct ChatOptions {
    pub temperature: Option<f32>,
    /// Thinking: "off", "low", "medium" or "high"; None = the provider's default.
    pub reasoning: Option<String>,
    pub max_tokens: Option<u32>,
    pub timeout: Duration,
}

impl Default for ChatOptions {
    fn default() -> Self {
        ChatOptions { temperature: Some(0.2), reasoning: None, max_tokens: Some(8192), timeout: Duration::from_secs(300) }
    }
}

static STALL_S: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(120);
static REPLY_LIMIT_S: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(900);

/// Limits of a streamed reply: a stream that sends no data for `stall_s` seconds, or a reply
/// still running after `reply_limit_s`, fails as a transport error (so it is retried).
/// 0 leaves that limit off.
pub fn set_stream_limits(stall_s: u64, reply_limit_s: u64) {
    STALL_S.store(stall_s, std::sync::atomic::Ordering::Relaxed);
    REPLY_LIMIT_S.store(reply_limit_s, std::sync::atomic::Ordering::Relaxed);
}

pub fn stream_limits() -> (u64, u64) {
    (STALL_S.load(std::sync::atomic::Ordering::Relaxed), REPLY_LIMIT_S.load(std::sync::atomic::Ordering::Relaxed))
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout(timeout).build()
}

fn http_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v.pointer("/error/message").and_then(|m| m.as_str()).map(String::from))
                .unwrap_or_else(|| body.chars().take(400).collect());
            format!("HTTP {code}: {msg}")
        }
        other => other.to_string(),
    }
}

/// Builds the request body (public so it can be unit-tested and logged).
pub fn request_body(model: &str, messages: &[Message], tools: &[ToolSpec], opt: &ChatOptions) -> Value {
    let mut body = json!({"model": model, "messages": messages});
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.iter().map(|t| t.wire()).collect());
        body["tool_choice"] = json!("auto");
    }
    // the reasoner rejects sampling parameters
    if let Some(t) = opt.temperature {
        if !model.contains("reasoner") {
            body["temperature"] = json!(t);
        }
    }
    if let Some(m) = opt.max_tokens {
        body["max_tokens"] = json!(m);
    }
    if let Some(r) = opt.reasoning.as_deref().filter(|r| !r.is_empty()) {
        // OpenRouter's unified field; it maps it to each provider's own switch
        body["reasoning"] = if r == "off" { json!({"enabled": false}) } else { json!({"effort": r}) };
    }
    body
}

/// Parses a chat-completions response body.
pub fn parse_reply(v: &Value) -> Result<Reply, String> {
    if let Some(err) = v.get("error") {
        return Err(format!("provider error: {}", err.get("message").and_then(|m| m.as_str()).unwrap_or(&err.to_string())));
    }
    let choice = v.pointer("/choices/0").ok_or_else(|| format!("no choices in reply: {}", truncate(&v.to_string(), 300)))?;
    let m = choice.get("message").ok_or("no message in reply")?;
    let content = m.get("content").and_then(|c| c.as_str()).map(|s| Content::Text(s.to_string()));
    let mut tool_calls: Vec<ToolCall> = vec![];
    if let Some(arr) = m.get("tool_calls").and_then(|t| t.as_array()) {
        for (i, tc) in arr.iter().enumerate() {
            let name = tc.pointer("/function/name").and_then(|n| n.as_str()).unwrap_or("").to_string();
            // some providers send arguments as an object instead of a string
            let args = match tc.pointer("/function/arguments") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Null) | None => "{}".into(),
                Some(other) => other.to_string(),
            };
            let id = tc.get("id").and_then(|s| s.as_str()).filter(|s| !s.is_empty()).map(String::from).unwrap_or_else(|| format!("call_{i}"));
            tool_calls.push(ToolCall { id, kind: "function".into(), function: FunctionCall { name, arguments: args } });
        }
    }
    let reasoning = m.get("reasoning_content").or_else(|| m.get("reasoning")).and_then(|r| r.as_str()).filter(|s| !s.is_empty()).map(String::from);
    let usage = Usage {
        prompt_tokens: v.pointer("/usage/prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        completion_tokens: v.pointer("/usage/completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cached_tokens: cached_tokens(v.get("usage")),
    };
    Ok(Reply {
        message: Message {
            role: "assistant".into(),
            content,
            tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
            tool_call_id: None,
            name: None,
            reasoning_content: None,
        },
        reasoning,
        finish_reason: choice.get("finish_reason").and_then(|f| f.as_str()).unwrap_or("").to_string(),
        usage,
    })
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// One chat-completions call. Retries twice on rate limits and server errors.
pub fn chat(p: &Provider, model: &str, messages: &[Message], tools: &[ToolSpec], opt: &ChatOptions) -> Result<Reply, String> {
    let model = resolve_model(p, model)?;
    let mut body = request_body(&model, messages, tools, opt);
    p.keep_only_known(&mut body);
    let a = agent(opt.timeout);
    let mut last = String::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_secs(2 * attempt as u64 + 1));
        }
        let mut req = a.post(&p.url("chat/completions")).set("Content-Type", "application/json");
        if let Some(k) = p.key() {
            req = req.set("Authorization", &format!("Bearer {k}"));
        }
        if p.base_url.contains("openrouter.ai") {
            req = req.set("X-Title", "TellusHive");
        }
        match req.send_json(body.clone()) {
            Ok(r) => {
                let v: Value = r.into_json().map_err(|e| format!("bad JSON from {}: {e}", p.name))?;
                return parse_reply(&v);
            }
            Err(ureq::Error::Status(code, r)) if code == 429 || code >= 500 => {
                last = http_error(ureq::Error::Status(code, r));
            }
            Err(e) => {
                let status = match &e {
                    ureq::Error::Status(c, _) => Some(*c),
                    _ => None,
                };
                return Err(explain(p, status, format!("{}: {}", p.name, http_error(e))));
            }
        }
    }
    Err(format!("{}: {last}", p.name))
}

fn cached_tokens(u: Option<&Value>) -> u64 {
    let Some(u) = u else { return 0 };
    u.get("prompt_cache_hit_tokens").or_else(|| u.pointer("/prompt_tokens_details/cached_tokens")).and_then(|x| x.as_u64()).unwrap_or(0)
}

/// A piece of a streamed reply.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text(String),
    Reasoning(String),
    /// A tool call started (its name is known).
    ToolCall { index: usize, name: String },
}

/// Why a call failed, so the harness can decide to retry or compact.
#[derive(Debug, Clone, PartialEq)]
pub struct CallError {
    pub message: String,
    /// HTTP status, when the provider answered.
    pub status: Option<u16>,
    /// The request was longer than the model's context window.
    pub overflow: bool,
    pub cancelled: bool,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

fn is_overflow(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    ["maximum context length", "context length", "context_length_exceeded", "too many tokens", "prompt is too long", "context window"].iter().any(|k| m.contains(k))
}

/// Folds streamed chunks (`chat.completion.chunk`) into one reply.
#[derive(Default)]
pub struct StreamFold {
    text: String,
    reasoning: String,
    calls: Vec<(String, String, String)>,
    finish: String,
    usage: Usage,
}

impl StreamFold {
    /// Takes one `data:` payload; returns the deltas it carried.
    pub fn push(&mut self, v: &Value) -> Result<Vec<Delta>, String> {
        if let Some(err) = v.get("error") {
            return Err(err.get("message").and_then(|m| m.as_str()).map(String::from).unwrap_or_else(|| err.to_string()));
        }
        let mut out = vec![];
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage = Usage {
                prompt_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                completion_tokens: u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                cached_tokens: cached_tokens(Some(u)),
            };
        }
        let Some(choice) = v.pointer("/choices/0") else { return Ok(out) };
        if let Some(f) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            self.finish = f.to_string();
        }
        let Some(d) = choice.get("delta") else { return Ok(out) };
        if let Some(t) = d.get("content").and_then(|c| c.as_str()).filter(|t| !t.is_empty()) {
            self.text.push_str(t);
            out.push(Delta::Text(t.to_string()));
        }
        if let Some(t) = d.get("reasoning_content").or_else(|| d.get("reasoning")).and_then(|c| c.as_str()).filter(|t| !t.is_empty()) {
            self.reasoning.push_str(t);
            out.push(Delta::Reasoning(t.to_string()));
        }
        if let Some(arr) = d.get("tool_calls").and_then(|t| t.as_array()) {
            for tc in arr {
                let index = tc.get("index").and_then(|i| i.as_u64()).map(|i| i as usize).unwrap_or(self.calls.len().saturating_sub(1));
                while self.calls.len() <= index {
                    self.calls.push(Default::default());
                }
                let slot = &mut self.calls[index];
                if let Some(id) = tc.get("id").and_then(|s| s.as_str()).filter(|s| !s.is_empty()) {
                    slot.0 = id.to_string();
                }
                if let Some(n) = tc.pointer("/function/name").and_then(|s| s.as_str()).filter(|s| !s.is_empty()) {
                    let first = slot.1.is_empty();
                    slot.1.push_str(n);
                    if first {
                        out.push(Delta::ToolCall { index, name: n.to_string() });
                    }
                }
                match tc.pointer("/function/arguments") {
                    Some(Value::String(a)) => slot.2.push_str(a),
                    Some(Value::Null) | None => {}
                    Some(other) => slot.2.push_str(&other.to_string()),
                }
            }
        }
        Ok(out)
    }

    pub fn finish(self) -> Reply {
        let tool_calls: Vec<ToolCall> = self
            .calls
            .into_iter()
            .enumerate()
            .filter(|(_, c)| !c.1.is_empty())
            .map(|(i, (id, name, args))| ToolCall {
                id: if id.is_empty() { format!("call_{i}") } else { id },
                kind: "function".into(),
                function: FunctionCall { name, arguments: if args.trim().is_empty() { "{}".into() } else { args } },
            })
            .collect();
        Reply {
            message: Message {
                role: "assistant".into(),
                content: if self.text.is_empty() && !tool_calls.is_empty() { None } else { Some(Content::Text(self.text)) },
                tool_calls: if tool_calls.is_empty() { None } else { Some(tool_calls) },
                tool_call_id: None,
                name: None,
                reasoning_content: None,
            },
            reasoning: if self.reasoning.is_empty() { None } else { Some(self.reasoning) },
            finish_reason: self.finish,
            usage: self.usage,
        }
    }
}

/// One streamed chat-completions call (server-sent events). `on` sees every delta as it
/// arrives; the reply is the same as `chat` would return. No retries here: the harness
/// owns retry policy.
pub fn chat_stream(p: &Provider, model: &str, messages: &[Message], tools: &[ToolSpec], opt: &ChatOptions, on: &mut dyn FnMut(Delta), cancel: &std::sync::atomic::AtomicBool) -> Result<Reply, CallError> {
    use std::io::BufRead;
    use std::sync::atomic::Ordering;
    let err = |message: String, status: Option<u16>| CallError { overflow: is_overflow(&message), message, status, cancelled: false };
    let model = resolve_model(p, model).map_err(|m| err(m, None))?;
    let mut body = request_body(&model, messages, tools, opt);
    p.keep_only_known(&mut body);
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage": true});
    let (stall_s, limit_s) = stream_limits();
    let stall = (stall_s > 0).then(|| Duration::from_secs(stall_s));
    let limit = (limit_s > 0).then(|| Duration::from_secs(limit_s));
    // no overall timeout here (it would override the read timeout): a silent socket fails
    // after `stall`, and the reply's whole time is checked line by line below
    let mut b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20));
    if let Some(d) = stall {
        b = b.timeout_read(d);
    }
    let mut req = b.build().post(&p.url("chat/completions")).set("Content-Type", "application/json").set("Accept", "text/event-stream");
    if let Some(k) = p.key() {
        req = req.set("Authorization", &format!("Bearer {k}"));
    }
    if p.base_url.contains("openrouter.ai") {
        req = req.set("X-Title", "TellusHive");
    }
    let resp = match req.send_json(body) {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => return Err(err(explain(p, Some(code), format!("{}: {}", p.name, http_error(ureq::Error::Status(code, r)))), Some(code))),
        Err(e) => return Err(err(explain(p, None, format!("{}: {}", p.name, http_error(e))), None)),
    };
    let mut fold = StreamFold::default();
    let (start, mut last_data) = (std::time::Instant::now(), std::time::Instant::now());
    for line in std::io::BufReader::new(resp.into_reader()).lines() {
        if cancel.load(Ordering::Relaxed) {
            return Err(CallError { message: "cancelled".into(), status: None, overflow: false, cancelled: true });
        }
        let line = line.map_err(|e| {
            let timed_out = matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock);
            match stall {
                Some(d) if timed_out => err(format!("{}: the reply stalled (nothing received for {} s)", p.name, d.as_secs()), None),
                _ => err(format!("{}: stream broke: {e}", p.name), None),
            }
        })?;
        if let Some(l) = limit.filter(|l| start.elapsed() > *l) {
            return Err(err(format!("{}: the reply ran past its limit of {} min", p.name, l.as_secs() / 60), None));
        }
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            // keep-alive comments (": OPENROUTER PROCESSING") while nothing else arrives
            if let Some(d) = stall.filter(|d| last_data.elapsed() > *d) {
                return Err(err(format!("{}: the reply stalled (only keep-alive messages for {} s)", p.name, d.as_secs()), None));
            }
            continue;
        };
        last_data = std::time::Instant::now();
        if data == "[DONE]" {
            break;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else { continue };
        for d in fold.push(&v).map_err(|m| err(format!("{}: {m}", p.name), None))? {
            on(d);
        }
    }
    Ok(fold.finish())
}

/// Embeddings through the OpenAI `/embeddings` endpoint.
pub fn embed(p: &Provider, model: &str, inputs: &[String]) -> Result<Vec<Vec<f32>>, String> {
    let a = agent(Duration::from_secs(120));
    let mut req = a.post(&p.url("embeddings"));
    if let Some(k) = p.key() {
        req = req.set("Authorization", &format!("Bearer {k}"));
    }
    let v: Value = req.send_json(json!({"model": model, "input": inputs})).map_err(http_error)?.into_json().map_err(|e| e.to_string())?;
    let data = v.get("data").and_then(|d| d.as_array()).ok_or("no data in embeddings reply")?;
    Ok(data
        .iter()
        .map(|d| d.get("embedding").and_then(|e| e.as_array()).map(|e| e.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect()).unwrap_or_default())
        .collect())
}

/// Lists the models a provider serves (for the settings combo box).
pub fn list_models(p: &Provider) -> Result<Vec<String>, String> {
    if p.base_url == "cli://claude-code" {
        return Ok(vec!["sonnet".into(), "opus".into(), "haiku".into()]);
    }
    if p.is_cli() {
        return Ok(vec![]);
    }
    if p.unconfigured() {
        return Err(format!("Set the Base URL of provider '{}' first.", p.name));
    }
    list_models_raw(p).map_err(|e| explain(p, None, e))
}

fn list_models_raw(p: &Provider) -> Result<Vec<String>, String> {
    let a = agent(Duration::from_secs(20));
    let mut req = a.get(&p.url("models"));
    if let Some(k) = p.key() {
        req = req.set("Authorization", &format!("Bearer {k}"));
    }
    let v: Value = req.call().map_err(http_error)?.into_json().map_err(|e| e.to_string())?;
    let mut out: Vec<String> = v.get("data").and_then(|d| d.as_array()).map(|a| a.iter().filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from)).collect()).unwrap_or_default();
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_has_tools_and_no_temperature_for_reasoner() {
        let t = ToolSpec { name: "f".into(), description: "d".into(), parameters: json!({"type":"object","properties":{}}) };
        let b = request_body("deepseek-reasoner", &[Message::user("hi")], &[t], &ChatOptions::default());
        assert_eq!(b["tools"][0]["function"]["name"], "f");
        assert!(b.get("temperature").is_none());
        let b = request_body("deepseek-chat", &[Message::user("hi")], &[], &ChatOptions::default());
        assert!(b.get("tools").is_none());
        assert!(b.get("temperature").is_some());
    }

    #[test]
    fn parses_tool_calls_and_reasoning() {
        let v = json!({"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":null,"reasoning_content":"think",
            "tool_calls":[{"id":"c1","type":"function","function":{"name":"search","arguments":"{\"q\":\"x\"}"}},
                          {"function":{"name":"obj","arguments":{"a":1}}}]}}],"usage":{"prompt_tokens":10,"completion_tokens":5}});
        let r = parse_reply(&v).unwrap();
        let tc = r.message.tool_calls.unwrap();
        assert_eq!(tc.len(), 2);
        assert_eq!(tc[0].function.arguments, "{\"q\":\"x\"}");
        assert_eq!(tc[1].id, "call_1");
        assert_eq!(tc[1].function.arguments, "{\"a\":1}");
        assert_eq!(r.reasoning.as_deref(), Some("think"));
        assert_eq!(r.usage.prompt_tokens, 10);
    }

    #[test]
    fn reasoning_switch_in_the_body() {
        let mut o = ChatOptions::default();
        assert!(request_body("m", &[], &[], &o).get("reasoning").is_none());
        o.reasoning = Some("off".into());
        assert_eq!(request_body("m", &[], &[], &o)["reasoning"], json!({"enabled": false}));
        o.reasoning = Some("high".into());
        assert_eq!(request_body("m", &[], &[], &o)["reasoning"]["effort"], "high");
    }

    /// The thinking level reaches OpenRouter only; a saved provider without the field thinks
    /// with thinking off.
    #[test]
    fn thinking_goes_to_openrouter_only() {
        let o = ChatOptions { reasoning: Some("low".into()), ..Default::default() };
        let mut b = request_body("m", &[], &[], &o);
        Provider::openrouter().keep_only_known(&mut b);
        assert_eq!(b["reasoning"]["effort"], "low");
        for p in [Provider::openai(), Provider::deepseek(), Provider::ollama()] {
            let mut b = request_body("m", &[], &[], &o);
            p.keep_only_known(&mut b);
            assert!(b.get("reasoning").is_none(), "{} gets no reasoning field", p.name);
        }
        let old: Provider = serde_json::from_value(json!({"name": "openrouter", "base_url": "https://openrouter.ai/api/v1", "api_key": "", "key_env": "", "default_model": "x", "vision": true})).unwrap();
        assert_eq!(old.thinking, "off");
        assert_eq!(Provider::openrouter().thinking, "off");
        assert!(Provider::claude_code().thinking.is_empty());
    }

    /// vLLM gets thinking through the chat template and gpt-oss's reasoning_effort, never
    /// OpenRouter's `reasoning` field.
    #[test]
    fn vllm_thinking_goes_through_the_template() {
        let p = Provider::vllm();
        let mut b = request_body("m", &[], &[], &ChatOptions { reasoning: Some("off".into()), ..Default::default() });
        p.keep_only_known(&mut b);
        assert!(b.get("reasoning").is_none());
        assert_eq!(b["chat_template_kwargs"], json!({"enable_thinking": false, "thinking": false}));
        assert!(b.get("reasoning_effort").is_none());
        let mut b = request_body("m", &[], &[], &ChatOptions { reasoning: Some("high".into()), ..Default::default() });
        p.keep_only_known(&mut b);
        assert_eq!(b["chat_template_kwargs"]["enable_thinking"], true);
        assert_eq!(b["reasoning_effort"], "high");
        let mut b = request_body("m", &[], &[], &ChatOptions::default());
        p.keep_only_known(&mut b);
        assert!(b.get("chat_template_kwargs").is_none(), "provider default leaves the template alone");
    }

    /// Local servers and vLLM run without a key; a remote API needs one; the Vast preset
    /// asks for its address before any call goes out.
    #[test]
    fn which_providers_need_a_key() {
        assert!(!Provider::ollama().needs_key());
        assert!(!Provider::vllm().needs_key());
        let vast = Provider { base_url: "http://203.0.113.7:41234/v1".into(), ..Provider::vllm_vast() };
        assert!(!vast.needs_key() && vast.ready() && !vast.is_local());
        assert!(Provider::openai().needs_key());
        assert!(!Provider::claude_code().needs_key());
        assert!(Provider::vllm_vast().unconfigured());
        assert!(resolve_model(&Provider::vllm_vast(), "").unwrap_err().contains("Base URL"));
        assert_eq!(resolve_model(&Provider::vllm(), "Qwen/Qwen3-32B").unwrap(), "Qwen/Qwen3-32B");
        let old: Provider = serde_json::from_value(json!({"name": "x", "base_url": "https://a/v1"})).unwrap();
        assert!(!old.is_vllm());
        let e = explain(&Provider::vllm(), Some(400), "vllm: HTTP 400: \"auto\" tool choice requires --enable-auto-tool-choice and --tool-call-parser to be set".into());
        assert!(e.contains("--tool-call-parser <hermes"), "{e}");
    }

    /// With no model named, the call uses the one the server lists.
    #[test]
    fn empty_model_uses_the_served_one() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = c.read(&mut buf).unwrap();
            assert!(String::from_utf8_lossy(&buf[..n]).starts_with("GET /v1/models"));
            let body = r#"{"object":"list","data":[{"id":"qwen3-32b","max_model_len":32768}]}"#;
            let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
        });
        let p = Provider { base_url: format!("http://127.0.0.1:{port}/v1"), ..Provider::vllm() };
        assert_eq!(resolve_model(&p, "").unwrap(), "qwen3-32b");
        assert_eq!(resolve_model(&p, " ").unwrap(), "qwen3-32b", "asked once, then cached");
    }

    #[test]
    fn folds_a_stream() {
        let mut f = StreamFold::default();
        let chunks = [
            json!({"choices":[{"delta":{"reasoning_content":"hm"}}]}),
            json!({"choices":[{"delta":{"content":"Hel"}}]}),
            json!({"choices":[{"delta":{"content":"lo"}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c9","function":{"name":"calc","arguments":"{\"a\""}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3,"prompt_cache_hit_tokens":5}}),
        ];
        let mut deltas = vec![];
        for c in &chunks {
            deltas.extend(f.push(c).unwrap());
        }
        assert_eq!(deltas[2], Delta::Text("lo".into()));
        assert_eq!(deltas[3], Delta::ToolCall { index: 0, name: "calc".into() });
        let r = f.finish();
        assert_eq!(r.message.text(), "Hello");
        assert_eq!(r.reasoning.as_deref(), Some("hm"));
        let tc = r.message.tool_calls.unwrap();
        assert_eq!((tc[0].id.as_str(), tc[0].function.arguments.as_str()), ("c9", "{\"a\":1}"));
        assert_eq!(r.finish_reason, "tool_calls");
        assert_eq!((r.usage.prompt_tokens, r.usage.cached_tokens), (7, 5));
        assert!(is_overflow("This model's maximum context length is 65536 tokens"));
    }

    #[test]
    fn message_serialises_like_openai() {
        let m = Message::user_parts(vec![Part::Text { text: "see".into() }, Part::ImageUrl { image_url: ImageUrl { url: "data:x".into() } }]);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["content"][1]["type"], "image_url");
        assert_eq!(v["content"][1]["image_url"]["url"], "data:x");
        let back: Message = serde_json::from_value(v).unwrap();
        assert!(back.content.unwrap().has_images());
    }

    /// A reply that goes silent, or sends only keep-alive comments, fails as a transport
    /// error after the stall limit instead of waiting for ever.
    #[test]
    fn stalled_stream_is_abandoned() {
        use std::io::{Read, Write};
        set_stream_limits(1, 0);
        for keepalive in [false, true] {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            std::thread::spawn(move || {
                let (mut c, _) = l.accept().unwrap();
                let mut buf = [0u8; 65536];
                let _ = c.read(&mut buf);
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
                for _ in 0..40 {
                    if keepalive {
                        let _ = c.write_all(b": OPENROUTER PROCESSING\n\n");
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            });
            let p = Provider { base_url: format!("http://127.0.0.1:{port}/v1"), ..Provider::ollama() };
            let t0 = std::time::Instant::now();
            let e = chat_stream(&p, "m", &[Message::user("hi")], &[], &ChatOptions::default(), &mut |_| {}, &std::sync::atomic::AtomicBool::new(false)).unwrap_err();
            assert!(e.message.contains("stalled"), "{}", e.message);
            assert!(e.status.is_none() && !e.cancelled);
            assert!(t0.elapsed() < Duration::from_secs(3), "{:?}", t0.elapsed());
        }
        set_stream_limits(120, 900);
    }
}
