//! Adapter for Session's input, selection and VT screen ownership.
use std::sync::Arc;

use super::*;
use crate::preview::{
    self, Document, Request, Worker,
    config::{ImageProtocol, Layout, Position},
    graphics::Graphics,
    layout::{self, Floating, FloatingSize, Placement},
    mouse, render, target,
};

/// Bound the history used to account for full-screen scrolling after a prompt begins.
pub(super) const PROMPT_HISTORY: usize = 256;
const REFRESH: Duration = Duration::from_millis(500);

#[cfg(unix)]
pub(super) fn watch_shutdown(sender: Sender<Event>) -> std::io::Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([libc::SIGTERM, libc::SIGHUP, libc::SIGINT, libc::SIGQUIT])?;
    std::thread::Builder::new().name("lig-preview-signals".into()).spawn(move || {
        if let Some(signal) = signals.forever().next() {
            let _ = sender.send(Event::Shutdown(signal));
        }
    })?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn terminate_shell(pid: Option<u32>) {
    if let Some(pid) = pid {
        // SAFETY: Lig owns the shell child in its private PTY; the child has not been reaped.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
            libc::kill(pid as i32, libc::SIGKILL);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Input {
    Toggle,
    ToggleLayout,
    ResizeFloating,
    Open,
    Close,
    Up,
    Down,
    PageUp,
    PageDown,
}

struct ScreenClear {
    alternate: bool,
    normal_erased: bool,
}

impl vte::Perform for ScreenClear {
    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        match (intermediates, action) {
            ([], 'J') if !self.alternate && params.len() == 1 && params.iter().next() == Some(&[2]) => {
                self.normal_erased = true;
            }
            ([b'?'], 'h' | 'l') if params.iter().any(|arg| matches!(arg, [47] | [1047] | [1049])) => {
                self.alternate = action == 'h'
            }
            _ => {}
        }
    }
}

pub(super) struct State {
    sender: Sender<Event>,
    worker: Option<Worker>,
    unavailable: bool,
    bindings: Vec<(Key, Input)>,
    pub(super) cwd: PathBuf,
    pub(super) input_tail: String,
    prompt_top: Option<u16>,
    screen_clear: vte::Parser,
    screen_alternate: bool,
    display: bool,
    mode: Layout,
    image_size: FloatingSize,
    image_preferred: Option<(u16, u16)>,
    tall: bool,
    selection: Option<usize>,
    request: Option<Request>,
    pending_id: u64,
    refresh_at: Option<Instant>,
    doc: Option<Arc<Document>>,
    title: String,
    scroll: usize,
    // Available areas for rendering. viewport() fits the frame for painting.
    placement: Option<Placement>,
    delta_placement: Option<Placement>,
    drawn: Option<Placement>,
    paint: bool,
    colors: render::Colors,
    graphics: Graphics,
    mouse: mouse::ModeGuard,
    framer: mouse::Framer,
    capture_until: Option<Instant>,
}

impl State {
    pub(super) fn new(config: &preview::Config, completion: &Bindings, sender: Sender<Event>) -> Self {
        let bindings = [
            (config.keys.toggle.as_str(), Input::Toggle),
            (config.keys.toggle_layout.as_deref().unwrap_or_default(), Input::ToggleLayout),
            (config.keys.resize_floating.as_deref().unwrap_or_default(), Input::ResizeFloating),
            (config.keys.open.as_str(), Input::Open),
            (config.keys.close.as_str(), Input::Close),
            (config.keys.up.as_str(), Input::Up),
            (config.keys.down.as_str(), Input::Down),
            (config.keys.page_up.as_str(), Input::PageUp),
            (config.keys.page_down.as_str(), Input::PageDown),
        ]
        .into_iter()
        .filter_map(|(key, action)| {
            let key = Key::parse_name(key)?;
            completion.action(&key).is_none().then_some((key, action))
        })
        .collect();
        Self {
            sender,
            worker: None,
            unavailable: false,
            bindings,
            cwd: PathBuf::new(),
            input_tail: String::new(),
            prompt_top: None,
            screen_clear: vte::Parser::new(),
            screen_alternate: false,
            display: !config.start_hidden,
            mode: config.layout,
            image_size: FloatingSize::default(),
            image_preferred: None,
            tall: false,
            selection: None,
            request: None,
            pending_id: 0,
            refresh_at: None,
            doc: None,
            title: String::new(),
            scroll: 0,
            placement: None,
            delta_placement: None,
            drawn: None,
            paint: false,
            colors: render::Colors::terminal(),
            graphics: Graphics::new(config.image_protocol),
            mouse: mouse::ModeGuard::default(),
            framer: mouse::Framer::default(),
            capture_until: None,
        }
    }

    pub(super) fn mark_prompt(&mut self, screen: &mut vt100::Screen) {
        if !screen.alternate_screen() {
            self.prompt_top = Some(screen.cursor_position().0);
        }
    }

    pub(super) fn reset_prompt(&mut self) {
        self.prompt_top = None;
        self.input_tail.clear();
    }

    fn cancel(&mut self) {
        if self.request.take().is_some()
            && let Some(worker) = &self.worker
        {
            self.pending_id = worker.cancel();
        }
        self.doc = None;
        self.refresh_at = None;
        self.scroll = 0;
        self.placement = None;
        self.delta_placement = None;
        self.selection = None;
    }

    fn request(&mut self, request: Request, config: &preview::Config) {
        if self.request.as_ref() == Some(&request) || self.unavailable {
            return;
        }
        if self.worker.is_none() {
            let sender = self.sender.clone();
            match Worker::start(config.clone(), move |id, result| {
                let _ = sender.send(Event::PreviewReady(id, result));
            }) {
                Ok(worker) => self.worker = Some(worker),
                Err(err) => {
                    crate::log(&format!("cannot start preview worker: {err}"));
                    self.unavailable = true;
                    return;
                }
            }
        }
        let same_file = self.request.as_ref().is_some_and(|old| old.path == request.path);
        let same_source = self.request.as_ref().is_some_and(|old| {
            old.path == request.path
                && old.cwd == request.cwd
                && old.git == request.git
                && old.search_path == request.search_path
        });
        self.pending_id = self.worker.as_ref().unwrap().request(request.clone());
        self.title = render::title(&request.path);
        self.request = Some(request);
        if !same_file {
            self.scroll = 0;
        }
        if !same_source {
            self.doc = None;
        }
        self.refresh_at = None;
        self.paint = true;
    }

    fn visible_document(&self) -> Option<&Document> {
        self.doc
            .as_deref()
            .filter(|doc| doc.git_diff != Some(preview::git::Diff::SideBySide) || self.delta_placement.is_some())
    }

    fn image_geometry(&self, config: &preview::Config) -> Option<(render::ImageStyle, (u32, u32))> {
        let r = self.placement?.content();
        let doc = self.visible_document()?;
        let style = if doc.bitmap().is_some() && self.graphics.protocol().is_some() {
            render::ImageStyle::Bitmap
        } else if config.image_protocol == ImageProtocol::HalfBlocks {
            render::ImageStyle::HalfBlocks
        } else {
            render::ImageStyle::Quadrants
        };
        let dimensions =
            preview::raster::fit_cells(doc.image_size()?, self.graphics.cells(), (r.width, r.height), style.samples());
        Some((style, dimensions))
    }

    fn viewport(&self, config: &preview::Config, ui: &crate::config::Ui) -> Option<Placement> {
        let placement = self.placement?;
        self.request.as_ref()?;
        if let Some((style, (w, h))) = self.image_geometry(config) {
            let (sx, sy) = style.samples();
            return Some(if config.adaptive_size || !placement.inline {
                let height = if config.adaptive_size { h.div_ceil(sy) as u16 } else { u16::MAX };
                placement.fit((w.div_ceil(sx) as u16, height))
            } else {
                placement
            });
        }
        let doc = self.visible_document();
        let placement = if doc.is_some_and(|doc| doc.git_diff == Some(preview::git::Diff::SideBySide)) {
            self.delta_placement.unwrap_or(placement)
        } else {
            placement
        };
        if !config.adaptive_size {
            return Some(placement);
        }
        let (width, height) =
            doc.map_or((16, 1), |doc| (doc.width(ui), doc.content.lines().min(usize::from(u16::MAX)) as u16));
        Some(placement.fit((width, if self.tall && !placement.inline { u16::MAX } else { height })))
    }

    fn scroll(&mut self, down: bool, page: bool, config: &preview::Config, ui: &crate::config::Ui) -> bool {
        let Some(viewport) = self.viewport(config, ui).filter(|placement| self.drawn == Some(*placement)) else {
            return false;
        };
        let Some(doc) = self.visible_document() else { return true };
        let height = usize::from(viewport.content().height);
        let step = if page { height.saturating_sub(1).max(1) } else { config.scroll_step };
        let scroll = if down {
            self.scroll.saturating_add(step).min(doc.content.lines().saturating_sub(height))
        } else {
            self.scroll.saturating_sub(step)
        };
        self.paint |= scroll != self.scroll;
        self.scroll = scroll;
        true
    }

    fn set_mouse(&mut self, enabled: bool, screen: &vt100::Screen, out: &mut Vec<u8>) {
        if !enabled && self.mouse.enabled() {
            self.capture_until = Some(Instant::now() + Duration::from_millis(100));
        }
        self.mouse.set(enabled, screen, out);
    }

    fn prompt(&mut self, screen: &mut vt100::Screen, fallback_top: Option<u16>) -> std::ops::Range<u16> {
        let (rows, cols) = screen.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        let mut input_top = cursor_row;
        while input_top > 0 && screen.row_wrapped(input_top - 1) {
            input_top -= 1;
        }
        if let Some(top) = fallback_top {
            input_top = input_top.min(top);
        }
        // The end of the left buffer alone cannot protect a multi-row edit when the
        // cursor is moved up. Use the full buffer when reported, plus occupied screen rows.
        let mut final_row = cursor_row;
        let mut col = cursor_col;
        for c in self.input_tail.chars() {
            if c == '\n' {
                final_row = final_row.saturating_add(1);
                col = 0;
            } else {
                let width =
                    if c == '\t' { 8 - col % 8 } else { unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) as u16 };
                if col.saturating_add(width) > cols {
                    final_row = final_row.saturating_add(1);
                    col = 0;
                }
                col = col.saturating_add(width);
            }
        }
        let mut end = final_row.saturating_add(1).min(rows);
        while end < rows
            && (screen.row_wrapped(end - 1)
                || (0..cols).any(|col| screen.cell(end, col).is_some_and(|cell| cell.has_contents())))
        {
            end += 1;
        }

        let start = self.prompt_top.filter(|&start| start <= input_top);
        start.unwrap_or(0)..end
    }
}

