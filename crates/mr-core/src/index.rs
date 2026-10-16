//! Merged virtual index over every part of one export (FR-06), the JSON
//! load, and the export sanity summary (FR-07, FR-13).
//!
//! Nothing is extracted: the index holds names, sizes, CRCs and the archive
//! order of every entry, de-duplicated across parts by name + size + CRC-32.
//! Memory is proportional to the number of entries, never to their bytes.

use std::collections::HashMap;

use chrono::{DateTime, NaiveTime, Utc};

use crate::discover::{Discovery, Export, JSON_ENTRY, MEDIA_DIR, Part, PartEntry};
use crate::entry::{ParsedEntry, Summary, parse_entry};
use crate::fields::{DateField, MediaType, media_type_from_ext};
use crate::filename::{FileKind, ParsedName, parse_entry_path};
use crate::issue::{Code, Issue};
use crate::stream::SavedMediaReader;
use crate::zipindex::UnsafeReason;

/// A file inside a specific part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRef {
    pub part: usize,
    pub index: usize,
    pub name: String,
    pub size: u64,
    pub crc32: Option<u32>,
}

impl EntryRef {
    pub fn new(part: usize, e: &PartEntry) -> EntryRef {
        EntryRef {
            part,
            index: e.index,
            name: e.name.clone(),
            size: e.size,
            crc32: e.crc32,
        }
    }

    pub fn to_part_entry(&self) -> PartEntry {
        PartEntry {
            index: self.index,
            name: self.name.clone(),
            size: self.size,
            crc32: self.crc32,
            usable: true,
        }
    }
}

/// A recognised `memories/` file.
#[derive(Debug, Clone)]
pub struct MediaFile {
    pub parsed: ParsedName,
    pub entry: EntryRef,
    /// `(part number, entry index)`: the ZIP entry order of SRS section 3.6.
    /// `None` for folder parts, which have no order.
    pub order: Option<(u32, usize)>,
}

impl MediaFile {
    pub fn key(&self) -> (chrono::NaiveDate, String) {
        self.parsed.pair_key()
    }

    pub fn media_type(&self) -> Option<MediaType> {
        media_type_from_ext(&self.parsed.ext)
    }
}

/// Every part of an export, merged.
#[derive(Debug, Default)]
pub struct MergedIndex {
    pub export: usize,
    pub parts: usize,
    /// Main files in order (ZIP order across parts; unordered files last).
    pub mains: Vec<MediaFile>,
    pub overlays: Vec<MediaFile>,
    /// Files under `memories/` that match no pattern (copied to `_unsorted/`).
    pub unrecognised: Vec<EntryRef>,
    pub refused: Vec<(EntryRef, UnsafeReason)>,
    /// `(kept, dropped)` pairs with equal name, size and CRC across parts.
    pub duplicates: Vec<(EntryRef, EntryRef)>,
    pub json: Option<EntryRef>,
    /// Bytes of every listed entry, duplicates included.
    pub total_bytes: u64,
    /// Bytes of the recognised media kept.
    pub media_bytes: u64,
    /// `true` when every media file has a ZIP entry order.
    pub ordered: bool,
}

