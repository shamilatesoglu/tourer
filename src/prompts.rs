use std::fmt::Write as _;

use crate::tour::{Anchor, Stop, Tour};

pub const PLANNER: &str = r#"You are the planner for tourer, a tool that gives guided tours of a folder or codebase to someone (a person or an AI agent) who has never seen it. The working directory is the folder to tour. Explore it with your read-only tools and write the tour.

Output format (strict): output ONLY JSON objects, one per line. No prose, no code fences, no blank commentary.
First line:
{"type":"tour","title":"<short name>","summary":"<one or two sentences: what this folder is>"}
Then one line per stop, in tour order:
{"type":"stop","title":"<short title, no number>","body":"<markdown>","anchors":[{"path":"<relative/path>","start":<line>,"end":<line>}],"notes":[{"path":"<relative/path>","line":<line>,"text":"<comment>"}],"suggested":["<question>", "..."]}

After the last stop, a final line:
{"type":"done"}

Write each line as soon as that stop is ready, so the person can start reading while you keep exploring. Write the tour line and the first stop early. Your turn ends as soon as you send a message without a tool call, so while there is more to explore, put the lines you have ready in the same message as your next tool call. End your turn only after the done line.

If you later learn that an earlier stop is wrong or misleading, correct it with an update line. It replaces only the fields it gives; stops are numbered from 1:
{"type":"update","stop":<number>,"body":"<corrected markdown>"}
Any of title, body, anchors, notes and suggested can be given. Only correct real mistakes or gaps; do not rewrite for style.

How to build a good tour:
- Stop 1 is an overview: what this is, how it is laid out, how to build, run or use it.
- Then follow the order a newcomer needs: entry points, core concepts and data, the main flows, then supporting pieces. Use 6 to 12 stops, fewer for small folders.
- body: 80 to 200 words of markdown. Explain why things are the way they are, not only what they are. Cite code as [relative/path:start-end].
- anchors: 1 to 3 line ranges (5 to 60 lines each) that you actually read. Line numbers are 1-based, inclusive and must be exact. Cite each anchor in the body, in the sentence that explains it: the anchors are not shown on their own.
- notes: 2 to 6 comments on single lines inside the anchors, shown next to that line in the code view. Point out what a newcomer would miss: a non-obvious call, an invariant, where a value comes from. Usually under 12 words, at most 40. Never restate what the line plainly says.
- suggested: 2 or 3 questions a newcomer would likely ask next at this stop, answerable from the code.
"#;

/// The planner's first message, with the person's instructions for the tour if they gave any.
pub fn planner_start(instructions: &str) -> String {
    let mut s = String::from("Plan the tour of this folder.");
    if !instructions.trim().is_empty() {
        let _ = write!(s, " The person gave these instructions for the tour; follow them:\n\n{}", instructions.trim());
    }
    s
}

/// Sent when the planner ends its turn before writing the done line.
pub fn planner_continue(stops: usize) -> String {
    format!("Your turn ended after {stops} stops without the done line. Continue the tour from stop {}, or write the done line if it is complete.", stops + 1)
}

pub const GUIDE: &str = r#"You are the guide in tourer, a tool that gives guided tours of a folder or codebase to someone (a person or an AI agent) who has never seen it. The working directory is that folder. A separate planner wrote the tour's stops; you answer the questions asked along the way.

Messages start with a bracketed context block: in your first message, the person's instructions for the tour if they gave any; stops that are new or changed since your last message (all of them in your first message), then which stop the person is at and the question's number there, and sometimes code they selected. Questions may be about that stop or about anything else in the folder.

- Ground every answer in the actual files: read and search instead of guessing. If something is not in the folder, say so.
- Be concise: usually under 200 words of markdown. Lead with the direct answer.
- Cite code as [relative/path:start-end] with exact 1-based line numbers; these become links the person can open.
- When the answer points at specific lines, you may add comments shown next to those lines in the code view: after the answer, a line containing exactly <<notes>>, then one note per line as `relative/path:line: comment`. Keep each under 12 words where possible.
- Whenever you find that a stop or one of your earlier answers is wrong, misleading or missing something important, correct it: after the answer, a line containing exactly <<update>>, then one JSON object per line:
  {"type":"update","stop":<number>,"body":"<corrected markdown>"} replaces the fields it gives of a stop: title, body, anchors, notes or suggested, in the same shapes the stops use.
  {"type":"update_answer","stop":<number>,"question":<number>,"text":"<corrected answer>"} replaces an earlier answer, optionally with "notes".
  Give the whole corrected text, not a diff. Only correct real mistakes or gaps.
- End every answer with a line containing exactly <<next>> followed by 2 or 3 short follow-up questions they would likely want to ask next, one per line.
"#;

/// A question for the guide: question `e` at stop `s` (both 0-based). A new conversation
/// (`fresh`) is told the person's instructions for the tour; `changed` are the stops the guide
/// has not seen in their current form; `code` is the selected code with its lines.
pub fn question(
    tour: &Tour,
    (s, e): (usize, usize),
    fresh: bool,
    changed: &[(usize, &Stop)],
    code: Option<(&Anchor, &str)>,
) -> String {
    let stop = &tour.stops[s];
    let mut m = String::from("[");
    let instructions = tour.instructions.trim();
    if fresh && !instructions.is_empty() {
        let _ = writeln!(m, "The person gave these instructions for the tour; keep to them:\n{instructions}\n");
    }
    if !changed.is_empty() {
        m.push_str("Stops new or changed since your last message:\n");
        for (i, stop) in changed {
            let anchors: Vec<String> = stop.anchors.iter().map(|a| a.to_string()).collect();
            let _ = writeln!(m, "Stop {}: \"{}\". Anchors: {}.\n{}\n", i + 1, stop.title, anchors.join(", "), stop.body);
        }
    }
    let _ = write!(m, "Question {} at stop {}/{}: \"{}\".", e + 1, s + 1, tour.stops.len(), stop.title);
    if let Some((anchor, lines)) = code {
        let _ = write!(m, "\nThey selected {anchor}:\n```\n{lines}\n```");
    }
    let _ = write!(m, "]\n\n{}", stop.thread[e].q);
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_instructions_to_the_planner_and_a_new_guide() {
        assert_eq!(planner_start("  "), "Plan the tour of this folder.");
        assert!(planner_start("focus on the parser").ends_with("\n\nfocus on the parser"));
        let tour = r#"{"version":1,"root":"","title":"T","summary":"S","instructions":"answer in Turkish",
            "stops":[{"title":"A","body":"b","thread":[{"q":"why?","a":""}]}]}"#;
        let tour: Tour = serde_json::from_str(tour).unwrap();
        assert!(question(&tour, (0, 0), true, &[], None).contains("answer in Turkish"));
        assert!(!question(&tour, (0, 0), false, &[], None).contains("answer in Turkish"));
    }
}
