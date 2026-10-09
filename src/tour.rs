use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tour {
    pub version: u32,
    pub root: String,
    pub title: String,
    pub summary: String,
    pub stops: Vec<Stop>,
    /// The guide's conversation, kept so it can be continued after a restart.
    #[serde(default)]
    pub guide: GuideMemory,
}

/// What the guide's saved conversation knows, so each question only tells it what changed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GuideMemory {
    /// The agent's id for the conversation; `None` until it has started.
    pub session: Option<String>,
    /// The version of each stop the guide was last shown, in stop order.
    pub known: Vec<u32>,
}

/// Whether a person has seen the latest version of something agents can update.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Freshness {
    /// How many times it was updated after it was first written.
    revision: u32,
    /// The revision last shown to the person; `None` if it never was.
    seen: Option<u32>,
}

impl Freshness {
    pub fn revision(self) -> u32 {
        self.revision
    }

    pub fn update(&mut self) {
        self.revision += 1;
    }

    /// Records that the person saw `revision`. A later revision stays unseen.
    pub fn mark_seen(&mut self, revision: u32) {
        let revision = revision.min(self.revision);
        self.seen = Some(self.seen.map_or(revision, |s| s.max(revision)));
    }

    pub fn is_visited(self) -> bool {
        self.seen.is_some()
    }

    /// Updated since the person last saw it.
    pub fn has_unseen_update(self) -> bool {
        self.seen.is_some_and(|s| s < self.revision)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stop {
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub anchors: Vec<Anchor>,
    #[serde(default)]
    pub notes: Vec<Note>,
    #[serde(default)]
    pub suggested: Vec<String>,
    #[serde(default)]
    pub thread: Vec<Exchange>,
    #[serde(default)]
    pub freshness: Freshness,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Anchor {
    pub path: String,
    pub start: usize,
    pub end: usize,
}

impl std::fmt::Display for Anchor {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "[{}:{}-{}]", self.path, self.start, self.end)
    }
}

/// The guide's comment on one line of code, shown next to that line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub path: String,
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Exchange {
    pub q: String,
    /// The code the question is about, when it was asked about selected lines.
    #[serde(default)]
    pub code: Option<Anchor>,
    pub a: String,
    #[serde(default)]
    pub notes: Vec<Note>,
    #[serde(default)]
    pub suggested: Vec<String>,
    #[serde(default)]
    pub freshness: Freshness,
}

impl Stop {
    /// Suggestions to offer now: the latest answer's, else the stop's own.
    pub fn suggestions(&self) -> &[String] {
        match self.thread.last() {
            Some(ex) if !ex.suggested.is_empty() => &ex.suggested,
            Some(ex) if ex.a.is_empty() => &[],
            _ => &self.suggested,
        }
    }
}

impl Stop {
    /// Changes whenever the stop or one of its answers is updated.
    pub fn version(&self) -> u32 {
        self.freshness.revision() + self.thread.iter().map(|ex| ex.freshness.revision()).sum::<u32>()
    }

    /// The stop or one of its answers was updated since the person last saw it.
    pub fn has_unseen_update(&self) -> bool {
        self.freshness.has_unseen_update() || self.thread.iter().any(|ex| ex.freshness.has_unseen_update())
    }

    /// Replaces the fields the update gives.
    pub fn apply(&mut self, update: StopUpdate) {
        let StopUpdate { stop: _, title, body, anchors, notes, suggested } = update;
        if let Some(title) = title {
            self.title = title;
        }
        if let Some(body) = body {
            self.body = body;
        }
        if let Some(anchors) = anchors {
            self.anchors = anchors;
        }
        if let Some(notes) = notes {
            self.notes = notes;
        }
        if let Some(suggested) = suggested {
            self.suggested = suggested;
        }
        self.freshness.update();
    }

    /// Notes on a file from this stop and the answers given at it, without repeats.
    pub fn notes_in(&self, path: &str) -> Vec<&Note> {
        let answers = self.thread.iter().flat_map(|ex| &ex.notes);
        let mut notes: Vec<&Note> = vec![];
        for note in self.notes.iter().chain(answers).filter(|n| n.path == path) {
            if !notes.contains(&note) {
                notes.push(note);
            }
        }
        notes
    }
}

/// One line of planner output.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PlanLine {
    Tour { title: String, summary: String },
    Stop(Stop),
    Update(StopUpdate),
    /// The planner wrote its last stop.
    Done,
}