/// Build the merged index of one export. Reads no media; only folder files
/// that collide on name and size get their CRC computed.
pub fn build_index(d: &Discovery, export: &Export) -> MergedIndex {
    let mut idx = MergedIndex {
        export: export.id,
        ordered: true,
        ..MergedIndex::default()
    };
    let mut seen: HashMap<(String, u64), EntryRef> = HashMap::new();
    for part in d.parts_of(export) {
        idx.parts += 1;
        if !part.has_entry_order() {
            idx.ordered = false;
        }
        let number = part.number();
        for (entry, reason) in part.refused() {
            idx.refused.push((EntryRef::new(part.id, &entry), reason));
        }
        for e in part.entries() {
            idx.total_bytes += e.size;
            if !e.usable {
                continue;
            }
            let r = EntryRef::new(part.id, &e);
            if e.name == JSON_ENTRY {
                if idx.json.is_none() {
                    idx.json = Some(r);
                }
                continue;
            }
            let Some(rest) = e.name.strip_prefix(MEDIA_DIR) else {
                continue;
            };
            if rest.is_empty() || rest.ends_with('/') {
                continue;
            }
            if let Some(previous) = seen.get(&(e.name.clone(), e.size))
                && same_content(d, previous, part, &e)
            {
                idx.duplicates.push((previous.clone(), r));
                continue;
            }
            seen.insert((e.name.clone(), e.size), r.clone());
            match parse_entry_path(&e.name) {
                Some(parsed) => {
                    idx.media_bytes += e.size;
                    let file = MediaFile {
                        parsed,
                        entry: r,
                        order: if part.has_entry_order() {
                            Some((number.unwrap_or(u32::MAX), e.index))
                        } else {
                            None
                        },
                    };
                    if file.parsed.kind == FileKind::Overlay {
                        idx.overlays.push(file);
                    } else {
                        idx.mains.push(file);
                    }
                }
                None => idx.unrecognised.push(r),
            }
        }
    }
    let by_order = |f: &MediaFile| (f.order.is_none(), f.order, f.entry.name.clone());
    idx.mains.sort_by_key(by_order);
    idx.overlays.sort_by_key(by_order);
    idx
}

fn same_content(d: &Discovery, previous: &EntryRef, part: &Part, e: &PartEntry) -> bool {
    let prev_crc = match previous.crc32 {
        Some(c) => Some(c),
        None => d
            .part(previous.part)
            .crc32_of(&previous.to_part_entry())
            .ok(),
    };
    let this_crc = match e.crc32 {
        Some(c) => Some(c),
        None => part.crc32_of(e).ok(),
    };
    matches!((prev_crc, this_crc), (Some(a), Some(b)) if a == b)
}

// ---------------------------------------------------------------------------
// JSON load and profiles
// ---------------------------------------------------------------------------

/// Which matching path one JSON entry can take (SRS v0.2 section 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryProfile {
    /// `sid`/`mid` present: exact matching.
    P2026a,
    /// No URL: ordinal matching.
    P2026b,
    /// A URL without usable parameters: a legacy link.
    Legacy,
}

pub fn entry_profile(e: &ParsedEntry) -> EntryProfile {
    if !e.url_tokens().is_empty() {
        EntryProfile::P2026a
    } else if e.has_url() {
        EntryProfile::Legacy
    } else {
        EntryProfile::P2026b
    }
}

/// Entries per matching path.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProfileCounts {
    pub exact: usize,
    pub ordinal: usize,
    pub metadata_only: usize,
}

impl ProfileCounts {
    pub fn record(&mut self, profile: EntryProfile) {
        match profile {
            EntryProfile::P2026a => self.exact += 1,
            EntryProfile::P2026b => self.ordinal += 1,
            EntryProfile::Legacy => self.metadata_only += 1,
        }
    }

    pub fn total(&self) -> usize {
        self.exact + self.ordinal + self.metadata_only
    }

    /// The entries per matching path once media presence is known (SRS v0.3
    /// section 3.3). With media bundled, a URL without usable `sid`/`mid` is
    /// not a legacy link: the entry takes the ordinal path, which is what the
    /// matcher does with it. Without media, every entry is metadata-only.
    pub fn paths(self, media_present: bool) -> ProfileCounts {
        if media_present {
            ProfileCounts {
                exact: self.exact,
                ordinal: self.ordinal + self.metadata_only,
                metadata_only: 0,
            }
        } else if self.exact + self.metadata_only > 0 {
            ProfileCounts {
                exact: 0,
                ordinal: 0,
                metadata_only: self.total(),
            }
        } else {
            self
        }
    }
}

