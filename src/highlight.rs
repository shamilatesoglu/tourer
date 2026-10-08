//! Syntax highlighting with the terminal's own palette, so it suits light and dark themes.

use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use syntect::easy::HighlightLines;
use syntect::highlighting::{self, FontStyle, Theme};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Files longer than this are shown without highlighting.
const MAX_LINES: usize = 20_000;

pub type StyledLine = Vec<Span<'static>>;

pub struct Highlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl Highlighter {
    pub fn new() -> Self {
        let theme = two_face::theme::extra().get(two_face::theme::EmbeddedThemeName::Ansi).clone();
        Highlighter { syntaxes: two_face::syntax::extra_newlines(), theme }
    }

    /// Highlights a whole file, picking the syntax from its name or first line.
    pub fn file(&self, path: &str, text: &str) -> Vec<StyledLine> {
        let p = Path::new(path);
        let syntax = p
            .extension()
            .and_then(|e| self.syntaxes.find_syntax_by_extension(&e.to_string_lossy()))
            .or_else(|| {
                let name = p.file_name()?.to_string_lossy();
                self.syntaxes.find_syntax_by_extension(&name)
            })
            .or_else(|| self.syntaxes.find_syntax_by_first_line(text.lines().next()?));
        self.lines(syntax, text)
    }

    /// Highlights a fenced code block by its language tag, e.g. "rust" or "py".
    pub fn block(&self, lang: &str, text: &str) -> Vec<StyledLine> {
        self.lines(self.syntaxes.find_syntax_by_token(lang.trim()), text)
    }

    fn lines(&self, syntax: Option<&SyntaxReference>, text: &str) -> Vec<StyledLine> {
        let plain = |l: &str| vec![Span::raw(l.trim_end_matches(['\n', '\r']).replace('\t', "    "))];
        let Some(syntax) = syntax.filter(|_| text.lines().count() <= MAX_LINES) else {
            return text.lines().map(plain).collect();
        };
        let mut h = HighlightLines::new(syntax, &self.theme);
        syntect::util::LinesWithEndings::from(text)
            .map(|line| match h.highlight_line(line, &self.syntaxes) {
                Ok(regions) => regions
                    .into_iter()
                    .map(|(style, s)| {
                        let s = s.trim_end_matches(['\n', '\r']).replace('\t', "    ");
                        Span::styled(s, convert(style))
                    })
                    .collect(),
                Err(_) => plain(line),
            })
            .collect()
    }
}

/// bat's "ansi" theme encodes palette indexes in alpha 0 and "default color" in alpha 1.
fn convert(s: highlighting::Style) -> Style {
    let c = s.foreground;
    let mut style = match c.a {
        0 => Style::new().fg(Color::Indexed(c.r)),
        1 => Style::new(),
        _ => Style::new().fg(Color::Rgb(c.r, c.g, c.b)),
    };
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    style
}
