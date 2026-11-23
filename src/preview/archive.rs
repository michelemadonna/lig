//! Archive detection and formatting of optional 7-Zip technical listings.
use super::content::Text;
use super::native::Listing;

const COMPOUND_EXTENSIONS: &[&str] = &[
    "tar.gz", "tar.bz2", "tar.xz", "tar.lzma", "tar.lz", "tar.zst", "tar.lz4", "tar.z", "cpio.gz", "cpio.bz2",
    "cpio.xz", "cpio.zst",
];

pub(super) fn compound_extension(name: &str) -> Option<&'static str> {
    COMPOUND_EXTENSIONS.iter().copied().find(|ext| name.strip_suffix(ext).is_some_and(|base| base.ends_with('.')))
}

pub(super) fn detects(ext: &str, header: &[u8]) -> bool {
    let signatures: &[&[u8]] = &[
        b"PK\x03\x04",
        b"PK\x05\x06",
        b"\x1f\x8b",
        b"7z\xbc\xaf\x27\x1c",
        b"Rar!\x1a\x07",
        b"\xfd7zXZ\0",
        b"\x28\xb5\x2f\xfd",
        b"\x04\x22\x4d\x18",
        b"\x02\x21\x4c\x18",
        b"\x1f\x9d",
        b"LZIP",
        b"MSCF",
        b"xar!",
        b"!<arch>\n",
        b"\xed\xab\xee\xdb",
        b"\x71\xc7",
        b"\xc7\x71",
    ];
    signatures.iter().any(|magic| header.starts_with(magic))
        || header.get(257..262) == Some(b"ustar")
        || matches!(header.get(..4), Some([b'B', b'Z', b'h', b'1'..=b'9']))
        || (matches!(header.get(..6), Some(b"070701" | b"070702" | b"070707"))
            && header.get(6..26).is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit)))
        || (matches!(header.get(2..5), Some(b"-lh" | b"-lz"))
            && matches!(header.get(5..7), Some([b'0'..=b'9' | b'd' | b's', b'-'])))
        || COMPOUND_EXTENSIONS.contains(&ext)
        || matches!(
            ext,
            "zip"
                | "zipx"
                | "tar"
                | "tgz"
                | "tbz"
                | "tbz2"
                | "txz"
                | "tlz"
                | "tzst"
                | "tlz4"
                | "tz"
                | "gz"
                | "7z"
                | "rar"
                | "xz"
                | "bz2"
                | "zst"
                | "zstd"
                | "lz4"
                | "lz"
                | "lzma"
                | "z"
                | "cpio"
                | "cab"
                | "iso"
                | "xar"
                | "pkg"
                | "lha"
                | "lzh"
                | "deb"
                | "ar"
                | "rpm"
                | "dmg"
                | "wim"
        )
}

pub(super) fn from_seven_zip(bytes: &[u8], max_lines: usize) -> Option<Text> {
    let output = std::str::from_utf8(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).ok()?;
    let mut format = None;
    let mut lines = output.lines();
    loop {
        match lines.next()? {
            "----------" => break,
            line if line.starts_with("Type = ") => {
                format = line.strip_prefix("Type = ").filter(|value| !value.is_empty())
            }
            _ => {}
        }
    }
    let format = super::content::clip(format?, 80, 0).0;
    let mut listing = Listing::new(max_lines);
    let mut total = 0;
    let mut record = Vec::new();
    for line in lines.chain(std::iter::once("")) {
        if !line.is_empty() {
            record.push(line);
            continue;
        }
        if record.is_empty() {
            continue;
        }
        let entry = SevenZipEntry::parse(&record)?;
        if total < max_lines.saturating_sub(1) {
            listing.add(entry.path, entry.directory, entry.size, entry.encrypted);
        }
        total += 1;
        record.clear();
    }
    Some(listing.finish(format, Some(total)))
}

struct SevenZipEntry<'a> {
    path: &'a str,
    directory: bool,
    size: Option<u64>,
    encrypted: bool,
}

impl<'a> SevenZipEntry<'a> {
    fn parse(lines: &[&'a str]) -> Option<Self> {
        let path = lines.first()?.strip_prefix("Path = ").filter(|path| !path.is_empty())?;
        let (mut directory, mut size, mut attributes, mut encrypted) = (None, None, None, None);
        for line in &lines[1..] {
            let (key, value) = line.split_once(" = ")?;
            let field = match key {
                "Path" => return None,
                "Folder" => &mut directory,
                "Size" => &mut size,
                "Attributes" => &mut attributes,
                "Encrypted" => &mut encrypted,
                _ => continue,
            };
            if field.replace(value).is_some() {
                return None;
            }
        }
        let directory = directory
            .map(|value| value == "+")
            .unwrap_or_else(|| path.ends_with(['/', '\\']) || attributes.is_some_and(|value| value.starts_with('D')));
        Some(Self {
            path,
            directory,
            size: size.and_then(|value| value.parse().ok()),
            encrypted: encrypted == Some("+"),
        })
    }
}
