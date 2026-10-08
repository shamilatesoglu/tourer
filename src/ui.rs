use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::app::App;
use crate::engine::AnswerState;
use crate::markdown::Renderer;
use crate::tour;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(f.area());
    let [list, right] = Layout::horizontal([Constraint::Length(32), Constraint::Fill(1)]).areas(main);
    let [code, text] = Layout::vertical([Constraint::Percentage(45), Constraint::Fill(1)]).areas(right);
    draw_stops(f, app, list);
    // The narrative decides the links, and the code pane may show one of them.
    draw_text(f, app, text);
    draw_code(f, app, code);
    draw_status(f, app, status);
}

fn draw_stops(f: &mut Frame, app: &App, area: Rect) {
    let tour = &app.engine.tour;
    let title = if tour.title.is_empty() { "tourer" } else { &tour.title };
    let room = (area.width as usize).saturating_sub(9);
    let mut lines: Vec<Line> = tour
        .stops
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let marker = if i == app.sel { "▶" } else { " " };
            let asked = if s.thread.is_empty() { "" } else { " ·" };
            let title = if s.title.chars().count() > room {
                format!("{}…", s.title.chars().take(room.saturating_sub(1)).collect::<String>())
            } else {
                s.title.clone()
            };
            let line = Line::from(format!("{marker} {:>2} {title}{asked}", i + 1));
            if i == app.sel { line.bold().cyan() } else { line }
        })
        .collect();
    if app.engine.planning {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("  … planning: {}", app.engine.planner_activity)).dim());
    }
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title.to_string())), area);
}

fn draw_code(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(target) = app.code_target() else {
        let summary = &app.engine.tour.summary;
        let msg = if summary.is_empty() { "Exploring the folder…" } else { summary };
        let p = Paragraph::new(msg.to_string()).wrap(Wrap { trim: true });
        f.render_widget(p.block(Block::bordered()), area);
        return;
    };
    let block = Block::bordered().title(format!("{}:{}-{}", target.path, target.start, target.end));
    let height = area.height.saturating_sub(2) as usize;
    let scroll = app.code_scroll;
    let lines: Vec<Line> = match app.file(&target.path) {
        None => vec![Line::from("(file not found)").dim()],
        Some(file) => {
            let width = file.len().to_string().len();
            file.iter()
                .enumerate()
                .skip(scroll)
                .take(height)
                .map(|(i, spans)| {
                    let n = i + 1;
                    let (number, gutter) = if (target.start..=target.end).contains(&n) {
                        (Span::from(format!("{n:>width$}")).bold(), Span::from("▌").cyan())
                    } else {
                        (Span::from(format!("{n:>width$}")).dark_gray(), Span::from(" "))
                    };
                    let mut line = vec![number, gutter];
                    line.extend(spans.iter().cloned());
                    Line::from(line)
                })
                .collect()
        }
    };
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_text(f: &mut Frame, app: &mut App, area: Rect) {
    let width = area.width.saturating_sub(2) as usize;
    let height = area.height.saturating_sub(2) as usize;
    let engine = &app.engine;
    let mut r = Renderer::new(width, &app.hl, app.link);
    if let Some(stop) = engine.tour.stops.get(app.sel) {
        r.text(&format!("{}. {}", app.sel + 1, stop.title), Style::new().bold(), "", "");
        for a in &stop.anchors {
            r.text(&a.to_string(), Style::new(), "", "");
        }
        r.blank();
        r.markdown(&stop.body, "");
        for (i, ex) in stop.thread.iter().enumerate() {
            r.blank();
            r.text(&ex.q, Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD), "› ", "  ");
            let answer = tour::split_answer(&ex.a).0;
            let state = engine.answer_state((app.sel, i));
            if !answer.is_empty() {
                r.markdown(answer, "  ");
            }
            match state {
                AnswerState::Queued => r.text("… queued", Style::new().dim(), "  ", "  "),
                AnswerState::Answering => {
                    r.text(&format!("… {}", engine.guide_activity), Style::new().dim(), "  ", "  ")
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
    let (lines, links) = (r.lines, r.links);
    app.links = links;
    let max = lines.len().saturating_sub(height);
    if app.follow {
        app.text_scroll = max;
    }
    app.text_scroll = app.text_scroll.min(max);
    let p = Paragraph::new(lines).scroll((app.text_scroll as u16, 0));
    f.render_widget(p.block(Block::bordered()), area);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let line = if let Some(input) = &app.input {
        Line::from(vec![Span::from(" ask › ").cyan().bold(), Span::from(format!("{input}▏"))])
    } else if let Some(err) = &app.engine.error {
        Line::from(format!(" error: {err}")).red()
    } else {
        let mut parts = vec![" n/p stop", "j/k scroll", "tab links", "J/K code", "? ask", "1-3 suggested", "q quit"];
        if app.engine.is_answering() {
            parts.push("· guide answering…");
        }
        Line::from(parts.join("  ")).dim()
    };
    f.render_widget(Paragraph::new(line), area);
}
