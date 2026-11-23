use std::ops::Range;

use crate::popup::Rect;

use super::config::{Config, Layout, Position};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FloatingSize {
    #[default]
    Configured,
    Expanded,
    Large,
}

impl FloatingSize {
    pub fn next(self) -> Self {
        match self {
            Self::Configured => Self::Expanded,
            Self::Expanded => Self::Large,
            Self::Large => Self::Configured,
        }
    }

    fn percent(self) -> Option<u32> {
        match self {
            Self::Configured => None,
            Self::Expanded => Some(66),
            Self::Large => Some(90),
        }
    }
}

#[derive(Clone, Copy, Default)]
pub enum Floating {
    #[default]
    Compact,
    Tall {
        content_width: u16,
    },
    Wide,
    Image {
        preset: FloatingSize,
        preferred: Option<(u16, u16)>,
    },
}

pub const MIN_DIFF_COLUMNS: u16 = 80;

pub fn inline_sides(preferred: Position) -> [Position; 2] {
    if preferred == Position::Left { [Position::Left, Position::Right] } else { [Position::Right, Position::Left] }
}

#[derive(Clone)]
pub struct Context {
    pub size: (u16, u16),
    pub completion: Rect,
    pub known_from: u16,
    /// Full rows occupied by the prompt and the edit buffer, including right prompts.
    pub prompt: Range<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub frame: Rect,
    pub position: Position,
    pub inline: bool,
    pub preferred: Option<(u16, u16)>,
}

impl Placement {
    pub fn framed(self) -> bool {
        self.frame.width >= 3 && self.frame.height >= 3
    }

    pub fn content(self) -> Rect {
        let Rect { row, col, height, width } = self.frame;
        let border = u16::from(self.framed());
        Rect { row: row + border, col: col + border, height: height - 2 * border, width: width - 2 * border }
    }

    pub fn fit(self, (columns, lines): (u16, u16)) -> Self {
        let border = 2 * u16::from(self.framed());
        let width = columns.max(1).saturating_add(border).min(self.frame.width);
        let height = lines.max(1).saturating_add(border).min(self.frame.height);
        let mut frame = self.frame;
        if !self.inline || self.position == Position::Left {
            frame.col += frame.width - width;
        }
        if (self.inline && self.position == Position::Above) || (!self.inline && self.position == Position::Below) {
            frame.row += frame.height - height;
        }
        frame.width = width;
        frame.height = height;
        Self { frame, ..self }
    }
}

fn end(r: Rect) -> u16 {
    r.row.saturating_add(r.height)
}

fn right(r: Rect) -> u16 {
    r.col.saturating_add(r.width)
}

pub fn overlaps(a: Rect, b: Rect) -> bool {
    a.row < end(b) && b.row < end(a) && a.col < right(b) && b.col < right(a)
}

pub fn place(
    config: &Config,
    context: &Context,
    previous: Option<Placement>,
    mode: Layout,
    size: Floating,
) -> Option<Placement> {
    let floating = || floating_corner(config, context, previous.filter(|p| !p.inline), size);
    if mode == Layout::Floating {
        return floating();
    }
    if mode == Layout::Auto
        && previous.is_some_and(|p| !p.inline)
        && let Some(corner) = floating()
    {
        return Some(corner);
    }

    let automatic = [Position::Right, Position::Left, Position::Above, Position::Below];
    let sides = inline_sides(config.position);
    let positions = match (mode, &config.position) {
        (Layout::Inline, _) => sides.as_slice(),
        (_, Position::Auto) => automatic.as_slice(),
        (_, position) => std::slice::from_ref(position),
    };
    let preferred = previous.filter(|p| p.inline && positions.contains(&p.position)).map(|p| p.position);
    let inline = preferred
        .into_iter()
        .chain(positions.iter().copied())
        .find_map(|position| inline_at(config, context, position));
    inline.or_else(|| if mode == Layout::Auto { floating() } else { None })
}

