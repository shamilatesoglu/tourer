//! Drawing: the start screen, and the tour screen with its popups. Drawing also records where
//! things landed on screen, which input handling in `view.rs` uses.

use std::path::Path;
use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use textwrap::core::display_width;

use crate::agent::Agent;
use crate::app::{App, Row, Screen, Start};
use crate::engine::AnswerState;
use crate::highlight::StyledLine;
use crate::markdown::Renderer;
use crate::tour::{self, Note, Stop};
use crate::view::{Areas, Doc, Item, Pane, Popup, Shown, Step, Target, TourView};

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let root = shown_path(&app.root);
    match &mut app.screen {
        Screen::Start(start) => draw_start(f, start, &root, main),
        Screen::Tour(view) => draw_tour(f, view, main),
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
    if start.tours.is_some() {
        let room = (width as usize).saturating_sub(18);
        let text = match (start.instructions.is_empty(), row == Row::Instructions) {
            (true, false) => Span::from("none, e.g. focus on the parser").dim(),
            (true, true) => Span::from("▏type what the tour should focus on or how to explain").dim(),
            (false, true) => Span::from(format!("{}▏", left_cut(&start.instructions, room))),
            (false, false) => Span::from(left_cut(&start.instructions, room + 1)),
        };
        lines.push((Some(Row::Instructions), Line::from(vec![label(Row::Instructions, "Instructions"), text])));
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

/// The tour: a progress line, the document of stops, and the code beside it (or in a popup when
/// the screen is narrow). Also fills in the layout that input handling uses.
fn draw_tour(f: &mut Frame, view: &mut TourView, area: Rect) {
    let [progress, main] = Layout::vertical([Constraint::Length(1), Constraint::Fill(1)]).areas(area);
    view.wide = main.width >= 100;
    if !view.wide {
        view.zoomed = false;
        if view.focus == Pane::Code && !matches!(view.popup, Some(Popup::Code)) {
            view.focus = Pane::Text;
        }
    }
    let empty = Rect::new(main.x, main.y, 0, 0);
    let (text_area, code_area) = match (view.wide, view.zoomed, view.focus) {
        (false, ..) => (main, empty),
        (true, true, Pane::Text) => (main, empty),
        (true, true, Pane::Code) => (empty, main),
        (true, false, _) => {
            let [t, c] = Layout::horizontal([Constraint::Percentage(view.text_percent), Constraint::Fill(1)]).areas(main);
            (t, c)
        }
    };
    view.areas = Areas { text: text_area, code: code_area, ..Default::default() };

    // The document is laid out even when zoomed away, so links and the current stop stay right.
    let laid_out = if text_area.width > 0 { text_area } else { main };
    let width = laid_out.width.saturating_sub(2) as usize;
    let height = laid_out.height.saturating_sub(2) as usize;
    let (lines, doc, spots) = document(view, width, height);
    view.doc = doc;
    // A jump to a stop aims at where the stop is now, and the stop stays current until it lands.
    if let Some(i) = view.jump
        && let Some(&line) = view.doc.stop_lines.get(i)
    {
        view.text.to(line);
    }
    scroll_text(view, height);
    match view.jump {
        Some(_) if view.text.is_moving() => {}
        _ => {
            view.jump = None;
            view.current = match view.step {
                Some(step) => step.stop(),
                None => view.doc.stop_lines.iter().rposition(|&l| l <= view.text.top() + 1).unwrap_or(0),
            };
        }
    }

    if text_area.width > 0 {
        draw_text(f, view, lines, spots, text_area);
    }
    draw_progress(f, view, progress);
    if code_area.width > 0 {
        draw_code(f, view, code_area);
    }
    match view.popup {
        Some(Popup::Code) => {
            let area = main.inner(Margin::new(1, 1));
            f.render_widget(Clear, area);
            view.areas.code = area;
            draw_code(f, view, area);
        }
        Some(Popup::Outline { .. }) => draw_outline(f, view, main),
        Some(Popup::Files { .. }) => draw_files(f, view, main),
        None => {}
    }
    if view.areas.code.width > 0 {
        draw_note_popup(f, view);
    }
    let visible = seen_candidates(view);
    view.track_seen(visible);
}

/// Scrolls the text toward its target, keeping in view what must be: the question being typed
/// and the answer being followed.
fn scroll_text(view: &mut TourView, height: usize) {
    let keep = view.doc.input_line.or_else(|| view.follow.and_then(|s| view.doc.thread_ends.get(s).copied()));
    if let Some(line) = keep {
        let bottom = view.text.target() + height;
        if line + 2 > bottom {
            view.text.to(line + 2 - height.min(line + 2));
        }
    }
    view.text.limit(view.doc.len.saturating_sub(height));
    view.text.step();
}

/// A clickable spot in the document: a line, columns of it, and what it does.
struct DocSpot {
    line: usize,
    cols: std::ops::Range<usize>,
    target: Target,
}

/// The whole tour as one document: each stop with its code references, text, questions and
/// answers; the current stop's suggestions and the question being typed.
fn document(view: &mut TourView, width: usize, height: usize) -> (Vec<Line<'static>>, Doc, Vec<DocSpot>) {
    // Code excerpts go inline when there is no room for the code pane beside the text.
    let excerpts: Vec<Vec<Line<'static>>> = if view.wide {
        vec![]
    } else {
        let stops = view.state.tour.stops.clone();
        stops.iter().map(|stop| excerpt_lines(view, stop, width)).collect()
    };
    let shown = match &view.code.shown {
        Shown::Ref(a) => Some(a.clone()),
        _ => None,
    };
    let selected = match view.step.and_then(|step| view.step_link(step)) {
        Some(i) => Some(i),
        None => shown.and_then(|a| view.doc.links.iter().position(|l| *l == a)),
    };
    let step = view.step;
    if let Some(i) = view.jump {
        view.current = i;
    }
    let state = &view.state;
    let current = view.current;
    let mut r = Renderer::new(width, &view.hl, selected);
    let mut doc = Doc { height, ..Default::default() };
    let mut spots = vec![];
    let unseen = Span::from(" ✦").yellow().bold();
    // Steps other than code references, found while rendering.
    let mut steps: Vec<(Step, usize)> = vec![];
    // Per stop: the range of its links.
    let mut stop_links = vec![];

    if !state.tour.title.is_empty() {
        r.lines.push(Line::from(state.tour.title.clone()).bold());
        if !state.tour.summary.is_empty() {
            r.text(&state.tour.summary, Style::new().dim(), "", "");
        }
    }
    for (i, stop) in state.tour.stops.iter().enumerate() {
        doc.stop_lines.push(r.lines.len());
        let start = r.lines.len();
        let first_link = r.links.len();
        r.lines.push(Line::from("─".repeat(width)).dim());
        let mut number = Span::from(format!("{} ", i + 1)).cyan().bold();
        if step == Some(Step::Stop(i)) {
            number = number.reversed();
        }
        let mut header = vec![number, Span::from(stop.title.clone()).bold()];
        if stop.freshness.has_unseen_update() {
            header.push(unseen.clone());
        }
        r.lines.push(Line::from(header));
        r.blank();
        r.markdown(&stop.body, "");
        // Anchors belong where the text cites them; any it leaves out are listed after it.
        let cited = tour::refs(&stop.body);
        let uncited: Vec<String> = (stop.anchors.iter())
            .filter(|a| !cited.iter().any(|(_, c)| c.path == a.path && c.start <= a.end && a.start <= c.end))
            .map(|a| a.to_string())
            .collect();
        if !uncited.is_empty() {
            r.blank();
            r.text(&format!("See also {}", uncited.join(" ")), Style::new().dim(), "", "");
        }
        if let Some(lines) = excerpts.get(i) {
            r.lines.extend(lines.iter().cloned());
        }
        doc.items.push((start..r.lines.len(), Item::Stop(i)));

        for (e, ex) in stop.thread.iter().enumerate() {
            let start = r.lines.len();
            r.blank();
            let about = ex.code.as_ref().map(|a| format!("{a} ")).unwrap_or_default();
            let style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
            steps.push((Step::Answer(i, e), r.lines.len()));
            let marker = if step == Some(Step::Answer(i, e)) { "▶ " } else { "› " };
            r.text(&format!("{about}{}", ex.q), style, marker, "  ");
            if ex.freshness.has_unseen_update()
                && let Some(last) = r.lines.last_mut()
            {
                last.spans.push(unseen.clone());
            }
            let answer = tour::split_answer(&ex.a).text;
            if !answer.is_empty() {
                r.markdown(answer, "  ");
            }
            match state.answer_state((i, e)) {
                AnswerState::Queued => r.text("… queued", Style::new().dim(), "  ", "  "),
                AnswerState::Answering => r.text(&format!("… {}", state.guide_activity), Style::new().dim(), "  ", "  "),
                AnswerState::Done => {}
            }
            doc.items.push((start..r.lines.len(), Item::Answer(i, e)));
        }
        doc.thread_ends.push(r.lines.len());
        stop_links.push(first_link..r.links.len());

        // Every stop shows its suggestions, so the layout does not shift as the current stop
        // changes; the current stop's stand out, and only they have number keys. The last row
        // asks a question of one's own.
        let suggestions = stop.suggestions();
        let ask = if suggestions.is_empty() { "Ask a question" } else { "Ask a different question" };
        let rows = suggestions.iter().map(String::as_str).chain([ask]);
        r.blank();
        for (k, s) in rows.enumerate() {
            let from = r.lines.len();
            let (this, target) = if k < suggestions.len() {
                (Step::Suggestion { stop: i, n: k }, Target::Suggestion(i, s.to_string()))
            } else {
                (Step::Ask(i), Target::Ask(i))
            };
            let arrow = if step == Some(this) { "▶" } else { "›" };
            let (mut style, marker) = if i == current {
                (Style::new().fg(Color::Yellow), format!("{} {arrow} ", k + 1))
            } else {
                (Style::new().dim(), format!("  {arrow} "))
            };
            if this == Step::Ask(i) {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if step == Some(this) {
                style = style.add_modifier(Modifier::BOLD);
            }
            steps.push((this, from));
            r.text(s, style, &marker, "    ");
            for line in from..r.lines.len() {
                spots.push(DocSpot { line, cols: 0..width, target: target.clone() });
            }
        }
        if let Some(input) = view.input.as_ref().filter(|input| input.stop == i) {
            r.blank();
            doc.input_line = Some(r.lines.len());
            let about = input.code.as_ref().map(|a| format!("{a} ")).unwrap_or_default();
            r.text(&format!("{about}{}▏", input.text), Style::new().fg(Color::Cyan), "› ", "  ");
        }
        r.blank();
    }
    if state.planning {
        let activity = if state.tour.stops.is_empty() { "exploring the folder" } else { "planning more stops" };
        r.text(&format!("… {activity}: {}", state.planner_activity), Style::new().dim(), "", "");
    }
    // Room to scroll the last stop to the top, so jumping to it works like jumping to any other.
    for _ in 0..height * 2 / 3 {
        r.blank();
    }

    for s in &r.spots {
        spots.push(DocSpot { line: s.line, cols: s.cols.clone(), target: Target::Link(s.link) });
        if doc.link_lines.len() == s.link {
            doc.link_lines.push(s.line);
        }
    }
    // Code references are steps; a stop without any is one step itself.
    for (i, links) in stop_links.iter().enumerate() {
        let mut any = false;
        for (n, l) in links.clone().enumerate() {
            if let Some(&line) = doc.link_lines.get(l) {
                steps.push((Step::Ref { stop: i, n }, line));
                any = true;
            }
        }
        if !any {
            steps.push((Step::Stop(i), doc.stop_lines[i] + 1));
        }
    }
    // In document order; a question comes before a reference on its own line.
    steps.sort_by_key(|&(step, line)| (line, !matches!(step, Step::Answer(..))));
    doc.steps = steps;
    doc.stop_links = stop_links;
    doc.links = r.links;
    doc.len = r.lines.len();
    (r.lines, doc, spots)
}

/// A stop's code, for showing inline under its text: the start of each anchor.
fn excerpt_lines(view: &mut TourView, stop: &Stop, width: usize) -> Vec<Line<'static>> {
    const MAX_LINES: usize = 15;
    let mut out = vec![];
    for a in &stop.anchors {
        let notes: Vec<Note> = stop.notes_in(&a.path).into_iter().cloned().collect();
        let Some(file) = view.file(&a.path) else { continue };
        out.push(Line::default());
        out.push(Line::from(a.to_string()).dim());
        let lines = (a.end + 1).saturating_sub(a.start);
        let marks = Marks { range: Some((a.start, a.end)), ..Default::default() };
        out.extend(code_lines(file, &notes, a.start.saturating_sub(1), lines.min(MAX_LINES), width, marks));
        if lines > MAX_LINES {
            let more = format!("  … {} more; click the reference to open it", count(lines - MAX_LINES, "line"));
            out.push(Line::from(more).dim());
        }
    }
    out
}

fn draw_text(f: &mut Frame, view: &mut TourView, lines: Vec<Line<'static>>, spots: Vec<DocSpot>, area: Rect) {
    let top = view.text.top();
    let height = area.height.saturating_sub(2) as usize;
    for s in spots.into_iter().filter(|s| (top..top + height).contains(&s.line)) {
        let x = area.x + 1 + s.cols.start as u16;
        let y = area.y + 1 + (s.line - top) as u16;
        let width = (s.cols.len() as u16).min(area.right().saturating_sub(x + 1));
        view.areas.spots.push((Rect::new(x, y, width, 1), s.target));
    }
    let shown: Vec<Line> = lines.into_iter().skip(top).take(height).collect();
    f.render_widget(Paragraph::new(shown).block(pane_block(view, Pane::Text)), area);
}

/// A pane's border, highlighted when the pane has focus.
fn pane_block(view: &TourView, pane: Pane) -> Block<'static> {
    let block = Block::bordered();
    if view.focus == pane && view.wide { block.border_style(Style::new().fg(Color::Cyan)) } else { block }
}

