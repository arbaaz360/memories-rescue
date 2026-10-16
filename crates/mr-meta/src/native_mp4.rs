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
//! When the shift would push a 32-bit `stco` offset past 4 GiB, that table
//! is upgraded to `co64` (and the growth this adds is part of the shift). An
//! existing `moov/meta` with an `mdta` handler is merged: its other keys and
//! values are kept, ours replace or join them. Limits (SRS v0.3 section
//! 4.3): a `moov/meta` with another handler is replaced; only the first
//! `moov` is handled; `moov` is read into memory (a few MB even for an hour
//! of video, since it holds only the sample tables).

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
    /// `stco` tables rewritten as `co64` because an offset passed 4 GiB.
    pub tables_upgraded: usize,
    /// Keys kept from an existing `mdta` `moov/meta`.
    pub keys_kept: usize,
}

const META_KEY_CREATION_DATE: &str = "com.apple.quicktime.creationdate";
use crate::guard::MAX_MOOV_BYTES;
/// Real files have a handful; a crafted one could list millions of tiny boxes.
const MAX_TOP_LEVEL_BOXES: usize = 4096;
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

    // Old moov payload (sample tables only; capped so a crafted size cannot
    // become an allocation).
    let old_payload = read_moov(&mut input, moov)?;
    let payload_len = old_payload.len();

    // New payload: original children minus meta/udta(©xyz), plus ours.
    let mut new_payload = Vec::with_capacity(payload_len + 512);
    let mut udta_children: Vec<Vec<u8>> = Vec::new();
    let mut existing_keys: Vec<KeyEntry> = Vec::new();
    for child in children(&old_payload) {
        match &child.typ {
            // Merged below when it is an `mdta` table, replaced otherwise.
            b"meta" => existing_keys = mdta_entries(&old_payload[child.payload.clone()]),
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
    let ours: Vec<&str> = keys.iter().map(|(k, _)| *k).collect();
    let keys_kept = existing_keys
        .iter()
        .filter(|e| !(e.namespace == *b"mdta" && ours.contains(&e.name.as_str())))
        .count();
    new_payload.extend_from_slice(&meta_box_merged(existing_keys, &keys));
    if !udta_children.is_empty() {
        new_payload.extend_from_slice(&boxed(b"udta", &udta_children.concat()));
    }

    // co64 upgrade: if the shift could push a 32-bit offset past 4 GiB,
    // rewrite those tables as co64 first. The bound assumes every table is
    // upgraded, so one pass is enough: the real growth is never larger.
    let old_moov_end = moov.start + moov.size;
    let delta0 = 8 + new_payload.len() as i64 - moov.size as i64;
    let max_delta = delta0 + 4 * stco_entries(&new_payload) as i64;
    let mut tables_upgraded = 0;
    if max_delta > 0 {
        let overflows = |stco: &[u8]| {
            table_entries(stco, false)
                .any(|o| o >= old_moov_end && o as i64 + max_delta > u32::MAX as i64)
        };
        let (rebuilt, n) = upgrade_tables(&new_payload, &overflows);
        if n > 0 {
            new_payload = rebuilt;
            tables_upgraded = n;
        }
    }

    // Times and chunk offsets, on the final layout.
    let new_moov_size = 8 + new_payload.len() as u64;
    if new_moov_size > u32::MAX as u64 {
        return Err(Error::Unsupported("moov larger than 4 GiB"));
    }
    let delta = new_moov_size as i64 - moov.size as i64;
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
        tables_upgraded,
        keys_kept,
    })
}

/// One key of an `mdta` `keys` table with its `ilst` item (the item's
/// children, usually one `data` box), if it has a value.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyEntry {
    namespace: [u8; 4],
    name: String,
    item: Option<Vec<u8>>,
}

