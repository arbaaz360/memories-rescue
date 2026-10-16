//! The completion report (SRS FR-38): `_report/report.html`,
//! `_report/report.csv` and `_report/failed_items.json`.
//!
//! The engine builds a [`ReportData`] and calls [`write_report`]. Every input
//! media entry and every JSON entry is one [`Row`] (FR-38 AC1). Text is plain
//! language; rows name the export part by its file name and entries by their
//! name inside the export, outputs by their path relative to the output
//! folder. [`redact`] strips URLs and absolute paths from any detail text, so
//! nothing outside the user's output folder and no download link reaches the
//! files.
//!
//! This crate knows nothing about jobs or SQLite; it renders what it is given.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Report status of a row. The first six are FR-38's; the rest exist so that
/// every input entry can appear exactly once (AC1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Done,
    MetadataPartial,
    LocationUncertain,
    MissingMedia,
    Failed,
    SkippedFreeLimit,
    /// Not processed yet (the job is paused or cancelled).
    Pending,
    /// An overlay whose main file is not in the export; not output (FR-12 AC2).
    OrphanOverlay,
    /// A file in `memories/` with a name the app does not recognise.
    Unrecognised,
    /// An entry refused for safety (NFR-PRV-07); never read.
    Refused,
    /// A second copy of a file already listed (same name, size, CRC).
    Duplicate,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Done => "done",
            Status::MetadataPartial => "metadata_partial",
            Status::LocationUncertain => "location_uncertain",
            Status::MissingMedia => "missing_media",
            Status::Failed => "failed",
            Status::SkippedFreeLimit => "skipped_free_limit",
            Status::Pending => "pending",
            Status::OrphanOverlay => "orphan_overlay",
            Status::Unrecognised => "unrecognised",
            Status::Refused => "refused",
            Status::Duplicate => "duplicate",
        }
    }

    /// Plain-language label for the summary.
    pub fn label(self) -> &'static str {
        match self {
            Status::Done => "Done",
            Status::MetadataPartial => "Done, exact time unknown",
            Status::LocationUncertain => "Done, location not applied",
            Status::MissingMedia => "Not in your export",
            Status::Failed => "Failed to process",
            Status::SkippedFreeLimit => "Skipped: free limit",
            Status::Pending => "Not processed yet",
            Status::OrphanOverlay => "Overlay without its photo or video",
            Status::Unrecognised => "Unrecognised file",
            Status::Refused => "Refused for safety",
            Status::Duplicate => "Duplicate copy",
        }
    }

    pub const ALL: [Status; 11] = [
        Status::Done,
        Status::MetadataPartial,
        Status::LocationUncertain,
        Status::MissingMedia,
        Status::Failed,
        Status::SkippedFreeLimit,
        Status::Pending,
        Status::OrphanOverlay,
        Status::Unrecognised,
        Status::Refused,
        Status::Duplicate,
    ];
}

/// What a row stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RowKind {
    /// A media item: a main file, with its overlay and JSON entry if any.
    Item,
    /// A JSON entry with no file of its own.
    JsonEntry,
    /// A file that is not an item (orphan overlay, unrecognised, refused, duplicate).
    File,
    /// A video joined from consecutive clips (FR-27); the clips are items too.
    JoinedClip,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Row {
    pub status: Option<Status>,
    pub kind: Option<RowKind>,
    /// `YYYY-MM-DD_ID`.
    pub item: Option<String>,
    pub json_index: Option<usize>,
    /// `photo` or `video`.
    pub media_type: Option<String>,
    /// The part's file name, e.g. `mydata~1729000000000-2.zip`.
    pub source_part: Option<String>,
    /// The entry inside the export, e.g. `memories/2021-12-25_A81B-main.jpg`.
    pub source_entry: Option<String>,
    pub overlay_entry: Option<String>,
    /// Relative to the output folder.
    pub output_path: Option<String>,
    /// `2021-12-25 19:42:07 +01:00`.
    pub capture_local: Option<String>,
    /// `second` or `day`.
    pub time_precision: Option<String>,
    /// `exact`, `ordinal` or `none`.
    pub match_confidence: Option<String>,
    /// `applied`, `none` or `uncertain`.
    pub location: Option<String>,
    pub error_code: Option<String>,
    pub message: Option<String>,
    pub attempts: Option<u32>,
    pub composite_pending: bool,
}

