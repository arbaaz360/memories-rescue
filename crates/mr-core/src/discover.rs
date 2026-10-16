//! Input discovery and part classification (FR-01, FR-02, FR-03, FR-05).
//!
//! Inputs are `.zip` files and folders. A ZIP is indexed from its central
//! directory (see [`crate::zipindex`]); a folder is listed. A part is
//! *primary* when it holds `json/memories_history.json`. Parts are grouped
//! into exports by their base name (`mydata~1729000000000`, `-2`, `-3`, …)
//! and, for renamed files such as `mydata (1).zip`, by content: the part is
//! attached to the export whose media IDs it overlaps (an overlay in one part
//! pairs with a main in another), or to the only export there is.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::NaiveDate;
use regex::Regex;

use crate::filename::{ParsedName, parse_entry_path};
use crate::issue::{Code, Issue};
use crate::zipindex::{Integrity, UnsafeReason, ZipIndex, ZipIndexError};

pub const JSON_ENTRY: &str = "json/memories_history.json";
pub const MEDIA_DIR: &str = "memories/";

/// The pieces of an export part's file name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartName {
    /// `mydata~1729000000000` for `mydata~1729000000000-2 (1).zip`.
    pub base: String,
    /// The `-N` suffix; `None` for the unsuffixed (primary) part.
    pub number: Option<u32>,
    /// A browser's ` (1)` duplicate suffix.
    pub copy: Option<u32>,
}

impl PartName {
    /// Base names of the export's own shape (`mydata~<digits>`) identify an
    /// export on their own; anything else needs content to be grouped.
    pub fn is_strong(&self) -> bool {
        STRONG_BASE.is_match(&self.base)
    }
}

static PART_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<base>.+?)(?:-(?P<n>[0-9]{1,3}))?(?: \((?P<c>[0-9]+)\))?\.(?i:zip)$")
        .expect("valid regex")
});
static STRONG_BASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_.-]+~[0-9]{6,}$").expect("valid regex"));

/// Parse a ZIP file name into base, part number and copy suffix.
pub fn parse_part_name(file_name: &str) -> Option<PartName> {
    let caps = PART_NAME.captures(file_name)?;
    Some(PartName {
        base: caps["base"].to_string(),
        number: caps.name("n").and_then(|m| m.as_str().parse().ok()),
        copy: caps.name("c").and_then(|m| m.as_str().parse().ok()),
    })
}

/// One file inside a part, however the part is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartEntry {
    /// Order within the part: ZIP entry order, or name order for a folder.
    pub index: usize,
    /// Path relative to the export root with `/` separators.
    pub name: String,
    pub size: u64,
    /// Known for ZIP entries; computed on demand for folder files.
    pub crc32: Option<u32>,
    /// Regular, safe and complete.
    pub usable: bool,
}

/// A listed folder: an export that was already extracted.
#[derive(Debug)]
pub struct FolderIndex {
    pub root: PathBuf,
    pub entries: Vec<PartEntry>,
}

impl FolderIndex {
    fn list(root: &Path) -> io::Result<FolderIndex> {
        let mut entries = Vec::new();
        for top in ["index.html", JSON_ENTRY] {
            let p = root.join(top);
            if let Ok(meta) = fs::metadata(&p)
                && meta.is_file()
            {
                entries.push((top.to_string(), meta.len()));
            }
        }
        let media = root.join("memories");
        if media.is_dir() {
            let mut names: Vec<(String, u64)> = fs::read_dir(&media)?
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    let meta = e.metadata().ok()?;
                    if !meta.is_file() {
                        return None;
                    }
                    let name = e.file_name().to_string_lossy().into_owned();
                    Some((format!("{MEDIA_DIR}{name}"), meta.len()))
                })
                .collect();
            names.sort();
            entries.extend(names);
        }
        Ok(FolderIndex {
            root: root.to_path_buf(),
            entries: entries
                .into_iter()
                .enumerate()
                .map(|(index, (name, size))| PartEntry {
                    index,
                    name,
                    size,
                    crc32: None,
                    usable: true,
                })
                .collect(),
        })
    }

    pub fn path_of(&self, entry: &PartEntry) -> PathBuf {
        self.root
            .join(entry.name.replace('/', std::path::MAIN_SEPARATOR_STR))
    }
}