impl Session {
    pub(super) fn process_preview_output(&mut self, bytes: &[u8]) {
        let Some(preview) = self.preview.as_mut() else {
            self.parser.process(bytes);
            return;
        };
        if self.unknown_rows != 0 {
            let mut clear = ScreenClear {
                alternate: preview.screen_alternate || self.parser.screen().alternate_screen(),
                normal_erased: false,
            };
            preview.screen_clear.advance(&mut clear, bytes);
            preview.screen_alternate = clear.alternate;
            if clear.normal_erased {
                self.unknown_rows = 0;
            }
        }
        if preview.prompt_top.is_none() {
            self.parser.process(bytes);
            return;
        }
        // vt100 keeps an offset of one anchored when the screen scrolls, even after
        // the scrollback buffer is full. Restore the live view before using the model.
        let screen = self.parser.screen_mut();
        screen.set_scrollback(1);
        let base = screen.scrollback();
        self.parser.process(bytes);
        let screen = self.parser.screen_mut();
        if base == 0 {
            screen.set_scrollback(usize::MAX);
        }
        let after = screen.scrollback();
        screen.set_scrollback(0);
        preview.prompt_top = after.checked_sub(base).filter(|_| !screen.alternate_screen()).and_then(|scroll| {
            let row = preview.prompt_top?;
            Some(if after >= PROMPT_HISTORY { 0 } else { row.saturating_sub(scroll as u16) })
        });
    }