fn preferred_size(config: &Config, terminal: (u16, u16), available: (u16, u16), preset: FloatingSize) -> (u16, u16) {
    let limit = |setting: &crate::config::Size, total, available, min, cap| {
        preset
            .percent()
            .map_or_else(
                || setting.resolve(total).unwrap_or(total),
                |percent| ((u32::from(available) * percent + 50) / 100) as u16,
            )
            .clamp(min, cap)
    };
    let width = limit(&config.width, terminal.1, available.0, config.min_width, 256);
    let height = limit(&config.height, terminal.0, available.1, config.min_height, 128);
    (width, height)
}

fn floating_space(config: &Config, context: &Context, known_from: u16, use_margin: bool) -> [(Position, u16, u16); 4] {
    let (rows, cols) = context.size;
    let list = context.completion;
    let prompt_gap = if use_margin && config.preserve_prompt { config.margin } else { 0 };
    let gap = if use_margin { config.margin.max(1) } else { 0 };
    let list_top = list.row.saturating_sub(gap);
    let list_bottom = end(list).saturating_add(gap);
    let side_width = cols.saturating_sub(right(list).saturating_add(gap));

    let top = known_from..context.prompt.start.min(rows).saturating_sub(prompt_gap);
    let bottom = context.prompt.end.saturating_add(prompt_gap).max(known_from)..rows;
    let [above, below] = [(Position::Above, top), (Position::Below, bottom)].map(|(position, band)| {
        let full_height = band.end.saturating_sub(band.start);
        if list_top < band.end && band.start < list_bottom {
            let clear_height = if position == Position::Above {
                list_top.saturating_sub(band.start).min(full_height)
            } else {
                band.end.saturating_sub(list_bottom.max(band.start))
            };
            [(position, cols, clear_height), (position, side_width, full_height)]
        } else {
            [(position, cols, full_height), (position, 0, 0)]
        }
    });
    [above[0], above[1], below[0], below[1]]
}

fn floating_corner(
    config: &Config,
    context: &Context,
    previous: Option<Placement>,
    size: Floating,
) -> Option<Placement> {
    // Compare the two sides before considering the previous pane's dimensions.
    // Rows above Lig's origin count towards the geometry, but cannot be painted.
    let (side, _, _) = floating_space(config, context, 0, true).into_iter().max_by_key(|&(position, w, h)| {
        (u32::from(w) * u32::from(h), previous.is_some_and(|p| p.position == position), position == Position::Above)
    })?;
    let other = if side == Position::Above { Position::Below } else { Position::Above };
    let preferred = match size {
        Floating::Image { preferred, .. } => preferred,
        _ => None,
    };
    let configured = preferred_size(config, context.size, context.size, FloatingSize::Configured);
    let best = |side, use_margin, minimum| {
        floating_space(config, context, context.known_from, use_margin)
            .into_iter()
            .map(|(position, w, h)| (position, w.min(256), h.min(128)))
            .filter(|&(position, w, h)| position == side && w >= minimum && h >= minimum)
            .max_by_key(|&(_, w, h)| match size {
                Floating::Tall { content_width } => {
                    let keeps_width = w >= content_width.saturating_add(2).min(configured.0);
                    (keeps_width, false, if keeps_width { u32::from(h) } else { u32::from(w) * u32::from(h) }, w)
                }
                Floating::Wide => (w >= MIN_DIFF_COLUMNS + 2, false, u32::from(h), w),
                _ => {
                    let fits = preferred.is_some_and(|(width, height)| width <= w && height <= h);
                    let meets_config = preferred.is_none() && w >= config.min_width && h >= config.min_height;
                    (fits, meets_config, u32::from(w) * u32::from(h), w)
                }
            })
    };
    let framed = |side| best(side, true, 3).or_else(|| best(side, false, 3));
    let available = |side| framed(side).or_else(|| best(side, false, 1));
    let (position, w, h) = available(side).or_else(|| available(other))?;
    let (width, height, preferred) = match size {
        Floating::Compact => (configured.0.min(w), configured.1.min(h), None),
        Floating::Tall { .. } => (configured.0.min(w), h, None),
        Floating::Wide => (w, h, None),
        Floating::Image { preset, .. } => {
            let target = preferred.unwrap_or_else(|| {
                let (width, height) = preferred_size(config, context.size, (w, h), preset);
                if w >= config.min_width && h >= config.min_height {
                    (width.min(w), height.min(h))
                } else {
                    (width, height)
                }
            });
            let (width, height) = if target.0 <= w && target.1 <= h { target } else { (w, h) };
            (width, height, Some(target))
        }
    };
    let (rows, cols) = context.size;
    let row = if position == Position::Above { context.known_from } else { rows - height };
    let frame = Rect { row, col: cols - width, width, height };
    debug_assert!(!overlaps(frame, context.completion));
    Some(Placement { frame, position, inline: false, preferred })
}