/// Where a part's bytes live.
#[derive(Debug)]
pub enum Source {
    Zip(ZipIndex),
    Folder(FolderIndex),
}

/// One input that looks like (part of) an export.
#[derive(Debug)]
pub struct Part {
    pub id: usize,
    pub path: PathBuf,
    /// File or folder name, for messages.
    pub label: String,
    pub source: Source,
    pub name: Option<PartName>,
    pub has_json: bool,
    /// Usable `memories/` files that match the SRS naming.
    pub media_count: usize,
    /// `(date, upper-cased id)` keys of the recognised media, for grouping.
    pub media_keys: HashSet<(NaiveDate, String)>,
    pub issues: Vec<Issue>,
    /// Set when this part is a byte-for-byte copy of another input.
    pub duplicate_of: Option<usize>,
    /// Part number given to a renamed extra part from where its media dates
    /// fall among the numbered parts (exports are written newest first).
    pub assigned_number: Option<u32>,
}

impl Part {
    pub fn is_zip(&self) -> bool {
        matches!(self.source, Source::Zip(_))
    }

    /// ZIP parts carry an entry order (SRS section 3.6 step 2); folders do not.
    pub fn has_entry_order(&self) -> bool {
        self.is_zip()
    }

    pub fn is_truncated(&self) -> bool {
        match &self.source {
            Source::Zip(z) => z.integrity.is_truncated(),
            Source::Folder(_) => false,
        }
    }

    /// Part number: the `-N` suffix, `1` for a primary without one.
    pub fn number(&self) -> Option<u32> {
        match self.name.as_ref().and_then(|n| n.number) {
            Some(n) => Some(n),
            None if self.has_json => Some(1),
            None => self.assigned_number,
        }
    }

    /// Oldest and newest filename dates of the recognised media.
    pub fn date_range(&self) -> Option<(NaiveDate, NaiveDate)> {
        let mut it = self.media_keys.iter().map(|(d, _)| *d);
        let first = it.next()?;
        Some(it.fold((first, first), |(lo, hi), d| (lo.min(d), hi.max(d))))
    }

    pub fn entries(&self) -> Vec<PartEntry> {
        match &self.source {
            Source::Zip(z) => z
                .entries
                .iter()
                .map(|e| PartEntry {
                    index: e.index,
                    name: e.name.clone(),
                    size: e.size,
                    crc32: Some(e.crc32),
                    usable: e.is_usable(),
                })
                .collect(),
            Source::Folder(f) => f.entries.clone(),
        }
    }

    pub fn find(&self, name: &str) -> Option<PartEntry> {
        self.entries().into_iter().find(|e| e.name == name)
    }

    /// Run `f` over one entry's bytes.
    pub fn read_entry<T>(
        &self,
        entry: &PartEntry,
        f: impl FnOnce(&mut dyn Read) -> io::Result<T>,
    ) -> Result<T, ReadError> {
        match &self.source {
            Source::Zip(z) => z.with_entry(entry.index, f).map_err(ReadError::Zip),
            Source::Folder(folder) => {
                let mut file = fs::File::open(folder.path_of(entry))?;
                Ok(f(&mut file)?)
            }
        }
    }

    /// CRC-32 of an entry: recorded for ZIPs, computed for folder files.
    pub fn crc32_of(&self, entry: &PartEntry) -> Result<u32, ReadError> {
        if let Some(crc) = entry.crc32 {
            return Ok(crc);
        }
        self.read_entry(entry, |r| {
            let mut hasher = crc32fast::Hasher::new();
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = r.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(hasher.finalize())
        })
    }

