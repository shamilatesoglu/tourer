//! Agent sessions: a coding agent's CLI running read-only in the toured folder.
//! The rest of tourer only sees [`Event`]s, so each agent's protocol stays in its own module.

mod claude;
mod codex;

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::Sender;

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// The coding agents tourer can use as planner and guide.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize, clap::ValueEnum,
)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    #[default]
    Claude,
    Codex,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::Claude, Agent::Codex];

    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    pub fn is_installed(self) -> bool {
        self.program().is_some()
    }

    /// The agent's program, found on PATH the way a shell finds it. On Windows that includes
    /// the `.cmd` scripts npm installs, which `Command` would skip for a later `.exe`.
    fn program(self) -> Option<PathBuf> {
        let extensions: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path).find_map(|dir| {
            extensions.iter().map(|ext| dir.join(format!("{}{ext}", self.name()))).find(|file| file.is_file())
        })
    }

    /// A command that runs the agent's program.
    fn command(self) -> Command {
        Command::new(self.program().unwrap_or_else(|| self.name().into()))
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Debug)]
pub enum Event {
    /// Streamed assistant text.
    Text(String),
    /// What the agent is doing, e.g. "Read src/main.rs".
    Activity(String),
    /// The agent finished responding to the last message. `Ok` says why the turn ended, for the log.
    TurnDone(Result<String, String>),
    /// The process ended; carries the tail of stderr.
    Exited(String),
    /// The agent's id for a kept conversation, to continue it later with [`Memory::Resume`].
    Session(String),
    /// Tokens used since the last report.
    Usage(Usage),
}

/// Token counts. `input` includes the `cached` part.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, other: Usage) {
        self.input += other.input;
        self.cached += other.cached;
        self.output += other.output;
    }
}

impl fmt::Display for Usage {
    /// "12.3k in (87% cached), 1.2k out".
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let short = |n: u64| if n < 1000 { n.to_string() } else { format!("{:.1}k", n as f64 / 1000.0) };
        let percent = (self.cached * 100).checked_div(self.input).unwrap_or(0);
        write!(f, "{} in ({percent}% cached), {} out", short(self.input), short(self.output))
    }
}

/// Whether the agent keeps the conversation so it can be continued after a restart.
#[derive(Debug, Clone, Copy)]
pub enum Memory<'a> {
    /// Thrown away when the session ends.
    None,
    /// Kept; its id arrives as [`Event::Session`].
    Keep,
    /// Continues the kept conversation with this id.
    Resume(&'a str),
}

pub struct Options<'a> {
    pub cwd: &'a Path,
    pub system_prompt: &'a str,
    pub model: Option<&'a str>,
    pub memory: Memory<'a>,
}

pub enum Session {
    Claude(claude::Session),
    Codex(codex::Session),
}

impl Session {
    /// Starts a session. Its events are sent on `tx` together with `tag`.
    pub fn spawn<T: Copy + Send + 'static>(
        agent: Agent,
        tag: T,
        opts: &Options,
        tx: Sender<(T, Event)>,
    ) -> Result<Self> {
        Ok(match agent {
            Agent::Claude => Session::Claude(claude::Session::spawn(tag, opts, tx)?),
            Agent::Codex => Session::Codex(codex::Session::spawn(tag, opts, tx)),
        })
    }

    /// Sends a user message, which starts a turn.
    pub fn send(&mut self, text: &str) -> Result<()> {
        match self {
            Session::Claude(s) => s.send(text),
            Session::Codex(s) => s.send(text),
        }
    }
}

/// The last 300 characters of an agent's stderr.
fn tail(stderr: &str) -> String {
    let tail = stderr.trim().chars().rev().take(300).collect::<Vec<_>>();
    tail.into_iter().rev().collect()
}
