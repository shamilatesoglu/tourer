//! A small markdown renderer for the narrative pane. It wraps text itself so it knows the
//! exact line count, and collects code references as links in display order.

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
}

enum Flag {
    Bold,
    Italic,
    Code,
}

struct Piece {
    text: String,
    style: Style,
    link: bool,
}

impl<'h> Renderer<'h> {
    pub fn new(width: usize, hl: &'h Highlighter, selected: Option<usize>) -> Self {
        Renderer { width: width.max(10), hl, selected, lines: vec![], links: vec![] }
    }

    pub fn blank(&mut self) {
        self.lines.push(Line::default());
    }

    /// A markdown document, every line indented by `indent`.
    pub fn markdown(&mut self, md: &str, indent: &str) {
        let mut fence: Option<(String, Vec<&str>)> = None;
        for line in md.lines() {
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
                pieces.push(Piece { text: std::mem::take(&mut buf), style: style(bold, italic, code), link: false });
                pieces.push(Piece { text: text[range.clone()].to_string(), style: s, link: true });
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
                    pieces.push(Piece { text: std::mem::take(&mut buf), style: style(bold, italic, code), link: false });
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
        pieces.push(Piece { text: buf, style: style(bold, italic, code), link: false });
        pieces
    }

    /// Lays out pieces, breaking lines only at whitespace (or inside words wider than a line).
    fn wrap(&mut self, pieces: Vec<Piece>, first: &str, rest: &str) {
        // Group into words: runs of non-whitespace, possibly in several styles, e.g. "[a.rs:1])."
        let mut words: Vec<(bool, Vec<(String, Style)>)> = vec![];
        let mut space_before = false;
        for p in &pieces {
            let parts = if p.link { vec![p.text.as_str()] } else { split_words(&p.text) };
            for part in parts {
                if part.chars().all(char::is_whitespace) {
                    space_before = true;
                    continue;
                }
                match words.last_mut() {
                    Some((_, word)) if !space_before => word.push((part.to_string(), p.style)),
                    _ => words.push((space_before, vec![(part.to_string(), p.style)])),
                }
                space_before = false;
            }
        }

        let prefix_style = Style::new().fg(Color::DarkGray);
        let mut line = vec![Span::styled(first.to_string(), prefix_style)];
        let mut indent = display_width(first);
        let mut w = indent;
        for (space, parts) in words {
            let width: usize = parts.iter().map(|(t, _)| display_width(t)).sum();
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
            for (mut text, style) in parts {
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
                    line.push(Span::styled(cut, style));
                    w = self.width;
                }
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
    fn highlights_fenced_code() {
        let (lines, _) = render("```rust\nfn main() {}\n```", 80);
        assert_eq!(lines, vec!["▏ fn main() {}"]);
    }
}
