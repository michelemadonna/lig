//! Owns the images and capability replies that belong to Lig's outer terminal.
use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};
use std::io::Write;
use std::time::{Duration, Instant};

use crate::popup::Rect;

use super::config::ImageProtocol;

const REPLY_WAIT: Duration = Duration::from_millis(250);
const ESCAPE_WAIT: Duration = Duration::from_millis(20);
const MAX_REPLY: usize = 256;
const KITTY: &[u8] = b"\x1b_G";
const ITERM: &[u8] = b"\x1b]1337;ReportCellSize=";
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Kitty,
    Iterm,
}

enum Probe {
    Idle,
    Tmux(std::sync::mpsc::Receiver<bool>, Instant),
    Waiting(Protocol, Instant),
    Done,
}

#[derive(Clone, Copy)]
enum Painted {
    Kitty(u32),
    Iterm(Rect),
}

pub struct Graphics {
    probe: Probe,
    tmux: bool,
    protocol: Option<Protocol>,
    cell_size: Option<(u32, u32)>,
    painted: Option<Painted>,
    probe_id: u32,
    next_id: u32,
    sent_ids: VecDeque<u32>,
    ack: Option<(u32, Instant)>,
    partial: Vec<u8>,
    partial_until: Option<Instant>,
    paste: bool,
    kitty_queried: bool,
    iterm_reply_until: Option<Instant>,
}

impl Graphics {
    pub fn new(mode: ImageProtocol) -> Self {
        let seed = RandomState::new().hash_one(std::process::id()) as u32;
        Self {
            probe: if mode == ImageProtocol::Auto { Probe::Idle } else { Probe::Done },
            tmux: mode == ImageProtocol::Auto && std::env::var_os("TMUX").is_some(),
            protocol: None,
            cell_size: tty_cells(),
            painted: None,
            probe_id: seed.max(1),
            next_id: seed.rotate_right(15).max(1),
            sent_ids: VecDeque::new(),
            ack: None,
            partial: Vec::new(),
            partial_until: None,
            paste: false,
            kitty_queried: false,
            iterm_reply_until: None,
        }
    }

    pub fn cells(&self) -> (u32, u32) {
        self.cell_size.unwrap_or((8, 16))
    }

    pub fn protocol(&self) -> Option<Protocol> {
        self.protocol
    }

