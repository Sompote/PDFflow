//! Loop hygiene: the repeat-tool reminder (dsh-repeat-tool-reminder), the continuation
//! guard on `agent/turn-stopping`, and output spill (dsh-spill).

use std::path::PathBuf;

use crate::agent_loop::AgentLoop;
use crate::context::{Context, Plugin};
use crate::keys;
use crate::plugins::SpillLocator;
use crate::tools::ToolRegistry;

/// Reminds the model, without blocking, when it repeats an identical tool call.
pub struct RepeatToolReminder {
    pub thresholds: Vec<usize>,
    pub exclude: Vec<String>,
}

#[derive(Default)]
struct Repeat {
    key: String,
    count: usize,
}

/// Failures in a row per tool: (error text, count).
#[derive(Default)]
struct Failing(std::collections::HashMap<String, (String, usize)>);

impl Plugin for RepeatToolReminder {
    fn name(&self) -> String {
        "hive-repeat-tool-reminder".into()
    }
    fn describe(&self) -> String {
        format!("advisory reminder at {:?} identical calls; ends the turn when one tool fails the same way the agent's fail limit times in a row", self.thresholds)
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::TOOLS, keys::LOOP]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        if self.thresholds.is_empty() || self.thresholds.iter().any(|t| *t < 2) {
            return Err("thresholds must be non-empty and at least 2".into());
        }
        let tools = ctx.expect::<ToolRegistry>(keys::TOOLS)?;
        let lp = ctx.expect::<AgentLoop>(keys::LOOP)?;
        let (th, ex) = (self.thresholds.clone(), self.exclude.clone());
        ctx.effect(tools.post_execute.on(false, move |p, next| {
            // a tool that keeps failing the same way stops the agent: its parent hears why
            let limit = p.agent.preset.fail_limit;
            let err = p.output.is_error.then(|| crate::clip(&p.output.text, 300));
            let n = p.agent.with_state::<Failing, _>("repeat-fail", |f| match &err {
                Some(e) => {
                    let slot = f.0.entry(p.tool.clone()).or_insert_with(|| (e.clone(), 0));
                    if slot.0 != *e {
                        *slot = (e.clone(), 0);
                    }
                    slot.1 += 1;
                    slot.1
                }
                None => {
                    f.0.remove(&p.tool);
                    0
                }
            });
            if limit > 0 && n >= limit {
                p.agent.halt(format!("stopped: {} failed {n} times in a row with the same error: {}", p.tool, err.unwrap_or_default()));
            } else if limit > 0 && n + 1 == limit {
                p.output.additional_context.push(("plugin:repeat-tool-reminder".into(), format!("{} has now failed {n} times with the same error. Read the error and change the call; one more identical failure ends your task.", p.tool)));
            }
            if !ex.contains(&p.tool) {
                // serde_json maps are ordered by key, so property order does not matter
                // a revision counter is bookkeeping, not a different request
                let mut args = p.args.clone();
                if let Some(o) = args.as_object_mut() {
                    o.remove("revision");
                }
                let key = format!("{}|{}", p.tool, args);
                let n = p.agent.with_state::<Repeat, _>("repeat-tool", |r| {
                    if r.key == key {
                        r.count += 1;
                    } else {
                        *r = Repeat { key, count: 1 };
                    }
                    r.count
                });
                if th.contains(&n) {
                    let msg = if n == th[0] {
                        format!("You have called {} {n} times with identical arguments. Look at the previous result before calling it again.", p.tool)
                    } else {
                        format!("You have called {} {n} times in a row with the same arguments:\n{}\nThe result will not change. Change the approach, gather different evidence, or finish with what you have.", p.tool, crate::clip(&p.args.to_string(), 500))
                    };
                    p.output.additional_context.push(("plugin:repeat-tool-reminder".into(), msg));
                }
            }
            next(p)
        }));
        // a new message from the person or the parent clears the count
        ctx.effect(lp.pre_step.on(false, |ps, next| {
            if ps.messages.iter().any(|m| m.source == "user" || m.source == "parent") {
                ps.agent.with_state::<Repeat, _>("repeat-tool", |r| *r = Repeat::default());
                ps.agent.with_state::<Failing, _>("repeat-fail", |f| f.0.clear());
            }
            next(ps)
        }));
        Ok(())
    }
}

/// Continues a turn the model ended too early: a reply cut off at the output limit, an
/// empty reply, or one that only announces the next action ("Let me …").
pub struct ContinuationGuard {
    pub max_nudges: usize,
}