/// One mark per stop, the current one's title, and where the tour is.
fn draw_progress(f: &mut Frame, view: &mut TourView, area: Rect) {
    let stops = &view.state.tour.stops;
    let mut spans = vec![Span::from(" ")];
    let mut x = area.x + 1;
    for (i, stop) in stops.iter().enumerate() {
        let mark = if stop.has_unseen_update() {
            Span::from("✦").yellow().bold()
        } else if i == view.current {
            Span::from("◉").cyan().bold()
        } else if stop.freshness.is_visited() {
            Span::from("●")
        } else {
            Span::from("○").dim()
        };
        if x + 2 > area.right().saturating_sub(20) {
            spans.push(Span::from("…").dim());
            break;
        }
        view.areas.spots.push((Rect::new(x, area.y, 1, 1), Target::Stop(i)));
        spans.push(mark);
        spans.push(Span::from(" "));
        x += 2;
    }
    if let Some(stop) = stops.get(view.current) {
        spans.push(Span::from(format!("  {}/{} ", view.current + 1, stops.len())).dim());
        spans.push(Span::from(stop.title.clone()).bold());
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_code(f: &mut Frame, view: &mut TourView, area: Rect) {
    let target = view.code_target();
    if target != view.code.last {
        if let Some(t) = &target {
            let (start, end) = t.range.unwrap_or((1, 1));
            view.code.scroll.to(start.saturating_sub(4));
            view.code.cursor = start;
            view.code.mark = None;
            view.code.flash = t.range.map(|_| Instant::now());
            // Long ranges start at the top; short ones get some code above them for context.
            if end - start > area.height as usize {
                view.code.scroll.to(start.saturating_sub(1));
            }
        }
        view.code.last = target.clone();
    }
    let Some(target) = target else {
        let summary = &view.state.tour.summary;
        let msg = if summary.is_empty() { "Exploring the folder…" } else { summary };
        let p = Paragraph::new(msg.to_string()).wrap(Wrap { trim: true });
        f.render_widget(p.block(pane_block(view, Pane::Code)), area);
        return;
    };
    let title = match target.range {
        Some((start, end)) => format!("{}:{start}-{end}", target.path),
        None => target.path.clone(),
    };
    let height = area.height.saturating_sub(2) as usize;
    let width = area.width.saturating_sub(2) as usize;
    let notes: Vec<Note> = match view.state.tour.stops.get(view.current) {
        Some(stop) => stop.notes_in(&target.path).into_iter().cloned().collect(),
        None => vec![],
    };
    view.code.height = height;
    view.code.lines = view.file(&target.path).map_or(0, |f| f.len());
    view.code.scroll.limit(view.code.lines.saturating_sub(height));
    view.code.scroll.step();
    let focused = view.focus == Pane::Code;
    let marks = Marks {
        range: target.range,
        cursor: focused.then_some(view.code.cursor),
        selection: view.code.mark.map(|_| view.code.selection()),
        flash: view.code.flash.is_some_and(|t| t.elapsed() < Duration::from_millis(700)),
    };
    let top = view.code.scroll.top();
    let block = pane_block(view, Pane::Code).title(title);
    let lines = match view.file(&target.path) {
        None => vec![Line::from("(file not found)").dim()],
        Some(file) => code_lines(file, &notes, top, height, width, marks),
    };
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// What to point out in the code: the cited lines, the cursor and the selection.
#[derive(Debug, Default, Clone, Copy)]
struct Marks {
    range: Option<(usize, usize)>,
    cursor: Option<usize>,
    selection: Option<(usize, usize)>,
    /// The cited lines just changed; show them brighter for a moment.
    flash: bool,
}

/// Numbered code lines from `scroll` on. Notes go after their line, cut to fit; the full text
/// shows in a popup.
fn code_lines(
    file: &[StyledLine],
    notes: &[Note],
    scroll: usize,
    height: usize,
    width: usize,
    marks: Marks,
) -> Vec<Line<'static>> {
    let digits = file.len().to_string().len();
    let note_style = Style::new().fg(Color::Green).add_modifier(Modifier::ITALIC);
    let within = |r: Option<(usize, usize)>, n: usize| r.is_some_and(|(a, b)| (a..=b).contains(&n));
    let mut out = vec![];
    for (i, spans) in file.iter().enumerate().skip(scroll).take(height) {
        let n = i + 1;
        let mut number = Span::from(format!("{n:>digits$}"));
        number = if within(marks.range, n) { number.bold() } else { number.dark_gray() };
        if marks.flash && within(marks.range, n) {
            number = number.cyan().reversed();
        }
        if marks.cursor == Some(n) {
            number = number.reversed();
        }
        let gutter = if within(marks.selection, n) {
            Span::from("┃").yellow().bold()
        } else if within(marks.range, n) {
            Span::from("▌").cyan()
        } else {
            Span::from(" ")
        };
        let code: String = spans.iter().map(|s| s.content.as_ref()).collect();
        let used = digits + 1 + display_width(&code);
        let mut line = vec![number, gutter];
        line.extend(spans.iter().cloned());
        let said: Vec<&str> = notes.iter().filter(|note| note.line == n).map(|note| note.text.as_str()).collect();
        if !said.is_empty() {
            let room = width.saturating_sub(used + 4);
            let text = said.join(" · ");
            let hint = if room < 6 {
                " ◂".to_string()
            } else if display_width(&text) <= room {
                format!("  ◂ {text}")
            } else {
                let cut: String = text.chars().take(room.saturating_sub(1)).collect();
                format!("  ◂ {}…", cut.trim_end())
            };
            line.push(Span::styled(hint, note_style));
        }
        out.push(Line::from(line));
    }
    out
}

/// The full notes on the line under the mouse, or under the cursor in the focused code pane.
fn draw_note_popup(f: &mut Frame, view: &mut TourView) {
    let area = view.areas.code;
    let hovered = view.hover.filter(|p| area.contains(*p)).and_then(|p| view.code_line_at(p.y));
    let line = hovered.or((view.focus == Pane::Code).then_some(view.code.cursor));
    let (Some(line), Some(target)) = (line, view.code.last.as_ref()) else { return };
    let Some(stop) = view.state.tour.stops.get(view.current) else { return };
    let notes: Vec<String> = stop.notes_in(&target.path).iter().filter(|n| n.line == line).map(|n| n.text.clone()).collect();
    let top = view.code.scroll.top();
    if notes.is_empty() || line <= top || line > top + view.code.height {
        return;
    }
    let width = area.width.saturating_sub(8).min(64);
    let text: Vec<Line> = notes
        .iter()
        .flat_map(|n| textwrap::wrap(n, width.saturating_sub(4) as usize))
        .map(|l| Line::from(l.into_owned()))
        .collect();
    let height = text.len() as u16 + 2;
    let row = area.y + 1 + (line - 1 - top) as u16;
    let y = if row + 1 + height <= area.bottom() { row + 1 } else { row.saturating_sub(height) };
    let popup = Rect::new(area.x + 6, y, width, height).intersection(f.area());
    f.render_widget(Clear, popup);
    let block = Block::bordered().border_style(Style::new().fg(Color::Green)).title(format!(" note · line {line} "));
    f.render_widget(Paragraph::new(text).block(block), popup);
}

/// A box in the middle of `area`.
fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width.saturating_sub(4));
    let height = height.min(area.height.saturating_sub(2));
    Rect::new(area.x + (area.width - width) / 2, area.y + (area.height - height) / 3, width, height)
}

