//! Pinned facts: values a tool returned that must survive compaction exactly.
//!
//! A tool hands back a [`FactSet`] with its result (`ToolOutput::facts`). The pipeline logs
//! it as a `custom` event of kind [`KIND`] right after the `tool/result`, with the seq of
//! that result as its source, so every fact is traceable to one tool call. A later set of
//! the same `group` replaces the earlier one (a calculation that was run again). Nothing
//! is shown to the model while the result itself is still in the history; the facts come
//! back verbatim when the result is pruned or summarised away (see `plugins::compaction`
//! and `Session::surface`).

use serde::{Deserialize, Serialize};

use crate::session::Seq;

/// The `custom` event kind of a recorded fact set.
pub const KIND: &str = "facts/set";

/// One pinned value. `value` is the text the tool showed, so the model reads the same
/// figure before and after compaction.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Fact {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub unit: String,
    /// "input", "result" or "check"; anything else is listed under its own heading.
    pub kind: String,
    /// Short context, e.g. the demand, capacity and ratio of a check.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// A headline result (a declared output), kept when the set is shown in compact form.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub key: bool,
}

impl Fact {
    pub fn new(kind: &str, name: impl Into<String>, value: impl Into<String>, unit: impl Into<String>) -> Fact {
        Fact { name: name.into(), value: value.into(), unit: unit.into(), kind: kind.into(), note: String::new(), key: false }
    }
    pub fn key(mut self) -> Fact {
        self.key = true;
        self
    }
    pub fn note(mut self, note: impl Into<String>) -> Fact {
        self.note = note.into();
        self
    }
}

/// The facts of one tool result.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FactSet {
    /// What the facts belong to; a later set of the same group replaces this one.
    pub group: String,
    /// Where the full record is, as the model should name it (e.g. `calcs/B1.json`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub origin: String,
    /// One line about the whole set, e.g. the title and the overall verdict.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub headline: String,
    pub facts: Vec<Fact>,
}

/// A fact set as logged: with the tool and the `tool/result` it came from.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct FactRecord {
    /// Seq of the `tool/result` event that produced the facts.
    pub source: Seq,
    pub tool: String,
    #[serde(flatten)]
    pub set: FactSet,
}

/// The records in force: the last one of each group, oldest first.
pub fn current(all: &[FactRecord]) -> Vec<&FactRecord> {
    let mut out: Vec<&FactRecord> = vec![];
    for r in all {
        out.retain(|o| o.set.group != r.set.group);
        out.push(r);
    }
    out
}

/// Whether a later record replaced this one.
pub fn superseded(all: &[FactRecord], r: &FactRecord) -> bool {
    all.iter().any(|o| o.set.group == r.set.group && o.source > r.source)
}

fn one(f: &Fact) -> String {
    let unit = if f.unit.is_empty() { String::new() } else { format!(" {}", f.unit) };
    let note = if f.note.is_empty() { String::new() } else { format!(" ({})", f.note) };
    if f.name.is_empty() {
        return format!("{}{unit}{note}", f.value);
    }
    format!("{} = {}{unit}{note}", f.name, f.value)
}

/// One record as text: a heading line, then one line per kind of fact.
pub fn render(r: &FactRecord) -> String {
    render_with(r, |_| true, "")
}

/// The record with its inputs, headline results and checks only (intermediate steps left
/// out), or `None` when that would leave nothing out.
pub fn render_compact(r: &FactRecord) -> Option<String> {
    let keep = |f: &Fact| f.kind != "result" || f.key;
    if r.set.facts.iter().all(keep) {
        return None;
    }
    Some(render_with(r, keep, " (intermediate steps left out)"))
}

