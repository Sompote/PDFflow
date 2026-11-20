//! Claude Code and Codex as the agent's model (providers `claude-code` and `codex`, ported from
//! PDFflow, after TellusHive's `external.rs` and `bundle.rs`). One step of the agent is one
//! non-interactive run of the program: it gets the system prompt and the conversation on stdin,
//! and PDFflow's tools over MCP (`pdfflow --mcp --connect 127.0.0.1:<port>`, the stdio bridge
//! to this running editor), so it edits the document open in the window, live. Its stream of
//! events (Claude Code `--output-format stream-json`, Codex `--json`) becomes the chat's text
//! and tool rows: each tool call and result is logged in the agent's session with a `cli:` id
//! (shown, not part of the model history).
//!
//! By default the programs run on the subscription they are logged in with: ANTHROPIC_API_KEY
//! (Codex: OPENAI_API_KEY) is taken out of their environment, because a stray or invalid key
//! in the shell would otherwise be used instead.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use th_hive::{keys, AgentRegistry, Context, EventBody, LlmAdapter, LlmRequest};
use th_llm::{CallError, Delta, Message, Reply, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    ClaudeCode,
    Codex,
}

impl Which {
    pub fn program(self) -> &'static str {
        match self {
            Which::ClaudeCode => "claude",
            Which::Codex => "codex",
        }
    }
    pub fn of(provider: &str) -> Option<Which> {
        match provider {
            "claude-code" | "cli://claude-code" => Some(Which::ClaudeCode),
            "codex" | "cli://codex" => Some(Which::Codex),
            _ => None,
        }
    }
}

/// Finds a program: the configured path, else PATH and the usual install folders a GUI app
/// does not inherit (~/.local/bin, ~/.npm-global/bin, /opt/homebrew/bin, nvm's node folders).
pub fn find_program(name: &str, configured: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    find_program_in(name, configured, &std::env::var_os("PATH").unwrap_or_default(), &home)
}

pub fn find_program_in(name: &str, configured: &str, path: &std::ffi::OsStr, home: &Path) -> Option<PathBuf> {
    if !configured.trim().is_empty() {
        let p = PathBuf::from(configured.trim());
        return p.is_file().then_some(p);
    }
    let mut dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
    dirs.extend([home.join(".local/bin"), home.join(".npm-global/bin"), home.join(".cargo/bin"), home.join(".bun/bin"), PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]);
    if let Ok(rd) = std::fs::read_dir(home.join(".nvm/versions/node")) {
        let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
        v.sort();
        v.reverse();
        dirs.extend(v);
    }
    let names: Vec<String> = if cfg!(windows) { vec![format!("{name}.exe"), format!("{name}.cmd"), name.to_string()] } else { vec![name.to_string()] };
    dirs.into_iter().flat_map(|d| names.iter().map(move |n| d.join(n)).collect::<Vec<_>>()).find(|p| p.is_file())
}

/// PATH for the child: the program's own folder first (node for claude), then the usual ones.
fn child_path(prog: &Path) -> std::ffi::OsString {
    let mut parts: Vec<PathBuf> = prog.parent().map(|d| vec![d.to_path_buf()]).unwrap_or_default();
    parts.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()));
    if cfg!(unix) {
        parts.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"].map(PathBuf::from));
    }
    std::env::join_paths(parts).unwrap_or_default()
}

/// The PDFflow MCP server a run gets.
#[derive(Debug, Clone, PartialEq)]
pub struct McpServer {
    pub command: PathBuf,
    pub args: Vec<String>,
}

impl McpServer {
    /// `pdfflow --mcp --connect <addr>`: the stdio bridge to the editor serving at `addr`.
    pub fn pdfflow(exe: &Path, addr: &str) -> McpServer {
        McpServer { command: exe.to_path_buf(), args: vec!["--mcp".into(), "--connect".into(), addr.into()] }
    }
}

/// One non-interactive run.
#[derive(Debug, Clone)]
pub struct Job {
    pub prompt: String,
    pub workdir: PathBuf,
    pub mcp: McpServer,
    /// `--model` / `-m`; empty: the program's default.
    pub model: String,
    /// Images the user attached (Claude Code reads them with Read; Codex gets `--image`).
    pub images: Vec<PathBuf>,
    pub subscription: bool,
}