/// Replaces fields of an earlier stop. Fields left out stay as they are.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct StopUpdate {
    /// The stop's number, starting at 1.
    pub stop: usize,
    pub title: Option<String>,
    pub body: Option<String>,
    pub anchors: Option<Vec<Anchor>>,
    pub notes: Option<Vec<Note>>,
    pub suggested: Option<Vec<String>>,
}

/// Replaces one of the guide's earlier answers.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AnswerUpdate {
    /// The stop's number, starting at 1.
    pub stop: usize,
    /// The question's number at that stop, starting at 1.
    pub question: usize,
    pub text: String,
    pub notes: Option<Vec<Note>>,
}

/// A correction the guide makes to something written earlier.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Update {
    Update(StopUpdate),
    UpdateAnswer(AnswerUpdate),
}

/// A title without the number some models put in front of it: "2. Setup" → "Setup".
pub fn without_number(title: &str) -> &str {
    let rest = title.trim_start_matches(|c: char| c.is_ascii_digit());
    match rest.strip_prefix(['.', ')', ':']) {
        Some(rest) if rest.len() + 1 < title.len() => rest.trim_start(),
        _ => title,
    }
}

pub fn parse_plan_line(line: &str) -> Option<PlanLine> {
    let line = line.trim();
    if !line.starts_with('{') {
        return None;
    }
    serde_json::from_str(line).ok()
}

/// Marker the guide puts before its line notes.
pub const NOTES_MARKER: &str = "<<notes>>";
/// Marker the guide puts before its corrections to earlier stops and answers.
pub const UPDATE_MARKER: &str = "<<update>>";
/// Marker the guide puts before its suggested follow-up questions.
pub const NEXT_MARKER: &str = "<<next>>";
const MARKERS: [&str; 3] = [NOTES_MARKER, UPDATE_MARKER, NEXT_MARKER];

/// A guide answer split into its parts.
#[derive(Debug, Default, PartialEq)]
pub struct Answer<'a> {
    pub text: &'a str,
    pub notes: Vec<Note>,
    pub updates: Vec<Update>,
    pub suggested: Vec<String>,
}

/// Splits a raw guide answer into text, line notes, updates and suggested follow-ups.
/// Also hides a marker that is only partially streamed so far.
pub fn split_answer(raw: &str) -> Answer<'_> {
    let at = |marker: &str| raw.find(marker);
    let end = MARKERS.iter().filter_map(|m| at(m)).min();
    let text = match end {
        Some(i) => raw[..i].trim_end(),
        None => match raw.rfind("\n<") {
            Some(i) if MARKERS.iter().any(|m| m.starts_with(raw[i + 1..].trim_end())) => {
                raw[..i].trim_end()
            }
            _ => raw,
        },
    };
    let section = |marker: &str| {
        let Some(i) = at(marker) else { return vec![] };
        let body = &raw[i + marker.len()..];
        let stop = MARKERS.iter().filter_map(|m| body.find(m)).min();
        body[..stop.unwrap_or(body.len())]
            .lines()
            .map(|l| l.trim().trim_start_matches(['-', '*', ' ']).trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let notes = section(NOTES_MARKER).iter().filter_map(|l| parse_note(l)).collect();
    let updates = section(UPDATE_MARKER).iter().filter_map(|l| serde_json::from_str(l).ok()).collect();
    let suggested = section(NEXT_MARKER).into_iter().take(3).collect();
    Answer { text, notes, updates, suggested }
}

/// "src/a.rs:12: text" or "[src/a.rs:12] text" → a note.
fn parse_note(line: &str) -> Option<Note> {
    static NOTE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\[?([\w./@+-]+):(\d+)(?:-\d+)?\]?:?\s+(.+)$").unwrap());
    let c = NOTE.captures(line)?;
    Some(Note { path: c[1].to_string(), line: c[2].parse().ok()?, text: c[3].trim().to_string() })
}

/// A code reference. Some models write it as a markdown link, `[a.rs:3](a.rs:3)`; the link
/// target is part of the match so it is not shown.
static REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([\w./@+-]+):(\d+)(?:-(\d+))?\](?:\([^)\s]*\))?").unwrap());