    /// Refused entries (NFR-PRV-07) of a ZIP part.
    pub fn refused(&self) -> Vec<(PartEntry, UnsafeReason)> {
        match &self.source {
            Source::Zip(z) => z
                .unsafe_entries()
                .map(|(e, r)| {
                    (
                        PartEntry {
                            index: e.index,
                            name: e.name.clone(),
                            size: e.size,
                            crc32: Some(e.crc32),
                            usable: false,
                        },
                        r,
                    )
                })
                .collect(),
            Source::Folder(_) => Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error(transparent)]
    Zip(#[from] ZipIndexError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Parts that belong to one export.
#[derive(Debug, Default)]
pub struct Export {
    pub id: usize,
    /// Base name when it came from a strongly named part.
    pub base: Option<String>,
    /// Part ids in part-number order (unnumbered last).
    pub parts: Vec<usize>,
    pub primary: Option<usize>,
    /// Part numbers absent from `1..=max` (FR-03).
    pub missing_numbers: Vec<u32>,
    /// Parts attached by content rather than by name (FR-02 AC2).
    pub attached_by_content: Vec<usize>,
    pub issues: Vec<Issue>,
}

/// An input that was not an export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ignored {
    pub path: PathBuf,
    pub detail: String,
}

/// The result of looking at every input.
#[derive(Debug, Default)]
pub struct Discovery {
    pub parts: Vec<Part>,
    pub exports: Vec<Export>,
    /// `INPUT_NOT_EXPORT` inputs.
    pub ignored: Vec<Ignored>,
}

impl Discovery {
    pub fn part(&self, id: usize) -> &Part {
        &self.parts[id]
    }

    /// Parts of an export that will be read: duplicates excluded.
    pub fn parts_of(&self, export: &Export) -> impl Iterator<Item = &Part> {
        export
            .parts
            .iter()
            .map(|&id| &self.parts[id])
            .filter(|p| p.duplicate_of.is_none())
    }
}

/// Look at every input and group the parts into exports. Never reads media
/// data; ZIPs are indexed from their central directories only.
pub fn discover(inputs: &[PathBuf]) -> Discovery {
    let mut d = Discovery::default();
    let mut queue: Vec<PathBuf> = inputs.to_vec();
    let mut seen_paths: HashSet<PathBuf> = HashSet::new();
    let mut i = 0;
    while i < queue.len() {
        let path = queue[i].clone();
        i += 1;
        let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        if !seen_paths.insert(canonical) {
            continue;
        }
        if path.is_dir() {
            match classify_folder(&path) {
                FolderKind::Export => match FolderIndex::list(&path) {
                    Ok(folder) => add_part(&mut d, &path, Source::Folder(folder), None),
                    Err(e) => d.ignored.push(Ignored {
                        path,
                        detail: format!("cannot list folder: {e}"),
                    }),
                },
                FolderKind::ZipContainer(zips) => queue.extend(zips),
                FolderKind::Other => d.ignored.push(Ignored {
                    path,
                    detail: "folder has neither memories/ nor json/ nor export ZIPs".into(),
                }),
            }
            continue;
        }
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if !file_name.to_ascii_lowercase().ends_with(".zip") {
            d.ignored.push(Ignored {
                path,
                detail: "not a .zip file".into(),
            });
            continue;
        }
        match ZipIndex::open(&path) {
            Ok(zip) => {
                let name = parse_part_name(&file_name);
                add_part(&mut d, &path, Source::Zip(zip), name);
            }
            Err(ZipIndexError::NotZip { detail, .. }) => d.ignored.push(Ignored {
                path,
                detail: format!("not a ZIP archive: {detail}"),
            }),
            Err(e) => d.ignored.push(Ignored {
                path,
                detail: e.to_string(),
            }),
        }
    }
    mark_duplicates(&mut d);
    group_exports(&mut d);
    log_discovery(inputs.len(), &d);
    d
}

/// FR-41: what discovery found, with paths redacted.
fn log_discovery(inputs: usize, d: &Discovery) {
    use crate::redact;
    for ig in &d.ignored {
        tracing::info!(
            path = %redact::path(&ig.path),
            detail = %redact::text(&ig.detail),
            "input ignored (INPUT_NOT_EXPORT)"
        );
    }
    for p in &d.parts {
        for issue in &p.issues {
            tracing::warn!(
                part = %redact::text(&p.label),
                code = issue.code.as_str(),
                detail = %redact::text(&issue.detail),
                "part issue"
            );
        }
        if p.duplicate_of.is_some() {
            tracing::info!(part = %redact::text(&p.label), "duplicate download of a part; read once");
        }
    }
    tracing::info!(
        inputs,
        parts = d.parts.len(),
        exports = d.exports.len(),
        ignored = d.ignored.len(),
        "inputs discovered"
    );
}

enum FolderKind {
    Export,
    ZipContainer(Vec<PathBuf>),
    Other,
}

fn classify_folder(path: &Path) -> FolderKind {
    if path.join("memories").is_dir() || path.join(JSON_ENTRY).is_file() {
        return FolderKind::Export;
    }
    let zips: Vec<PathBuf> = fs::read_dir(path)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("zip"))
                })
                .collect()
        })
        .unwrap_or_default();
    if zips.is_empty() {
        FolderKind::Other
    } else {
        let mut zips = zips;
        zips.sort();
        FolderKind::ZipContainer(zips)
    }
}