    pub(super) fn preview_wake(&self) -> Option<Instant> {
        let preview = self.preview.as_ref()?;
        [preview.framer.wake(), preview.graphics.wake(), preview.refresh_at].into_iter().flatten().min()
    }

    pub(super) fn preview_tick(&mut self) {
        if let Some(preview) = &mut self.preview {
            let mut out = Vec::new();
            let (keys, redraw) = preview.graphics.tick(&mut out);
            preview.paint |= redraw;
            if redraw {
                self.mark_dirty();
            }
            if !out.is_empty() {
                self.write_out(&out);
            }
            if !keys.is_empty() {
                self.on_preview_input(&keys);
            }
        }
        if let Some(bytes) = self.preview.as_mut().and_then(|p| p.framer.expired()) {
            self.on_input(&bytes);
        }
        let active = self.resync.is_none() && self.popup_open();
        if let Some(preview) = self.preview.as_mut().filter(|p| p.refresh_at.is_some_and(|at| at <= Instant::now())) {
            preview.refresh_at = None;
            if active && let (Some(worker), Some(request)) = (&preview.worker, &preview.request) {
                preview.pending_id = worker.request(request.clone());
            }
        }
    }

    pub(super) fn preview_terminal_input(&mut self, data: Vec<u8>) -> Vec<u8> {
        let Some(state) = &mut self.preview else { return data };
        let (keys, redraw) = state.graphics.input(data);
        if redraw {
            state.paint = true;
            self.mark_dirty();
        }
        keys
    }