/// The list of stops, to jump to one, with what the agents have used so far.
fn draw_outline(f: &mut Frame, view: &mut TourView, area: Rect) {
    let Some(Popup::Outline { sel }) = view.popup else { return };
    let stops = &view.state.tour.stops;
    let popup = centered(area, 76, stops.len() as u16 + 5);
    f.render_widget(Clear, popup);
    let inner = popup.inner(Margin::new(1, 1));
    let rows = inner.height.saturating_sub(2) as usize;
    let first = sel.saturating_sub(rows.saturating_sub(1));
    let mut lines = vec![];
    for (i, stop) in stops.iter().enumerate().skip(first).take(rows) {
        let mark = if stop.has_unseen_update() {
            Span::from("✦ ").yellow().bold()
        } else if stop.freshness.is_visited() {
            Span::from("● ")
        } else {
            Span::from("○ ").dim()
        };
        let asked = match stop.thread.len() {
            0 => String::new(),
            n => format!("  · {}", count(n, "question")),
        };
        let mut line = Line::from(vec![mark, Span::from(format!("{:>2} {}", i + 1, stop.title)), Span::from(asked).dim()]);
        if i == sel {
            line = line.reversed();
        }
        view.areas.popup_rows.push((Rect::new(inner.x, inner.y + lines.len() as u16, inner.width, 1), i));
        lines.push(line);
    }
    let state = &view.state;
    let used: Vec<String> = [("planner", state.planner_usage), ("guide", state.guide_usage)]
        .into_iter()
        .filter(|(_, u)| u.input > 0)
        .map(|(role, u)| format!("{role} {u}"))
        .collect();
    if !used.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(format!("tokens: {}", used.join(" · "))).dim());
    }
    view.areas.popup = popup;
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" stops ")), popup);
}

