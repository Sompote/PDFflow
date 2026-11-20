//! Fact audit: a deterministic check of an agent's final answer against its pinned facts.
//!
//! When a turn is about to end, every number in the answer written with three or more
//! significant digits must match, at the precision written, a value pinned in force in the
//! session or a number the engineer (or the parent agent) gave. Any that does not is listed
//! back to the agent once per turn with a request to verify it against the files, which
//! catches values a summary or the model's own recall got wrong. The audit is a
//! `facts/audit` custom event either way, so its effect can be measured from the log. With
//! nothing pinned there is nothing to check against, and the audit stays silent.

use serde_json::json;

use crate::agent_loop::AgentLoop;
use crate::context::{Context, Plugin};
use crate::facts::{self, Shown};
use crate::keys;
use crate::session::{EventBody, Session};

pub const KIND: &str = "facts/audit";

pub struct FactAudit {
    /// Audit nudges per turn.
    pub max_nudges: usize,
    /// Numbers with fewer significant digits are not checked (case numbers, angles, widths).
    pub min_digits: usize,
}

#[derive(Default)]
struct Count {
    turn: u64,
    n: usize,
}

/// The numbers of `answer` that match neither a pinned value in force nor a number given in
/// a user or parent message.
pub fn unverified(session: &Session, answer: &str, min_digits: usize) -> Option<(Vec<String>, usize)> {
    let records = session.fact_records();
    if records.is_empty() {
        return None;
    }
    let mut known: Vec<Shown> = vec![];
    for r in facts::current(&records) {
        for f in &r.set.facts {
            known.extend(facts::numbers(&f.value));
            known.extend(facts::numbers(&f.note));
        }
    }
    for e in session.events() {
        if let EventBody::UserMessage { text, source, .. } = &e.body {
            if source == "user" || source == "parent" {
                known.extend(facts::numbers(text));
            }
        }
    }
    let mut bad: Vec<String> = vec![];
    let mut checked = 0;
    for n in facts::numbers(answer).into_iter().filter(|n| n.digits >= min_digits) {
        checked += 1;
        if !known.iter().any(|k| facts::same_number(n, *k)) {
            let s = if n.decimals > 0 { format!("{:.*}", n.decimals as usize, n.value) } else { format!("{}", n.value) };
            if !bad.contains(&s) {
                bad.push(s);
            }
        }
    }
    Some((bad, checked))
}

impl Plugin for FactAudit {
    fn name(&self) -> String {
        "hive-fact-audit".into()
    }
    fn describe(&self) -> String {
        format!("agent/turn-stopping: numbers of {}+ digits checked against pinned facts, {} nudge per turn", self.min_digits, self.max_nudges)
    }
    fn inject(&self) -> Vec<&'static str> {
        vec![keys::LOOP]
    }
    fn apply(&self, ctx: &Context) -> Result<(), String> {
        let lp = ctx.expect::<AgentLoop>(keys::LOOP)?;
        let (max, min_digits) = (self.max_nudges, self.min_digits);
        ctx.effect(lp.turn_stopping.on(move |ts| {
            if ts.text.trim().is_empty() {
                return None;
            }
            let (bad, checked) = unverified(&ts.agent.session, &ts.text, min_digits)?;
            let nudge = !bad.is_empty()
                && ts.agent.with_state::<Count, _>("fact-audit", |c| {
                    if c.turn != ts.turn {
                        *c = Count { turn: ts.turn, n: 0 };
                    }
                    c.n += 1;
                    c.n <= max
                });
            ts.agent.session.append(EventBody::Custom { kind: KIND.into(), data: json!({"turn": ts.turn, "checked": checked, "unverified": bad, "nudged": nudge}) });
            if !nudge {
                return None;
            }
            let list = bad.iter().take(20).cloned().collect::<Vec<_>>().join(", ");
            Some(format!(
                "Fact check: these numbers in your answer match no value a tool returned in this conversation and no number the engineer gave: {list}. Check each one against the saved calculations (read_file) or the tool results, correct any that are wrong, and give your whole final answer again. Keep a number that you derived yourself (a sum, a ratio) or that comes from a tool output without pinned values, and say where it comes from."
            ))
        }));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Emit;
    use crate::facts::{Fact, FactRecord, FactSet};
    use crate::session::SessionMeta;
    use std::sync::Arc;

    #[test]
    fn flags_only_numbers_that_no_pin_or_input_supports() {
        let s = Session::memory(SessionMeta { id: "a".into(), agent: "a".into(), ..Default::default() }, Arc::new(Emit::new("session/event")));
        let quiet = unverified(&s, "q_ult = 471.4 kPa", 3);
        assert!(quiet.is_none(), "nothing pinned, nothing to check");
        s.append(EventBody::UserMessage { text: "column load 800 kN, gamma = 18".into(), images: vec![], source: "user".into(), surface_op: None });
        let set = FactSet { group: "calcs/case2".into(), facts: vec![Fact::new("result", "q_ult", "468.5", "kPa").key(), Fact::new("result", "Q_all", "1200", "kN").key()], ..Default::default() };
        s.append(EventBody::Custom { kind: facts::KIND.into(), data: serde_json::to_value(FactRecord { source: 1, tool: "t".into(), set }).unwrap() });
        let (bad, checked) = unverified(&s, "case2: q_ult = 471.4 kPa, Q_all = 1,200 kN; carries 800 kN at B = 1.5 m, phi = 28", 3).unwrap();
        assert_eq!(bad, ["471.4"]);
        assert_eq!(checked, 3);
        assert!(unverified(&s, "q_ult = 468.5 kPa and Q_all = 1200.3 kN", 3).unwrap().0.is_empty());
    }
}
