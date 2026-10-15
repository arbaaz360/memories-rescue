//! Fixture integration tests (SRS v0.2 §11.2): run the M0 parsers over
//! F-01 `small-2026b`, F-15 `small-2026a`, F-02 `small-legacy` and F-10
//! `weird-json` and compare every count and parsed value with the generator's
//! `truth.json`.
//!
//! The fixtures live in `<repo>/fixtures/` (gitignored). If any is missing,
//! the first test to run generates all of them by calling
//! `python tools/make_fixture --all` (needs Python 3 with Pillow; ffmpeg is
//! optional, without it video main files are absent and `truth.json` says so).

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

use chrono::{DateTime, Utc};
use mr_core::entry::{ParsedEntry, RawEntry, Summary, Warning, parse_entry};
use mr_core::fields::{DateField, LocationParse, MediaTypeHint, parse_location, redact_url};
use mr_core::filename::{FileKind, parse_entry_path};
use mr_core::stream::SavedMediaReader;
use serde::Deserialize;
use serde_json::Value;
use zip::ZipArchive;

const FIXTURES: [&str; 4] = ["small-2026b", "small-2026a", "small-legacy", "weird-json"];
const JSON_ENTRY: &str = "json/memories_history.json";

// ---------------------------------------------------------------------------
// truth.json schema 1
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Truth {
    schema: u32,
    fixture: String,
    format: String,
    generator: Generator,
    parts: Vec<Part>,
    json: JsonMeta,
    counts: Counts,
    items: Vec<Value>,
}

#[derive(Deserialize)]
struct Generator {
    zip_order: String,
    json_order: String,
    ffmpeg: Value,
}

#[derive(Deserialize)]
struct Part {
    file: String,
    role: String,
    entries: Vec<String>,
}

#[derive(Deserialize)]
struct JsonMeta {
    bom: bool,
    crlf: bool,
    top_level_keys_before: Vec<String>,
}

#[derive(Deserialize)]
struct Counts {
    json_entries: usize,
    items: usize,
    photos: usize,
    videos: usize,
    #[serde(default)]
    unknown_type: usize,
    overlays: usize,
    with_location: usize,
    #[serde(default)]
    with_instant: usize,
    with_url: usize,
    #[serde(default)]
    with_tokens: usize,
    media_files: usize,
    main_files: usize,
    main_files_skipped: usize,
}

#[derive(Deserialize, Clone, Copy)]
struct Loc {
    lat: f64,
    lon: f64,
}

#[derive(Deserialize)]
struct Item2026 {
    json_index: usize,
    id: String,
    date_utc: String,
    filename_date: String,
    media_type: String,
    ext: String,
    main: Option<String>,
    main_skipped: Option<String>,
    overlay: Option<String>,
    location: Option<Loc>,
    #[serde(default)]
    sid: Option<String>,
    #[serde(default)]
    mid: Option<String>,
    zip_index_main: Option<usize>,
    zip_index_overlay: Option<usize>,
}

#[derive(Deserialize)]
struct ItemLegacy {
    json_index: usize,
    date_utc: String,
    media_type: String,
    location: Option<Loc>,
    download_link: String,
    media_download_url: String,
    download_link_redacted: String,
    sid: String,
    mid: String,
}

#[derive(Deserialize)]
struct ItemWeird {
    json_index: usize,
    case: String,
    expected: Expected,
}

