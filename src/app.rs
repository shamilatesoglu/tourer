//! The TUI on top of the daemon: the start screen, then the tour screen (`view.rs`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::agent::Agent;
use crate::daemon::{Client, Reply, Request, TourSummary};
use crate::prefs::Prefs;
use crate::tour::Anchor;
use crate::view::{Item, TourView};

pub struct App {
    client: Client,
    pub root: PathBuf,
    /// A problem reaching the daemon.
    pub error: Option<String>,
    pub screen: Screen,
    prefs: Prefs,
    pub quit: bool,
}

pub enum Screen {
    Start(Start),
    Tour(Box<TourView>),
}

/// What a screen asks the app to do after a key or a click.
pub enum Action {
    Quit,
    Start { agent: Agent, model: Option<String>, replan: bool, instructions: String },
    Ask { stop: usize, question: String, code: Option<Anchor> },
}

impl App {
    /// Opens on the start screen, with `agent` and `model` picked if given, else the last ones.
    pub fn new(mut client: Client, root: PathBuf, agent: Option<Agent>, model: Option<String>, replan: bool) -> Self {
        let prefs = Prefs::load();
        let agent = agent.unwrap_or(prefs.agent);
        let model = model.or_else(|| prefs.model(agent).map(str::to_string)).unwrap_or_default();
        let error = client.send(&Request::List { root: root.clone() }).err().map(|e| format!("{e:#}"));
        let start = Start::new(agent, model, prefs.models.clone(), replan);
        App { client, root, error, screen: Screen::Start(start), prefs, quit: false }
    }

    pub fn on_reply(&mut self, reply: Reply) {
        match (&mut self.screen, reply) {
            (Screen::Start(start), Reply::Tours { tours, .. }) => start.set_tours(tours),
            (Screen::Tour(view), Reply::State { state, .. }) => view.set_state(*state),
            (_, Reply::Error { message }) => self.error = Some(message),
            _ => {}
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        let action = match &mut self.screen {
            Screen::Start(start) => start.on_key(key),
            Screen::Tour(view) => view.on_key(key),
        };
        self.act(action);
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        let action = match &mut self.screen {
            Screen::Start(start) => start.on_mouse(m),
            Screen::Tour(view) => view.on_mouse(m),
        };
        self.act(action);
    }

    fn act(&mut self, action: Option<Action>) {
        match action {
            None => {}
            Some(Action::Quit) => self.quit = true,
            Some(Action::Start { agent, model, replan, instructions }) => {
                self.prefs.agent = agent;
                match &model {
                    Some(model) => self.prefs.models.insert(agent, model.clone()),
                    None => self.prefs.models.remove(&agent),
                };
                if let Err(err) = self.prefs.save() {
                    self.error = Some(format!("cannot save preferences: {err:#}"));
                }
                let request = Request::Open { root: self.root.clone(), agent, model, replan, instructions };
                self.send(&request);
                self.screen = Screen::Tour(Box::new(TourView::new(self.root.clone(), agent)));
            }
            Some(Action::Ask { stop, question, code }) => {
                let Screen::Tour(view) = &self.screen else { return };
                let request = Request::Ask { root: self.root.clone(), agent: view.agent, stop, question, code };
                self.send(&request);
            }
        }
    }

    /// Whether frames must keep coming for something moving on screen.
    pub fn is_animating(&self) -> bool {
        matches!(&self.screen, Screen::Tour(view) if view.is_animating())
    }

    /// Tells the daemon which items the person has now seen.
    pub fn report_seen(&mut self) {
        let Screen::Tour(view) = &mut self.screen else { return };
        let agent = view.agent;
        let seen = std::mem::take(&mut view.seen_out);
        for (item, revision) in seen {
            let (stop, exchange) = match item {
                Item::Stop(s) => (s, None),
                Item::Answer(s, e) => (s, Some(e)),
            };
            self.send(&Request::Seen { root: self.root.clone(), agent, stop, exchange, revision });
        }
    }

    fn send(&mut self, request: &Request) {
        if let Err(err) = self.client.send(request) {
            self.error = Some(format!("{err:#}"));
        }
    }

    /// Writes the open tour, if any, as JSON.
    pub fn tour_json(&self) -> Option<String> {
        match &self.screen {
            Screen::Tour(view) => serde_json::to_string_pretty(&view.state.tour).ok(),
            Screen::Start(_) => None,
        }
    }
}

// ---- Start screen ----

/// The rows of the start screen, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Row {
    Continue,
    New,
    Instructions,
    Agent,
    Model,
}