fn add_part(d: &mut Discovery, path: &Path, source: Source, name: Option<PartName>) {
    let id = d.parts.len();
    let label = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let mut part = Part {
        id,
        path: path.to_path_buf(),
        label,
        source,
        name,
        has_json: false,
        media_count: 0,
        media_keys: HashSet::new(),
        issues: Vec::new(),
        duplicate_of: None,
        assigned_number: None,
    };
    let entries = part.entries();
    let mut any_export_shape = false;
    for e in &entries {
        if !e.usable {
            continue;
        }
        if e.name == JSON_ENTRY {
            part.has_json = true;
            any_export_shape = true;
        } else if let Some(rest) = e.name.strip_prefix(MEDIA_DIR) {
            any_export_shape = true;
            if !rest.contains('/')
                && let Some(parsed) = parse_entry_path(&e.name)
            {
                part.media_count += 1;
                part.media_keys.insert(parsed.pair_key());
            }
        } else if e.name == "index.html" || e.name.starts_with("html/") {
            any_export_shape = true;
        }
    }
    if !any_export_shape {
        d.ignored.push(Ignored {
            path: path.to_path_buf(),
            detail: "no memories/ or json/ inside".into(),
        });
        return;
    }
    if let Source::Zip(z) = &part.source {
        if let Integrity::Truncated {
            readable, partial, ..
        } = &z.integrity
        {
            let partial = partial
                .as_deref()
                .map(|p| format!("; {p} is cut"))
                .unwrap_or_default();
            part.issues.push(Issue::for_part(
                Code::PartTruncated,
                id,
                format!(
                    "{}: central directory unreadable; {readable} entries recovered{partial}",
                    part.label
                ),
            ));
        }
        let refused = z.unsafe_entries().count();
        if refused > 0 {
            part.issues.push(Issue::for_part(
                Code::EntryRefused,
                id,
                format!("{}: {refused} entries refused for unsafe names", part.label),
            ));
        }
    }
    d.parts.push(part);
}

/// Byte-identical inputs (same entry names, sizes and CRCs) are read once.
fn mark_duplicates(d: &mut Discovery) {
    let mut fingerprints: HashMap<Vec<(String, u64, Option<u32>)>, usize> = HashMap::new();
    for id in 0..d.parts.len() {
        let fp: Vec<(String, u64, Option<u32>)> = d.parts[id]
            .entries()
            .into_iter()
            .map(|e| (e.name, e.size, e.crc32))
            .collect();
        if fp.is_empty() || !d.parts[id].is_zip() {
            continue;
        }
        match fingerprints.get(&fp) {
            Some(&first) => d.parts[id].duplicate_of = Some(first),
            None => {
                fingerprints.insert(fp, id);
            }
        }
    }
}