/// The file picker: what is typed, and the files that match it.
fn draw_files(f: &mut Frame, view: &mut TourView, area: Rect) {
    let Some(Popup::Files { query, sel }) = &view.popup else { return };
    let (query, sel) = (query.clone(), *sel);
    let found = view.matching_files(&query);
    let sel = sel.min(found.len().saturating_sub(1));
    if let Some(Popup::Files { sel: s, .. }) = &mut view.popup {
        *s = sel;
    }
    let popup = centered(area, 80, 20);
    f.render_widget(Clear, popup);
    let inner = popup.inner(Margin::new(1, 1));
    let rows = inner.height.saturating_sub(2) as usize;
    let first = sel.saturating_sub(rows.saturating_sub(1));
    let mut lines = vec![Line::from(vec![Span::from("› ").cyan().bold(), Span::from(format!("{query}▏"))]), Line::default()];
    for (i, path) in found.iter().enumerate().skip(first).take(rows) {
        let line = Line::from(path.clone());
        view.areas.popup_rows.push((Rect::new(inner.x, inner.y + lines.len() as u16, inner.width, 1), i));
        lines.push(if i == sel { line.reversed() } else { line });
    }
    view.areas.popup = popup;
    let title = format!(" open a file · {} ", count(found.len(), "match"));
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title)), popup);
}