#[derive(Deserialize)]
struct Expected {
    date_utc: Option<String>,
    date_kind: String,
    media_type: String,
    location: Option<Loc>,
    location_reason: String,
    download_link: Option<String>,
    media_download_url: Option<String>,
    sid: Option<String>,
    mid: Option<String>,
    unknown_fields: Vec<String>,
    non_string_fields: Vec<String>,
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

static GENERATE: Once = Once::new();

/// Generate the fixtures once per test binary if any is missing.
fn ensure_fixtures() {
    GENERATE.call_once(|| {
        let root = repo_root();
        let missing: Vec<&str> = FIXTURES
            .iter()
            .copied()
            .filter(|n| !root.join("fixtures").join(n).join("truth.json").is_file())
            .collect();
        if missing.is_empty() {
            return;
        }
        eprintln!("fixtures missing ({missing:?}); running tools/make_fixture --all");
        for python in ["python", "python3", "py"] {
            let result = Command::new(python)
                .args(["tools/make_fixture", "--all", "--quiet"])
                .current_dir(&root)
                .status();
            match result {
                Ok(status) if status.success() => return,
                Ok(status) => panic!("{python} tools/make_fixture --all failed with {status}"),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => panic!("could not run {python}: {e}"),
            }
        }
        panic!(
            "fixtures are missing and no Python interpreter was found; \
             run `python tools/make_fixture --all` (needs Pillow) and retry"
        );
    });
}

struct Fixture {
    dir: PathBuf,
    truth: Truth,
}

fn load(name: &str) -> Fixture {
    ensure_fixtures();
    let dir = repo_root().join("fixtures").join(name);
    let text = std::fs::read_to_string(dir.join("truth.json"))
        .unwrap_or_else(|e| panic!("{}: {e}", dir.join("truth.json").display()));
    let truth: Truth = serde_json::from_str(&text).expect("truth.json matches schema 1");
    assert_eq!(truth.schema, 1);
    assert_eq!(truth.fixture, name);
    assert_eq!(truth.parts.len(), 1, "M0 fixtures are single-part");
    assert_eq!(truth.parts[0].role, "primary");
    Fixture { dir, truth }
}

impl Fixture {
    fn archive(&self) -> ZipArchive<File> {
        let path = self.dir.join(&self.truth.parts[0].file);
        ZipArchive::new(File::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
            .expect("fixture ZIP opens")
    }

    fn entry_names(&self) -> Vec<String> {
        let mut archive = self.archive();
        (0..archive.len())
            .map(|i| {
                archive
                    .by_index(i)
                    .expect("entry")
                    .name()
                    .expect("utf-8 name")
                    .into_owned()
            })
            .collect()
    }

    fn items<T: for<'de> Deserialize<'de>>(&self) -> Vec<T> {
        let mut items: Vec<(usize, T)> = self
            .truth
            .items
            .iter()
            .map(|v| {
                let index = v["json_index"].as_u64().expect("json_index") as usize;
                (
                    index,
                    serde_json::from_value(v.clone()).expect("item shape"),
                )
            })
            .collect();
        items.sort_by_key(|(i, _)| *i);
        items.into_iter().map(|(_, t)| t).collect()
    }

    /// Stream `json/memories_history.json` straight out of the ZIP entry:
    /// returns the skipped top-level keys and every raw element.
    fn stream_json(&self) -> (Vec<String>, Vec<RawEntry>) {
        let mut archive = self.archive();
        let entry = archive
            .by_name(JSON_ENTRY)
            .expect("fixture has json/memories_history.json");
        let reader = SavedMediaReader::new(entry).expect("Saved Media found");
        let skipped = reader.skipped_keys().to_vec();
        let raws: Vec<RawEntry> = reader
            .enumerate()
            .map(|(i, r)| r.unwrap_or_else(|e| panic!("element {i}: {e}")))
            .collect();
        (skipped, raws)
    }

    fn json_bytes(&self) -> Vec<u8> {
        let mut archive = self.archive();
        let mut entry = archive.by_name(JSON_ENTRY).expect("json entry");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read json");
        bytes
    }
}

fn parsed_all(raws: &[RawEntry]) -> (Vec<ParsedEntry>, Summary) {
    let mut summary = Summary::default();
    let parsed: Vec<ParsedEntry> = raws
        .iter()
        .map(|r| {
            let p = parse_entry(r);
            summary.record(&p);
            p
        })
        .collect();
    (parsed, summary)
}

fn rfc3339(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .unwrap_or_else(|e| panic!("{s:?}: {e}"))
        .with_timezone(&Utc)
}

fn media_type_name(hint: MediaTypeHint) -> &'static str {
    match hint {
        MediaTypeHint::Photo => "photo",
        MediaTypeHint::Video => "video",
        MediaTypeHint::Unknown => "unknown",
    }
}

fn assert_location(entry: &ParsedEntry, want: Option<Loc>, context: &str) {
    match (entry.location, want) {
        (None, None) => {}
        (Some(got), Some(want)) => {
            assert!(
                (got.lat - want.lat).abs() < 1e-6 && (got.lon - want.lon).abs() < 1e-6,
                "{context}: location {got:?} != {want:?}",
                want = (want.lat, want.lon)
            );
        }
        (got, want) => panic!(
            "{context}: location {got:?} but truth says {:?}",
            want.map(|l| (l.lat, l.lon))
        ),
    }
}

fn sorted(v: &[String]) -> BTreeSet<&str> {
    v.iter().map(String::as_str).collect()
}

fn upper(s: &Option<String>) -> Option<String> {
    s.as_ref().map(|s| s.to_ascii_uppercase())
}

// ---------------------------------------------------------------------------
// F-01 small-2026b and F-15 small-2026a share their checks
// ---------------------------------------------------------------------------

/// JSON-side checks for a bundled-media fixture. `profile` is `2026a` or `2026b`.
fn check_2026_json(name: &str, profile: &str) {
    let fx = load(name);
    assert_eq!(fx.truth.format, profile);
    assert_eq!(fx.truth.generator.json_order, "newest_first");
    let (skipped, raws) = fx.stream_json();
    assert!(skipped.is_empty());
    assert_eq!(raws.len(), fx.truth.counts.json_entries);
    assert_eq!(raws.len(), fx.truth.counts.items);

    let (parsed, summary) = parsed_all(&raws);
    assert_eq!(summary.entries, fx.truth.counts.items);
    assert_eq!(summary.photos, fx.truth.counts.photos);
    assert_eq!(summary.videos, fx.truth.counts.videos);
    assert_eq!(summary.unknown_type, 0);
    assert_eq!(summary.with_location, fx.truth.counts.with_location);
    assert_eq!(summary.with_url, fx.truth.counts.with_url);
    assert_eq!(summary.with_tokens, fx.truth.counts.with_tokens);
    assert_eq!(summary.with_instant, fx.truth.counts.items);
    assert_eq!(
        summary.with_warnings, 0,
        "a clean 2026 export parses without warnings"
    );
    match profile {
        "2026a" => assert_eq!(
            summary.with_tokens, fx.truth.counts.items,
            "every 2026a entry has sid/mid"
        ),
        "2026b" => assert_eq!(summary.with_url, 0, "2026b entries carry no URLs"),
        other => panic!("unknown profile {other}"),
    }

    let items: Vec<Item2026> = fx.items();
    assert_eq!(items.len(), parsed.len());
    let mut previous: Option<DateTime<Utc>> = None;
    for (item, entry) in items.iter().zip(&parsed) {
        let context = format!("{name} item {} ({})", item.json_index, item.id);
        assert_eq!(entry.index, item.json_index);
        let want = rfc3339(&item.date_utc);
        assert_eq!(
            entry.date,
            Some(DateField::Instant(want)),
            "{context}: date"
        );
        assert!(
            previous.is_none_or(|p| p >= want),
            "{context}: JSON is newest first"
        );
        previous = Some(want);
        assert_eq!(
            want.format("%Y-%m-%d").to_string(),
            item.filename_date,
            "{context}"
        );
        assert_eq!(
            media_type_name(entry.media_type),
            item.media_type,
            "{context}: media type"
        );
        assert_location(entry, item.location, &context);
        let tokens = entry.url_tokens();
        assert_eq!(tokens.sid, upper(&item.sid), "{context}: sid");
        assert_eq!(tokens.mid, upper(&item.mid), "{context}: mid");
        if profile == "2026a" {
            // The exact-matching precondition of SRS v0.2 §3.6 step 1: the
            // upper-cased token equals the upper-cased filename ID.
            if let Some(file) = item.main.as_deref().or(item.overlay.as_deref()) {
                let (_, file_id) = parse_entry_path(file).expect("recognised name").pair_key();
                assert_eq!(
                    tokens.sid.as_deref(),
                    Some(file_id.as_str()),
                    "{context}: sid == filename id"
                );
                assert_eq!(
                    tokens.mid.as_deref(),
                    Some(file_id.as_str()),
                    "{context}: mid == filename id"
                );
            }
            assert!(
                !format!("{:?}", raws[item.json_index]).contains("sig="),
                "{context}: Debug leaks"
            );
        }
    }
}

/// ZIP-side checks for a bundled-media fixture.
fn check_2026_media(name: &str) {
    let fx = load(name);
    let names = fx.entry_names();
    assert_eq!(
        names, fx.truth.parts[0].entries,
        "truth records the ZIP entry order"
    );
    assert!(names.iter().any(|n| n == JSON_ENTRY));

    let media: Vec<(usize, &String)> = names
        .iter()
        .enumerate()
        .filter(|(_, n)| n.starts_with("memories/"))
        .collect();
    assert_eq!(media.len(), fx.truth.counts.media_files);
    let mut mains = 0;
    let mut overlays = 0;
    for (_, name) in &media {
        let parsed = parse_entry_path(name).unwrap_or_else(|| panic!("{name}: unrecognised"));
        match parsed.kind {
            FileKind::Main => mains += 1,
            FileKind::Overlay => overlays += 1,
            FileKind::MainNoSuffix => panic!("{name}: fixture writes -main names only"),
        }
    }
    // `main_files` counts the main files actually in the ZIP; videos that the
    // generator could not render (no ffmpeg) are in `main_files_skipped`.
    assert_eq!(mains, fx.truth.counts.main_files);
    assert_eq!(
        fx.truth.counts.main_files + fx.truth.counts.main_files_skipped,
        fx.truth.counts.items
    );
    assert_eq!(overlays, fx.truth.counts.overlays);

    let ffmpeg_available = fx.truth.generator.ffmpeg["available"].as_bool().unwrap();
    let items: Vec<Item2026> = fx.items();
    let mut skipped = 0;
    let mut main_positions: Vec<(usize, usize)> = Vec::new(); // (zip index, json index)
    for item in &items {
        let context = format!("{name} item {} ({})", item.json_index, item.id);
        match (&item.main, &item.main_skipped) {
            (Some(main), None) => {
                let p = parse_entry_path(main).unwrap_or_else(|| panic!("{context}: {main}"));
                assert_eq!(p.kind, FileKind::Main, "{context}");
                assert_eq!(p.id, item.id, "{context}");
                assert_eq!(
                    p.date.format("%Y-%m-%d").to_string(),
                    item.filename_date,
                    "{context}"
                );
                assert_eq!(p.ext, item.ext, "{context}");
                let zip_index = names
                    .iter()
                    .position(|n| n == main)
                    .expect("main is in the ZIP");
                assert_eq!(Some(zip_index), item.zip_index_main, "{context}: zip index");
                main_positions.push((zip_index, item.json_index));
                if let Some(overlay) = &item.overlay {
                    let o =
                        parse_entry_path(overlay).unwrap_or_else(|| panic!("{context}: {overlay}"));
                    assert_eq!(o.kind, FileKind::Overlay, "{context}");
                    assert_eq!(
                        o.pair_key(),
                        p.pair_key(),
                        "{context}: overlay pairs with main"
                    );
                    let oi = names
                        .iter()
                        .position(|n| n == overlay)
                        .expect("overlay is in the ZIP");
                    assert_eq!(Some(oi), item.zip_index_overlay, "{context}");
                }
            }
            (None, Some(reason)) => {
                assert_eq!(reason, "ffmpeg_missing", "{context}");
                assert_eq!(
                    item.media_type, "video",
                    "{context}: only videos can be skipped"
                );
                assert!(
                    !ffmpeg_available,
                    "{context}: skipped although ffmpeg was available"
                );
                skipped += 1;
            }
            other => panic!("{context}: inconsistent main/main_skipped {other:?}"),
        }
    }
    assert_eq!(skipped, fx.truth.counts.main_files_skipped);

    // The §3.6 ordinal-matching assumption: ZIP entry order follows JSON order.
    // truth.json records whether this fixture honours it, so a shuffled run
    // can be told apart from a bug. Passing here proves nothing about real
    // exports: the generator encodes the assumption (SRS v0.2 §11.3).
    let json_order_in_zip: Vec<usize> = {
        let mut v = main_positions.clone();
        v.sort_by_key(|(zip, _)| *zip);
        v.into_iter().map(|(_, json)| json).collect()
    };
    let follows_json = json_order_in_zip.windows(2).all(|w| w[0] < w[1]);
    match fx.truth.generator.zip_order.as_str() {
        "json" => assert!(
            follows_json,
            "zip_order=json but entries are not in JSON order"
        ),
        "shuffled" => assert!(
            !follows_json,
            "zip_order=shuffled but entries follow JSON order"
        ),
        other => panic!("unknown zip_order {other:?}"),
    }
}

#[test]
fn f01_small_2026b_json_entries_match_truth() {
    check_2026_json("small-2026b", "2026b");
}

#[test]
fn f01_small_2026b_media_filenames_match_truth() {
    check_2026_media("small-2026b");
}

#[test]
fn f15_small_2026a_json_entries_and_tokens_match_truth() {
    check_2026_json("small-2026a", "2026a");
}

#[test]
fn f15_small_2026a_media_filenames_match_truth() {
    check_2026_media("small-2026a");
}

// ---------------------------------------------------------------------------
// F-02 small-legacy
// ---------------------------------------------------------------------------

#[test]
fn f02_small_legacy_matches_truth() {
    let fx = load("small-legacy");
    assert_eq!(fx.truth.format, "legacy");
    let names = fx.entry_names();
    assert_eq!(names, fx.truth.parts[0].entries);
    assert!(
        !names.iter().any(|n| n.starts_with("memories/")),
        "legacy exports bundle no media"
    );
    assert_eq!(fx.truth.counts.media_files, 0);

    let (skipped, raws) = fx.stream_json();
    assert!(skipped.is_empty());
    assert_eq!(raws.len(), fx.truth.counts.json_entries);
    let (parsed, summary) = parsed_all(&raws);
    assert_eq!(summary.photos, fx.truth.counts.photos);
    assert_eq!(summary.videos, fx.truth.counts.videos);
    assert_eq!(summary.with_location, fx.truth.counts.with_location);
    assert_eq!(summary.with_url, fx.truth.counts.with_url);
    assert_eq!(
        summary.with_url, fx.truth.counts.items,
        "every legacy entry has URLs"
    );
    assert_eq!(summary.with_warnings, 0);

    let items: Vec<ItemLegacy> = fx.items();
    for (item, entry) in items.iter().zip(&parsed) {
        let context = format!("item {}", item.json_index);
        assert_eq!(entry.index, item.json_index);
        assert_eq!(
            entry.date,
            Some(DateField::Instant(rfc3339(&item.date_utc))),
            "{context}"
        );
        assert_eq!(
            media_type_name(entry.media_type),
            item.media_type,
            "{context}"
        );
        assert_location(entry, item.location, &context);
        assert_eq!(
            entry.download_link.as_deref(),
            Some(item.download_link.as_str()),
            "{context}"
        );
        assert_eq!(
            entry.media_download_url.as_deref(),
            Some(item.media_download_url.as_str()),
            "{context}"
        );
        let tokens = entry.url_tokens();
        assert_eq!(
            tokens.sid.as_deref(),
            Some(item.sid.to_ascii_uppercase().as_str()),
            "{context}: sid"
        );
        assert_eq!(
            tokens.mid.as_deref(),
            Some(item.mid.to_ascii_uppercase().as_str()),
            "{context}: mid"
        );
        let redacted = redact_url(&item.download_link);
        assert_eq!(redacted, item.download_link_redacted, "{context}");
        assert!(
            !redacted.contains("sig="),
            "{context}: signature must not survive redaction"
        );
        assert!(
            !format!("{:?}", raws[item.json_index]).contains("sig="),
            "{context}: Debug leaks"
        );
    }
}

// ---------------------------------------------------------------------------
// F-10 weird-json
// ---------------------------------------------------------------------------

#[test]
fn f10_weird_json_is_actually_weird() {
    let fx = load("weird-json");
    let bytes = fx.json_bytes();
    assert_eq!(fx.truth.json.bom, bytes.starts_with(&[0xEF, 0xBB, 0xBF]));
    assert!(
        fx.truth.json.bom,
        "fixture must start with a byte-order mark"
    );
    assert_eq!(fx.truth.json.crlf, bytes.windows(2).any(|w| w == b"\r\n"));
    assert!(fx.truth.json.crlf, "fixture must use CRLF line endings");
    assert!(bytes.contains(&b'\t'), "fixture is tab-indented");
}

#[test]
fn f10_weird_json_matches_truth() {
    let fx = load("weird-json");
    let (skipped, raws) = fx.stream_json();
    assert_eq!(skipped, fx.truth.json.top_level_keys_before);
    assert_eq!(raws.len(), fx.truth.counts.json_entries);
    let (parsed, summary) = parsed_all(&raws);
    assert_eq!(summary.photos, fx.truth.counts.photos);
    assert_eq!(summary.videos, fx.truth.counts.videos);
    assert_eq!(summary.unknown_type, fx.truth.counts.unknown_type);
    assert_eq!(summary.with_location, fx.truth.counts.with_location);
    assert_eq!(summary.with_instant, fx.truth.counts.with_instant);
    assert_eq!(summary.with_url, fx.truth.counts.with_url);

    let items: Vec<ItemWeird> = fx.items();
    for (item, (raw, entry)) in items.iter().zip(raws.iter().zip(&parsed)) {
        let want = &item.expected;
        let context = format!("case {} (index {})", item.case, item.json_index);
        assert_eq!(entry.index, item.json_index);

        match want.date_kind.as_str() {
            "instant" => {
                let dt = rfc3339(want.date_utc.as_deref().expect("instant has date_utc"));
                assert_eq!(entry.date, Some(DateField::Instant(dt)), "{context}: date");
            }
            "day_only" => assert!(
                matches!(entry.date, Some(DateField::DayOnly(_))),
                "{context}: {:?}",
                entry.date
            ),
            "unparseable" => {
                assert_eq!(entry.date, Some(DateField::Unparseable), "{context}");
                assert!(
                    entry.warnings.contains(&Warning::DateUnparseable),
                    "{context}"
                );
            }
            "missing" => {
                assert_eq!(entry.date, None, "{context}");
                assert!(entry.warnings.contains(&Warning::DateMissing), "{context}");
            }
            other => panic!("{context}: unknown date_kind {other}"),
        }

        assert_eq!(
            media_type_name(entry.media_type),
            want.media_type,
            "{context}: media type"
        );
        assert_location(entry, want.location, &context);
        let reason = match parse_location(raw.location.as_deref()) {
            LocationParse::Found(_) => "found",
            LocationParse::Empty => "missing",
            LocationParse::Unrecognised => "unrecognised",
            LocationParse::NullIsland => "null_island",
            LocationParse::OutOfRange => "out_of_range",
        };
        assert_eq!(reason, want.location_reason, "{context}: location reason");
        match reason {
            "missing" => assert!(
                entry.warnings.contains(&Warning::LocationMissing),
                "{context}"
            ),
            "unrecognised" => assert!(
                entry.warnings.contains(&Warning::LocationUnrecognised),
                "{context}"
            ),
            "out_of_range" => assert!(
                entry.warnings.contains(&Warning::LocationOutOfRange),
                "{context}"
            ),
            _ => {}
        }

        assert_eq!(
            entry.download_link, want.download_link,
            "{context}: download link"
        );
        assert_eq!(
            entry.media_download_url, want.media_download_url,
            "{context}: media url"
        );
        let tokens = entry.url_tokens();
        assert_eq!(tokens.sid, upper(&want.sid), "{context}: sid");
        assert_eq!(tokens.mid, upper(&want.mid), "{context}: mid");

        assert_eq!(
            sorted(&raw.unknown_fields),
            sorted(&want.unknown_fields),
            "{context}: unknown fields"
        );
        assert_eq!(
            sorted(&raw.non_string_fields),
            sorted(&want.non_string_fields),
            "{context}: non-string fields"
        );
        for name in &want.unknown_fields {
            assert!(
                entry
                    .warnings
                    .contains(&Warning::UnknownField(name.clone())),
                "{context}: warning for {name}"
            );
        }
    }
}