/// `memories_history.json`, streamed and parsed.
#[derive(Debug, Default)]
pub struct JsonLoad {
    pub entries: Vec<ParsedEntry>,
    pub summary: Summary,
    pub profile_counts: ProfileCounts,
    pub skipped_keys: Vec<String>,
    /// Elements that could not be read; iteration continued past them.
    pub element_errors: Vec<String>,
    /// `JSON_PARSE`: the file could not be read to the end.
    pub fatal: Option<String>,
}

/// Stream the export's JSON out of its part. `None` when there is none.
pub fn load_json(d: &Discovery, index: &MergedIndex) -> Option<JsonLoad> {
    let entry = index.json.as_ref()?;
    let part = d.part(entry.part);
    let result = part.read_entry(&entry.to_part_entry(), |r| {
        let mut load = JsonLoad::default();
        let reader = match SavedMediaReader::new(r) {
            Ok(reader) => reader,
            Err(e) => {
                load.fatal = Some(e.to_string());
                return Ok(load);
            }
        };
        load.skipped_keys = reader.skipped_keys().to_vec();
        for item in reader {
            match item {
                Ok(raw) => {
                    let parsed = parse_entry(&raw);
                    load.summary.record(&parsed);
                    load.profile_counts.record(entry_profile(&parsed));
                    load.entries.push(parsed);
                }
                Err(e) if e.is_recoverable() => load.element_errors.push(e.to_string()),
                Err(e) => {
                    load.fatal = Some(e.to_string());
                    break;
                }
            }
        }
        Ok(load)
    });
    Some(match result {
        Ok(load) => load,
        Err(e) => JsonLoad {
            fatal: Some(e.to_string()),
            ..JsonLoad::default()
        },
    })
}

// ---------------------------------------------------------------------------
// Export summary (FR-07) and format profile (FR-13)
// ---------------------------------------------------------------------------

/// Export-level format profile (SRS v0.2 section 3.3, FR-13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// Media bundled, `sid`/`mid` in every entry.
    P2026a,
    /// Media bundled, URL fields empty or absent.
    P2026b,
    /// Signed links, no bundled media.
    Legacy,
    /// Entries of more than one profile: with media bundled, some with
    /// `sid`/`mid` (exact) and some without (ordinal); without media, links
    /// next to entries that have none.
    Mixed,
    /// No JSON, or an empty one.
    Unknown,
}

impl Profile {
    pub fn as_str(self) -> &'static str {
        match self {
            Profile::P2026a => "2026a",
            Profile::P2026b => "2026b",
            Profile::Legacy => "legacy",
            Profile::Mixed => "mixed",
            Profile::Unknown => "unknown",
        }
    }
}

/// Export-level profile. A legacy link carries `sid`/`mid` too, so an
/// entry alone cannot tell `2026a` from `legacy`: without any bundled media,
/// URL-bearing entries mean links only.
pub fn classify(counts: ProfileCounts, media_present: bool) -> Profile {
    let ProfileCounts {
        exact,
        ordinal,
        metadata_only,
    } = counts;
    let urls = exact + metadata_only;
    if urls == 0 && ordinal == 0 {
        return Profile::Unknown;
    }
    if !media_present && urls > 0 {
        return if ordinal > 0 {
            Profile::Mixed
        } else {
            Profile::Legacy
        };
    }
    // Media bundled: a URL without usable parameters is on the ordinal path.
    match (exact > 0, ordinal + metadata_only > 0) {
        (true, false) => Profile::P2026a,
        (false, true) => Profile::P2026b,
        _ => Profile::Mixed,
    }
}

/// How to request a new export, from Snap's support article [R1] (FR-13 AC1,
/// FR-49). Snap's address is text, not a link (NFR-PRV-02).
pub const REQUEST_EXPORT_STEPS: &str = "1. Go to accounts.snapchat.com and open My Data.\n\
2. Tick Export your Memories and Export JSON Files.\n\
3. Choose All Time as the date range, then submit the request.\n\
4. Snapchat emails you when the export is ready; this can take up to 7 days. Download every part soon after: the links may stop working after about 3 days.";

