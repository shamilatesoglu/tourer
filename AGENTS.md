# tourer

A Rust TUI that gives guided tours of unfamiliar folders and codebases. An AI agent runs
in the background, plans the tour as a series of stops, and answers questions at every stop.

## Decisions
- Audience: people reading folders or codebases they have never seen before, in the TUI.
- No MCP server (removed in October 2026). Coding agents can already explore a codebase and
  start their own subagents, so a tour adds little for them beyond cost and latency. Bring it
  back only for a real need, such as a person and an agent sharing one tour.
- One-conversation model: a tour is a conversation with a guide. Stops are messages the
  guide planned in advance, and questions are messages in that same conversation.
  - You can ask at any stop, anytime, including while stops are still being generated.
  - Each stop keeps its own question-and-answer thread.
  - Every question is sent with the current stop as context.
  - Answers cite code as `[path:start-end]`, and the TUI turns these into links you can jump to.
  - A stop's anchors are not listed under its title: the body cites them where it explains
    them, and any it leaves out follow the text as "See also". With nothing cited on screen,
    the code pane shows the current stop's first anchor.
  - Questions can be about selected lines of code; the lines go along with the question.
  - Agents correct earlier stops and answers whenever they learn better: the planner with
    `update` lines, the guide in an `<<update>>` section. Nothing is marked as revised; the
    TUI only shows ✦ on updates the person has not seen yet (seen = on screen for 1.5 s).
  - Stops and answers come with 2–3 suggested follow-up questions you can pick with a key.
- Agents: the planner and guide run on a coding agent's CLI, read-only, in the toured folder. Picking
  one is a background setting, not the point of the app: the start screen remembers the last choice.
  - claude: `claude -p` in stream-json mode, one process per conversation (`agent/claude.rs`).
  - codex: `codex app-server` over JSON-RPC, one thread per conversation, read-only sandbox. The
    user's MCP servers and tool-adding features are switched off per session (`agent/codex.rs`).
  - Gemini is left out for now: Gemini CLI no longer serves personal Google accounts.
  - Programs are found on PATH the way a shell finds them, including npm's `.cmd` scripts.
  - The app only depends on `agent::Event`, so a new agent only adds the code that produces events.
  - Each folder has one tour per agent.
  - The guide's conversation is kept by the agent (claude `--resume`, codex `thread/resume`)
    and its id saved with the tour, so it survives restarts. Each question tells the guide
    only the stops it has not seen in their current form. If a saved conversation cannot be
    continued, a new one starts and gets the whole tour. Token use and cache hits per turn go
    to the daemon log.
- The planner and the guide are two separate agent sessions, so planning never blocks questions.
- A new tour can be given instructions (what to focus on, how to explain). They are saved with
  the tour, sent to the planner, and given to the guide at the start of each conversation.
- The planner ends the tour with a `done` line. An agent's turn ends whenever it replies without
  a tool call, so a planner can stop early; if its turn ends without the done line, it is asked to
  continue, up to 3 times. The daemon log says why each turn ended and shows any planner line
  that could not be read. Several JSON objects on one line are all read.
- Simplicity first: no feature, crate or abstraction until it is needed. One crate for now.
- Diff tours: `--diff [REV]` tours the changes since a git revision instead of the whole folder:
  from where REV meets HEAD (so `main` means a branch's own changes) to the files on disk, so
  uncommitted and untracked files count and `--diff` alone means the uncommitted changes.
  `--diff OLD..NEW` (or `OLD...NEW`) tours from where OLD meets NEW to NEW. The new side is always
  a folder of real files, so anchors, the code pane and the agents' reads work as in any tour: the
  folder as it is, or for `OLD..NEW` a checkout of NEW in the data folder's `checkouts/<commit>`,
  written with `git read-tree` and `git checkout-index` into a separate index so the repository's
  own index and work tree are not touched. The engine puts that folder in the state it sends, the
  agents work in it, and the TUI reads files from it. A new daemon deletes the checkouts no saved
  tour ends at. The agents cannot run git, so `diff.rs` runs it and the diff goes in the planner's
  first message and in the guide's first message (not the system prompt, which is a command-line
  argument for claude). The prompts swap the newcomer's order for a reviewer's. A tour saves the
  commits it was planned from, and each revision has its own tour per agent; the start screen
  says when the revisions name other commits now. The code pane marks added
  lines `+`, lines with removed ones just above `-`, and both `±`; the removed lines show in the
  note popup, so every screen row stays one line of the file. Changes are read from git again
  whenever they are used, and by the TUI when a file it shows changes on disk.
  - In a git repository the start screen's Tour row picks the folder, the uncommitted changes,
    the branch's own changes since the main branch (`origin/HEAD`, else `main` or `master`), or
    the changes since a revision typed in (or between two, typed as `OLD..NEW`). `--diff`
    preselects it; it is not saved in prefs.
