//! A minimal MCP server over stdio, so other agents can take tours: `tourer --mcp`.
//! It speaks just enough JSON-RPC for tools: initialize, ping, tools/list and tools/call.
//! Tool calls are passed to the daemon, which runs them with [`call`] on the shared tour.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::agent::Agent;
use crate::daemon::{self, Client, Reads, Reply, Request, Shared};
use crate::engine::{AnswerState, Engine};
use crate::tour::{self, Anchor, Note};

const PLAN_WAIT: Duration = Duration::from_secs(60);
const ANSWER_WAIT: Duration = Duration::from_secs(300);

const INSTRUCTIONS: &str = "tourer gives guided tours of folders and codebases you have not seen before. \
Call `tour` first to get the outline, read stops in order with `stop`, and use `ask` for anything \
you want explained. Answers cite code as [path:start-end].";

struct Server {
    default_root: PathBuf,
    agent: Agent,
    model: Option<String>,
    out: Mutex<std::io::Stdout>,
    daemon: Mutex<Option<Client>>,
    /// JSON-RPC ids of tool calls the daemon is working on, by our own call id.
    pending: Mutex<HashMap<u64, Value>>,
    next_call: AtomicU64,
}

pub fn serve(default_root: PathBuf, agent: Agent, model: Option<String>) -> Result<()> {
    let server = Arc::new(Server {
        default_root,
        agent,
        model,
        out: Mutex::new(std::io::stdout()),
        daemon: Mutex::new(None),
        pending: Mutex::new(HashMap::new()),
        next_call: AtomicU64::new(1),
    });
    for line in std::io::stdin().lock().lines() {
        let Ok(msg) = serde_json::from_str::<Value>(&line?) else { continue };
        let (Some(id), Some(method)) = (msg.get("id").cloned(), msg["method"].as_str()) else {
            continue; // notifications and responses
        };
        let params = msg["params"].clone();
        match method {
            "initialize" => {
                let version = params["protocolVersion"].as_str().unwrap_or("2025-06-18");
                server.reply(id, Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "tourer", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": INSTRUCTIONS,
                })));
            }
            "ping" => server.reply(id, Ok(json!({}))),
            "tools/list" => server.reply(id, Ok(json!({"tools": tools()}))),
            "tools/call" => {
                if let Err(err) = server.forward(id.clone(), &params) {
                    server.reply_tool(id, format!("{err:#}"), true);
                }
            }
            _ => server.reply(id, Err((-32601, format!("unknown method {method}")))),
        }
    }
    Ok(())
}

fn tools() -> Value {
    let path = json!({"type": "string", "description": "Folder to tour. Defaults to the server's folder."});
    json!([
        {
            "name": "tour",
            "description": "Start or continue a guided tour of a folder and return its outline: title, summary and numbered stops. Planning takes a minute or two; if the outline says it is still being planned, call again for the rest.",
            "inputSchema": {"type": "object", "properties": {"path": path}},
        },
        {
            "name": "stop",
            "description": "Read one stop of the tour: its explanation, the code it points at, questions already asked there, and suggested follow-up questions.",
            "inputSchema": {"type": "object", "properties": {
                "path": path,
                "number": {"type": "integer", "minimum": 1, "description": "Stop number, starting at 1."},
            }, "required": ["number"]},
        },
        {
            "name": "ask",
            "description": "Ask the tour guide a question about the folder. The guide reads the code to answer and cites it as [path:start-end]. Give the stop you are at for context. The guide may also correct earlier stops and answers; the reply says which stops changed.",
            "inputSchema": {"type": "object", "properties": {
                "path": path,
                "question": {"type": "string"},
                "stop": {"type": "integer", "minimum": 1, "description": "The stop you are at. Defaults to 1."},
                "code": {"type": "string", "description": "Lines the question is about, as relative/path:start-end."},
            }, "required": ["question"]},
        },
    ])
}

