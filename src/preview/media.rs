//! Media detection and formatting of optional MediaInfo JSON output.
use std::borrow::Cow;

use serde_json::Value;

use super::content::{Style, Text, TextBuilder, clip};
use super::native::display_size;

fn file_brands(header: &[u8]) -> impl Iterator<Item = &[u8]> {
    let end =
        header.get(..4).and_then(|bytes| bytes.try_into().ok()).map(u32::from_be_bytes).unwrap_or_default() as usize;
    let branded = header.get(4..8) == Some(b"ftyp") && end >= 16;
    header
        .get(8..12)
        .filter(|_| branded)
        .into_iter()
        .chain(header.get(16..end.min(header.len())).filter(|_| branded).unwrap_or_default().chunks_exact(4))
}

pub(super) fn is_bmff_image(header: &[u8]) -> bool {
    file_brands(header).any(|brand| {
        matches!(
            brand,
            b"avif"
                | b"avis"
                | b"mif1"
                | b"msf1"
                | b"miaf"
                | b"heic"
                | b"heix"
                | b"heim"
                | b"heis"
                | b"hevc"
                | b"hevx"
                | b"hevm"
                | b"hevs"
        )
    })
}

pub(super) fn detects(header: &[u8]) -> bool {
    file_brands(header).any(|brand| {
        matches!(
            brand,
            b"isom"
                | b"iso2"
                | b"iso3"
                | b"iso4"
                | b"iso5"
                | b"iso6"
                | b"iso7"
                | b"iso8"
                | b"iso9"
                | b"mp41"
                | b"mp42"
                | b"avc1"
                | b"qt  "
                | b"M4A "
                | b"M4B "
                | b"M4V "
                | b"dash"
                | b"msdh"
                | b"msix"
        ) || brand.starts_with(b"3gp")
            || brand.starts_with(b"3g2")
    }) || matches!(header.get(..4), Some(b"fLaC" | b"OggS"))
        || header.starts_with(b"FLV\x01")
        || header.starts_with(b"\x30\x26\xb2\x75\x8e\x66\xcf\x11\xa6\xd9\x00\xaa\x00\x62\xce\x6c")
        || super::webm::detects(header)
        || super::webm::detects_matroska(header)
        || (matches!(header.get(..4), Some(b"RIFF" | b"RF64" | b"BW64"))
            && matches!(header.get(8..12), Some(b"WAVE" | b"AVI " | b"AVIX")))
        || (header.starts_with(b"FORM") && matches!(header.get(8..12), Some(b"AIFF" | b"AIFC")))
        || id3_header(header)
        || transport_stream(header, 0, 188)
        || transport_stream(header, 4, 192)
}

fn id3_header(header: &[u8]) -> bool {
    matches!(header.get(..10), Some([b'I', b'D', b'3', 2..=4, 0..=254, _, a, b, c, d])
        if [a, b, c, d].into_iter().all(|size_byte| *size_byte < 0x80))
}

fn transport_stream(header: &[u8], start: usize, stride: usize) -> bool {
    (0..3).all(|packet| {
        let offset = start + packet * stride;
        matches!(header.get(offset..offset + 4), Some([0x47, _, _, flags]) if flags & 0x30 != 0)
    })
}

pub(super) fn extension_matches(ext: &str) -> bool {
    matches!(
        ext,
        "mp4"
            | "m4v"
            | "mov"
            | "mkv"
            | "mk3d"
            | "webm"
            | "avi"
            | "mts"
            | "m2ts"
            | "mpg"
            | "mpeg"
            | "3gp"
            | "3g2"
            | "flv"
            | "wmv"
            | "asf"
            | "mp3"
            | "mp2"
            | "m4a"
            | "m4b"
            | "aac"
            | "flac"
            | "wav"
            | "wave"
            | "aiff"
            | "aif"
            | "aifc"
            | "ogg"
            | "oga"
            | "ogv"
            | "opus"
            | "ac3"
            | "eac3"
            | "wma"
            | "alac"
            | "ape"
            | "mka"
    )
}

