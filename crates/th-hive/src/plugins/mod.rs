//! The base bundle (DeepSeek Harness `dsh-base` in spirit): the plugins every profile
//! starts from. An application adds its own plugins after these and may switch any of
//! them off by name (`BaseConfig::disabled`), the way a dsh profile patch removes a row.

pub mod audit;
pub mod compaction;
pub mod core;
pub mod guards;
pub mod llm;
pub mod subagent;
pub mod team_board;
pub mod todo;

use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::Agent;
use crate::context::Plugin;

pub type SpillLocator = Arc<dyn Fn(&Agent) -> Option<(PathBuf, String)> + Send + Sync>;

/// Configuration of the base bundle.
#[derive(Clone)]
pub struct BaseConfig {
    /// Folder for session files; `None` keeps sessions in memory.
    pub sessions_dir: Option<PathBuf>,
    /// Calls of one reply that may run at once.
    pub max_parallel: usize,
    /// Longest tool result the model sees, in characters.
    pub max_result_chars: usize,
    /// Estimated prompt tokens above which compaction runs before a step.
    pub compaction_budget_tokens: usize,
    /// Surface nodes kept verbatim at the end of the history.
    pub compaction_keep_recent: usize,
    /// Repeat counts of an identical tool call that trigger a reminder.
    pub repeat_thresholds: Vec<usize>,
    /// Retries of a failed model request (rate limits, server errors, broken streams).
    pub retries: u32,
    /// Tool results longer than this (characters) are spilled to a file; 0 = never.
    pub spill_chars: usize,
    /// Where an agent's spilled outputs go: (folder, how the model should name it).
    pub spill_locator: Option<SpillLocator>,
    /// Deepest delegation (the root agent is depth 0).
    pub max_depth: usize,
    /// Plugin names left out of the tree.
    pub disabled: Vec<String>,
    /// Log the facts tools pin to their results and audit final answers against them
    /// (off only for ablation studies).
    pub pin_facts: bool,
    /// Tools whose outputs are never spilled to a file (reading a file back must not spill).
    pub spill_exclude: Vec<String>,
}

impl Default for BaseConfig {
    fn default() -> Self {
        BaseConfig {
            sessions_dir: None,
            max_parallel: 8,
            max_result_chars: 20_000,
            compaction_budget_tokens: 90_000,
            compaction_keep_recent: 8,
            repeat_thresholds: vec![3, 5, 8],
            retries: 3,
            spill_chars: 12_000,
            spill_locator: None,
            max_depth: 3,
            disabled: vec![],
            pin_facts: true,
            spill_exclude: vec!["read_file".into()],
        }
    }
}

/// The base plugins, in mount order.
pub fn base_bundle(cfg: &BaseConfig) -> Vec<Arc<dyn Plugin>> {
    let all: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(core::Core { sessions_dir: cfg.sessions_dir.clone(), max_parallel: cfg.max_parallel, max_result_chars: cfg.max_result_chars, pin_facts: cfg.pin_facts }),
        Arc::new(llm::OpenAiAdapter),
        Arc::new(llm::Retry { attempts: cfg.retries }),
        Arc::new(compaction::Compaction { budget_tokens: cfg.compaction_budget_tokens, keep_recent: cfg.compaction_keep_recent, prune_min_chars: 600, pin_chars: 16_000 }),
        Arc::new(guards::RepeatToolReminder { thresholds: cfg.repeat_thresholds.clone(), exclude: vec!["todo_write".into(), "task_list".into()] }),
        Arc::new(guards::ContinuationGuard { max_nudges: 2 }),
        Arc::new(guards::Spill { chars: cfg.spill_chars, locate: cfg.spill_locator.clone(), exclude: cfg.spill_exclude.clone() }),
        Arc::new(todo::Todo),
        Arc::new(subagent::SubagentPlugin { max_depth: cfg.max_depth }),
        Arc::new(team_board::TeamBoardPlugin),
    ];
    let mut all = all;
    if cfg.pin_facts {
        all.push(Arc::new(audit::FactAudit { max_nudges: 1, min_digits: 3 }));
    }
    all.into_iter().filter(|p| !cfg.disabled.contains(&p.name())).collect()
}
