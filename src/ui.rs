use std::path::Path;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};
use textwrap::core::display_width;

use crate::agent::Agent;
use crate::app::{App, Areas, Pane, Row, Screen, Start, TourView};
use crate::engine::AnswerState;
use crate::highlight::StyledLine;
use crate::markdown::Renderer;
use crate::tour::{self, Anchor, Note};

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let root = shown_path(&app.root);
    match &mut app.screen {
        Screen::Start(start) => draw_start(f, start, &root, main),
        Screen::Tour(view) => {
            view.areas = layout(view, main);
            let Areas { stops, code, text, .. } = view.areas;
            draw_stops(f, view, stops);
            // The narrative decides the links, and the code pane may show one of them. Hidden
            // panes have empty areas but are still laid out, so the links stay current.
            draw_text(f, view, text);
            draw_code(f, view, code);
        }
    }
    draw_status(f, app, status);
}

/// A path as people write it, without the `\\?\` prefix Windows adds to canonical paths.
fn shown_path(path: &Path) -> String {
    let s = path.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

/// "1 stop", "2 stops".
fn count(n: usize, thing: &str) -> String {
    if n == 1 { format!("1 {thing}") } else { format!("{n} {thing}s") }
}

/// The end of `s` if it is wider than `width`, after an ellipsis.
fn left_cut(s: &str, width: usize) -> String {
    if display_width(s) <= width {
        return s.to_string();
    }
    let tail: Vec<char> = s.chars().rev().take(width.saturating_sub(1)).collect();
    format!("…{}", tail.into_iter().rev().collect::<String>())
}

/// The tour to continue or start, and the agent that guides it.
fn draw_start(f: &mut Frame, start: &mut Start, root: &str, area: Rect) {
    let width = area.width.min(76);
    let x = area.x + (area.width - width) / 2;
    let y = area.y + area.height.saturating_sub(10) / 3;
    let inner = Rect::new(x, y, width, area.bottom() - y);
    let row = start.row();
    let label = |r: Row, text: &str| {
        let span = Span::from(format!("{}{text:<15}", if r == row { "▶ " } else { "  " }));
        if r == row { span.bold().cyan() } else { span }
    };

    let mut lines: Vec<(Option<Row>, Line)> = vec![
        (None, Line::from("tourer").bold()),
        (None, Line::from(left_cut(root, width as usize)).dim()),
        (None, Line::from("")),
    ];
    match (&start.tours, start.existing()) {
        (None, _) => lines.push((None, Line::from("  Looking for saved tours…").dim())),
        (Some(_), Some(tour)) => {
            let title = if tour.title.is_empty() { "untitled" } else { &tour.title };
            let progress = if tour.planning {
                format!("being planned, {} so far", count(tour.stops, "stop"))
            } else {
                format!("{} · {}", count(tour.stops, "stop"), count(tour.questions, "question"))
            };
            let about = vec![label(Row::Continue, "Continue tour"), Span::from(title.to_string()).bold()];
            lines.push((Some(Row::Continue), Line::from(about)));
            lines.push((None, Line::from(format!("{:17}{progress}", "")).dim()));
            let new = vec![label(Row::New, "New tour"), Span::from("plan it again, replacing this one").dim()];
            lines.push((Some(Row::New), Line::from(new)));
        }
        (Some(_), None) => {
            let new = vec![label(Row::New, "Start tour"), Span::from("plan a tour of this folder").dim()];
            lines.push((Some(Row::New), Line::from(new)));
        }
    }
    lines.push((None, Line::from("")));
    let mut agents = vec![label(Row::Agent, "Guide")];
    for agent in Agent::ALL {
        agents.push(if agent == start.agent {
            Span::from(agent.name()).reversed()
        } else if start.installed.contains(&agent) {
            Span::from(agent.name()).dim()
        } else {
            Span::from(format!("{agent} (not installed)")).dim().crossed_out()
        });
        agents.push(Span::from("   "));
    }
    lines.push((Some(Row::Agent), Line::from(agents)));
    let model = match (start.model.is_empty(), row == Row::Model) {
        (true, false) => Span::from("default").dim(),
        (true, true) => Span::from("▏default").dim(),
        (false, true) => Span::from(format!("{}▏", start.model)),
        (false, false) => Span::from(start.model.clone()),
    };
    lines.push((Some(Row::Model), Line::from(vec![label(Row::Model, "Model"), model])));

    start.row_spots = lines
        .iter()
        .enumerate()
        .filter(|&(i, _)| (i as u16) < inner.height)
        .filter_map(|(i, (row, _))| Some((Rect::new(inner.x, inner.y + i as u16, inner.width, 1), (*row)?)))
        .collect();
    let lines: Vec<Line> = lines.into_iter().map(|(_, line)| line).collect();
    f.render_widget(Paragraph::new(lines), inner);
}

/// The stop list on the left, code above the narrative on the right; or one zoomed pane.
fn layout(view: &TourView, main: Rect) -> Areas {
    let mut areas = Areas { main, ..Default::default() };
    if view.zoomed {
        match view.focus {
            Pane::Stops => areas.stops = main,
            Pane::Code => areas.code = main,
            Pane::Text => areas.text = main,
        }
        return areas;
    }
    let width = view.stops_width.min(main.width.saturating_sub(20));
    let [stops, right] = Layout::horizontal([Constraint::Length(width), Constraint::Fill(1)]).areas(main);
    let [code, text] =
        Layout::vertical([Constraint::Percentage(view.code_percent), Constraint::Fill(1)]).areas(right);
    Areas { main, stops, code, text }
}

/// A pane's border, highlighted when the pane has focus.
fn block(view: &TourView, pane: Pane) -> Block<'static> {
    let block = Block::bordered();
    if view.focus == pane { block.border_style(Style::new().fg(Color::Cyan)) } else { block }
}

