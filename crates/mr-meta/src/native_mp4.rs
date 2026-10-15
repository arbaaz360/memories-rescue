//! Native MP4/MOV metadata writer: patches `mvhd`/`tkhd`/`mdhd` times in
//! place, replaces `moov/meta` with an `mdta` Keys box carrying
//! `com.apple.quicktime.creationdate` and `com.apple.quicktime.location.ISO6709`,
//! and sets `moov/udta/©xyz`. Everything outside `moov` is copied byte for
//! byte, so this is the "lossless remux" of SRS §4.3 without a demuxer.
//!
//! Because `moov` grows, every chunk offset (`stco`/`co64`) that points past
//! it is shifted by the growth. That is the whole difficulty of the native
//! path and the reason exiftool exists; this module is the M1 spike's measure
//! of how much code the native route really needs (Q-02).
//!
//! Known limits, deliberate for the spike: an existing `moov/meta` is replaced
//! rather than merged (other Keys are lost); `stco` offsets that would exceed
//! 4 GiB after the shift are an error instead of a `co64` upgrade; only the
//! first `moov` is handled.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::fields::Capture;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("not an ISO BMFF file (no ftyp/moov)")]
    NotMp4,
    #[error("no moov atom")]
    NoMoov,
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
}

/// What the writer did, for the benchmark and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Patched {
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// `moov` preceded `mdat` in the source, so chunk offsets had to shift.
    pub moov_first: bool,
    pub offsets_shifted: usize,
}

const META_KEY_CREATION_DATE: &str = "com.apple.quicktime.creationdate";
const META_KEY_LOCATION: &str = "com.apple.quicktime.location.ISO6709";
/// `©xyz` language field; exiftool writes 0 (undetermined) and Apple's readers accept it.
const XYZ_LANG: u16 = 0;

/// Copy `src` to `dst` with the §4.3 video field set applied.
pub fn write_mp4(src: &Path, dst: &Path, cap: &Capture) -> Result<Patched, Error> {
    let mut input = BufReader::with_capacity(1 << 20, File::open(src)?);
    let len = input.get_ref().metadata()?.len();
    let boxes = top_level(&mut input, len)?;
    if !boxes.iter().any(|b| &b.typ == b"ftyp") && !boxes.iter().any(|b| &b.typ == b"moov") {
        return Err(Error::NotMp4);
    }
    let moov = boxes
        .iter()
        .find(|b| &b.typ == b"moov")
        .ok_or(Error::NoMoov)?;
    let moov_first = boxes
        .iter()
        .any(|b| &b.typ == b"mdat" && b.start > moov.start);

    // Old moov payload.
    input.seek(SeekFrom::Start(moov.start + moov.header_len))?;
    let payload_len = (moov.size - moov.header_len) as usize;
    let mut old_payload = vec![0u8; payload_len];
    input.read_exact(&mut old_payload)?;

    // New payload: original children minus meta/udta(©xyz), plus ours.
    let mut new_payload = Vec::with_capacity(payload_len + 512);
    let mut udta_children: Vec<Vec<u8>> = Vec::new();
    for child in children(&old_payload) {
        match &child.typ {
            b"meta" => {} // replaced below
            b"udta" => {
                let udta = &old_payload[child.payload.clone()];
                for grand in children(udta) {
                    if &grand.typ != b"\xa9xyz" {
                        udta_children.push(udta[grand.whole.clone()].to_vec());
                    }
                }
            }
            _ => new_payload.extend_from_slice(&old_payload[child.whole.clone()]),
        }
    }
    let mut keys = vec![(META_KEY_CREATION_DATE, cap.iso8601_local())];
    if let Some(iso) = cap.iso6709() {
        keys.push((META_KEY_LOCATION, iso.clone()));
        udta_children.push(xyz_box(&iso));
    }
    new_payload.extend_from_slice(&meta_box(&keys));
    if !udta_children.is_empty() {
        new_payload.extend_from_slice(&boxed(b"udta", &udta_children.concat()));
    }

    // Times and chunk offsets, on the final layout.
    let new_moov_size = 8 + new_payload.len() as u64;
    if new_moov_size > u32::MAX as u64 {
        return Err(Error::Unsupported("moov larger than 4 GiB"));
    }
    let delta = new_moov_size as i64 - moov.size as i64;
    let old_moov_end = moov.start + moov.size;
    let qt = cap.qt_seconds();
    let mut offset_boxes = Vec::new();
    let payload_len_new = new_payload.len();
    patch_tree(&mut new_payload, 0, payload_len_new, qt, &mut offset_boxes);
    let mut shifted = 0usize;
    if delta != 0 {
        for (pos, is64) in offset_boxes {
            shifted += shift_offsets(&mut new_payload, pos, is64, old_moov_end, delta)?;
        }
    }

    // Write: prefix, new moov, suffix.
    let mut out = BufWriter::with_capacity(1 << 20, File::create(dst)?);
    input.seek(SeekFrom::Start(0))?;
    io::copy(&mut (&mut input).take(moov.start), &mut out)?;
    out.write_all(&(new_moov_size as u32).to_be_bytes())?;
    out.write_all(b"moov")?;
    out.write_all(&new_payload)?;
    input.seek(SeekFrom::Start(old_moov_end))?;
    io::copy(&mut input, &mut out)?;
    out.flush()?;
    let bytes_out = out.get_ref().metadata()?.len();
    Ok(Patched {
        bytes_in: len,
        bytes_out,
        moov_first,
        offsets_shifted: shifted,
    })
}

