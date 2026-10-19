use std::fmt::Write as _;

use crate::tour::{Anchor, Stop, Tour};

const PLANNER_FOLDER: &str = "You are the planner for tourer, a tool that gives guided tours of a folder or codebase to someone who has never seen it. The working directory is the folder to tour. Explore it with your read-only tools and write the tour.\n\n";

const PLANNER_DIFF: &str = "You are the planner for tourer, a tool that gives guided tours of code to someone who has not seen it. This tour is of a change: the first message gives the diff between a git commit and the files in the working directory. Explore the folder with your read-only tools to understand the change and the code around it, and write the tour. The files you read are the new versions; the old lines are only in the diff.\n\n";

const PLANNER_FORMAT: &str = r#"Output format (strict): output ONLY JSON objects, one per line. No prose, no code fences, no blank commentary.
First line:
{"type":"tour","title":"<short name>","summary":"<one or two sentences: what this folder is>"}
Then one line per stop, in tour order:
{"type":"stop","title":"<short title, no number>","body":"<markdown>","anchors":[{"path":"<relative/path>","start":<line>,"end":<line>}],"notes":[{"path":"<relative/path>","line":<line>,"text":"<comment>","detail":"<explanation>"}],"suggested":["<question>", "..."]}

After the last stop, a final line:
{"type":"done"}

Write each line as soon as that stop is ready, so the person can start reading while you keep exploring. Write the tour line and the first stop early. Your turn ends as soon as you send a message without a tool call, so while there is more to explore, put the lines you have ready in the same message as your next tool call. End your turn only after the done line.

If you later learn that an earlier stop is wrong or misleading, correct it with an update line. It replaces only the fields it gives; stops are numbered from 1:
{"type":"update","stop":<number>,"body":"<corrected markdown>"}
Any of title, body, anchors, notes and suggested can be given. Only correct real mistakes or gaps; do not rewrite for style.
"#;

const FOLDER_STOPS: &str = r#"
How to build a good tour:
- Stop 1 is an overview: what this is, how it is laid out, how to build, run or use it.
- Then follow the order a newcomer needs: entry points, core concepts and data, the main flows, then supporting pieces. Use 6 to 12 stops, fewer for small folders.
- body: 80 to 200 words of markdown. Explain why things are the way they are, not only what they are. Cite code as [relative/path:start-end].
- anchors: 1 to 3 line ranges (5 to 60 lines each) that you actually read. Line numbers are 1-based, inclusive and must be exact. Cite each anchor in the body, in the sentence that explains it: the anchors are not shown on their own.
- notes: 2 to 6 comments on single lines inside the anchors, shown next to that line in the code view. Point out what a newcomer would miss: a non-obvious call, an invariant, where a value comes from. text: usually under 12 words, at most 40; never restate what the line plainly says. detail: shown when the person hovers the note; 1 to 3 plain sentences (no markdown) that explain it further: why the line is there, what happens at runtime, what would break without it, or where to look next.
- suggested: 2 or 3 questions a newcomer would likely ask next at this stop, answerable from the code.
"#;

const DIFF_STOPS: &str = r#"
How to build a good tour of a change:
- Stop 1 is an overview: what the change does and why, as far as the code shows, and how the changed files group together.
- Then follow the order a reviewer needs: the heart of the change first (new or changed data, types and interfaces), then the code that uses it, then supporting changes such as tests, configuration and docs. Group related hunks from different files into one stop. Mention trivial changes (formatting, renames, imports) together instead of giving them stops. Use 3 to 10 stops, fewer for small changes.
- body: 80 to 200 words of markdown. Say what the code did before and what it does now, why that matters, and what a reviewer should check: changed behavior, edge cases, callers that were not updated, missing tests. Cite code as [relative/path:start-end] in the current version of the file. Removed lines have no current line numbers: describe them or quote them briefly.
- anchors: 1 to 3 line ranges (5 to 60 lines each) of current files that cover the changed lines with a little context. Line numbers are 1-based, inclusive and must be exact. Cite each anchor in the body, in the sentence that explains it: the anchors are not shown on their own. A deleted file has no anchors.
- notes: 2 to 6 comments on single changed lines inside the anchors, shown next to that line in the code view. Point out what is easy to miss: a changed condition, a new invariant, what replaced removed code. text: usually under 12 words, at most 40; never restate what the line plainly says. detail: shown when the person hovers the note; 1 to 3 plain sentences (no markdown) that explain it further: what the line did before, why it changed, or what could break.
- suggested: 2 or 3 questions a reviewer would likely ask next at this stop, answerable from the code.
"#;

/// The planner's system prompt, for a tour of the whole folder or of a change to it.
pub fn planner(diff: bool) -> String {
    match diff {
        false => [PLANNER_FOLDER, PLANNER_FORMAT, FOLDER_STOPS].concat(),
        true => [PLANNER_DIFF, PLANNER_FORMAT, DIFF_STOPS].concat(),
    }
}

