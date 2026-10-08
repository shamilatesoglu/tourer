//! TUI state on top of the engine: selection, links, scrolling and key handling.

use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::agent::{Event, Role};
use crate::engine::Engine;
use crate::highlight::{Highlighter, StyledLine};
use crate::tour::Anchor;

pub struct App {
    pub engine: Engine,
    root: PathBuf,
    pub hl: Highlighter,
    pub sel: usize,
    /// Selected index into `links`.
    pub link: Option<usize>,
    /// Code references in the narrative pane, in display order. Set while drawing.
    pub links: Vec<Anchor>,
    pub input: Option<String>,
    pub text_scroll: usize,
    pub follow: bool,
    pub code_scroll: usize,
    files: HashMap<String, Option<Vec<StyledLine>>>,
    pub quit: bool,
}

impl App {
    pub fn new(engine: Engine, root: PathBuf) -> Self {
        App {
            engine,
            root,
            hl: Highlighter::new(),
            sel: 0,
            link: None,
            links: vec![],
            input: None,
            text_scroll: 0,
            follow: false,
            code_scroll: 0,
            files: HashMap::new(),
            quit: false,
        }
    }

    pub fn on_agent(&mut self, role: Role, event: Event) {
        let had_stops = !self.engine.tour.stops.is_empty();
        self.engine.on_agent(role, event);
        if !had_stops && !self.engine.tour.stops.is_empty() {
            self.select(0);
        }
    }

    /// The code shown in the code pane: the selected link, else the stop's first anchor.
    pub fn code_target(&self) -> Option<Anchor> {
        match self.link {
            Some(i) => self.links.get(i).cloned(),
            None => self.engine.tour.stops.get(self.sel)?.anchors.first().cloned(),
        }
    }

    /// A file's highlighted lines, read once and cached.
    pub fn file(&mut self, path: &str) -> Option<&Vec<StyledLine>> {
        let (root, hl) = (&self.root, &self.hl);
        self.files
            .entry(path.to_string())
            .or_insert_with(|| {
                let bytes = std::fs::read(root.join(path)).ok()?;
                Some(hl.file(path, &String::from_utf8_lossy(&bytes)))
            })
            .as_ref()
    }

    fn select(&mut self, i: usize) {
        self.sel = i;
        self.link = None;
        self.text_scroll = 0;
        self.follow = false;
        self.focus_code();
    }

    fn focus_code(&mut self) {
        self.code_scroll = self.code_target().map_or(0, |a| a.start.saturating_sub(4));
    }

    fn cycle_link(&mut self, forward: bool) {
        let n = self.links.len();
        if n == 0 {
            return;
        }
        self.link = Some(match (self.link, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        });
        self.focus_code();
    }

    fn ask(&mut self, q: &str) {
        if self.engine.ask(self.sel, q).is_some() {
            self.follow = true;
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if let Some(input) = &mut self.input {
            match key.code {
                KeyCode::Enter => {
                    let q = self.input.take().unwrap_or_default();
                    self.ask(&q);
                }
                KeyCode::Esc => self.input = None,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => input.push(c),
                _ => {}
            }
            return;
        }
        let stops = self.engine.tour.stops.len();
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('n') if self.sel + 1 < stops => {
                self.select(self.sel + 1)
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('p') if self.sel > 0 => {
                self.select(self.sel - 1)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.follow = false;
                self.text_scroll += 1;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.follow = false;
                self.text_scroll = self.text_scroll.saturating_sub(1);
            }
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.follow = false;
                self.text_scroll += 10;
            }
            KeyCode::PageUp => {
                self.follow = false;
                self.text_scroll = self.text_scroll.saturating_sub(10);
            }
            KeyCode::Char('J') => self.code_scroll += 1,
            KeyCode::Char('K') => self.code_scroll = self.code_scroll.saturating_sub(1),
            KeyCode::Tab => self.cycle_link(true),
            KeyCode::BackTab => self.cycle_link(false),
            KeyCode::Esc => {
                self.link = None;
                self.focus_code();
            }
            KeyCode::Char('?') | KeyCode::Char('a') | KeyCode::Enter if stops > 0 => {
                self.input = Some(String::new())
            }
            KeyCode::Char(c @ '1'..='3') => {
                let i = c as usize - '1' as usize;
                let stop = self.engine.tour.stops.get(self.sel);
                if let Some(q) = stop.and_then(|s| s.suggestions().get(i)).cloned() {
                    self.ask(&q);
                }
            }
            _ => {}
        }
    }
}
