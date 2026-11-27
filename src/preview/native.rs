use std::borrow::Cow;
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;

use flate2::bufread::GzDecoder;
use image::{ImageDecoder, ImageReader, Limits, RgbaImage};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, ThemeSet},
    parsing::SyntaxSet,
};

use super::config::{Config, Engine, ImageProtocol};
use super::content::{Color, Style, Text, TextBuilder};
use super::raster::Raster;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Text,
    Image,
    Archive,
    Media,
    Directory,
}

pub fn extension(path: &Path) -> String {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    if let Some(ext) = super::archive::compound_extension(&name) {
        ext.into()
    } else {
        path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase()
    }
}

pub fn category(ext: &str, header: &[u8], directory: bool) -> Category {
    if directory {
        Category::Directory
    } else if image::guess_format(header).is_ok() || super::media::is_bmff_image(header) {
        Category::Image
    } else if super::archive::detects("", header) {
        Category::Archive
    } else if super::media::detects(header) {
        Category::Media
    } else if header.starts_with(b"\xef\xbb\xbf") || header.starts_with(b"\xff\xfe") || header.starts_with(b"\xfe\xff")
    {
        Category::Text
    } else if super::archive::detects(ext, &[]) {
        Category::Archive
    } else {
        match ext {
            "png" | "jpg" | "jpeg" | "jpe" | "jfif" | "gif" | "webp" | "bmp" | "tiff" | "svg" | "ico" | "avif"
            | "heic" | "heif" => Category::Image,
            ext if super::media::extension_matches(ext) => Category::Media,
            _ => Category::Text,
        }
    }
}

pub fn engine<'a>(config: &'a Config, ext: &str, category: Category) -> &'a Engine {
    config.rules.get(ext).or_else(|| ext.rsplit_once('.').and_then(|(_, ext)| config.rules.get(ext))).unwrap_or(
        match category {
            Category::Text => &config.text,
            Category::Image => &config.image,
            Category::Archive => &config.archive,
            Category::Media => &config.media,
            Category::Directory => &config.directory,
        },
    )
}

#[derive(Debug)]
pub enum Content {
    Text(Text),
    Image(Raster),
    Directory(Vec<TreeRow>),
}

impl Content {
    pub fn weight(&self) -> usize {
        match self {
            Self::Text(text) => text.weight(),
            Self::Image(image) => image.weight(),
            Self::Directory(rows) => {
                rows.capacity() * std::mem::size_of::<TreeRow>()
                    + rows.iter().map(|row| row.name.capacity() + row.prefix.capacity()).sum::<usize>()
            }
        }
    }

    pub fn lines(&self) -> usize {
        match self {
            Self::Text(text) => text.lines.len(),
            Self::Image(_) => 0,
            Self::Directory(rows) => rows.len(),
        }
    }
}

pub fn metadata(meta: &Metadata, detail: &str, max_lines: usize, locale: Option<&str>) -> Content {
    let kind = if meta.is_dir() {
        "Directory"
    } else if meta.is_file() {
        "File"
    } else {
        "Special file"
    };
    let modified = meta
        .modified()
        .ok()
        .and_then(|time| super::locale::modified(time, locale))
        .map(|date| format!("Modified: {date}\n"))
        .unwrap_or_default();
    Content::Text(Text::plain(&format!("{modified}{kind} · {} bytes\n{detail}", meta.len()), max_lines))
}

pub fn open_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
}

pub struct Options<'a> {
    pub config: &'a Config,
    pub engine: &'a Engine,
    pub light: bool,
    pub show_hidden: bool,
    pub cancelled: &'a dyn Fn() -> bool,
}

