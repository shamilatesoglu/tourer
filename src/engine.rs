//! The tour engine: runs the planner and the guide and keeps the tour up to date.
//! Both the TUI and the MCP server drive it by feeding it agent events.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;

use crate::agent::{self, Event, Role, Session};
use crate::prompts;
use crate::tour::{self, Exchange, PlanLine, Tour};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AnswerState {
    Queued,
    Answering,
    Done,
}

pub struct Engine {
    root: PathBuf,
    model: Option<String>,
    tx: Sender<(Role, Event)>,
    pub tour: Tour,
    planner: Option<Session>,
    guide: Option<Session>,
    plan_buf: String,
    pub planning: bool,
    pub planner_activity: String,
    pub guide_activity: String,
    queue: VecDeque<(usize, usize)>,
    answering: Option<(usize, usize)>,
    pub error: Option<String>,
}

impl Engine {
    /// Starts planning a tour of `root`. Agent events arrive on the receiver paired with `tx`
    /// and must be passed to [`Engine::on_agent`].
    pub fn new(root: PathBuf, model: Option<String>, tx: Sender<(Role, Event)>) -> Result<Self> {
        let mut engine = Engine {
            tour: Tour { version: 1, root: root.display().to_string(), ..Default::default() },
            root,
            model,
            tx,
            planner: None,
            guide: None,
            plan_buf: String::new(),
            planning: true,
            planner_activity: "starting".into(),
            guide_activity: String::new(),
            queue: VecDeque::new(),
            answering: None,
            error: None,
        };
        let mut planner = engine.spawn(Role::Planner)?;
        planner.send(prompts::PLANNER_START)?;
        engine.planner = Some(planner);
        Ok(engine)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn spawn(&self, role: Role) -> Result<Session> {
        let system_prompt = match role {
            Role::Planner => prompts::PLANNER,
            Role::Guide => prompts::GUIDE,
        };
        let opts = agent::Options { cwd: &self.root, system_prompt, model: self.model.as_deref() };
        Session::spawn(role, &opts, self.tx.clone())
    }

    pub fn on_agent(&mut self, role: Role, event: Event) {
        match (role, event) {
            (Role::Planner, Event::Text(t)) => {
                self.plan_buf.push_str(&t);
                while let Some(i) = self.plan_buf.find('\n') {
                    let line: String = self.plan_buf.drain(..=i).collect();
                    self.on_plan_line(&line);
                }
            }
            (Role::Planner, Event::Activity(a)) => self.planner_activity = self.relative(&a),
            (Role::Planner, Event::TurnDone(done)) => {
                let rest = std::mem::take(&mut self.plan_buf);
                self.on_plan_line(&rest);
                self.finish_planning(done.err());
            }
            (Role::Planner, Event::Exited(err)) => {
                if self.planning {
                    self.finish_planning(Some(format!("planner exited: {err}")));
                }
            }
            (Role::Guide, Event::Text(t)) => {
                if let Some((s, e)) = self.answering {
                    self.tour.stops[s].thread[e].a.push_str(&t);
                }
            }
            (Role::Guide, Event::Activity(a)) => self.guide_activity = self.relative(&a),
            (Role::Guide, Event::TurnDone(done)) => {
                if let Some((s, e)) = self.answering.take() {
                    let ex = &mut self.tour.stops[s].thread[e];
                    let (answer, suggested) = tour::split_answer(&ex.a);
                    ex.a = answer.to_string();
                    ex.suggested = suggested;
                    if let Err(err) = done {
                        ex.a.push_str(&format!("\n\n(guide error: {err})"));
                    }
                }
                self.guide_activity.clear();
                self.next_question();
            }
            (Role::Guide, Event::Exited(err)) => {
                self.guide = None;
                if let Some((s, e)) = self.answering.take() {
                    self.tour.stops[s].thread[e].a.push_str(&format!("\n\n(guide exited: {err})"));
                }
                self.next_question();
            }
        }
    }

    fn on_plan_line(&mut self, line: &str) {
        match tour::parse_plan_line(line) {
            Some(PlanLine::Tour { title, summary }) => {
                self.tour.title = title;
                self.tour.summary = summary;
            }
            Some(PlanLine::Stop(stop)) => self.tour.stops.push(stop),
            None => {}
        }
    }

    fn finish_planning(&mut self, err: Option<String>) {
        self.planning = false;
        self.planner = None;
        self.planner_activity.clear();
        if let Some(err) = err {
            self.error = Some(err);
        } else if self.tour.stops.is_empty() {
            self.error = Some("the planner finished without producing any stops".into());
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
        let s = self.tour.stops.get_mut(stop)?;
        if q.is_empty() {
            return None;
        }
        s.thread.push(Exchange { q: q.to_string(), a: String::new(), suggested: vec![] });
        let at = (stop, s.thread.len() - 1);
        self.queue.push_back(at);
        if self.answering.is_none() {
            self.next_question();
        }
        Some(at)
    }

    fn next_question(&mut self) {
        let Some((s, e)) = self.queue.pop_front() else { return };
        let stop = &self.tour.stops[s];
        let msg = prompts::question(s + 1, self.tour.stops.len(), stop, &stop.thread[e].q);
        let sent = match self.guide.take() {
            Some(g) => Ok(g),
            None => self.spawn(Role::Guide),
        }
        .and_then(|mut g| g.send(&msg).map(|_| g));
        match sent {
            Ok(g) => {
                self.guide = Some(g);
                self.answering = Some((s, e));
                self.guide_activity = "thinking".into();
            }
            Err(err) => {
                self.tour.stops[s].thread[e].a = format!("(could not reach the guide: {err})");
                self.next_question();
            }
        }
    }

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
