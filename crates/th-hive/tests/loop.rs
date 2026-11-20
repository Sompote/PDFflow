//! End-to-end checks of the harness with a scripted model in place of a provider.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use th_hive::plugins::subagent::{ChildAgent, Subagents};
use th_hive::plugins::{base_bundle, BaseConfig};
use th_hive::*;
use th_llm::{CallError, Content, Delta, FunctionCall, Message, Provider, Reply, ToolCall, Usage};

type Script = dyn Fn(&LlmRequest, usize) -> Result<Reply, CallError> + Send + Sync;

/// A model adapter that answers from a script; it sees every request.
struct Scripted {
    calls: AtomicUsize,
    script: Box<Script>,
    seen: Mutex<Vec<LlmRequest>>,
}

impl LlmAdapter for Scripted {
    fn id(&self) -> &str {
        "scripted"
    }
    fn serves(&self, _: &Provider) -> bool {
        true
    }
    fn reasoning_passback(&self, p: &Provider) -> bool {
        p.base_url.contains("deepseek.com")
    }
    fn stream(&self, req: &LlmRequest, on: &mut dyn FnMut(Delta), _: &AtomicBool) -> Result<Reply, CallError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen.lock().unwrap().push(req.clone());
        let r = (self.script)(req, n)?;
        on(Delta::Text(r.message.text()));
        Ok(r)
    }
}

fn text(t: &str) -> Reply {
    Reply { message: Message::assistant(t), reasoning: None, finish_reason: "stop".into(), usage: Usage { prompt_tokens: 10, completion_tokens: 5, cached_tokens: 0 } }
}

fn calls(cs: &[(&str, &str, serde_json::Value)]) -> Reply {
    let mut m = Message::assistant("");
    m.content = Some(Content::Text(String::new()));
    m.tool_calls = Some(cs.iter().map(|(id, n, a)| ToolCall { id: id.to_string(), kind: "function".into(), function: FunctionCall { name: n.to_string(), arguments: a.to_string() } }).collect());
    Reply { message: m, reasoning: Some("thinking".into()), finish_reason: "tool_calls".into(), usage: Usage::default() }
}

fn preset(id: &str) -> AgentPreset {
    AgentPreset { id: id.into(), name: id.to_uppercase(), role: format!("{id} role"), provider: Provider::deepseek(), model: "m".into(), temperature: Some(0.2), max_tokens: Some(1000), reasoning: None, max_steps: 12, time_limit: None, fail_limit: 3, tools: ToolFilter::All, meta: json!({}) }
}

fn boot(cfg: BaseConfig, script: Box<Script>) -> (Hive, Arc<Scripted>) {
    let adapter = Arc::new(Scripted { calls: AtomicUsize::new(0), script, seen: Mutex::new(vec![]) });
    let a2 = adapter.clone();
    let mut plugins = base_bundle(&cfg);
    plugins.push(fn_plugin("test-adapter", vec![keys::LLM], move |ctx| {
        let llm = ctx.expect::<LlmService>(keys::LLM)?;
        ctx.effect(llm.register(a2.clone()));
        Ok(())
    }));
    plugins.push(fn_plugin("test-tools", vec![keys::TOOLS, keys::SUBAGENTS], |ctx| {
        let tools = ctx.expect::<ToolRegistry>(keys::TOOLS)?;
        tools.register(ctx, ToolDef::new("add", "adds", json!({"type":"object"}), |_, a| Ok(ToolOutput::ok(format!("{}", a["a"].as_i64().unwrap_or(0) + a["b"].as_i64().unwrap_or(0))))))?;
        tools.register(ctx, ToolDef::new("big", "long output", json!({"type":"object"}), |_, _| Ok(ToolOutput::ok("x".repeat(5000)))))?;
        // a calculation: a long printed sheet, with its values pinned to the result
        tools.register(ctx, ToolDef::new("sheet", "runs a sheet", json!({"type":"object"}), |_, a| {
            let s = |k: &str| a[k].as_str().unwrap_or("").to_string();
            let set = FactSet {
                group: format!("calcs/{}", s("name")),
                origin: format!("calcs/{}.json", s("name")),
                headline: "all checks pass".into(),
                facts: vec![Fact::new("input", "b", "300", "mm"), Fact::new("result", &s("symbol"), &s("value"), &s("unit")).note("ACI 318-19 22.2"), Fact::new("check", "Capacity", "PASS", "").note(format!("ratio {}", s("ratio")))],
            };
            Ok(ToolOutput { text: format!("sheet {} {}", s("name"), "row ".repeat(250)), facts: vec![set], ..Default::default() })
        }))?;
        let subs = ctx.expect::<Subagents>(keys::SUBAGENTS)?;
        tools.register(ctx, ToolDef::new("broken", "always fails", json!({"type":"object"}), |_, _| Err("give `code`".into())))?;
        let slow = AgentPreset { time_limit: Some(Duration::from_millis(250)), ..preset("slowpoke") };
        subs.source(ctx, move |a| {
            if a.depth == 0 {
                vec![Arc::new(ChildAgent { preset: preset("worker") }) as Arc<dyn th_hive::plugins::subagent::SubagentProvider>, Arc::new(ChildAgent { preset: slow.clone() })]
            } else {
                vec![]
            }
        });
        Ok(())
    }));
    (Hive::boot(plugins), adapter)
}