    pub(super) fn on_preview_input(&mut self, data: &[u8]) {
        let Some(preview) = self.preview.as_mut() else { return self.on_input(data) };
        let capture = preview.mouse.enabled()
            || (self.parser.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None
                && preview.capture_until.is_some_and(|deadline| deadline > Instant::now()));
        if !capture && !preview.framer.has_pending() {
            return self.on_input(data);
        }
        let mut keyboard = Vec::new();
        for input in preview.framer.feed(data, capture) {
            match input {
                mouse::Input::Bytes(bytes) => keyboard.extend(bytes),
                mouse::Input::Mouse(report) => {
                    if !keyboard.is_empty() {
                        self.on_input(&std::mem::take(&mut keyboard));
                    }
                    if self.popup_open()
                        && let Some(preview) = self.preview.as_mut().filter(|p| p.mouse.enabled())
                        && let Some(down) = preview.drawn.and_then(|place| report.scroll(place.content()))
                        && preview.scroll(down, false, &self.cfg.preview, &self.cfg.ui)
                    {
                        self.mark_dirty();
                    }
                }
            }
        }
        if !keyboard.is_empty() {
            self.on_input(&keyboard);
        }
    }

    pub(super) fn preview_key(&mut self, key: &Key) -> bool {
        if self.bindings.action(key).is_some() || self.remote.is_some() || self.history_mode || self.resync.is_some() {
            return false;
        }
        let Some(preview) = self.preview.as_mut() else { return false };
        let Some(action) = preview.bindings.iter().find_map(|(binding, action)| (binding == key).then_some(*action))
        else {
            return false;
        };
        match action {
            action @ (Input::Toggle | Input::Open | Input::Close | Input::ToggleLayout) => {
                if !self.items.get(self.selected).is_some_and(target::file_candidate) {
                    return false;
                }
                match action {
                    Input::Toggle => preview.display = !preview.display,
                    Input::Open => preview.display = true,
                    Input::Close if preview.display => preview.display = false,
                    Input::Close => return false,
                    Input::ToggleLayout => {
                        let inline = match preview.mode {
                            Layout::Inline => true,
                            Layout::Floating => false,
                            Layout::Auto => preview.placement.is_none_or(|placement| placement.inline),
                        };
                        preview.mode = if inline { Layout::Floating } else { Layout::Inline };
                        preview.placement = None;
                    }
                    _ => unreachable!(),
                }
                true
            }
            Input::ResizeFloating => {
                if !preview.display
                    || preview.selection != Some(self.selected)
                    || preview.drawn.is_none_or(|placement| placement.inline)
                {
                    return false;
                }
                let Some(doc) = &preview.doc else { return true };
                if doc.image_size().is_some() {
                    preview.image_size = preview.image_size.next();
                    preview.image_preferred = None;
                } else {
                    preview.tall = !preview.tall;
                }
                true
            }
            _ if preview.selection != Some(self.selected) => false,
            _ => preview.scroll(
                matches!(action, Input::Down | Input::PageDown),
                matches!(action, Input::PageUp | Input::PageDown),
                &self.cfg.preview,
                &self.cfg.ui,
            ),
        }
    }