impl Server {
    fn reply(&self, id: Value, result: Result<Value, (i64, String)>) {
        let msg = match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        };
        let mut out = self.out.lock().unwrap();
        let _ = writeln!(out, "{msg}");
        let _ = out.flush();
    }

    fn reply_tool(&self, id: Value, text: String, is_error: bool) {
        self.reply(id, Ok(json!({"content": [{"type": "text", "text": text}], "isError": is_error})));
    }

    /// Sends a tool call to the daemon, connecting first if needed. The reply comes later.
    fn forward(self: &Arc<Self>, id: Value, params: &Value) -> Result<()> {
        let args = &params["arguments"];
        let path = args["path"].as_str().map_or(self.default_root.clone(), |p| self.default_root.join(p));
        let root = path.canonicalize().with_context(|| format!("cannot open {}", path.display()))?;
        let call = self.next_call.fetch_add(1, Ordering::SeqCst);
        let request = Request::Tool {
            id: call,
            root,
            agent: self.agent,
            model: self.model.clone(),
            name: params["name"].as_str().unwrap_or("").to_string(),
            arguments: args.clone(),
        };
        let mut daemon = self.daemon.lock().unwrap();
        if daemon.is_none() {
            let (client, replies) = daemon::connect()?;
            *daemon = Some(client);
            let server = self.clone();
            thread::spawn(move || server.read_replies(replies));
        }
        self.pending.lock().unwrap().insert(call, id);
        let sent = daemon.as_mut().unwrap().send(&request);
        if sent.is_err() {
            *daemon = None;
            self.pending.lock().unwrap().remove(&call);
        }
        sent
    }

    fn read_replies(&self, replies: impl Iterator<Item = Reply>) {
        for reply in replies {
            if let Reply::Tool { id, text, is_error } = reply
                && let Some(rpc_id) = self.pending.lock().unwrap().remove(&id)
            {
                self.reply_tool(rpc_id, text, is_error);
            }
        }
        // The daemon went away: fail what it was working on and reconnect on the next call.
        *self.daemon.lock().unwrap() = None;
        for (_, rpc_id) in self.pending.lock().unwrap().drain() {
            self.reply_tool(rpc_id, "lost the connection to the tourer daemon; try again".into(), true);
        }
    }
}

/// Runs a tool call on a tour for a client that has read what `read` records. Called by the daemon.
pub fn call(shared: &Shared, name: &str, args: &Value, read: &Reads) -> Result<String> {
    let number = |key: &str| args[key].as_u64().map(|n| n.max(1) as usize);
    let planning_before = |n: usize| move |e: &Engine| e.state.planning && e.state.tour.stops.len() < n;
    match name {
        "tour" => Ok(outline(&shared.wait(PLAN_WAIT, |e| e.state.planning), &read.lock().unwrap())),
        "stop" => {
            let n = number("number").context("`number` is required")?;
            let engine = shared.wait(PLAN_WAIT, planning_before(n));
            let text = stop_text(&engine, n)?;
            read.lock().unwrap().insert(n - 1, engine.state.tour.stops[n - 1].version());
            Ok(text)
        }
        "ask" => {
            let q = args["question"].as_str().context("`question` is required")?;
            let code = match args["code"].as_str() {
                Some(code) => {
                    let code = format!("[{}]", code.trim().trim_matches(['[', ']']));
                    let found = tour::refs(&code).into_iter().next().map(|(_, a)| a);
                    Some(found.context("`code` must look like relative/path:start-end")?)
                }
                None => None,
            };
            let n = number("stop").unwrap_or(1);
            let n = {
                let engine = shared.wait(PLAN_WAIT, planning_before(n));
                match n.min(engine.state.tour.stops.len()) {
                    0 => return Err(no_stops(&engine)),
                    n => n,
                }
            };
            let versions = |e: &Engine| e.state.tour.stops.iter().map(|s| s.version()).collect::<Vec<_>>();
            let (at, before) = shared.update(|e| (e.ask(n - 1, q, code), versions(e)));
            let at = at.context("no such stop")?;
            let engine = shared.wait(ANSWER_WAIT, |e| e.state.answer_state(at) != AnswerState::Done);
            if engine.state.answer_state(at) != AnswerState::Done {
                return Err(anyhow!("the guide is still answering; ask again later"));
            }
            let ex = engine.state.tour.stops.get(at.0).and_then(|s| s.thread.get(at.1));
            let ex = ex.context("the tour was planned again while answering")?;
            let mut text = ex.a.clone();
            // Stops whose text or answers the guide corrected while answering.
            let after = versions(&engine);
            let updated: Vec<String> = (0..after.len())
                .filter(|&i| before.get(i).is_some_and(|b| *b != after[i]))
                .map(|i| (i + 1).to_string())
                .collect();
            if !updated.is_empty() {
                let _ = write!(
                    text,
                    "\n\nThe guide also corrected stop {}; read it again with `stop`.",
                    updated.join(", ")
                );
            }
            if !ex.notes.is_empty() {
                text.push_str("\n\nNotes on lines:\n");
                for n in &ex.notes {
                    let _ = writeln!(text, "- [{}:{}] {}", n.path, n.line, n.text);
                }
            }
            if !ex.suggested.is_empty() {
                text.push_str("\n\nSuggested follow-up questions:\n");
                for s in &ex.suggested {
                    let _ = writeln!(text, "- {s}");
                }
            }
            Ok(text)
        }
        name => Err(anyhow!("unknown tool {name}")),
    }
}

