//! The tour engine: runs the planner and the guide and keeps the tour up to date.
//! The daemon hosts one per folder and feeds it agent events; front-ends see its [`State`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent::{self, Agent, Event, Memory, Session, Usage};
use crate::prompts;
use crate::tour::{self, Anchor, Exchange, PlanLine, StopUpdate, Tour, Update};

/// How often a planner that ends its turn without the done line is asked to go on.
const MAX_PLAN_CONTINUES: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Role {
    Planner,
    Guide,
}

/// Which agent session an event came from. Sessions from before a replan keep their old
/// generation, so their late events are ignored.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tag {
    role: Role,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnswerState {
    Queued,
    Answering,
    Done,
}

/// Everything a front-end shows: the tour and what the agents are doing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub tour: Tour,
    pub planning: bool,
    pub planner_activity: String,
    pub guide_activity: String,
    queue: VecDeque<(usize, usize)>,
    answering: Option<(usize, usize)>,
    pub error: Option<String>,
    /// Tokens used since the tour was opened.
    pub planner_usage: Usage,
    pub guide_usage: Usage,
}

impl State {
    pub fn answer_state(&self, at: (usize, usize)) -> AnswerState {
        if self.answering == Some(at) {
            AnswerState::Answering
        } else if self.queue.contains(&at) {
            AnswerState::Queued
        } else {
            AnswerState::Done
        }
    }

    pub fn is_answering(&self) -> bool {
        self.answering.is_some()
    }
}

pub struct Engine {
    root: PathBuf,
    agent: Agent,
    model: Option<String>,
    tx: Sender<(Tag, Event)>,
    generation: u64,
    planner: Option<Session>,
    guide: Option<Session>,
    /// The guide continues a saved conversation and has not finished a turn in it yet. If it
    /// fails now, the conversation is dropped and a new one started.
    guide_resuming: bool,
    plan_buf: String,
    /// The planner wrote its done line.
    plan_done: bool,
    /// Times the planner was asked to go on after ending its turn without the done line.
    plan_continues: u32,
    /// Tokens used in each agent's current turn.
    turn_usage: [Usage; 2],
    pub state: State,
}