pub struct Start {
    /// The folder's tours; `None` until the daemon has listed them.
    pub tours: Option<Vec<TourSummary>>,
    pub agent: Agent,
    pub installed: Vec<Agent>,
    /// Empty means the agent's default model.
    pub model: String,
    /// The model last picked for each agent, shown when switching to it.
    models: BTreeMap<Agent, String>,
    /// What to ask of a new tour, filled in from the existing tour's.
    pub instructions: String,
    row: Row,
    /// Plan anew even if a tour exists, as asked on the command line.
    replan: bool,
    /// Screen positions of the rows. Set while drawing.
    pub row_spots: Vec<(Rect, Row)>,
}

impl Start {
    fn new(agent: Agent, model: String, models: BTreeMap<Agent, String>, replan: bool) -> Self {
        let installed: Vec<Agent> = Agent::ALL.into_iter().filter(|a| a.is_installed()).collect();
        let instructions = String::new();
        Start { tours: None, agent, installed, model, models, instructions, row: Row::New, replan, row_spots: vec![] }
    }

    fn set_tours(&mut self, tours: Vec<TourSummary>) {
        self.tours = Some(tours);
        self.instructions = self.existing().map(|t| t.instructions.clone()).unwrap_or_default();
        self.row = if self.existing().is_some() && !self.replan { Row::Continue } else { Row::New };
    }

    /// The selected agent's tour of this folder, if it has one.
    pub fn existing(&self) -> Option<&TourSummary> {
        self.tours.as_ref()?.iter().find(|t| t.agent == self.agent)
    }

    /// The rows that can be picked now. Continue needs an existing tour.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::New, Row::Instructions, Row::Agent, Row::Model];
        if self.existing().is_some() {
            rows.insert(0, Row::Continue);
        }
        rows
    }

    pub fn row(&self) -> Row {
        self.row
    }

    fn move_row(&mut self, down: bool) {
        let rows = self.rows();
        let i = rows.iter().position(|&r| r == self.row).unwrap_or(0);
        let i = if down { (i + 1).min(rows.len() - 1) } else { i.saturating_sub(1) };
        self.row = rows[i];
    }

    /// Picks the next or previous agent, skipping ones that are not installed.
    fn cycle_agent(&mut self, forward: bool) {
        let all = Agent::ALL;
        let i = all.iter().position(|&a| a == self.agent).unwrap_or(0);
        for step in 1..=all.len() {
            let j = if forward { (i + step) % all.len() } else { (i + all.len() * 2 - step) % all.len() };
            if self.installed.contains(&all[j]) {
                if all[j] != self.agent {
                    self.agent = all[j];
                    self.model = self.models.get(&self.agent).cloned().unwrap_or_default();
                    self.instructions = self.existing().map(|t| t.instructions.clone()).unwrap_or_default();
                }
                break;
            }
        }
        if self.row == Row::Continue && self.existing().is_none() {
            self.row = Row::New;
        }
    }

    /// Starts the tour: continues it on Continue, plans a new one on New or Instructions,
    /// replacing any old one.
    fn start(&self, row: Row) -> Option<Action> {
        self.tours.as_ref()?;
        let replan = match row {
            Row::Continue => false,
            Row::New | Row::Instructions => true,
            Row::Agent | Row::Model => self.replan || self.existing().is_none(),
        };
        let model = self.model.trim();
        let model = (!model.is_empty()).then(|| model.to_string());
        let instructions = self.instructions.trim().to_string();
        Some(Action::Start { agent: self.agent, model, replan, instructions })
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        match (key.code, self.row) {
            (KeyCode::Enter, row) => return self.start(row),
            (KeyCode::Up, _) | (KeyCode::BackTab, _) => self.move_row(false),
            (KeyCode::Down, _) | (KeyCode::Tab, _) => self.move_row(true),
            (KeyCode::Left, Row::Agent) => self.cycle_agent(false),
            (KeyCode::Right, Row::Agent) | (KeyCode::Char(' '), Row::Agent) => self.cycle_agent(true),
            (KeyCode::Backspace, Row::Model) => {
                self.model.pop();
            }
            (KeyCode::Char(c), Row::Model) => self.model.push(c),
            (KeyCode::Backspace, Row::Instructions) => {
                self.instructions.pop();
            }
            (KeyCode::Char(c), Row::Instructions) => self.instructions.push(c),
            (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => return Some(Action::Quit),
            _ => {}
        }
        None
    }

    fn on_mouse(&mut self, m: MouseEvent) -> Option<Action> {
        let MouseEventKind::Down(MouseButton::Left) = m.kind else { return None };
        let at = Position::new(m.column, m.row);
        let &(_, row) = self.row_spots.iter().find(|(r, _)| r.contains(at))?;
        self.row = row;
        match row {
            Row::Continue | Row::New => return self.start(row),
            Row::Agent => self.cycle_agent(true),
            Row::Instructions | Row::Model => {}
        }
        None
    }
}
