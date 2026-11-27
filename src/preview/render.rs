use std::io::Write;
use std::path::Path;

use image::{
    RgbaImage,
    imageops::{FilterType, resize},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    config::Ui,
    engine::Kind,
    icons, popup,
    theme::{self, Theme},
};

use super::{
    content::{Color, Style, Text, clip},
    layout::Placement,
    native::{Content, TreeRow},
};

#[derive(Debug, Clone, Copy)]
pub enum Colors {
    Mono,
    Basic,
    Indexed,
    Rgb,
}

impl Colors {
    pub fn terminal() -> Self {
        let term = std::env::var("TERM").unwrap_or_default();
        let colorterm = std::env::var("COLORTERM").unwrap_or_default();
        let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
        if term == "dumb" {
            Self::Mono
        } else if matches!(colorterm.as_str(), "truecolor" | "24bit")
            || term.ends_with("-direct")
            || term.contains("kitty")
            || term.contains("ghostty")
            || matches!(program.as_str(), "iTerm.app" | "ghostty" | "WezTerm")
            || std::env::var_os("WT_SESSION").is_some()
        {
            Self::Rgb
        } else if term.contains("256color") || cfg!(windows) {
            Self::Indexed
        } else {
            Self::Basic
        }
    }

    fn write(self, out: &mut Vec<u8>, color: Color, bg: bool) {
        let base = if bg { 40 } else { 30 };
        let ext = base + 8;
        if matches!(self, Self::Mono) {
            return;
        }
        let color = match color {
            Color::Rgb(r, g, b) if matches!(self, Self::Rgb) => {
                let _ = write!(out, ";{ext};2;{r};{g};{b}");
                return;
            }
            Color::Rgb(r, g, b) => cube(r, g, b),
            Color::Index(index) => index,
        };
        match (self, color) {
            (_, 0..=7) => {
                let _ = write!(out, ";{}", base + color);
            }
            (_, 8..=15) => {
                let _ = write!(out, ";{}", base + 60 + color - 8);
            }
            (Self::Basic, _) => {
                let _ = write!(out, ";{}", base + basic(color));
            }
            _ => {
                let _ = write!(out, ";{ext};5;{color}");
            }
        }
    }
}

fn cube(r: u8, g: u8, b: u8) -> u8 {
    let quantize = |x| ((u16::from(x) * 5 + 127) / 255) as u8;
    if r.max(g).max(b) - r.min(g).min(b) <= 5 && (8..=238).contains(&r) {
        return 232 + (r - 8) / 10;
    }
    16 + 36 * quantize(r) + 6 * quantize(g) + quantize(b)
}

fn basic(code: u8) -> u8 {
    if code >= 232 {
        return if code >= 244 { 7 } else { 0 };
    }
    let code = code - 16;
    u8::from(code / 36 >= 3) + 2 * u8::from(code / 6 % 6 >= 3) + 4 * u8::from(code % 6 >= 3)
}

fn style(out: &mut Vec<u8>, colors: Colors, t: &Theme, s: Style, dim: bool) {
    let _ = write!(out, "\x1b[0;{};{}", t.bg, t.fg);
    if dim {
        out.extend_from_slice(b";2");
    }
    for (on, sgr) in [(s.bold, 1), (s.faint, 2), (s.italic, 3), (s.underline, 4), (s.inverse, 7), (s.strike, 9)] {
        if on {
            let _ = write!(out, ";{sgr}");
        }
    }
    if let Some(fg) = s.fg {
        colors.write(out, fg, false);
    }
    if let Some(bg) = s.bg {
        colors.write(out, bg, true);
    }
    out.push(b'm');
}

fn digits(lines: usize) -> usize {
    (lines.max(1).ilog10() + 1) as usize
}

fn span_start(text: &str, first: bool) -> &str {
    if first {
        text.trim_start_matches(|c: char| {
            let mut bytes = [0; 4];
            c.encode_utf8(&mut bytes).width() == 0
        })
    } else {
        text
    }
}

