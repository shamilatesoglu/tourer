//! The tour screen: the tour as one scrolling document beside the code it cites, with popups.
//! This file holds its state and input handling; `ui.rs` draws it and fills in the layout.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::agent::Agent;
use crate::app::Action;
use crate::engine::State;
use crate::highlight::{Highlighter, StyledLine};
use crate::tour::Anchor;

/// How long an item must be on screen before it counts as seen.
const SEEN_AFTER: Duration = Duration::from_millis(1500);
/// Folders the file picker leaves out, besides hidden ones.
const SKIPPED_DIRS: [&str; 7] = ["target", "node_modules", "dist", "build", "__pycache__", "venv", "vendor"];
const MAX_FILES: usize = 20_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pane {
    Text,
    Code,
}

/// A scroll position that glides to its target over a few frames.
#[derive(Debug, Default)]
pub struct Scroll {
    pos: f32,
    target: usize,
}

impl Scroll {
    /// The first line shown now.
    pub fn top(&self) -> usize {
        self.pos.round() as usize
    }

    pub fn target(&self) -> usize {
        self.target
    }

    pub fn is_moving(&self) -> bool {
        self.target as f32 != self.pos
    }

    pub fn to(&mut self, line: usize) {
        self.target = line;
    }

    fn by(&mut self, lines: isize) {
        self.target = self.target.saturating_add_signed(lines);
    }

    /// Keeps the target, and the position, at or above `max`.
    pub fn limit(&mut self, max: usize) {
        self.target = self.target.min(max);
        self.pos = self.pos.min(max as f32);
    }

    /// Moves part of the way to the target. Returns whether it is still moving.
    pub fn step(&mut self) -> bool {
        let distance = self.target as f32 - self.pos;
        if distance.abs() < 0.5 {
            self.pos = self.target as f32;
            return false;
        }
        self.pos += distance * 0.4;
        true
    }
}

/// What the code pane shows.
#[derive(Debug, Clone, PartialEq)]
pub enum Shown {
    /// The code cited where the person is reading.
    Follow,
    /// A code reference the person picked.
    Ref(Anchor),
    /// A file the person opened.
    File(String),
}

/// A file and, unless a whole file was opened, the lines to point out.
#[derive(Debug, Clone, PartialEq)]
pub struct CodeTarget {
    pub path: String,
    pub range: Option<(usize, usize)>,
}

#[derive(Debug)]
pub struct CodeView {
    pub shown: Shown,
    /// What was shown in the last frame, to notice when it changes.
    pub last: Option<CodeTarget>,
    pub scroll: Scroll,
    /// The line the cursor is on, 1-based.
    pub cursor: usize,
    /// Where a selection started; the selection runs from here to the cursor.
    pub mark: Option<usize>,
    /// When the pointed-out lines last changed, to flash them.
    pub flash: Option<Instant>,
    /// Set while drawing.
    pub lines: usize,
    pub height: usize,
}

impl CodeView {
    /// The selected lines, or the cursor's line.
    pub fn selection(&self) -> (usize, usize) {
        let mark = self.mark.unwrap_or(self.cursor);
        (mark.min(self.cursor), mark.max(self.cursor))
    }

    fn move_cursor(&mut self, by: isize, extend: bool) {
        if extend {
            self.mark.get_or_insert(self.cursor);
        } else {
            self.mark = None;
        }
        self.cursor = self.cursor.saturating_add_signed(by).clamp(1, self.lines.max(1));
        self.reveal_cursor();
    }

    /// Scrolls just enough to keep the cursor on screen with a little room around it.
    fn reveal_cursor(&mut self) {
        let room = 2.min(self.height / 4);
        let line = self.cursor - 1;
        if line < self.scroll.target() + room {
            self.scroll.to(line.saturating_sub(room));
        } else if line + room >= self.scroll.target() + self.height {
            self.scroll.to(line + room + 1 - self.height.min(line + room + 1));
        }
    }
}

/// What the person is typing: a question at a stop, maybe about some lines.
#[derive(Debug)]
pub struct Input {
    pub stop: usize,
    pub code: Option<Anchor>,
    pub text: String,
}