fn group_exports(d: &mut Discovery) {
    // 1. Strongly named parts group by base name.
    let mut by_base: HashMap<String, usize> = HashMap::new();
    let mut weak: Vec<usize> = Vec::new();
    for part in &d.parts {
        if part.duplicate_of.is_some() {
            continue;
        }
        match &part.name {
            Some(name) if name.is_strong() => {
                let export_id = *by_base.entry(name.base.clone()).or_insert_with(|| {
                    d.exports.push(Export {
                        id: d.exports.len(),
                        base: Some(name.base.clone()),
                        ..Export::default()
                    });
                    d.exports.len() - 1
                });
                d.exports[export_id].parts.push(part.id);
            }
            _ => weak.push(part.id),
        }
    }
    // 2. Weakly named parts: a primary starts its own export; an extra part
    //    joins the export whose media it overlaps, or the only export.
    for part_id in weak {
        let part = &d.parts[part_id];
        if part.has_json {
            d.exports.push(Export {
                id: d.exports.len(),
                base: None,
                parts: vec![part_id],
                ..Export::default()
            });
            continue;
        }
        let mut best: Option<(usize, usize)> = None; // (overlap, export id)
        for export in &d.exports {
            let overlap: usize = export
                .parts
                .iter()
                .map(|&other| {
                    d.parts[other]
                        .media_keys
                        .intersection(&part.media_keys)
                        .count()
                })
                .sum();
            if overlap > 0 && best.is_none_or(|(o, _)| overlap > o) {
                best = Some((overlap, export.id));
            }
        }
        let target = match best {
            Some((_, export_id)) => Some(export_id),
            None if d.exports.len() == 1 => Some(0),
            None => None,
        };
        match target {
            Some(export_id) => {
                d.exports[export_id].parts.push(part_id);
                d.exports[export_id].attached_by_content.push(part_id);
            }
            None => {
                let mut export = Export {
                    id: d.exports.len(),
                    base: None,
                    parts: vec![part_id],
                    ..Export::default()
                };
                export.issues.push(Issue::for_part(
                    Code::PartMissing,
                    part_id,
                    "could not be assigned to an export: no primary and no overlapping media",
                ));
                d.exports.push(export);
            }
        }
    }
    // 3. Give renamed extra parts a number from their media dates: parts are
    //    written newest first, so a part's date range places it among the
    //    numbered ones; the first free number in that window is taken.
    let mut assignments: Vec<(usize, u32)> = Vec::new();
    for export in &d.exports {
        let mut numbered: Vec<(u32, usize)> = export
            .parts
            .iter()
            .filter_map(|&id| d.parts[id].number().map(|n| (n, id)))
            .collect();
        let unnumbered: Vec<usize> = export
            .parts
            .iter()
            .copied()
            .filter(|&id| d.parts[id].number().is_none())
            .collect();
        for id in unnumbered {
            let max = numbered.iter().map(|(n, _)| *n).max().unwrap_or(1);
            let free: Vec<u32> = (1..=max + 1)
                .filter(|n| !numbered.iter().any(|(m, _)| m == n))
                .collect();
            let chosen = match d.parts[id].date_range() {
                Some((lo, hi)) => {
                    let after_newer = numbered
                        .iter()
                        .filter(|(_, other)| {
                            d.parts[*other]
                                .date_range()
                                .is_some_and(|(olo, _)| olo >= hi)
                        })
                        .map(|(n, _)| n + 1)
                        .max()
                        .unwrap_or(1);
                    let before_older = numbered
                        .iter()
                        .filter(|(_, other)| {
                            d.parts[*other]
                                .date_range()
                                .is_some_and(|(_, ohi)| ohi <= lo)
                        })
                        .map(|(n, _)| n - 1)
                        .min()
                        .unwrap_or(u32::MAX);
                    free.iter()
                        .copied()
                        .find(|n| *n >= after_newer && *n <= before_older)
                        .or_else(|| free.first().copied())
                }
                None => free.first().copied(),
            }
            .unwrap_or(max + 1);
            numbered.push((chosen, id));
            assignments.push((id, chosen));
        }
    }
    for (id, number) in assignments {
        d.parts[id].assigned_number = Some(number);
    }
    // 4. Order parts, find the primary and the gaps.
    for export in &mut d.exports {
        export.parts.sort_by_key(|&id| {
            let p = &d.parts[id];
            (p.number().unwrap_or(u32::MAX), id)
        });
        export.primary = export
            .parts
            .iter()
            .copied()
            .find(|&id| d.parts[id].has_json);
        let mut numbers: HashMap<u32, Vec<usize>> = HashMap::new();
        for &id in &export.parts {
            if let Some(n) = d.parts[id].number() {
                numbers.entry(n).or_default().push(id);
            }
        }
        let max = numbers.keys().copied().max().unwrap_or(0);
        export.missing_numbers = (1..=max).filter(|n| !numbers.contains_key(n)).collect();
        for &n in &export.missing_numbers {
            export.issues.push(Issue::new(
                Code::PartMissing,
                format!("Part {n} appears to be missing."),
            ));
        }
        if export.primary.is_none() {
            export.issues.push(Issue::new(
                Code::JsonMissing,
                "no part holds json/memories_history.json; exact times are not available",
            ));
        }
        for (n, ids) in numbers.iter().filter(|(_, ids)| ids.len() > 1) {
            let labels: Vec<&str> = ids.iter().map(|&id| d.parts[id].label.as_str()).collect();
            export.issues.push(Issue::new(
                Code::PartMissing,
                format!(
                    "part {n} is claimed by {} different files ({}); all are read",
                    ids.len(),
                    labels.join(", ")
                ),
            ));
        }
        for &id in &export.parts {
            export.issues.extend(d.parts[id].issues.iter().cloned());
        }
    }
    d.exports.sort_by_key(|e| e.id);
}

