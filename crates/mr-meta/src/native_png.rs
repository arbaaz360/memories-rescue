//! Native PNG EXIF writer (SRS section 4.3 "PNG photo: eXIf chunk"): the
//! same field set as JPEG, stored in a standard `eXIf` chunk (PNG 1.5
//! extension, read by Apple Photos, Windows and exiftool).
//!
//! `little_exif` encodes the EXIF/TIFF block, but its own PNG writer stores
//! it as an ImageMagick-style `zTXt` "Raw profile type exif" chunk, which is
//! not what the SRS asks for; so the chunk is assembled here: placed right
//! after `IHDR`, replacing any earlier `eXIf` or raw-profile chunk.

use std::io;
use std::path::Path;

use little_exif::filetype::FileExtension;
use little_exif::metadata::Metadata;

use crate::fields::Capture;
use crate::native_jpeg;

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const PNG_READ: FileExtension = FileExtension::PNG {
    as_zTXt_chunk: false,
};

struct Chunk<'a> {
    kind: [u8; 4],
    data: &'a [u8],
    raw: &'a [u8],
}

fn chunks(png: &[u8]) -> io::Result<Vec<Chunk<'_>>> {
    if png.len() < 8 || &png[..8] != SIGNATURE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a PNG file"));
    }
    let mut out = Vec::new();
    let mut i = 8;
    while i + 12 <= png.len() {
        let len = u32::from_be_bytes(png[i..i + 4].try_into().unwrap()) as usize;
        let end = i + 12 + len;
        if end > png.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated PNG chunk",
            ));
        }
        let kind: [u8; 4] = png[i + 4..i + 8].try_into().unwrap();
        out.push(Chunk {
            kind,
            data: &png[i + 8..i + 8 + len],
            raw: &png[i..end],
        });
        i = end;
        if &kind == b"IEND" {
            break;
        }
    }
    if out.first().map(|c| &c.kind) != Some(b"IHDR") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "PNG without IHDR",
        ));
    }
    Ok(out)
}

fn is_old_exif(c: &Chunk<'_>) -> bool {
    match &c.kind {
        b"eXIf" => true,
        b"zTXt" | b"tEXt" | b"iTXt" => c.data.starts_with(b"Raw profile type exif"),
        _ => false,
    }
}

fn chunk_bytes(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut h = crc32fast::Hasher::new();
    h.update(kind);
    h.update(data);
    out.extend_from_slice(&h.finalize().to_be_bytes());
    out
}

/// Read `src`, apply the photo field set (keeping other EXIF), write `dst`.
/// Returns `(bytes_in, bytes_out)`.
pub fn write_png(src: &Path, dst: &Path, cap: &Capture) -> io::Result<(u64, u64)> {
    let data = std::fs::read(src)?;
    let parsed = chunks(&data)?;
    // Snapchat PNGs carry no EXIF; little_exif reports that as an error.
    let mut md = Metadata::new_from_vec(&data, PNG_READ).unwrap_or_else(|_| Metadata::new());
    native_jpeg::apply(&mut md, cap);
    let mut tiff = md.encode()?;
    if tiff.starts_with(b"Exif\0\0") {
        tiff.drain(..6);
    }
    if !(tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*")) {
        return Err(io::Error::other("EXIF encoder produced no TIFF header"));
    }
    let mut out = Vec::with_capacity(data.len() + tiff.len() + 12);
    out.extend_from_slice(SIGNATURE);
    for (i, c) in parsed.iter().enumerate() {
        if is_old_exif(c) {
            continue;
        }
        out.extend_from_slice(c.raw);
        if i == 0 {
            out.extend_from_slice(&chunk_bytes(b"eXIf", &tiff));
        }
    }
    std::fs::write(dst, &out)?;
    Ok((data.len() as u64, out.len() as u64))
}

/// Read the EXIF of a PNG (from its `eXIf` chunk, or a raw-profile chunk).
pub fn read_png(path: &Path) -> io::Result<Metadata> {
    let data = std::fs::read(path)?;
    Metadata::new_from_vec(&data, PNG_READ)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_crc_matches_the_png_spec_example() {
        // IEND with no data has the well-known CRC AE 42 60 82.
        assert_eq!(
            chunk_bytes(b"IEND", &[]),
            [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]
        );
    }
}
