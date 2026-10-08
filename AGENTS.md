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
  - Stops and answers come with 2–3 suggested follow-up questions you can pick with a key.
- Agent backends:
  - A: the `claude` CLI in headless stream-json mode (now)
  - B: direct Anthropic API calls with our own read-only tools (later)
  - The app only depends on `agent::Event`, so B replaces only the code that produces events.
- The planner and the guide are two separate agent sessions, so planning never blocks questions.
- Simplicity first: no feature, crate or abstraction until it is needed. One crate for now.
- Tour format: our own small JSON format (see `src/tour.rs`). CodeTour `.tour` export can be added later.
- `engine.rs` owns planning and Q&A. The TUI (`app.rs`, `ui.rs`) and the MCP server (`mcp.rs`) are
  both thin front-ends that feed it agent events.
- Formatting: a small line-based markdown renderer of our own (`markdown.rs`), so we control wrapping
  and link order. Syntax highlighting uses syntect with bat's "ansi" theme (via two-face), so colors
  come from the terminal palette and work on light and dark themes.
- Agents use MCP over stdio. We implement the few JSON-RPC methods we need by hand instead of
  pulling in an async MCP SDK. Tools: `tour` (outline), `stop` (text plus code excerpts) and `ask`.

## Running
- `cargo run -- <folder> [--model sonnet] [--out tour.json]` (needs `claude` on PATH and logged in).
- Keys: `n`/`p` change stop, `j`/`k` scroll text, `Tab`/`Shift-Tab` cycle code links, `J`/`K` scroll code,
  `Esc` back to the stop's code, `?` ask, `1`–`3` ask a suggested question, `q` quit.
- MCP: `tourer --mcp [default-folder]`, e.g. `claude mcp add tourer -- /path/to/tourer --mcp`.
  Tool calls take an optional `path`, and each folder's tour is planned once and shared.
- `cargo test` covers markdown rendering and parsing of planner lines, answers and code references.
