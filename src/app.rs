//! The TUI on top of the daemon: the start screen, then the tour screen (`view.rs`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::agent::Agent;
use crate::daemon::{Client, Reply, Request, TourSummary};
use crate::diff;
use crate::prefs::Prefs;
use crate::tour::Anchor;
use crate::view::{Item, TourView};

pub struct App {
    client: Client,
    pub root: PathBuf,
    /// For an open tour of the changes between git revisions, those revisions as typed.
    pub diff: Option<String>,
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
    /// List the tours of the folder, or of its changes since `diff`.
    List { diff: Option<String> },
    Start { agent: Agent, model: Option<String>, diff: Option<String>, replan: bool, instructions: String },
    Ask { stop: usize, question: String, code: Option<Anchor> },
}

impl App {
    /// Opens on the start screen, with `diff`, `agent` and `model` picked if given, else the
    /// whole folder and the last agent and model.
    pub fn new(
        mut client: Client,
        root: PathBuf,
        diff: Option<String>,
        agent: Option<Agent>,
        model: Option<String>,
        replan: bool,
    ) -> Self {
        let prefs = Prefs::load();
        let agent = agent.unwrap_or(prefs.agent);
        let model = model.or_else(|| prefs.model(agent).map(str::to_string)).unwrap_or_default();
        let subjects = Subject::offered(&root, diff.as_deref());
        let start = Start::new(subjects, diff.as_deref(), agent, model, prefs.models.clone(), replan);
        let list = Request::List { root: root.clone(), diff: start.diff() };
        let error = client.send(&list).err().map(|e| format!("{e:#}"));
        App { client, root, diff: None, error, screen: Screen::Start(start), prefs, quit: false }
    }

    pub fn on_reply(&mut self, reply: Reply) {
        match (&mut self.screen, reply) {
            // Lists for a subject the person has since moved on from are dropped.
            (Screen::Start(start), Reply::Tours { diff, tours, .. }) if diff == start.diff() => start.set_tours(tours),
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
            Some(Action::List { diff }) => {
                self.error = None;
                let request = Request::List { root: self.root.clone(), diff };
                self.send(&request);
            }
            Some(Action::Start { agent, model, diff, replan, instructions }) => {
                if let Some(rev) = &diff
                    && let Err(err) = diff::resolve(&self.root, rev)
                {
                    self.error = Some(format!("{err:#}"));
                    return;
                }
                self.prefs.agent = agent;
                match &model {
                    Some(model) => self.prefs.models.insert(agent, model.clone()),
                    None => self.prefs.models.remove(&agent),
                };
                if let Err(err) = self.prefs.save() {
                    self.error = Some(format!("cannot save preferences: {err:#}"));
                }
                let request =
                    Request::Open { root: self.root.clone(), diff: diff.clone(), agent, model, replan, instructions };
                self.send(&request);
                self.diff = diff;
                self.screen = Screen::Tour(Box::new(TourView::new(self.root.clone(), agent)));
            }
            Some(Action::Ask { stop, question, code }) => {
                let Screen::Tour(view) = &self.screen else { return };
                let (root, diff, agent) = (self.root.clone(), self.diff.clone(), view.agent);
                let request = Request::Ask { root, diff, agent, stop, question, code };
                self.send(&request);
            }
        }
    }

    /// Whether frames must keep coming for something moving on screen.
    pub fn is_animating(&self) -> bool {
        matches!(&self.screen, Screen::Tour(view) if view.is_animating())
    }

