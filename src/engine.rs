//! The tour engine: runs the planner and the guide and keeps the tour up to date.
//! The daemon hosts one per folder and feeds it agent events; front-ends see its [`State`].

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent::{self, Agent, Event, Session};
use crate::prompts;
use crate::tour::{self, Exchange, PlanLine, Tour};

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
    plan_buf: String,
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
            plan_buf: String::new(),
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
            None => engine.plan()?,
        }
        Ok(engine)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Throws the tour away, questions included, and plans a new one.
    pub fn replan(&mut self) -> Result<()> {
        self.stop_agents();
        self.generation += 1;
        self.plan_buf.clear();
        self.state = State::default();
        self.plan()
    }

    /// Ends the agent processes.
    pub fn stop_agents(&mut self) {
        self.planner = None;
        self.guide = None;
    }

    pub fn is_busy(&self) -> bool {
        self.state.planning || self.state.is_answering()
    }

    fn plan(&mut self) -> Result<()> {
        self.state.tour = Tour { version: 1, root: self.root.display().to_string(), ..Default::default() };
        self.state.planning = true;
        self.state.planner_activity = "starting".into();
        let mut planner = self.spawn(Role::Planner)?;
        planner.send(prompts::PLANNER_START)?;
        self.planner = Some(planner);
        Ok(())
    }

    fn spawn(&self, role: Role) -> Result<Session> {
        let system_prompt = match role {
            Role::Planner => prompts::PLANNER,
            Role::Guide => prompts::GUIDE,
        };
        let opts = agent::Options { cwd: &self.root, system_prompt, model: self.model.as_deref() };
        Session::spawn(self.agent, Tag { role, generation: self.generation }, &opts, self.tx.clone())
    }

    pub fn on_agent(&mut self, tag: Tag, event: Event) {
        if tag.generation != self.generation {
            return;
        }
        match (tag.role, event) {
            (Role::Planner, Event::Text(t)) => {
                self.plan_buf.push_str(&t);
                while let Some(i) = self.plan_buf.find('\n') {
                    let line: String = self.plan_buf.drain(..=i).collect();
                    self.on_plan_line(&line);
                }
            }
            (Role::Planner, Event::Activity(a)) => self.state.planner_activity = self.relative(&a),
            (Role::Planner, Event::TurnDone(done)) => {
                let rest = std::mem::take(&mut self.plan_buf);
                self.on_plan_line(&rest);
                self.finish_planning(done.err());
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
                if let Some((s, e)) = self.state.answering.take() {
                    let ex = &mut self.state.tour.stops[s].thread[e];
                    let answer = tour::split_answer(&ex.a);
                    (ex.notes, ex.suggested) = (answer.notes, answer.suggested);
                    ex.a = answer.text.to_string();
                    if let Err(err) = done {
                        ex.a.push_str(&format!("\n\n(guide error: {err})"));
                    }
                }
                self.state.guide_activity.clear();
                self.next_question();
            }
            (Role::Guide, Event::Exited(err)) => {
                self.guide = None;
                if let Some((s, e)) = self.state.answering.take() {
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
            Some(PlanLine::Stop(stop)) => self.state.tour.stops.push(stop),
            None => {}
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

    /// Queues a question at a stop (0-based). Returns where its answer will appear.
    pub fn ask(&mut self, stop: usize, q: &str) -> Option<(usize, usize)> {
        let q = q.trim();
        let s = self.state.tour.stops.get_mut(stop)?;
        if q.is_empty() {
            return None;
        }
        s.thread.push(Exchange { q: q.to_string(), a: String::new(), notes: vec![], suggested: vec![] });
        let at = (stop, s.thread.len() - 1);
        self.state.queue.push_back(at);
        if self.state.answering.is_none() {
            self.next_question();
        }
        Some(at)
    }

    fn next_question(&mut self) {
        let Some((s, e)) = self.state.queue.pop_front() else { return };
        let tour = &self.state.tour;
        let stop = &tour.stops[s];
        let msg = prompts::question(s + 1, tour.stops.len(), stop, &stop.thread[e].q);
        let sent = match self.guide.take() {
            Some(g) => Ok(g),
            None => self.spawn(Role::Guide),
        }
        .and_then(|mut g| g.send(&msg).map(|_| g));
        match sent {
            Ok(g) => {
                self.guide = Some(g);
                self.state.answering = Some((s, e));
                self.state.guide_activity = "thinking".into();
            }
            Err(err) => {
                self.state.tour.stops[s].thread[e].a = format!("(could not reach the guide: {err})");
                self.next_question();
            }
        }
    }
}