/// Code references like `[src/main.rs:10-20]` found in text, with their byte ranges.
pub fn refs(text: &str) -> Vec<(std::ops::Range<usize>, Anchor)> {
    REF.captures_iter(text)
        .map(|c| {
            let start: usize = c[2].parse().unwrap_or(1);
            let end = c.get(3).and_then(|m| m.as_str().parse().ok()).unwrap_or(start);
            let anchor = Anchor { path: c[1].to_string(), start, end: end.max(start) };
            (c.get(0).unwrap().range(), anchor)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan_lines() {
        assert!(matches!(
            parse_plan_line(r#"{"type":"tour","title":"T","summary":"S"}"#),
            Some(PlanLine::Tour { .. })
        ));
        let stop = r#"{"type":"stop","title":"A","body":"b","anchors":[{"path":"x.rs","start":1,"end":4}],"suggested":["why?"]}"#;
        let Some(PlanLine::Stop(s)) = parse_plan_line(stop) else { panic!() };
        assert_eq!(s.anchors[0].end, 4);
        assert!(parse_plan_line("Let me look around.").is_none());
        assert!(matches!(parse_plan_line(r#"{"type":"done"}"#), Some(PlanLine::Done)));
    }

    #[test]
    fn splits_answers() {
        let a = split_answer("It works.\n<<next>>\n- Why?\nHow?\n");
        assert_eq!(a.text, "It works.");
        assert_eq!(a.suggested, vec!["Why?", "How?"]);
        assert_eq!(split_answer("Partial\n<<ne").text, "Partial");
        assert_eq!(split_answer("Partial\n<<no").text, "Partial");
        assert_eq!(split_answer("a < b\n<div>").text, "a < b\n<div>");
    }

    #[test]
    fn splits_answer_notes() {
        let a = split_answer("Yes.\n<<notes>>\nsrc/a.rs:12: 3 files opened\n- [b.rs:3] loops\n<<next>>\nWhy?");
        assert_eq!(a.text, "Yes.");
        assert_eq!(a.notes[0], Note { path: "src/a.rs".into(), line: 12, text: "3 files opened".into() });
        assert_eq!(a.notes[1].text, "loops");
        assert_eq!(a.suggested, vec!["Why?"]);
    }

    #[test]
    fn parses_updates() {
        let line = r#"{"type":"update","stop":2,"body":"New text."}"#;
        let Some(PlanLine::Update(u)) = parse_plan_line(line) else { panic!() };
        assert_eq!((u.stop, u.body.as_deref(), u.title), (2, Some("New text."), None));
        let a = split_answer(concat!(
            "Answer.\n<<update>>\n",
            r#"{"type":"update","stop":1,"title":"Better"}"#,
            "\n",
            r#"{"type":"update_answer","stop":1,"question":2,"text":"Fixed."}"#,
            "\nnot json\n<<next>>\nWhy?",
        ));
        assert_eq!(a.text, "Answer.");
        assert_eq!(a.updates.len(), 2);
        assert!(matches!(&a.updates[1], Update::UpdateAnswer(u) if u.question == 2 && u.text == "Fixed."));
        assert_eq!(a.suggested, vec!["Why?"]);
        assert_eq!(split_answer("Partial\n<<up").text, "Partial");
    }

    #[test]
    fn drops_numbers_from_titles() {
        assert_eq!(without_number("2. A tour"), "A tour");
        assert_eq!(without_number("10) Setup"), "Setup");
        assert_eq!(without_number("3D rendering"), "3D rendering");
        assert_eq!(without_number(". odd"), ". odd");
    }

    #[test]
    fn tracks_unseen_updates() {
        let mut f = Freshness::default();
        assert!(!f.is_visited() && !f.has_unseen_update());
        f.update();
        assert!(!f.has_unseen_update(), "never shown, so there is no change to point out");
        f.mark_seen(1);
        f.update();
        assert!(f.has_unseen_update());
        f.mark_seen(1);
        assert!(f.has_unseen_update(), "seeing an older revision keeps the newer one unseen");
        f.mark_seen(2);
        assert!(!f.has_unseen_update());
    }

    #[test]
    fn finds_refs() {
        let r = refs("see [src/a.rs:3-9] and [b.py:7], not [x]");
        assert_eq!(r.len(), 2);
        let text = "at [a.rs:57](a.rs:57) here";
        let linked = refs(text);
        assert_eq!(&text[linked[0].0.clone()], "[a.rs:57](a.rs:57)");
        assert_eq!(r[0].1, Anchor { path: "src/a.rs".into(), start: 3, end: 9 });
        assert_eq!(r[1].1.end, 7);
    }
}
