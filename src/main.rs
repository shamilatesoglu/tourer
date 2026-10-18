mod agent;
mod app;
mod daemon;
mod diff;
mod engine;
mod highlight;
mod markdown;
mod prefs;
mod prompts;
mod tour;
mod ui;
mod view;

use std::path::PathBuf;
use std::sync::mpsc::{self, TryRecvError};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::execute;

use crate::agent::Agent;

/// Guided, AI-planned tours of unfamiliar folders and codebases.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Folder to tour.
    #[arg(default_value = ".")]
    path: PathBuf,
    /// The agent that plans and guides the tour. Defaults to the one picked last time.
    #[arg(long)]
    agent: Option<Agent>,
    /// Model passed to the agent, e.g. "sonnet" for claude.
    #[arg(long)]
    model: Option<String>,
    /// Write the tour, including questions and answers, as JSON when quitting.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Tour the changes since this git revision instead of the whole folder: the changes from
    /// where it meets HEAD to the files on disk. Without a revision, the uncommitted changes.
    #[arg(long, value_name = "REV", num_args = 0..=1, default_missing_value = "HEAD")]
    diff: Option<String>,
    /// Throw away the folder's saved tour and plan a new one.
    #[arg(long)]
    replan: bool,
    /// Run as the background daemon that owns all tours. Started automatically.
    #[arg(long, hide = true)]
    daemon: bool,
    /// The build of tourer that started the daemon.
    #[arg(long, hide = true, default_value = "")]
    build: String,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.daemon {
        return daemon::run(args.build);
    }
    let root = args.path.canonicalize().with_context(|| format!("cannot open {}", args.path.display()))?;
    if let Some(rev) = &args.diff {
        diff::base(&root, rev)?;
    }

    let (client, replies) = daemon::connect()?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for reply in replies {
            if tx.send(reply).is_err() {
                return;
            }
        }
    });
    let mut app = app::App::new(client, root, args.diff, args.agent, args.model, args.replan);

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        execute!(std::io::stdout(), EnableMouseCapture)?;
        while !app.quit {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            app.report_seen();
            app.check_files();
            let wait = if app.is_animating() { 16 } else { 50 };
            if event::poll(Duration::from_millis(wait))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => app.on_key(key),
                    Event::Mouse(mouse) => app.on_mouse(mouse),
                    _ => {}
                }
            }
            loop {
                match rx.try_recv() {
                    Ok(reply) => app.on_reply(reply),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        app.error = Some("lost the connection to the tourer daemon".into());
                        break;
                    }
                }
            }
        }
        Ok(())
    })();
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    if let Some(out) = args.out
        && let Some(json) = app.tour_json()
    {
        std::fs::write(&out, json).with_context(|| format!("cannot write {}", out.display()))?;
    }
    result
}
