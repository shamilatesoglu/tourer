//! TUI state on top of the daemon's tour state: the start screen, then the tour with its
//! selection, links, scrolling, and key and mouse handling.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::agent::Agent;
use crate::daemon::{Client, Reply, Request, TourSummary};
use crate::engine::State;
use crate::highlight::{Highlighter, StyledLine};
use crate::prefs::Prefs;
use crate::tour::Anchor;

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
enum Action {
    Quit,
    Start { agent: Agent, model: Option<String>, replan: bool },
    Ask { stop: usize, question: String },
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
            Screen::Tour(view) => {
                view.on_mouse(m);
                None
            }
        };
        self.act(action);
    }

    fn act(&mut self, action: Option<Action>) {
        match action {
            None => {}
            Some(Action::Quit) => self.quit = true,
            Some(Action::Start { agent, model, replan }) => {
                self.prefs.agent = agent;
                match &model {
                    Some(model) => self.prefs.models.insert(agent, model.clone()),
                    None => self.prefs.models.remove(&agent),
                };
                if let Err(err) = self.prefs.save() {
                    self.error = Some(format!("cannot save preferences: {err:#}"));
                }
                let request = Request::Open { root: self.root.clone(), agent, model, replan };
                self.send(&request);
                self.screen = Screen::Tour(Box::new(TourView::new(self.root.clone(), agent)));
            }
            Some(Action::Ask { stop, question }) => {
                let Screen::Tour(view) = &mut self.screen else { return };
                let request = Request::Ask { root: self.root.clone(), agent: view.agent, stop, question };
                view.follow = true;
                self.send(&request);
            }
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
    row: Row,
    /// Plan anew even if a tour exists, as asked on the command line.
    replan: bool,
    /// Screen positions of the rows. Set while drawing.
    pub row_spots: Vec<(Rect, Row)>,
}

impl Start {
    fn new(agent: Agent, model: String, models: BTreeMap<Agent, String>, replan: bool) -> Self {
        let installed: Vec<Agent> = Agent::ALL.into_iter().filter(|a| a.is_installed()).collect();
        Start { tours: None, agent, installed, model, models, row: Row::New, replan, row_spots: vec![] }
    }

    fn set_tours(&mut self, tours: Vec<TourSummary>) {
        self.tours = Some(tours);
        self.row = if self.existing().is_some() && !self.replan { Row::Continue } else { Row::New };
    }

    /// The selected agent's tour of this folder, if it has one.
    pub fn existing(&self) -> Option<&TourSummary> {
        self.tours.as_ref()?.iter().find(|t| t.agent == self.agent)
    }

    /// The rows that can be picked now. Continue needs an existing tour.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::New, Row::Agent, Row::Model];
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
                }
                break;
            }
        }
        if self.row == Row::Continue && self.existing().is_none() {
            self.row = Row::New;
        }
    }

    /// Starts the tour: continues it on Continue, else plans a new one, replacing any old one.
    fn start(&self, row: Row) -> Option<Action> {
        self.tours.as_ref()?;
        let replan = match row {
            Row::Continue => false,
            Row::New => true,
            Row::Agent | Row::Model => self.replan || self.existing().is_none(),
        };
        let model = self.model.trim();
        let model = (!model.is_empty()).then(|| model.to_string());
        Some(Action::Start { agent: self.agent, model, replan })
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
            Row::Model => {}
        }
        None
    }
}

// ---- Tour screen ----

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pane {
    Stops,
    Text,
    Code,
}

impl Pane {
    /// The pane Tab moves to.
    fn next(self) -> Pane {
        match self {
            Pane::Stops => Pane::Text,
            Pane::Text => Pane::Code,
            Pane::Code => Pane::Stops,
        }
    }

    fn previous(self) -> Pane {
        self.next().next()
    }
}

/// Where each pane was drawn, for mouse handling. A hidden pane has an empty area.
#[derive(Debug, Default, Clone, Copy)]
pub struct Areas {
    pub main: Rect,
    pub stops: Rect,
    pub code: Rect,
    pub text: Rect,
}