/// Items on screen that the person has not seen in their latest version, with that version.
/// Answers count once they are finished.
fn seen_candidates(view: &TourView) -> Vec<(Item, u32)> {
    let top = view.text.top();
    let visible = top..top + view.doc.height;
    if view.areas.text.width == 0 {
        return vec![];
    }
    let stops = &view.state.tour.stops;
    view.doc
        .items
        .iter()
        .filter(|(range, _)| range.start < visible.end && visible.start < range.end)
        .filter_map(|&(_, item)| {
            let freshness = match item {
                Item::Stop(s) => stops.get(s)?.freshness,
                Item::Answer(s, e) => {
                    let ex = stops.get(s)?.thread.get(e)?;
                    let done = view.state.answer_state((s, e)) == AnswerState::Done && !ex.a.is_empty();
                    done.then_some(ex.freshness)?
                }
            };
            (!freshness.is_visited() || freshness.has_unseen_update()).then_some((item, freshness.revision()))
        })
        .collect()
}

/// The bottom line: an error, or the keys that work right now and what the guide is doing.
fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let tour = match &app.screen {
        Screen::Tour(view) => Some(view),
        Screen::Start(_) => None,
    };
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
        Row::Instructions => keys.push(("type", "what the new tour should focus on")),
        Row::Continue | Row::New => {}
    }
    keys.push(("Enter", if start.row() == Row::Instructions { "plan with them" } else { "start" }));
    if !matches!(start.row(), Row::Model | Row::Instructions) {
        keys.push(("q", "quit"));
    }
    keys
}

