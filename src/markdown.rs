//! A small markdown renderer for the narrative pane. It wraps text itself so it knows the
//! exact line count, and collects code references as links in display order.

use std::ops::Range;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use textwrap::core::display_width;

use crate::highlight::Highlighter;
use crate::tour::{self, Anchor};

pub struct Renderer<'h> {
    width: usize,
    hl: &'h Highlighter,
    selected: Option<usize>,
    pub lines: Vec<Line<'static>>,
    pub links: Vec<Anchor>,
    /// Where each link was placed, for mouse clicks.
    pub spots: Vec<Spot>,
}

/// Part of a link on screen: columns of one rendered line.
pub struct Spot {
    pub line: usize,
    pub cols: Range<usize>,
    pub link: usize,
}

enum Flag {
    Bold,
    Italic,
    Code,
}

struct Piece {
    text: String,
    style: Style,
    /// The index of the link this piece is, if it is one.
    link: Option<usize>,
}

impl<'h> Renderer<'h> {
    pub fn new(width: usize, hl: &'h Highlighter, selected: Option<usize>) -> Self {
        Renderer { width: width.max(10), hl, selected, lines: vec![], links: vec![], spots: vec![] }
    }

    pub fn blank(&mut self) {
        self.lines.push(Line::default());
    }

    /// A markdown document, every line indented by `indent`.
    pub fn markdown(&mut self, md: &str, indent: &str) {
        let mut fence: Option<(String, Vec<&str>)> = None;
        let lines: Vec<&str> = md.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            i += 1;
            let t = line.trim_start();
            if let Some(lang) = t.strip_prefix("```").or_else(|| t.strip_prefix("~~~")) {
                match fence.take() {
                    Some((lang, body)) => self.code(&lang, &body, indent),
                    None => fence = Some((lang.trim().to_string(), vec![])),
                }
                continue;
            }
            if let Some((_, body)) = &mut fence {
                body.push(line);
                continue;
            }
            if let Some(aligns) = lines.get(i).and_then(|next| table_separator(next)).filter(|_| t.contains('|')) {
                let mut rows = vec![table_cells(t)];
                i += 1;
                while let Some(row) = lines.get(i).map(|l| l.trim()).filter(|l| l.contains('|')) {
                    rows.push(table_cells(row));
                    i += 1;
                }
                self.table(&rows, &aligns, indent);
                continue;
            }
            let pad = format!("{indent}{}", " ".repeat((line.len() - t.len()) / 2 * 2));
            let hashes = t.chars().take_while(|&c| c == '#').count();
            if t.is_empty() {
                self.blank();
            } else if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
                let mut style = Style::new().add_modifier(Modifier::BOLD);
                if hashes == 1 {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                self.text(t[hashes..].trim(), style, indent, indent);
            } else if t.len() >= 3 && ['-', '*', '_'].iter().any(|&r| t.chars().all(|c| c == r)) {
                let rule = "─".repeat(self.width.saturating_sub(display_width(indent)));
                self.lines.push(Line::from(format!("{indent}{rule}")).style(Style::new().fg(Color::DarkGray)));
            } else if let Some(rest) = t.strip_prefix('>') {
                let bar = format!("{indent}│ ");
                self.text(rest.trim_start(), Style::new().add_modifier(Modifier::ITALIC), &bar, &bar);
            } else if let Some(rest) = ["- ", "* ", "+ "].iter().find_map(|b| t.strip_prefix(b)) {
                self.text(rest, Style::new(), &format!("{pad}• "), &format!("{pad}  "));
            } else if let Some((num, rest)) = numbered(t) {
                let hang = " ".repeat(num.len() + 1);
                self.text(rest, Style::new(), &format!("{pad}{num} "), &format!("{pad}{hang}"));
            } else {
                self.text(t, Style::new(), indent, indent);
            }
        }
        // An unclosed fence while an answer is still streaming.
        if let Some((lang, body)) = fence {
            self.code(&lang, &body, indent);
        }
    }

    fn code(&mut self, lang: &str, body: &[&str], indent: &str) {
        let bar = Span::styled(format!("{indent}▏ "), Style::new().fg(Color::DarkGray));
        for spans in self.hl.block(lang, &body.join("\n")) {
            let mut line = vec![bar.clone()];
            line.extend(spans);
            self.lines.push(Line::from(line));
        }
    }

    /// A table with a bold header row. Columns shrink to fit, wrapping their cells.
    fn table(&mut self, rows: &[Vec<String>], aligns: &[Align], indent: &str) {
        let cols = rows.iter().map(Vec::len).max().unwrap_or(0).max(aligns.len());
        let cell = |row: &[String], c: usize| row.get(c).map_or("", String::as_str).to_string();
        let mut natural = vec![1; cols];
        for row in rows {
            for (c, n) in natural.iter_mut().enumerate() {
                let mut r = self.sub(10_000);
                r.text(&cell(row, c), Style::new(), "", "");
                *n = (*n).max(line_width(&r.lines[0]));
            }
        }
        let room = self.width.saturating_sub(display_width(indent) + 3 * (cols - 1));
        let widths = fit(&natural, room);

        let dim = Style::new().fg(Color::DarkGray);
        for (r, row) in rows.iter().enumerate() {
            let style = if r == 0 { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
            let mut cells = vec![];
            for (c, &w) in widths.iter().enumerate() {
                let mut sub = self.sub(w);
                sub.text(&cell(row, c), style, "", "");
                for spot in &mut sub.spots {
                    spot.link += self.links.len();
                }
                self.links.append(&mut sub.links);
                cells.push((sub.lines, sub.spots));
            }
            let height = cells.iter().map(|(lines, _)| lines.len()).max().unwrap_or(1);
            for l in 0..height {
                let mut line = vec![Span::raw(indent.to_string())];
                let mut x = display_width(indent);
                for (c, &w) in widths.iter().enumerate() {
                    if c > 0 {
                        line.push(Span::styled(" │ ", dim));
                        x += 3;
                    }
                    let (lines, spots) = &cells[c];
                    let spans = lines.get(l).map(|l| l.spans.clone()).unwrap_or_default();
                    let gap = w.saturating_sub(spans.iter().map(|s| display_width(&s.content)).sum());
                    let (left, right) = match aligns.get(c).copied().unwrap_or(Align::Left) {
                        Align::Left => (0, gap),
                        Align::Center => (gap / 2, gap - gap / 2),
                        Align::Right => (gap, 0),
                    };
                    for spot in spots.iter().filter(|s| s.line == l) {
                        let cols = x + left + spot.cols.start..x + left + spot.cols.end;
                        self.spots.push(Spot { line: self.lines.len(), cols, link: spot.link });
                    }
                    line.push(Span::raw(" ".repeat(left)));
                    line.extend(spans);
                    line.push(Span::raw(" ".repeat(right)));
                    x += w;
                }
                self.lines.push(Line::from(line));
            }
            if r == 0 {
                let rule: Vec<String> = widths.iter().map(|&w| "─".repeat(w)).collect();
                self.lines.push(Line::from(format!("{indent}{}", rule.join("─┼─"))).style(dim));
            }
        }
    }

    /// A renderer of another width that shares this one's link numbering.
    fn sub(&self, width: usize) -> Renderer<'h> {
        let selected = self.selected.and_then(|s| s.checked_sub(self.links.len()));
        Renderer { width: width.max(1), hl: self.hl, selected, lines: vec![], links: vec![], spots: vec![] }
    }

    /// One line of inline markdown, wrapped. `first` prefixes the first line, `rest` the others.
    pub fn text(&mut self, text: &str, style: Style, first: &str, rest: &str) {
        let pieces = self.inline(text, style);
        self.wrap(pieces, first, rest);
    }

    /// Splits inline markdown into styled pieces: **bold**, *italic*, `code` and code references.
    fn inline(&mut self, text: &str, base: Style) -> Vec<Piece> {
        let refs = tour::refs(text);
        let (mut bold, mut italic, mut code) = (false, false, false);
        let mut pieces = vec![];
        let mut buf = String::new();
        let style = |bold: bool, italic: bool, code: bool| {
            let mut s = base;
            if bold {
                s = s.add_modifier(Modifier::BOLD);
            }
            if italic {
                s = s.add_modifier(Modifier::ITALIC);
            }
            if code {
                s = s.fg(Color::Magenta);
            }
            s
        };
        let mut i = 0;
        while i < text.len() {
            let rest = &text[i..];
            let toggle = if let Some((range, anchor)) = refs.iter().find(|(r, _)| r.start == i) {
                let mut s = Style::new().fg(Color::Blue).add_modifier(Modifier::UNDERLINED);
                if self.selected == Some(self.links.len()) {
                    s = s.add_modifier(Modifier::REVERSED);
                }
                pieces.push(Piece { text: std::mem::take(&mut buf), style: style(bold, italic, code), link: None });
                pieces.push(Piece { text: text[range.clone()].to_string(), style: s, link: Some(self.links.len()) });
                self.links.push(anchor.clone());
                i = range.end;
                continue;
            } else if rest.starts_with('`') {
                Some((1, Flag::Code))
            } else if !code && rest.starts_with("**") {
                Some((2, Flag::Bold))
            } else if !code
                && rest.starts_with('*')
                && (italic || rest[1..].chars().next().is_some_and(|c| !c.is_whitespace()))
            {
                Some((1, Flag::Italic))
            } else {
                None
            };
            match toggle {
                Some((len, flag)) => {
                    pieces.push(Piece { text: std::mem::take(&mut buf), style: style(bold, italic, code), link: None });
                    match flag {
                        Flag::Bold => bold = !bold,
                        Flag::Italic => italic = !italic,
                        Flag::Code => code = !code,
                    }
                    i += len;
                }
                None => {
                    let c = rest.chars().next().unwrap();
                    buf.push(c);
                    i += c.len_utf8();
                }
            }
        }
        pieces.push(Piece { text: buf, style: style(bold, italic, code), link: None });
        pieces
    }

    /// Records that part of a link sits at columns `col..col + width` of the line being built.
    fn spot(&mut self, link: Option<usize>, col: usize, width: usize) {
        if let Some(link) = link {
            self.spots.push(Spot { line: self.lines.len(), cols: col..col + width, link });
        }
    }

    /// Lays out pieces, breaking lines only at whitespace (or inside words wider than a line).
    fn wrap(&mut self, pieces: Vec<Piece>, first: &str, rest: &str) {
        // Group into words: runs of non-whitespace, possibly in several styles, e.g. "[a.rs:1])."
        let mut words: Vec<(bool, Vec<Piece>)> = vec![];
        let mut space_before = false;
        for p in &pieces {
            let parts = if p.link.is_some() { vec![p.text.as_str()] } else { split_words(&p.text) };
            for part in parts {
                if part.chars().all(char::is_whitespace) {
                    space_before = true;
                    continue;
                }
                let piece = Piece { text: part.to_string(), style: p.style, link: p.link };
                match words.last_mut() {
                    Some((_, word)) if !space_before => word.push(piece),
                    _ => words.push((space_before, vec![piece])),
                }
                space_before = false;
            }
        }

        let prefix_style = Style::new().fg(Color::DarkGray);
        let mut line = vec![Span::styled(first.to_string(), prefix_style)];
        let mut indent = display_width(first);
        let mut w = indent;
        for (space, parts) in words {
            let width: usize = parts.iter().map(|p| display_width(&p.text)).sum();
            let gap = usize::from(space && w > indent);
            if w > indent && w + gap + width > self.width {
                self.lines.push(Line::from(std::mem::take(&mut line)));
                line.push(Span::styled(rest.to_string(), prefix_style));
                indent = display_width(rest);
                w = indent;
            } else if gap == 1 {
                line.push(Span::raw(" "));
                w += 1;
            }
            for Piece { mut text, style, link } in parts {
                // Only a word wider than a whole line gets split.
                while display_width(&text) > self.width.saturating_sub(w) {
                    if w > indent {
                        self.lines.push(Line::from(std::mem::take(&mut line)));
                        line.push(Span::styled(rest.to_string(), prefix_style));
                        indent = display_width(rest);
                        w = indent;
                        continue;
                    }
                    let room = self.width.saturating_sub(w).max(1);
                    let cut: String = text.chars().take(room).collect();
                    text = text[cut.len()..].to_string();
                    self.spot(link, w, display_width(&cut));
                    line.push(Span::styled(cut, style));
                    w = self.width;
                }
                self.spot(link, w, display_width(&text));
                w += display_width(&text);
                line.push(Span::styled(text, style));
            }
        }
        self.lines.push(Line::from(line));
    }
}