#[derive(Debug)]
pub enum Popup {
    /// The list of stops.
    Outline { sel: usize },
    /// Opens any file in the folder.
    Files { query: String, sel: usize },
    /// The code pane, over the text, when the screen is too narrow for both.
    Code,
}

/// Something in the document that can be updated, and so seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Item {
    Stop(usize),
    Answer(usize, usize),
}

/// A place ↑ and ↓ move between in the text, in document order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// The `n`-th code reference in a stop's part of the document.
    Ref { stop: usize, n: usize },
    /// A stop without code references.
    Stop(usize),
    /// A question asked at a stop, with its answer.
    Answer(usize, usize),
    /// The `n`-th suggested question at a stop.
    Suggestion { stop: usize, n: usize },
    /// The row for asking a question of one's own at a stop.
    Ask(usize),
}

impl Step {
    pub fn stop(self) -> usize {
        match self {
            Step::Ref { stop, .. } | Step::Suggestion { stop, .. } => stop,
            Step::Stop(stop) | Step::Answer(stop, _) | Step::Ask(stop) => stop,
        }
    }
}

/// Something clickable in the text pane.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Link(usize),
    /// A suggested question at a stop.
    Suggestion(usize, String),
    /// The row for asking a question of one's own at a stop.
    Ask(usize),
    Stop(usize),
}

/// The document's layout, found while drawing and used to handle input.
#[derive(Debug, Default)]
pub struct Doc {
    pub links: Vec<Anchor>,
    /// The document line of each link.
    pub link_lines: Vec<usize>,
    /// The document line each stop starts at.
    pub stop_lines: Vec<usize>,
    /// The links in each stop's part of the document.
    pub stop_links: Vec<Range<usize>>,
    /// The steps, in order, with their document lines.
    pub steps: Vec<(Step, usize)>,
    pub items: Vec<(Range<usize>, Item)>,
    /// Lines in the document, and in the text pane.
    pub len: usize,
    pub height: usize,
    /// The input's document line, if it is open.
    pub input_line: Option<usize>,
    /// The last line of each stop's answers, for following an answer as it arrives.
    pub thread_ends: Vec<usize>,
}

/// Where things were drawn, for the mouse. Hidden things have empty areas.
#[derive(Debug, Default)]
pub struct Areas {
    pub text: Rect,
    pub code: Rect,
    pub popup: Rect,
    /// Clickable spots on screen.
    pub spots: Vec<(Rect, Target)>,
    /// Rows of the popup's list, with the index each shows.
    pub popup_rows: Vec<(Rect, usize)>,
}

enum Drag {
    Border,
    Select,
}

pub struct TourView {
    pub agent: Agent,
    pub state: State,
    root: PathBuf,
    pub hl: Highlighter,
    files: HashMap<String, Option<Vec<StyledLine>>>,
    /// Every file in the folder, listed when the file picker first opens.
    all_files: Option<Vec<String>>,
    pub text: Scroll,
    pub code: CodeView,
    pub focus: Pane,
    pub zoomed: bool,
    /// Whether the text and code fit side by side. Set while drawing.
    pub wide: bool,
    /// The text pane's share of the width, in percent, when side by side.
    pub text_percent: u16,
    pub popup: Option<Popup>,
    pub input: Option<Input>,
    /// The stop at the top of the text pane. Set while drawing.
    pub current: usize,
    /// A stop being scrolled to, which counts as current until the text gets there.
    pub jump: Option<usize>,
    /// Keep the end of this stop's answers in view while they arrive.
    pub follow: Option<usize>,
    /// Where ↑ and ↓ have moved to; `None` after scrolling by hand.
    pub step: Option<Step>,
    pub doc: Doc,
    pub areas: Areas,
    pub hover: Option<Position>,
    drag: Option<Drag>,
    /// When each item that is still to be seen came into view.
    in_view: HashMap<(Item, u32), Instant>,
    sent_seen: HashSet<(Item, u32)>,
    /// Items seen, waiting to be reported.
    pub seen_out: Vec<(Item, u32)>,
}