// ---- box plumbing -----------------------------------------------------------

#[derive(Debug, Clone)]
struct TopBox {
    typ: [u8; 4],
    start: u64,
    header_len: u64,
    size: u64,
}

fn top_level<R: Read + Seek>(r: &mut R, len: u64) -> Result<Vec<TopBox>, Error> {
    let mut boxes = Vec::new();
    let mut pos = 0u64;
    let mut hdr = [0u8; 16];
    while pos + 8 <= len {
        r.seek(SeekFrom::Start(pos))?;
        let n = r.read(&mut hdr)?;
        if n < 8 {
            break;
        }
        let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as u64;
        let typ: [u8; 4] = hdr[4..8].try_into().unwrap();
        let (size, header_len) = match size32 {
            0 => (len - pos, 8),
            1 => {
                if n < 16 {
                    return Err(Error::NotMp4);
                }
                (u64::from_be_bytes(hdr[8..16].try_into().unwrap()), 16)
            }
            s => (s, 8),
        };
        if size < header_len || pos + size > len {
            return Err(Error::NotMp4);
        }
        boxes.push(TopBox {
            typ,
            start: pos,
            header_len,
            size,
        });
        pos += size;
    }
    Ok(boxes)
}

struct Child {
    typ: [u8; 4],
    whole: std::ops::Range<usize>,
    payload: std::ops::Range<usize>,
}

/// Children of a container payload. Tolerant: stops at the first malformed box.
fn children(buf: &[u8]) -> Vec<Child> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= buf.len() {
        let size32 = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        let typ: [u8; 4] = buf[pos + 4..pos + 8].try_into().unwrap();
        let (size, hdr) = match size32 {
            0 => (buf.len() - pos, 8),
            1 if pos + 16 <= buf.len() => (
                u64::from_be_bytes(buf[pos + 8..pos + 16].try_into().unwrap()) as usize,
                16,
            ),
            1 => break,
            s => (s, 8),
        };
        if size < hdr || pos + size > buf.len() {
            break;
        }
        out.push(Child {
            typ,
            whole: pos..pos + size,
            payload: pos + hdr..pos + size,
        });
        pos += size;
    }
    out
}

fn boxed(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + payload.len());
    v.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
    v.extend_from_slice(typ);
    v.extend_from_slice(payload);
    v
}

fn full_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + payload.len());
    p.extend_from_slice(&[0, 0, 0, 0]); // version 0, flags 0
    p.extend_from_slice(payload);
    boxed(typ, &p)
}

/// `moov/meta` with an `mdta` handler, a `keys` table and an `ilst` whose
/// item types are the 1-based key indices. Written the QuickTime way, as a
/// plain box with no version/flags and a 32-byte `hdlr` with an empty name,
/// byte for byte what exiftool writes for `-Keys:*` (ffmpeg's demuxer and
/// exiftool read both this and the ISO full-box form).
fn meta_box(keys: &[(&str, String)]) -> Vec<u8> {
    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&[0, 0, 0, 0]); // pre_defined
    hdlr.extend_from_slice(b"mdta");
    hdlr.extend_from_slice(&[0u8; 12]); // reserved; empty name, no terminator
    let hdlr = full_box(b"hdlr", &hdlr);

    let mut keys_payload = Vec::new();
    keys_payload.extend_from_slice(&(keys.len() as u32).to_be_bytes());
    for (name, _) in keys {
        keys_payload.extend_from_slice(&((8 + name.len()) as u32).to_be_bytes());
        keys_payload.extend_from_slice(b"mdta");
        keys_payload.extend_from_slice(name.as_bytes());
    }
    let keys_box = full_box(b"keys", &keys_payload);

    let mut ilst_payload = Vec::new();
    for (i, (_, value)) in keys.iter().enumerate() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u32.to_be_bytes()); // type indicator: UTF-8
        data.extend_from_slice(&0u32.to_be_bytes()); // locale
        data.extend_from_slice(value.as_bytes());
        let data_box = boxed(b"data", &data);
        let item_type = ((i + 1) as u32).to_be_bytes();
        ilst_payload.extend_from_slice(&boxed(&item_type, &data_box));
    }
    let ilst = boxed(b"ilst", &ilst_payload);

    boxed(b"meta", &[hdlr, keys_box, ilst].concat())
}

