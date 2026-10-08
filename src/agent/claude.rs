//! Sessions backed by the `claude` CLI in headless stream-json mode: one process per
//! conversation, user messages on stdin.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{Agent, Event, Memory, Options, Usage, tail};

pub struct Session {
    child: Child,
    stdin: ChildStdin,
}

impl Session {
    pub fn spawn<T: Copy + Send + 'static>(tag: T, opts: &Options, tx: Sender<(T, Event)>) -> Result<Self> {
        let mut cmd = Agent::Claude.command();
        cmd.current_dir(opts.cwd)
            .args(["-p", "--verbose", "--include-partial-messages"])
            .args(["--input-format", "stream-json", "--output-format", "stream-json"])
            .args(["--tools", "Read,Grep,Glob", "--strict-mcp-config"])
            .args(["--disable-slash-commands", "--append-system-prompt", opts.system_prompt]);
        if let Some(model) = opts.model {
            cmd.args(["--model", model]);
        }
        match opts.memory {
            Memory::None => {
                cmd.arg("--no-session-persistence");
            }
            Memory::Keep => {}
            Memory::Resume(id) => {
                cmd.args(["--resume", id]);
            }
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to start `claude`; is Claude Code installed and on PATH?")?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();

        thread::spawn(move || {
            let mut text_in_turn = false;
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                for event in decode(&v, &mut text_in_turn) {
                    if tx.send((tag, event)).is_err() {
                        return;
                    }
                }
            }
            let mut err = String::new();
            let _ = stderr.read_to_string(&mut err);
            let _ = tx.send((tag, Event::Exited(tail(&err))));
        });

        Ok(Session { child, stdin })
    }

    pub fn send(&mut self, text: &str) -> Result<()> {
        let msg = json!({"type": "user", "message": {"role": "user", "content": text}});
        writeln!(self.stdin, "{msg}")?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Turns one stream-json line into zero or more events. Subagent output is ignored.
fn decode(v: &Value, text_in_turn: &mut bool) -> Vec<Event> {
    if !v["parent_tool_use_id"].is_null() {
        return vec![];
    }
    match v["type"].as_str() {
        Some("stream_event") => {
            let e = &v["event"];
            match e["type"].as_str() {
                // Separate text from consecutive assistant messages (around tool calls).
                Some("message_start") if *text_in_turn => vec![Event::Text("\n\n".into())],
                Some("content_block_delta") if e["delta"]["type"] == "text_delta" => {
                    *text_in_turn = true;
                    vec![Event::Text(e["delta"]["text"].as_str().unwrap_or("").into())]
                }
                _ => vec![],
            }
        }
        Some("assistant") => v["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["type"] == "tool_use")
            .map(|c| {
                let input = &c["input"];
                let target = ["file_path", "pattern", "path"]
                    .iter()
                    .find_map(|k| input[k].as_str())
                    .unwrap_or("");
                Event::Activity(format!("{} {}", c["name"].as_str().unwrap_or("?"), target))
            })
            .collect(),
        Some("system") if v["subtype"] == "init" => match v["session_id"].as_str() {
            Some(id) => vec![Event::Session(id.to_string())],
            None => vec![],
        },
        Some("result") => {
            let u = &v["usage"];
            let count = |key: &str| u[key].as_u64().unwrap_or(0);
            let cached = count("cache_read_input_tokens");
            let input = count("input_tokens") + count("cache_creation_input_tokens") + cached;
            let usage = Event::Usage(Usage { input, cached, output: count("output_tokens") });
            *text_in_turn = false;
            let done = if v["is_error"].as_bool().unwrap_or(false) {
                let reason = v["result"].as_str().or(v["subtype"].as_str()).unwrap_or("error");
                Err(reason.to_string())
            } else {
                Ok(())
            };
            vec![usage, Event::TurnDone(done)]
        }
        _ => vec![],
    }
}