/// The keys and values of a `moov/meta` payload whose handler is `mdta`;
/// empty for any other handler (or a malformed box).
fn mdta_entries(meta_payload: &[u8]) -> Vec<KeyEntry> {
    // QuickTime writes `meta` as a plain box, ISO as a full box.
    let inner = if meta_payload.len() >= 4 && meta_payload[..4] == [0, 0, 0, 0] {
        &meta_payload[4..]
    } else {
        meta_payload
    };
    let kids = children(inner);
    let is_mdta = kids.iter().any(|c| {
        let p = &inner[c.payload.clone()];
        &c.typ == b"hdlr" && p.len() >= 12 && &p[8..12] == b"mdta"
    });
    if !is_mdta {
        return Vec::new();
    }
    let mut entries: Vec<KeyEntry> = Vec::new();
    let mut items: Vec<(usize, Vec<u8>)> = Vec::new();
    for c in &kids {
        let p = &inner[c.payload.clone()];
        match &c.typ {
            b"keys" if p.len() >= 8 => {
                let n = u32::from_be_bytes(p[4..8].try_into().unwrap()) as usize;
                let mut q = 8;
                for _ in 0..n {
                    if q + 8 > p.len() {
                        break;
                    }
                    let size = u32::from_be_bytes(p[q..q + 4].try_into().unwrap()) as usize;
                    if size < 8 || q + size > p.len() {
                        break;
                    }
                    entries.push(KeyEntry {
                        namespace: p[q + 4..q + 8].try_into().unwrap(),
                        name: String::from_utf8_lossy(&p[q + 8..q + size]).into_owned(),
                        item: None,
                    });
                    q += size;
                }
            }
            b"ilst" => {
                for item in children(p) {
                    items.push((
                        u32::from_be_bytes(item.typ) as usize,
                        p[item.payload].to_vec(),
                    ));
                }
            }
            _ => {}
        }
    }
    for (idx, payload) in items {
        if let Some(e) = idx.checked_sub(1).and_then(|i| entries.get_mut(i)) {
            e.item = Some(payload);
        }
    }
    entries
}

/// A UTF-8 `data` box as an `ilst` item payload.
fn utf8_item(value: &str) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&1u32.to_be_bytes()); // type indicator: UTF-8
    data.extend_from_slice(&0u32.to_be_bytes()); // locale
    data.extend_from_slice(value.as_bytes());
    boxed(b"data", &data)
}

/// `meta_box` over the keys an existing `mdta` table had: theirs kept in
/// their order, ours replacing a key of the same name or appended.
fn meta_box_merged(mut existing: Vec<KeyEntry>, ours: &[(&str, String)]) -> Vec<u8> {
    for (name, value) in ours {
        let item = Some(utf8_item(value));
        match existing
            .iter_mut()
            .find(|e| e.namespace == *b"mdta" && e.name == *name)
        {
            Some(e) => e.item = item,
            None => existing.push(KeyEntry {
                namespace: *b"mdta",
                name: name.to_string(),
                item,
            }),
        }
    }
    existing.retain(|e| e.item.is_some());
    meta_box_entries(&existing)
}

/// Entries of every `stco` in a `moov` payload.
fn stco_entries(buf: &[u8]) -> usize {
    let mut n = 0;
    for c in children(buf) {
        let p = &buf[c.payload.clone()];
        match &c.typ {
            b"trak" | b"mdia" | b"minf" | b"stbl" => n += stco_entries(p),
            b"stco" if p.len() >= 8 => {
                n += (u32::from_be_bytes(p[4..8].try_into().unwrap()) as usize)
                    .min((p.len() - 8) / 4)
            }
            _ => {}
        }
    }
    n
}

/// The offsets of a `stco` (or `co64`) payload.
fn table_entries(payload: &[u8], is64: bool) -> impl Iterator<Item = u64> + '_ {
    let n = if payload.len() >= 8 {
        u32::from_be_bytes(payload[4..8].try_into().unwrap()) as usize
    } else {
        0
    };
    let w = if is64 { 8 } else { 4 };
    (0..n).map_while(move |i| {
        let q = 8 + i * w;
        let b = payload.get(q..q + w)?;
        Some(if is64 {
            u64::from_be_bytes(b.try_into().unwrap())
        } else {
            u32::from_be_bytes(b.try_into().unwrap()) as u64
        })
    })
}