pub trait Previewer {
    fn preview(&self, path: &Path, file: Option<File>, header: &[u8], opts: &Options<'_>) -> anyhow::Result<Content>;
}

impl Previewer for Category {
    fn preview(&self, path: &Path, file: Option<File>, header: &[u8], opts: &Options<'_>) -> anyhow::Result<Content> {
        if *self == Self::Directory {
            return Ok(Content::Directory(directory(path, opts)?));
        }
        let file = file.ok_or_else(|| anyhow::anyhow!("file could not be opened"))?;
        Ok(match self {
            Self::Text => Content::Text(text(path, file, opts)?),
            Self::Image => Content::Image(Raster::new(
                image(file, header, opts.cancelled)?,
                opts.config.image.image_protocol.unwrap_or_default() == ImageProtocol::Auto,
                opts.cancelled,
            )?),
            Self::Archive => Content::Text(archive(path, file, header, opts)?),
            Self::Media => anyhow::bail!("Unsupported media"),
            Self::Directory => unreachable!(),
        })
    }
}

fn text(path: &Path, file: File, opts: &Options<'_>) -> anyhow::Result<Text> {
    let mut raw = Vec::new();
    CancelRead(file, opts.cancelled).take(opts.config.max_bytes as u64).read_to_end(&mut raw)?;
    let text = decode_text(&raw)?;
    let max_lines = opts.engine.max_lines;
    let mut out = TextBuilder::new(max_lines, opts.engine.line_numbers.unwrap_or(true));

    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    static THEMES: OnceLock<ThemeSet> = OnceLock::new();
    let highlight = opts.engine.syntax_highlighting.unwrap_or(true).then(|| {
        let syntaxes = SYNTAXES.get_or_init(two_face::syntax::extra_newlines);
        let themes = THEMES.get_or_init(ThemeSet::load_defaults);
        let theme = &themes.themes[if opts.light { "InspiredGitHub" } else { "base16-ocean.dark" }];
        let ext = extension(path);
        let token = match ext.as_str() {
            "jsonc" => "json",
            "conf" => "ini",
            _ => &ext,
        };
        let syntax = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| syntaxes.find_syntax_by_extension(name))
            .or_else(|| syntaxes.find_syntax_by_extension(token))
            .or_else(|| {
                text.lines()
                    .next()
                    .filter(|line| line.len() <= 2048)
                    .and_then(|line| syntaxes.find_syntax_by_first_line(line))
            })
            .unwrap_or_else(|| syntaxes.find_syntax_plain_text());
        (syntaxes, theme, HighlightLines::new(syntax, theme))
    });
    let mut highlight = highlight;
    let mut highlighted = 0;
    for line in text.split_inclusive('\n').take(max_lines) {
        anyhow::ensure!(!(opts.cancelled)(), "cancelled");
        // The regex engine's cost is bounded independently of a user-supplied file size.
        // A skipped line invalidates the lexical state of subsequent lines.
        if line.len() > 2048 || highlighted + line.len() > 65_536 {
            highlight = None;
        }
        if let Some((syntaxes, theme, highlighter)) = &mut highlight
            && let Ok(spans) = highlighter.highlight_line(line, syntaxes)
        {
            highlighted += line.len();
            for (style, s) in spans {
                let fg = style.foreground;
                out.push(
                    s.trim_end_matches(['\n', '\r']),
                    Style {
                        fg: (Some(fg) != theme.settings.foreground).then_some(Color::Rgb(fg.r, fg.g, fg.b)),
                        bold: style.font_style.contains(FontStyle::BOLD),
                        italic: style.font_style.contains(FontStyle::ITALIC),
                        underline: style.font_style.contains(FontStyle::UNDERLINE),
                        ..Style::default()
                    },
                );
            }
        } else {
            out.push(line.trim_end_matches(['\n', '\r']), Style::default());
        }
        out.newline();
    }
    Ok(out.finish())
}