    pub fn start(&mut self, out: &mut Vec<u8>) {
        if !matches!(self.probe, Probe::Idle) {
            return;
        }
        let term = std::env::var("TERM").unwrap_or_default();
        if term == "dumb" || std::env::var_os("STY").is_some() || std::env::var_os("ZELLIJ").is_some() {
            self.probe = Probe::Done;
        } else if self.tmux {
            self.probe = tmux_client().map_or(Probe::Done, |rx| Probe::Tmux(rx, Instant::now()));
        } else if term.starts_with("screen") || term.starts_with("tmux") {
            self.probe = Probe::Done;
        } else if std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "iTerm.app") {
            self.query(Protocol::Iterm, out);
        } else {
            self.query(Protocol::Kitty, out);
        }
    }

    fn query(&mut self, protocol: Protocol, out: &mut Vec<u8>) {
        self.probe = Probe::Waiting(protocol, Instant::now() + REPLY_WAIT);
        match protocol {
            Protocol::Kitty => {
                self.kitty_queried = true;
                let _ = write!(out, "\x1b_Gi={},s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\", self.probe_id);
            }
            Protocol::Iterm => {
                self.iterm_reply_until = Some(Instant::now() + REPLY_WAIT * 2);
                out.extend_from_slice(b"\x1b]1337;ReportCellSize\x1b\\");
            }
        }
    }

    pub fn wake(&self) -> Option<Instant> {
        let probe = match self.probe {
            Probe::Tmux(_, next) | Probe::Waiting(_, next) => Some(next),
            _ => None,
        };
        [probe, self.partial_until, self.ack.map(|(_, until)| until)].into_iter().flatten().min()
    }

    pub fn tick(&mut self, out: &mut Vec<u8>) -> (Vec<u8>, bool) {
        let now = Instant::now();
        match &mut self.probe {
            Probe::Tmux(rx, next) if *next <= now => match rx.try_recv() {
                Ok(true) => self.query(Protocol::Kitty, out),
                Err(std::sync::mpsc::TryRecvError::Empty) => *next = now + Duration::from_millis(15),
                _ => self.probe = Probe::Done,
            },
            Probe::Waiting(Protocol::Iterm, until) if *until <= now => self.query(Protocol::Kitty, out),
            Probe::Waiting(_, until) if *until <= now => self.probe = Probe::Done,
            _ => {}
        }
        let failed = self.ack.is_some_and(|(_, until)| until <= now);
        if failed {
            self.fail_kitty();
        }
        let bytes = if self.partial_until.is_some_and(|until| until <= now) {
            self.partial_until = None;
            if self.owns_prefix(&self.partial) {
                self.partial.clear();
            }
            std::mem::take(&mut self.partial)
        } else {
            Vec::new()
        };
        (bytes, failed)
    }

    fn fail_kitty(&mut self) {
        crate::log("preview graphics: Kitty upload failed, using cells");
        self.ack = None;
        self.protocol = None;
    }

    fn set_protocol(&mut self, protocol: Protocol) {
        self.probe = Probe::Done;
        self.protocol = Some(protocol);
        crate::log(&format!("preview graphics: {protocol:?}"));
    }

    pub fn input(&mut self, mut data: Vec<u8>) -> (Vec<u8>, bool) {
        if self.partial.is_empty() && !data.contains(&27) && !self.paste {
            return (data, false);
        }
        let iterm_queried = self.iterm_reply_until.is_some_and(|until| until > Instant::now());
        if self.partial.is_empty() && !self.kitty_queried && !iterm_queried {
            return (data, false);
        }

        if !self.partial.is_empty() {
            self.partial.append(&mut data);
            data = std::mem::take(&mut self.partial);
        }
        let mut input = data.as_slice();
        let mut key_bytes = Vec::with_capacity(data.len());
        let mut redraw = false;
        self.partial_until = None;
        while !input.is_empty() {
            let plain = input.iter().position(|&byte| byte == 27).unwrap_or(input.len());
            key_bytes.extend_from_slice(&input[..plain]);
            input = &input[plain..];
            if input.is_empty() {
                break;
            }
            let paste_marker = if self.paste { PASTE_END } else { PASTE_START };
            if input.starts_with(paste_marker) {
                key_bytes.extend_from_slice(paste_marker);
                input = &input[paste_marker.len()..];
                self.paste = !self.paste;
                continue;
            }
            if paste_marker.starts_with(input) {
                break;
            }
            if !self.paste {
                let prefixes = [(KITTY, self.kitty_queried), (ITERM, iterm_queried)];
                if let Some((prefix, _)) =
                    prefixes.into_iter().find(|(prefix, active)| *active && input.starts_with(prefix))
                {
                    let body = &input[prefix.len()..];
                    let end = body.iter().position(|&byte| byte == 27 || byte == 7);
                    let tail = end.map(|end| &body[end..]);
                    let terminator = match tail {
                        Some(rest) if rest.starts_with(b"\x1b\\") => 2,
                        Some(rest) if prefix == ITERM && rest.starts_with(b"\x07") => 1,
                        _ => 0,
                    };
                    if let Some(end) = end.filter(|_| terminator > 0) {
                        let len = prefix.len() + end + terminator;
                        let (owned, changed) = if len <= MAX_REPLY {
                            self.reply(prefix, &body[..end])
                        } else {
                            (self.owns_prefix(input), false)
                        };
                        redraw |= changed;
                        if !owned {
                            key_bytes.extend_from_slice(&input[..len]);
                        }
                        input = &input[len..];
                        continue;
                    }
                    if let Some(end) = end.filter(|&end| body[end..] != *b"\x1b") {
                        if !self.owns_prefix(input) {
                            key_bytes.extend_from_slice(&input[..prefix.len() + end]);
                        }
                        input = &body[end..];
                        continue;
                    }
                    if input.len() < MAX_REPLY {
                        break;
                    }
                } else if prefixes.iter().any(|(prefix, active)| *active && prefix.starts_with(input)) {
                    break;
                }
            }
            key_bytes.push(input[0]);
            input = &input[1..];
        }
        if !input.is_empty() {
            self.partial.extend_from_slice(input);
            let wait = if self.owns_prefix(input) { REPLY_WAIT } else { ESCAPE_WAIT };
            self.partial_until = Some(Instant::now() + wait);
        }
        (key_bytes, redraw)
    }

    fn owns_prefix(&self, data: &[u8]) -> bool {
        if self.paste {
            return false;
        }
        if self.iterm_reply_until.is_some() && data.starts_with(ITERM) {
            return true;
        }
        let Some(body) = data.strip_prefix(KITTY) else { return false };
        if body.len() < 2 {
            return self.kitty_queried && b"i=".starts_with(body);
        }
        let Some(id) = body.strip_prefix(b"i=") else { return false };
        if id.iter().all(u8::is_ascii_digit) {
            return std::iter::once(&self.probe_id)
                .chain(&self.sent_ids)
                .any(|value| value.to_string().as_bytes().starts_with(id));
        }
        kitty_id(body).is_some_and(|id| id == self.probe_id || self.sent_ids.contains(&id))
    }

    fn reply(&mut self, prefix: &[u8], body: &[u8]) -> (bool, bool) {
        let Ok(text) = std::str::from_utf8(body) else { return (false, false) };
        if prefix == ITERM {
            let waiting = matches!(self.probe, Probe::Waiting(Protocol::Iterm, _));
            if self.iterm_reply_until.is_some_and(|until| until > Instant::now())
                && let Some(cells) = iterm_cells(text)
            {
                self.iterm_reply_until = None;
                if waiting {
                    self.cell_size = Some(cells);
                    self.set_protocol(Protocol::Iterm);
                }
                return (true, waiting);
            }
            return (false, false);
        }
        let Some((params, status)) = text.split_once(';') else { return (false, false) };
        let Some(id) = kitty_id(params.as_bytes()) else { return (false, false) };
        if id == self.probe_id {
            let waiting = matches!(self.probe, Probe::Waiting(Protocol::Kitty, _));
            if waiting {
                if status == "OK" {
                    self.set_protocol(Protocol::Kitty);
                } else {
                    self.probe = Probe::Done;
                }
            }
            return (true, waiting && status == "OK");
        }
        if self.ack.is_some_and(|(pending, _)| pending == id) {
            self.ack = None;
            if status != "OK" {
                self.fail_kitty();
                return (true, true);
            }
        }
        (self.sent_ids.contains(&id), false)
    }

    pub fn draw(&mut self, out: &mut Vec<u8>, viewport: Rect, (w, h): (u32, u32), payload: &str) {
        let Some(protocol) = self.protocol else { return };
        let _ = write!(out, "\x1b[{};{}H", viewport.row + 1, viewport.col + 1);
        match protocol {
            Protocol::Kitty => {
                self.next_id = self.next_id.wrapping_add(1).max(1);
                if self.next_id == self.probe_id {
                    self.next_id = self.next_id.wrapping_add(1).max(1);
                }
                let id = self.next_id;
                if self.sent_ids.len() == 32 {
                    self.sent_ids.pop_front();
                }
                self.sent_ids.push_back(id);
                let chunks = payload.as_bytes().chunks(4096);
                let count = chunks.len();
                for (index, chunk) in chunks.enumerate() {
                    let more = u8::from(index + 1 < count);
                    if index == 0 {
                        let _ = write!(out, "\x1b_Ga=T,t=d,f=100,i={id},c={w},r={h},C=1,m={more};");
                    } else {
                        let _ = write!(out, "\x1b_Gm={more};");
                    }
                    out.extend_from_slice(chunk);
                    out.extend_from_slice(b"\x1b\\");
                }
                self.ack = Some((id, Instant::now() + REPLY_WAIT));
                self.painted = Some(Painted::Kitty(id));
            }
            Protocol::Iterm => {
                out.extend_from_slice(
                    format!("\x1b]1337;File=inline=1;width={w};height={h};preserveAspectRatio=1:").as_bytes(),
                );
                out.extend_from_slice(payload.as_bytes());
                out.extend_from_slice(b"\x07");
                self.painted = Some(Painted::Iterm(Rect { width: w as u16, height: h as u16, ..viewport }));
            }
        }
    }

    pub fn erase(&mut self, out: &mut Vec<u8>, (rows, cols): (u16, u16)) {
        self.ack = None;
        match self.painted.take() {
            Some(Painted::Kitty(id)) => {
                let _ = write!(out, "\x1b_Ga=d,d=I,i={id},q=2\x1b\\");
            }
            Some(Painted::Iterm(rect)) if rect.col < cols => {
                let width = rect.width.min(cols - rect.col);
                for row in rect.row..rect.row.saturating_add(rect.height).min(rows) {
                    let _ = write!(out, "\x1b[{};{}H\x1b[{width}X", row + 1, rect.col + 1);
                }
            }
            _ => {}
        }
    }

    pub fn resize(&mut self, out: &mut Vec<u8>, size: (u16, u16)) {
        if matches!(self.painted, Some(Painted::Iterm(_))) {
            out.extend_from_slice(b"\x1b7");
            self.erase(out, size);
            out.extend_from_slice(b"\x1b8");
        } else {
            self.erase(out, size);
        }
        if let Some(cells) = tty_cells() {
            self.cell_size = Some(cells);
        }
        self.end_popup();
    }

    pub fn end_popup(&mut self) {
        if self.tmux {
            // The attached tmux client can change between previews.
            self.probe = Probe::Idle;
            self.protocol = None;
        }
    }
}

