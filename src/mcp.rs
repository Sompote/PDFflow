//! The Model Context Protocol server that lets AI agents drive PDFflow. The running editor
//! serves MCP's Streamable HTTP transport on 127.0.0.1 (port 7878, or `PDFFLOW_MCP_PORT`), so
//! every edit an agent makes appears in the window and can be undone there. `pdfflow --mcp`
//! is a stdio bridge for clients that start their servers as processes: it answers the
//! handshake and the tool list itself and forwards each tool call to the editor, starting the
//! editor first when it is not running.
//!
//! Requests from web pages are refused (an `Origin` header that is not local, or a body that
//! is not `application/json`), so a browser cannot be used to reach the editor.

use eframe::egui;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

pub const DEFAULT_PORT: u16 = 7878;
const PROTOCOLS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// How long a tool call may wait for the editor (it runs between frames).
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

pub fn port() -> u16 {
    std::env::var("PDFFLOW_MCP_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// A tool call waiting for the editor's UI thread.
pub struct Call {
    pub name: String,
    pub args: Value,
    pub reply: Sender<Result<Value, String>>,
}

/// The listening side inside the editor.
pub struct Server {
    pub rx: Receiver<Call>,
    pub addr: String,
}

impl Server {
    /// Listen on 127.0.0.1; `None` when the port is taken (another PDFflow window serves it)
    /// or `PDFFLOW_MCP=0`.
    pub fn start(ctx: egui::Context) -> Option<Server> {
        if std::env::var("PDFFLOW_MCP").is_ok_and(|v| v == "0") {
            return None;
        }
        let addr = format!("127.0.0.1:{}", port());
        let listener = TcpListener::bind(&addr).ok()?;
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("mcp".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let (tx, ctx) = (tx.clone(), ctx.clone());
                    std::thread::spawn(move || serve(stream, tx, ctx));
                }
            })
            .ok()?;
        Some(Server { rx, addr })
    }
}

// ------------------------------------------------------------------------------------- HTTP

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_request(r: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    if r.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?.to_owned(), parts.next()?.to_owned());
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_owned(), v.trim().to_owned()));
        }
    }
    let len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse::<usize>().ok()).unwrap_or(0);
    if len > 64 << 20 {
        return None;
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body).ok()?;
    Some(Request { method, path, headers, body })
}

fn respond(s: &mut TcpStream, status: &str, ctype: Option<&str>, body: &[u8]) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n", body.len());
    if let Some(t) = ctype {
        head.push_str(&format!("Content-Type: {t}\r\n"));
    }
    head.push_str("\r\n");
    s.write_all(head.as_bytes())?;
    s.write_all(body)?;
    s.flush()
}

/// Browsers always send `Origin` on cross-site requests; only local pages are let through.
fn local_origin(o: Option<&str>) -> bool {
    match o {
        None | Some("null") => true,
        Some(o) => {
            let host = o.split("://").nth(1).unwrap_or(o).split(['/', ':']).next().unwrap_or("");
            matches!(host, "localhost" | "127.0.0.1" | "[::1]")
        }
    }
}

fn serve(stream: TcpStream, tx: Sender<Call>, ctx: egui::Context) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(300)));
    let Ok(mut out) = stream.try_clone() else { return };
    let mut r = BufReader::new(stream);
    while let Some(req) = read_request(&mut r) {
        let ok = match (req.method.as_str(), req.path.split('?').next().unwrap_or("")) {
            _ if !local_origin(req.header("origin")) => respond(&mut out, "403 Forbidden", None, b""),
            ("POST", "/mcp" | "/") => {
                if !req.header("content-type").is_some_and(|t| t.to_ascii_lowercase().starts_with("application/json")) {
                    respond(&mut out, "415 Unsupported Media Type", None, b"")
                } else {
                    match serde_json::from_slice::<Value>(&req.body) {
                        Ok(msg) => match handle(&msg, &mut |name, args| call_editor(&tx, &ctx, name, args)) {
                            Some(reply) => respond(&mut out, "200 OK", Some("application/json"), reply.to_string().as_bytes()),
                            None => respond(&mut out, "202 Accepted", None, b""),
                        },
                        Err(e) => respond(&mut out, "400 Bad Request", Some("application/json"), error(Value::Null, -32700, &e.to_string()).to_string().as_bytes()),
                    }
                }
            }
            ("GET", "/health") => respond(&mut out, "200 OK", Some("application/json"), json!({"name": "pdfflow", "version": env!("CARGO_PKG_VERSION")}).to_string().as_bytes()),
            ("GET" | "DELETE", _) => respond(&mut out, "405 Method Not Allowed", None, b""),
            _ => respond(&mut out, "404 Not Found", None, b""),
        };
        if ok.is_err() || req.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close")) {
            break;
        }
    }
    let _ = out.shutdown(Shutdown::Both);
}