impl Engine {
    /// Starts planning a tour of `root`, or continues `saved`. Agent events arrive on the
    /// receiver paired with `tx` and must be passed to [`Engine::on_agent`].
    pub fn new(
        root: PathBuf,
        agent: Agent,
        model: Option<String>,
        tx: Sender<(Tag, Event)>,
        saved: Option<Tour>,
    ) -> Result<Self> {
        let mut engine = Engine {
            root,
            agent,
            model,
            tx,
            generation: 0,
            planner: None,
            guide: None,
            guide_resuming: false,
            plan_buf: String::new(),
            plan_done: false,
            plan_continues: 0,
            turn_usage: [Usage::default(); 2],
            state: State::default(),
        };
        match saved {
            Some(tour) => {
                engine.state.tour = tour;
                // Questions that were still open when the tour was saved.
                for (s, stop) in engine.state.tour.stops.iter().enumerate() {
                    for (e, ex) in stop.thread.iter().enumerate() {
                        if ex.a.is_empty() {
                            engine.state.queue.push_back((s, e));
                        }
                    }
                }
                engine.next_question();
            }
            None => engine.plan(String::new())?,
        }
        Ok(engine)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Throws the tour away, questions included, and plans a new one, following `instructions`.
    pub fn replan(&mut self, instructions: String) -> Result<()> {
        self.stop_agents();
        self.generation += 1;
        self.plan_buf.clear();
        self.state = State::default();
        self.plan(instructions)
    }

    /// Ends the agent processes.
    pub fn stop_agents(&mut self) {
        self.planner = None;
        self.guide = None;
        self.guide_resuming = false;
    }

    pub fn is_busy(&self) -> bool {
        self.state.planning || self.state.is_answering()
    }

    fn plan(&mut self, instructions: String) -> Result<()> {
        let root = self.root.display().to_string();
        self.state.tour = Tour { version: 1, root, instructions, ..Default::default() };
        self.state.planning = true;
        self.state.planner_activity = "starting".into();
        (self.plan_done, self.plan_continues) = (false, 0);
        let mut planner = self.spawn(Role::Planner)?;
        planner.send(&prompts::planner_start(&self.state.tour.instructions))?;
        self.planner = Some(planner);
        Ok(())
    }

    fn spawn(&self, role: Role) -> Result<Session> {
        let (system_prompt, memory) = match role {
            Role::Planner => (prompts::PLANNER, Memory::None),
            Role::Guide => match &self.state.tour.guide.session {
                Some(id) => (prompts::GUIDE, Memory::Resume(id)),
                None => (prompts::GUIDE, Memory::Keep),
            },
        };
        let opts = agent::Options { cwd: &self.root, system_prompt, model: self.model.as_deref(), memory };
        Session::spawn(self.agent, Tag { role, generation: self.generation }, &opts, self.tx.clone())
    }

    fn log_turn(&mut self, role: Role, done: &Result<String, String>) {
        let usage = std::mem::take(&mut self.turn_usage[role as usize]);
        let role = match role {
            Role::Planner => "planner",
            Role::Guide => "guide",
        };
        let ended = match done {
            Ok(reason) => reason,
            Err(err) => err,
        };
        eprintln!("{} ({}) {role} turn: {usage}; ended: {ended}", self.root.display(), self.agent);
    }

    pub fn on_agent(&mut self, tag: Tag, event: Event) {
        if tag.generation != self.generation {
            return;
        }
        match (tag.role, event) {
            (role, Event::Usage(usage)) => {
                self.turn_usage[role as usize] += usage;
                match role {
                    Role::Planner => self.state.planner_usage += usage,
                    Role::Guide => self.state.guide_usage += usage,
                }
            }
            (Role::Planner, Event::Session(_)) => {}
            (Role::Guide, Event::Session(id)) => self.state.tour.guide.session = Some(id),
            (Role::Planner, Event::Text(t)) => {
                self.plan_buf.push_str(&t);
                while let Some(i) = self.plan_buf.find('\n') {
                    let line: String = self.plan_buf.drain(..=i).collect();
                    self.on_plan_line(&line);
                }
            }
            (Role::Planner, Event::Activity(a)) => self.state.planner_activity = self.relative(&a),
            (Role::Planner, Event::TurnDone(done)) => {
                self.log_turn(Role::Planner, &done);
                let rest = std::mem::take(&mut self.plan_buf);
                self.on_plan_line(&rest);
                if done.is_ok() && !self.plan_done && self.plan_continues < MAX_PLAN_CONTINUES {
                    self.continue_planning();
                } else {
                    self.finish_planning(done.err());
                }
            }
            (Role::Planner, Event::Exited(err)) => {
                if self.state.planning {
                    self.finish_planning(Some(format!("planner exited: {err}")));
                }
            }
            (Role::Guide, Event::Text(t)) => {
                if let Some((s, e)) = self.state.answering {
                    self.state.tour.stops[s].thread[e].a.push_str(&t);
                }
            }
            (Role::Guide, Event::Activity(a)) => self.state.guide_activity = self.relative(&a),
            (Role::Guide, Event::TurnDone(done)) => {
                self.log_turn(Role::Guide, &done);
                self.guide_resuming = false;
                if let Some((s, e)) = self.state.answering.take() {
                    let ex = &mut self.state.tour.stops[s].thread[e];
                    let answer = tour::split_answer(&ex.a);
                    let updates = answer.updates;
                    (ex.notes, ex.suggested) = (answer.notes, answer.suggested);
                    ex.a = answer.text.to_string();
                    if let Err(err) = done {
                        ex.a.push_str(&format!("\n\n(guide error: {err})"));
                    }
                    if ex.a.is_empty() && !updates.is_empty() {
                        ex.a = "(The guide corrected the tour.)".into();
                    }
                    for update in updates {
                        self.apply_guide_update(update);
                    }
                }
                self.state.guide_activity.clear();
                self.next_question();
            }
            (Role::Guide, Event::Exited(err)) => {
                self.guide = None;
                let answering = self.state.answering.take();
                if std::mem::take(&mut self.guide_resuming) {
                    // The saved conversation could not be continued: start a new one and ask again.
                    eprintln!("{}: cannot continue the guide's conversation: {err}", self.root.display());
                    self.state.tour.guide = Default::default();
                    if let Some(at) = answering {
                        self.state.tour.stops[at.0].thread[at.1].a.clear();
                        self.state.queue.push_front(at);
                    }
                } else if let Some((s, e)) = answering {
                    self.state.tour.stops[s].thread[e].a.push_str(&format!("\n\n(guide exited: {err})"));
                }
                self.next_question();
            }
        }
    }

    fn on_plan_line(&mut self, line: &str) {
        match tour::parse_plan_line(line) {
            Some(PlanLine::Tour { title, summary }) => {
                self.state.tour.title = title;
                self.state.tour.summary = summary;
            }
            Some(PlanLine::Stop(mut stop)) => {
                stop.title = tour::without_number(&stop.title).to_string();
                self.state.tour.stops.push(stop);
            }
            Some(PlanLine::Update(update)) => {
                self.update_stop(update);
            }
            Some(PlanLine::Done) => self.plan_done = true,
            None => {}
        }
    }

    /// Applies a correction to a stop. Returns the stop's index, or `None` if there is no such stop.
    fn update_stop(&mut self, update: StopUpdate) -> Option<usize> {
        let i = update.stop.checked_sub(1)?;
        self.state.tour.stops.get_mut(i)?.apply(update);
        Some(i)
    }

    fn apply_guide_update(&mut self, update: Update) {
        match update {
            Update::Update(update) => {
                // The guide knows what it wrote, so the stop need not be sent back to it.
                if let Some(i) = self.update_stop(update) {
                    let revision = self.state.tour.stops[i].freshness.revision();
                    if let Some(known) = self.state.tour.guide.known.get_mut(i) {
                        *known = revision;
                    }
                }
            }
            Update::UpdateAnswer(update) => {
                let stop = update.stop.checked_sub(1).and_then(|s| self.state.tour.stops.get_mut(s));
                let ex = stop.and_then(|s| s.thread.get_mut(update.question.checked_sub(1)?));
                if let Some(ex) = ex.filter(|ex| !ex.a.is_empty()) {
                    ex.a = update.text;
                    if let Some(notes) = update.notes {
                        ex.notes = notes;
                    }
                    ex.freshness.update();
                }
            }
        }
    }

    /// Records that the person saw a stop, or one of its answers, at `revision`.
    pub fn mark_seen(&mut self, stop: usize, exchange: Option<usize>, revision: u32) {
        let Some(s) = self.state.tour.stops.get_mut(stop) else { return };
        match exchange {
            None => s.freshness.mark_seen(revision),
            Some(e) => {
                if let Some(ex) = s.thread.get_mut(e) {
                    ex.freshness.mark_seen(revision);
                }
            }
        }
    }

    /// Asks a planner that ended its turn too early to go on with the tour.
    fn continue_planning(&mut self) {
        self.plan_continues += 1;
        let stops = self.state.tour.stops.len();
        eprintln!(
            "{} ({}) planner stopped after {stops} stops without the done line; asking it to continue ({}/{MAX_PLAN_CONTINUES})",
            self.root.display(),
            self.agent,
            self.plan_continues
        );
        let sent = match self.planner.as_mut() {
            Some(planner) => planner.send(&prompts::planner_continue(stops)),
            None => return,
        };
        if let Err(err) = sent {
            self.finish_planning(Some(format!("cannot continue planning: {err}")));
        }
    }

    fn finish_planning(&mut self, err: Option<String>) {
        self.planner = None;
        let state = &mut self.state;
        state.planning = false;
        state.planner_activity.clear();
        if let Some(err) = err {
            state.error = Some(err);
        } else if state.tour.stops.is_empty() {
            state.error = Some("the planner finished without producing any stops".into());
        }
    }

    /// Shows tool targets relative to the toured folder.
    fn relative(&self, activity: &str) -> String {
        let prefix = format!("{}/", self.root.display());
        activity.replace(&prefix, "")
    }

    /// Queues a question at a stop (0-based), optionally about some lines of code. Returns where
    /// its answer will appear.
    pub fn ask(&mut self, stop: usize, q: &str, code: Option<Anchor>) -> Option<(usize, usize)> {
        let q = q.trim();
        let s = self.state.tour.stops.get_mut(stop)?;
        if q.is_empty() {
            return None;
        }
        s.thread.push(Exchange { q: q.to_string(), code, ..Default::default() });
        let at = (stop, s.thread.len() - 1);
        self.state.queue.push_back(at);
        if self.state.answering.is_none() {
            self.next_question();
        }
        Some(at)
    }

    fn next_question(&mut self) {
        let Some((s, e)) = self.state.queue.pop_front() else { return };
        let resuming = self.guide.is_none() && self.state.tour.guide.session.is_some();
        let msg = self.question_message(s, e);
        let sent = match self.guide.take() {
            Some(g) => Ok(g),
            None => self.spawn(Role::Guide),
        }
        .and_then(|mut g| g.send(&msg).map(|_| g));
        match sent {
            Ok(g) => {
                self.guide = Some(g);
                self.guide_resuming |= resuming;
                let tour = &mut self.state.tour;
                tour.guide.known = tour.stops.iter().map(|s| s.freshness.revision()).collect();
                self.state.answering = Some((s, e));
                self.state.guide_activity = "thinking".into();
            }
            Err(err) => {
                self.state.tour.stops[s].thread[e].a = format!("(could not reach the guide: {err})");
                self.next_question();
            }
        }
    }

    /// The message for question `e` at stop `s`, with the stops the guide has not seen as they
    /// are now, and the selected code.
    fn question_message(&self, s: usize, e: usize) -> String {
        let tour = &self.state.tour;
        let known = &tour.guide.known;
        let changed: Vec<_> = tour
            .stops
            .iter()
            .enumerate()
            .filter(|(i, stop)| known.get(*i) != Some(&stop.freshness.revision()))
            .collect();
        let stop = &tour.stops[s];
        let ex = &stop.thread[e];
        let lines = ex.code.as_ref().map(|a| self.lines(a));
        let code = ex.code.as_ref().zip(lines.as_deref());
        // A new conversation has seen no stops yet.
        prompts::question(tour, (s, e), known.is_empty(), &changed, code)
    }

    /// The text of an anchor's lines, at most 200 of them.
    fn lines(&self, a: &Anchor) -> String {
        let text = std::fs::read(self.root.join(&a.path)).map(|b| String::from_utf8_lossy(&b).into_owned());
        let text = text.unwrap_or_default();
        let count = (a.end + 1).saturating_sub(a.start).min(200);
        text.lines().skip(a.start.saturating_sub(1)).take(count).collect::<Vec<_>>().join("\n")
    }
}
