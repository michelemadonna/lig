//! WebM and Matroska detection from bounded EBML headers.
use std::io::{self, Cursor, Read, Seek, SeekFrom};

const EBML: u32 = 0x1A45DFA3;
const DOC_TYPE: u32 = 0x4282;
pub fn detects(header: &[u8]) -> bool {
    detects_doc_type(header, b"webm")
}

pub fn detects_matroska(header: &[u8]) -> bool {
    detects_doc_type(header, b"matroska")
}

fn detects_doc_type(header: &[u8], kind: &[u8]) -> bool {
    header.starts_with(&EBML.to_be_bytes())
        && Scan::new(Cursor::new(header), header.len() as u64, header.len() as u64).ebml_header(kind).is_ok()
}

#[derive(Clone, Copy)]
struct Element {
    id: u32,
    start: u64,
    end: Option<u64>,
}

impl Element {
    fn finite_end(self) -> io::Result<u64> {
        self.end.ok_or_else(invalid)
    }
}

struct Scan<R> {
    source: R,
    length: u64,
    limit: u64,
    headers_left: usize,
}

impl<R: Read + Seek> Scan<R> {
    fn new(source: R, length: u64, budget: u64) -> Self {
        Self { source, length, limit: length.min(budget), headers_left: 8192 }
    }

    fn ebml_header(&mut self, kind: &[u8]) -> io::Result<u64> {
        let header = self.header(0, self.length)?.ok_or_else(invalid)?;
        let end = header.finite_end()?;
        if header.id != EBML || end > self.limit {
            return Err(invalid());
        }
        let mut matches_type = false;
        self.visit(header.start, end, |scan, field| {
            if field.id == DOC_TYPE {
                matches_type = scan.bytes::<8>(field)?.is_some_and(|(value, len)| &value[..len] == kind);
                return Ok(false);
            }
            Ok(true)
        })?;
        if matches_type { Ok(end) } else { Err(invalid()) }
    }

    fn visit(
        &mut self,
        mut start: u64,
        end: u64,
        mut visit: impl FnMut(&mut Self, Element) -> io::Result<bool>,
    ) -> io::Result<()> {
        while let Some(element) = self.header(start, end)? {
            if !visit(self, element)? {
                break;
            }
            let Some(next) = element.end else { break };
            start = next;
        }
        Ok(())
    }

    fn header(&mut self, offset: u64, parent_end: u64) -> io::Result<Option<Element>> {
        let available = parent_end.min(self.limit).saturating_sub(offset);
        if available == 0 || self.headers_left == 0 {
            return Ok(None);
        }
        self.headers_left -= 1;
        self.source.seek(SeekFrom::Start(offset))?;
        let mut reader = (&mut self.source).take(available);
        let result: io::Result<Element> = (|| {
            let (id, id_len) = vint(&mut reader, 4, false)?;
            let (size, size_len) = vint(&mut reader, 8, true)?;
            let start = offset + id_len + size_len;
            let end = if size == (1 << (size_len * 7)) - 1 {
                None
            } else {
                Some(start.checked_add(size).filter(|end| *end <= parent_end).ok_or_else(invalid)?)
            };
            Ok(Element { id: id as u32, start, end })
        })();
        match result {
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof && self.limit < parent_end && available < 12 => {
                Ok(None)
            }
            other => other.map(Some),
        }
    }

    fn bytes<const N: usize>(&mut self, field: Element) -> io::Result<Option<([u8; N], usize)>> {
        let end = field.finite_end()?;
        let len = end - field.start;
        if len == 0 || len > N as u64 || end > self.limit {
            return Ok(None);
        }
        let mut bytes = [0; N];
        self.source.seek(SeekFrom::Start(field.start))?;
        self.source.read_exact(&mut bytes[..len as usize])?;
        Ok(Some((bytes, len as usize)))
    }
}

fn vint(reader: &mut impl Read, max: u8, trim_marker: bool) -> io::Result<(u64, u64)> {
    let mut first = [0];
    reader.read_exact(&mut first)?;
    let length = first[0].leading_zeros() as u8 + 1;
    if length > max {
        return Err(invalid());
    }
    let mut value = u64::from(if trim_marker { first[0] ^ (0x80 >> (length - 1)) } else { first[0] });
    for _ in 1..length {
        let mut byte = [0];
        reader.read_exact(&mut byte)?;
        value = (value << 8) | u64::from(byte[0]);
    }
    Ok((value, u64::from(length)))
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid WebM container")
}
