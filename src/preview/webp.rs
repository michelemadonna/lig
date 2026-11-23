//! Check nested WebP rasters before the codec allocates from their headers.
//! See the WebP RIFF container and VP8/VP8L bitstream specifications.
use super::native::{check_raster, le16, le32};

pub fn validate(encoded: &[u8], cancelled: &dyn Fn() -> bool) -> anyhow::Result<usize> {
    anyhow::ensure!(encoded.starts_with(b"RIFF") && encoded.get(8..12) == Some(b"WEBP"), "Invalid WebP header");
    let len = 8usize.checked_add(le32(&encoded[4..]) as usize).ok_or_else(|| anyhow::anyhow!("Invalid WebP length"))?;
    let mut body = encoded.get(12..len).ok_or_else(|| anyhow::anyhow!("Truncated WebP"))?;
    let mut canvas = None;
    let mut bitstream = false;
    while !body.is_empty() {
        anyhow::ensure!(!cancelled(), "cancelled");
        let (kind, chunk) = next(&mut body)?;
        match kind {
            b"VP8X" if chunk.len() == 10 => {
                let size = (dimension24(&chunk[4..]), dimension24(&chunk[7..]));
                check_raster(size.0, size.1)?;
                canvas = Some(size);
            }
            b"VP8 " | b"VP8L" => {
                check_bitstream(kind, chunk, canvas)?;
                bitstream = true;
            }
            b"ANMF" if chunk.len() >= 16 => {
                let size = (dimension24(&chunk[6..]), dimension24(&chunk[9..]));
                check_raster(size.0, size.1)?;
                let mut frame = &chunk[16..];
                while !frame.is_empty() {
                    let (kind, chunk) = next(&mut frame)?;
                    if matches!(kind, b"VP8 " | b"VP8L") {
                        check_bitstream(kind, chunk, Some(size))?;
                        bitstream = true;
                    }
                }
            }
            b"VP8X" | b"ANMF" => anyhow::bail!("Invalid WebP frame header"),
            _ => {}
        }
    }
    anyhow::ensure!(bitstream, "WebP has no image data");
    Ok(len)
}

fn next<'a>(body: &mut &'a [u8]) -> anyhow::Result<(&'a [u8], &'a [u8])> {
    let header = body.get(..8).ok_or_else(|| anyhow::anyhow!("Truncated WebP chunk header"))?;
    let len = le32(&header[4..]) as usize;
    let end = 8usize
        .checked_add(len)
        .filter(|&end| end <= body.len())
        .ok_or_else(|| anyhow::anyhow!("Truncated WebP chunk"))?;
    let chunk = &body[8..end];
    *body = body.get(end + len % 2..).ok_or_else(|| anyhow::anyhow!("Truncated WebP padding"))?;
    Ok((&header[..4], chunk))
}

fn dimension24(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]) + 1
}

fn check_bitstream(kind: &[u8], chunk: &[u8], expected: Option<(u32, u32)>) -> anyhow::Result<()> {
    let size = match kind {
        b"VP8 " if chunk.len() >= 10 && chunk[3..6] == [0x9d, 0x01, 0x2a] => {
            (u32::from(le16(&chunk[6..]) & 0x3fff), u32::from(le16(&chunk[8..]) & 0x3fff))
        }
        b"VP8L" if chunk.len() >= 5 && chunk[0] == 0x2f => {
            let bits = le32(&chunk[1..]);
            ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
        }
        _ => anyhow::bail!("Invalid WebP bitstream header"),
    };
    check_raster(size.0, size.1)?;
    anyhow::ensure!(
        expected.is_none_or(|expected| size == expected),
        "WebP frame dimensions do not match the container"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(out: &mut Vec<u8>, name: &[u8; 4], bytes: &[u8]) {
        out.extend_from_slice(name);
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(bytes);
        if bytes.len() % 2 != 0 {
            out.push(0);
        }
    }

    #[test]
    fn extended_webp_checks_frame_dimensions_before_decode() {
        let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
        chunk(&mut bytes, b"VP8X", &[0; 10]); // One-pixel canvas.
        let mut frame = [0, 0, 0, 0x9d, 1, 0x2a, 0, 0, 0, 0];
        frame[6..8].copy_from_slice(&8192u16.to_le_bytes());
        frame[8..10].copy_from_slice(&8192u16.to_le_bytes());
        chunk(&mut bytes, b"VP8 ", &frame);
        let len = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&len.to_le_bytes());
        assert!(validate(&bytes, &|| false).unwrap_err().to_string().contains("dimensions exceed"));
    }

    #[test]
    fn decoder_slice_excludes_trailing_chunks() {
        let mut bytes = b"RIFF\0\0\0\0WEBP".to_vec();
        chunk(&mut bytes, b"VP8L", &[0x2f, 0, 0, 0, 0]);
        let boundary = bytes.len();
        bytes[4..8].copy_from_slice(&((boundary - 8) as u32).to_le_bytes());
        chunk(&mut bytes, b"ANMF", &[0; 12]);
        let parsed = validate(&bytes, &|| false).unwrap();
        assert_eq!(parsed, boundary);
        assert!(parsed < bytes.len());
    }
}