pub fn inline_at(config: &Config, ctx: &Context, position: Position) -> Option<Placement> {
    let (rows, cols) = ctx.size;
    let list = ctx.completion;
    if list.col >= cols || end(list) > rows || right(list) > cols {
        return None;
    }
    let bands = [ctx.known_from..ctx.prompt.start.min(rows), ctx.known_from.max(ctx.prompt.end)..rows];
    for band in bands {
        if band.start > list.row || end(list) > band.end {
            continue;
        }
        let available = match position {
            Position::Right => (cols - right(list), band.end - list.row),
            Position::Left => (list.col, band.end - list.row),
            Position::Above => (cols, list.row - band.start),
            Position::Below => (cols, band.end - end(list)),
            Position::Auto => unreachable!(),
        };
        let (width, height) = preferred_size(config, ctx.size, available, FloatingSize::Configured);
        let (width, height) = (width.min(available.0), height.min(available.1));
        if width < config.min_width || height < config.min_height {
            continue;
        }
        let (row, col) = match position {
            Position::Right => (list.row, right(list)),
            Position::Left => (list.row, list.col - width),
            Position::Above => (list.row - height, list.col.min(cols - width)),
            Position::Below => (end(list), list.col.min(cols - width)),
            Position::Auto => unreachable!(),
        };
        let frame = Rect { row, col, width, height };
        return Some(Placement { frame, position, inline: true, preferred: None });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(row: u16, col: u16, height: u16, width: u16) -> Rect {
        Rect { row, col, height, width }
    }

    #[test]
    fn representative_viewports_fit_without_covering_prompt_or_list() {
        let config = Config::default();
        let cases = [
            ((24, 80), rect(4, 3, 10, 33), 0..4, Some(Position::Right)),
            ((40, 120), rect(12, 78, 8, 42), 2..12, Some(Position::Left)),
            ((50, 160), rect(36, 2, 12, 57), 4..36, Some(Position::Right)),
            ((24, 22), rect(5, 0, 10, 22), 0..5, Some(Position::Below)),
            ((5, 120), rect(3, 0, 2, 20), 0..3, Some(Position::Below)),
            ((24, 62), rect(15, 0, 6, 42), 21..24, Some(Position::Above)),
            ((24, 80), rect(15, 0, 5, 75), 20..23, Some(Position::Above)),
            ((40, 40), rect(7, 0, 8, 40), 2..7, Some(Position::Below)),
            ((40, 80), rect(5, 1, 8, 70), 2..5, Some(Position::Below)),
        ];
        for ((rows, cols), completion, prompt, chosen) in cases {
            let ctx = Context { size: (rows, cols), completion, known_from: 0, prompt: prompt.clone() };
            let found = place(&config, &ctx, None, config.layout, Floating::default());
            assert_eq!(found.map(|p| p.position), chosen, "{rows}x{cols} {completion:?}");
            if let Some(result) = found {
                assert!(!overlaps(result.frame, completion));
                assert!(!overlaps(result.frame, rect(prompt.start, 0, prompt.end - prompt.start, cols)));
                assert!(right(result.frame) <= cols && end(result.frame) <= rows);
                assert!(result.content().width + 2 * u16::from(result.framed()) == result.frame.width);
                assert_eq!(result.fit((u16::MAX, u16::MAX)), result);
            }
        }
    }

    #[test]
    fn corner_switches_for_more_area_and_keeps_previous_corner_at_a_tie() {
        let config = Config { layout: Layout::Floating, position: Position::Left, ..Config::default() };
        let ctx = Context { size: (40, 80), completion: rect(13, 0, 18, 56), known_from: 0, prompt: 9..12 };
        let solve = |context: &Context, previous| {
            place(&config, context, previous, config.layout, Floating::default()).unwrap()
        };
        let tied = solve(&ctx, None);
        assert_eq!(tied.frame, rect(0, 48, 8, 32));
        assert_eq!(tied.fit((2, 2)).frame, rect(0, 76, 4, 4));

        let taller = Context { size: (41, 80), ..ctx.clone() };
        let bottom = solve(&taller, Some(tied));
        assert_eq!(bottom.frame, rect(32, 48, 9, 32));
        assert_eq!(bottom.fit((2, 2)).frame, rect(37, 76, 4, 4));
        assert_eq!(solve(&ctx, Some(bottom)).frame, rect(32, 48, 8, 32));
        assert_eq!(solve(&Context { size: (39, 80), ..ctx }, Some(bottom)).position, Position::Above);
    }

    #[test]
    fn floating_never_uses_unknown_top_rows_or_pushes_the_list() {
        let config = Config::default();
        let ctx = Context { size: (25, 80), completion: rect(21, 0, 4, 60), known_from: 2, prompt: 19..21 };
        let top = place(&config, &ctx, None, Layout::Floating, Floating::default()).unwrap();
        assert_eq!(top.frame, rect(2, 48, 15, 32));
        let known = Context { known_from: 0, ..ctx };
        let top = place(&config, &known, None, Layout::Floating, Floating::default()).unwrap();
        assert_eq!(top.frame, rect(0, 48, 15, 32));
    }

    #[test]
    fn auto_floating_fallback_uses_the_same_corner_geometry() {
        let config = Config { position: Position::Right, ..Config::default() };
        let ctx = Context { size: (30, 80), completion: rect(2, 0, 10, 80), known_from: 0, prompt: 0..2 };
        let auto = place(&config, &ctx, None, Layout::Auto, Floating::default()).unwrap();
        assert_eq!(auto.frame, rect(13, 48, 17, 32));
        assert_eq!(Some(auto), place(&config, &ctx, None, Layout::Floating, Floating::default()));
    }

    #[test]
    fn floating_presets_use_both_dimensions_of_the_free_region() {
        let config = Config { layout: Layout::Floating, position: Position::Right, ..Config::default() };
        let ctx = Context { size: (40, 120), completion: rect(8, 10, 12, 34), known_from: 0, prompt: 0..8 };
        let mut size = FloatingSize::Configured;
        for (width, height) in [(48, 24), (50, 20), (68, 28), (48, 24)] {
            let placement =
                place(&config, &ctx, None, Layout::Floating, Floating::Image { preset: size, preferred: None })
                    .unwrap();
            assert_eq!(placement.frame, rect(40 - height, 120 - width, height, width));
            assert_eq!(placement.position, Position::Below);
            assert!(!overlaps(placement.frame, ctx.completion));
            size = size.next();
        }
        let inline = place(
            &config,
            &ctx,
            None,
            Layout::Inline,
            Floating::Image { preset: FloatingSize::Large, preferred: None },
        )
        .unwrap();
        assert_eq!(inline.frame, rect(8, 44, 24, 48));
    }

    #[test]
    fn explicit_inline_never_stacks() {
        let config = Config { position: Position::Below, ..Config::default() };
        let ctx = Context { size: (40, 80), completion: rect(7, 0, 8, 74), known_from: 0, prompt: 0..7 };
        assert!(place(&config, &ctx, None, Layout::Inline, Floating::default()).is_none());
        assert_eq!(
            place(&config, &ctx, None, Layout::Floating, Floating::default()).unwrap().position,
            Position::Below
        );
    }

    #[test]
    fn remembered_size_moves_to_the_other_corner_then_recovers_after_a_tight_viewport() {
        let config = Config::default();
        let bottom = Context { size: (40, 100), completion: rect(2, 0, 8, 100), known_from: 0, prompt: 0..2 };
        let preset = FloatingSize::Large;
        let large =
            place(&config, &bottom, None, Layout::Floating, Floating::Image { preset, preferred: None }).unwrap();
        assert_eq!(large.frame, rect(14, 10, 26, 90));

        let near_end = Context { size: (30, 80), completion: rect(20, 0, 8, 80), known_from: 4, prompt: 28..30 };
        let size = Floating::Image { preset, preferred: large.preferred };
        let constrained = place(&config, &near_end, None, Layout::Floating, size).unwrap();
        assert_eq!(constrained.frame, rect(4, 0, 15, 80));
        assert_eq!(constrained.fit((6, 1)).frame, rect(4, 72, 3, 8));
        assert_eq!(constrained.preferred, large.preferred);
        assert_eq!(place(&config, &bottom, None, Layout::Floating, size), Some(large));
    }

    #[test]
    fn larger_half_takes_priority_over_a_smaller_half_that_fits_the_remembered_size() {
        let config = Config::default();
        let ctx = Context { size: (45, 100), completion: rect(35, 0, 7, 50), known_from: 28, prompt: 34..35 };
        let size = Floating::Image { preset: FloatingSize::Large, preferred: Some((32, 8)) };
        let visible = place(&config, &ctx, None, Layout::Floating, size).unwrap();
        assert_eq!(visible.frame, rect(28, 0, 5, 100));

        let ctx = Context { known_from: 34, ..ctx };
        let visible = place(&config, &ctx, None, Layout::Floating, size).unwrap();
        assert_eq!(visible.position, Position::Below);
        assert_eq!(visible.frame, rect(37, 68, 8, 32));
    }

    #[test]
    fn tiny_floating_area_uses_safe_cells_without_a_border() {
        let config = Config::default();
        let ctx = Context { size: (6, 20), completion: rect(2, 0, 3, 20), known_from: 1, prompt: 1..2 };
        let pane = place(&config, &ctx, None, Layout::Floating, Floating::default()).unwrap();
        assert_eq!(pane.frame, rect(5, 0, 1, 20));
        assert!(!pane.framed());
        assert_eq!(pane.content(), pane.frame);
        assert!(
            place(&config, &Context { size: (5, 20), ..ctx }, None, Layout::Floating, Floating::default()).is_none()
        );
    }

    #[test]
    fn tight_floating_frames_stay_anchored_to_the_corner() {
        let placement = Placement {
            frame: rect(3, 20, 12, 60),
            position: Position::Below,
            inline: false,
            preferred: Some((60, 12)),
        };
        let portrait = placement.fit((4, 10));
        assert_eq!(portrait.frame, rect(3, 74, 12, 6));
        assert_eq!(portrait.preferred, placement.preferred);
        assert_eq!(placement.fit((58, 3)).frame, rect(10, 20, 5, 60));
    }

    #[test]
    fn intersection_excludes_touching_edges() {
        assert!(!overlaps(rect(0, 0, 3, 3), rect(0, 3, 3, 3)));
        assert!(overlaps(rect(0, 0, 3, 3), rect(2, 2, 3, 3)));
    }
}