fn draw_stops(f: &mut Frame, view: &TourView, area: Rect) {
    let tour = &view.state.tour;
    let title = if tour.title.is_empty() { "tourer" } else { &tour.title };
    let room = (area.width as usize).saturating_sub(9);
    let mut lines: Vec<Line> = tour
        .stops
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let marker = if i == view.sel { "▶" } else { " " };
            let asked = if s.thread.is_empty() { "" } else { " ·" };
            let title = if s.title.chars().count() > room {
                format!("{}…", s.title.chars().take(room.saturating_sub(1)).collect::<String>())
            } else {
                s.title.clone()
            };
            let line = Line::from(format!("{marker} {:>2} {title}{asked}", i + 1));
            if i == view.sel { line.bold().cyan() } else { line }
        })
        .collect();
    if view.state.planning {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("  … planning: {}", view.state.planner_activity)).dim());
    }
    f.render_widget(Paragraph::new(lines).block(block(view, Pane::Stops).title(title.to_string())), area);
}

fn draw_code(f: &mut Frame, view: &mut TourView, area: Rect) {
    let Some(target) = view.code_target() else {
        let summary = &view.state.tour.summary;
        let msg = if summary.is_empty() { "Exploring the folder…" } else { summary };
        let p = Paragraph::new(msg.to_string()).wrap(Wrap { trim: true });
        f.render_widget(p.block(block(view, Pane::Code)), area);
        return;
    };
    let block = block(view, Pane::Code).title(format!("{}:{}-{}", target.path, target.start, target.end));
    let height = area.height.saturating_sub(2) as usize;
    let width = area.width.saturating_sub(2) as usize;
    let notes: Vec<Note> = match view.state.tour.stops.get(view.sel) {
        Some(stop) => stop.notes_in(&target.path).into_iter().cloned().collect(),
        None => vec![],
    };
    if let Some(file) = view.file(&target.path) {
        let last_page = file.len().saturating_sub(height);
        view.code_scroll = view.code_scroll.min(last_page);
    }
    let scroll = view.code_scroll;
    let lines = match view.file(&target.path) {
        None => vec![Line::from("(file not found)").dim()],
        Some(file) => code_lines(file, &target, &notes, scroll, height, width),
    };
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Numbered code lines from `scroll` on, with the anchored range marked in the gutter.
/// A note that fits after its line goes there; longer ones wrap onto lines below it.
fn code_lines(
    file: &[StyledLine],
    target: &Anchor,
    notes: &[Note],
    scroll: usize,
    height: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let digits = file.len().to_string().len();
    let note_style = Style::new().fg(Color::Green).add_modifier(Modifier::ITALIC);
    let mut out = vec![];
    for (i, spans) in file.iter().enumerate().skip(scroll) {
        if out.len() >= height {
            break;
        }
        let n = i + 1;
        let (number, gutter) = if (target.start..=target.end).contains(&n) {
            (Span::from(format!("{n:>digits$}")).bold(), Span::from("▌").cyan())
        } else {
            (Span::from(format!("{n:>digits$}")).dark_gray(), Span::from(" "))
        };
        let code: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let mut used = digits + 1 + display_width(&code);
        let mut line = vec![number, gutter];
        line.extend(spans.iter().cloned());
        let mut below = vec![];
        for note in notes.iter().filter(|note| note.line == n) {
            let hint = format!("  ◂ {}", note.text);
            if below.is_empty() && used + display_width(&hint) <= width {
                used += display_width(&hint);
                line.push(Span::styled(hint, note_style));
                continue;
            }
            // Under the code it describes, aligned with the line's indentation.
            let indent = (code.len() - code.trim_start().len()).min(width / 2);
            let pad = " ".repeat(digits + 1 + indent);
            let room = width.saturating_sub(pad.len() + 2).max(10);
            for (k, part) in textwrap::wrap(&note.text, room).into_iter().enumerate() {
                let mark = if k == 0 { "╰ " } else { "  " };
                below.push(Line::from(vec![Span::raw(pad.clone()), Span::styled(format!("{mark}{part}"), note_style)]));
            }
        }
        out.push(Line::from(line));
        out.extend(below);
    }
    out.truncate(height);
    out
}

fn draw_text(f: &mut Frame, view: &mut TourView, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;
    let state = &view.state;
    let mut r = Renderer::new(width, &view.hl, view.link);
    if let Some(stop) = state.tour.stops.get(view.sel) {
        r.text(&format!("{}. {}", view.sel + 1, stop.title), Style::new().bold(), "", "");
        for a in &stop.anchors {
            r.text(&a.to_string(), Style::new(), "", "");
        }
        r.blank();
        r.markdown(&stop.body, "");
        for (i, ex) in stop.thread.iter().enumerate() {
            r.blank();
            r.text(&ex.q, Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD), "› ", "  ");
            let answer = tour::split_answer(&ex.a).text;
            let answer_state = state.answer_state((view.sel, i));
            if !answer.is_empty() {
                r.markdown(answer, "  ");
            }
            match answer_state {
                AnswerState::Queued => r.text("… queued", Style::new().dim(), "  ", "  "),
                AnswerState::Answering => {
                    r.text(&format!("… {}", state.guide_activity), Style::new().dim(), "  ", "  ")
                }
                AnswerState::Done => {}
            }
        }
        let suggestions = stop.suggestions();
        if !suggestions.is_empty() {
            r.blank();
        }
        for (i, s) in suggestions.iter().enumerate() {
            r.text(s, Style::new().fg(Color::Yellow), &format!("{} › ", i + 1), "    ");
        }
    }
    let (lines, links, spots) = (r.lines, r.links, r.spots);
    view.links = links;
    let max = lines.len().saturating_sub(height);
    if view.follow {
        view.text_scroll = max;
    }
    view.text_scroll = view.text_scroll.min(max);
    let visible = view.text_scroll..view.text_scroll + height;
    view.link_spots = spots
        .into_iter()
        .filter(|s| visible.contains(&s.line))
        .map(|s| {
            let x = area.x + 1 + s.cols.start as u16;
            let y = area.y + 1 + (s.line - view.text_scroll) as u16;
            (Rect::new(x, y, s.cols.len() as u16, 1), s.link)
        })
        .collect();
    let p = Paragraph::new(lines).scroll((view.text_scroll as u16, 0));
    f.render_widget(p.block(block(view, Pane::Text)), area);
}