    /// Picks up edits to the files the tour shows.
    pub fn check_files(&mut self) {
        if let Screen::Tour(view) = &mut self.screen {
            view.check_files();
        }
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
            let (root, diff) = (self.root.clone(), self.diff.clone());
            self.send(&Request::Seen { root, diff, agent, stop, exchange, revision });
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

/// What the start screen can tour.
#[derive(Debug, Clone, PartialEq)]
pub enum Subject {
    Folder,
    /// The changes since the last commit.
    Uncommitted { files: usize },
    /// The changes since HEAD left the main branch.
    Branch { main: String, commits: usize },
    /// The changes since a revision the person types, or between two as "OLD..NEW".
    Since(String),
}

impl Subject {
    /// What there is to tour in `root`. A `diff` that none of them tours is filled in as the
    /// revision typed. Outside a git repository, only the whole folder.
    fn offered(root: &Path, diff: Option<&str>) -> Vec<Subject> {
        let Some(offer) = diff::offer(root) else { return vec![Subject::Folder] };
        let mut subjects = vec![Subject::Folder];
        if offer.uncommitted > 0 {
            subjects.push(Subject::Uncommitted { files: offer.uncommitted });
        }
        if let Some((main, commits)) = offer.branch {
            subjects.push(Subject::Branch { main, commits });
        }
        let typed = diff.filter(|&rev| !subjects.iter().any(|s| s.diff().as_deref() == Some(rev)));
        subjects.push(Subject::Since(typed.unwrap_or_default().to_string()));
        subjects
    }

    /// The git revision whose changes this tours; `None` for the whole folder.
    pub fn diff(&self) -> Option<String> {
        match self {
            Subject::Folder => None,
            Subject::Uncommitted { .. } => Some("HEAD".into()),
            Subject::Branch { main, .. } => Some(main.clone()),
            Subject::Since(rev) => Some(rev.trim().to_string()),
        }
    }
}

/// The rows of the start screen, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Row {
    /// What to tour. Shown only in a git repository.
    Subject,
    Continue,
    New,
    Instructions,
    Agent,
    Model,
}

pub struct Start {
    /// What can be toured, the whole folder first.
    pub subjects: Vec<Subject>,
    /// The index of the subject picked.
    pub subject: usize,
    /// The tours of the subject picked; `None` until the daemon has listed them.
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
    /// Whether no list of tours has come yet.
    first_list: bool,
    /// Plan anew even if a tour exists, as asked on the command line.
    replan: bool,
    /// Screen positions of the rows. Set while drawing.
    pub row_spots: Vec<(Rect, Row)>,
}

impl Start {
    /// Opens with `diff` picked: the subject that tours it, else the revision typed.
    fn new(
        subjects: Vec<Subject>,
        diff: Option<&str>,
        agent: Agent,
        model: String,
        models: BTreeMap<Agent, String>,
        replan: bool,
    ) -> Self {
        let installed: Vec<Agent> = Agent::ALL.into_iter().filter(|a| a.is_installed()).collect();
        let subject = subjects.iter().position(|s| s.diff().as_deref() == diff).unwrap_or(0);
        Start {
            subjects,
            subject,
            tours: None,
            agent,
            installed,
            model,
            models,
            instructions: String::new(),
            row: Row::New,
            first_list: true,
            replan,
            row_spots: vec![],
        }
    }

    /// The git revision whose changes are to be toured; `None` for the whole folder.
    pub fn diff(&self) -> Option<String> {
        self.subjects[self.subject].diff()
    }

    /// Takes the tours of the subject picked. The first time, picks Continue if there is a tour
    /// to continue, else New.
    fn set_tours(&mut self, tours: Vec<TourSummary>) {
        let first = self.first_list;
        self.first_list = false;
        self.tours = Some(tours);
        self.instructions = self.existing().map(|t| t.instructions.clone()).unwrap_or_default();
        if first && self.row == Row::New && self.existing().is_some() && !self.replan {
            self.row = Row::Continue;
        } else if self.row == Row::Continue && self.existing().is_none() {
            self.row = Row::New;
        }
    }

    /// The selected agent's tour of this folder, if it has one.
    pub fn existing(&self) -> Option<&TourSummary> {
        self.tours.as_ref()?.iter().find(|t| t.agent == self.agent)
    }

