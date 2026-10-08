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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exchange {
    pub q: String,
    pub a: String,
    #[serde(default)]
    pub notes: Vec<Note>,
    #[serde(default)]
    pub suggested: Vec<String>,
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
/// Marker the guide puts before its suggested follow-up questions.
pub const NEXT_MARKER: &str = "<<next>>";

/// A guide answer split into its parts.
#[derive(Debug, Default, PartialEq)]
pub struct Answer<'a> {
    pub text: &'a str,
    pub notes: Vec<Note>,
    pub suggested: Vec<String>,
}

/// Splits a raw guide answer into text, line notes and suggested follow-ups.
/// Also hides a marker that is only partially streamed so far.
pub fn split_answer(raw: &str) -> Answer<'_> {
    let notes_at = raw.find(NOTES_MARKER);
    let next_at = raw.find(NEXT_MARKER);
    let end = notes_at.into_iter().chain(next_at).min();
    let text = match end {
        Some(i) => raw[..i].trim_end(),
        None => match raw.rfind("
<") {
            Some(i) if [NOTES_MARKER, NEXT_MARKER].iter().any(|m| m.starts_with(raw[i + 1..].trim_end())) => {
                raw[..i].trim_end()
            }
            _ => raw,
        },
    };
    let section = |at: Option<usize>, marker: &str| {
        let Some(i) = at else { return vec![] };
        let body = &raw[i + marker.len()..];
        let stop = [NOTES_MARKER, NEXT_MARKER].iter().filter_map(|m| body.find(m)).min();
        body[..stop.unwrap_or(body.len())]
            .lines()
            .map(|l| l.trim().trim_start_matches(['-', '*', ' ']).trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let notes = section(notes_at, NOTES_MARKER).iter().filter_map(|l| parse_note(l)).collect();
    let suggested = section(next_at, NEXT_MARKER).into_iter().take(3).collect();
    Answer { text, notes, suggested }
}

/// "src/a.rs:12: text" or "[src/a.rs:12] text" → a note.
fn parse_note(line: &str) -> Option<Note> {
    static NOTE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\[?([\w./@+-]+):(\d+)(?:-\d+)?\]?:?\s+(.+)$").unwrap());
    let c = NOTE.captures(line)?;
    Some(Note { path: c[1].to_string(), line: c[2].parse().ok()?, text: c[3].trim().to_string() })
}

static REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([\w./@+-]+):(\d+)(?:-(\d+))?\]").unwrap());

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
    fn finds_refs() {
        let r = refs("see [src/a.rs:3-9] and [b.py:7], not [x]");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].1, Anchor { path: "src/a.rs".into(), start: 3, end: 9 });
        assert_eq!(r[1].1.end, 7);
    }
}