    pub(super) fn on_preview_ready(&mut self, id: u64, result: Arc<Document>) {
        if let Some(preview) = self.preview.as_mut().filter(|p| p.pending_id == id && p.request.is_some()) {
            let refresh = if preview.request.as_ref().is_some_and(|request| request.git.is_some()) {
                Duration::from_secs(2)
            } else {
                REFRESH
            };
            preview.refresh_at = Some(Instant::now() + refresh);
            if preview.doc.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &result)) {
                preview.doc = Some(result);
                preview.paint = true;
                self.mark_dirty();
            }
        }
    }

    pub(super) fn sync_preview(
        &mut self,
        list: &mut Option<popup::Layout>,
        items: &[Suggestion],
        fresh: bool,
        known_from: u16,
    ) {
        let Some(preview) = self.preview.as_mut() else { return };
        let Some(original) = list.as_ref().filter(|_| self.remote.is_none() && !self.history_mode) else {
            preview.cancel();
            preview.graphics.end_popup();
            preview.display = !self.cfg.preview.start_hidden;
            return;
        };
        if !fresh || !preview.display || preview.unavailable {
            preview.cancel();
            return;
        }
        let Some(path) =
            items.get(self.selected).and_then(|item| target::file_from_candidate(&self.line, item, &preview.cwd))
        else {
            preview.cancel();
            return;
        };
        let git = target::git_context(&self.line, &self.items_command, &self.env.aliases);
        let config = &self.cfg.preview;
        let doc = preview
            .request
            .as_ref()
            .filter(|request| {
                request.path == path
                    && request.git == git
                    && request.cwd == preview.cwd
                    && request.search_path == self.env.path
            })
            .and(preview.doc.as_deref());
        let ordinary = if preview.tall { Floating::Tall { content_width: u16::MAX } } else { Floating::Compact };
        let image = doc.is_some_and(|doc| doc.image_size().is_some());
        let floating = if image {
            Floating::Image { preset: preview.image_size, preferred: preview.image_preferred }
        } else {
            ordinary
        };
        let wide = preview.tall && doc.is_some_and(|doc| doc.git_diff.is_some());
        let tall_width = doc
            .filter(|doc| config.adaptive_size && preview.tall && !image && doc.git_diff.is_none())
            .map(|doc| doc.width(&self.cfg.ui));
        let screen = self.parser.screen_mut();
        let mut context = layout::Context {
            size: screen.size(),
            completion: original.rect,
            known_from,
            prompt: preview.prompt(screen, self.prompt_end.as_ref().map(|p| p.row)),
        };
        let cursor = screen.cursor_position();
        let prompt = context.prompt.clone();
        let size = context.size;
        let layout_list = |ui: &crate::config::Ui| {
            let token: String = self.line.chars().rev().take(items[0].delete).collect();
            popup::layout_at(
                ui,
                items,
                size,
                (prompt.end.saturating_sub(1).max(cursor.0), cursor.1),
                prompt.start,
                token.width().min(usize::from(u16::MAX)) as u16,
            )
            .filter(|l| l.rect.row >= known_from)
        };
        let mut replacement = None;
        let list_end = original.rect.row.saturating_add(original.rect.height);
        if original.rect.row < prompt.end && list_end > prompt.start {
            // Upstream knows about wrapped rows before the cursor. When showing a
            // preview, also keep the list clear of a multiline prompt or buffer tail.
            let Some(safe) = layout_list(&self.cfg.ui) else {
                preview.cancel();
                return;
            };
            context.completion = safe.rect;
            replacement = Some(safe);
        }
        let mut placement = layout::place(config, &context, preview.placement, preview.mode, floating);
        if preview.mode != Layout::Inline && placement.is_none_or(|p| !p.framed()) {
            let list = replacement.as_ref().unwrap_or(original);
            let above_prompt = list.rect.row + list.rect.height <= prompt.start;
            // Three rows suffice for a frame with one content row.
            let mut fallback = None;
            for rows in 1..=3 {
                let Some(compact) = list.compact_height(rows, above_prompt) else { break };
                let space = layout::Context { completion: compact.rect, ..context.clone() };
                if let Some(pane) = layout::place(config, &space, preview.placement, Layout::Floating, floating) {
                    let framed = pane.framed();
                    if framed || (placement.is_none() && fallback.is_none()) {
                        fallback = Some((compact, pane));
                    }
                    if framed {
                        break;
                    }
                }
            }
            if let Some((compact, pane)) = fallback {
                replacement = Some(compact);
                placement = Some(pane);
            }
        }
        if placement.is_none() && preview.mode == Layout::Inline && context.completion.width > 20 {
            let mut ui = self.cfg.ui.clone();
            ui.width = crate::config::Size::Cells(20);
            // A narrow layout exposes the normal candidate anchor without the
            // right-edge clamp imposed by a wide list.
            if let Some(narrow) = layout_list(&ui) {
                let mut best_width = 0;
                for side in layout::inline_sides(config.position) {
                    let width = match side {
                        Position::Right => size.1.saturating_sub(narrow.rect.col.saturating_add(config.min_width)),
                        Position::Left if narrow.rect.col >= config.min_width => size.1 - config.min_width,
                        _ => continue,
                    }
                    .min(context.completion.width);
                    if width < 20 || width <= best_width {
                        continue;
                    }
                    ui.width = crate::config::Size::Cells(width);
                    if let Some(compact) = layout_list(&ui)
                        && let Some(side) = layout::inline_at(
                            config,
                            &layout::Context { completion: compact.rect, ..context.clone() },
                            side,
                        )
                    {
                        best_width = compact.rect.width;
                        replacement = Some(compact);
                        placement = Some(side);
                    }
                }
            }
        }
        let Some(placement) = placement else {
            preview.cancel();
            return;
        };
        if let Some(target) = placement.preferred {
            preview.image_preferred = Some(target);
        }
        if let Some(replacement) = replacement {
            context.completion = replacement.rect;
            *list = Some(replacement);
        }
        let floating_place = |sizing| layout::place(config, &context, Some(placement), Layout::Floating, sizing);
        preview.delta_placement = if wide && !placement.inline {
            floating_place(Floating::Wide)
                .filter(|wide| wide.position == placement.position && wide.content().width >= layout::MIN_DIFF_COLUMNS)
        } else {
            None
        };
        // External output may depend on its viewport. Request a size based on
        // the terminal and configuration even when the drawn pane can fit more tightly.
        let allocated =
            if image && !placement.inline { floating_place(ordinary).unwrap_or(placement) } else { placement };
        let r = allocated.content();
        let placement = if !placement.inline
            && let Some(content_width) = tall_width
        {
            floating_place(Floating::Tall { content_width }).unwrap_or(placement)
        } else {
            placement
        };
        preview.request(
            Request {
                path,
                cwd: preview.cwd.clone(),
                search_path: self.env.path.clone(),
                viewport: (r.width, r.height),
                light: self.cfg.ui.theme == "light",
                date_locale: preview::locale::date_locale(&self.env.vars),
                show_hidden: self.cfg.behavior.show_hidden_files,
                git,
                git_width: preview.delta_placement.map(|place| place.content().width),
            },
            config,
        );
        preview.selection = Some(self.selected);
        preview.placement = Some(placement);
    }

    pub(super) fn erase_preview(&mut self, out: &mut Vec<u8>) {
        let Some(preview) = self.preview.as_mut() else { return };
        preview.set_mouse(false, self.parser.screen(), out);
        if let Some(placement) = preview.drawn.take() {
            preview.graphics.erase(out, self.parser.screen().size());
            self.begin_sync(out);
            popup::erase(out, self.parser.screen(), placement.frame);
            popup::restore_cursor(out, self.parser.screen());
        }
    }

    pub(super) fn reconcile_preview(&mut self, out: &mut Vec<u8>) {
        if let Some(state) = &self.preview
            && state.drawn.is_some()
            && state.drawn != state.viewport(&self.cfg.preview, &self.cfg.ui)
        {
            self.erase_preview(out);
        }
    }

    pub(super) fn draw_preview(&mut self, out: &mut Vec<u8>) {
        let Some(preview) = self.preview.as_mut() else { return };
        let Some(placement) = preview.viewport(&self.cfg.preview, &self.cfg.ui) else { return };
        if preview.paint || preview.drawn != Some(placement) {
            preview.graphics.erase(out, self.parser.screen().size());
            let height = usize::from(placement.content().height);
            if preview.doc.as_ref().is_some_and(|doc| doc.bitmap().is_some()) {
                preview.graphics.start(out);
            }
            let image = preview.image_geometry(&self.cfg.preview);
            if let Some(doc) = preview.visible_document() {
                preview.scroll = preview.scroll.min(doc.content.lines().saturating_sub(height));
            }
            render::draw(
                out,
                &self.cfg.ui,
                placement,
                preview.visible_document().map(|doc| &doc.content),
                if self.cfg.preview.show_title { &preview.title } else { "" },
                preview.scroll,
                render::Options { colors: preview.colors, image },
            );
            if let Some((render::ImageStyle::Bitmap, dimensions)) = image
                && let Some(payload) = preview.doc.as_ref().and_then(|doc| doc.bitmap())
            {
                preview.graphics.draw(out, placement.content(), dimensions, payload);
            }
            preview.drawn = Some(placement);
            preview.paint = false;
        }
        let mouse =
            self.cfg.preview.mouse && self.parser.screen().mouse_protocol_mode() == vt100::MouseProtocolMode::None;
        preview.set_mouse(mouse, self.parser.screen(), out);
    }

    pub(super) fn preview_resizing(&mut self, size: (u16, u16)) {
        let Some(preview) = self.preview.as_mut() else { return };
        let mut bytes = vec![];
        preview.set_mouse(false, self.parser.screen(), &mut bytes);
        preview.graphics.resize(&mut bytes, size);
        if let (Some(worker), Some(request)) = (&preview.worker, &mut preview.request) {
            preview.pending_id = worker.cancel();
            request.viewport = (0, 0);
            preview.refresh_at = None;
        }
        self.write_out(&bytes);
    }

    pub(super) fn preview_shift(&mut self, shift: i32, rows: u16, reflow: bool) {
        if let Some(preview) = &mut self.preview {
            if reflow || shift < 0 {
                preview.screen_clear = vte::Parser::new();
                preview.screen_alternate = self.parser.screen().alternate_screen();
            }
            preview.prompt_top = preview
                .prompt_top
                .filter(|_| !reflow)
                .map(|row| (i32::from(row) - shift).clamp(0, i32::from(rows) - 1) as u16);
            preview.drawn =
                preview.drawn.and_then(|p| shifted(p.frame, shift, rows).map(|frame| Placement { frame, ..p }));
            preview.paint = true;
        }
    }

    pub(super) fn preview_theme_changed(&mut self) {
        if let Some(preview) = &mut self.preview {
            preview.paint = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ScreenClear;

    #[test]
    fn only_a_real_full_clear_of_the_normal_screen_makes_its_rows_known() {
        let mut tracker = ScreenClear { alternate: false, normal_erased: false };
        let mut parser = vte::Parser::new();
        parser.advance(&mut tracker, b"\x1b[?1049h\x1b[2J\x1b[?1049l\x1b[0J\x1b[?2J");
        assert!(!tracker.normal_erased);
        parser.advance(&mut tracker, b"\x1b[2");
        assert!(!tracker.normal_erased);
        parser.advance(&mut tracker, b"J\x1b[37;1H");
        assert!(tracker.normal_erased);
    }
}
