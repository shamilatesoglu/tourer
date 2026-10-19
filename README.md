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

The start screen lets you pick what to tour, continue its saved tour or plan a new one, and
pick the agent and model. The agent and model are remembered.

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
and hover a note on a line of code for a longer explanation. The bottom bar always shows the keys that work
right now.

Options:

```
tourer [FOLDER] [--diff [REV]] [--agent claude|codex] [--model NAME] [--replan] [--out tour.json]
```

`--out` writes the whole tour, with questions and answers, as JSON when you quit.

### Tour a change

A tour can cover a change instead of the whole folder, the way a colleague would walk you
through a pull request: what it does, the heart of it first, then what uses it, and what a
reviewer should check. In a git repository, the start screen's Tour row offers the
uncommitted changes, the branch's own changes, or the changes since a revision you type
(or between two, as `OLD..NEW`); use `←` / `→` to pick. `--diff` picks one from the command line:

```sh
tourer --diff main       # this branch's changes since it left main
tourer --diff HEAD~3     # the last three commits
tourer --diff            # the uncommitted changes
tourer --diff main..pr   # the changes on branch pr since it left main, without checking it out
```

The changes run from where the revision meets HEAD to the files on disk, so uncommitted and
untracked files are included. In the code pane, `+` marks added lines, `-` marks lines with
removed code just above them, and `±` both; hover one (or move the cursor to it) to see the
removed lines. The file picker lists changed files first, and edits you make during the tour
show up in the code pane. Each revision gets its own saved tour; the start screen tells you
when the revision has moved since the tour was planned, e.g. after a rebase.

`OLD..NEW` tours the changes from where OLD meets NEW to NEW, leaving your own work out. The
agents cannot run git, so tourer writes NEW's files to a folder of its own (next to the saved
tours) for them to read; your repository's files and index are not touched. That folder is
deleted when no saved tour needs it any more.

## How it works

A small background daemon owns the tours. The first `tourer` that needs it starts it, and
it exits after 10 idle minutes. Front-ends talk to it over localhost TCP with a random token.
It saves tours in `$XDG_STATE_HOME/tourer` (`~/.local/state/tourer`) or
`%LOCALAPPDATA%\tourer` on Windows, with a `daemon.log` that shows each agent turn and its
token use.

Each tour has two agent sessions: a planner that writes the stops, and a guide that answers
questions. Planning never blocks your questions.

## License

MIT. See [LICENSE](LICENSE).
