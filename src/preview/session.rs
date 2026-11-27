//! Adapter for Session's input, selection and VT screen ownership.
use std::sync::Arc;

use super::*;

fn animation_frame_at(elapsed: Duration, duration_ms: u64) -> u8 {
    if duration_ms == 0 { 4 } else { (elapsed.as_millis() * 4 / u128::from(duration_ms)).min(4) as u8 }
}
use crate::preview::{
    self, Document, Request, Worker,
    config::{ImageProtocol, Layout, ShowOn},
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

pub(super) struct State {
    sender: Sender<Event>,
    worker: Option<Worker>,
    unavailable: bool,
    bindings: Vec<(Key, Input)>,
    pub(super) cwd: PathBuf,
    pub(super) input_tail: String,
    display: bool,
    mode: Layout,
    image_size: FloatingSize,
    image_preferred: Option<(u16, u16)>,
    expanded: bool,
    zoom_target: Option<(usize, PathBuf)>,
    selection: Option<usize>,
    hover_target: Option<(usize, PathBuf)>,
    hover_at: Option<Instant>,
    suppressed_selection: Option<(usize, PathBuf)>,
    request: Option<Request>,
    pending_id: u64,
    refresh_at: Option<Instant>,
    doc: Option<Arc<Document>>,
    title: String,
    title_icon: Option<(String, &'static str)>,
    loading_since: Option<Instant>,
    loading_frame: u8,
    animation_target: Option<(usize, PathBuf)>,
    animation_started: Option<Instant>,
    animation_frame: u8,
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
            display: config.show_on == ShowOn::Always,
            mode: config.layout,
            image_size: FloatingSize::default(),
            image_preferred: None,
            expanded: false,
            zoom_target: None,
            selection: None,
            hover_target: None,
            hover_at: None,
            suppressed_selection: None,
            request: None,
            pending_id: 0,
            refresh_at: None,
            doc: None,
            title: String::new(),
            title_icon: None,
            loading_since: None,
            loading_frame: 0,
            animation_target: None,
            animation_started: None,
            animation_frame: 4,
            scroll: 0,
            placement: None,
            delta_placement: None,
            drawn: None,
            paint: false,
            colors: render::Colors::terminal(),
            graphics: Graphics::new(config.image.image_protocol.unwrap_or_default()),
            mouse: mouse::ModeGuard::default(),
            framer: mouse::Framer::default(),
            capture_until: None,
        }
    }

    fn loading_wake(&self) -> Option<Instant> {
        let since = self.loading_since?;
        Some(since + Duration::from_millis(u64::from(self.loading_frame + 1) * 120))
    }

    fn animation_step(&self) -> u8 {
        self.animation_frame
    }

    fn animation_wake(&self, effect: preview::config::Animation, duration_ms: u64) -> Option<Instant> {
        if effect == preview::config::Animation::None || duration_ms == 0 {
            return None;
        }
        let start = self.animation_started?;
        let next = u64::from(self.animation_frame + 1);
        (self.animation_frame < 4).then_some(start + Duration::from_millis((next * duration_ms).div_ceil(4)))
    }

    fn reset_zoom(&mut self, target: Option<(usize, PathBuf)>) {
        self.zoom_target = target;
        self.expanded = false;
        self.image_size = FloatingSize::default();
        self.image_preferred = None;
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
        self.loading_since = None;
        self.loading_frame = 0;
        self.animation_target = None;
        self.animation_started = None;
        self.animation_frame = 4;
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
            self.loading_since = Some(Instant::now());
            self.loading_frame = 0;
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
        let placement = self.placement?;
        let r = placement.content();
        let doc = self.visible_document()?;
        // Leave the border as spare space for native image protocols that advance the cursor.
        let style = if placement.framed() && doc.bitmap().is_some() && self.graphics.protocol().is_some() {
            render::ImageStyle::Bitmap
        } else if config.image.image_protocol.unwrap_or_default() == ImageProtocol::HalfBlocks {
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
        if !config.adaptive_size || (self.expanded && !placement.inline && doc.is_some_and(|doc| doc.file_text)) {
            return Some(placement);
        }
        let (width, height) =
            doc.map_or((16, 1), |doc| (doc.width(ui), doc.content.lines().min(usize::from(u16::MAX)) as u16));
        Some(placement.fit((width, if self.expanded && !placement.inline { u16::MAX } else { height })))
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

    fn prompt(
        &self,
        screen: &vt100::Screen,
        prompt_top: Option<u16>,
        fallback_top: Option<u16>,
    ) -> std::ops::Range<u16> {
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

        let start = prompt_top.filter(|&start| start <= input_top);
        start.unwrap_or(0)..end
    }
}

impl Session {
    pub(super) fn preview_selection_changed(&mut self) {
        if let Some(preview) = &mut self.preview {
            preview.reset_zoom(None);
            preview.selection = None;
        }
    }

    pub(super) fn preview_wake(&self) -> Option<Instant> {
        let preview = self.preview.as_ref()?;
        [
            preview.framer.wake(),
            preview.graphics.wake(),
            preview.refresh_at,
            preview.hover_at,
            preview.loading_wake(),
            preview.animation_wake(self.cfg.preview.animation, self.cfg.preview.animation_duration_ms),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(super) fn preview_tick(&mut self) {
        let now = Instant::now();
        let auto_due = self.preview.as_ref().is_some_and(|p| p.hover_at.is_some_and(|at| at <= now));
        if auto_due {
            let target = self.preview.as_ref().and_then(|p| p.hover_target.as_ref());
            let still_selected = target.is_some_and(|(index, path)| {
                *index == self.selected
                    && self.items.get(*index).is_some_and(target::file_candidate)
                    && self
                        .items
                        .get(*index)
                        .and_then(|item| target::file_from_candidate(&self.line, item, &self.preview.as_ref()?.cwd))
                        == Some(path.clone())
            });
            if self.popup_open() && self.resync.is_none() && still_selected {
                if let Some(preview) = &mut self.preview {
                    preview.display = true;
                    preview.hover_at = None;
                    preview.paint = true;
                    preview.animation_started = Some(now);
                    preview.animation_frame = 0;
                    preview.animation_target = preview.hover_target.clone();
                    self.mark_dirty();
                }
            } else if let Some(preview) = &mut self.preview {
                preview.hover_at = None;
            }
        }
        let mut needs_redraw = false;
        if let Some(preview) = &mut self.preview {
            if let Some(since) = preview.loading_since {
                let frame = (since.elapsed().as_millis() / 120).min(u128::from(u8::MAX)) as u8;
                if frame > preview.loading_frame {
                    preview.loading_frame = frame;
                    needs_redraw = true;
                }
            }
            if self.cfg.preview.animation != preview::config::Animation::None
                && self.cfg.preview.animation_duration_ms > 0
                && let Some(started) = preview.animation_started
            {
                let frame = animation_frame_at(started.elapsed(), self.cfg.preview.animation_duration_ms);
                if frame > preview.animation_frame {
                    preview.animation_frame = frame;
                    needs_redraw = true;
                }
            }
            if needs_redraw {
                preview.paint = true;
            }
            let mut out = Vec::new();
            let (keys, redraw) = preview.graphics.tick(&mut out);
            preview.paint |= redraw;
            if redraw {
                needs_redraw = true;
            }
            if !out.is_empty() {
                self.write_out(&out);
            }
            if !keys.is_empty() {
                self.on_preview_input(&keys);
            }
        }
        if needs_redraw {
            self.mark_dirty();
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
                let Some(item) = self.items.get(self.selected).filter(|item| target::file_candidate(item)) else {
                    return false;
                };
                let current_key =
                    target::file_from_candidate(&self.line, item, &preview.cwd).map(|path| (self.selected, path));
                match action {
                    Input::Toggle => {
                        preview.display = !preview.display;
                        if preview.display {
                            preview.suppressed_selection = None;
                            preview.animation_target = current_key.clone();
                            preview.animation_started = Some(Instant::now());
                            preview.animation_frame = 0;
                        } else {
                            preview.suppressed_selection = current_key.clone();
                        }
                        preview.hover_at = None;
                    }
                    Input::Open => {
                        preview.display = true;
                        preview.suppressed_selection = None;
                        preview.hover_at = None;
                        preview.animation_target = current_key.clone();
                        preview.animation_started = Some(Instant::now());
                        preview.animation_frame = 0;
                    }
                    Input::Close if preview.display => {
                        preview.display = false;
                        preview.suppressed_selection = current_key.clone();
                        preview.hover_at = None;
                    }
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
                let path = self
                    .items
                    .get(self.selected)
                    .and_then(|item| target::file_from_candidate(&self.line, item, &preview.cwd));
                if path.as_ref() != preview.zoom_target.as_ref().map(|(_, path)| path) {
                    return false;
                }
                let Some(doc) = &preview.doc else { return true };
                if doc.image_size().is_some() {
                    preview.image_size = preview.image_size.next();
                    preview.image_preferred = None;
                } else {
                    preview.expanded = !preview.expanded;
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
                preview.loading_since = None;
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
        let closed = !self.drawable() || self.remote.is_some() || self.history_mode;
        let Some(preview) = self.preview.as_mut() else { return };
        let Some(original) = list.as_mut().filter(|_| !closed) else {
            preview.cancel();
            preview.hover_target = None;
            preview.hover_at = None;
            preview.graphics.end_popup();
            if closed {
                preview.reset_zoom(None);
                preview.suppressed_selection = None;
                preview.display = self.cfg.preview.show_on == ShowOn::Always;
            }
            return;
        };
        if !fresh || preview.unavailable {
            preview.cancel();
            preview.hover_target = None;
            preview.hover_at = None;
            return;
        }
        let Some(item) = items.get(self.selected).filter(|item| target::file_candidate(item)) else {
            preview.cancel();
            preview.reset_zoom(None);
            preview.hover_target = None;
            preview.hover_at = None;
            preview.suppressed_selection = None;
            preview.display = self.cfg.preview.show_on == ShowOn::Always;
            return;
        };
        let Some(path) = target::file_from_candidate(&self.line, item, &preview.cwd) else {
            preview.cancel();
            preview.reset_zoom(None);
            preview.hover_target = None;
            preview.hover_at = None;
            preview.suppressed_selection = None;
            preview.display = self.cfg.preview.show_on == ShowOn::Always;
            return;
        };
        let key = (self.selected, path.clone());
        if preview.zoom_target.as_ref() != Some(&key) {
            preview.reset_zoom(Some(key.clone()));
        }
        let (glyph, color) =
            crate::icons::glyph(item.kind, item.icon, &item.label, crate::icons::use_nerd(&self.cfg.ui.icons));
        preview.title_icon = (self.cfg.ui.icons != "none").then(|| (glyph.to_owned(), color));
        let changed = preview.animation_target.as_ref().is_some_and(|current| current != &key);
        if changed {
            preview.hover_at = None;
            preview.hover_target = None;
            match self.cfg.preview.show_on {
                ShowOn::Always => {
                    preview.display = true;
                    preview.suppressed_selection = None;
                }
                ShowOn::KeyToggle | ShowOn::OnHover => preview.display = false,
            }
        }
        if preview.suppressed_selection.as_ref() != Some(&key) {
            preview.suppressed_selection = None;
        }
        if self.cfg.preview.show_on == ShowOn::Always && preview.suppressed_selection.is_none() {
            preview.display = true;
        }
        if !preview.display {
            if self.cfg.preview.show_on == ShowOn::OnHover && preview.suppressed_selection.is_none() {
                if preview.hover_target.as_ref() != Some(&key) {
                    preview.hover_target = Some(key.clone());
                    preview.hover_at =
                        Some(Instant::now() + Duration::from_millis(self.cfg.preview.on_hover_open_delay));
                }
            } else {
                preview.hover_target = None;
                preview.hover_at = None;
            }
            preview.cancel();
            return;
        }
        preview.hover_target = None;
        preview.hover_at = None;
        if preview.animation_target.as_ref() != Some(&key) {
            preview.animation_target = Some(key.clone());
            preview.animation_started = Some(Instant::now());
            preview.animation_frame = 0;
        }
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
        let expanded_text = preview.expanded && doc.is_some_and(|doc| doc.file_text);
        let ordinary = if expanded_text {
            Floating::ExpandedText
        } else if preview.expanded {
            Floating::Tall { content_width: u16::MAX }
        } else {
            Floating::Compact
        };
        let image = doc.is_some_and(|doc| doc.image_size().is_some());
        let floating = if image {
            Floating::Image { preset: preview.image_size, preferred: preview.image_preferred }
        } else {
            ordinary
        };
        let wide = preview.expanded && doc.is_some_and(|doc| doc.git_diff.is_some());
        let tall_width = doc
            .filter(|doc| {
                config.adaptive_size && preview.expanded && !expanded_text && !image && doc.git_diff.is_none()
            })
            .map(|doc| doc.width(&self.cfg.ui));
        let screen = self.parser.screen();
        let mut context = layout::Context {
            size: screen.size(),
            completion: original.rect,
            known_from,
            prompt: preview.prompt(screen, self.screen_tracking.prompt_top, self.prompt_end.as_ref().map(|p| p.row)),
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
                known_from,
                token.width().min(usize::from(u16::MAX)) as u16,
            )
        };
        let mut replacement = None;
        let list_end = original.rect.row.saturating_add(original.rect.height);
        if original.rect.row < prompt.end && list_end > prompt.start {
            // Upstream knows about wrapped rows before the cursor. When showing a
            // preview, also keep the list clear of a multiline prompt or buffer tail.
            let Some(safe) = layout_list(&self.cfg.ui) else {
                preview.cancel();
                if self.screen_tracking.prompt_top.is_some() {
                    *list = None;
                }
                return;
            };
            context.completion = safe.rect;
            *original = safe;
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
                for side in layout::inline_sides() {
                    let width = match side {
                        layout::Position::Right => {
                            size.1.saturating_sub(narrow.rect.col.saturating_add(config.min_width))
                        }
                        layout::Position::Left if narrow.rect.col >= config.min_width => size.1 - config.min_width,
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
                render::Options {
                    colors: preview.colors,
                    image,
                    title_icon: self.cfg.preview.show_title.then(|| preview.title_icon.clone()).flatten(),
                    loading_frame: preview.loading_frame,
                    animation: (self.cfg.preview.animation != preview::config::Animation::None
                        && self.cfg.preview.animation_duration_ms > 0
                        && preview.animation_step() < 4)
                        .then_some((self.cfg.preview.animation, preview.animation_step())),
                },
            );
            if let Some((render::ImageStyle::Bitmap, dimensions)) = image
                && (self.cfg.preview.animation == preview::config::Animation::None
                    || self.cfg.preview.animation_duration_ms == 0
                    || preview.animation_step() >= 4)
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

    pub(super) fn preview_shift(&mut self, shift: i32, rows: u16) {
        if let Some(preview) = &mut self.preview {
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
    use super::animation_frame_at;
    use std::time::Duration;

    #[test]
    fn animation_duration_controls_the_four_transition_frames() {
        assert_eq!(animation_frame_at(Duration::ZERO, 300), 0);
        assert_eq!(animation_frame_at(Duration::from_millis(75), 300), 1);
        assert_eq!(animation_frame_at(Duration::from_millis(300), 300), 4);
        assert_eq!(animation_frame_at(Duration::ZERO, 0), 4);
    }
}