/// `udta/©xyz`: 16-bit length, 16-bit packed language, ISO 6709 string.
fn xyz_box(iso6709: &str) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + iso6709.len());
    p.extend_from_slice(&(iso6709.len() as u16).to_be_bytes());
    p.extend_from_slice(&XYZ_LANG.to_be_bytes());
    p.extend_from_slice(iso6709.as_bytes());
    boxed(b"\xa9xyz", &p)
}

/// Walk a box sequence, patch `mvhd`/`tkhd`/`mdhd` times, and record the
/// payload offsets of every `stco`/`co64`.
fn patch_tree(buf: &mut [u8], base: usize, end: usize, qt: u64, offsets: &mut Vec<(usize, bool)>) {
    let kids: Vec<([u8; 4], std::ops::Range<usize>)> = children(&buf[base..end])
        .into_iter()
        .map(|c| (c.typ, base + c.payload.start..base + c.payload.end))
        .collect();
    for (typ, range) in kids {
        match &typ {
            b"mvhd" | b"tkhd" | b"mdhd" => patch_times(&mut buf[range], qt),
            b"trak" | b"mdia" | b"minf" | b"stbl" => {
                patch_tree(buf, range.start, range.end, qt, offsets)
            }
            b"stco" => offsets.push((range.start, false)),
            b"co64" => offsets.push((range.start, true)),
            _ => {}
        }
    }
}

/// Full box: version(1) flags(3) then creation and modification time,
/// 32-bit in version 0 and 64-bit in version 1, at the same position for
/// `mvhd`, `tkhd` and `mdhd`.
fn patch_times(payload: &mut [u8], qt: u64) {
    match payload.first() {
        Some(0) if payload.len() >= 12 => {
            let v = (qt.min(u32::MAX as u64) as u32).to_be_bytes();
            payload[4..8].copy_from_slice(&v);
            payload[8..12].copy_from_slice(&v);
        }
        Some(1) if payload.len() >= 20 => {
            let v = qt.to_be_bytes();
            payload[4..12].copy_from_slice(&v);
            payload[12..20].copy_from_slice(&v);
        }
        _ => {}
    }
}

/// Shift every chunk offset at or beyond `boundary` by `delta`. Returns how many moved.
fn shift_offsets(
    buf: &mut [u8],
    pos: usize,
    is64: bool,
    boundary: u64,
    delta: i64,
) -> Result<usize, Error> {
    let count = u32::from_be_bytes(buf[pos + 4..pos + 8].try_into().unwrap()) as usize;
    let mut moved = 0;
    let width = if is64 { 8 } else { 4 };
    let mut p = pos + 8;
    for _ in 0..count {
        if p + width > buf.len() {
            return Err(Error::Unsupported("truncated chunk offset table"));
        }
        let old = if is64 {
            u64::from_be_bytes(buf[p..p + 8].try_into().unwrap())
        } else {
            u32::from_be_bytes(buf[p..p + 4].try_into().unwrap()) as u64
        };
        if old >= boundary {
            let new = (old as i64 + delta) as u64;
            if is64 {
                buf[p..p + 8].copy_from_slice(&new.to_be_bytes());
            } else {
                if new > u32::MAX as u64 {
                    return Err(Error::Unsupported(
                        "stco offset would exceed 4 GiB; co64 upgrade not implemented",
                    ));
                }
                buf[p..p + 4].copy_from_slice(&(new as u32).to_be_bytes());
            }
            moved += 1;
        }
        p += width;
    }
    Ok(moved)
}

// ---- read-back for tests ------------------------------------------------------