/// "12. rest" or "12) rest" → ("12.", "rest").
fn numbered(t: &str) -> Option<(&str, &str)> {
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    let after = t.get(digits..digits + 2)?;
    if digits == 0 || digits > 3 || !(after == ". " || after == ") ") {
        return None;
    }
    Some((&t[..=digits], &t[digits + 2..]))
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

/// A table's header separator like "|:---|--:|", giving each column's alignment.
fn table_separator(line: &str) -> Option<Vec<Align>> {
    let line = line.trim();
    if !line.contains('-') || !line.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
        return None;
    }
    table_cells(line)
        .iter()
        .map(|c| match (c.starts_with(':'), c.ends_with(':')) {
            _ if !c.contains('-') => None,
            (true, true) => Some(Align::Center),
            (false, true) => Some(Align::Right),
            _ => Some(Align::Left),
        })
        .collect()
}

/// "| a | b \| c |" → ["a", "b | c"].
fn table_cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').filter(|l| !l.ends_with('\\')).unwrap_or(line);
    let mut cells = vec![String::new()];
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => cells.last_mut().unwrap().push(chars.next().unwrap()),
            '|' => cells.push(String::new()),
            c => cells.last_mut().unwrap().push(c),
        }
    }
    cells.iter().map(|c| c.trim().to_string()).collect()
}