fn new_agent(h: &Hive, id: &str) -> Arc<Agent> {
    let s = h.sessions().create(SessionMeta { id: id.into(), agent: "lead".into(), ..Default::default() }).unwrap();
    h.agent(preset("lead"), s, Arc::new(AtomicBool::new(false)))
}

fn user(t: &str) -> Inbound {
    Inbound { text: t.into(), images: vec![], source: "user".into() }
}

#[test]
fn every_base_plugin_is_active() {
    let (h, _) = boot(BaseConfig::default(), Box::new(|_, _| Ok(text("hi"))));
    for (name, st, _) in h.ctx.plugins() {
        assert_eq!(st, PluginStatus::Active, "{name}");
    }
    let names = h.tools().names();
    for t in ["todo_write", "task_create", "task_update", "task_list", "add"] {
        assert!(names.contains(&t.to_string()), "{t}");
    }
}

#[test]
fn tool_step_then_answer_and_reasoning_passback() {
    let (h, ad) = boot(BaseConfig::default(), Box::new(|req, n| match n {
        0 => Ok(calls(&[("c1", "add", json!({"a": 2, "b": 3}))])),
        _ => {
            // the open turn's reasoning goes back to DeepSeek with the call
            let a = req.messages.iter().find(|m| m.role == "assistant").unwrap();
            assert_eq!(a.reasoning_content.as_deref(), Some("thinking"));
            let r = req.messages.iter().find(|m| m.role == "tool").unwrap();
            assert_eq!(r.text(), "5");
            Ok(text("The sum is 5."))
        }
    }));
    let a = new_agent(&h, "t1");
    let out = h.send(&a, user("add 2 and 3")).unwrap();
    assert_eq!(out, TurnOutcome { text: "The sum is 5.".into(), reason: "completed".into() });
    assert_eq!(ad.calls.load(Ordering::SeqCst), 2);
    let kinds: Vec<String> = a.session.events().iter().map(|e| serde_json::to_value(&e.body).unwrap()["type"].as_str().unwrap().to_string()).collect();
    assert_eq!(
        kinds,
        ["session/start", "turn/start", "step/start", "system/message", "user/message", "assistant/message", "tool/call", "tool/result", "step/end", "step/start", "assistant/message", "step/end", "turn/end"]
    );
    // the system prompt is logged once while it does not change
    assert_eq!(a.session.events().iter().filter(|e| matches!(e.body, EventBody::SystemMessage { .. })).count(), 1);
}

#[test]
fn delegations_in_one_reply_run_concurrently() {
    let (h, _) = boot(BaseConfig::default(), Box::new(|req, _| {
        if req.messages[0].text().starts_with("Task from LEAD") {
            std::thread::sleep(Duration::from_millis(300));
            return Ok(text(&format!("done: {}", req.messages[0].text().lines().nth(1).unwrap_or(""))));
        }
        if req.messages.iter().any(|m| m.role == "tool") {
            return Ok(text("both finished"));
        }
        Ok(calls(&[("d1", "delegate", json!({"agent": "worker", "task": "A"})), ("d2", "delegate", json!({"agent": "worker", "task": "B"}))]))
    }));
    let a = new_agent(&h, "t2");
    let t0 = Instant::now();
    let out = h.send(&a, user("do A and B")).unwrap();
    assert_eq!(out.text, "both finished");
    assert!(t0.elapsed() < Duration::from_millis(550), "ran one after another: {:?}", t0.elapsed());
    let results: Vec<String> = a.session.events().iter().filter_map(|e| if let EventBody::ToolResult { text, .. } = &e.body { Some(text.clone()) } else { None }).collect();
    assert_eq!(results, ["done: A", "done: B"]);
    // each child ran on its own session linked to the parent
    assert!(h.agents().children(&a.id).is_empty(), "children are forgotten after they finish");
}