pub fn text_width(doc: &Text) -> u16 {
    let width = doc
        .lines
        .iter()
        .filter_map(|line| {
            let last = line.iter().rposition(|span| !span.visible_text().is_empty())?;
            let mut used = 0;
            for span in &line[..last] {
                used += clip(span_start(&span.text, used == 0), usize::MAX, 0).1;
            }
            Some(used + clip(span_start(line[last].visible_text(), used == 0), usize::MAX, 0).1)
        })
        .max()
        .unwrap_or(0);
    let gutter = if doc.numbered && !doc.lines.is_empty() { digits(doc.lines.len()) + 1 } else { 0 };
    (width + gutter).min(usize::from(u16::MAX)) as u16
}

pub fn tree_width(ui: &Ui, rows: &[TreeRow]) -> u16 {
    rows.iter()
        .map(|row| {
            clip(&row.prefix, usize::MAX, 0).1
                + clip(&popup::printable(&row.name), usize::MAX, 0).1
                + tree_icon(ui, row, 2).map_or(0, |(_, _, cells)| cells + 1)
        })
        .max()
        .unwrap_or(0)
        .min(usize::from(u16::MAX)) as u16
}

pub fn title(path: &Path) -> String {
    let text = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
    popup::printable(&text)
}

pub fn draw(
    out: &mut Vec<u8>,
    ui: &Ui,
    placement: Placement,
    document: Option<&Content>,
    title: &str,
    scroll: usize,
    options: Options,
) {
    let Options { colors, image, title_icon, loading_frame, animation } = options;
    let t = theme::resolve(ui);
    let frame = placement.frame;
    let r = placement.content();
    let framed = placement.framed();
    let edge = format!("\x1b[0;{};{}m", t.border_bg, t.border_fg);
    let w = usize::from(r.width);
    let (tl, tr, bl, br) =
        if ui.border == "rounded" { ("╭", "╮", "╰", "╯") } else { ("┌", "┐", "└", "┘") };
    if framed {
        let title = title.trim_start_matches(|c: char| c.is_control() || c == '\u{200b}');
        let (glyph, glyph_color, glyph_width) = title_icon.map_or((String::new(), "", 0), |(glyph, color)| {
            let (glyph, width) = clip(&glyph, w.saturating_sub(3), 0);
            (glyph.to_owned(), color, width)
        });
        let icon_width = usize::from(glyph_width > 0) * (glyph_width + 1);
        let (title, title_width) = clip(title, w.saturating_sub(icon_width + 3), 0);
        let total = icon_width + title_width;
        let fill = "─".repeat(w.saturating_sub(if total == 0 { 0 } else { total + 2 }));
        let _ = write!(out, "\x1b[{};{}H{edge}{tl}", frame.row + 1, frame.col + 1);
        if glyph_width > 0 {
            let _ = write!(out, " \x1b[{glyph_color}m{glyph}\x1b[0;{};{}m", t.bg, t.fg);
        }
        if !title.is_empty() {
            let _ = write!(out, " {title} ");
        }
        let _ = write!(out, "{fill}{tr}\x1b[0m");
    }
    let mosaic = match (document, image) {
        (Some(Content::Image(raster)), Some((style @ (ImageStyle::Quadrants | ImageStyle::HalfBlocks), (w, h)))) => {
            Some((resize(&raster.pixels, w, h, FilterType::Triangle), style))
        }
        _ => None,
    };
    let h = usize::from(r.height);
    let reveal_rows =
        animation.map_or(
            h,
            |(effect, step)| {
                if effect == super::config::Animation::Reveal { h * usize::from(step) / 4 } else { h }
            },
        );
    let fade = animation.is_some_and(|(effect, step)| effect == super::config::Animation::Fade && step < 2);
    for row in 0..h {
        let _ = write!(out, "\x1b[{};{}H", usize::from(r.row) + row + 1, frame.col + 1);
        if framed {
            let _ = write!(out, "{edge}│");
        }
        let _ = write!(out, "\x1b[0;{};{}m", t.bg, t.fg);
        if fade {
            out.extend_from_slice(b"\x1b[2m");
        }
        let used = if row >= reveal_rows {
            0
        } else {
            match document {
                Some(Content::Text(doc)) => text_row(out, doc, scroll + row, w, &t, colors, fade),
                Some(Content::Image(_)) => {
                    mosaic.as_ref().map_or(0, |(pixels, style)| image_row(out, pixels, row as u32, &t, colors, *style))
                }
                Some(Content::Directory(rows)) => rows.get(scroll + row).map_or(0, |row| tree_row(out, ui, row, w, &t)),
                None if row == 0 => {
                    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
                    let label = format!("{} Loading preview…", SPINNER[usize::from(loading_frame) % SPINNER.len()]);
                    let (label, used) = clip(&label, w, 0);
                    out.extend_from_slice(label.as_bytes());
                    used
                }
                None => 0,
            }
        };
        let _ = write!(out, "\x1b[0;{}m{}", t.bg, " ".repeat(w - used));
        if framed {
            let track = if document.is_some_and(|doc| {
                let lines = doc.lines();
                lines > h && row >= scroll * h / lines && row < scroll * h / lines + (h * h / lines).max(1)
            }) {
                "┃"
            } else {
                "│"
            };
            let _ = write!(out, "{edge}{track}");
        }
        out.extend_from_slice(b"\x1b[0m");
    }
    if framed {
        let _ = write!(out, "\x1b[{};{}H{edge}{bl}{}{br}\x1b[0m", r.row + r.height + 1, frame.col + 1, "─".repeat(w));
    }
    if matches!(image, Some((ImageStyle::Bitmap, _))) {
        let _ = write!(out, "\x1b[0;{}m", t.bg);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ImageStyle {
    Quadrants,
    HalfBlocks,
    Bitmap,
}

impl ImageStyle {
    pub fn samples(self) -> (u32, u32) {
        match self {
            Self::Quadrants => (2, 2),
            Self::HalfBlocks => (1, 2),
            Self::Bitmap => (1, 1),
        }
    }
}

pub struct Options {
    pub colors: Colors,
    pub image: Option<(ImageStyle, (u32, u32))>,
    pub title_icon: Option<(String, &'static str)>,
    pub loading_frame: u8,
    pub animation: Option<(super::config::Animation, u8)>,
}

fn tree_icon(ui: &Ui, row: &TreeRow, width: usize) -> Option<(String, &'static str, usize)> {
    if ui.icons == "none" {
        return None;
    }
    let kind = if row.directory { Kind::Folder } else { Kind::File };
    let (glyph, color) = icons::glyph(kind, None, &row.name, icons::use_nerd(&ui.icons));
    let mut printable = popup::printable(glyph);
    let (glyph, cells) = clip(&printable, width.min(2), 0);
    printable.truncate(glyph.len());
    (cells > 0).then_some((printable, color, cells))
}

fn tree_row(out: &mut Vec<u8>, ui: &Ui, row: &TreeRow, width: usize, theme: &Theme) -> usize {
    let (prefix, mut used) = clip(&row.prefix, width, 0);
    out.extend_from_slice(prefix.as_bytes());
    if prefix.len() != row.prefix.len() {
        return used;
    }
    if used + 2 <= width
        && let Some((glyph, color, cells)) = tree_icon(ui, row, width - used - 1)
    {
        let _ = write!(out, "\x1b[{color}m{glyph} \x1b[{}m", theme.fg);
        used += cells + 1;
    }
    let printable = popup::printable(&row.name);
    let (label, cells) = clip(&printable, width, used);
    out.extend_from_slice(label.as_bytes());
    used + cells
}

fn text_row(out: &mut Vec<u8>, doc: &Text, line: usize, width: usize, t: &Theme, colors: Colors, dim: bool) -> usize {
    let Some(spans) = doc.lines.get(line) else { return 0 };
    let mut used = 0;
    let number_width = digits(doc.lines.len());
    if doc.numbered && width > number_width {
        let _ = write!(out, "\x1b[{}m{:>number_width$} ", t.desc_fg, line + 1);
        used = number_width + 1;
    }
    let start = used;
    for span in spans {
        let source = span_start(&span.text, used == start);
        let (text, count) = clip(source, width, used);
        if !text.is_empty() {
            style(out, colors, t, span.style, dim);
            out.extend_from_slice(text.as_bytes());
        }
        used += count;
        if used == width || text.len() < source.len() {
            break;
        }
    }
    used
}

fn background(theme: &Theme) -> [u8; 3] {
    if let Some(rgb) = theme.bg.strip_prefix("48;2;") {
        let mut values = rgb.split(';').filter_map(|v| v.parse::<u8>().ok());
        if let (Some(r), Some(g), Some(b)) = (values.next(), values.next(), values.next()) {
            return [r, g, b];
        }
    }
    let index = theme.bg.strip_prefix("48;5;").and_then(|n| n.parse::<u8>().ok()).unwrap_or(0);
    match index {
        0..=15 => {
            let level = if index < 8 { 128 } else { 255 };
            if index == 8 { [128; 3] } else { [index & 1, (index >> 1) & 1, (index >> 2) & 1].map(|b| b * level) }
        }
        16..=231 => {
            let index = index - 16;
            [index / 36, (index / 6) % 6, index % 6].map(|n| if n == 0 { 0 } else { n * 40 + 55 })
        }
        _ => [8 + (index - 232) * 10; 3],
    }
}

const QUADRANTS: [char; 16] = [' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█'];

fn quadrants(samples: [[u8; 3]; 4]) -> (char, [u8; 3], [u8; 3]) {
    let mut best = (u32::MAX, ' ', [0; 3], [0; 3]);
    // Complementary masks differ only in the choice of foreground and background.
    for mask in 0..8 {
        let mut sum = [[0u16; 3]; 2];
        let mut count = [0u16; 2];
        for (i, pixel) in samples.iter().enumerate() {
            let group = (mask >> i) & 1;
            count[group] += 1;
            for (total, &value) in sum[group].iter_mut().zip(pixel) {
                *total += u16::from(value);
            }
        }
        let mean = [0, 1].map(|g| sum[g].map(|v| ((v + count[g] / 2) / count[g].max(1)) as u8));
        let error = samples
            .iter()
            .enumerate()
            .map(|(i, rgb)| {
                rgb.iter().zip(mean[(mask >> i) & 1]).map(|(&a, b)| u32::from(a.abs_diff(b)).pow(2)).sum::<u32>()
            })
            .sum();
        if error < best.0 {
            best = (error, QUADRANTS[mask], mean[1], mean[0]);
            if error == 0 {
                break;
            }
        }
    }
    (best.1, best.2, best.3)
}

fn image_row(out: &mut Vec<u8>, img: &RgbaImage, row: u32, t: &Theme, colors: Colors, style_kind: ImageStyle) -> usize {
    if 2 * row >= img.height() {
        return 0;
    }
    let background = background(t);
    let pixel = |x: u32, y: u32| {
        img.get_pixel_checked(x, y).map_or(background, |sample| {
            [0, 1, 2].map(|i| {
                sample[i].saturating_add(((u16::from(background[i]) * u16::from(255 - sample[3]) + 127) / 255) as u8)
            })
        })
    };
    let color = |[r, g, b]: [u8; 3]| ([r, g, b] != background).then_some(Color::Rgb(r, g, b));
    let sx = if style_kind == ImageStyle::Quadrants { 2 } else { 1 };
    let w = img.width().div_ceil(sx);
    if matches!(colors, Colors::Mono) {
        const SHADES: [char; 5] = [' ', '░', '▒', '▓', '█'];
        for x in 0..w {
            let sum: usize = (x * sx..(x + 1) * sx)
                .flat_map(|x| [pixel(x, row * 2), pixel(x, row * 2 + 1)])
                .flatten()
                .map(usize::from)
                .sum();
            let range = sx as usize * 6 * 255;
            out.extend_from_slice(SHADES[(4 * sum + range / 2) / range].encode_utf8(&mut [0; 4]).as_bytes());
        }
        return w as usize;
    }
    let mut prev = None;
    for x in 0..w {
        let (glyph, top, bottom) = if sx == 2 {
            quadrants([
                pixel(x * 2, row * 2),
                pixel(x * 2 + 1, row * 2),
                pixel(x * 2, row * 2 + 1),
                pixel(x * 2 + 1, row * 2 + 1),
            ])
        } else {
            ('▀', pixel(x, row * 2), pixel(x, row * 2 + 1))
        };
        let (fg, bg) = (color(top), color(bottom));
        if prev != Some((fg, bg)) {
            style(out, colors, t, Style { fg, bg, ..Style::default() }, false);
            prev = Some((fg, bg));
        }
        out.extend_from_slice(glyph.encode_utf8(&mut [0; 4]).as_bytes());
    }
    w as usize
}

#[cfg(test)]
mod tests {
    use super::super::{content::Text, layout::Position};
    use super::*;
    use crate::{config::Ui, popup::Rect};

    #[test]
    fn native_rows_use_existing_theme_and_clip_wide_text() {
        let mut bytes = Vec::new();
        let placement = Placement {
            frame: Rect { row: 1, col: 3, height: 4, width: 8 },
            position: Position::Right,
            inline: true,
            preferred: None,
        };
        let content = Content::Text(Text::plain("a界界界\x1b[99H\nZ", 5));
        draw(
            &mut bytes,
            &Ui::default(),
            placement,
            Some(&content),
            "file",
            0,
            Options { colors: Colors::Rgb, image: None, title_icon: None, loading_frame: 0, animation: None },
        );
        let mut screen = vt100::Parser::new(8, 20, 0);
        screen.process(b"protected");
        screen.process(&bytes);
        let screen = screen.screen();
        assert_eq!(screen.contents_between(0, 0, 0, 20), "protected");
        assert_eq!(screen.contents_between(2, 3, 2, 11), "│a界界 │");
        assert!(!String::from_utf8(bytes).unwrap().contains("\x1b[99H"));
    }

    #[test]
    fn quadrants_preserve_vertical_and_diagonal_edges_with_two_colors() {
        let (dark, light) = ([0; 3], [255; 3]);
        assert_eq!(quadrants([light, dark, light, dark]), ('▌', light, dark));
        assert_eq!(quadrants([dark, light, light, dark]), ('▞', light, dark));
        assert_eq!(super::super::raster::fit_cells((200, 100), (8, 16), (20, 10), (2, 2)), (40, 10));
    }

    #[test]
    fn title_icon_and_loading_spinner_are_rendered() {
        let mut bytes = Vec::new();
        let placement = Placement {
            frame: Rect { row: 0, col: 0, height: 4, width: 20 },
            position: Position::Right,
            inline: true,
            preferred: None,
        };
        draw(
            &mut bytes,
            &Ui::default(),
            placement,
            None,
            "file.rs",
            0,
            Options {
                colors: Colors::Rgb,
                image: None,
                title_icon: Some(("◆".into(), "38;5;75")),
                loading_frame: 2,
                animation: None,
            },
        );
        let output = String::from_utf8(bytes).unwrap();
        assert!(output.contains("◆"));
        assert!(output.contains("file.rs"));
        assert!(output.contains("Loading preview"));
        assert!(output.contains("⠹"));
    }

    #[test]
    fn reveal_starts_with_blank_content_and_completes_after_four_steps() {
        let mut bytes = Vec::new();
        let placement = Placement {
            frame: Rect { row: 0, col: 0, height: 4, width: 20 },
            position: Position::Right,
            inline: true,
            preferred: None,
        };
        let content = Content::Text(Text::plain("visible", 1));
        draw(
            &mut bytes,
            &Ui::default(),
            placement,
            Some(&content),
            "file",
            0,
            Options {
                colors: Colors::Rgb,
                image: None,
                title_icon: None,
                loading_frame: 0,
                animation: Some((super::super::config::Animation::Reveal, 0)),
            },
        );
        assert!(!String::from_utf8(bytes).unwrap().contains("visible"));
    }
}