fn tour_keys(view: &TourView) -> Vec<(&'static str, &'static str)> {
    if view.input.is_some() {
        return vec![("Enter", "send"), ("Esc", "cancel")];
    }
    match view.popup {
        Some(Popup::Outline { .. }) => return vec![("↑↓", "choose"), ("Enter", "go"), ("Esc", "close")],
        Some(Popup::Files { .. }) => {
            return vec![("type", "to filter"), ("↑↓", "choose"), ("Enter", "open"), ("Esc", "close")];
        }
        Some(Popup::Code) | None => {}
    }
    let mut keys = vec![];
    match view.focus {
        Pane::Code => {
            keys.push(("↑↓", "line"));
            keys.push(("Shift ↑↓", "select"));
            keys.push(("Enter", if view.code.mark.is_some() { "ask about lines" } else { "ask about line" }));
            if view.code.mark.is_some() {
                keys.push(("Esc", "clear selection"));
            } else if matches!(view.popup, Some(Popup::Code)) {
                keys.push(("Esc", "back"));
            } else if view.code.shown != Shown::Follow {
                keys.push(("Esc", "follow the text"));
            }
            keys.push(("←→", "stop"));
            if view.wide {
                keys.push(("Tab", "text"));
            }
        }
        Pane::Text => {
            keys.push(("↑↓", "step"));
            keys.push(("←→", "stop"));
            if !view.state.tour.stops.is_empty() {
                let suggestion = matches!(view.step, Some(Step::Suggestion { .. }));
                keys.push(("Enter", if suggestion { "ask this" } else { "ask" }));
            }
            if view.code.shown != Shown::Follow {
                keys.push(("Esc", "follow the text"));
            }
            if view.wide {
                keys.push(("Tab", "code"));
            }
        }
    }
    keys.push(("o", "stops"));
    keys.push(("f", "open file"));
    if view.wide {
        keys.push(if view.zoomed { ("z", "unzoom") } else { ("z", "zoom") });
    }
    keys.push(("q", "quit"));
    keys
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puts_notes_after_lines_cut_to_fit() {
        let file: Vec<StyledLine> = ["fn a() {", "    b();", "}"].iter().map(|l| vec![Span::raw(*l)]).collect();
        let note = |line, text: &str| Note { path: "x.rs".into(), line, text: text.into() };
        let notes = [note(1, "short"), note(2, "a much longer note that does not fit after the line")];
        let marks = Marks { range: Some((1, 3)), ..Default::default() };
        let lines = code_lines(&file, &notes, 0, 10, 30, marks);
        let text: Vec<String> = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert_eq!(text[0], "1▌fn a() {  ◂ short");
        assert!(text[1].starts_with("2▌    b();  ◂ a much") && text[1].ends_with('…'), "{text:?}");
        assert_eq!(text[2], "3▌}");
        assert!(text.iter().all(|l| display_width(l) <= 30), "{text:?}");
    }

    #[test]
    fn down_steps_through_refs_questions_and_suggestions() {
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        let tour = r#"{"version":1,"root":"","title":"T","summary":"S","stops":[
            {"title":"A","body":"See [a.rs:1-2] and [b.rs:3-4].","anchors":[{"path":"a.rs","start":1,"end":2},{"path":"d.rs","start":5,"end":9}],
             "suggested":["Why?"],"thread":[{"q":"Q1","a":"Look at [c.rs:1-1]."}]},
            {"title":"B","body":"No code here."}]}"#;
        let mut view = TourView::new(".".into(), Agent::Claude);
        view.state.tour = serde_json::from_str(tour).unwrap();
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        let mut walked = vec![];
        for _ in 0..9 {
            term.draw(|f| draw_tour(f, &mut view, f.area())).unwrap();
            view.on_key(KeyEvent::from(KeyCode::Down));
            walked.push(view.step.unwrap());
        }
        use Step::*;
        assert_eq!(
            walked,
            // The text cites the a.rs anchor; the d.rs one it leaves out follows the text.
            [
                Ref { stop: 0, n: 0 },
                Ref { stop: 0, n: 1 },
                Ref { stop: 0, n: 2 },
                Answer(0, 0),
                Ref { stop: 0, n: 3 },
                Suggestion { stop: 0, n: 0 },
                Ask(0),
                Stop(1),
                Ask(1),
            ]
        );
        assert_eq!(view.code.shown, Shown::Follow);
    }

    #[test]
    fn scroll_glides_to_its_target() {
        let mut s = crate::view::Scroll::default();
        s.to(100);
        let mut frames = 0;
        while s.step() {
            frames += 1;
        }
        assert_eq!(s.top(), 100);
        assert!((5..30).contains(&frames), "{frames} frames");
    }
}
