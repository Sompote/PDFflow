//! Prompt assembly (`ctx.systemPrompt`): plugins register ordered sections; the
//! `system-prompt/assemble` waterfall may rewrite the list before it is joined.

use std::sync::{Arc, RwLock};

use crate::agent::Agent;
use crate::events::{Disposer, Waterfall};
use crate::tools::ToolDef;

pub type SectionFn = dyn Fn(&Agent, &[Arc<ToolDef>]) -> Option<String> + Send + Sync;

pub struct Assemble {
    pub agent: Arc<Agent>,
    /// (order, section id, text)
    pub sections: Vec<(i32, String, String)>,
}

pub struct SystemPrompt {
    sections: Arc<RwLock<Vec<(i32, String, Arc<SectionFn>)>>>,
    pub assemble_hook: Waterfall<Assemble, ()>,
}

impl Default for SystemPrompt {
    fn default() -> Self {
        SystemPrompt { sections: Arc::new(RwLock::new(vec![])), assemble_hook: Waterfall::new("system-prompt/assemble") }
    }
}

impl SystemPrompt {
    /// Adds a section; lower `order` comes first.
    pub fn section(&self, order: i32, id: &str, f: impl Fn(&Agent, &[Arc<ToolDef>]) -> Option<String> + Send + Sync + 'static) -> Disposer {
        let id = id.to_string();
        if let Ok(mut s) = self.sections.write() {
            s.push((order, id.clone(), Arc::new(f)));
        }
        let list = self.sections.clone();
        Box::new(move || {
            if let Ok(mut s) = list.write() {
                s.retain(|(_, i, _)| *i != id);
            }
        })
    }

    pub fn section_ids(&self) -> Vec<String> {
        self.sections.read().map(|s| s.iter().map(|(_, i, _)| i.clone()).collect()).unwrap_or_default()
    }

    /// The prompt for one step, given the tools the step admits.
    pub fn assemble(&self, agent: &Arc<Agent>, tools: &[Arc<ToolDef>]) -> String {
        let fns: Vec<(i32, String, Arc<SectionFn>)> = self.sections.read().map(|s| s.clone()).unwrap_or_default();
        let mut sections: Vec<(i32, String, String)> = fns.into_iter().filter_map(|(o, id, f)| f(agent, tools).map(|t| (o, id, t))).filter(|(_, _, t)| !t.trim().is_empty()).collect();
        sections.sort_by_key(|(o, _, _)| *o);
        let mut a = Assemble { agent: agent.clone(), sections };
        self.assemble_hook.dispatch(&mut a, &|_| ());
        a.sections.into_iter().map(|(_, _, t)| t.trim_end().to_string()).collect::<Vec<_>>().join("\n\n")
    }
}
