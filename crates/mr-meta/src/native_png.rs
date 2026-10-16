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
use crate::guard;
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
        let Some(end) = len.checked_add(i + 12).filter(|&end| end <= png.len()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated PNG chunk",
            ));
        };
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

/// The EXIF block (TIFF) a PNG carries, from its `eXIf` chunk or, failing
/// that, an ImageMagick raw-profile text chunk, checked by
/// [`guard::check_tiff`]. `Ok(None)` when there is none.
fn exif_tiff(parsed: &[Chunk<'_>]) -> io::Result<Option<Vec<u8>>> {
    let bad = |e: String| guard::invalid(format!("unreadable EXIF: {e}"));
    if let Some(c) = parsed.iter().find(|c| &c.kind == b"eXIf") {
        let tiff = c.data.strip_prefix(b"Exif\0\0").unwrap_or(c.data);
        guard::check_tiff(tiff).map_err(bad)?;
        return Ok(Some(tiff.to_vec()));
    }
    if let Some(c) = parsed.iter().find(|c| is_old_exif(c)) {
        let raw = raw_profile(c).map_err(bad)?;
        let tiff = raw.strip_prefix(b"Exif\0\0").unwrap_or(&raw).to_vec();
        guard::check_tiff(&tiff).map_err(bad)?;
        return Ok(Some(tiff));
    }
    Ok(None)
}

/// Decode an ImageMagick "Raw profile type exif" text chunk (tEXt, zTXt or
/// iTXt): optional zlib, then a name line, a decimal length and hex digits,
/// inflated and decoded within [`guard::MAX_EXIF_BYTES`].
fn raw_profile(c: &Chunk<'_>) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let nul = c.data.iter().position(|&b| b == 0).ok_or("no keyword")?;
    let rest = &c.data[nul + 1..];
    let (compressed, text): (bool, &[u8]) = match &c.kind {
        b"tEXt" => (false, rest),
        b"zTXt" => (true, rest.get(1..).ok_or("no compression method")?),
        _ => {
            // iTXt: flag, method, language NUL, translated keyword NUL, text.
            let flag = *rest.first().ok_or("no iTXt flag")?;
            let after = rest.get(2..).ok_or("short iTXt")?;
            let l = after.iter().position(|&b| b == 0).ok_or("no language")?;
            let after = &after[l + 1..];
            let t = after.iter().position(|&b| b == 0).ok_or("no translation")?;
            (flag == 1, &after[t + 1..])
        }
    };
    let text = if compressed {
        let mut out = Vec::new();
        flate2::read::ZlibDecoder::new(text)
            .take(guard::MAX_EXIF_BYTES as u64 * 3 + 64)
            .read_to_end(&mut out)
            .map_err(|e| format!("raw profile does not inflate: {e}"))?;
        out
    } else {
        text.to_vec()
    };
    let s = std::str::from_utf8(&text).map_err(|_| "raw profile is not text")?;
    let mut lines = s.trim_start_matches('\n').splitn(3, '\n');
    let _name = lines.next().ok_or("no profile name")?;
    let len: usize = lines
        .next()
        .and_then(|l| l.trim().parse().ok())
        .ok_or("no profile length")?;
    if len > guard::MAX_EXIF_BYTES {
        return Err("raw profile too large".into());
    }
    let hex: Vec<u8> = lines
        .next()
        .unwrap_or("")
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if hex.len() < len * 2 {
        return Err("raw profile shorter than its length".into());
    }
    let nibble = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    hex[..len * 2]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[hi, lo]| Some((nibble(*hi)? << 4) | nibble(*lo)?))
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(|| "raw profile is not hex".into())
}

/// A minimal PNG (IHDR, eXIf, IEND) for little_exif to read, so that it
/// never sees the rest of an untrusted file.
fn exif_only_png(parsed: &[Chunk<'_>], tiff: &[u8]) -> Vec<u8> {
    let mut v = SIGNATURE.to_vec();
    v.extend_from_slice(parsed[0].raw);
    v.extend_from_slice(&chunk_bytes(b"eXIf", tiff));
    v.extend_from_slice(&chunk_bytes(b"IEND", &[]));
    v
}

fn existing_exif(parsed: &[Chunk<'_>]) -> io::Result<Option<Metadata>> {
    match exif_tiff(parsed)? {
        Some(tiff) => {
            let png = exif_only_png(parsed, &tiff);
            guard::no_panic(|| Metadata::new_from_vec(&png, PNG_READ)).map(Some)
        }
        None => Ok(None),
    }
}

/// Read `src`, apply the photo field set (keeping other EXIF), write `dst`.
/// Returns `(bytes_in, bytes_out)`.
pub fn write_png(src: &Path, dst: &Path, cap: &Capture) -> io::Result<(u64, u64)> {
    let data = guard::read_capped(src, guard::MAX_PHOTO_BYTES, "photo")?;
    let parsed = chunks(&data)?;
    // Snapchat PNGs carry no EXIF. EXIF that is there but cannot be read
    // safely is an error, so the file is kept as it is instead of losing it.
    let mut md = existing_exif(&parsed)?.unwrap_or_else(Metadata::new);
    native_jpeg::apply(&mut md, cap);
    let mut tiff = guard::no_panic(|| md.encode())?;
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
    let data = guard::read_capped(path, guard::MAX_PHOTO_BYTES, "photo")?;
    let parsed = chunks(&data)?;
    existing_exif(&parsed)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "No EXIF data found"))
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