fn field<'a>(track: &'a Value, name: &str) -> Option<Cow<'a, str>> {
    match track.get(name)? {
        Value::String(value) if !value.trim().is_empty() => Some(Cow::Borrowed(value.trim())),
        Value::Number(value) => Some(Cow::Owned(value.to_string())),
        _ => None,
    }
}

fn duration(seconds: &str) -> Option<String> {
    // MediaInfo's JSON Duration is expressed in seconds.
    let millis = seconds.parse::<f64>().ok()? * 1000.0;
    if !millis.is_finite() || millis < 0.0 || millis >= u64::MAX as f64 {
        return None;
    }
    let millis = millis.round() as u64;
    let seconds = millis / 1000;
    let mut value = format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60);
    if millis % 1000 != 0 {
        value.push_str(&format!(".{:03}", millis % 1000));
    }
    Some(value)
}

fn bitrate(value: &str) -> Option<String> {
    let rate = value.parse::<u64>().ok().filter(|&rate| rate > 0)?;
    Some(if rate >= 1_000_000 {
        format!("{:.2} Mb/s", rate as f64 / 1_000_000.0)
    } else if rate >= 1000 {
        format!("{:.1} kb/s", rate as f64 / 1000.0)
    } else {
        format!("{rate} b/s")
    })
}

fn track_line(track: &Value, kind: &str, label: &str, index: usize) -> String {
    let get = |name| field(track, name);
    let integer = |name| get(name)?.parse::<u32>().ok().filter(|&value| value > 0);
    let mut details = Vec::new();
    details.extend(get("Format").or_else(|| get("CodecID")).map(Cow::into_owned));
    match kind {
        "Video" => {
            if let (Some(width), Some(height)) = (integer("Width"), integer("Height")) {
                details.push(format!("{width} × {height}"));
            }
            if let Some(rate) = get("FrameRate")
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|rate| rate.is_finite() && *rate > 0.0)
            {
                details.push(format!("{rate} fps"));
            }
        }
        "Audio" => {
            if let Some(channels) = integer("Channels").or_else(|| integer("Channel(s)")) {
                let unit = if channels == 1 { "channel" } else { "channels" };
                details.push(format!("{channels} {unit}"));
            }
            if let Some(rate) = integer("SamplingRate") {
                details.push(format!("{rate} Hz"));
            }
            details.extend(get("Language").map(Cow::into_owned));
        }
        "Text" => details.extend(get("Language").map(Cow::into_owned)),
        _ => {}
    }
    let mut line = format!("{label} {}", index + 1);
    for detail in details {
        line.push_str(" · ");
        line.push_str(clip(&detail, 160, 0).0);
    }
    line
}

fn track_type(track: &Value) -> Option<&str> {
    track.get("@type").and_then(Value::as_str)
}

pub(super) fn from_mediainfo(bytes: &[u8], file_size: u64, max_lines: usize) -> Option<Text> {
    let json: Value = serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).ok()?;
    let tracks = json.get("media")?.get("track")?.as_array()?;
    if !tracks.iter().any(|track| matches!(track_type(track), Some("Audio" | "Video"))) {
        return None;
    }

    let general = tracks.iter().find(|track| track_type(track) == Some("General"));
    let get = |name| general.and_then(|track| field(track, name));
    let container = get("Format").unwrap_or(Cow::Borrowed("Media"));
    let container = clip(&container, 80, 0).0;
    let mut heading = format!("{container} · {}", display_size(file_size));
    heading.extend(get("Duration").and_then(|value| duration(&value)).map(|value| format!(" · {value}")));
    heading.extend(get("OverallBitRate").and_then(|value| bitrate(&value)).map(|value| format!(" · {value}")));

    let mut doc = TextBuilder::new(max_lines, false);
    doc.push(&heading, Style::default());
    doc.newline();
    for (kind, label) in [("Video", "Video"), ("Audio", "Audio"), ("Text", "Subtitles")] {
        for (index, track) in tracks.iter().filter(|track| track_type(track) == Some(kind)).enumerate() {
            if doc.full() {
                return Some(doc.finish());
            }
            doc.push(&track_line(track, kind, label, index), Style::default());
            doc.newline();
        }
    }
    Some(doc.finish())
}
