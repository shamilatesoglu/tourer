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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exchange {
    pub q: String,
    pub a: String,
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

/// Marker the guide puts before its suggested follow-up questions.
pub const NEXT_MARKER: &str = "<<next>>";

/// Splits a guide answer into the visible answer and its suggested follow-ups.
/// Also hides a marker that is only partially streamed so far.
pub fn split_answer(raw: &str) -> (&str, Vec<String>) {
    if let Some(i) = raw.find(NEXT_MARKER) {
        let suggested = raw[i + NEXT_MARKER.len()..]
            .lines()
            .map(|l| l.trim().trim_start_matches(['-', '*', ' ']).trim())
            .filter(|l| !l.is_empty())
            .take(3)
            .map(String::from)
            .collect();
        return (raw[..i].trim_end(), suggested);
    }
    match raw.rfind("\n<") {
        Some(i) if NEXT_MARKER.starts_with(raw[i + 1..].trim_end()) => (raw[..i].trim_end(), vec![]),
        _ => (raw, vec![]),
    }
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
        let (a, s) = split_answer("It works.\n<<next>>\n- Why?\nHow?\n");
        assert_eq!(a, "It works.");
        assert_eq!(s, vec!["Why?", "How?"]);
        assert_eq!(split_answer("Partial\n<<ne").0, "Partial");
        assert_eq!(split_answer("a < b\n<div>").0, "a < b\n<div>");
    }

    #[test]
    fn finds_refs() {
        let r = refs("see [src/a.rs:3-9] and [b.py:7], not [x]");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].1, Anchor { path: "src/a.rs".into(), start: 3, end: 9 });
        assert_eq!(r[1].1.end, 7);
    }
}