/// The bottom line: the question being typed, an error, or the keys that work right now.
fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let tour = match &app.screen {
        Screen::Tour(view) => Some(view),
        Screen::Start(_) => None,
    };
    if let Some(input) = tour.and_then(|v| v.input.as_ref()) {
        let line = Line::from(vec![
            Span::from(" ask › ").cyan().bold(),
            Span::from(format!("{input}▏")),
            Span::from("   Enter send · Esc cancel").dim(),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    if let Some(err) = app.error.as_ref().or(tour.and_then(|v| v.state.error.as_ref())) {
        f.render_widget(Paragraph::new(Line::from(format!(" error: {err}")).red()), area);
        return;
    }
    let keys = match &app.screen {
        Screen::Start(start) => start_keys(start),
        Screen::Tour(view) => tour_keys(view),
    };
    let status = tour.map(|v| {
        let busy = if v.state.planning {
            " · planning…"
        } else if v.state.is_answering() {
            " · answering…"
        } else {
            ""
        };
        format!("{}{busy} ", v.agent)
    });
    let status_width = status.as_ref().map_or(0, |s| display_width(s) as u16);
    let [left, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Length(status_width)]).areas(area);
    // Keys come most useful first; the ones that do not fit are left out.
    let mut spans = vec![Span::from(" ")];
    let mut used = 1;
    for (key, what) in keys {
        let what = format!(" {what}   ");
        used += display_width(key) + display_width(&what);
        if used > left.width as usize + 3 {
            break;
        }
        spans.push(Span::from(key).bold());
        spans.push(Span::from(what).dim());
    }
    f.render_widget(Paragraph::new(Line::from(spans)), left);
    if let Some(status) = status {
        f.render_widget(Paragraph::new(Line::from(status).dim()), right);
    }
}

fn start_keys(start: &Start) -> Vec<(&'static str, &'static str)> {
    let mut keys = vec![("↑↓", "choose")];
    match start.row() {
        Row::Agent => keys.push(("←→", "change guide")),
        Row::Model => keys.push(("type", "a model name, empty for the default")),
        Row::Continue | Row::New => {}
    }
    keys.push(("Enter", "start"));
    if start.row() != Row::Model {
        keys.push(("q", "quit"));
    }
    keys
}

fn tour_keys(view: &TourView) -> Vec<(&'static str, &'static str)> {
    let mut keys = vec![("←→", "stop")];
    keys.push(match view.focus {
        Pane::Stops => ("↑↓", "choose"),
        Pane::Text | Pane::Code => ("↑↓", "scroll"),
    });
    if view.focus == Pane::Text && !view.links.is_empty() {
        keys.push(("Enter", "next code ref"));
    }
    if view.link.is_some() {
        keys.push(("Esc", "stop's code"));
    }
    if !view.state.tour.stops.is_empty() {
        keys.push(("?", "ask"));
    }
    if view.state.tour.stops.get(view.sel).is_some_and(|s| !s.suggestions().is_empty()) {
        keys.push(("1-3", "suggested"));
    }
    keys.push(("Tab", "next pane"));
    keys.push(if view.zoomed { ("z", "unzoom") } else { ("z", "zoom") });
    keys.push(("q", "quit"));
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_notes_after_or_under_lines() {
        let file: Vec<StyledLine> = ["fn a() {", "    b();", "}"].iter().map(|l| vec![Span::raw(*l)]).collect();
        let target = Anchor { path: "x.rs".into(), start: 1, end: 3 };
        let note = |line, text: &str| Note { path: "x.rs".into(), line, text: text.into() };
        let notes = [note(1, "short"), note(2, "a much longer note that does not fit after the line")];
        let lines = code_lines(&file, &target, &notes, 0, 10, 30);
        let text: Vec<String> = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert_eq!(text[0], "1▌fn a() {  ◂ short");
        assert_eq!(text[1], "2▌    b();");
        assert!(text[2].starts_with("      ╰ a much"), "{text:?}");
        assert_eq!(text.last().unwrap(), "3▌}");
        assert!(text.iter().all(|l| display_width(l) <= 30));
    }
}