fn no_stops(engine: &Engine) -> anyhow::Error {
    match &engine.state.error {
        Some(err) => anyhow!("the tour failed: {err}"),
        None => anyhow!("no stops yet; the tour is still being planned"),
    }
}

fn outline(engine: &Engine, read: &HashMap<usize, u32>) -> String {
    let tour = &engine.state.tour;
    if tour.stops.is_empty() {
        return no_stops(engine).to_string();
    }
    let mut s = format!("# {}\n\n{}\n\n", tour.title, tour.summary);
    for (i, stop) in tour.stops.iter().enumerate() {
        let anchors: Vec<String> = stop.anchors.iter().map(|a| a.to_string()).collect();
        let updated = read.get(&i).is_some_and(|&v| v < stop.version());
        let updated = if updated { " (updated since you read it)" } else { "" };
        let _ = writeln!(s, "{}. {} {}{updated}", i + 1, stop.title, anchors.join(" "));
    }
    if engine.state.planning {
        s.push_str("\nThe tour is still being planned; call `tour` again for the remaining stops.\n");
    }
    s.push_str("\nRead stops in order with `stop`, and `ask` about anything along the way.\n");
    s
}

fn stop_text(engine: &Engine, n: usize) -> Result<String> {
    let tour = &engine.state.tour;
    let stop = tour.stops.get(n - 1).ok_or_else(|| match tour.stops.len() {
        0 => no_stops(engine),
        len => anyhow!("there are {len} stops{}", if engine.state.planning { " so far" } else { "" }),
    })?;
    let mut s = format!("# Stop {n}/{}: {}\n\n{}\n", tour.stops.len(), stop.title, stop.body);
    if !stop.anchors.is_empty() {
        s.push_str("\n## Code\n");
    }
    for a in &stop.anchors {
        let notes = stop.notes_in(&a.path);
        let _ = write!(s, "\n### {a}\n```\n{}```\n", excerpt(engine.root(), a, &notes));
    }
    for ex in &stop.thread {
        let about = ex.code.as_ref().map(|a| format!("{a} ")).unwrap_or_default();
        let a = if ex.a.is_empty() { "(not answered yet)" } else { &ex.a };
        let _ = write!(s, "\n## Q: {about}{}\n\n{a}\n", ex.q);
    }
    let suggestions = stop.suggestions();
    if !suggestions.is_empty() {
        s.push_str("\n## Suggested questions\n");
        for q in suggestions {
            let _ = writeln!(s, "- {q}");
        }
    }
    Ok(s)
}

/// Numbered lines from a file with the guide's notes under them, capped so a bad range
/// cannot flood the caller.
fn excerpt(root: &Path, a: &Anchor, notes: &[&Note]) -> String {
    let Ok(bytes) = std::fs::read(root.join(&a.path)) else { return "(file not found)\n".into() };
    let text = String::from_utf8_lossy(&bytes);
    let end = a.end.min(a.start + 120);
    let mut out = String::new();
    for (i, line) in text.lines().enumerate().skip(a.start.saturating_sub(1)).take(end + 1 - a.start.max(1)) {
        let _ = writeln!(out, "{:>5}  {line}", i + 1);
        for note in notes.iter().filter(|n| n.line == i + 1) {
            let _ = writeln!(out, "       ^ note: {}", note.text);
        }
    }
    out
}