- Tour format: our own small JSON format (see `src/tour.rs`). CodeTour `.tour` export can be added later.
- `engine.rs` owns planning and Q&A. A background daemon (`daemon.rs`) hosts one engine per folder,
  so every open TUI (`app.rs`, `ui.rs`) shares the tours and sees the others' questions live. The first front-end that needs the daemon starts it; it runs from a copy of the
  exe (so rebuilds are not blocked), saves tours to `%LOCALAPPDATA%\tourer` (or
  `$XDG_STATE_HOME/tourer`), continues saved tours, and exits after 10 idle minutes. Front-ends
  talk to it over localhost TCP with a token, one JSON message per line. A front-end built
  differently from the running daemon starts a new one.
- TUI: a start screen (what to tour, continue the tour, plan a new one, instructions for it, guide, model), then the tour: one
  scrolling document of all stops beside the code pane, which follows the first code reference
  on screen. Scrolling glides, stops have a progress line on top, questions are typed inline,
  and popups show notes, the list of stops and a file picker. Under 100 columns the code goes
  inline under each stop and opens in a popup. Keys are few and arrow-based, and the bottom bar
  shows the ones that work right now. ↑/↓ walk the tour step by step rather than scroll: each
  code reference, each question, each suggested question, then an "Ask a different question"
  row; entering a stop shows its top
  first. Scrolling by hand drops the step, and the next ↓ starts from the screen. `view.rs` holds the tour screen's state and input,
  `ui.rs` draws it.
- Line notes: the planner gives each stop notes on single lines, and the guide can add some after
  `<<notes>>` (one JSON object per line; `path:line: text` is also read). A note has a short
  text, shown after its line and cut to fit, and a detail of 1–3 sentences. Hovering the line,
  or putting the cursor on it, shows the detail in a popup, with the text too only if it was
  cut; a note that fits and has no detail gets no popup.
- Formatting: a small line-based markdown renderer of our own (`markdown.rs`), so we control wrapping
  and link order. Syntax highlighting uses syntect with bat's "ansi" theme (via two-face), so colors
  come from the terminal palette and work on light and dark themes.

## Later
- History tours: besides a tour of the folder as it is, a tour of how it got there. Git commits
  are only part of it; much of a project's reasoning now lives in agent conversations (Claude
  Code and Codex sessions: the prompts, the options weighed, the decisions made). Show that
  development and its decisions in a way that is good to look at, not as a log. Diff tours
  (`--diff`) cover touring a change, but not its history.

## Running
- `cargo run -- <folder> [--diff [REV | OLD..NEW]] [--agent claude|codex] [--model NAME] [--out tour.json] [--replan]` (needs
  the agent's CLI on PATH and logged in). `--diff`, `--agent`, `--model` and `--replan` preselect the start
  screen; the agent and model picked there are saved in `prefs.json` next to the tours.
- Keys: `↑`/`↓` (or `Space`) step through the text, or move the code cursor; `←`/`→` change
  stop; `PgUp`/`PgDn`/`Home`/`End` scroll; `Shift`+`↑`/`↓` select code lines; `Tab` switch pane;
  `Enter` ask the selected suggestion, or ask your own question (about the selected lines in
  the code pane); `1`–`4` pick a suggestion row (`?` also asks); `o` list of stops; `f` open
  any file; `z` zoom; `Esc` back; `q` quit.
- Mouse: click a reference, suggestion, the ask row or a progress mark; click or drag in the code to select
  lines; hover a noted line for the note's details; drag the border to resize; the wheel scrolls.
- Daemon log: `daemon.log` next to the saved tours.
- Demo: `vhs demo/tourer.tape` records the README's `docs/demo.gif`; the tape says how to prepare it.
- `cargo test` covers markdown rendering (tables, link positions), line notes in the code pane,
  smooth scrolling, the order ↓ steps through, reading git diffs and marking changed lines, what
  the start screen offers to tour, whether a diff tour's commits moved, and touring and checking
  out the changes between two revisions (against a throwaway git repository), tour file names, decoding codex events, unseen-update tracking, and parsing of planner
  lines, updates, answers and code references.
