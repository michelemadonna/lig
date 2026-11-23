//! SGR mouse framing and temporary terminal mouse ownership for preview scrolling.
use crate::popup::Rect;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub button: u16,
    pub col: u16,
    pub row: u16,
    pub release: bool,
}
impl Report {
    pub fn scroll(self, rect: Rect) -> Option<bool> {
        let inside = self.col >= rect.col
            && self.col < rect.col.saturating_add(rect.width)
            && self.row >= rect.row
            && self.row < rect.row.saturating_add(rect.height);
        if inside && !self.release && self.button & !28 == 64 {
            Some(false)
        } else if inside && !self.release && self.button & !28 == 65 {
            Some(true)
        } else {
            None
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub enum Input {
    Bytes(Vec<u8>),
    Mouse(Report),
}
#[derive(Default)]
pub struct Framer {
    pending: Vec<u8>,
    since: Option<Instant>,
    discard_mouse: bool,
}
impl Framer {
    pub fn has_pending(&self) -> bool {
        self.since.is_some() || self.discard_mouse
    }

    pub fn wake(&self) -> Option<Instant> {
        self.since.map(|t| t + Duration::from_millis(25))
    }
    pub fn expired(&mut self) -> Option<Vec<u8>> {
        if self.wake().is_some_and(|t| t <= Instant::now()) {
            self.since = None;
            if self.pending.starts_with(b"\x1b[<") {
                self.pending.clear();
                self.discard_mouse = true;
                None
            } else {
                Some(std::mem::take(&mut self.pending))
            }
        } else {
            None
        }
    }
    pub fn feed(&mut self, data: &[u8], capture: bool) -> Vec<Input> {
        if !capture && self.pending.starts_with(b"\x1b[<") {
            self.pending.clear();
            self.discard_mouse = true;
        }
        let data = if self.discard_mouse {
            let end = data.iter().position(|b| !b.is_ascii_digit() && *b != b';');
            if let Some(end) = end {
                self.discard_mouse = false;
                if matches!(data[end], b'M' | b'm') { &data[end + 1..] } else { &data[end..] }
            } else {
                return Vec::new();
            }
        } else {
            data
        };
        self.pending.extend_from_slice(data);
        self.since = None;
        if !capture {
            return vec![Input::Bytes(std::mem::take(&mut self.pending))];
        }
        let mut result = Vec::new();
        loop {
            let Some(start) = self.pending.iter().position(|b| *b == 27) else {
                if !self.pending.is_empty() {
                    result.push(Input::Bytes(std::mem::take(&mut self.pending)));
                }
                break;
            };
            if start > 0 {
                result.push(Input::Bytes(self.pending.drain(..start).collect()));
            }
            if self.pending == b"\x1b"
                || self.pending == b"\x1b["
                || (self.pending.starts_with(b"\x1b[<")
                    && self.pending.len() < 64
                    && !self.pending[3..].iter().any(|b| b.is_ascii_alphabetic()))
            {
                self.since = Some(Instant::now());
                break;
            }
            if self.pending.starts_with(b"\x1b[<") {
                let end = self.pending[3..].iter().position(|b| b.is_ascii_alphabetic()).map(|n| n + 3);
                if let Some(end) = end {
                    let bytes: Vec<u8> = self.pending.drain(..=end).collect();
                    if matches!(bytes[end], b'M' | b'm') {
                        let text = std::str::from_utf8(&bytes[3..end]).unwrap_or("");
                        let fields = text.split(';').map(|n| n.parse::<u16>().ok()).collect::<Option<Vec<_>>>();
                        if let Some(fields) = fields
                            && fields.len() == 3
                            && fields[1] > 0
                            && fields[2] > 0
                        {
                            result.push(Input::Mouse(Report {
                                button: fields[0],
                                col: fields[1] - 1,
                                row: fields[2] - 1,
                                release: bytes[end] == b'm',
                            }));
                        }
                    }
                    continue;
                }
                // An oversized malformed report is discarded while we own mouse input.
                self.pending.clear();
                self.discard_mouse = true;
                break;
            }
            // A keyboard escape sequence stays intact, including bracketed paste.
            let len = crate::keys::split(&self.pending).first().map_or(1, |(bytes, _)| bytes.len());
            result.push(Input::Bytes(self.pending.drain(..len).collect()));
        }
        result
    }
}

#[derive(Default)]
pub struct ModeGuard {
    enabled: bool,
    restore: Vec<u8>,
}
impl ModeGuard {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn set(&mut self, enabled: bool, screen: &vt100::Screen, out: &mut Vec<u8>) {
        if self.enabled == enabled {
            return;
        }
        if enabled {
            self.restore = restoration(screen);
            out.extend_from_slice(
                b"\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1000h\x1b[?1006h",
            );
        } else {
            out.extend_from_slice(&self.restore);
        }
        self.enabled = enabled;
    }
}
impl Drop for ModeGuard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = crate::term::write(&self.restore);
        }
    }
}
fn restoration(screen: &vt100::Screen) -> Vec<u8> {
    let mut out = b"\x1b[?9l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l".to_vec();
    use vt100::{MouseProtocolEncoding as E, MouseProtocolMode as M};
    out.extend_from_slice(match screen.mouse_protocol_mode() {
        M::None => b"",
        M::Press => b"\x1b[?9h",
        M::PressRelease => b"\x1b[?1000h",
        M::ButtonMotion => b"\x1b[?1002h",
        M::AnyMotion => b"\x1b[?1003h",
    });
    out.extend_from_slice(match screen.mouse_protocol_encoding() {
        E::Default => b"",
        E::Utf8 => b"\x1b[?1005h",
        E::Sgr => b"\x1b[?1006h",
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expired_mouse_never_becomes_shell_input() {
        let mut f = Framer::default();
        assert!(f.feed(b"\x1b[<65;40;", true).is_empty());
        f.since = Some(Instant::now() - Duration::from_secs(1));
        assert!(f.expired().is_none());
        assert_eq!(f.feed(b"7Mx", true), vec![Input::Bytes(b"x".to_vec())]);
        assert!(f.feed(b"\x1b[<65;40;", true).is_empty());
        f.since = Some(Instant::now() - Duration::from_secs(1));
        f.expired();
        assert_eq!(f.feed(b"echo", false), vec![Input::Bytes(b"echo".to_vec())]);
    }
    #[test]
    fn fragmented_reports_and_closed_passthrough() {
        let mut f = Framer::default();
        assert!(f.feed(b"\x1b[<65;", true).is_empty());
        assert_eq!(f.feed(b"12;7M", true), vec![Input::Mouse(Report { button: 65, col: 11, row: 6, release: false })]);
        assert_eq!(f.feed(b"\x1b[<65;12;7M", false), vec![Input::Bytes(b"\x1b[<65;12;7M".to_vec())]);
    }
    #[test]
    fn wheel_only_inside_preview() {
        let r = Rect { row: 5, col: 10, height: 4, width: 20 };
        let report = Report { button: 65, col: 11, row: 6, release: false };
        assert_eq!(report.scroll(r), Some(true));
        assert_eq!(Report { col: 9, ..report }.scroll(r), None);
        assert_eq!(Report { button: 0, ..report }.scroll(r), None);
        assert_eq!(Report { release: true, ..report }.scroll(r), None);
    }
    #[test]
    fn mixed_keyboard_mouse_and_paste() {
        let mut f = Framer::default();
        let out = f.feed(b"\x1b[B\x1b[<65;12;7Mx", true);
        assert_eq!(out.len(), 3);
        assert!(matches!(&out[1], Input::Mouse(_)));
        let paste = b"\x1b[200~\x1b[<65;12;7M\x1b[201~";
        assert_eq!(f.feed(paste, true), vec![Input::Bytes(paste.to_vec())]);
    }
    #[test]
    fn mode_roundtrip_restores_child_state() {
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process(b"\x1b[?1002h\x1b[?1006h");
        let mut guard = ModeGuard::default();
        let mut out = Vec::new();
        guard.set(true, parser.screen(), &mut out);
        assert!(guard.enabled());
        guard.set(false, parser.screen(), &mut out);
        assert!(!guard.enabled());
        assert!(out.ends_with(b"\x1b[?1002h\x1b[?1006h"));
    }
}