/// Hand a tool call to the UI thread and wait for its result.
fn call_editor(tx: &Sender<Call>, ctx: &egui::Context, name: &str, args: Value) -> Result<Value, String> {
    let (reply, answer) = channel();
    tx.send(Call { name: name.to_owned(), args, reply }).map_err(|_| "PDFflow is closing.".to_owned())?;
    ctx.request_repaint();
    answer.recv_timeout(CALL_TIMEOUT).map_err(|_| "PDFflow did not answer in time (is the window minimised or showing a dialog?).".to_owned())?
}

// --------------------------------------------------------------------------------- JSON-RPC

fn error(id: Value, code: i64, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

pub const INSTRUCTIONS: &str = "PDFflow is a PDF editor; these tools work on the document open in its window, live, \
and every change can be undone there (undo / redo tools). Pages are numbered from 1. Coordinates are PDF points \
(1/72 inch) on the page as it was opened: origin at the top-left corner, x to the right, y down, so an A4 page is \
595 x 842. Objects on a page (text, shapes, ink, pictures, comments) are numbered from 0 in drawing order, as \
list_objects shows them. Typical flow: open_pdf -> document_info / get_text / find_text -> edit (add_text, \
markup_text, add_shape, add_comment, pages...) -> render_page to look at the result -> save_pdf. Colours are \
'#rrggbb', '#rrggbbaa' or a name (black, white, red, green, blue, yellow, orange, purple, gray).";

/// Answer one JSON-RPC message; `None` for notifications. Tool calls go to `call`.
pub fn handle(msg: &Value, call: &mut dyn FnMut(&str, Value) -> Result<Value, String>) -> Option<Value> {
    if let Some(batch) = msg.as_array() {
        let out: Vec<Value> = batch.iter().filter_map(|m| handle(m, call)).collect();
        return (!out.is_empty()).then_some(Value::Array(out));
    }
    let id = msg.get("id").cloned()?;
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOLS[1]);
            let version = if PROTOCOLS.contains(&asked) { asked } else { PROTOCOLS[1] };
            json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "pdfflow", "title": "PDFflow", "version": env!("CARGO_PKG_VERSION")},
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": crate::mcp_tools::list()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if !crate::mcp_tools::exists(name) {
                return Some(error(id, -32602, &format!("Unknown tool: {name}")));
            }
            let args = params.get("arguments").cloned().filter(|a| a.is_object()).unwrap_or_else(|| json!({}));
            match call(name, args) {
                Ok(v) => tool_result(v),
                Err(e) => json!({"content": [{"type": "text", "text": e}], "isError": true}),
            }
        }
        "resources/list" => json!({"resources": []}),
        "prompts/list" => json!({"prompts": []}),
        _ => return Some(error(id, -32601, &format!("Method not found: {method}"))),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// A tool's value as MCP content: pictures (`{"image_png": base64, ...}`) become image
/// content, everything else is JSON text.
fn tool_result(v: Value) -> Value {
    let mut content = Vec::new();
    let mut v = v;
    if let Some(png) = v.as_object_mut().and_then(|o| o.remove("image_png")) {
        content.push(json!({"type": "image", "data": png, "mimeType": "image/png"}));
    }
    let text = match &v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    };
    content.insert(0, json!({"type": "text", "text": text}));
    let mut out = json!({"content": content, "isError": false});
    if v.is_object() {
        out["structuredContent"] = v;
    }
    out
}

// ------------------------------------------------------------------------------- stdio bridge

/// `pdfflow --mcp [--connect host:port]`: MCP over stdio, forwarding tool calls to the editor.
pub fn bridge(args: &[String]) -> std::process::ExitCode {
    let addr = args.windows(2).find(|w| w[0] == "--connect").map(|w| w[1].clone()).unwrap_or_else(|| format!("127.0.0.1:{}", port()));
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg, &mut |name, args| forward(&addr, name, args)),
            Err(e) => Some(error(Value::Null, -32700, &e.to_string())),
        };
        if let Some(r) = reply {
            if writeln!(stdout, "{r}").and_then(|_| stdout.flush()).is_err() {
                break;
            }
        }
    }
    std::process::ExitCode::SUCCESS
}