fn decode_text(raw: &[u8]) -> anyhow::Result<Cow<'_, str>> {
    let text = match raw {
        [0xff, 0xfe, 0, 0, ..] | [0, 0, 0xfe, 0xff, ..] => anyhow::bail!("UTF-32 is unsupported"),
        [0xff, 0xfe, bytes @ ..] | [0xfe, 0xff, bytes @ ..] => {
            let little_endian = raw[0] == 0xff;
            let words = bytes.chunks_exact(2).map(|chunk| {
                let pair = [chunk[0], chunk[1]];
                if little_endian { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) }
            });
            Cow::Owned(char::decode_utf16(words).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect())
        }
        [0xef, 0xbb, 0xbf, bytes @ ..] => String::from_utf8_lossy(bytes),
        _ => String::from_utf8_lossy(raw),
    };
    let bytes = text.as_bytes();
    let sample = &bytes[..bytes.len().min(8192)];
    let controls = sample.iter().filter(|&&b| b < 0x20 && !matches!(b, b'\n' | b'\r' | b'\t' | 12)).count();
    anyhow::ensure!(!sample.contains(&0) && controls <= sample.len() / 10, "Binary file");
    Ok(text)
}

pub(super) fn display_size(size: u64) -> String {
    match size {
        0..=1023 => format!("{size} B"),
        1024..=1_048_575 => format!("{:.1} KiB", size as f64 / 1024.0),
        _ => format!("{:.1} MiB", size as f64 / (1024.0 * 1024.0)),
    }
}

#[derive(Debug)]
pub struct TreeRow {
    pub prefix: String,
    pub name: String,
    pub directory: bool,
}

fn directory(path: &Path, opts: &Options<'_>) -> io::Result<Vec<TreeRow>> {
    let mut tree = DirectoryTree { remaining: 4096, rows: Vec::new(), opts };
    tree.walk(path, "", 1)?;
    Ok(tree.rows)
}

struct DirectoryTree<'a, 'b> {
    remaining: usize,
    rows: Vec<TreeRow>,
    opts: &'a Options<'b>,
}

impl DirectoryTree<'_, '_> {
    fn walk(&mut self, path: &Path, indent: &str, level: usize) -> io::Result<()> {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path)?.take(self.remaining) {
            if (self.opts.cancelled)() {
                return Err(io::Error::other("cancelled"));
            }
            self.remaining -= 1;
            let Ok(entry) = entry else { continue };
            let name = entry.file_name().to_string_lossy().into_owned();
            if !self.opts.show_hidden && name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            // Match the completion menu's icon for symlinks to directories.
            let directory = std::fs::metadata(&path).is_ok_and(|meta| meta.is_dir());
            entries.push((directory, name, path));
        }
        entries.sort_unstable_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let count = entries.len();
        for (index, (directory, name, path)) in entries.into_iter().enumerate() {
            if (self.opts.cancelled)() {
                return Err(io::Error::other("cancelled"));
            }
            if self.rows.len() >= self.opts.engine.max_lines {
                break;
            }
            let last = index + 1 == count;
            let branch = if last { "└── " } else { "├── " };
            self.rows.push(TreeRow { prefix: format!("{indent}{branch}"), name, directory });
            if directory
                && level < 3
                && self.remaining > 0
                && self.rows.len() < self.opts.engine.max_lines
                && std::fs::symlink_metadata(&path).is_ok_and(|meta| traversable_directory(&meta))
            {
                let branch = if last { "    " } else { "│   " };
                // A child may disappear or become unreadable after it was listed.
                let _ = self.walk(&path, &format!("{indent}{branch}"), level + 1);
            }
        }
        Ok(())
    }
}

fn traversable_directory(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    meta.file_type().is_dir()
}

const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PIXELS: u64 = 8_000_000;

pub(super) fn check_raster(w: u32, h: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        w > 0 && h > 0 && w <= 8192 && h <= 8192 && u64::from(w) * u64::from(h) <= MAX_PIXELS,
        "Image dimensions exceed the preview budget"
    );
    Ok(())
}

