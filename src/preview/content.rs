use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Index(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub faint: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub strike: bool,
}

impl Style {
    fn paints_spaces(self) -> bool {
        self.bg.is_some() || self.inverse || self.underline || self.strike
    }
}

#[derive(Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    pub fn visible_text(&self) -> &str {
        if self.style.paints_spaces() { &self.text } else { self.text.trim_end() }
    }
}

pub type Line = Vec<Span>;

#[derive(Debug)]
pub struct Text {
    pub lines: Vec<Line>,
    pub numbered: bool,
}

const MAX_STYLED_SPANS: usize = 16_384;

impl Text {
    pub fn trim_blank_tail(&mut self) {
        while self.lines.last().is_some_and(|line| line.iter().all(|span| span.visible_text().is_empty())) {
            self.lines.pop();
        }
    }

    pub fn plain(text: &str, max_lines: usize) -> Self {
        let mut doc = TextBuilder::new(max_lines, false);
        for line in text.lines().take(max_lines) {
            doc.push(line, Style::default());
            doc.newline();
        }
        doc.finish()
    }

    pub fn weight(&self) -> usize {
        self.lines.capacity() * std::mem::size_of::<Line>()
            + self
                .lines
                .iter()
                .map(|line| {
                    line.capacity() * std::mem::size_of::<Span>()
                        + line.iter().map(|span| span.text.capacity()).sum::<usize>()
                })
                .sum::<usize>()
    }
}

pub struct TextBuilder {
    doc: Text,
    line: Line,
    limit: usize,
    spans: usize,
}

impl TextBuilder {
    pub fn new(limit: usize, numbered: bool) -> Self {
        Self { doc: Text { lines: vec![], numbered }, line: vec![], limit, spans: 0 }
    }

    pub fn full(&self) -> bool {
        self.doc.lines.len() >= self.limit
    }

    pub fn newline(&mut self) {
        if !self.full() {
            self.doc.lines.push(std::mem::take(&mut self.line));
        }
    }

    pub fn push(&mut self, s: &str, style: Style) {
        for c in s.chars() {
            self.character(c, style);
        }
    }

    fn character(&mut self, c: char, style: Style) {
        if self.full() || c == '\r' {
            return;
        }
        let style = if self.spans < MAX_STYLED_SPANS { style } else { Style::default() };
        if !self.line.last().is_some_and(|span| span.style == style) {
            self.line.push(Span { text: String::new(), style });
            self.spans += 1;
        }
        let text = &mut self.line.last_mut().unwrap().text;
        match c {
            '\t' => text.push_str("    "),
            c if c.is_control() => text.push('\u{fffd}'),
            c => text.push(c),
        }
    }

    pub fn finish(mut self) -> Text {
        if !self.line.is_empty() {
            self.newline();
        }
        self.doc
    }
}

struct Ansi {
    text: TextBuilder,
    style: Style,
    colors: bool,
}

