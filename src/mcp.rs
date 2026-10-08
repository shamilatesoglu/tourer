//! A minimal MCP server over stdio, so other agents can take tours: `tourer --mcp`.
//! It speaks just enough JSON-RPC for tools: initialize, ping, tools/list and tools/call.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::engine::{AnswerState, Engine};

const PLAN_WAIT: Duration = Duration::from_secs(60);
const ANSWER_WAIT: Duration = Duration::from_secs(300);

const INSTRUCTIONS: &str = "tourer gives guided tours of folders and codebases you have not seen before. \
Call `tour` first to get the outline, read stops in order with `stop`, and use `ask` for anything \
you want explained. Answers cite code as [path:start-end].";

/// One tour being planned or taken, shared between request threads.
struct Shared {
    engine: Mutex<Engine>,
    changed: Condvar,
}

struct Server {
    default_root: PathBuf,
    model: Option<String>,
    tours: Mutex<HashMap<PathBuf, Arc<Shared>>>,
    out: Mutex<std::io::Stdout>,
}

pub fn serve(default_root: PathBuf, model: Option<String>) -> Result<()> {
    let server = Arc::new(Server {
        default_root,
        model,
        tours: Mutex::new(HashMap::new()),
        out: Mutex::new(std::io::stdout()),
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
                let server = server.clone();
                thread::spawn(move || {
                    let result = match server.call(&params) {
                        Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
                        Err(e) => json!({"content": [{"type": "text", "text": e.to_string()}], "isError": true}),
                    };
                    server.reply(id, Ok(result));
                });
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
            "description": "Ask the tour guide a question about the folder. The guide reads the code to answer and cites it as [path:start-end]. Give the stop you are at for context.",
            "inputSchema": {"type": "object", "properties": {
                "path": path,
                "question": {"type": "string"},
                "stop": {"type": "integer", "minimum": 1, "description": "The stop you are at. Defaults to 1."},
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

    fn call(&self, params: &Value) -> Result<String> {
        let args = &params["arguments"];
        let shared = self.tour(args["path"].as_str())?;
        let number = |key: &str| args[key].as_u64().map(|n| n.max(1) as usize);
        match params["name"].as_str().unwrap_or("") {
            "tour" => {
                let engine = wait(&shared, PLAN_WAIT, |e| e.planning);
                Ok(outline(&engine))
            }
            "stop" => {
                let n = number("number").context("`number` is required")?;
                let engine = wait(&shared, PLAN_WAIT, |e| e.planning && e.tour.stops.len() < n);
                stop_text(&engine, n)
            }
            "ask" => {
                let q = args["question"].as_str().context("`question` is required")?;
                let n = number("stop").unwrap_or(1);
                let mut engine = wait(&shared, PLAN_WAIT, |e| e.planning && e.tour.stops.len() < n);
                let n = n.min(engine.tour.stops.len());
                let at = engine.ask(n.saturating_sub(1), q).ok_or_else(|| no_stops(&engine))?;
                let engine = wait_on(engine, &shared, ANSWER_WAIT, |e| e.answer_state(at) != AnswerState::Done);
                if engine.answer_state(at) != AnswerState::Done {
                    return Err(anyhow!("the guide is still answering; ask again later"));
                }
                let ex = &engine.tour.stops[at.0].thread[at.1];
                let mut text = ex.a.clone();
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

    /// The tour of a folder, started on first use.
    fn tour(&self, path: Option<&str>) -> Result<Arc<Shared>> {
        let path = path.map_or(self.default_root.clone(), |p| self.default_root.join(p));
        let root = path.canonicalize().with_context(|| format!("cannot open {}", path.display()))?;
        let mut tours = self.tours.lock().unwrap();
        if let Some(shared) = tours.get(&root) {
            return Ok(shared.clone());
        }
        let (tx, rx) = mpsc::channel();
        let engine = Engine::new(root.clone(), self.model.clone(), tx)?;
        let shared = Arc::new(Shared { engine: Mutex::new(engine), changed: Condvar::new() });
        let pump = shared.clone();
        thread::spawn(move || {
            for (role, event) in rx {
                pump.engine.lock().unwrap().on_agent(role, event);
                pump.changed.notify_all();
            }
        });
        tours.insert(root, shared.clone());
        Ok(shared)
    }
}

fn wait<'a>(shared: &'a Shared, timeout: Duration, busy: impl Fn(&Engine) -> bool) -> MutexGuard<'a, Engine> {
    wait_on(shared.engine.lock().unwrap(), shared, timeout, busy)
}

fn wait_on<'a>(
    guard: MutexGuard<'a, Engine>,
    shared: &'a Shared,
    timeout: Duration,
    busy: impl Fn(&Engine) -> bool,
) -> MutexGuard<'a, Engine> {
    shared.changed.wait_timeout_while(guard, timeout, |e| busy(e)).unwrap().0
}

fn no_stops(engine: &Engine) -> anyhow::Error {
    match &engine.error {
        Some(err) => anyhow!("the tour failed: {err}"),
        None => anyhow!("no stops yet; the tour is still being planned"),
    }
}

fn outline(engine: &Engine) -> String {
    let tour = &engine.tour;
    if tour.stops.is_empty() {
        return no_stops(engine).to_string();
    }
    let mut s = format!("# {}\n\n{}\n\n", tour.title, tour.summary);
    for (i, stop) in tour.stops.iter().enumerate() {
        let anchors: Vec<String> = stop.anchors.iter().map(|a| a.to_string()).collect();
        let _ = writeln!(s, "{}. {} {}", i + 1, stop.title, anchors.join(" "));
    }
    if engine.planning {
        s.push_str("\nThe tour is still being planned; call `tour` again for the remaining stops.\n");
    }
    s.push_str("\nRead stops in order with `stop`, and `ask` about anything along the way.\n");
    s
}

fn stop_text(engine: &Engine, n: usize) -> Result<String> {
    let tour = &engine.tour;
    let stop = tour.stops.get(n - 1).ok_or_else(|| match tour.stops.len() {
        0 => no_stops(engine),
        len => anyhow!("there are {len} stops{}", if engine.planning { " so far" } else { "" }),
    })?;
    let mut s = format!("# Stop {n}/{}: {}\n\n{}\n", tour.stops.len(), stop.title, stop.body);
    if !stop.anchors.is_empty() {
        s.push_str("\n## Code\n");
    }
    for a in &stop.anchors {
        let _ = write!(s, "\n### {a}\n```\n{}```\n", excerpt(engine.root(), &a.path, a.start, a.end));
    }
    for ex in &stop.thread {
        let _ = write!(s, "\n## Q: {}\n\n{}\n", ex.q, if ex.a.is_empty() { "(not answered yet)" } else { &ex.a });
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

/// Numbered lines from a file, capped so a bad range cannot flood the caller.
fn excerpt(root: &Path, path: &str, start: usize, end: usize) -> String {
    let Ok(bytes) = std::fs::read(root.join(path)) else { return "(file not found)\n".into() };
    let text = String::from_utf8_lossy(&bytes);
    let end = end.min(start + 120);
    let mut out = String::new();
    for (i, line) in text.lines().enumerate().skip(start.saturating_sub(1)).take(end + 1 - start.max(1)) {
        let _ = writeln!(out, "{:>5}  {line}", i + 1);
    }
    out
}