/// A pane border being dragged with the mouse.
enum Drag {
    StopsWidth,
    CodeHeight,
}

pub struct TourView {
    pub agent: Agent,
    pub state: State,
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
    pub focus: Pane,
    /// The focused pane fills the screen.
    pub zoomed: bool,
    pub stops_width: u16,
    pub code_percent: u16,
    /// Set while drawing.
    pub areas: Areas,
    /// Screen positions of links in the narrative pane. Set while drawing.
    pub link_spots: Vec<(Rect, usize)>,
    drag: Option<Drag>,
    files: HashMap<String, Option<Vec<StyledLine>>>,
}

impl TourView {
    fn new(root: PathBuf, agent: Agent) -> Self {
        TourView {
            agent,
            state: State::default(),
            root,
            hl: Highlighter::new(),
            sel: 0,
            link: None,
            links: vec![],
            input: None,
            text_scroll: 0,
            follow: false,
            code_scroll: 0,
            focus: Pane::Text,
            zoomed: false,
            stops_width: 32,
            code_percent: 45,
            areas: Areas::default(),
            link_spots: vec![],
            drag: None,
            files: HashMap::new(),
        }
    }

    fn set_state(&mut self, state: State) {
        let had_stops = !self.state.tour.stops.is_empty();
        self.state = state;
        let stops = self.state.tour.stops.len();
        if !had_stops && stops > 0 || self.sel >= stops.max(1) {
            self.select(0);
        }
    }