impl TourView {
    pub fn new(root: PathBuf, agent: Agent) -> Self {
        TourView {
            agent,
            state: State::default(),
            root,
            hl: Highlighter::new(),
            files: HashMap::new(),
            all_files: None,
            text: Scroll::default(),
            code: CodeView {
                shown: Shown::Follow,
                last: None,
                scroll: Scroll::default(),
                cursor: 1,
                mark: None,
                flash: None,
                lines: 0,
                height: 0,
            },
            focus: Pane::Text,
            zoomed: false,
            wide: true,
            text_percent: 50,
            popup: None,
            input: None,
            current: 0,
            jump: None,
            follow: None,
            step: None,
            doc: Doc::default(),
            areas: Areas::default(),
            hover: None,
            drag: None,
            in_view: HashMap::new(),
            sent_seen: HashSet::new(),
            seen_out: vec![],
        }
    }

    pub fn set_state(&mut self, state: State) {
        self.state = state;
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

    /// Whether something is still moving on screen, so frames must keep coming.
    pub fn is_animating(&self) -> bool {
        let flashing = self.code.flash.is_some_and(|t| t.elapsed() < Duration::from_millis(900));
        self.text.is_moving() || self.code.scroll.is_moving() || flashing || !self.in_view.is_empty()
    }

    /// Counts the items now on screen that are still to be seen, and queues the ones that have
    /// been on screen long enough. Called after each frame is laid out.
    pub fn track_seen(&mut self, visible: Vec<(Item, u32)>) {
        let now = Instant::now();
        self.in_view.retain(|key, _| visible.contains(key));
        for key in visible {
            if self.sent_seen.contains(&key) {
                continue;
            }
            let since = *self.in_view.entry(key).or_insert(now);
            if now - since >= SEEN_AFTER {
                self.in_view.remove(&key);
                self.sent_seen.insert(key);
                self.seen_out.push(key);
            }
        }
    }

    /// The code reference the code pane follows: the first one on screen, else the current stop's.
    pub fn followed_ref(&self) -> Option<Anchor> {
        let top = self.text.top();
        let on_screen = |line: &usize| (top..top + self.doc.height).contains(line);
        match self.doc.link_lines.iter().position(on_screen) {
            Some(i) => self.doc.links.get(i).cloned(),
            None => self.state.tour.stops.get(self.current)?.anchors.first().cloned(),
        }
    }

    /// What the code pane should show now.
    pub fn code_target(&self) -> Option<CodeTarget> {
        let anchor = match &self.code.shown {
            Shown::File(path) => return Some(CodeTarget { path: path.clone(), range: None }),
            Shown::Ref(a) => Some(a.clone()),
            Shown::Follow => self.followed_ref(),
        }?;
        Some(CodeTarget { path: anchor.path, range: Some((anchor.start, anchor.end)) })
    }

    /// Scrolls the text to a stop. Drawing aims at where the stop is now.
    fn go_to_stop(&mut self, i: usize) {
        if i < self.doc.stop_lines.len() {
            self.jump = Some(i);
            self.current = i;
            self.code.shown = Shown::Follow;
            self.follow = None;
            self.step = None;
        }
    }

    /// Scrolls the text by hand, which also stops following an answer or a picked reference.
    fn scroll_text(&mut self, lines: isize) {
        self.text.by(lines);
        self.follow = None;
        self.jump = None;
        self.step = None;
        if let Shown::Ref(_) = self.code.shown {
            self.code.shown = Shown::Follow;
        }
    }

    /// Shows a code reference from the text: in the code pane, or in a popup when narrow.
    fn open_link(&mut self, i: usize) {
        let Some(anchor) = self.doc.links.get(i).cloned() else { return };
        if let Some(stop) = self.doc.stop_links.iter().position(|r| r.contains(&i)) {
            self.step = Some(Step::Ref { stop, n: i - self.doc.stop_links[stop].start });
        }
        self.code.shown = Shown::Ref(anchor);
        if !self.wide {
            self.popup = Some(Popup::Code);
            self.focus = Pane::Code;
        }
    }

    /// The document link a code reference step points at.
    pub fn step_link(&self, step: Step) -> Option<usize> {
        match step {
            Step::Ref { stop, n } => Some(self.doc.stop_links.get(stop)?.start + n),
            _ => None,
        }
    }

    /// Moves to the next step, or the previous one. After scrolling by hand, starts from the
    /// first step on screen.
    fn step_by(&mut self, forward: bool) {
        let steps = &self.doc.steps;
        let at = self.step.and_then(|step| steps.iter().position(|(s, _)| *s == step));
        let i = match at {
            Some(i) if forward => i + 1,
            Some(i) => i.saturating_sub(1),
            None => steps.iter().position(|&(_, line)| line >= self.text.target()).unwrap_or(steps.len()),
        };
        if let Some(&(step, _)) = steps.get(i.min(steps.len().saturating_sub(1))) {
            self.set_step(step);
        }
    }

    /// Moves to a step: scrolls the text to it and shows its code.
    fn set_step(&mut self, step: Step) {
        let Some(&(_, line)) = self.doc.steps.iter().find(|(s, _)| *s == step) else { return };
        let height = self.doc.height;
        let top = self.text.target();
        let stop_line = self.doc.stop_lines.get(step.stop()).copied().unwrap_or(0);
        // A stop is entered at its top, so its text is read before its code.
        if step.stop() != self.current && line + 3 < stop_line + height {
            self.text.to(stop_line);
        } else if !(top + 1..top + height.saturating_sub(2)).contains(&line) {
            self.text.to(line.saturating_sub(height / 3));
        }
        self.step = Some(step);
        self.current = step.stop();
        self.follow = None;
        self.jump = None;
        match step {
            Step::Ref { .. } if self.wide => {
                if let Some(anchor) = self.step_link(step).and_then(|i| self.doc.links.get(i)) {
                    self.code.shown = Shown::Ref(anchor.clone());
                }
            }
            Step::Stop(_) => self.code.shown = Shown::Follow,
            _ => {}
        }
    }

    /// Enter in the text: asks the selected suggestion, or opens the question box.
    fn enter(&mut self) -> Option<Action> {
        match self.step {
            Some(Step::Suggestion { stop, n }) => {
                let q = self.state.tour.stops.get(stop)?.suggestions().get(n)?.clone();
                self.ask(q, stop, None)
            }
            _ => {
                self.start_input();
                None
            }
        }
    }

    fn open_file(&mut self, path: String) {
        self.code.shown = Shown::File(path);
        self.focus = Pane::Code;
        self.popup = if self.wide { None } else { Some(Popup::Code) };
    }

    /// The folder's files whose paths contain every word of `query`, shortest first.
    pub fn matching_files(&mut self, query: &str) -> Vec<String> {
        let root = self.root.clone();
        let all = self.all_files.get_or_insert_with(|| list_files(&root));
        let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
        let mut found: Vec<String> =
            all.iter().filter(|p| words.iter().all(|w| p.to_lowercase().contains(w))).cloned().collect();
        found.sort_by_key(|p| (p.len(), p.clone()));
        found
    }

    fn ask(&mut self, question: String, stop: usize, code: Option<Anchor>) -> Option<Action> {
        if question.trim().is_empty() || stop >= self.state.tour.stops.len() {
            return None;
        }
        self.follow = Some(stop);
        self.step = Some(Step::Answer(stop, self.state.tour.stops[stop].thread.len()));
        Some(Action::Ask { stop, question, code })
    }

    fn start_input(&mut self) {
        if self.state.tour.stops.is_empty() {
            return;
        }
        let code = match (self.focus, self.code_target()) {
            (Pane::Code, Some(t)) => {
                let (start, end) = self.code.selection();
                Some(Anchor { path: t.path, start, end })
            }
            _ => None,
        };
        let stop = self.current.min(self.state.tour.stops.len() - 1);
        self.input = Some(Input { stop, code, text: String::new() });
        self.follow = Some(stop);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if let Some(input) = &mut self.input {
            match key.code {
                KeyCode::Enter => {
                    let Input { stop, code, text } = self.input.take()?;
                    return self.ask(text, stop, code);
                }
                KeyCode::Esc => self.input = None,
                KeyCode::Backspace => {
                    input.text.pop();
                }
                KeyCode::Char(c) => input.text.push(c),
                _ => {}
            }
            return None;
        }
        match &mut self.popup {
            Some(Popup::Outline { sel }) => {
                let n = self.state.tour.stops.len();
                match key.code {
                    KeyCode::Up => *sel = sel.saturating_sub(1),
                    KeyCode::Down => *sel = (*sel + 1).min(n.saturating_sub(1)),
                    KeyCode::Enter => {
                        let i = *sel;
                        self.popup = None;
                        self.go_to_stop(i);
                    }
                    KeyCode::Esc | KeyCode::Char('o') | KeyCode::Char('q') => self.popup = None,
                    _ => {}
                }
                return None;
            }
            Some(Popup::Files { query, sel }) => {
                match key.code {
                    KeyCode::Up => *sel = sel.saturating_sub(1),
                    KeyCode::Down => *sel += 1,
                    KeyCode::Backspace => {
                        query.pop();
                        *sel = 0;
                    }
                    KeyCode::Char(c) => {
                        query.push(c);
                        *sel = 0;
                    }
                    KeyCode::Esc => self.popup = None,
                    KeyCode::Enter => {
                        let (query, sel) = (query.clone(), *sel);
                        if let Some(path) = self.matching_files(&query).into_iter().nth(sel) {
                            self.open_file(path);
                        }
                    }
                    _ => {}
                }
                return None;
            }
            Some(Popup::Code) if key.code == KeyCode::Esc && self.code.mark.is_none() => {
                self.popup = None;
                self.focus = Pane::Text;
                self.code.shown = Shown::Follow;
                return None;
            }
            Some(Popup::Code) | None => {}
        }

        let page = |height: usize| height.saturating_sub(2).max(1) as isize;
        match (key.code, self.focus) {
            (KeyCode::Char('q'), _) => return Some(Action::Quit),
            (KeyCode::Right, _) => self.go_to_stop(self.current + 1),
            (KeyCode::Left, _) => self.go_to_stop(self.current.saturating_sub(1)),
            (KeyCode::Down | KeyCode::Char(' '), Pane::Text) => self.step_by(true),
            (KeyCode::Up, Pane::Text) => self.step_by(false),
            (KeyCode::PageDown, Pane::Text) => self.scroll_text(page(self.doc.height)),
            (KeyCode::PageUp, Pane::Text) => self.scroll_text(-page(self.doc.height)),
            (KeyCode::Home, Pane::Text) => self.scroll_text(isize::MIN / 2),
            (KeyCode::End, Pane::Text) => self.scroll_text(isize::MAX / 2),
            (KeyCode::Down, Pane::Code) => self.code.move_cursor(1, shift),
            (KeyCode::Up, Pane::Code) => self.code.move_cursor(-1, shift),
            (KeyCode::PageDown, Pane::Code) => self.code.move_cursor(page(self.code.height), shift),
            (KeyCode::PageUp, Pane::Code) => self.code.move_cursor(-page(self.code.height), shift),
            (KeyCode::Home, Pane::Code) => self.code.move_cursor(isize::MIN / 2, shift),
            (KeyCode::End, Pane::Code) => self.code.move_cursor(isize::MAX / 2, shift),
            (KeyCode::Tab | KeyCode::BackTab, _) if self.wide => {
                self.focus = if self.focus == Pane::Text { Pane::Code } else { Pane::Text };
            }
            (KeyCode::Enter, Pane::Text) => return self.enter(),
            (KeyCode::Enter, Pane::Code) => self.start_input(),
            (KeyCode::Esc, _) => {
                if self.code.mark.is_some() {
                    self.code.mark = None;
                } else if self.code.shown != Shown::Follow {
                    self.code.shown = Shown::Follow;
                } else {
                    self.zoomed = false;
                }
            }
            (KeyCode::Char('z'), _) if self.wide => self.zoomed = !self.zoomed,
            (KeyCode::Char('?') | KeyCode::Char('4'), _) => self.start_input(),
            (KeyCode::Char('o'), _) => self.popup = Some(Popup::Outline { sel: self.current }),
            (KeyCode::Char('f'), _) => self.popup = Some(Popup::Files { query: String::new(), sel: 0 }),
            (KeyCode::Char(c @ '1'..='3'), _) => {
                let i = c as usize - '1' as usize;
                let stop = self.state.tour.stops.get(self.current);
                if let Some(q) = stop.and_then(|s| s.suggestions().get(i)).cloned() {
                    return self.ask(q, self.current, None);
                }
            }
            _ => {}
        }
        None
    }

    pub fn on_mouse(&mut self, m: MouseEvent) -> Option<Action> {
        let at = Position::new(m.column, m.row);
        self.hover = Some(at);
        let a = &self.areas;
        let in_code = a.code.contains(at);
        let in_text = a.text.contains(at);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.popup.is_some() && !matches!(self.popup, Some(Popup::Code)) {
                    return self.click_popup(at);
                }
                if let Some((_, target)) = a.spots.iter().find(|(r, _)| r.contains(at)) {
                    match target.clone() {
                        Target::Link(i) => self.open_link(i),
                        Target::Suggestion(stop, q) => return self.ask(q, stop, None),
                        Target::Ask(stop) => {
                            self.current = stop;
                            self.step = Some(Step::Ask(stop));
                            self.start_input();
                        }
                        Target::Stop(i) => self.go_to_stop(i),
                    }
                    return None;
                }
                let on_border = self.wide
                    && a.text.width > 0
                    && a.code.width > 0
                    && (a.text.right().saturating_sub(1)..=a.code.x).contains(&at.x)
                    && at.y >= a.text.y;
                if on_border {
                    self.drag = Some(Drag::Border);
                } else if in_code {
                    self.focus = Pane::Code;
                    if let Some(line) = self.code_line_at(at.y) {
                        self.code.cursor = line;
                        self.code.mark = Some(line);
                        self.drag = Some(Drag::Select);
                    }
                } else if in_text {
                    self.focus = Pane::Text;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => match self.drag {
                Some(Drag::Border) => {
                    let total = (a.text.width + a.code.width).max(1);
                    let x = at.x.saturating_sub(a.text.x) + 1;
                    self.text_percent = (x * 100 / total).clamp(25, 80);
                }
                Some(Drag::Select) => {
                    if let Some(line) = self.code_line_at(at.y) {
                        self.code.cursor = line;
                    }
                }
                None => {}
            },
            MouseEventKind::Up(MouseButton::Left) => {
                // A click without a drag leaves only the cursor.
                if let Some(Drag::Select) = self.drag
                    && self.code.mark == Some(self.code.cursor)
                {
                    self.code.mark = None;
                }
                self.drag = None;
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                let lines = if m.kind == MouseEventKind::ScrollDown { 3 } else { -3 };
                if in_code {
                    self.code.scroll.by(lines);
                } else if in_text {
                    self.scroll_text(lines);
                }
            }
            _ => {}
        }
        None
    }

    /// A click while a list popup is open: picks a row, or closes the popup when outside it.
    fn click_popup(&mut self, at: Position) -> Option<Action> {
        if !self.areas.popup.contains(at) {
            self.popup = None;
            return None;
        }
        let row = self.areas.popup_rows.iter().find(|(r, _)| r.contains(at)).map(|&(_, i)| i)?;
        match self.popup.take() {
            Some(Popup::Outline { .. }) => self.go_to_stop(row),
            Some(Popup::Files { query, .. }) => {
                if let Some(path) = self.matching_files(&query).into_iter().nth(row) {
                    self.open_file(path);
                }
            }
            other => self.popup = other,
        }
        None
    }

    /// The file line shown at screen row `y` of the code pane.
    pub fn code_line_at(&self, y: u16) -> Option<usize> {
        let area = self.areas.code;
        let row = y.checked_sub(area.y + 1)? as usize;
        let line = self.code.scroll.top() + row + 1;
        (row < self.code.height && line <= self.code.lines).then_some(line)
    }
}

/// The folder's files as relative paths with `/`, leaving out hidden and build folders.
fn list_files(root: &Path) -> Vec<String> {
    let mut found = vec![];
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            match entry.file_type() {
                Ok(t) if t.is_dir() => {
                    if !SKIPPED_DIRS.contains(&name.as_str()) {
                        dirs.push(path);
                    }
                }
                Ok(t) if t.is_file() => {
                    if let Ok(rel) = path.strip_prefix(root) {
                        found.push(rel.to_string_lossy().replace('\\', "/"));
                    }
                    if found.len() >= MAX_FILES {
                        return found;
                    }
                }
                _ => {}
            }
        }
    }
    found
}