fn render_with(r: &FactRecord, keep: impl Fn(&Fact) -> bool, mark: &str) -> String {
    let s = &r.set;
    let mut head = if s.origin.is_empty() { s.group.clone() } else { s.origin.clone() };
    if !r.tool.is_empty() {
        head.push_str(&format!(" [{}]", r.tool));
    }
    if !s.headline.is_empty() {
        head.push_str(&format!(": {}", s.headline));
    }
    head.push_str(mark);
    let mut out = head;
    let mut kinds: Vec<&str> = vec![];
    for f in s.facts.iter().filter(|f| keep(f)) {
        if !kinds.contains(&f.kind.as_str()) {
            kinds.push(&f.kind);
        }
    }
    for k in kinds {
        let label = match k {
            "input" => "inputs",
            "result" => "results",
            "check" => "checks",
            "" => "values",
            other => other,
        };
        let items: Vec<String> = s.facts.iter().filter(|f| f.kind == k && keep(f)).map(one).collect();
        out.push_str(&format!("\n  {label}: {}", items.join("; ")));
    }
    out
}

/// The block that follows a compaction summary: the records in force. When they do not
/// all fit in `max_chars`, the newest are shown in full, older ones in compact form
/// (inputs, headline results and checks), and only what still does not fit is named by
/// file. Empty when there is nothing to pin.
pub fn render_block(records: &[&FactRecord], max_chars: usize) -> String {
    if records.is_empty() {
        return String::new();
    }
    let mut kept: Vec<String> = vec![];
    let mut left_out: Vec<String> = vec![];
    let mut used = 0;
    for r in records.iter().rev() {
        let full = render(r);
        let text = if kept.is_empty() || used + full.len() <= max_chars {
            Some(full)
        } else {
            render_compact(r).filter(|c| used + c.len() <= max_chars)
        };
        match text {
            Some(t) if left_out.is_empty() => {
                used += t.len();
                kept.push(t);
            }
            _ => left_out.push(if r.set.origin.is_empty() { r.set.group.clone() } else { r.set.origin.clone() }),
        }
    }
    kept.reverse();
    left_out.reverse();
    let mut out = String::from("[Pinned values: copied exactly from the tool results of the earlier conversation. Where the summary above differs from them, these are right.]\n");
    out.push_str(&kept.join("\n"));
    if !left_out.is_empty() {
        let n = if left_out.len() == 1 { "1 older set is".to_string() } else { format!("{} older sets are", left_out.len()) };
        out.push_str(&format!("\n[{n} not listed to save context; read the files before using their values: {}]", left_out.join(", ")));
    }
    out
}

/// A number as written: its value and how many decimal places it shows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shown {
    pub value: f64,
    pub decimals: i32,
    /// Significant digits written.
    pub digits: usize,
}

/// The numbers in a text, as written. A number glued to a letter or underscore before it
/// (`case16`, `N_2`) is a label, not a value, and thousands separators (`1,068`) are read.
pub fn numbers(text: &str) -> Vec<Shown> {
    let cs: Vec<char> = text.chars().collect();
    let mut out = vec![];
    let mut i = 0;
    while i < cs.len() {
        let starts = cs[i].is_ascii_digit() && !(i > 0 && (cs[i - 1].is_alphanumeric() || cs[i - 1] == '_' || cs[i - 1] == '.'));
        if !starts {
            i += 1;
            continue;
        }
        let mut j = i;
        let mut s = String::new();
        while j < cs.len() {
            let c = cs[j];
            if c.is_ascii_digit() || c == '.' {
                s.push(c);
            } else if c == ',' && j + 3 < cs.len() + 1 && cs.get(j + 1).is_some_and(|d| d.is_ascii_digit()) && cs.get(j + 2).is_some_and(|d| d.is_ascii_digit()) && cs.get(j + 3).is_some_and(|d| d.is_ascii_digit()) && !cs.get(j + 4).is_some_and(|d| d.is_ascii_digit()) {
                // thousands separator
            } else {
                break;
            }
            j += 1;
        }
        let s = s.trim_end_matches('.').to_string();
        let neg = i > 0 && cs[i - 1] == '-' && !(i > 1 && cs[i - 2].is_alphanumeric());
        if let Ok(v) = s.parse::<f64>() {
            let decimals = s.split_once('.').map(|(_, d)| d.len() as i32).unwrap_or(0);
            let digits = s.chars().filter(|c| c.is_ascii_digit()).collect::<String>().trim_start_matches('0').len().max(1);
            out.push(Shown { value: if neg { -v } else { v }, decimals, digits });
        }
        i = j.max(i + 1);
    }
    out
}

