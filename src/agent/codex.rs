//! Sessions backed by `codex app-server`: JSON-RPC over stdio, one thread per conversation and
//! one turn per user message. The process starts in the background because finding the
//! user's MCP servers to switch off takes a couple of seconds.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use super::{Agent, Event, Memory, Options, Usage, tail};

/// Codex features that give the agent tools beyond reading the folder. Unknown names are
/// ignored when set through config, so this list is safe across Codex versions.
const FEATURES_OFF: [&str; 8] =
    ["apps", "plugins", "browser_use", "computer_use", "image_generation", "multi_agent", "hooks", "goals"];

/// The id of our `thread/start` or `thread/resume` request; turns use ids after it.
const THREAD_START: u64 = 2;

pub struct Session {
    inbox: Sender<String>,
    process: Arc<Mutex<Process>>,
}

/// The app-server process, which starts in the background.
enum Process {
    Starting,
    Running(Child),
    Stopped,
}

/// What the background thread needs from [`Options`].
struct Setup {
    cwd: PathBuf,
    system_prompt: String,
    model: Option<String>,
    thread: Thread,
}

/// The conversation to open: [`Memory`] with an owned id.
enum Thread {
    Throwaway,
    Kept,
    Resume(String),
}

impl Session {
    pub fn spawn<T: Copy + Send + 'static>(tag: T, opts: &Options, tx: Sender<(T, Event)>) -> Self {
        let (inbox, messages) = mpsc::channel();
        let process = Arc::new(Mutex::new(Process::Starting));
        let setup = Setup {
            cwd: opts.cwd.to_path_buf(),
            system_prompt: opts.system_prompt.to_string(),
            model: opts.model.map(str::to_string),
            thread: match opts.memory {
                Memory::None => Thread::Throwaway,
                Memory::Keep => Thread::Kept,
                Memory::Resume(id) => Thread::Resume(id.to_string()),
            },
        };
        let shared = process.clone();
        thread::spawn(move || {
            if let Err(err) = run(tag, &setup, messages, &shared, tx.clone()) {
                let _ = tx.send((tag, Event::Exited(format!("{err:#}"))));
            }
        });
        Session { inbox, process }
    }

    pub fn send(&mut self, text: &str) -> Result<()> {
        self.inbox.send(text.to_string()).map_err(|_| anyhow!("codex has exited"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let mut process = self.process.lock().unwrap();
        if let Process::Running(child) = &mut *process {
            let _ = child.kill();
            let _ = child.wait();
        }
        *process = Process::Stopped;
    }
}

/// Starts the app-server, opens a thread and turns each message from `messages` into a turn.
fn run<T: Copy + Send + 'static>(
    tag: T,
    setup: &Setup,
    messages: Receiver<String>,
    process: &Mutex<Process>,
    tx: Sender<(T, Event)>,
) -> Result<()> {
    let mut cmd = codex();
    cmd.arg("app-server");
    for server in mcp_servers()? {
        cmd.args(["-c", &format!("mcp_servers.{server}.enabled=false")]);
    }
    let mut child = cmd
        .current_dir(&setup.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to start `codex`; is Codex installed and on PATH?")?;
    let stdin = Arc::new(Mutex::new(child.stdin.take().unwrap()));
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    {
        let mut process = process.lock().unwrap();
        if let Process::Stopped = *process {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        *process = Process::Running(child);
    }

    let (thread_tx, thread_rx) = mpsc::channel();
    let replies = stdin.clone();
    let tx_session = tx.clone();
    thread::spawn(move || read(tag, stdout, stderr, &replies, &tx, thread_tx));

    write(&stdin, json!({"id": 1, "method": "initialize", "params": {"clientInfo": {
        "name": "tourer", "version": env!("CARGO_PKG_VERSION"),
    }}}))?;
    write(&stdin, json!({"method": "initialized"}))?;
    let mut params = json!({
        "cwd": setup.cwd,
        "sandbox": "read-only",
        "approvalPolicy": "never",
        "developerInstructions": setup.system_prompt,
        "model": setup.model,
    });
    let method = match &setup.thread {
        Thread::Throwaway | Thread::Kept => {
            params["ephemeral"] = json!(matches!(setup.thread, Thread::Throwaway));
            "thread/start"
        }
        Thread::Resume(id) => {
            params["threadId"] = json!(id);
            "thread/resume"
        }
    };
    write(&stdin, json!({"id": THREAD_START, "method": method, "params": params}))?;
    let thread = match thread_rx.recv() {
        Ok(Ok(thread)) => thread,
        Ok(Err(err)) => bail!("codex could not start a conversation: {err}"),
        // The process ended; the reader reports it.
        Err(_) => return Ok(()),
    };
    if !matches!(setup.thread, Thread::Throwaway) {
        let _ = tx_session.send((tag, Event::Session(thread.clone())));
    }
    for (id, text) in (THREAD_START + 1..).zip(messages) {
        write(&stdin, json!({"id": id, "method": "turn/start", "params": {
            "threadId": thread,
            "input": [{"type": "text", "text": text}],
        }}))?;
    }
    Ok(())
}

/// The `codex` command with [`FEATURES_OFF`] switched off.
fn codex() -> Command {
    let mut cmd = Agent::Codex.command();
    for feature in FEATURES_OFF {
        cmd.args(["-c", &format!("features.{feature}=false")]);
    }
    cmd
}

/// The names of the MCP servers that are switched on in the user's Codex config. Servers that
/// come with plugins are not listed, because plugins are off.
fn mcp_servers() -> Result<Vec<String>> {
    let out = codex()
        .args(["mcp", "list", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to start `codex`; is Codex installed and on PATH?")?;
    let servers: Value = serde_json::from_slice(&out.stdout).context("cannot read codex's MCP server list")?;
    let servers = servers.as_array().context("cannot read codex's MCP server list")?;
    Ok(servers
        .iter()
        .filter(|s| s["enabled"].as_bool().unwrap_or(true))
        .filter_map(|s| s["name"].as_str().map(str::to_string))
        .collect())
}

fn write(stdin: &Mutex<ChildStdin>, mut msg: Value) -> Result<()> {
    msg["jsonrpc"] = json!("2.0");
    let mut stdin = stdin.lock().unwrap();
    writeln!(stdin, "{msg}")?;
    stdin.flush()?;
    Ok(())
}

/// Reads the app-server's output until it exits. Sends the thread id, or why there is none,
/// on `thread_tx`.
fn read<T: Copy>(
    tag: T,
    stdout: impl Read,
    mut stderr: impl Read,
    stdin: &Mutex<ChildStdin>,
    tx: &Sender<(T, Event)>,
    thread_tx: Sender<Result<String, String>>,
) {
    let mut decoder = Decoder::default();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        match (msg.get("id"), msg["method"].as_str()) {
            // A request from codex, such as an approval. Nothing is approved.
            (Some(id), Some(method)) => {
                let message = format!("tourer does not support {method}");
                let _ = write(stdin, json!({"id": id, "error": {"code": -32601, "message": message}}));
            }
            (Some(id), None) => {
                let error = msg["error"]["message"].as_str().map(str::to_string);
                if id.as_u64() == Some(THREAD_START) {
                    let thread = msg["result"]["thread"]["id"].as_str().map(str::to_string);
                    let _ = thread_tx.send(thread.ok_or_else(|| error.unwrap_or_else(|| "no thread id".into())));
                } else if let Some(error) = error
                    && tx.send((tag, Event::TurnDone(Err(error)))).is_err()
                {
                    return;
                }
            }
            (None, Some(method)) => {
                for event in decoder.decode(method, &msg["params"]) {
                    if tx.send((tag, event)).is_err() {
                        return;
                    }
                }
            }
            (None, None) => {}
        }
    }
    let mut err = String::new();
    let _ = stderr.read_to_string(&mut err);
    let _ = tx.send((tag, Event::Exited(tail(&err))));
}

#[derive(Default)]
struct Decoder {
    text_in_turn: bool,
}

impl Decoder {
    /// Turns one notification into zero or more events.
    fn decode(&mut self, method: &str, params: &Value) -> Vec<Event> {
        let item = &params["item"];
        match (method, item["type"].as_str()) {
            // Separate text from consecutive messages, as around tool calls. Messages between
            // tool calls are text too: the planner writes its stops in them.
            ("item/started", Some("agentMessage")) if self.text_in_turn => vec![Event::Text("\n\n".into())],
            ("item/agentMessage/delta", _) => {
                self.text_in_turn = true;
                vec![Event::Text(params["delta"].as_str().unwrap_or("").into())]
            }
            ("item/started", Some("commandExecution")) => {
                vec![Event::Activity(command_activity(item))]
            }
            ("item/started", Some("mcpToolCall")) => {
                vec![Event::Activity(format!("{} {}", item["server"].as_str().unwrap_or(""), item["tool"].as_str().unwrap_or("")))]
            }
            ("item/started", Some("webSearch")) => {
                vec![Event::Activity(format!("Search the web {}", item["query"].as_str().unwrap_or("")))]
            }
            // Each report gives the tokens of the model call that just ended.
            ("thread/tokenUsage/updated", _) => {
                let last = &params["tokenUsage"]["last"];
                let count = |key: &str| last[key].as_u64().unwrap_or(0);
                let usage = Usage {
                    input: count("inputTokens"),
                    cached: count("cachedInputTokens"),
                    output: count("outputTokens"),
                };
                vec![Event::Usage(usage)]
            }
            ("turn/completed", _) => {
                self.text_in_turn = false;
                let turn = &params["turn"];
                let done = match turn["status"].as_str() {
                    Some("completed") => Ok("completed".to_string()),
                    status => Err(turn["error"]["message"]
                        .as_str()
                        .or(status)
                        .unwrap_or("the turn failed")
                        .to_string()),
                };
                vec![Event::TurnDone(done)]
            }
            _ => vec![],
        }
    }
}

/// A shell command, described by what codex says it does when it can tell.
fn command_activity(item: &Value) -> String {
    let action = &item["commandActions"][0];
    let text = |key: &str| action[key].as_str().unwrap_or("");
    match action["type"].as_str() {
        Some("read") => format!("Read {}", text("path")),
        Some("listFiles") => format!("List {}", action["path"].as_str().unwrap_or(".")),
        Some("search") => format!("Search {} {}", text("query"), text("path")),
        _ => {
            let command = item["command"].as_str().unwrap_or("");
            let short: String = command.chars().take(60).collect();
            if short.len() < command.len() { format!("Run {short}…") } else { format!("Run {short}") }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_all(decoder: &mut Decoder, lines: &[Value]) -> Vec<String> {
        lines
            .iter()
            .flat_map(|m| decoder.decode(m["method"].as_str().unwrap(), &m["params"]))
            .map(|e| format!("{e:?}"))
            .collect()
    }

    #[test]
    fn decodes_text_activity_and_turn_end() {
        let mut d = Decoder::default();
        let events = decode_all(&mut d, &[
            json!({"method": "item/started", "params": {"item": {"type": "agentMessage", "id": "c", "phase": "commentary"}}}),
            json!({"method": "item/agentMessage/delta", "params": {"itemId": "c", "delta": "Looking"}}),
            json!({"method": "item/completed", "params": {"item": {"type": "agentMessage", "id": "c", "text": "Looking around.\n"}}}),
            json!({"method": "item/started", "params": {"item": {"type": "commandExecution", "id": "x", "command": "pwsh -Command 'Get-Content a.rs'",
                "commandActions": [{"type": "read", "command": "Get-Content a.rs", "name": "a.rs", "path": "src/a.rs"}]}}}),
            json!({"method": "item/started", "params": {"item": {"type": "agentMessage", "id": "a", "phase": "final_answer"}}}),
            json!({"method": "item/agentMessage/delta", "params": {"itemId": "a", "delta": "Hi"}}),
            json!({"method": "item/started", "params": {"item": {"type": "agentMessage", "id": "b", "phase": "final_answer"}}}),
            json!({"method": "item/agentMessage/delta", "params": {"itemId": "b", "delta": "there"}}),
            json!({"method": "turn/completed", "params": {"turn": {"status": "failed", "error": {"message": "boom"}}}}),
        ]);
        assert_eq!(events, [
            r#"Text("Looking")"#,
            r#"Activity("Read src/a.rs")"#,
            r#"Text("\n\n")"#,
            r#"Text("Hi")"#,
            r#"Text("\n\n")"#,
            r#"Text("there")"#,
            r#"TurnDone(Err("boom"))"#,
        ]);
    }
}