/// The FR-13 AC1 message for a links-only export, with the steps.
pub fn legacy_message() -> String {
    format!(
        "This export contains links rather than media. These links expire and Memories Rescue v1 does not download them. Please request a new export:\n{REQUEST_EXPORT_STEPS}"
    )
}

/// The FR-13 AC2 note for exports whose exact times rest on ZIP order.
pub const ORDINAL_NOTE: &str = "Exact times for memories without a link in the export's data depend on the order of files inside the ZIP files. Keep the original ZIP files: from an extracted folder, most of these memories get their date only.";

impl ExportSummary {
    /// The FR-13 message that goes with the profile, if any: the legacy text
    /// with the steps to request a new export, or, when some exact times rest
    /// on ZIP order, that they do and that an extracted folder would give
    /// day precision. Folder input itself is a `MATCH_PARTIAL` issue.
    pub fn profile_message(&self) -> Option<String> {
        match self.profile {
            Profile::Legacy => Some(legacy_message()),
            Profile::P2026b | Profile::Mixed if self.profile_counts.ordinal > 0 && self.ordered => {
                Some(ORDINAL_NOTE.to_string())
            }
            _ => None,
        }
    }
}

/// What the Check & Summary screen shows (FR-07).
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSummary {
    pub profile: Profile,
    pub profile_counts: ProfileCounts,
    pub parts: usize,
    pub missing_parts: Vec<u32>,
    pub truncated_parts: usize,
    pub json_entries: usize,
    pub main_files: usize,
    pub photos: usize,
    pub videos: usize,
    pub overlays: usize,
    pub unrecognised: usize,
    pub refused: usize,
    pub duplicates: usize,
    pub date_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    pub with_location: usize,
    pub total_bytes: u64,
    pub media_bytes: u64,
    pub ordered: bool,
    pub issues: Vec<Issue>,
}