/// Rebuild the containers down to `stbl`, rewriting each `stco` for which
/// `upgrade` says so as a `co64` with the same offsets. Bytes after the
/// last well-formed child of a container are kept. Returns the new payload
/// and how many tables changed.
fn upgrade_tables(buf: &[u8], upgrade: &dyn Fn(&[u8]) -> bool) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(buf.len() + 1024);
    let mut n = 0;
    let mut end = 0;
    for c in children(buf) {
        end = c.whole.end;
        let p = &buf[c.payload.clone()];
        match &c.typ {
            b"trak" | b"mdia" | b"minf" | b"stbl" => {
                let (inner, k) = upgrade_tables(p, upgrade);
                n += k;
                out.extend_from_slice(&boxed(&c.typ, &inner));
            }
            b"stco" if p.len() >= 8 && upgrade(p) => {
                let mut q = Vec::with_capacity(p.len() * 2);
                q.extend_from_slice(&p[..8]); // version, flags, entry count
                for o in table_entries(p, false) {
                    q.extend_from_slice(&o.to_be_bytes());
                }
                out.extend_from_slice(&boxed(b"co64", &q));
                n += 1;
            }
            _ => out.extend_from_slice(&buf[c.whole.clone()]),
        }
    }
    out.extend_from_slice(&buf[end..]);
    (out, n)
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
    while pos.checked_add(8).is_some_and(|end| end <= len) {
        if boxes.len() >= MAX_TOP_LEVEL_BOXES {
            return Err(Error::Unsupported("too many top-level boxes"));
        }
        r.seek(SeekFrom::Start(pos))?;
        r.read_exact(&mut hdr[..8])?;
        let size32 = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as u64;
        let typ: [u8; 4] = hdr[4..8].try_into().unwrap();
        let (size, header_len) = match size32 {
            0 => (len - pos, 8),
            1 => {
                if pos + 16 > len {
                    return Err(Error::NotMp4);
                }
                r.read_exact(&mut hdr[8..16])?;
                (u64::from_be_bytes(hdr[8..16].try_into().unwrap()), 16)
            }
            s => (s, 8),
        };
        if size < header_len || pos.checked_add(size).is_none_or(|end| end > len) {
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
        if size < hdr || pos.checked_add(size).is_none_or(|end| end > buf.len()) {
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
fn meta_box_entries(entries: &[KeyEntry]) -> Vec<u8> {
    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&[0, 0, 0, 0]); // pre_defined
    hdlr.extend_from_slice(b"mdta");
    hdlr.extend_from_slice(&[0u8; 12]); // reserved; empty name, no terminator
    let hdlr = full_box(b"hdlr", &hdlr);

    let mut keys_payload = Vec::new();
    keys_payload.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for e in entries {
        keys_payload.extend_from_slice(&((8 + e.name.len()) as u32).to_be_bytes());
        keys_payload.extend_from_slice(&e.namespace);
        keys_payload.extend_from_slice(e.name.as_bytes());
    }
    let keys_box = full_box(b"keys", &keys_payload);

    let mut ilst_payload = Vec::new();
    for (i, e) in entries.iter().enumerate() {
        let item_type = ((i + 1) as u32).to_be_bytes();
        ilst_payload.extend_from_slice(&boxed(&item_type, e.item.as_deref().unwrap_or(&[])));
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
            // Only tables with their version, flags and count; shorter ones
            // are left alone (and fail the read-back).
            b"stco" if range.len() >= 8 => offsets.push((range.start, false)),
            b"co64" if range.len() >= 8 => offsets.push((range.start, true)),
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
            let new = old
                .checked_add_signed(delta)
                .ok_or(Error::Unsupported("chunk offset out of range"))?;
            if is64 {
                buf[p..p + 8].copy_from_slice(&new.to_be_bytes());
            } else {
                if new > u32::MAX as u64 {
                    // Cannot happen after the co64 upgrade; kept as a guard.
                    return Err(Error::Unsupported("stco offset would exceed 4 GiB"));
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
    /// How many chunk offset tables are `co64`.
    pub co64_tables: usize,
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
    let payload = read_moov(&mut f, moov)?;
    summarize_tree(&payload, &mut s);
    Ok(s)
}

/// The `moov` payload, refused above [`MAX_MOOV_BYTES`].
fn read_moov<R: Read + Seek>(r: &mut R, moov: &TopBox) -> Result<Vec<u8>, Error> {
    let len = moov.size - moov.header_len;
    if len > MAX_MOOV_BYTES {
        return Err(Error::Unsupported("moov larger than 256 MiB"));
    }
    r.seek(SeekFrom::Start(moov.start + moov.header_len))?;
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload)?;
    Ok(payload)
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
                s.co64_tables += usize::from(&c.typ == b"co64");
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
                                let Some(end) =
                                    q.checked_add(ks).filter(|&e| ks >= 8 && e <= mp.len())
                                else {
                                    break;
                                };
                                names.push(String::from_utf8_lossy(&mp[q + 8..end]).into_owned());
                                q = end;
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn cap() -> Capture {
        let utc = chrono::Utc
            .with_ymd_and_hms(2021, 12, 25, 18, 42, 7)
            .unwrap();
        Capture::new(utc, 0, Some((51.50735, -0.12776)))
    }

    fn times_box(typ: &[u8; 4]) -> Vec<u8> {
        // version 0: creation, modification, then 12 bytes of the rest.
        full_box(typ, &[0u8; 20])
    }

    fn stco(offsets: &[u32]) -> Vec<u8> {
        let mut p = (offsets.len() as u32).to_be_bytes().to_vec();
        for o in offsets {
            p.extend_from_slice(&o.to_be_bytes());
        }
        full_box(b"stco", &p)
    }

    /// ftyp, moov (one trak with the given stco, optional extra moov
    /// children), mdat. `moov` first, so offsets past it shift.
    fn mp4(offsets: &[u32], extra: &[u8]) -> Vec<u8> {
        let stbl = boxed(b"stbl", &stco(offsets));
        let minf = boxed(b"minf", &stbl);
        let mdia = boxed(b"mdia", &[times_box(b"mdhd"), minf].concat());
        let trak = boxed(b"trak", &[times_box(b"tkhd"), mdia].concat());
        let moov = boxed(
            b"moov",
            &[times_box(b"mvhd"), trak, extra.to_vec()].concat(),
        );
        let ftyp = boxed(b"ftyp", b"isom\0\0\x02\0isom");
        [ftyp, moov, boxed(b"mdat", &[7u8; 64])].concat()
    }

    fn roundtrip(name: &str, bytes: &[u8]) -> (Patched, Summary) {
        let dir = std::env::temp_dir().join(format!("mr-meta-mp4-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (src, dst) = (dir.join("in.mp4"), dir.join("out.mp4"));
        std::fs::write(&src, bytes).unwrap();
        let p = write_mp4(&src, &dst, &cap()).unwrap();
        let s = summarize(&dst).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        (p, s)
    }

    #[test]
    fn offsets_past_4_gib_upgrade_the_table_to_co64() {
        let base = mp4(&[0], b"");
        let moov_end = (base.len() - 72) as u32; // ftyp + moov; mdat is 72 bytes
        let near = u32::MAX - 40;
        let file = mp4(&[moov_end + 8, near], b"");
        let (p, s) = roundtrip("co64", &file);
        assert_eq!(p.tables_upgraded, 1);
        assert_eq!(s.co64_tables, 1);
        let delta = p.bytes_out as i64 - p.bytes_in as i64;
        assert!(delta > 40, "moov grew past the gap: {delta}");
        assert_eq!(
            s.chunk_offsets,
            vec![
                (moov_end + 8) as u64 + delta as u64,
                near as u64 + delta as u64
            ],
            "both offsets shifted by the full growth, including the co64 upgrade"
        );
        assert!(s.chunk_offsets[1] > u32::MAX as u64);
        assert_eq!(s.mvhd_creation, Some(cap().qt_seconds()));
    }

    #[test]
    fn offsets_well_under_4_gib_stay_stco() {
        let base = mp4(&[0], b"");
        let moov_end = (base.len() - 72) as u32;
        let (p, s) = roundtrip("stco", &mp4(&[moov_end + 8, 1 << 30], b""));
        assert_eq!(p.tables_upgraded, 0);
        assert_eq!(s.co64_tables, 0);
        assert_eq!(s.chunk_offsets.len(), 2);
    }

    #[test]
    fn an_existing_mdta_table_is_merged_not_replaced() {
        let theirs = vec![
            KeyEntry {
                namespace: *b"mdta",
                name: "com.apple.quicktime.make".into(),
                item: Some(utf8_item("Apple")),
            },
            KeyEntry {
                namespace: *b"mdta",
                name: META_KEY_CREATION_DATE.into(),
                item: Some(utf8_item("2000-01-01T00:00:00+0000")),
            },
        ];
        let file = mp4(&[], &meta_box_entries(&theirs));
        let (p, s) = roundtrip("merge", &file);
        assert_eq!(p.keys_kept, 1);
        let keys: std::collections::HashMap<String, String> = s.keys.into_iter().collect();
        assert_eq!(keys["com.apple.quicktime.make"], "Apple", "their key kept");
        assert_eq!(
            keys[META_KEY_CREATION_DATE],
            cap().iso8601_local(),
            "ours replaces theirs"
        );
        assert!(keys.contains_key(META_KEY_LOCATION), "ours added");
        assert_eq!(keys.len(), 3);
    }

    #[test]
    fn a_meta_box_with_another_handler_is_replaced() {
        let mut hdlr = vec![0u8; 4];
        hdlr.extend_from_slice(b"mdir");
        hdlr.extend_from_slice(&[0u8; 12]);
        let meta = boxed(b"meta", &full_box(b"hdlr", &hdlr));
        let (p, s) = roundtrip("mdir", &mp4(&[], &meta));
        assert_eq!(p.keys_kept, 0);
        assert_eq!(s.keys.len(), 2, "creation date and location");
    }
}