/// The command line of a run (the prompt goes in on stdin). For Claude Code the MCP config is
/// written to `<workdir>/.pdfflow_mcp.json`.
pub fn command(which: Which, prog: &Path, job: &Job) -> Result<Command, String> {
    std::fs::create_dir_all(&job.workdir).map_err(|e| format!("{}: {e}", job.workdir.display()))?;
    let mut cmd = Command::new(prog);
    cmd.current_dir(&job.workdir).env("PATH", child_path(prog));
    // a fresh session, not a child of a Claude Code session this app may run inside
    for (k, _) in std::env::vars() {
        if k == "CLAUDECODE" || k.starts_with("CLAUDE_CODE_") {
            cmd.env_remove(k);
        }
    }
    if job.subscription {
        cmd.env_remove(if which == Which::ClaudeCode { "ANTHROPIC_API_KEY" } else { "OPENAI_API_KEY" });
    }
    match which {
        Which::ClaudeCode => {
            let cfg = job.workdir.join(".pdfflow_mcp.json");
            let v = json!({"mcpServers": {"pdfflow": {"type": "stdio", "command": job.mcp.command, "args": job.mcp.args}}});
            std::fs::write(&cfg, serde_json::to_string_pretty(&v).unwrap_or_default()).map_err(|e| format!("{}: {e}", cfg.display()))?;
            cmd.arg("-p").arg("--output-format").arg("stream-json").arg("--verbose").arg("--mcp-config").arg(&cfg).arg("--strict-mcp-config");
            // PDFflow's tools only (and Read for attached images); -p cannot ask for others
            let mut allowed = "mcp__pdfflow".to_string();
            if !job.images.is_empty() {
                allowed.push_str(",Read");
                let mut dirs: Vec<PathBuf> = job.images.iter().filter_map(|i| i.parent().map(Path::to_path_buf)).collect();
                dirs.dedup();
                for d in dirs {
                    cmd.arg("--add-dir").arg(d);
                }
            }
            cmd.arg("--allowedTools").arg(allowed);
            if !job.model.trim().is_empty() {
                cmd.arg("--model").arg(job.model.trim());
            }
            // a long document (or a large merge) can take minutes
            cmd.env("MCP_TOOL_TIMEOUT", "1800000");
        }
        Which::Codex => {
            cmd.arg("exec").arg("--json").arg("--skip-git-repo-check").arg("--sandbox").arg("read-only").arg("-C").arg(&job.workdir);
            cmd.arg("--output-last-message").arg(job.workdir.join(".codex_last_message.txt"));
            if !job.model.trim().is_empty() {
                cmd.arg("-m").arg(job.model.trim());
            }
            cmd.arg("-c").arg(format!("mcp_servers.pdfflow.command={}", json!(job.mcp.command.display().to_string())));
            cmd.arg("-c").arg(format!("mcp_servers.pdfflow.args={}", json!(job.mcp.args)));
            cmd.arg("-c").arg("mcp_servers.pdfflow.tool_timeout_sec=1800");
            // the tools run on the agent's copy; codex exec cannot ask anyone to approve them
            cmd.arg("-c").arg("mcp_servers.pdfflow.default_tools_approval_mode=\"approve\"");
            for i in &job.images {
                cmd.arg(format!("--image={}", i.display()));
            }
            cmd.arg("-");
        }
    }
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    Ok(cmd)
}

/// One thing a run reported.
#[derive(Debug, Clone, PartialEq)]
pub enum CliEvent {
    Text(String),
    Reasoning(String),
    ToolCall { id: String, name: String, args: String },
    ToolResult { id: String, name: String, text: String, is_error: bool },
    /// The run's last word: its answer, tokens and cost when known.
    Result { text: String, is_error: bool, usage: Tokens, cost_usd: Option<f64> },
}

/// Tokens a run reported.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Tokens {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
}