    /// The rows that can be picked now. Continue needs an existing tour, and the subject row
    /// a git repository.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::New, Row::Instructions, Row::Agent, Row::Model];
        if self.existing().is_some() {
            rows.insert(0, Row::Continue);
        }
        if self.subjects.len() > 1 {
            rows.insert(0, Row::Subject);
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

    /// Picks the next or previous subject and asks for its tours.
    fn cycle_subject(&mut self, forward: bool) -> Option<Action> {
        let n = self.subjects.len();
        self.subject = if forward { (self.subject + 1) % n } else { (self.subject + n - 1) % n };
        self.relist()
    }

    /// Forgets the tours listed for the last subject and asks for the ones of this one.
    fn relist(&mut self) -> Option<Action> {
        self.tours = None;
        Some(Action::List { diff: self.diff() })
    }

    /// Whether the subject picked is a revision to type.
    pub fn typing(&self) -> bool {
        matches!(self.subjects[self.subject], Subject::Since(_))
    }

    /// The revision being typed, if that is the subject picked.
    fn typed(&mut self) -> Option<&mut String> {
        match &mut self.subjects[self.subject] {
            Subject::Since(rev) => Some(rev),
            _ => None,
        }
    }

    /// Starts the tour: continues it on Continue, plans a new one on New or Instructions,
    /// replacing any old one. Waits for a revision to be typed if that is the subject picked.
    fn start(&self, row: Row) -> Option<Action> {
        self.tours.as_ref()?;
        let diff = self.diff();
        if diff.as_deref() == Some("") {
            return None;
        }
        let replan = match row {
            Row::Continue => false,
            Row::New | Row::Instructions => true,
            Row::Subject | Row::Agent | Row::Model => self.replan || self.existing().is_none(),
        };
        let model = self.model.trim();
        let model = (!model.is_empty()).then(|| model.to_string());
        let instructions = self.instructions.trim().to_string();
        Some(Action::Start { agent: self.agent, model, diff, replan, instructions })
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        match (key.code, self.row) {
            (KeyCode::Enter, row) => return self.start(row),
            (KeyCode::Up, _) | (KeyCode::BackTab, _) => self.move_row(false),
            (KeyCode::Down, _) | (KeyCode::Tab, _) => self.move_row(true),
            (KeyCode::Left, Row::Subject) => return self.cycle_subject(false),
            (KeyCode::Right, Row::Subject) => return self.cycle_subject(true),
            (KeyCode::Backspace, Row::Subject) => {
                self.typed()?.pop();
                return self.relist();
            }
            // Revisions have no spaces, so Space moves on, as it does on the Guide row.
            (KeyCode::Char(c), Row::Subject) if c != ' ' && self.typed().is_some() => {
                self.typed()?.push(c);
                return self.relist();
            }
            (KeyCode::Char(' '), Row::Subject) => return self.cycle_subject(true),
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
            Row::Subject => return self.cycle_subject(true),
            Row::Agent => self.cycle_agent(true),
            Row::Instructions | Row::Model => {}
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn listed(action: Option<Action>) -> Option<String> {
        match action {
            Some(Action::List { diff }) => diff,
            _ => panic!("expected the tours to be listed again"),
        }
    }

    #[test]
    fn picks_what_to_tour() {
        let subjects = vec![
            Subject::Folder,
            Subject::Uncommitted { files: 2 },
            Subject::Branch { main: "main".into(), commits: 1 },
            Subject::Since(String::new()),
        ];
        let mut start = Start::new(subjects.clone(), Some("main"), Agent::Claude, String::new(), BTreeMap::new(), false);
        assert_eq!(start.diff().as_deref(), Some("main"));
        start.set_tours(vec![]);
        assert_eq!(start.rows()[0], Row::Subject);
        start.row = Row::Subject;

        assert_eq!(listed(start.on_key(key(KeyCode::Left))).as_deref(), Some("HEAD"));
        assert_eq!(listed(start.on_key(key(KeyCode::Left))), None);
        assert_eq!(listed(start.on_key(key(KeyCode::Left))).as_deref(), Some(""));
        // Nothing to start until a revision is typed; `q` is part of it, not quit.
        start.set_tours(vec![]);
        assert!(start.on_key(key(KeyCode::Enter)).is_none());
        assert_eq!(listed(start.on_key(key(KeyCode::Char('q')))).as_deref(), Some("q"));
        assert_eq!(listed(start.on_key(key(KeyCode::Backspace))).as_deref(), Some(""));
        for c in "v1.0".chars() {
            start.on_key(key(KeyCode::Char(c)));
        }
        start.set_tours(vec![]);
        assert!(matches!(
            start.on_key(key(KeyCode::Enter)),
            Some(Action::Start { diff: Some(rev), replan: true, .. }) if rev == "v1.0"
        ));

        let start = Start::new(subjects, None, Agent::Claude, String::new(), BTreeMap::new(), false);
        assert_eq!(start.diff(), None);
    }
}