fn image(file: File, header: &[u8], cancelled: &dyn Fn() -> bool) -> anyhow::Result<RgbaImage> {
    let format = image::guess_format(header)?;
    anyhow::ensure!(file.metadata()?.len() <= MAX_IMAGE_BYTES, "Image input exceeds 32 MiB");
    let source = CancelRead(file, cancelled);
    if format == image::ImageFormat::WebP {
        // The codec's canvas dimensions alone do not bound its nested lossy frame
        // allocations. Decode the same immutable bytes that passed the preflight.
        let mut encoded = Vec::new();
        source.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut encoded)?;
        anyhow::ensure!(encoded.len() as u64 <= MAX_IMAGE_BYTES, "Image input exceeds 32 MiB");
        let len = super::webp::validate(&encoded, cancelled)?;
        // Some decoders read past the declared RIFF range when trailing data exists.
        encoded.truncate(len);
        return decode_image(std::io::Cursor::new(encoded), format);
    }
    decode_image(BufReader::new(Budget::new(source, MAX_IMAGE_BYTES)), format)
}

fn decode_image(source: impl io::BufRead + Seek, format: image::ImageFormat) -> anyhow::Result<RgbaImage> {
    let mut reader = ImageReader::with_format(source, format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let decoder = reader.into_decoder()?;
    let (w, h) = decoder.dimensions();
    check_raster(w, h)?;
    // GIF's ImageDecoder reads the first composited frame.
    Ok(image::DynamicImage::from_decoder(decoder)?.into_rgba8())
}

struct CancelRead<'a, R>(R, &'a dyn Fn() -> bool);

impl<R: Read> Read for CancelRead<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if (self.1)() {
            return Err(io::Error::other("cancelled"));
        }
        self.0.read(buf)
    }
}

impl<R: Seek> Seek for CancelRead<'_, R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if (self.1)() {
            return Err(io::Error::other("cancelled"));
        }
        self.0.seek(pos)
    }
}

struct Budget<R> {
    reader: R,
    remaining: u64,
}

impl<R> Budget<R> {
    fn new(reader: R, remaining: u64) -> Self {
        Self { reader, remaining }
    }
}

impl<R: Read> Read for Budget<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.remaining == 0 && !buf.is_empty() {
            return Err(io::Error::other("preview read budget exhausted"));
        }
        let n = buf.len().min(self.remaining as usize);
        let used = self.reader.read(&mut buf[..n])?;
        self.remaining -= used as u64;
        Ok(used)
    }
}

impl<R: Seek> Seek for Budget<R> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.reader.seek(pos)
    }
}

const ARCHIVE_BUDGET: u64 = 8 * 1024 * 1024;

pub(super) struct Listing {
    doc: TextBuilder,
    count: usize,
    dirs: usize,
}

impl Listing {
    pub(super) fn new(max_lines: usize) -> Self {
        Self { doc: TextBuilder::new(max_lines, false), count: 0, dirs: 0 }
    }

    pub(super) fn add(&mut self, name: &str, is_dir: bool, size: Option<u64>, encrypted: bool) {
        self.count += 1;
        self.dirs += usize::from(is_dir);
        let name = super::content::clip(name, 1024, 0).0;
        self.doc.push(name, Style::default());
        if is_dir {
            if !name.ends_with('/') {
                self.doc.push("/", Style::default());
            }
        } else if let Some(size) = size {
            self.doc.push(&format!("   {}", display_size(size)), Style::default());
        }
        if encrypted {
            self.doc.push("   [encrypted]", Style::default());
        }
        self.doc.newline();
    }

    pub(super) fn finish(mut self, format: &str, total: Option<usize>) -> Text {
        self.doc.push(&archive_summary(format, self.count, self.dirs, total), Style::default());
        self.doc.finish()
    }
}

fn archive_summary(format: &str, count: usize, dirs: usize, total: Option<usize>) -> String {
    let shown = if total == Some(count) {
        format!("{count} entries")
    } else if let Some(total) = total {
        format!("{count} of {total} entries")
    } else {
        format!("{count} entries shown · partial")
    };
    format!("{format} · {shown} · {dirs} directories")
}