impl vte::Perform for Ansi {
    fn print(&mut self, c: char) {
        self.text.character(c, self.style);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.text.newline(),
            b'\t' => self.print('\t'),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, action: char) {
        if !self.colors || ignore || !intermediates.is_empty() || action != 'm' {
            return;
        }
        let mut params = params.iter();
        while let Some(group) = params.next() {
            let [code, rest @ ..] = group else { continue };
            match *code {
                0 => self.style = Style::default(),
                1 => self.style.bold = true,
                2 => self.style.faint = true,
                3 => self.style.italic = true,
                4 => self.style.underline = true,
                7 => self.style.inverse = true,
                9 => self.style.strike = true,
                22 => {
                    self.style.bold = false;
                    self.style.faint = false;
                }
                23 => self.style.italic = false,
                24 => self.style.underline = false,
                27 => self.style.inverse = false,
                29 => self.style.strike = false,
                30..=37 => self.style.fg = Some(Color::Index((*code - 30) as u8)),
                39 => self.style.fg = None,
                40..=47 => self.style.bg = Some(Color::Index((*code - 40) as u8)),
                49 => self.style.bg = None,
                90..=97 => self.style.fg = Some(Color::Index((*code - 90 + 8) as u8)),
                100..=107 => self.style.bg = Some(Color::Index((*code - 100 + 8) as u8)),
                38 | 48 | 58 => {
                    // Both semicolon and colon forms occur in terminal tools.
                    let color = if rest.is_empty() {
                        match params.next() {
                            Some([5]) => params.next().and_then(|v| indexed(v.first().copied())),
                            Some([2]) => {
                                let mut component = || params.next().and_then(|v| v.first().copied()).and_then(byte);
                                let (r, g, b) = (component(), component(), component());
                                r.zip(g).zip(b).map(|((r, g), b)| Color::Rgb(r, g, b))
                            }
                            _ => None,
                        }
                    } else {
                        match rest {
                            [5, n] => indexed(Some(*n)),
                            [2, r, g, b] | [2, 0, r, g, b] => rgb(*r, *g, *b),
                            _ => None,
                        }
                    };
                    match (*code, color) {
                        (38, Some(c)) => self.style.fg = Some(c),
                        (48, Some(c)) => self.style.bg = Some(c),
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

fn byte(n: u16) -> Option<u8> {
    u8::try_from(n).ok()
}

fn indexed(n: Option<u16>) -> Option<Color> {
    n.and_then(byte).map(Color::Index)
}

fn rgb(r: u16, g: u16, b: u16) -> Option<Color> {
    Some(Color::Rgb(byte(r)?, byte(g)?, byte(b)?))
}

pub fn external(raw: &[u8], max_lines: usize, colors: bool) -> Option<Text> {
    let mut performer = Ansi { text: TextBuilder::new(max_lines, false), style: Style::default(), colors };
    let mut parser = vte::Parser::<1024>::new();
    // VTE consumes CSI/OSC/DCS. APC, PM and SOS are also ignored by its state machine.
    // Only printable text, newlines, tabs and the SGR subset above reach the document.
    let input = String::from_utf8_lossy(raw);
    for chunk in input.as_bytes().chunks(4096) {
        parser.advance(&mut performer, chunk);
        if performer.text.full() {
            break;
        }
    }
    let text = performer.text.finish();
    text.lines.iter().flatten().any(|span| !span.visible_text().is_empty()).then_some(text)
}

/// Prefix ending on a character boundary, with its width in terminal cells.
pub fn clip(text: &str, width: usize, used: usize) -> (&str, usize) {
    let mut cells = used;
    let mut end = 0;
    for (offset, cluster) in text.grapheme_indices(true) {
        let w = cluster.width();
        if cells + w > width {
            break;
        }
        cells += w;
        end = offset + cluster.len();
    }
    (&text[..end], cells - used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_keeps_only_text_and_supported_styles() {
        let bytes = b"A\x1b[1;38:2::1:2:3mB\x1b[999;999H\x1b]52;c;secret\x07\x1b_Gbad\x1b\\\x1bPbad\x1b\\\x1b[mC";
        let text = external(bytes, 2, true).unwrap();
        let line = &text.lines[0];
        assert_eq!(line.iter().map(|span| span.text.as_str()).collect::<String>(), "ABC");
        assert_eq!(line[1].style.fg, Some(Color::Rgb(1, 2, 3)));
        assert!(line[1].style.bold);
        assert_eq!(line[2].style, Style::default());
        assert_eq!(external(bytes, 2, false).unwrap().lines[0].len(), 1);
        assert!(external(b" \x1b[31m\n", 2, true).is_none());
    }

    #[test]
    fn limits_lines_and_clips_unicode_cells() {
        assert_eq!(Text::plain("1\n2\n3", 2).lines.len(), 2);
        assert_eq!(clip("a界界", 4, 0), ("a界", 3));
        assert_eq!(clip("界", 2, 1), ("", 0));
        assert_eq!(clip("éx", 1, 0), ("é", 1));
    }
}