/// Claude Code's `stream-json` lines; `names` maps tool-use ids to tool names.
pub fn parse_claude(line: &str, names: &mut std::collections::HashMap<String, String>) -> Vec<CliEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { return vec![] };
    let mut out = vec![];
    let tool = |n: &str| n.strip_prefix("mcp__pdfflow__").unwrap_or(n).to_string();
    match v["type"].as_str().unwrap_or("") {
        "assistant" => {
            for b in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
                match b["type"].as_str().unwrap_or("") {
                    "text" => out.push(CliEvent::Text(b["text"].as_str().unwrap_or("").to_string())),
                    "thinking" => out.push(CliEvent::Reasoning(b["thinking"].as_str().unwrap_or("").to_string())),
                    "tool_use" => {
                        let id = b["id"].as_str().unwrap_or("").to_string();
                        let name = tool(b["name"].as_str().unwrap_or(""));
                        names.insert(id.clone(), name.clone());
                        out.push(CliEvent::ToolCall { id, name, args: b["input"].to_string() });
                    }
                    _ => {}
                }
            }
        }
        "user" => {
            for b in v.pointer("/message/content").and_then(Value::as_array).into_iter().flatten() {
                if b["type"] != "tool_result" {
                    continue;
                }
                let id = b["tool_use_id"].as_str().unwrap_or("").to_string();
                let text = match &b["content"] {
                    Value::String(s) => s.clone(),
                    Value::Array(a) => a.iter().filter_map(|c| match c["type"].as_str() {
                        Some("text") => c["text"].as_str().map(String::from),
                        Some("image") => Some("[picture]".into()),
                        _ => None,
                    }).collect::<Vec<_>>().join("\n"),
                    _ => String::new(),
                };
                let name = names.get(&id).cloned().unwrap_or_default();
                out.push(CliEvent::ToolResult { id, name, text, is_error: b["is_error"].as_bool().unwrap_or(false) });
            }
        }
        "result" => {
            let u = |k: &str| v.pointer(&format!("/usage/{k}")).and_then(Value::as_u64).unwrap_or(0);
            let cached = u("cache_read_input_tokens");
            out.push(CliEvent::Result {
                text: v["result"].as_str().unwrap_or("").to_string(),
                is_error: v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|s| s.starts_with("error")),
                usage: Tokens { prompt_tokens: u("input_tokens") + u("cache_creation_input_tokens") + cached, completion_tokens: u("output_tokens"), cached_tokens: cached },
                cost_usd: v["total_cost_usd"].as_f64(),
            });
        }
        _ => {}
    }
    out
}

/// Codex's `--json` lines (`item.*`, `turn.completed`, `turn.failed`, `error`).
pub fn parse_codex(line: &str) -> Vec<CliEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { return vec![] };
    let item = &v["item"];
    let kind = v["type"].as_str().unwrap_or("");
    let itype = item["type"].as_str().unwrap_or("");
    let args = |a: &Value| match a {
        Value::String(s) => s.clone(),
        Value::Null => "{}".into(),
        a => a.to_string(),
    };
    match (kind, itype) {
        ("item.started", "mcp_tool_call") => vec![CliEvent::ToolCall { id: item["id"].as_str().unwrap_or("").into(), name: item["tool"].as_str().unwrap_or("").into(), args: args(&item["arguments"]) }],
        ("item.completed", "mcp_tool_call") => {
            let err = item["error"]["message"].as_str().map(String::from).or_else(|| item["error"].as_str().map(String::from));
            let text = match &err {
                Some(e) => e.clone(),
                None => item.pointer("/result/content").and_then(Value::as_array).map(|a| a.iter().filter_map(|c| match c["type"].as_str() {
                    Some("text") => c["text"].as_str().map(String::from),
                    Some("image") => Some("[picture]".into()),
                    _ => None,
                }).collect::<Vec<_>>().join("\n")).unwrap_or_default(),
            };
            let failed = err.is_some() || item["status"].as_str() == Some("failed") || item.pointer("/result/is_error").and_then(Value::as_bool).unwrap_or(false);
            vec![CliEvent::ToolResult { id: item["id"].as_str().unwrap_or("").into(), name: item["tool"].as_str().unwrap_or("").into(), text, is_error: failed }]
        }
        ("item.completed", "agent_message") => vec![CliEvent::Text(item["text"].as_str().unwrap_or("").into())],
        ("item.completed", "reasoning") => vec![CliEvent::Reasoning(item["text"].as_str().unwrap_or("").into())],
        ("turn.completed", _) => {
            let u = |k: &str| v.pointer(&format!("/usage/{k}")).and_then(Value::as_u64).unwrap_or(0);
            vec![CliEvent::Result { text: String::new(), is_error: false, usage: Tokens { prompt_tokens: u("input_tokens"), completion_tokens: u("output_tokens"), cached_tokens: u("cached_input_tokens") }, cost_usd: None }]
        }
        ("turn.failed", _) => vec![CliEvent::Result { text: v["error"]["message"].as_str().unwrap_or("Codex failed").into(), is_error: true, usage: Tokens::default(), cost_usd: None }],
        ("error", _) => vec![CliEvent::Result { text: v["message"].as_str().unwrap_or("Codex reported an error").into(), is_error: true, usage: Tokens::default(), cost_usd: None }],
        _ => vec![],
    }
}