fn archive(path: &Path, mut file: File, header: &[u8], opts: &Options<'_>) -> anyhow::Result<Text> {
    let ext = extension(path);
    let max_entries = opts.engine.max_lines.saturating_sub(1);
    let mut listing = Listing::new(opts.engine.max_lines);
    let gzip = header.starts_with(b"\x1f\x8b");
    let tar = header.get(257..262) == Some(b"ustar");
    if header.starts_with(b"PK\x03\x04") || header.starts_with(b"PK\x05\x06") || (ext == "zip" && !gzip && !tar) {
        let (bytes, total) = zip_directory(&mut file)?;
        let mut reader = CancelRead(file, opts.cancelled).take(bytes.min(ARCHIVE_BUDGET));
        for _ in 0..total.min(max_entries) {
            match zip_entry(&mut reader) {
                Ok((name, directory, size)) => listing.add(&name, directory, Some(size), false),
                Err(error) if listing.count == 0 => return Err(error),
                Err(_) => break,
            }
        }
        return Ok(listing.finish("ZIP", Some(total)));
    }
    anyhow::ensure!((gzip || tar || ext == "tar") && !(gzip && ext == "cpio.gz"), "Unsupported archive");

    let mut gzip_reader = gzip
        .then(|| {
            file.try_clone().map(|file| {
                GzDecoder::new(BufReader::new(Budget::new(CancelRead(file, opts.cancelled), ARCHIVE_BUDGET)))
            })
        })
        .transpose()?;
    if let Some(reader) = &mut gzip_reader
        && !matches!(ext.as_str(), "tar.gz" | "tgz")
    {
        let header = reader.header().ok_or_else(|| anyhow::anyhow!("Invalid GZIP header"))?;
        let filename = header.filename().map(String::from_utf8_lossy).unwrap_or_default();
        // ISIZE is the last member's uncompressed length modulo 2^32, not the stream length.
        let mut trailer = [0; 4];
        file.seek(SeekFrom::End(-4))?;
        file.read_exact(&mut trailer)?;
        return Ok(Text::plain(
            &format!("GZIP\n{filename}\nLast member size (mod 4 GiB): {} bytes", u32::from_le_bytes(trailer)),
            opts.engine.max_lines,
        ));
    }

    let source: Box<dyn Read + '_> =
        if let Some(reader) = gzip_reader { Box::new(reader) } else { Box::new(CancelRead(file, opts.cancelled)) };
    let mut archive = tar::Archive::new(Budget::new(source, ARCHIVE_BUDGET));
    let mut entries = archive.entries()?;
    let mut complete = false;
    for _ in 0..=max_entries {
        let Some(entry) = entries.next() else {
            complete = true;
            break;
        };
        if listing.count == max_entries {
            break;
        }
        match entry {
            Ok(entry) => listing.add(
                &String::from_utf8_lossy(&entry.path_bytes()),
                entry.header().entry_type().is_dir(),
                Some(entry.size()),
                false,
            ),
            Err(err) if listing.count == 0 => return Err(err.into()),
            Err(_) => break,
        }
    }
    let total = complete.then_some(listing.count);
    Ok(listing.finish(if gzip { "TAR.GZ" } else { "TAR" }, total))
}

pub(super) fn le16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes[..2].try_into().unwrap())
}

pub(super) fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes[..4].try_into().unwrap())
}

fn le64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes[..8].try_into().unwrap())
}