/// Recognised media of a part with its parsed names, in part order.
pub fn media_entries(part: &Part) -> Vec<(PartEntry, ParsedName)> {
    part.entries()
        .into_iter()
        .filter(|e| e.usable && e.name.starts_with(MEDIA_DIR))
        .filter_map(|e| parse_entry_path(&e.name).map(|p| (e, p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::CompressionMethod;
    use zip::write::{SimpleFileOptions, ZipWriter};

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("mr-core-discover-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, data) in entries {
            zw.start_file(*name, opts).unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    const JSON: &[u8] =
        br#"{"Saved Media": [{"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image"}]}"#;

    #[test]
    fn part_names() {
        let n = parse_part_name("mydata~1729000000000.zip").unwrap();
        assert_eq!(
            (n.base.as_str(), n.number, n.copy),
            ("mydata~1729000000000", None, None)
        );
        assert!(n.is_strong());
        let n = parse_part_name("mydata~1729000000000-2.zip").unwrap();
        assert_eq!(
            (n.base.as_str(), n.number, n.copy),
            ("mydata~1729000000000", Some(2), None)
        );
        let n = parse_part_name("mydata~1729000000000-12 (1).ZIP").unwrap();
        assert_eq!(
            (n.base.as_str(), n.number, n.copy),
            ("mydata~1729000000000", Some(12), Some(1))
        );
        let n = parse_part_name("mydata (1).zip").unwrap();
        assert_eq!(
            (n.base.as_str(), n.number, n.copy),
            ("mydata", None, Some(1))
        );
        assert!(!n.is_strong());
        let n = parse_part_name("export-2024.zip").unwrap();
        assert_eq!(
            (n.base.as_str(), n.number),
            ("export-2024", None),
            "4 digits is not a part number"
        );
        assert!(parse_part_name("notes.txt").is_none());
    }

    #[test]
    fn groups_by_base_name_in_any_order_and_finds_primary_and_gaps() {
        let dir = temp_dir("group");
        let primary = zip_bytes(&[
            ("index.html", b"<html>"),
            (JSON_ENTRY, JSON),
            ("memories/2021-12-25_A1-main.jpg", b"a"),
        ]);
        let p2 = zip_bytes(&[("memories/2021-12-24_B2-main.jpg", b"b")]);
        let p4 = zip_bytes(&[("memories/2021-12-23_C3-main.jpg", b"c")]);
        fs::write(dir.join("mydata~1729000000000-4.zip"), &p4).unwrap();
        fs::write(dir.join("mydata~1729000000000.zip"), &primary).unwrap();
        fs::write(dir.join("mydata~1729000000000-2.zip"), &p2).unwrap();
        let d = discover(&[
            dir.join("mydata~1729000000000-4.zip"),
            dir.join("mydata~1729000000000.zip"),
            dir.join("mydata~1729000000000-2.zip"),
            dir.join("README.txt"),
        ]);
        assert_eq!(d.exports.len(), 1);
        let export = &d.exports[0];
        assert_eq!(export.base.as_deref(), Some("mydata~1729000000000"));
        let labels: Vec<&str> = export
            .parts
            .iter()
            .map(|&id| d.parts[id].label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "mydata~1729000000000.zip",
                "mydata~1729000000000-2.zip",
                "mydata~1729000000000-4.zip"
            ]
        );
        assert_eq!(
            d.parts[export.primary.unwrap()].label,
            "mydata~1729000000000.zip"
        );
        assert_eq!(export.missing_numbers, [3]);
        assert!(
            export
                .issues
                .iter()
                .any(|i| i.code == Code::PartMissing && i.detail.contains("Part 3"))
        );
        assert_eq!(
            d.ignored.len(),
            1,
            "README.txt is not an export: {:?}",
            d.ignored
        );
        assert_eq!(d.ignored[0].detail, "not a .zip file");
    }

    #[test]
    fn renamed_part_is_attached_by_content() {
        let dir = temp_dir("renamed");
        let primary = zip_bytes(&[
            (JSON_ENTRY, JSON),
            ("memories/2021-12-25_A1-main.jpg", b"a"),
            ("memories/2021-12-24_B2-overlay.png", b"o"),
        ]);
        // The renamed part holds the main for the overlay above.
        let renamed = zip_bytes(&[("memories/2021-12-24_B2-main.jpg", b"b")]);
        // A second export with its own primary, to make the choice non-trivial.
        let other = zip_bytes(&[
            (JSON_ENTRY, JSON),
            ("memories/2020-01-01_Z9-main.jpg", b"z"),
        ]);
        fs::write(dir.join("mydata~1729000000000.zip"), &primary).unwrap();
        fs::write(dir.join("mydata (1).zip"), &renamed).unwrap();
        fs::write(dir.join("mydata~1600000000000.zip"), &other).unwrap();
        let d = discover(&[
            dir.join("mydata (1).zip"),
            dir.join("mydata~1729000000000.zip"),
            dir.join("mydata~1600000000000.zip"),
        ]);
        assert_eq!(d.exports.len(), 2);
        let e = d
            .exports
            .iter()
            .find(|e| e.base.as_deref() == Some("mydata~1729000000000"))
            .unwrap();
        assert_eq!(e.parts.len(), 2);
        assert_eq!(e.attached_by_content.len(), 1);
        let renamed = &d.parts[e.attached_by_content[0]];
        assert_eq!(renamed.label, "mydata (1).zip");
        assert_eq!(renamed.number(), Some(2), "takes the first free number");
        assert!(e.missing_numbers.is_empty());
        let other = d
            .exports
            .iter()
            .find(|e| e.base.as_deref() == Some("mydata~1600000000000"))
            .unwrap();
        assert_eq!(other.parts.len(), 1);
    }

    #[test]
    fn lone_renamed_extra_part_joins_the_only_export() {
        let dir = temp_dir("lone");
        let primary = zip_bytes(&[
            (JSON_ENTRY, JSON),
            ("memories/2021-12-25_A1-main.jpg", b"a"),
        ]);
        let extra = zip_bytes(&[("memories/2019-01-01_Q7-main.mp4", b"v")]);
        fs::write(dir.join("mydata~1729000000000.zip"), &primary).unwrap();
        fs::write(dir.join("whatever.zip"), &extra).unwrap();
        let d = discover(&[
            dir.join("whatever.zip"),
            dir.join("mydata~1729000000000.zip"),
        ]);
        assert_eq!(d.exports.len(), 1);
        assert_eq!(d.exports[0].parts.len(), 2);
        assert!(d.exports[0].missing_numbers.is_empty());
    }

    #[test]
    fn duplicate_download_is_read_once() {
        let dir = temp_dir("dup");
        let primary = zip_bytes(&[
            (JSON_ENTRY, JSON),
            ("memories/2021-12-25_A1-main.jpg", b"a"),
        ]);
        let p2 = zip_bytes(&[("memories/2021-12-24_B2-main.jpg", b"b")]);
        fs::write(dir.join("mydata~1729000000000.zip"), &primary).unwrap();
        fs::write(dir.join("mydata~1729000000000-2.zip"), &p2).unwrap();
        fs::write(dir.join("mydata~1729000000000-2 (1).zip"), &p2).unwrap();
        let d = discover(&[
            dir.join("mydata~1729000000000.zip"),
            dir.join("mydata~1729000000000-2.zip"),
            dir.join("mydata~1729000000000-2 (1).zip"),
        ]);
        assert_eq!(d.exports.len(), 1);
        let dup = d.parts.iter().find(|p| p.label.contains("(1)")).unwrap();
        assert!(dup.duplicate_of.is_some());
        assert_eq!(d.parts_of(&d.exports[0]).count(), 2);
    }

    #[test]
    fn folder_input_and_non_export_inputs() {
        let dir = temp_dir("folder");
        let export = dir.join("extracted");
        fs::create_dir_all(export.join("json")).unwrap();
        fs::create_dir_all(export.join("memories")).unwrap();
        fs::write(export.join(JSON_ENTRY), JSON).unwrap();
        fs::write(export.join("memories/2021-12-25_A1-main.jpg"), b"a").unwrap();
        fs::write(export.join("memories/2021-12-25_A1-overlay.png"), b"o").unwrap();
        fs::write(export.join("memories/notes.txt"), b"?").unwrap();
        let empty = dir.join("empty");
        fs::create_dir_all(&empty).unwrap();
        fs::write(dir.join("photo.jpg"), b"jpg").unwrap();
        fs::write(dir.join("fake.zip"), b"not a zip").unwrap();
        let d = discover(&[
            export.clone(),
            empty.clone(),
            dir.join("photo.jpg"),
            dir.join("fake.zip"),
        ]);
        assert_eq!(d.exports.len(), 1);
        let part = &d.parts[d.exports[0].parts[0]];
        assert!(!part.has_entry_order());
        assert!(part.has_json);
        assert_eq!(part.media_count, 2);
        assert_eq!(part.number(), Some(1));
        let names: Vec<String> = part.entries().into_iter().map(|e| e.name).collect();
        assert_eq!(
            names,
            [
                JSON_ENTRY,
                "memories/2021-12-25_A1-main.jpg",
                "memories/2021-12-25_A1-overlay.png",
                "memories/notes.txt"
            ]
        );
        let entry = part.find("memories/2021-12-25_A1-main.jpg").unwrap();
        assert_eq!(part.crc32_of(&entry).unwrap(), crc32fast::hash(b"a"));
        let ignored: Vec<&str> = d.ignored.iter().map(|i| i.detail.as_str()).collect();
        assert_eq!(ignored.len(), 3, "{ignored:?}");
    }

    #[test]
    fn folder_of_zips_is_expanded() {
        let dir = temp_dir("container");
        let primary = zip_bytes(&[(JSON_ENTRY, JSON)]);
        let p2 = zip_bytes(&[("memories/2021-12-24_B2-main.jpg", b"b")]);
        fs::write(dir.join("mydata~1729000000000.zip"), &primary).unwrap();
        fs::write(dir.join("mydata~1729000000000-2.zip"), &p2).unwrap();
        let d = discover(std::slice::from_ref(&dir));
        assert_eq!(d.exports.len(), 1);
        assert_eq!(d.exports[0].parts.len(), 2);
    }

    #[test]
    fn extras_without_any_primary_report_json_missing() {
        let dir = temp_dir("noprimary");
        let p2 = zip_bytes(&[("memories/2021-12-24_B2-main.jpg", b"b")]);
        let p3 = zip_bytes(&[("memories/2021-12-23_C3-main.jpg", b"c")]);
        fs::write(dir.join("mydata~1729000000000-2.zip"), &p2).unwrap();
        fs::write(dir.join("mydata~1729000000000-3.zip"), &p3).unwrap();
        let d = discover(&[
            dir.join("mydata~1729000000000-2.zip"),
            dir.join("mydata~1729000000000-3.zip"),
        ]);
        assert_eq!(d.exports.len(), 1);
        let e = &d.exports[0];
        assert!(e.primary.is_none());
        assert_eq!(e.missing_numbers, [1]);
        assert!(e.issues.iter().any(|i| i.code == Code::JsonMissing));
    }
}