fn kitty_id(body: &[u8]) -> Option<u32> {
    let header = body.split(|&byte| byte == b';').next()?;
    std::str::from_utf8(header).ok()?.split(',').find_map(|param| param.strip_prefix("i=")?.parse::<u32>().ok())
}

fn iterm_cells(text: &str) -> Option<(u32, u32)> {
    let mut fields = text.split(';');
    let height = fields.next()?.parse::<f64>().ok()?;
    let width = fields.next()?.parse::<f64>().ok()?;
    let scale = fields.next().map_or(Some(1.0), |value| value.parse::<f64>().ok())?;
    let (width, height) = ((width * scale).round(), (height * scale).round());
    (fields.next().is_none() && (1.0..=512.0).contains(&width) && (1.0..=512.0).contains(&height))
        .then_some((width as u32, height as u32))
}

fn tty_cells() -> Option<(u32, u32)> {
    #[cfg(unix)]
    {
        // SAFETY: ioctl writes a winsize into the initialized, writable structure.
        let mut size = unsafe { std::mem::zeroed::<libc::winsize>() };
        if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0
            && size.ws_col != 0
            && size.ws_row != 0
        {
            let w = u32::from(size.ws_xpixel) / u32::from(size.ws_col);
            let h = u32::from(size.ws_ypixel) / u32::from(size.ws_row);
            return ((1..=512).contains(&w) && (1..=512).contains(&h)).then_some((w, h));
        }
    }
    None
}