/// PKWARE APPNOTE §§4.3.12–16. Read only the central directory, with no allocation
/// based on the declared entry count or the size of a packed member.
fn zip_directory(file: &mut File) -> anyhow::Result<(u64, usize)> {
    let len = file.metadata()?.len();
    let tail_len = len.min(22 + u64::from(u16::MAX) + 20);
    file.seek(SeekFrom::Start(len - tail_len))?;
    let mut tail = vec![0; tail_len as usize];
    file.read_exact(&mut tail)?;
    let (index, end) = tail
        .windows(22)
        .enumerate()
        .rfind(|(index, end)| {
            end.starts_with(b"PK\x05\x06") && *index + 22 + usize::from(le16(&end[20..])) == tail.len()
        })
        .ok_or_else(|| anyhow::anyhow!("ZIP end record is missing"))?;
    let end_offset = len - tail_len + index as u64;
    anyhow::ensure!(end[4..8] == [0; 4] && end[8..10] == end[10..12], "Multi-disk ZIP is unsupported");
    let count = le16(&end[10..]);
    let size = le32(&end[12..]);
    let offset = le32(&end[16..]);

    let (boundary, size, offset, count) = if count == u16::MAX
        || size == u32::MAX
        || offset == u32::MAX
        || (index >= 20 && &tail[index - 20..index - 16] == b"PK\x06\x07")
    {
        let locator = tail
            .get(index.saturating_sub(20)..index)
            .filter(|b| b.len() == 20 && b.starts_with(b"PK\x06\x07"))
            .ok_or_else(|| anyhow::anyhow!("ZIP64 locator is missing"))?;
        anyhow::ensure!(le32(&locator[4..]) == 0 && le32(&locator[16..]) == 1, "Multi-disk ZIP64 is unsupported");
        let position = le64(&locator[8..]);
        let mut record = [0; 56];
        anyhow::ensure!(position.checked_add(56).is_some_and(|n| n <= end_offset - 20), "Invalid ZIP64 offset");
        file.seek(SeekFrom::Start(position))?;
        file.read_exact(&mut record)?;
        anyhow::ensure!(
            record.starts_with(b"PK\x06\x06") && record[16..24] == [0; 8] && record[24..32] == record[32..40],
            "Invalid ZIP64 end record"
        );
        (position, le64(&record[40..]), le64(&record[48..]), le64(&record[32..]))
    } else {
        (end_offset, u64::from(size), u64::from(offset), u64::from(count))
    };

    let start = boundary
        .checked_sub(size)
        .filter(|&start| start >= offset)
        .ok_or_else(|| anyhow::anyhow!("ZIP central directory is out of bounds"))?;
    anyhow::ensure!(count <= size / 46, "ZIP directory entry count exceeds its length");
    file.seek(SeekFrom::Start(start))?;
    Ok((size, usize::try_from(count)?))
}

fn zip_entry(reader: &mut impl Read) -> anyhow::Result<(String, bool, u64)> {
    let mut header = [0; 46];
    reader.read_exact(&mut header)?;
    anyhow::ensure!(header.starts_with(b"PK\x01\x02"), "Invalid ZIP directory record");
    let mut name = vec![0; usize::from(le16(&header[28..]))];
    reader.read_exact(&mut name)?;
    let size = le32(&header[24..]);
    let extra = u64::from(le16(&header[30..]));
    let comment = u64::from(le16(&header[32..]));
    let mut size64 = u64::from(size);
    if size == u32::MAX {
        let mut extra_fields = vec![0; extra as usize];
        reader.read_exact(&mut extra_fields)?;
        let mut remaining = extra_fields.as_slice();
        while remaining.len() >= 4 {
            let length = usize::from(le16(&remaining[2..]));
            let Some(field) = remaining.get(4..4 + length) else { break };
            if le16(remaining) == 1 && field.len() >= 8 {
                size64 = le64(field);
                break;
            }
            remaining = &remaining[4 + length..];
        }
    }
    let skip = comment + if size == u32::MAX { 0 } else { extra };
    anyhow::ensure!(io::copy(&mut reader.take(skip), &mut io::sink())? == skip, "Truncated ZIP directory");
    let name = String::from_utf8_lossy(&name).into_owned();
    let directory = name.ends_with('/') || le32(&header[38..]) & 0x10 != 0;
    Ok((name, directory, size64))
}