/// What a run ended with.
#[derive(Debug, Clone, Default)]
pub struct RunOutput {
    pub text: String,
    pub ok: bool,
    pub usage: Usage,
    pub cost_usd: Option<f64>,
}

/// Runs one job, streaming its events to `on`, until it ends, `cancel` is set or `timeout`
/// passes (the program is killed then).
pub fn run(which: Which, prog: &Path, job: &Job, timeout: Duration, cancel: &AtomicBool, on: &mut dyn FnMut(&CliEvent)) -> Result<RunOutput, String> {
    let mut cmd = command(which, prog, job)?;
    let last_msg = job.workdir.join(".codex_last_message.txt");
    let _ = std::fs::remove_file(&last_msg);
    let t0 = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("cannot start {}: {e}", prog.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let p = job.prompt.clone();
        std::thread::spawn(move || {
            let _ = stdin.write_all(p.as_bytes());
        });
    }
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || {
        for l in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    let err_t = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::BufReader::new(stderr), &mut s);
        s
    });
    let mut names = Default::default();
    let mut out = RunOutput::default();
    let mut texts: Vec<String> = vec![];
    let mut raw: Vec<String> = vec![];
    let mut failed_result = false;
    let mut handle = |line: &str, out: &mut RunOutput| {
        let evs = match which {
            Which::ClaudeCode => parse_claude(line, &mut names),
            Which::Codex => parse_codex(line),
        };
        for ev in &evs {
            match ev {
                CliEvent::Text(t) => texts.push(t.clone()),
                CliEvent::Result { text, is_error, usage, cost_usd } => {
                    if !text.is_empty() || *is_error {
                        out.text = text.clone();
                    }
                    failed_result |= *is_error;
                    out.usage = Usage { prompt_tokens: usage.prompt_tokens, completion_tokens: usage.completion_tokens, cached_tokens: usage.cached_tokens };
                    out.cost_usd = *cost_usd;
                }
                _ => {}
            }
            on(ev);
        }
    };
    let status = loop {
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(150)) {
            handle(&line, &mut out);
            if raw.len() < 400 {
                raw.push(line);
            }
            continue;
        }
        if cancel.load(Ordering::Relaxed) || t0.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(if cancel.load(Ordering::Relaxed) { "cancelled".into() } else { format!("{} took longer than {} min and was stopped", which.program(), timeout.as_secs() / 60) });
        }
        if let Some(st) = child.try_wait().map_err(|e| e.to_string())? {
            // the rest of its output
            let _ = reader.join();
            while let Ok(line) = rx.try_recv() {
                handle(&line, &mut out);
            }
            break st;
        }
    };
    drop(handle);
    let stderr = err_t.join().unwrap_or_default();
    out.ok = status.success() && !failed_result;
    if which == Which::Codex {
        let last = std::fs::read_to_string(&last_msg).unwrap_or_default();
        if !last.trim().is_empty() && !failed_result {
            out.text = last;
        } else if out.text.is_empty() {
            out.text = texts.last().cloned().unwrap_or_default();
        }
    }
    if out.text.trim().is_empty() && !out.ok {
        out.text = format!("{}\n{}", tail(&raw.join("\n"), 2000), tail(&stderr, 2000));
    }
    out.text = out.text.trim().to_string();
    Ok(out)
}

fn tail(s: &str, n: usize) -> String {
    let c: Vec<char> = s.chars().collect();
    c[c.len().saturating_sub(n)..].iter().collect()
}