fn tmux_client() -> Option<std::sync::mpsc::Receiver<bool>> {
    let pane = std::env::var("TMUX_PANE").ok()?;
    let number = pane.strip_prefix('%')?;
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("lig-preview-tmux".into())
        .spawn(move || {
            let mut command = std::process::Command::new("tmux");
            command.args(["display-message", "-p", "-t", &pane, "#{client_termfeatures}"]);
            let supports = super::external::run(command, None, 1024, REPLY_WAIT, &|| false)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .is_some_and(|features| features.trim().split(',').any(|feature| feature == "kittygraphics"));
            let _ = tx.send(supports);
        })
        .ok()?;
    Some(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_replies_are_matched_by_id_and_fragmented_without_eating_keys() {
        let mut graphics = Graphics::new(ImageProtocol::Auto);
        let mut out = vec![];
        graphics.query(Protocol::Kitty, &mut out);
        let id = graphics.probe_id;
        let reply = format!("\x1b_Gi={id};OK\x1b\\");
        assert_eq!(graphics.input(b"x\x1b_Gi=".to_vec()), (b"x".to_vec(), false));
        assert_eq!(graphics.input([&reply.as_bytes()[5..], b"\x1bp"].concat()), (b"\x1bp".to_vec(), true));
        assert_eq!(graphics.protocol(), Some(Protocol::Kitty));
        let foreign = format!("\x1b_Gi={};OK\x1b\\", id.wrapping_add(1));
        assert_eq!(graphics.input(foreign.as_bytes().to_vec()), (foreign.into_bytes(), false));
        let paste = [PASTE_START, reply.as_bytes(), PASTE_END].concat();
        assert_eq!(graphics.input(paste.clone()), (paste, false));
    }

    #[test]
    fn late_kitty_probe_is_consumed_and_missing_upload_ack_disables_native_output() {
        let mut graphics = Graphics::new(ImageProtocol::Auto);
        graphics.probe = Probe::Waiting(Protocol::Kitty, Instant::now());
        graphics.kitty_queried = true;
        let mut out = vec![];
        assert_eq!(graphics.tick(&mut out), (vec![], false));
        let reply = format!("\x1b_Gi={};OK\x1b\\", graphics.probe_id);
        assert_eq!(graphics.input(reply.into_bytes()), (vec![], false));
        assert_eq!(graphics.protocol(), None);

        graphics.set_protocol(Protocol::Kitty);
        let area = Rect { row: 0, col: 1, width: 6, height: 3 };
        graphics.draw(&mut out, area, (6, 3), "AAAA");
        let (id, _) = graphics.ack.unwrap();
        graphics.ack = Some((id, Instant::now()));
        assert_eq!(graphics.tick(&mut out), (vec![], true));
        assert_eq!(graphics.protocol(), None);
        let mut cleanup = vec![];
        graphics.erase(&mut cleanup, (24, 80));
        assert_eq!(cleanup, format!("\x1b_Ga=d,d=I,i={id},q=2\x1b\\").into_bytes());
    }

    #[test]
    fn native_tmux_needs_client_capability_and_kitty_probe() {
        for client in [true, false] {
            let (tx, rx) = std::sync::mpsc::channel();
            tx.send(client).unwrap();
            let mut graphics = Graphics::new(ImageProtocol::Auto);
            graphics.probe = Probe::Tmux(rx, Instant::now());
            let mut out = vec![];
            assert_eq!(graphics.tick(&mut out), (vec![], false));
            assert_eq!(out.starts_with(KITTY), client);
            assert_eq!(graphics.protocol(), None);
        }
    }

    #[test]
    fn retina_iterm_reply_is_owned_only_during_our_query() {
        let mut graphics = Graphics::new(ImageProtocol::Auto);
        let mut out = vec![];
        graphics.query(Protocol::Iterm, &mut out);
        let reply = b"\x1b]1337;ReportCellSize=17.5;8;2\x07";
        assert_eq!(graphics.input(reply.to_vec()), (vec![], true));
        assert_eq!(graphics.cells(), (16, 35));
        assert_eq!(graphics.protocol(), Some(Protocol::Iterm));
        assert_eq!(graphics.input(reply.to_vec()), (reply.to_vec(), false));
    }
}
