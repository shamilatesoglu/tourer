mod agent;
mod app;
mod engine;
mod highlight;
mod markdown;
mod mcp;
mod prompts;
mod tour;
mod ui;

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

/// Guided, AI-planned tours of unfamiliar folders and codebases.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Folder to tour. With --mcp, the default folder for tool calls.
    #[arg(default_value = ".")]
    path: PathBuf,
    /// Model passed to the agent, e.g. "sonnet" or "opus".
    #[arg(long)]
    model: Option<String>,
    /// Write the tour, including questions and answers, as JSON when quitting.
    #[arg(long)]
    out: Option<PathBuf>,
    /// Serve tours to other agents over MCP (stdio) instead of opening the TUI.
    #[arg(long)]
    mcp: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let root = args.path.canonicalize().with_context(|| format!("cannot open {}", args.path.display()))?;
    if args.mcp {
        return mcp::serve(root, args.model);
    }

    let (tx, rx) = mpsc::channel();
    let engine = engine::Engine::new(root.clone(), args.model, tx)?;
    let mut app = app::App::new(engine, root);

    let mut terminal = ratatui::init();
    let result = (|| -> Result<()> {
        while !app.quit {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            if event::poll(Duration::from_millis(50))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                app.on_key(key);
            }
            while let Ok((role, ev)) = rx.try_recv() {
                app.on_agent(role, ev);
            }
        }
        Ok(())
    })();
    ratatui::restore();

    if let Some(out) = args.out {
        std::fs::write(&out, serde_json::to_string_pretty(&app.engine.tour)?)
            .with_context(|| format!("cannot write {}", out.display()))?;
    }
    result
}