    /// The code shown in the code pane: the selected link, else the stop's first anchor.
    pub fn code_target(&self) -> Option<Anchor> {
        match self.link {
            Some(i) => self.links.get(i).cloned(),
            None => self.state.tour.stops.get(self.sel)?.anchors.first().cloned(),
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

    /// Shows the next code reference in the narrative, after the last one it wraps around.
    fn next_link(&mut self) {
        let n = self.links.len();
        if n == 0 {
            return;
        }
        self.link = Some(self.link.map_or(0, |i| (i + 1) % n));
        self.focus_code();
    }

    /// Scrolls a pane by `lines`; in the stop list, moves the selection instead.
    fn scroll(&mut self, pane: Pane, lines: isize) {
        let by = |n: usize| n.saturating_add_signed(lines);
        match pane {
            Pane::Text => {
                self.follow = false;
                self.text_scroll = by(self.text_scroll);
            }
            Pane::Code => self.code_scroll = by(self.code_scroll),
            Pane::Stops => {
                let i = by(self.sel).min(self.state.tour.stops.len().saturating_sub(1));
                if i != self.sel {
                    self.select(i);
                }
            }
        }
    }

    /// The number of lines a page holds in the focused pane.
    fn page(&self) -> isize {
        let area = match self.focus {
            Pane::Stops => self.areas.stops,
            Pane::Text => self.areas.text,
            Pane::Code => self.areas.code,
        };
        area.height.saturating_sub(3).max(1) as isize
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        let at = Position::new(m.column, m.row);
        let a = self.areas;
        let pane = [(a.stops, Pane::Stops), (a.code, Pane::Code), (a.text, Pane::Text)]
            .into_iter()
            .find(|(r, _)| r.contains(at))
            .map(|(_, p)| p);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                // Borders are shared: the left pane's last column or the right pane's first.
                let on_stops_border = a.stops.width > 0
                    && a.code.width > 0
                    && (a.stops.right() - 1..=a.stops.right()).contains(&at.x)
                    && a.main.contains(at);
                let on_code_border = a.code.height > 0
                    && a.text.height > 0
                    && (a.code.bottom() - 1..=a.code.bottom()).contains(&at.y)
                    && (a.code.left()..a.code.right()).contains(&at.x);
                if on_stops_border {
                    self.drag = Some(Drag::StopsWidth);
                } else if on_code_border {
                    self.drag = Some(Drag::CodeHeight);
                } else if let Some(pane) = pane {
                    self.focus = pane;
                    self.click(pane, at);
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                Some(Drag::StopsWidth) => {
                    let max = a.main.width.saturating_sub(20).max(12);
                    self.stops_width = (at.x.saturating_sub(a.main.x) + 1).clamp(12, max);
                }
                Some(Drag::CodeHeight) => {
                    let total = (a.code.height + a.text.height).max(1);
                    let percent = (at.y.saturating_sub(a.code.y) + 1) * 100 / total;
                    self.code_percent = percent.clamp(10, 90);
                }
                None => {}
            },
            MouseEventKind::Up(MouseButton::Left) => self.drag = None,
            MouseEventKind::ScrollDown => {
                if let Some(pane) = pane {
                    self.scroll(pane, if pane == Pane::Stops { 1 } else { 3 });
                }
            }
            MouseEventKind::ScrollUp => {
                if let Some(pane) = pane {
                    self.scroll(pane, if pane == Pane::Stops { -1 } else { -3 });
                }
            }
            _ => {}
        }
    }

    /// A left click inside a pane: picks a stop or opens a link.
    fn click(&mut self, pane: Pane, at: Position) {
        match pane {
            Pane::Stops => {
                // Rows start below the border.
                let row = at.y.saturating_sub(self.areas.stops.y + 1) as usize;
                if at.y > self.areas.stops.y && row < self.state.tour.stops.len() {
                    self.select(row);
                }
            }
            Pane::Text => {
                if let Some(&(_, link)) = self.link_spots.iter().find(|(r, _)| r.contains(at)) {
                    self.link = Some(link);
                    self.focus_code();
                }
            }
            Pane::Code => {}
        }
    }

    fn ask(&self, question: String) -> Option<Action> {
        let ok = !question.trim().is_empty() && self.sel < self.state.tour.stops.len();
        ok.then_some(Action::Ask { stop: self.sel, question })
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if let Some(input) = &mut self.input {
            match key.code {
                KeyCode::Enter => {
                    let q = self.input.take().unwrap_or_default();
                    return self.ask(q);
                }
                KeyCode::Esc => self.input = None,
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => input.push(c),
                _ => {}
            }
            return None;
        }
        let stops = self.state.tour.stops.len();
        match key.code {
            KeyCode::Char('q') => return Some(Action::Quit),
            KeyCode::Right if self.sel + 1 < stops => self.select(self.sel + 1),
            KeyCode::Left if self.sel > 0 => self.select(self.sel - 1),
            KeyCode::Down => self.scroll(self.focus, 1),
            KeyCode::Up => self.scroll(self.focus, -1),
            KeyCode::PageDown => self.scroll(self.focus, self.page()),
            KeyCode::PageUp => self.scroll(self.focus, -self.page()),
            KeyCode::Home => self.scroll(self.focus, isize::MIN / 2),
            KeyCode::End => self.scroll(self.focus, isize::MAX / 2),
            KeyCode::Tab => self.focus = self.focus.next(),
            KeyCode::BackTab => self.focus = self.focus.previous(),
            KeyCode::Enter => match self.focus {
                Pane::Text => self.next_link(),
                Pane::Stops => self.focus = Pane::Text,
                Pane::Code => {}
            },
            KeyCode::Esc => {
                if self.link.is_some() {
                    self.link = None;
                    self.focus_code();
                } else {
                    self.zoomed = false;
                }
            }
            KeyCode::Char('z') => self.zoomed = !self.zoomed,
            KeyCode::Char('?') if stops > 0 => self.input = Some(String::new()),
            KeyCode::Char(c @ '1'..='3') => {
                let i = c as usize - '1' as usize;
                let stop = self.state.tour.stops.get(self.sel);
                if let Some(q) = stop.and_then(|s| s.suggestions().get(i)).cloned() {
                    return self.ask(q);
                }
            }
            _ => {}
        }
        None
    }
}
