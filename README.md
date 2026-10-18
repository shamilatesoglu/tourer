# tourer

Guided tours of folders and codebases you have never seen before, in your terminal.

![A tour of tourer itself](docs/demo.gif)

Point `tourer` at a folder. An AI guide reads it and plans a tour as a series of stops,
each one explaining part of the code with links to the exact lines. You read the stops next
to the code and can ask about anything at any stop, even while the rest of the tour is still
being planned.

Asking a coding agent to "explain this repo" gives you one long answer. A tour is different:

- **It has an order.** The guide plans where to start and what comes next, the way a
  colleague would walk you through it.
- **The code is always next to the text.** Every `[path:lines]` the guide cites is a link,
  and the code pane follows whatever you are reading. The guide also leaves short notes on
  single lines of code.
- **Questions belong to a stop.** Each stop keeps its own questions and answers. You can
  ask about lines you select, and every stop and answer suggests 2–3 follow-up questions.
- **It corrects itself.** When the guide learns something that changes an earlier stop or
  answer, it updates it, and marks the parts you have not seen yet with ✦.
- **It is saved.** Tours and the guide's conversation survive restarts, so you can come back
  tomorrow and keep asking.

## Install

```sh
cargo install tourer
```

tourer does not call any model itself. It runs a coding agent's CLI that you already use,
read-only, in the folder you are touring, so you need one of these on your `PATH` and
logged in:

- [Claude Code](https://docs.claude.com/en/docs/claude-code) (`claude`)
- [Codex](https://github.com/openai/codex) (`codex`)

The tour runs on your agent subscription or API key like any other session, and your code is
sent to that agent's provider just as it would be if you used the agent directly.

## Use

```sh
tourer path/to/folder
```

The start screen lets you continue the folder's saved tour or plan a new one, and pick the
agent and model. Your choice is remembered.

| Key | |
| --- | --- |
| `↑` / `↓`, `Space` | step through the tour: each code reference, question and suggestion |
| `←` / `→` | previous / next stop |
| `Enter` | ask the selected suggestion, or type your own question |
| `1`–`4` | ask a suggested question |
| `Shift`+`↑`/`↓` | select code lines, then `Enter` to ask about them |
| `Tab` | switch between the tour and the code |
| `o` / `f` | list of stops / open any file |
| `z` | zoom |
| `q` | quit |

The mouse works too: click links, suggestions and progress marks, select code by dragging,
and hover a noted line to read its note. The bottom bar always shows the keys that work
right now.

Options:

```
tourer [FOLDER] [--agent claude|codex] [--model NAME] [--replan] [--out tour.json]
```

`--out` writes the whole tour, with questions and answers, as JSON when you quit.

## How it works

A small background daemon owns the tours. The first `tourer` that needs it starts it, and
it exits after 10 idle minutes. Front-ends talk to it over localhost TCP with a random token.
It saves tours in `$XDG_STATE_HOME/tourer` (`~/.local/state/tourer`) or
`%LOCALAPPDATA%\tourer` on Windows, with a `daemon.log` that shows each agent turn and its
token use.

Each tour has two agent sessions: a planner that writes the stops, and a guide that answers
questions. Planning never blocks your questions.

## License

MIT or Apache-2.0, at your option.