pub fn summarize(
    d: &Discovery,
    export: &Export,
    index: &MergedIndex,
    json: Option<&JsonLoad>,
) -> ExportSummary {
    let mut issues = export.issues.clone();
    let raw_counts = json.map(|j| j.profile_counts).unwrap_or_default();
    let media_present = !index.mains.is_empty();
    let profile = classify(raw_counts, media_present);
    let profile_counts = raw_counts.paths(media_present);
    match json {
        None => issues.push(Issue::new(
            Code::JsonMissing,
            "Exact times aren't available; using dates from filenames.",
        )),
        Some(j) => {
            if let Some(fatal) = &j.fatal {
                issues.push(Issue::new(
                    Code::JsonParse,
                    format!("The export's data file is damaged ({fatal}); using filenames where possible."),
                ));
            }
            if !j.element_errors.is_empty() {
                issues.push(Issue::new(
                    Code::JsonParse,
                    format!(
                        "{} entries could not be read and were skipped",
                        j.element_errors.len()
                    ),
                ));
            }
        }
    }
    if profile == Profile::Legacy {
        issues.push(Issue::new(Code::LegacyLinksOnly, legacy_message()));
    }
    if !index.ordered && index.mains.len() > 1 {
        issues.push(Issue::new(
            Code::MatchPartial,
            "Folder input has no entry order: exact times need the original ZIP for days with more than one item.",
        ));
    }

    let mut date_range: Option<(DateTime<Utc>, DateTime<Utc>)> = None;
    let mut extend = |t: DateTime<Utc>| {
        date_range = Some(match date_range {
            None => (t, t),
            Some((lo, hi)) => (lo.min(t), hi.max(t)),
        });
    };
    let mut with_instant = false;
    if let Some(j) = json {
        for e in &j.entries {
            if let Some(DateField::Instant(t)) = e.date {
                extend(t);
                with_instant = true;
            }
        }
    }
    if !with_instant {
        for f in &index.mains {
            extend(f.parsed.date.and_time(NaiveTime::MIN).and_utc());
        }
    }

    let photos = index
        .mains
        .iter()
        .filter(|f| f.media_type() == Some(MediaType::Photo))
        .count();
    let videos = index
        .mains
        .iter()
        .filter(|f| f.media_type() == Some(MediaType::Video))
        .count();
    ExportSummary {
        profile,
        profile_counts,
        parts: d.parts_of(export).count(),
        missing_parts: export.missing_numbers.clone(),
        truncated_parts: d.parts_of(export).filter(|p| p.is_truncated()).count(),
        json_entries: json.map(|j| j.entries.len()).unwrap_or(0),
        main_files: index.mains.len(),
        photos,
        videos,
        overlays: index.overlays.len(),
        unrecognised: index.unrecognised.len(),
        refused: index.refused.len(),
        duplicates: index.duplicates.len(),
        date_range,
        with_location: json.map(|j| j.summary.with_location).unwrap_or(0),
        total_bytes: index.total_bytes,
        media_bytes: index.media_bytes,
        ordered: index.ordered,
        issues,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::RawEntry;
    use serde_json::{Value, json};

    fn entry(v: Value) -> ParsedEntry {
        match v {
            Value::Object(m) => parse_entry(&RawEntry::from_json_object(0, m)),
            _ => unreachable!(),
        }
    }

    #[test]
    fn entry_profiles() {
        assert_eq!(
            entry_profile(&entry(
                json!({"Download Link": "https://a.invalid/?sid=A81B&mid=A81B"})
            )),
            EntryProfile::P2026a
        );
        assert_eq!(
            entry_profile(&entry(
                json!({"Download Link": "https://a.invalid/x?sig=1"})
            )),
            EntryProfile::Legacy
        );
        assert_eq!(
            entry_profile(&entry(
                json!({"Download Link": "", "Media Download Url": "N/A"})
            )),
            EntryProfile::P2026b
        );
        assert_eq!(entry_profile(&entry(json!({}))), EntryProfile::P2026b);
    }

    #[test]
    fn export_profile_classification() {
        let c = |exact, ordinal, metadata_only| ProfileCounts {
            exact,
            ordinal,
            metadata_only,
        };
        assert_eq!(classify(c(0, 0, 0), false), Profile::Unknown);
        assert_eq!(classify(c(0, 0, 0), true), Profile::Unknown);
        assert_eq!(classify(c(50, 0, 0), true), Profile::P2026a);
        assert_eq!(classify(c(0, 50, 0), true), Profile::P2026b);
        assert_eq!(classify(c(0, 0, 50), false), Profile::Legacy);
        assert_eq!(
            classify(c(50, 0, 0), false),
            Profile::Legacy,
            "sid/mid links without media"
        );
        assert_eq!(
            classify(c(0, 50, 0), false),
            Profile::P2026b,
            "media simply absent"
        );
        assert_eq!(
            classify(c(0, 0, 50), true),
            Profile::P2026b,
            "URLs without sid/mid next to media take the ordinal path (SRS v0.3)"
        );
        assert_eq!(classify(c(10, 40, 0), true), Profile::Mixed);
        assert_eq!(classify(c(10, 0, 5), true), Profile::Mixed, "F-03 shape");
        assert_eq!(classify(c(10, 5, 0), false), Profile::Mixed);
        assert_eq!(
            c(10, 5, 3).paths(true),
            c(10, 8, 0),
            "tokenless URLs are ordinal with media"
        );
        assert_eq!(c(10, 0, 3).paths(false), c(0, 0, 13), "links only");
        assert_eq!(c(0, 7, 0).paths(false), c(0, 7, 0), "media simply absent");
    }
}