/// The planner's first message, with the person's instructions for the tour if they gave any.
/// A diff tour also gets `diff`: which changes it tours, and the changed files with the diff.
pub fn planner_start(instructions: &str, diff: Option<(&str, &str)>) -> String {
    let mut s = match diff {
        Some((about, _)) => format!("Plan the tour of {about}."),
        None => String::from("Plan the tour of this folder."),
    };
    if !instructions.trim().is_empty() {
        let _ = write!(s, " The person gave these instructions for the tour; follow them:\n\n{}", instructions.trim());
    }
    if let Some((_, changes)) = diff {
        let _ = write!(s, "\n\n{changes}");
    }
    s
}

/// Sent when the planner ends its turn before writing the done line.
pub fn planner_continue(stops: usize) -> String {
    format!("Your turn ended after {stops} stops without the done line. Continue the tour from stop {}, or write the done line if it is complete.", stops + 1)
}

const GUIDE: &str = r#"You are the guide in tourer, a tool that gives guided tours of a folder or codebase to someone who has never seen it. The working directory is that folder. A separate planner wrote the tour's stops; you answer the questions asked along the way.

Messages start with a bracketed context block: in your first message, the person's instructions for the tour if they gave any; stops that are new or changed since your last message (all of them in your first message), then which stop the person is at and the question's number there, and sometimes code they selected. Questions may be about that stop or about anything else in the folder.

- Ground every answer in the actual files: read and search instead of guessing. If something is not in the folder, say so.
- Be concise: usually under 200 words of markdown. Lead with the direct answer.
- Cite code as [relative/path:start-end] with exact 1-based line numbers; these become links the person can open.
- When the answer points at specific lines, you may add comments shown next to those lines in the code view: after the answer, a line containing exactly <<notes>>, then one JSON object per line: {"path":"<relative/path>","line":<line>,"text":"<comment, under 12 words where possible>","detail":"<1 to 3 plain sentences shown when the note is hovered>"}.
- Whenever you find that a stop or one of your earlier answers is wrong, misleading or missing something important, correct it: after the answer, a line containing exactly <<update>>, then one JSON object per line:
  {"type":"update","stop":<number>,"body":"<corrected markdown>"} replaces the fields it gives of a stop: title, body, anchors, notes or suggested, in the same shapes the stops use.
  {"type":"update_answer","stop":<number>,"question":<number>,"text":"<corrected answer>"} replaces an earlier answer, optionally with "notes".
  Give the whole corrected text, not a diff. Only correct real mistakes or gaps.
- End every answer with a line containing exactly <<next>> followed by 2 or 3 short follow-up questions they would likely want to ask next, one per line.
"#;

const GUIDE_DIFF: &str = "
This tour is of a change: the diff between a git commit and the files in the working directory, given in your first message. The files you read are the new versions; old lines exist only in the diff, so cite current lines and quote removed ones. Answer like a careful reviewer: when asked whether a change is right, safe or complete, check its callers, its tests and the code it replaced.
";

/// The guide's system prompt, for a tour of the whole folder or of a change to it.
pub fn guide(diff: bool) -> String {
    if diff { [GUIDE, GUIDE_DIFF].concat() } else { GUIDE.to_string() }
}

/// A question for the guide: question `e` at stop `s` (both 0-based). A new conversation
/// (`fresh`) is told the person's instructions for the tour; `diff` is the change a diff tour
/// is about (which commits it compares, and the diff), given only to a new conversation;
/// `changed` are the stops the guide has not seen in their current form; `code` is the selected code with its lines.
pub fn question(
    tour: &Tour,
    (s, e): (usize, usize),
    fresh: bool,
    diff: Option<(&str, &str)>,
    changed: &[(usize, &Stop)],
    code: Option<(&Anchor, &str)>,
) -> String {
    let stop = &tour.stops[s];
    let mut m = String::from("[");
    let instructions = tour.instructions.trim();
    if fresh && !instructions.is_empty() {
        let _ = writeln!(m, "The person gave these instructions for the tour; keep to them:\n{instructions}\n");
    }
    if let Some((about, changes)) = diff {
        let _ = writeln!(m, "This tour is of {about}.\n{changes}");
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
        assert_eq!(planner_start("  ", None), "Plan the tour of this folder.");
        assert!(planner_start("focus on the parser", None).ends_with("\n\nfocus on the parser"));
        let tour = r#"{"version":1,"root":"","title":"T","summary":"S","instructions":"answer in Turkish",
            "stops":[{"title":"A","body":"b","thread":[{"q":"why?","a":""}]}]}"#;
        let tour: Tour = serde_json::from_str(tour).unwrap();
        assert!(question(&tour, (0, 0), true, None, &[], None).contains("answer in Turkish"));
        assert!(!question(&tour, (0, 0), false, None, &[], None).contains("answer in Turkish"));
    }
}
