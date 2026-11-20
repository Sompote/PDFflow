//! The Assistant: an AI chat beside the document, ported from Aedifex. It runs on TellusHive's
//! agent harness (`crates/th-hive`, a Rust port of DeepSeek Harness) with the OpenAI-compatible
//! client of `crates/th-llm` (DeepSeek, OpenAI, OpenRouter, Ollama, vLLM), or with Claude Code /
//! Codex as the model, which reach the same PDFflow tools over MCP.
//!
//! - [`engine`]: one chat message as a th-hive turn whose tools run in the editor.
//! - [`cli`]: Claude Code and Codex as the model, their event streams parsed into the chat.
//! - [`settings`]: providers, models, keys and options.
//! - [`prompt`]: the working rules and the per-message context.
//! - [`transcript`]: what the chat shows, live or rebuilt from a session log.
//! - [`chat`], [`markdown`]: the panel.

pub mod chat;
pub mod cli;
pub mod engine;
pub mod markdown;
pub mod prompt;
pub mod settings;
pub mod transcript;