#[test]
fn retry_then_continuation_nudge() {
    let (h, _) = boot(BaseConfig::default(), Box::new(|_, n| match n {
        0 => Err(CallError { message: "HTTP 503".into(), status: Some(503), overflow: false, cancelled: false }),
        1 => Ok(text("Let me run the check now.")),
        _ => Ok(text("CHECK: PASS")),
    }));
    // keep the test fast: shorten the retry wait by answering first
    h.ctx.plugin(fn_plugin("fast-retry", vec![keys::LOOP], |ctx| {
        let lp = ctx.expect::<AgentLoop>(keys::LOOP)?;
        ctx.effect(lp.request_error.on(true, |re, next| if re.error.status == Some(503) { ErrorAction::Retry(0) } else { next(re) }));
        Ok(())
    }));
    let a = new_agent(&h, "t3");
    let out = h.send(&a, user("check it")).unwrap();
    assert_eq!(out.text, "CHECK: PASS");
    let evs = a.session.events();
    assert!(evs.iter().any(|e| matches!(&e.body, EventBody::AssistantAttempt { error, .. } if error == "HTTP 503")));
    assert!(evs.iter().any(|e| matches!(&e.body, EventBody::UserMessage { source, .. } if source == "plugin:turn-stopping")));
}

#[test]
fn repeat_reminder_and_spill_and_prune() {
    let dir = std::env::temp_dir().join(format!("hive_spill_{}", std::process::id()));
    let d2 = dir.clone();
    let cfg = BaseConfig { spill_chars: 1000, spill_locator: Some(Arc::new(move |_| Some((d2.clone(), "spill".into())))), ..Default::default() };
    let (h, _) = boot(cfg, Box::new(|_, n| if n < 3 { Ok(calls(&[(&format!("c{n}"), "big", json!({})) ])) } else { Ok(text("ok")) }));
    let a = new_agent(&h, "t4");
    h.send(&a, user("go")).unwrap();
    let evs = a.session.events();
    let r: Vec<&String> = evs.iter().filter_map(|e| if let EventBody::ToolResult { text, .. } = &e.body { Some(text) } else { None }).collect();
    assert!(r[0].contains("5000 characters in all") && r[0].len() < 1000);
    // short handles the model can copy: big_1.txt, big_2.txt, ...
    assert!(r[0].contains("spill/big_1.txt"), "{}", r[0]);
    assert!(std::fs::read_to_string(dir.join("big_1.txt")).unwrap().len() == 5000);
    assert!(dir.join("big_3.txt").exists());
    assert!(evs.iter().any(|e| matches!(&e.body, EventBody::UserMessage { source, text, .. } if source == "plugin:repeat-tool-reminder" && text.contains("3 times"))));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn compaction_summarises_old_surface() {
    let cfg = BaseConfig { compaction_budget_tokens: 300, compaction_keep_recent: 2, spill_chars: 0, ..Default::default() };
    let (h, ad) = boot(cfg, Box::new(|req, _| {
        if req.purpose == "compaction" {
            return Ok(text("SUMMARY: earlier work"));
        }
        Ok(text(&"answer ".repeat(80)))
    }));
    let a = new_agent(&h, "t5");
    for k in 0..4 {
        h.send(&a, user(&format!("question {k} {}", "words ".repeat(60)))).unwrap();
    }
    let evs = a.session.events();
    assert!(evs.iter().any(|e| matches!(e.body, EventBody::CompactionSummary { .. })));
    let last = ad.seen.lock().unwrap().iter().rev().find(|r| r.purpose == "agent").unwrap().clone();
    assert!(last.messages[0].text().contains("SUMMARY: earlier work"), "{:?}", last.messages[0].text());
    assert!(last.messages.len() <= 4);
}

/// The summary a model writes may drop or misquote numbers; the values the tools pinned
/// must reach the next request exactly, and a sheet that was run again must show only its
/// latest values.
#[test]
fn compaction_keeps_pinned_values_exactly() {
    let cfg = BaseConfig { compaction_budget_tokens: 300, compaction_keep_recent: 2, spill_chars: 0, ..Default::default() };
    let (h, ad) = boot(cfg, Box::new(|req, n| {
        if req.purpose == "compaction" {
            // a summary that keeps no number at all
            return Ok(text("SUMMARY: the beam and the footing were designed"));
        }
        let last = req.messages.last().unwrap();
        if last.role == "user" && last.text().starts_with("run ") {
            let args: serde_json::Value = serde_json::from_str(last.text().trim_start_matches("run ")).unwrap();
            return Ok(calls(&[(&format!("c{n}"), "sheet", args)]));
        }
        Ok(text(&"answer ".repeat(80)))
    }));
    let a = new_agent(&h, "t5b");
    let runs = [
        json!({"name": "B1", "symbol": "M_u", "value": "120.5", "unit": "kN*m", "ratio": "0.731"}),
        json!({"name": "F1", "symbol": "q_ult", "value": "688.25", "unit": "kPa", "ratio": "0.4127"}),
        json!({"name": "B1", "symbol": "M_u", "value": "131.2", "unit": "kN*m", "ratio": "0.796"}),
    ];
    for r in &runs {
        h.send(&a, user(&format!("run {r}"))).unwrap();
    }
    for k in 0..3 {
        h.send(&a, user(&format!("question {k} {}", "words ".repeat(60)))).unwrap();
    }
    // every set is in the log with the result it came from
    let evs = a.session.events();
    let records = a.session.fact_records();
    assert_eq!(records.len(), 3);
    for r in &records {
        assert!(evs.iter().any(|e| e.seq == r.source && matches!(&e.body, EventBody::ToolResult { name, .. } if name == "sheet")), "source {}", r.source);
    }
    assert!(evs.iter().any(|e| matches!(e.body, EventBody::CompactionSummary { .. })));
    // the sheets themselves are gone from the request; their values are not
    let last = ad.seen.lock().unwrap().iter().rev().find(|r| r.purpose == "agent").unwrap().clone();
    assert!(!last.messages.iter().any(|m| m.role == "tool"), "the tool results should be summarised away");
    let head = last.messages[0].text();
    assert!(head.contains("SUMMARY: the beam and the footing were designed"), "{head}");
    for exact in ["b = 300 mm", "M_u = 131.2 kN*m (ACI 318-19 22.2)", "Capacity = PASS (ratio 0.796)", "q_ult = 688.25 kPa (ACI 318-19 22.2)", "Capacity = PASS (ratio 0.4127)", "calcs/B1.json [sheet]: all checks pass", "calcs/F1.json [sheet]"] {
        assert!(head.contains(exact), "missing `{exact}` in:\n{head}");
    }
    // the first run of B1 was replaced by the second
    assert!(!head.contains("120.5") && !head.contains("0.731"), "{head}");
    assert_eq!(head.matches("calcs/B1.json").count(), 1, "{head}");
}

/// A worker's pinned values reach the parent with its answer, and the parent's final answer
/// is audited against them: a misquoted value is sent back once, then corrected.
#[test]
fn delegated_facts_are_promoted_and_the_answer_is_audited() {
    let (h, ad) = boot(BaseConfig { spill_chars: 0, ..Default::default() }, Box::new(|req, _| {
        if req.messages[0].text().starts_with("Task from LEAD") {
            if req.messages.iter().any(|m| m.role == "tool") {
                return Ok(text("B1 done: M_u = 131.2 kN*m"));
            }
            return Ok(calls(&[("w1", "sheet", json!({"name": "B1", "symbol": "M_u", "value": "131.2", "unit": "kN*m", "ratio": "0.796"}))]));
        }
        let last = req.messages.last().unwrap();
        if last.role == "user" && last.text().starts_with("Fact check:") {
            assert!(last.text().contains("133.9"), "{}", last.text());
            return Ok(text("Corrected: M_u = 131.2 kN*m, ratio 0.796."));
        }
        if req.messages.iter().any(|m| m.role == "tool") {
            return Ok(text("The design moment is M_u = 133.9 kN*m."));
        }
        Ok(calls(&[("d1", "delegate", json!({"agent": "worker", "task": "run sheet B1"}))]))
    }));
    let a = new_agent(&h, "t5c");
    let out = h.send(&a, user("design B1")).unwrap();
    assert_eq!(out.text, "Corrected: M_u = 131.2 kN*m, ratio 0.796.");
    let recs = a.session.fact_records();
    assert_eq!(recs.len(), 1, "the worker's sheet is pinned in the lead's session");
    assert_eq!(recs[0].tool, "delegate");
    assert!(recs[0].set.facts.iter().any(|f| f.name == "M_u" && f.value == "131.2"));
    let audits: Vec<serde_json::Value> = a.session.events().into_iter().filter_map(|e| match e.body { EventBody::Custom { kind, data } if kind == "facts/audit" => Some(data), _ => None }).collect();
    assert_eq!(audits.len(), 2, "{audits:?}");
    assert_eq!(audits[0]["unverified"], json!(["133.9"]));
    assert_eq!(audits[0]["nudged"], json!(true));
    assert_eq!(audits[1]["unverified"], json!([]));
    assert!(ad.calls.load(Ordering::SeqCst) >= 4);
}

/// An update that changes nothing keeps the revision and says so, and calls that differ only
/// by their revision count as repeats.
#[test]
fn no_op_board_updates_are_reported_and_count_as_repeats() {
    let (h, _) = boot(BaseConfig::default(), Box::new(|req, _| {
        let n = req.messages.iter().filter(|m| m.role == "tool").count();
        match n {
            0 => Ok(calls(&[("t1", "task_create", json!({"title": "Check C1"}))])),
            1..=3 => Ok(calls(&[(&format!("u{n}"), "task_update", json!({"id": "task-1", "revision": n, "status": "pending"}))])),
            _ => Ok(text("stopped")),
        }
    }));
    let a = new_agent(&h, "t7");
    h.send(&a, user("plan")).unwrap();
    let evs = a.session.events();
    let results: Vec<String> = evs.iter().filter_map(|e| if let EventBody::ToolResult { name, text, .. } = &e.body { (name == "task_update").then(|| text.clone()) } else { None }).collect();
    assert!(results[0].starts_with("No change: task-1 is already pending") && results[0].contains("(rev 1)"), "{}", results[0]);
    assert!(evs.iter().any(|e| matches!(&e.body, EventBody::UserMessage { source, text, .. } if source == "plugin:repeat-tool-reminder" && text.contains("task_update 3 times"))));
}

#[test]
fn team_board_is_shared_and_rejects_cycles() {
    let (h, _) = boot(BaseConfig::default(), Box::new(|req, _| {
        if req.messages[0].text().starts_with("Task from LEAD") {
            if req.messages.iter().any(|m| m.role == "tool") {
                return Ok(text("updated"));
            }
            return Ok(calls(&[("w1", "task_update", json!({"id": "task-1", "status": "completed", "note": "B1 passes", "revision": 1}))]));
        }
        let n_tools = req.messages.iter().filter(|m| m.role == "tool").count();
        match n_tools {
            0 => Ok(calls(&[("t1", "task_create", json!({"title": "Design B1"})), ("t2", "task_create", json!({"title": "Check B1", "blocked_by": ["task-1"]}))])),
            2 => Ok(calls(&[("t3", "task_update", json!({"id": "task-1", "blocked_by": ["task-2"]}))])),
            3 => Ok(calls(&[("t4", "delegate", json!({"agent": "worker", "task": "finish task-1"}))])),
            4 => Ok(calls(&[("t5", "task_list", json!({}))])),
            _ => Ok(text("board read")),
        }
    }));
    let a = new_agent(&h, "t6");
    h.send(&a, user("plan")).unwrap();
    let results: Vec<String> = a.session.events().iter().filter_map(|e| if let EventBody::ToolResult { text, .. } = &e.body { Some(text.clone()) } else { None }).collect();
    assert!(results[2].contains("cycle"), "{}", results[2]);
    assert!(results[4].contains("task-1 [completed] Design B1"), "{}", results[4]);
    assert!(results[4].contains("WORKER: B1 passes"), "{}", results[4]);
}

#[test]
fn resumed_session_keeps_history() {
    let dir = std::env::temp_dir().join(format!("hive_resume_{}", std::process::id()));
    let cfg = BaseConfig { sessions_dir: Some(dir.clone()), ..Default::default() };
    let (h, _) = boot(cfg.clone(), Box::new(|req, _| Ok(text(&format!("seen {} messages", req.messages.len())))));
    let a = new_agent(&h, "main");
    h.send(&a, user("one")).unwrap();
    drop(h);
    let (h, _) = boot(cfg, Box::new(|req, _| Ok(text(&format!("seen {} messages", req.messages.len())))));
    let s = h.sessions().open("main").unwrap();
    let a = h.agent(preset("lead"), s, Arc::new(AtomicBool::new(false)));
    assert_eq!(h.send(&a, user("two")).unwrap().text, "seen 3 messages");
    let _ = std::fs::remove_dir_all(dir);
}

/// A tool that fails the same way three times in a row ends the turn, even when the
/// arguments change, and says why; the reminder comes one failure before.
#[test]
fn repeated_identical_failures_stop_the_agent() {
    let (h, ad) = boot(BaseConfig::default(), Box::new(|req, n| {
        if n == 2 {
            assert!(req.messages.iter().any(|m| m.text().contains("one more identical failure ends your task")));
        }
        Ok(calls(&[(&format!("c{n}"), "broken", json!({"try": n}))]))
    }));
    let a = new_agent(&h, "fail1");
    let out = h.send(&a, user("run it")).unwrap();
    assert_eq!(out.reason, "stuck");
    assert!(out.text.contains("broken failed 3 times in a row"), "{}", out.text);
    assert_eq!(ad.calls.load(Ordering::SeqCst), 3);
}

fn cut_off() -> Reply {
    Reply { message: Message::assistant(""), reasoning: Some("very long thinking".into()), finish_reason: "length".into(), usage: Usage::default() }
}

/// A reply cut off at the output limit before any answer is asked again once, then reported.
#[test]
fn reply_cut_off_at_the_output_limit() {
    let (h, _) = boot(BaseConfig::default(), Box::new(|req, n| match n {
        0 => Ok(cut_off()),
        _ => {
            assert!(req.messages.iter().any(|m| m.text().contains("reached the output limit of 1000 tokens")));
            Ok(text("short answer"))
        }
    }));
    let a = new_agent(&h, "cut1");
    assert_eq!(h.send(&a, user("go")).unwrap(), TurnOutcome { text: "short answer".into(), reason: "completed".into() });
    let (h, _) = boot(BaseConfig::default(), Box::new(|_, _| Ok(cut_off())));
    let a = new_agent(&h, "cut2");
    let out = h.send(&a, user("go")).unwrap();
    assert_eq!(out.reason, "truncated");
    assert!(out.text.contains("output limit of 1000 tokens twice"), "{}", out.text);
}

/// A delegated task past its time limit is stopped; the parent hears so and carries on.
#[test]
fn delegated_task_stops_at_its_time_limit() {
    let (h, _) = boot(BaseConfig::default(), Box::new(|req, n| {
        if req.messages[0].text().starts_with("Task from LEAD") {
            std::thread::sleep(Duration::from_millis(120));
            return Ok(calls(&[(&format!("a{n}"), "add", json!({"a": 1, "b": 1}))]));
        }
        if req.messages.iter().any(|m| m.role == "tool") {
            return Ok(text("lead carries on"));
        }
        Ok(calls(&[("d1", "delegate", json!({"agent": "slowpoke", "task": "loop for ever"}))]))
    }));
    let a = new_agent(&h, "time1");
    let t0 = Instant::now();
    let out = h.send(&a, user("go")).unwrap();
    assert_eq!(out.text, "lead carries on");
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    let r: Vec<String> = a.session.events().iter().filter_map(|e| if let EventBody::ToolResult { text, .. } = &e.body { Some(text.clone()) } else { None }).collect();
    assert!(r[0].contains("SLOWPOKE stopped at its time limit"), "{}", r[0]);
    assert!(!a.cancelled(), "the parent is not stopped");
}