/// Column widths within `room`: narrow columns keep their width, wide ones share the rest.
fn fit(natural: &[usize], room: usize) -> Vec<usize> {
    let mut widths = natural.to_vec();
    let mut order: Vec<usize> = (0..natural.len()).collect();
    order.sort_by_key(|&c| natural[c]);
    let mut left = room;
    for (k, &c) in order.iter().enumerate() {
        let share = left / (order.len() - k);
        widths[c] = natural[c].min(share).max(3);
        left = left.saturating_sub(widths[c]);
    }
    widths
}

fn line_width(line: &Line) -> usize {
    line.spans.iter().map(|s| display_width(&s.content)).sum()
}

/// Splits text into alternating runs of whitespace and non-whitespace.
fn split_words(text: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut start = 0;
    let mut prev: Option<bool> = None;
    for (i, c) in text.char_indices() {
        let ws = c.is_whitespace();
        if prev.is_some_and(|p| p != ws) {
            out.push(&text[start..i]);
            start = i;
        }
        prev = Some(ws);
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(md: &str, width: usize) -> (Vec<String>, Vec<Anchor>) {
        let hl = Highlighter::new();
        let mut r = Renderer::new(width, &hl, None);
        r.markdown(md, "");
        let lines = r.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        (lines, r.links)
    }

    #[test]
    fn renders_blocks_and_inline() {
        let (lines, links) = render("# Title\n- **bold** item with `code`\n2. see [a.rs:1-2]", 80);
        assert_eq!(lines, vec!["Title", "• bold item with code", "2. see [a.rs:1-2]"]);
        assert_eq!(links[0].path, "a.rs");
    }

    #[test]
    fn wraps_without_splitting_links() {
        let (lines, links) = render("aaaa bbbb [src/some/long/path.rs:10-20] cccc", 32);
        assert!(lines.iter().any(|l| l.contains("[src/some/long/path.rs:10-20]")));
        assert_eq!(links.len(), 1);
        assert_eq!(lines.len(), 3);
        assert!(lines.iter().all(|l| display_width(l) <= 32));
    }

    #[test]
    fn keeps_punctuation_with_links() {
        let (lines, _) = render("aaaa bbbb cccc ([x/y.rs:45]).", 26);
        assert_eq!(lines, vec!["aaaa bbbb cccc", "([x/y.rs:45])."]);
    }

    #[test]
    fn keeps_multiplication_literal() {
        let (lines, _) = render("a * b and *it*", 80);
        assert_eq!(lines, vec!["a * b and it"]);
    }

    #[test]
    fn renders_tables() {
        let md = "| Key | Does |\n|-----|-----:|\n| `n` | next [a.rs:1] |\n| q | quit |";
        let (lines, links) = render(md, 80);
        assert_eq!(lines, vec!["Key │          Does", "────┼──────────────", "n   │ next [a.rs:1]", "q   │          quit"]);
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn records_link_positions() {
        let hl = Highlighter::new();
        let mut r = Renderer::new(80, &hl, None);
        r.markdown("see [a.rs:1] and [b.rs:2]\n\n| k | v |\n|--|--|\n| n | next [c.rs:3] |", "");
        let spots: Vec<_> = r.spots.iter().map(|s| (s.line, s.cols.clone(), s.link)).collect();
        assert_eq!(spots, vec![(0, 4..12, 0), (0, 17..25, 1), (4, 11..19, 2)]);
    }

    #[test]
    fn wraps_wide_tables() {
        let md = "a | b\n--|--\nshort | one two three four five six seven eight";
        let (lines, _) = render(md, 24);
        assert!(lines.len() > 3);
        assert!(lines.iter().all(|l| display_width(l) <= 24), "{lines:?}");
    }

    #[test]
    fn highlights_fenced_code() {
        let (lines, _) = render("```rust\nfn main() {}\n```", 80);
        assert_eq!(lines, vec!["▏ fn main() {}"]);
    }
}