impl Row {
    pub fn status(&self) -> Status {
        self.status.unwrap_or(Status::Pending)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PartInfo {
    pub label: String,
    pub number: Option<u32>,
    pub primary: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ReportData {
    pub job_id: String,
    pub app_version: String,
    pub generated_utc: String,
    /// Format profile (`2026a`, `2026b`, `legacy`, `mixed`, `unknown`).
    pub profile: String,
    pub parts: Vec<PartInfo>,
    pub missing_parts: Vec<u32>,
    /// Plain-language notes shown above the table (job-level conditions).
    pub notes: Vec<String>,
    pub rows: Vec<Row>,
}

/// Counts per status and per match confidence.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    pub by_status: BTreeMap<String, usize>,
    pub by_confidence: BTreeMap<String, usize>,
    pub items: usize,
    pub json_entries: usize,
    pub rows: usize,
    pub composite_pending: usize,
}

pub fn totals(rows: &[Row]) -> Totals {
    let mut t = Totals {
        rows: rows.len(),
        ..Totals::default()
    };
    for r in rows {
        *t.by_status
            .entry(r.status().as_str().to_string())
            .or_default() += 1;
        if let Some(c) = &r.match_confidence {
            *t.by_confidence.entry(c.clone()).or_default() += 1;
        }
        if r.kind == Some(RowKind::Item) {
            t.items += 1;
        }
        if r.json_index.is_some() {
            t.json_entries += 1;
        }
        if r.composite_pending {
            t.composite_pending += 1;
        }
    }
    t
}

/// The SRS section 8 user message for a code.
pub fn plain_message(code: &str) -> &'static str {
    match code {
        "ZIP_ENTRY_CRC" => {
            "This file is damaged in your download. Re-download the export part if your link still works."
        }
        "MEDIA_DECODE" => "This photo or video can't be read.",
        "METADATA_WRITE" => {
            "Couldn't save the date into this file. The file was copied without it."
        }
        "FFMPEG_FAIL" => {
            "Couldn't add the overlay to this video. The original and the overlay were saved separately."
        }
        "ENCODER_UNAVAILABLE" => {
            "Video overlays need an extra Windows component (Media Feature Pack)."
        }
        "MATCH_PARTIAL" => "Exact time unknown; the date comes from the file name.",
        "LOCATION_UNCERTAIN" => "Location not applied because the match was uncertain.",
        "LEGACY_LINKS_ONLY" => {
            "This export contains links instead of media; please request a new export (accounts.snapchat.com, My Data)."
        }
        "FREE_LIMIT" => "You've rescued 100 memories for free. Unlock to finish the rest.",
        "INPUT_UNAVAILABLE" => "The export file could not be read; reconnect the drive it is on.",
        "TEST_INJECTED" => "Failed on purpose by a test.",
        "COMPOSITE_PENDING" => {
            "Overlay saved next to the file; burning it in needs the overlay component."
        }
        "OVERLAY_ASPECT_MISMATCH" => {
            "The overlay had a different shape from the picture; it was fitted to the width and centred."
        }
        "ODD_SIZE_SCALED" => "The video had an odd size and was scaled by one pixel.",
        "AUDIO_REENCODED" => "The video's sound was converted to AAC.",
        "H264_COMPAT" => "Saved as H.264 so Windows can play it.",
        "HEVC_PLAYBACK" => {
            "Some videos are HEVC, which this computer can't play without Microsoft's HEVC Video Extensions."
        }
        "CLIP_JOINED" => "Consecutive clips joined into one video; the clips are kept too.",
        "CLIP_PENDING" => "Clips not joined yet.",
        "CLIP_DECLINED" => "Clips not joined: video processing is not available on this computer.",
        "CLIP_INCOMPLETE" => "Clips not joined because one of them could not be written.",
        _ => "Something went wrong with this item.",
    }
}

static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s"'<>]+"#).expect("regex"));
static WIN_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(\\\\\?\\)?\b[a-z]:[\\/][^\s"'<>:*?|]*"#).expect("regex"));
static UNC_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\\\\[^\s"'<>\\]+\\[^\s"'<>]*"#).expect("regex"));
static UNIX_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(^|[\s("'=])(/(?:Users|home|Volumes|tmp|var|private|mnt|media|opt|root)\b[^\s"'<>]*)"#,
    )
    .expect("regex")
});

/// Remove URLs and absolute paths from free text.
pub fn redact(text: &str) -> String {
    let s = URL.replace_all(text, "<link removed>");
    let s = UNC_PATH.replace_all(&s, "<path>");
    let s = WIN_PATH.replace_all(&s, "<path>");
    let s = UNIX_PATH.replace_all(&s, "$1<path>");
    s.into_owned()
}

/// The three files written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFiles {
    pub html: PathBuf,
    pub csv: PathBuf,
    pub failed_items: PathBuf,
}

/// Write the report into `dir` (normally `<output>/_report`), atomically per file.
pub fn write_report(dir: &Path, data: &ReportData) -> io::Result<ReportFiles> {
    fs::create_dir_all(dir)?;
    let files = ReportFiles {
        html: dir.join("report.html"),
        csv: dir.join("report.csv"),
        failed_items: dir.join("failed_items.json"),
    };
    write_atomic(&files.html, render_html(data).as_bytes())?;
    write_atomic(&files.csv, render_csv(data).as_bytes())?;
    write_atomic(&files.failed_items, render_failed_json(data).as_bytes())?;
    Ok(files)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp-write");
    {
        use std::io::Write;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)
}

fn clean(row: &Row) -> Row {
    let mut r = row.clone();
    r.message = r.message.as_deref().map(redact);
    r
}

pub const CSV_COLUMNS: [&str; 17] = [
    "status",
    "kind",
    "item",
    "json_index",
    "media_type",
    "source_part",
    "source_entry",
    "overlay_entry",
    "output_path",
    "capture_local",
    "time_precision",
    "match_confidence",
    "location",
    "error_code",
    "message",
    "attempts",
    "composite_pending",
];

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn kind_str(k: Option<RowKind>) -> &'static str {
    match k {
        Some(RowKind::Item) => "item",
        Some(RowKind::JsonEntry) => "json_entry",
        Some(RowKind::File) => "file",
        Some(RowKind::JoinedClip) => "joined_clip",
        None => "",
    }
}

pub fn render_csv(data: &ReportData) -> String {
    let mut out = String::new();
    out.push_str(&CSV_COLUMNS.join(","));
    out.push_str("\r\n");
    for row in &data.rows {
        let r = clean(row);
        let fields = [
            r.status().as_str().to_string(),
            kind_str(r.kind).to_string(),
            r.item.clone().unwrap_or_default(),
            r.json_index.map(|i| i.to_string()).unwrap_or_default(),
            r.media_type.clone().unwrap_or_default(),
            r.source_part.clone().unwrap_or_default(),
            r.source_entry.clone().unwrap_or_default(),
            r.overlay_entry.clone().unwrap_or_default(),
            r.output_path.clone().unwrap_or_default(),
            r.capture_local.clone().unwrap_or_default(),
            r.time_precision.clone().unwrap_or_default(),
            r.match_confidence.clone().unwrap_or_default(),
            r.location.clone().unwrap_or_default(),
            r.error_code.clone().unwrap_or_default(),
            r.message.clone().unwrap_or_default(),
            r.attempts.map(|a| a.to_string()).unwrap_or_default(),
            if r.composite_pending { "yes" } else { "" }.to_string(),
        ];
        let line: Vec<String> = fields.iter().map(|f| csv_field(f)).collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    out
}

#[derive(Serialize)]
struct FailedItem {
    item: Option<String>,
    json_index: Option<usize>,
    media_type: Option<String>,
    source_part: Option<String>,
    source_entry: Option<String>,
    error_code: Option<String>,
    message: Option<String>,
    attempts: Option<u32>,
    output_path: Option<String>,
}

pub fn render_failed_json(data: &ReportData) -> String {
    let failed: Vec<FailedItem> = data
        .rows
        .iter()
        .filter(|r| r.status() == Status::Failed)
        .map(clean)
        .map(|r| FailedItem {
            item: r.item,
            json_index: r.json_index,
            media_type: r.media_type,
            source_part: r.source_part,
            source_entry: r.source_entry,
            error_code: r.error_code,
            message: r.message,
            attempts: r.attempts,
            output_path: r.output_path,
        })
        .collect();
    let value = serde_json::json!({
        "job_id": data.job_id,
        "generated_utc": data.generated_utc,
        "failed": failed,
    });
    serde_json::to_string_pretty(&value).expect("json") + "\n"
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

pub fn render_html(data: &ReportData) -> String {
    let t = totals(&data.rows);
    let mut h = String::new();
    h.push_str("<!doctype html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">\n");
    h.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    h.push_str("<title>Memories Rescue report</title>\n<style>\n");
    h.push_str(
        ":root{color-scheme:light dark;font-family:system-ui,-apple-system,'Segoe UI',sans-serif;--fg:#1d1d1b;--bg:#fafaf8;--muted:#6b6b66;--line:#ddd}\n\
@media (prefers-color-scheme:dark){:root{--fg:#ecece8;--bg:#1b1b1a;--muted:#a3a39d;--line:#3a3a38}}\n\
body{margin:0;padding:24px;background:var(--bg);color:var(--fg);line-height:1.45}\n\
h1{margin:0 0 4px;font-size:1.6rem}h2{margin-top:2rem;font-size:1.15rem}\n\
.muted{color:var(--muted)}table{border-collapse:collapse;width:100%;font-size:.85rem}\n\
th,td{border-bottom:1px solid var(--line);padding:6px 8px;text-align:left;vertical-align:top}\n\
th{position:sticky;top:0;background:var(--bg)}td.n{text-align:right;font-variant-numeric:tabular-nums}\n\
.s-failed{color:#c0392b}.s-missing_media{color:#9a6b00}.s-done{color:#2e7d32}\n\
.summary td:first-child{width:60%}.wrap{overflow-x:auto}\n",
    );
    h.push_str("</style></head><body>\n");
    h.push_str("<h1>Memories Rescue report</h1>\n");
    let _ = writeln!(
        h,
        "<p class=\"muted\">Generated {} · job {} · app {} · export format {}</p>",
        esc(&data.generated_utc),
        esc(&data.job_id),
        esc(&data.app_version),
        esc(&data.profile)
    );
    h.push_str("<p class=\"muted\">\"Not in your export\" means the export you downloaded does not contain that photo or video; it is not a failure of the app. Memories Rescue is not affiliated with Snap Inc.</p>\n");

    h.push_str("<h2>Summary</h2>\n<table class=\"summary\"><tbody>\n");
    for s in Status::ALL {
        let n = t.by_status.get(s.as_str()).copied().unwrap_or(0);
        if n == 0 && !matches!(s, Status::Done | Status::Failed | Status::MissingMedia) {
            continue;
        }
        let _ = writeln!(
            h,
            "<tr><td>{}</td><td class=\"n\">{n}</td></tr>",
            esc(s.label())
        );
    }
    let _ = writeln!(
        h,
        "<tr><td>Items (photos and videos found)</td><td class=\"n\">{}</td></tr>",
        t.items
    );
    let _ = writeln!(
        h,
        "<tr><td>Entries in memories_history.json</td><td class=\"n\">{}</td></tr>",
        t.json_entries
    );
    if t.composite_pending > 0 {
        let _ = writeln!(
            h,
            "<tr><td>Overlays saved beside the file (burn-in pending)</td><td class=\"n\">{}</td></tr>",
            t.composite_pending
        );
    }
    h.push_str("</tbody></table>\n");

    if !t.by_confidence.is_empty() {
        h.push_str("<h2>How times were matched</h2>\n<table class=\"summary\"><tbody>\n");
        for (conf, label) in [
            ("exact", "Exact (the export ties the file to its entry)"),
            ("ordinal", "By order within the day"),
            ("none", "Date from the file name only"),
        ] {
            if let Some(n) = t.by_confidence.get(conf) {
                let _ = writeln!(h, "<tr><td>{label}</td><td class=\"n\">{n}</td></tr>");
            }
        }
        h.push_str("</tbody></table>\n");
    }

    h.push_str("<h2>Export parts</h2>\n<ul>\n");
    for p in &data.parts {
        let mut flags = Vec::new();
        if p.primary {
            flags.push("has the data file");
        }
        if p.truncated {
            flags.push("incomplete download");
        }
        let _ = writeln!(
            h,
            "<li>{}{}{}</li>",
            esc(&p.label),
            p.number.map(|n| format!(" (part {n})")).unwrap_or_default(),
            if flags.is_empty() {
                String::new()
            } else {
                format!(": {}", flags.join(", "))
            }
        );
    }
    for n in &data.missing_parts {
        let _ = writeln!(
            h,
            "<li><strong>Part {n} appears to be missing.</strong> Add it later to finish the items it holds.</li>"
        );
    }
    h.push_str("</ul>\n");

    if !data.notes.is_empty() {
        h.push_str("<h2>Notes</h2>\n<ul>\n");
        for n in &data.notes {
            let _ = writeln!(h, "<li>{}</li>", esc(&redact(n)));
        }
        h.push_str("</ul>\n");
    }

    h.push_str("<h2>Every item and entry</h2>\n<div class=\"wrap\"><table>\n<thead><tr>");
    for col in [
        "Status", "Item", "JSON #", "Type", "Part", "Entry", "Output", "Captured", "Match",
        "Location", "Problem",
    ] {
        let _ = write!(h, "<th>{col}</th>");
    }
    h.push_str("</tr></thead><tbody>\n");
    for row in &data.rows {
        let r = clean(row);
        let problem = match (&r.error_code, &r.message) {
            (Some(code), Some(m)) => format!("{m} ({code})"),
            (Some(code), None) => plain_message(code).to_string(),
            (None, Some(m)) => m.clone(),
            (None, None) => String::new(),
        };
        let captured = match (&r.capture_local, &r.time_precision) {
            (Some(c), Some(p)) if p == "day" => format!("{} (day only)", &c[..c.len().min(10)]),
            (Some(c), _) => c.clone(),
            _ => String::new(),
        };
        let _ = writeln!(
            h,
            "<tr class=\"s-{}\"><td>{}</td><td>{}</td><td class=\"n\">{}</td><td>{}</td><td>{}</td><td>{}{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            r.status().as_str(),
            esc(r.status().label()),
            esc(r.item.as_deref().unwrap_or("")),
            r.json_index.map(|i| i.to_string()).unwrap_or_default(),
            esc(r.media_type.as_deref().unwrap_or("")),
            esc(r.source_part.as_deref().unwrap_or("")),
            esc(r.source_entry.as_deref().unwrap_or("")),
            r.overlay_entry
                .as_deref()
                .map(|o| format!("<br><span class=\"muted\">+ {}</span>", esc(o)))
                .unwrap_or_default(),
            esc(r.output_path.as_deref().unwrap_or("")),
            esc(&captured),
            esc(r.match_confidence.as_deref().unwrap_or("")),
            esc(r.location.as_deref().unwrap_or("")),
            esc(&problem),
        );
    }
    h.push_str("</tbody></table></div>\n</body></html>\n");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ReportData {
        ReportData {
            job_id: "job-1".into(),
            app_version: "0.1.0".into(),
            generated_utc: "2026-10-16T12:00:00Z".into(),
            profile: "2026b".into(),
            parts: vec![PartInfo {
                label: "mydata~1.zip".into(),
                number: Some(1),
                primary: true,
                truncated: false,
            }],
            missing_parts: vec![3],
            notes: vec!["See https://example.invalid/x?sig=abc in C:\\Users\\adam\\x.zip".into()],
            rows: vec![
                Row {
                    status: Some(Status::Done),
                    kind: Some(RowKind::Item),
                    item: Some("2021-12-25_A81B".into()),
                    json_index: Some(0),
                    media_type: Some("photo".into()),
                    source_part: Some("mydata~1.zip".into()),
                    source_entry: Some("memories/2021-12-25_A81B-main.jpg".into()),
                    output_path: Some("2021/12/2021-12-25_194207_A81B.jpg".into()),
                    capture_local: Some("2021-12-25 19:42:07 +01:00".into()),
                    time_precision: Some("second".into()),
                    match_confidence: Some("ordinal".into()),
                    location: Some("applied".into()),
                    ..Row::default()
                },
                Row {
                    status: Some(Status::Failed),
                    kind: Some(RowKind::Item),
                    item: Some("2021-12-24_B".into()),
                    error_code: Some("ZIP_ENTRY_CRC".into()),
                    message: Some("read /Users/adam/Downloads/mydata.zip: \"bad, crc\"".into()),
                    attempts: Some(2),
                    ..Row::default()
                },
                Row {
                    status: Some(Status::MissingMedia),
                    kind: Some(RowKind::JsonEntry),
                    json_index: Some(1),
                    ..Row::default()
                },
            ],
        }
    }

    #[test]
    fn totals_count_statuses_and_confidence() {
        let t = totals(&sample().rows);
        assert_eq!(t.rows, 3);
        assert_eq!(t.items, 2);
        assert_eq!(t.json_entries, 2);
        assert_eq!(t.by_status["done"], 1);
        assert_eq!(t.by_status["failed"], 1);
        assert_eq!(t.by_status["missing_media"], 1);
        assert_eq!(t.by_confidence["ordinal"], 1);
    }

    #[test]
    fn redaction_removes_links_and_absolute_paths() {
        let s = redact(
            "see https://app.example.invalid/dmd?sid=1&sig=2 and C:\\Users\\adam\\x.zip, /Users/adam/y.zip or \\\\nas\\share\\z.zip",
        );
        assert!(!s.contains("http"), "{s}");
        assert!(!s.contains("adam"), "{s}");
        assert!(!s.contains("nas"), "{s}");
        assert_eq!(redact("2021/12/a.jpg stays"), "2021/12/a.jpg stays");
    }

    #[test]
    fn files_are_written_escaped_and_redacted() {
        let dir = std::env::temp_dir().join(format!("mr-report-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let files = write_report(&dir, &sample()).unwrap();
        let html = fs::read_to_string(&files.html).unwrap();
        assert!(html.contains("Part 3 appears to be missing."));
        assert!(html.contains("Not in your export"));
        assert!(!html.contains("adam"), "paths redacted");
        assert!(!html.contains("sig="), "links redacted");
        assert!(html.contains("&quot;bad, crc&quot;"), "escaped");
        let csv = fs::read_to_string(&files.csv).unwrap();
        let lines: Vec<&str> = csv.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 4, "header + one line per row");
        assert!(lines[0].starts_with("status,kind,item,json_index"));
        assert!(
            lines[2].contains("\"read <path> \"\"bad, crc\"\"\""),
            "{}",
            lines[2]
        );
        let failed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&files.failed_items).unwrap()).unwrap();
        assert_eq!(failed["failed"].as_array().unwrap().len(), 1);
        assert_eq!(failed["failed"][0]["error_code"], "ZIP_ENTRY_CRC");
        assert_eq!(failed["failed"][0]["attempts"], 2);
    }
}