#[derive(Default)]
struct Nudges {
    turn: u64,
    count: usize,
}

/// True when a final reply only announces work instead of reporting it.
pub fn announces_action(s: &str) -> bool {
    let t = s.trim().to_lowercase();
    if t.contains("check: pass") || t.contains("check: fail") {
        return false;
    }
    let last = t.trim_end_matches(['.', ':', ' ']).rsplit(['\n', '.', '!', '?']).map(str::trim).find(|x| !x.is_empty()).unwrap_or("");
    ["let me ", "i'll ", "i will ", "now i'll ", "now i will ", "now let", "next, i", "next i", "i am going to", "i'm going to", "first, i"].iter().any(|k| last.starts_with(k))
}

impl Plugin for ContinuationGuard {
    fn name(&self) -> String {
        "hive-continuation-guard".into()
    }
    fn describe(&self) -> String {
        format!("agent/turn-stopping: up to {} nudges per turn", self.max_nudges)
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::LOOP]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let lp = ctx.expect::<AgentLoop>(keys::LOOP)?;
        let max = self.max_nudges;
        ctx.effect(lp.turn_stopping.on(move |ts| {
            let truncated = ts.finish == "length";
            let empty = ts.text.trim().is_empty();
            if !(truncated || empty || announces_action(&ts.text)) {
                return None;
            }
            let ok = ts.agent.with_state::<Nudges, _>("continuation", |n| {
                if n.turn != ts.turn {
                    *n = Nudges { turn: ts.turn, count: 0 };
                }
                n.count += 1;
                n.count <= max
            });
            if !ok {
                return None;
            }
            let why = if truncated {
                "Your reply was cut off by the output limit."
            } else if empty {
                "Your reply was empty."
            } else {
                "You said what you would do next but did not do it."
            };
            Some(format!("{why} Continue now: make the tool calls you need, then give your final answer with the results."))
        }));
        Ok(())
    }
}

/// Saves long tool outputs to a file and gives the model the head and the file's name.
pub struct Spill {
    pub chars: usize,
    pub locate: Option<SpillLocator>,
    /// Tools whose outputs are never spilled.
    pub exclude: Vec<String>,
}

/// Creates `<dir>/<tool>_<n>.txt` with the first free `n`, so the handle the model has to
/// copy stays short (a long random id is easy to mistype).
fn spill_file(dir: &std::path::Path, tool: &str, text: &str) -> Option<String> {
    use std::io::Write;
    std::fs::create_dir_all(dir).ok()?;
    let tool: String = tool.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    for n in 1..100_000 {
        let name = format!("{tool}_{n}.txt");
        match std::fs::OpenOptions::new().write(true).create_new(true).open(dir.join(&name)) {
            Ok(mut f) => return f.write_all(text.as_bytes()).ok().map(|_| name),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

impl Plugin for Spill {
    fn name(&self) -> String {
        "hive-spill".into()
    }
    fn describe(&self) -> String {
        format!("outputs over {} characters go to a file (not {})", self.chars, self.exclude.join(", "))
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::TOOLS]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let (Some(locate), true) = (self.locate.clone(), self.chars > 0) else { return Ok(()) };
        let tools = ctx.expect::<ToolRegistry>(keys::TOOLS)?;
        let limit = self.chars;
        let exclude = self.exclude.clone();
        ctx.effect(tools.post_execute.on(false, move |p, next| {
            let n = p.output.text.chars().count();
            if n > limit && !exclude.contains(&p.tool) {
                if let Some((dir, shown)) = locate(&p.agent) {
                    let dir: PathBuf = dir;
                    if let Some(name) = spill_file(&dir, &p.tool, &p.output.text) {
                        let head = crate::clip(&p.output.text, limit * 2 / 5);
                        p.output.text = format!("{head}\n…[{n} characters in all; the full output is saved as {shown}/{name}. Read it with read_file when you need the rest]");
                    }
                }
            }
            next(p)
        }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::announces_action;

    #[test]
    fn nudge_detection() {
        assert!(announces_action("The sheet passes. Let me confirm the drawing, then write the final check."));
        assert!(announces_action("Now I'll write the report:"));
        assert!(!announces_action("CHECK: PASS\n1. All good."));
        assert!(!announces_action("phiMn = 146.9 kN m >= Mu = 153 kN m is false, so the beam fails in flexure."));
    }
}