/// The prompt of one step: the system prompt, how the program works here, and the
/// conversation so far as a transcript.
pub fn prompt(req: &LlmRequest, program: &str) -> String {
    let mut s = req.system.clone();
    s.push_str(&format!(
        "\n\n# How you work here\nYou are this agent inside PDFflow, run through {program} instead of a model API. Your tools are those of the MCP server `pdfflow`: the user's document is open in it, and every change you make appears in the PDFflow window as you make it, where the user watches and can undo it. Use them for everything; do not read or write files by other means, and do not use shell, web or file tools. When the work is done, reply with your final answer to the last message only.\n\n# Conversation so far\n"
    ));
    for m in &req.messages {
        let text = match &m.content {
            Some(th_llm::Content::Parts(parts)) => parts.iter().map(|p| match p {
                th_llm::Part::Text { text } => text.clone(),
                _ => "[picture]".into(),
            }).collect::<Vec<_>>().join("\n"),
            Some(c) => c.text(),
            None => String::new(),
        };
        match m.role.as_str() {
            "user" => s.push_str(&format!("\n## Message to you\n{text}\n")),
            "assistant" => {
                if !text.trim().is_empty() {
                    s.push_str(&format!("\n## You answered\n{text}\n"));
                }
            }
            _ => {}
        }
    }
    s.push_str("\n# Now reply to the last message.\n");
    s
}

/// How the CLI adapter runs this agent's steps.
#[derive(Clone)]
pub struct CliSetup {
    pub configured_path: String,
    pub subscription: bool,
    pub timeout: Duration,
    pub workdir: PathBuf,
    pub mcp: McpServer,
    pub images: Vec<PathBuf>,
}

/// The th-hive model adapter for providers `claude-code` and `codex`.
pub struct CliAdapter {
    pub ctx: Context,
    pub setup: CliSetup,
}

/// Session `custom` event kind of a run's cost in US dollars.
pub const COST_KIND: &str = "pdfflow/cost";