/// A small reader over the writer's own output, so unit tests do not need exiftool.
#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub mvhd_creation: Option<u64>,
    pub tkhd_creations: Vec<u64>,
    pub mdhd_creations: Vec<u64>,
    pub keys: Vec<(String, String)>,
    pub xyz: Option<String>,
    pub chunk_offsets: Vec<u64>,
    pub top_level: Vec<String>,
}

pub fn summarize(path: &Path) -> Result<Summary, Error> {
    let mut f = BufReader::new(File::open(path)?);
    let len = f.get_ref().metadata()?.len();
    let boxes = top_level(&mut f, len)?;
    let mut s = Summary {
        top_level: boxes
            .iter()
            .map(|b| String::from_utf8_lossy(&b.typ).into_owned())
            .collect(),
        ..Default::default()
    };
    let moov = boxes
        .iter()
        .find(|b| &b.typ == b"moov")
        .ok_or(Error::NoMoov)?;
    f.seek(SeekFrom::Start(moov.start + moov.header_len))?;
    let mut payload = vec![0u8; (moov.size - moov.header_len) as usize];
    f.read_exact(&mut payload)?;
    summarize_tree(&payload, &mut s);
    Ok(s)
}

fn read_time(payload: &[u8]) -> Option<u64> {
    match payload.first() {
        Some(0) if payload.len() >= 8 => {
            Some(u32::from_be_bytes(payload[4..8].try_into().unwrap()) as u64)
        }
        Some(1) if payload.len() >= 12 => {
            Some(u64::from_be_bytes(payload[4..12].try_into().unwrap()))
        }
        _ => None,
    }
}

fn summarize_tree(buf: &[u8], s: &mut Summary) {
    for c in children(buf) {
        let p = &buf[c.payload.clone()];
        match &c.typ {
            b"mvhd" => s.mvhd_creation = read_time(p),
            b"tkhd" => s.tkhd_creations.extend(read_time(p)),
            b"mdhd" => s.mdhd_creations.extend(read_time(p)),
            b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" => summarize_tree(p, s),
            b"stco" | b"co64" if p.len() >= 8 => {
                let n = u32::from_be_bytes(p[4..8].try_into().unwrap()) as usize;
                let w = if &c.typ == b"co64" { 8 } else { 4 };
                for i in 0..n {
                    let q = 8 + i * w;
                    if q + w > p.len() {
                        break;
                    }
                    s.chunk_offsets.push(if w == 8 {
                        u64::from_be_bytes(p[q..q + 8].try_into().unwrap())
                    } else {
                        u32::from_be_bytes(p[q..q + 4].try_into().unwrap()) as u64
                    });
                }
            }
            b"\xa9xyz" if p.len() >= 4 => {
                let n = u16::from_be_bytes([p[0], p[1]]) as usize;
                s.xyz = Some(String::from_utf8_lossy(&p[4..(4 + n).min(p.len())]).into_owned());
            }
            b"meta" => {
                let inner = if p.len() >= 4 && p[..4] == [0, 0, 0, 0] {
                    &p[4..]
                } else {
                    p
                };
                let mut names = Vec::new();
                let mut values = Vec::new();
                for m in children(inner) {
                    let mp = &inner[m.payload.clone()];
                    match &m.typ {
                        b"keys" if mp.len() >= 8 => {
                            let n = u32::from_be_bytes(mp[4..8].try_into().unwrap()) as usize;
                            let mut q = 8;
                            for _ in 0..n {
                                if q + 8 > mp.len() {
                                    break;
                                }
                                let ks =
                                    u32::from_be_bytes(mp[q..q + 4].try_into().unwrap()) as usize;
                                names.push(
                                    String::from_utf8_lossy(&mp[q + 8..(q + ks).min(mp.len())])
                                        .into_owned(),
                                );
                                q += ks;
                            }
                        }
                        b"ilst" => {
                            for item in children(mp) {
                                let ip = &mp[item.payload.clone()];
                                let idx = u32::from_be_bytes(item.typ) as usize;
                                for d in children(ip) {
                                    if &d.typ == b"data" && d.payload.len() >= 8 {
                                        let dp = &ip[d.payload.clone()];
                                        values.push((
                                            idx,
                                            String::from_utf8_lossy(&dp[8..]).into_owned(),
                                        ));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                for (idx, v) in values {
                    if let Some(name) = names.get(idx.wrapping_sub(1)) {
                        s.keys.push((name.clone(), v));
                    }
                }
            }
            _ => {}
        }
    }
}