/// Whether two written numbers are the same value at the coarser of their two precisions.
pub fn same_number(a: Shown, b: Shown) -> bool {
    let d = a.decimals.min(b.decimals);
    let tol = 0.5 * 10f64.powi(-d) + 1e-9 * a.value.abs().max(b.value.abs());
    (a.value - b.value).abs() <= tol
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(source: Seq, group: &str, v: &str) -> FactRecord {
        FactRecord { source, tool: "calc_sheet".into(), set: FactSet { group: group.into(), origin: format!("calcs/{group}.json"), headline: "all checks pass".into(), facts: vec![Fact::new("input", "b", "300", "mm"), Fact::new("result", "Mu", v, "kN*m"), Fact::new("check", "Flexure", "PASS", "").note("ratio 0.812")] } }
    }

    #[test]
    fn later_set_of_a_group_replaces_the_earlier_one() {
        let all = vec![rec(5, "B1", "120.5"), rec(9, "F1", "88"), rec(14, "B1", "131.2")];
        let cur = current(&all);
        assert_eq!(cur.iter().map(|r| r.source).collect::<Vec<_>>(), [9, 14]);
        assert!(superseded(&all, &all[0]) && !superseded(&all, &all[2]));
    }

    #[test]
    fn renders_by_kind_and_reports_what_did_not_fit() {
        let all = vec![rec(5, "B1", "120.5"), rec(9, "F1", "88")];
        let text = render(&all[0]);
        assert_eq!(text, "calcs/B1.json [calc_sheet]: all checks pass\n  inputs: b = 300 mm\n  results: Mu = 120.5 kN*m\n  checks: Flexure = PASS (ratio 0.812)");
        let block = render_block(&current(&all), 10);
        assert!(block.contains("Mu = 88 kN*m") && !block.contains("120.5"));
        assert!(block.contains("1 older set is not listed") && block.contains("calcs/B1.json"));
        assert_eq!(render_block(&[], 1000), "");
    }

    #[test]
    fn compact_form_keeps_inputs_key_results_and_checks() {
        let mut r = rec(5, "B1", "120.5");
        r.set.facts.push(Fact::new("result", "N_q", "14.72", ""));
        r.set.facts[1].key = true;
        let c = render_compact(&r).unwrap();
        assert!(c.contains("Mu = 120.5") && c.contains("b = 300") && c.contains("Flexure = PASS") && !c.contains("N_q"), "{c}");
        // over the budget: the newest in full, the older ones compact before they are dropped
        let mut old = rec(1, "A0", "99.1");
        for k in 0..8 {
            old.set.facts.push(Fact::new("result", format!("N_{k}q"), "14.72", ""));
        }
        old.set.facts[1].key = true;
        let new = rec(9, "F1", "88");
        let full_len = render(&new).len();
        let block = render_block(&[&old, &new], full_len + render_compact(&old).unwrap().len() + 5);
        assert!(block.contains("Mu = 88") && block.contains("Mu = 99.1") && block.contains("intermediate steps left out") && !block.contains("N_0q") && !block.contains("not listed"), "{block}");
    }

    #[test]
    fn numbers_are_read_as_written_and_matched_at_the_coarser_precision() {
        let n = numbers("case16: q_ult = 1,068 kPa, Q_all=800.9 kN; N_2 = 3; ratio 0.7958, -12.5 m; B = 1.5.");
        let v: Vec<f64> = n.iter().map(|s| s.value).collect();
        assert_eq!(v, [1068.0, 800.9, 3.0, 0.7958, -12.5, 1.5]);
        assert_eq!(n[3].digits, 4);
        let s = |t: &str| numbers(t)[0];
        assert!(same_number(s("1200"), s("1200.3")) && same_number(s("2031.2"), s("2031")) && same_number(s("468.5"), s("468.508")));
        assert!(!same_number(s("471.4"), s("468.5")) && !same_number(s("1201"), s("1200.3")));
    }

    #[test]
    fn record_round_trips_as_custom_data() {
        let r = rec(7, "B1", "120.5");
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["source"], 7);
        assert_eq!(v["group"], "B1");
        assert_eq!(serde_json::from_value::<FactRecord>(v).unwrap(), r);
    }
}