impl LlmAdapter for CliAdapter {
    fn id(&self) -> &str {
        "pdfflow-cli-models"
    }
    fn serves(&self, p: &th_llm::Provider) -> bool {
        p.is_cli()
    }
    fn stream(&self, req: &LlmRequest, on: &mut dyn FnMut(Delta), cancel: &AtomicBool) -> Result<Reply, CallError> {
        let fail = |message: String, cancelled: bool| CallError { message, status: Some(400), overflow: false, cancelled };
        let which = Which::of(&req.provider.base_url).ok_or_else(|| fail(format!("{} is not a coding-agent provider", req.provider.name), false))?;
        let agent = self.ctx.get::<AgentRegistry>(keys::AGENTS).and_then(|r| r.get(&req.session)).ok_or_else(|| fail(format!("no live agent for session {}", req.session), false))?;
        let prog = find_program(which.program(), &self.setup.configured_path).ok_or_else(|| fail(format!("'{}' was not found: install it, or set its path in the Assistant's Settings", which.program()), false))?;
        let job = Job { prompt: prompt(req, which.program()), workdir: self.setup.workdir.clone(), mcp: self.setup.mcp.clone(), model: req.model.clone(), images: self.setup.images.clone(), subscription: self.setup.subscription };
        let session = agent.session.clone();
        let mut on_ev = |ev: &CliEvent| match ev {
            CliEvent::Text(t) if !t.trim().is_empty() => on(Delta::Text(format!("{t}\n\n"))),
            CliEvent::Reasoning(t) if !t.trim().is_empty() => on(Delta::Reasoning(t.clone())),
            CliEvent::ToolCall { id, name, args } => {
                on(Delta::ToolCall { index: 0, name: name.clone() });
                session.append(EventBody::ToolCall { id: format!("cli:{id}"), name: name.clone(), arguments: args.clone() });
            }
            CliEvent::ToolResult { id, name, text, is_error } => {
                session.append(EventBody::ToolResult { id: format!("cli:{id}"), name: name.clone(), text: th_hive::clip(text, 20_000), is_error: *is_error, meta: Value::Null });
            }
            _ => {}
        };
        let out = run(which, &prog, &job, self.setup.timeout, cancel, &mut on_ev).map_err(|e| fail(format!("{}: {e}", which.program()), e == "cancelled"))?;
        if let Some(c) = out.cost_usd {
            session.append(EventBody::Custom { kind: COST_KIND.into(), data: json!({"usd": c}) });
        }
        if !out.ok {
            return Err(fail(format!("{} reported an error: {}", which.program(), th_hive::clip(&out.text, 1500)), false));
        }
        Ok(Reply { message: Message::assistant(out.text), reasoning: None, finish_reason: "stop".into(), usage: out.usage })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_stream_json_becomes_events() {
        let mut names = Default::default();
        let a = parse_claude(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Adding the note."},{"type":"tool_use","id":"tu1","name":"mcp__pdfflow__add_text","input":{"page":1}}]}}"#, &mut names);
        assert_eq!(a[0], CliEvent::Text("Adding the note.".into()));
        assert_eq!(a[1], CliEvent::ToolCall { id: "tu1".into(), name: "add_text".into(), args: r#"{"page":1}"#.into() });
        let r = parse_claude(r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu1","content":[{"type":"text","text":"{\"index\":0}"}],"is_error":false}]}}"#, &mut names);
        assert_eq!(r[0], CliEvent::ToolResult { id: "tu1".into(), name: "add_text".into(), text: r#"{"index":0}"#.into(), is_error: false });
        let d = parse_claude(r#"{"type":"result","subtype":"success","is_error":false,"result":"Done.","total_cost_usd":0.0123,"usage":{"input_tokens":10,"cache_read_input_tokens":100,"cache_creation_input_tokens":5,"output_tokens":20}}"#, &mut names);
        match &d[0] {
            CliEvent::Result { text, is_error, usage, cost_usd } => {
                assert_eq!(text, "Done.");
                assert!(!is_error);
                assert_eq!((usage.prompt_tokens, usage.completion_tokens, usage.cached_tokens), (115, 20, 100));
                assert_eq!(*cost_usd, Some(0.0123));
            }
            e => panic!("{e:?}"),
        }
    }

    #[test]
    fn codex_json_becomes_events() {
        let s = parse_codex(r#"{"type":"item.started","item":{"id":"item_1","type":"mcp_tool_call","server":"pdfflow","tool":"add_text","arguments":{"page":1},"status":"in_progress"}}"#);
        assert_eq!(s[0], CliEvent::ToolCall { id: "item_1".into(), name: "add_text".into(), args: r#"{"page":1}"#.into() });
        let c = parse_codex(r#"{"type":"item.completed","item":{"id":"item_1","type":"mcp_tool_call","server":"pdfflow","tool":"add_text","result":{"content":[{"type":"text","text":"ok"}]},"status":"completed"}}"#);
        assert_eq!(c[0], CliEvent::ToolResult { id: "item_1".into(), name: "add_text".into(), text: "ok".into(), is_error: false });
        let m = parse_codex(r#"{"type":"item.completed","item":{"id":"item_2","type":"agent_message","text":"Done."}}"#);
        assert_eq!(m[0], CliEvent::Text("Done.".into()));
        let u = parse_codex(r#"{"type":"turn.completed","usage":{"input_tokens":50,"cached_input_tokens":30,"output_tokens":7}}"#);
        assert!(matches!(&u[0], CliEvent::Result { usage, .. } if usage.prompt_tokens == 50 && usage.cached_tokens == 30));
    }

    #[test]
    fn programs_are_found_on_path_and_in_the_usual_places() {
        let d = std::env::temp_dir().join(format!("pdfflow-find-{}", std::process::id()));
        let bin = d.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("claude"), "#!/bin/sh\n").unwrap();
        let home = d.join("home");
        std::fs::create_dir_all(home.join(".npm-global/bin")).unwrap();
        std::fs::write(home.join(".npm-global/bin/codex"), "#!/bin/sh\n").unwrap();
        let path = std::env::join_paths([bin.clone()]).unwrap();
        assert_eq!(find_program_in("claude", "", &path, &home), Some(bin.join("claude")));
        assert_eq!(find_program_in("codex", "", &path, &home), Some(home.join(".npm-global/bin/codex")));
        assert_eq!(find_program_in("claude", "/no/such/claude", &path, &home), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