/// Run one tool in the editor, starting the editor when nothing listens yet.
fn forward(addr: &str, name: &str, args: Value) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": args}}).to_string();
    let reply = match post(addr, &body) {
        Ok(r) => r,
        Err(_) => {
            launch_editor(addr)?;
            post(addr, &body).map_err(|e| format!("Cannot reach PDFflow at {addr}: {e}"))?
        }
    };
    let v: Value = serde_json::from_str(&reply).map_err(|e| format!("Bad reply from PDFflow: {e}"))?;
    if let Some(e) = v.get("error") {
        return Err(e.get("message").and_then(Value::as_str).unwrap_or("error").to_owned());
    }
    let res = v.get("result").cloned().unwrap_or(Value::Null);
    // Unwrap the editor's MCP result back into the tool's value, so `handle` can wrap it again.
    let text = res.pointer("/content/0/text").and_then(Value::as_str).unwrap_or("").to_owned();
    if res.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(text);
    }
    let mut value = res.get("structuredContent").cloned().unwrap_or(Value::String(text));
    if let Some(img) = res.get("content").and_then(Value::as_array).and_then(|c| c.iter().find(|x| x["type"] == "image")) {
        if let Some(o) = value.as_object_mut() {
            o.insert("image_png".into(), img["data"].clone());
        }
    }
    Ok(value)
}

fn post(addr: &str, body: &str) -> std::io::Result<String> {
    let mut s = TcpStream::connect(addr)?;
    s.set_read_timeout(Some(CALL_TIMEOUT + Duration::from_secs(5)))?;
    write!(s, "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}", body.len())?;
    s.flush()?;
    let mut r = BufReader::new(s);
    let mut status = String::new();
    r.read_line(&mut status)?;
    let mut len = None;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = v.trim().parse::<usize>().ok();
            }
        }
    }
    let mut body = Vec::new();
    match len {
        Some(n) => {
            body.resize(n, 0);
            r.read_exact(&mut body)?;
        }
        None => {
            r.read_to_end(&mut body)?;
        }
    }
    if !status.contains(" 200 ") {
        return Err(std::io::Error::other(format!("PDFflow answered {}", status.trim())));
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Start the editor (this same program without `--mcp`) and wait until it listens.
fn launch_editor(addr: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let port = addr.rsplit(':').next().unwrap_or("7878").to_owned();
    std::process::Command::new(exe)
        .env("PDFFLOW_MCP_PORT", port)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot start PDFflow: {e}"))?;
    for _ in 0..100 {
        std::thread::sleep(Duration::from_millis(200));
        if TcpStream::connect(addr).is_ok() {
            // Give the window a moment to finish its first frame.
            std::thread::sleep(Duration::from_millis(400));
            return Ok(());
        }
    }
    Err(format!("PDFflow started but is not listening on {addr}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_list_and_calls() {
        let mut calls = Vec::new();
        let mut call = |n: &str, a: Value| {
            calls.push(n.to_owned());
            if n == "undo" { Err("nothing to undo".to_owned()) } else { Ok(json!({"ok": true, "args": a})) }
        };
        let init = handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}), &mut call).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
        assert!(handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}), &mut call).is_none());
        let list = handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}), &mut call).unwrap();
        let tools = list["result"]["tools"].as_array().unwrap();
        assert!(tools.len() > 40);
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{}", t["name"]);
            assert!(t["description"].as_str().unwrap().len() > 10);
        }
        let ok = handle(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"add_text","arguments":{"page":1}}}), &mut call).unwrap();
        assert_eq!(ok["result"]["isError"], false);
        assert_eq!(ok["result"]["structuredContent"]["args"]["page"], 1);
        let bad = handle(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"undo"}}), &mut call).unwrap();
        assert_eq!(bad["result"]["isError"], true);
        let unknown = handle(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"rm_rf"}}), &mut call).unwrap();
        assert_eq!(unknown["error"]["code"], -32602);
        assert_eq!(calls, ["add_text", "undo"]);
    }

    #[test]
    fn images_become_image_content() {
        let r = tool_result(json!({"width": 2, "image_png": "iVBO"}));
        assert_eq!(r["content"][1]["type"], "image");
        assert_eq!(r["content"][1]["data"], "iVBO");
        assert!(r["structuredContent"].get("image_png").is_none());
    }

    #[test]
    fn origins() {
        assert!(local_origin(None) && local_origin(Some("http://localhost:3000")) && local_origin(Some("http://127.0.0.1")));
        assert!(!local_origin(Some("https://evil.example")) && !local_origin(Some("http://localhost.evil.example")));
    }

    #[test]
    fn http_round_trip_through_a_fake_editor() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let ctx = egui::Context::default();
        let (tx, rx) = channel::<Call>();
        std::thread::spawn(move || {
            for s in listener.incoming().flatten() {
                let (tx, ctx) = (tx.clone(), ctx.clone());
                std::thread::spawn(move || serve(s, tx, ctx));
            }
        });
        std::thread::spawn(move || {
            while let Ok(c) = rx.recv() {
                let _ = c.reply.send(Ok(json!({"tool": c.name})));
            }
        });
        let v = forward(&addr, "document_info", json!({})).unwrap();
        assert_eq!(v["tool"], "document_info");
        // A browser page on another site is turned away.
        let mut s = TcpStream::connect(&addr).unwrap();
        write!(s, "POST /mcp HTTP/1.1\r\nOrigin: https://evil.example\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 403"), "{out}");
    }
}
