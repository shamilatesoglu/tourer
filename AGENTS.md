# tourer

A Rust TUI that gives guided tours of unfamiliar folders and codebases. An AI agent runs
in the background, plans the tour as a series of stops, and answers questions at every stop.

## Decisions
- Audience: humans (TUI) and other AI agents (via a protocol), both reading folders or
  codebases they have never seen before.
- One-conversation model: a tour is a conversation with a guide. Stops are messages the
  guide planned in advance, and questions are messages in that same conversation.
  - You can ask at any stop, anytime, including while stops are still being generated.
  - Each stop keeps its own question-and-answer thread.
  - Every question is sent with the current stop as context.
  - Answers cite code as `[path:start-end]`, and the TUI turns these into links you can jump to.
  - Questions can be about selected lines of code; the lines go along with the question.
  - Agents correct earlier stops and answers whenever they learn better: the planner with
    `update` lines, the guide in an `<<update>>` section. Nothing is marked as revised; the
    TUI only shows ✦ on updates the person has not seen yet (seen = on screen for 1.5 s),
    and MCP outlines say which stops changed since that client read them.
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
- The planner ends the tour with a `done` line. An agent's turn ends whenever it replies without
  a tool call, so a planner can stop early; if its turn ends without the done line, it is asked to
  continue, up to 3 times. The daemon log says why each turn ended.
- Simplicity first: no feature, crate or abstraction until it is needed. One crate for now.
- Tour format: our own small JSON format (see `src/tour.rs`). CodeTour `.tour` export can be added later.
- `engine.rs` owns planning and Q&A. A background daemon (`daemon.rs`) hosts one engine per folder,
  so the TUI (`app.rs`, `ui.rs`) and MCP servers (`mcp.rs`) share tours and see each other's
  questions live. The first front-end that needs the daemon starts it; it runs from a copy of the
  exe (so rebuilds are not blocked), saves tours to `%LOCALAPPDATA%\tourer` (or
  `$XDG_STATE_HOME/tourer`), continues saved tours, and exits after 10 idle minutes. Front-ends
  talk to it over localhost TCP with a token, one JSON message per line. A front-end built
  differently from the running daemon starts a new one.
- TUI: a start screen (continue the tour, plan a new one, guide, model), then the tour: one
  scrolling document of all stops beside the code pane, which follows the first code reference
  on screen. Scrolling glides, stops have a progress line on top, questions are typed inline,
  and popups show notes, the list of stops and a file picker. Under 100 columns the code goes
  inline under each stop and opens in a popup. Keys are few and arrow-based, and the bottom bar
  shows the ones that work right now. `view.rs` holds the tour screen's state and input,
  `ui.rs` draws it.
- Line notes: the planner gives each stop notes on single lines, and the guide can add some after
  `<<notes>>`. The code pane shows a note after its line, cut to fit, and the whole note in a
  popup for the line under the mouse or the cursor.
- Formatting: a small line-based markdown renderer of our own (`markdown.rs`), so we control wrapping
  and link order. Syntax highlighting uses syntect with bat's "ansi" theme (via two-face), so colors
  come from the terminal palette and work on light and dark themes.
- Agents use MCP over stdio. We implement the few JSON-RPC methods we need by hand instead of
  pulling in an async MCP SDK. Tools: `tour` (outline), `stop` (text plus code excerpts) and `ask`.

## Running
- `cargo run -- <folder> [--agent claude|codex] [--model NAME] [--out tour.json] [--replan]` (needs
  the agent's CLI on PATH and logged in). `--agent`, `--model` and `--replan` preselect the start
  screen; the agent and model picked there are saved in `prefs.json` next to the tours.
- Keys: `←`/`→` change stop, `↑`/`↓`/`PgUp`/`PgDn`/`Home`/`End` scroll the text or move the code
  cursor, `Shift`+`↑`/`↓` select code lines, `Tab` switch pane, `Enter` (in the text) show the
  next code reference, `?` ask (about the selected lines in the code pane), `1`–`3` ask a
  suggested question, `o` list of stops, `f` open any file, `z` zoom, `Esc` back, `q` quit.
- Mouse: click a reference, suggestion or progress mark; click or drag in the code to select
  lines; hover a noted line for its note; drag the border to resize; the wheel scrolls.
- Daemon log: `daemon.log` next to the saved tours.
- MCP: `tourer --mcp [--agent codex] [default-folder]` (default agent: the last one picked), e.g. `claude mcp add tourer -- /path/to/tourer --mcp`.
  Tool calls take an optional `path`, and each folder's tour is planned once and shared.
- `cargo test` covers markdown rendering (tables, link positions), line notes in the code pane,
  smooth scrolling, decoding codex events, unseen-update tracking, and parsing of planner
  lines, updates, answers and code references.
