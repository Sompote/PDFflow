//! The core services: agents, the agent loop, the model seam, tools, prompt assembly and
//! session storage.

use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::AgentRegistry;
use crate::agent_loop::AgentLoop;
use crate::context::{Context, Plugin};
use crate::keys;
use crate::llm::LlmService;
use crate::prompt::SystemPrompt;
use crate::store::SessionStore;
use crate::tools::ToolRegistry;

pub struct Core {
    pub sessions_dir: Option<PathBuf>,
    pub max_parallel: usize,
    pub max_result_chars: usize,
    pub pin_facts: bool,
}

impl Plugin for Core {
    fn name(&self) -> String {
        "hive-core".into()
    }
    fn describe(&self) -> String {
        format!("agents, agentLoop, llm, tools, systemPrompt, sessions{}", if self.pin_facts { "" } else { "; fact pinning off" })
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        ctx.provide(keys::SESSIONS, Arc::new(SessionStore::new(self.sessions_dir.clone())))?;
        ctx.provide(keys::AGENTS, Arc::new(AgentRegistry::default()))?;
        ctx.provide(keys::LLM, Arc::new(LlmService::default()))?;
        let mut tools = ToolRegistry::default();
        tools.max_parallel = self.max_parallel;
        tools.max_result_chars = self.max_result_chars;
        tools.pin_facts = self.pin_facts;
        ctx.provide(keys::TOOLS, Arc::new(tools))?;
        ctx.provide(keys::PROMPT, Arc::new(SystemPrompt::default()))?;
        ctx.provide(keys::LOOP, Arc::new(AgentLoop::default()))?;
        Ok(())
    }
}
