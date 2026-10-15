//! M1 integration tests: discovery, merged index, summary and matching over
//! F-01, F-15, F-04, F-05, F-06 and F-14, against each fixture's truth.json.
//!
//! Also covered: FR-11 AC1 (at least 99% correct capture times on an ordered
//! 2026b export), a shuffled ZIP being detected and refused rather than
//! matched wrong, and folder input (FR-05 AC2).

mod common;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Utc};
use common::{fixture_dir, generate_variant, zips_in};
use mr_core::discover::{Discovery, Source, discover};
use mr_core::fields::DatePrecision;
use mr_core::index::{ExportSummary, MergedIndex, Profile, build_index, load_json, summarize};
use mr_core::issue::Code;
use mr_core::matcher::{
    Confidence, ItemStatus, LocationDecision, MatchOptions, MatchReason, MatchReport, OrderVerdict,
    UnmatchedReason, match_export, order_agreement,
};
use mr_core::zipindex::{CrcCheck, UnsafeReason};
use serde::Deserialize;
use serde_json::Value;

// ---------------------------------------------------------------------------
// truth.json (schema 1, M1 fields)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Truth {
    format: String,
    generator: Generator,
    parts: Vec<PartT>,
    counts: Counts,
    items: Vec<Value>,
    #[serde(default)]
    uncertain_days: Vec<String>,
    missing_part: Option<MissingPart>,
    truncated_part: Option<TruncatedPart>,
    corrupt_entry: Option<CorruptEntry>,
    entries: Option<Vec<SlipEntry>>,
}

#[derive(Deserialize)]
struct Generator {
    zip_order: String,
}

#[derive(Deserialize)]
struct PartT {
    file: String,
    role: String,
    #[serde(default)]
    number: Option<u32>,
    #[serde(default)]
    renamed: bool,
    #[serde(default = "yes")]
    present: bool,
    entries: Vec<String>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Counts {
    json_entries: usize,
    items: usize,
    photos: usize,
    videos: usize,
    overlays: usize,
    with_location: usize,
    with_url: usize,
    with_tokens: usize,
    media_files: usize,
    main_files: usize,
    main_files_skipped: usize,
    multi_item_groups: usize,
    items_in_multi_item_groups: usize,
    expected_location_uncertain: usize,
    expected_second_precision: usize,
    refused_entries: usize,
}

#[derive(Deserialize)]
struct MissingPart {
    number: u32,
    lost_mains: usize,
    lost_overlays: usize,
    orphan_overlays: Vec<String>,
}

#[derive(Deserialize)]
struct TruncatedPart {
    number: u32,
    readable: Vec<String>,
    partial: Option<String>,
    lost: Vec<String>,
}

#[derive(Deserialize)]
struct CorruptEntry {
    part: u32,
    name: String,
    crc32: u32,
}

#[derive(Deserialize)]
struct SlipEntry {
    name: String,
    refused: bool,
    reason: Option<String>,
}

#[derive(Deserialize, Clone, Copy)]
struct Loc {
    lat: f64,
    lon: f64,
}

#[derive(Deserialize)]
struct FileRef {
    part: u32,
    name: String,
}

/// An item of the multipart family.
#[derive(Deserialize)]
struct ItemM1 {
    json_index: usize,
    id: String,
    date_utc: String,
    main: FileRef,
    overlay: Option<FileRef>,
    location: Option<Loc>,
    day_group_size: usize,
    expected: Expected,
    #[serde(default)]
    overlay_present: Option<bool>,
    #[serde(default)]
    crc_corrupt: bool,
}

#[derive(Deserialize)]
struct Expected {
    status: String,
    #[serde(default)]
    confidence: Option<String>,
    #[serde(default)]
    time_precision: Option<String>,
    #[serde(default)]
    location_decision: Option<String>,
}

fn load_truth(dir: &Path) -> Truth {
    let text = fs::read_to_string(dir.join("truth.json")).expect("truth.json");
    serde_json::from_str(&text).expect("truth.json shape")
}

fn rfc3339(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

/// `json_index -> (capture instant, main entry name)` for any fixture shape.
fn expected_times(truth: &Truth) -> HashMap<usize, (DateTime<Utc>, Option<String>)> {
    truth
        .items
        .iter()
        .map(|v| {
            let index = v["json_index"].as_u64().unwrap() as usize;
            let date = rfc3339(v["date_utc"].as_str().unwrap());
            let main = match &v["main"] {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("name").and_then(Value::as_str).map(str::to_string),
                _ => None,
            };
            (index, (date, main))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Pipeline runner
// ---------------------------------------------------------------------------

struct Run {
    d: Discovery,
    export: usize,
    index: MergedIndex,
    summary: ExportSummary,
    report: MatchReport,
    index_millis: u128,
}

fn run_inputs(inputs: &[PathBuf], opts: &MatchOptions) -> Run {
    let started = Instant::now();
    let d = discover(inputs);
    assert_eq!(
        d.exports.len(),
        1,
        "one export expected, got {:?}",
        d.exports
    );
    let export = &d.exports[0];
    let index = build_index(&d, export);
    let index_millis = started.elapsed().as_millis();
    let json = load_json(&d, &index);
    let summary = summarize(&d, export, &index, json.as_ref());
    let entries = json.as_ref().map(|j| j.entries.as_slice()).unwrap_or(&[]);
    let report = match_export(entries, &index, opts);
    let export = export.id;
    Run {
        d,
        export,
        index,
        summary,
        report,
        index_millis,
    }
}

fn run_dir(dir: &Path) -> Run {
    run_inputs(&zips_in(dir), &MatchOptions::default())
}

/// Fraction of items whose matched capture time equals the truth to the
/// second (items without a JSON match count as wrong).
fn time_accuracy(run: &Run, truth: &Truth) -> (usize, usize) {
    let expected = expected_times(truth);
    let by_main: HashMap<&str, (DateTime<Utc>, usize)> = expected
        .iter()
        .filter_map(|(i, (t, main))| main.as_deref().map(|m| (m, (*t, *i))))
        .collect();
    let mut correct = 0;
    let mut wrong_second_precision = Vec::new();
    for item in &run.report.items {
        let Some(&(want, _)) = by_main.get(item.main.name.as_str()) else {
            continue;
        };
        if item.date.precision == DatePrecision::Second {
            if item.date.utc == want {
                correct += 1;
            } else {
                wrong_second_precision.push((item.main.name.clone(), item.date.utc, want));
            }
        }
    }
    assert!(
        wrong_second_precision.is_empty(),
        "items given a wrong exact time: {wrong_second_precision:?}"
    );
    (correct, run.report.items.len())
}

fn part_by_label<'a>(run: &'a Run, label: &str) -> &'a mr_core::discover::Part {
    run.d
        .parts
        .iter()
        .find(|p| p.label == label)
        .unwrap_or_else(|| panic!("part {label} not found"))
}

// ---------------------------------------------------------------------------
// F-01 small-2026b and F-15 small-2026a
// ---------------------------------------------------------------------------

fn check_small(name: &str, profile: Profile, confidence: Confidence) {
    let dir = fixture_dir(name);
    let truth = load_truth(&dir);
    let run = run_dir(&dir);
    assert_eq!(run.summary.profile, profile, "{name}");
    assert_eq!(run.summary.json_entries, truth.counts.json_entries);
    assert_eq!(run.summary.parts, 1);
    assert_eq!(run.summary.main_files, truth.counts.main_files);
    assert_eq!(run.summary.overlays, truth.counts.overlays);
    assert_eq!(run.summary.with_location, truth.counts.with_location);
    assert_eq!(run.summary.profile_counts.exact, truth.counts.with_tokens);
    assert_eq!(
        run.summary.profile_counts.ordinal,
        truth.counts.json_entries - truth.counts.with_tokens
    );
    assert_eq!(
        run.summary.photos + run.summary.videos,
        truth.counts.main_files
    );
    assert!(run.summary.date_range.is_some());
    assert!(run.report.order.is_monotone(), "{:?}", run.report.order);
    assert_eq!(run.report.items.len(), truth.counts.main_files);
    for item in &run.report.items {
        assert_eq!(item.confidence, confidence, "{name}: {}", item.main.name);
        assert_eq!(
            item.date.precision,
            DatePrecision::Second,
            "{}",
            item.main.name
        );
    }
    let (correct, total) = time_accuracy(&run, &truth);
    assert_eq!(correct, total, "{name}: every item carries its exact time");
    // Entries whose video was not rendered (no ffmpeg) are missing media, nothing else is.
    assert_eq!(
        run.report.missing_media().count(),
        truth.counts.main_files_skipped
    );
    assert!(run.report.orphan_overlays.is_empty());
    assert_eq!(run.report.token_disagreements, 0);
    // Overlays are paired.
    let with_overlay = run
        .report
        .items
        .iter()
        .filter(|i| i.overlay.is_some())
        .count();
    let expected_overlays = truth
        .items
        .iter()
        .filter(|v| !v["overlay"].is_null() && !v["main"].is_null())
        .count();
    assert_eq!(with_overlay, expected_overlays);
}

#[test]
fn f01_small_2026b_pipeline() {
    check_small("small-2026b", Profile::P2026b, Confidence::Ordinal);
}

#[test]
fn f15_small_2026a_pipeline() {
    check_small("small-2026a", Profile::P2026a, Confidence::Exact);
}

#[test]
fn f02_small_legacy_is_links_only() {
    let dir = fixture_dir("small-legacy");
    let truth = load_truth(&dir);
    let run = run_dir(&dir);
    assert_eq!(
        run.summary.profile,
        Profile::Legacy,
        "links with sid/mid but no media are legacy"
    );
    assert_eq!(
        run.summary.profile_counts.metadata_only,
        truth.counts.json_entries
    );
    assert_eq!(run.summary.profile_counts.exact, 0);
    assert!(
        run.summary
            .issues
            .iter()
            .any(|i| i.code == Code::LegacyLinksOnly)
    );
    assert!(run.report.items.is_empty());
    assert_eq!(
        run.report.missing_media().count(),
        truth.counts.json_entries
    );
}

// ---------------------------------------------------------------------------
// F-04 multipart
// ---------------------------------------------------------------------------

#[test]
fn f04_multipart_discovery_index_and_summary() {
    let dir = fixture_dir("multipart");
    let truth = load_truth(&dir);
    assert_eq!(truth.generator.zip_order, "json");
    assert_eq!(truth.format, "2026b");
    let run = run_dir(&dir);
    println!(
        "FR-06 index: {} entries in {} parts built in {} ms",
        run.index.mains.len() + run.index.overlays.len(),
        run.index.parts,
        run.index_millis
    );
    assert!(run.index_millis < 60_000, "FR-06 AC1 budget");

    let export = &run.d.exports[run.export];
    assert_eq!(export.base.as_deref(), Some("mydata~1760000000000"));
    assert_eq!(export.parts.len(), 4);
    assert!(export.missing_numbers.is_empty());
    let primary = &run.d.parts[export.primary.unwrap()];
    assert_eq!(primary.label, truth.parts[0].file);
    assert_eq!(primary.number(), Some(1));
    let renamed = truth.parts.iter().find(|p| p.renamed).unwrap();
    let renamed_part = part_by_label(&run, &renamed.file);
    assert!(
        export.attached_by_content.contains(&renamed_part.id),
        "renamed part attached by content (FR-02 AC2)"
    );
    for p in &truth.parts {
        assert!(p.present);
        let part = part_by_label(&run, &p.file);
        let names: Vec<String> = part.entries().into_iter().map(|e| e.name).collect();
        assert_eq!(names, p.entries, "{}: entry order", p.file);
        assert_eq!(part.has_json, p.role == "primary");
        assert_eq!(
            part.number(),
            p.number,
            "{}: part number (renamed part placed by its dates)",
            p.file
        );
    }

    assert_eq!(run.summary.profile, Profile::P2026b);
    assert_eq!(run.summary.parts, 4);
    assert_eq!(run.summary.json_entries, truth.counts.json_entries);
    assert_eq!(run.summary.main_files, truth.counts.main_files);
    assert_eq!(run.summary.photos, truth.counts.photos);
    assert_eq!(run.summary.videos, truth.counts.videos);
    assert_eq!(run.summary.overlays, truth.counts.overlays);
    assert_eq!(run.summary.with_location, truth.counts.with_location);
    assert_eq!(run.summary.unrecognised, 0);
    assert_eq!(run.summary.duplicates, 0);
    assert_eq!(run.summary.refused, 0);
    assert!(run.summary.ordered);
    assert_eq!(
        run.index.mains.len() + run.index.overlays.len(),
        truth.counts.media_files
    );
    let (lo, hi) = run.summary.date_range.unwrap();
    let times = expected_times(&truth);
    let want_lo = times.values().map(|(t, _)| *t).min().unwrap();
    let want_hi = times.values().map(|(t, _)| *t).max().unwrap();
    assert_eq!((lo, hi), (want_lo, want_hi));
    assert!(run.summary.total_bytes > run.summary.media_bytes);
}

#[test]
fn f04_multipart_matching_meets_fr11_ac1_and_truth_decisions() {
    let dir = fixture_dir("multipart");
    let truth = load_truth(&dir);
    let run = run_dir(&dir);
    assert!(run.report.order.is_monotone(), "{:?}", run.report.order);
    assert_eq!(run.report.items.len(), truth.counts.items);
    assert!(
        run.report.unmatched.is_empty(),
        "{:?}",
        &run.report.unmatched[..5.min(run.report.unmatched.len())]
    );
    assert!(run.report.orphan_overlays.is_empty());
    assert!(run.report.duplicate_mains.is_empty());

    let items: Vec<ItemM1> = truth
        .items
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    let by_main: HashMap<&str, &mr_core::matcher::Item> = run
        .report
        .items
        .iter()
        .map(|i| (i.main.name.as_str(), i))
        .collect();
    for want in &items {
        let got = by_main[want.main.name.as_str()];
        let context = format!(
            "item {} ({}, group of {})",
            want.json_index, want.id, want.day_group_size
        );
        assert_eq!(got.json_index, Some(want.json_index), "{context}");
        assert_eq!(
            run.d.parts[got.main.part].number(),
            Some(want.main.part),
            "{context}: main part"
        );
        assert_eq!(
            got.confidence.as_str(),
            want.expected.confidence.as_deref().unwrap(),
            "{context}: confidence"
        );
        let precision = match got.date.precision {
            DatePrecision::Second => "second",
            DatePrecision::Day => "day",
        };
        assert_eq!(
            precision,
            want.expected.time_precision.as_deref().unwrap(),
            "{context}: precision"
        );
        assert_eq!(
            got.location_decision.as_str(),
            want.expected.location_decision.as_deref().unwrap(),
            "{context}: location decision"
        );
        assert_eq!(
            got.status.as_str(),
            want.expected.status,
            "{context}: status"
        );
        if got.date.precision == DatePrecision::Second {
            assert_eq!(got.date.utc, rfc3339(&want.date_utc), "{context}: time");
        }
        if got.location_decision == LocationDecision::Applied {
            let loc = got.location.expect("applied means present");
            let w = want.location.expect("truth has it");
            assert!(
                (loc.lat - w.lat).abs() < 1e-6 && (loc.lon - w.lon).abs() < 1e-6,
                "{context}"
            );
        } else {
            assert!(got.location.is_none(), "{context}: no GPS unless applied");
        }
        match &want.overlay {
            Some(o) => {
                let got_overlay = got
                    .overlay
                    .as_ref()
                    .unwrap_or_else(|| panic!("{context}: overlay missing"));
                assert_eq!(got_overlay.name, o.name, "{context}");
                assert_ne!(
                    got_overlay.part, got.main.part,
                    "{context}: overlay sits in another part"
                );
            }
            None => assert!(got.overlay.is_none(), "{context}"),
        }
        let reason_ok = match want.day_group_size {
            1 => got.reason == MatchReason::OrdinalSingle,
            _ => got.reason == MatchReason::OrdinalPosition,
        };
        assert!(reason_ok, "{context}: reason {:?}", got.reason);
    }

    let (correct, total) = time_accuracy(&run, &truth);
    println!("FR-11 AC1: {correct}/{total} items with the correct capture time");
    assert!(correct * 100 >= total * 99, "FR-11 AC1: {correct}/{total}");
    assert_eq!(correct, truth.counts.expected_second_precision);
    assert_eq!(
        run.report.counts.location_uncertain,
        truth.counts.expected_location_uncertain
    );
    assert_eq!(run.report.counts.ordinal, truth.counts.items);
    let uncertain_groups: BTreeSet<String> = run
        .report
        .groups
        .iter()
        .filter(|g| g.outcome == mr_core::matcher::GroupOutcome::LocationUncertain)
        .map(|g| {
            format!(
                "{}/{}",
                g.date,
                g.media_type
                    .map(|m| if m == mr_core::fields::MediaType::Photo {
                        "photo"
                    } else {
                        "video"
                    })
                    .unwrap()
            )
        })
        .collect();
    assert_eq!(
        uncertain_groups,
        truth.uncertain_days.iter().cloned().collect()
    );
    assert_eq!(
        run.report.groups.iter().filter(|g| g.entries > 1).count(),
        truth.counts.multi_item_groups
    );
}

// ---------------------------------------------------------------------------
// F-05 missing-part
// ---------------------------------------------------------------------------

#[test]
fn f05_missing_part_is_reported_and_its_items_are_missing_media() {
    let dir = fixture_dir("missing-part");
    let truth = load_truth(&dir);
    let missing = truth.missing_part.as_ref().unwrap();
    let run = run_dir(&dir);
    let export = &run.d.exports[run.export];
    assert_eq!(export.parts.len(), 3);
    assert_eq!(truth.parts.iter().filter(|p| p.present).count(), 3);
    assert_eq!(export.missing_numbers, [missing.number]);
    assert!(
        export.issues.iter().any(|i| i.code == Code::PartMissing
            && i.detail.contains(&format!("Part {}", missing.number)))
    );
    assert_eq!(run.summary.missing_parts, [missing.number]);
    assert_eq!(run.summary.main_files, truth.counts.main_files);
    assert_eq!(run.summary.overlays, truth.counts.overlays);

    let items: Vec<ItemM1> = truth
        .items
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    let lost: BTreeSet<usize> = items
        .iter()
        .filter(|i| i.expected.status == "missing_media")
        .map(|i| i.json_index)
        .collect();
    assert_eq!(lost.len(), missing.lost_mains);
    let unmatched: BTreeSet<usize> = run
        .report
        .unmatched
        .iter()
        .inspect(|u| assert_eq!(u.reason, UnmatchedReason::NoFile))
        .map(|u| u.json_index)
        .collect();
    assert_eq!(
        unmatched, lost,
        "exactly the entries whose main sat in part {}",
        missing.number
    );
    assert_eq!(
        run.report.items.len(),
        truth.counts.items - missing.lost_mains
    );

    let by_main: HashMap<&str, &mr_core::matcher::Item> = run
        .report
        .items
        .iter()
        .map(|i| (i.main.name.as_str(), i))
        .collect();
    let mut overlay_lost = 0;
    for want in items
        .iter()
        .filter(|i| i.expected.status != "missing_media")
    {
        let got = by_main[want.main.name.as_str()];
        if want.overlay_present == Some(false) {
            assert!(
                got.overlay.is_none(),
                "{}: overlay was in the missing part",
                want.id
            );
            overlay_lost += 1;
        } else if let Some(o) = &want.overlay {
            assert_eq!(
                got.overlay.as_ref().map(|e| e.name.as_str()),
                Some(o.name.as_str())
            );
        }
        // Remaining items keep their decisions: the day groups did not change.
        assert_eq!(
            got.confidence.as_str(),
            want.expected.confidence.as_deref().unwrap()
        );
    }
    assert_eq!(overlay_lost, missing.lost_overlays);
    let orphans: BTreeSet<&str> = run
        .report
        .orphan_overlays
        .iter()
        .map(|e| e.name.as_str())
        .collect();
    let want_orphans: BTreeSet<&str> = missing.orphan_overlays.iter().map(String::as_str).collect();
    assert_eq!(
        orphans, want_orphans,
        "overlays whose mains were in the missing part"
    );
}

// ---------------------------------------------------------------------------
// F-06 truncated
// ---------------------------------------------------------------------------

#[test]
fn f06_truncated_part_keeps_readable_entries_and_crc_damage_is_detected() {
    let dir = fixture_dir("truncated");
    let truth = load_truth(&dir);
    let trunc = truth.truncated_part.as_ref().unwrap();
    let corrupt = truth.corrupt_entry.as_ref().unwrap();
    let run = run_dir(&dir);
    let export = &run.d.exports[run.export];
    assert_eq!(export.parts.len(), 4);
    assert!(export.missing_numbers.is_empty());
    assert_eq!(run.summary.truncated_parts, 1);
    let truncated_file = &truth
        .parts
        .iter()
        .find(|p| p.number == Some(trunc.number))
        .unwrap()
        .file;
    let part = part_by_label(&run, truncated_file);
    assert!(part.is_truncated());
    assert!(
        part.issues.iter().any(|i| i.code == Code::PartTruncated),
        "{:?}",
        part.issues
    );
    let Source::Zip(zip) = &part.source else {
        panic!("zip part")
    };
    let readable: Vec<&str> = zip.usable().map(|e| e.name.as_str()).collect();
    assert_eq!(
        readable,
        trunc
            .readable
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    match &zip.integrity {
        mr_core::zipindex::Integrity::Truncated {
            partial, readable, ..
        } => {
            assert_eq!(partial.as_deref(), trunc.partial.as_deref());
            assert_eq!(*readable, trunc.readable.len());
        }
        other => panic!("{other:?}"),
    }
    // Lost entries are exactly the truth's lost list plus the partial one: none of them is indexed.
    let indexed: BTreeSet<&str> = run
        .index
        .mains
        .iter()
        .chain(&run.index.overlays)
        .map(|f| f.entry.name.as_str())
        .collect();
    for lost in trunc.lost.iter().chain(trunc.partial.iter()) {
        assert!(
            !indexed.contains(lost.as_str()),
            "{lost} must not be indexed"
        );
    }
    for name in &trunc.readable {
        assert!(indexed.contains(name.as_str()), "{name} must stay usable");
    }
    assert_eq!(run.summary.main_files, truth.counts.main_files);
    assert_eq!(run.summary.overlays, truth.counts.overlays);
    assert_eq!(
        run.report.missing_media().count(),
        truth.counts.items - truth.counts.main_files
    );

    // The CRC-damaged entry in part 1 is listed as usable (its name is fine)
    // and fails verification when read (ZIP_ENTRY_CRC).
    let part1 = &truth
        .parts
        .iter()
        .find(|p| p.number == Some(corrupt.part))
        .unwrap()
        .file;
    let part1 = part_by_label(&run, part1);
    let Source::Zip(zip1) = &part1.source else {
        panic!("zip part")
    };
    let bad = zip1.find(&corrupt.name).expect("corrupt entry is listed");
    assert_eq!(bad.crc32, corrupt.crc32);
    match zip1.verify_crc(bad.index).unwrap() {
        CrcCheck::Mismatch { recorded, .. } => assert_eq!(recorded, corrupt.crc32),
        CrcCheck::Ok => panic!("damage not detected"),
    }
    let good = zip1
        .usable()
        .find(|e| e.name != corrupt.name && e.name.ends_with("-main.jpg"))
        .unwrap();
    assert_eq!(zip1.verify_crc(good.index).unwrap(), CrcCheck::Ok);
    let items: Vec<ItemM1> = truth
        .items
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    assert_eq!(items.iter().filter(|i| i.crc_corrupt).count(), 1);
}

// ---------------------------------------------------------------------------
// F-14 zipslip
// ---------------------------------------------------------------------------

#[test]
fn f14_zipslip_entries_are_refused_and_never_indexed() {
    let dir = fixture_dir("zipslip");
    let truth = load_truth(&dir);
    let entries = truth.entries.as_ref().unwrap();
    let run = run_dir(&dir);
    let part = &run.d.parts[0];
    let Source::Zip(zip) = &part.source else {
        panic!("zip part")
    };
    for want in entries {
        let got = zip
            .find(&want.name)
            .unwrap_or_else(|| panic!("{} listed", want.name));
        let reason = got.unsafe_reason.map(|r| match r {
            UnsafeReason::ParentTraversal => "parent_traversal",
            UnsafeReason::AbsolutePath => "absolute_path",
            UnsafeReason::DriveLetter => "drive_letter",
            UnsafeReason::Symlink => "symlink",
            UnsafeReason::EmptyName => "empty",
            UnsafeReason::ControlCharacter => "control",
        });
        assert_eq!(got.unsafe_reason.is_some(), want.refused, "{}", want.name);
        assert_eq!(reason, want.reason.as_deref(), "{}", want.name);
        assert_eq!(got.is_usable(), !want.refused, "{}", want.name);
    }
    assert_eq!(run.summary.refused, truth.counts.refused_entries);
    assert!(part.issues.iter().any(|i| i.code == Code::EntryRefused));
    let indexed: Vec<&str> = run
        .index
        .mains
        .iter()
        .chain(&run.index.overlays)
        .map(|f| f.entry.name.as_str())
        .chain(run.index.unrecognised.iter().map(|e| e.name.as_str()))
        .collect();
    for want in entries.iter().filter(|e| e.refused) {
        assert!(
            !indexed.contains(&want.name.as_str()),
            "{} leaked into the index",
            want.name
        );
        assert!(
            zip.with_entry(zip.find(&want.name).unwrap().index, |_| Ok(()))
                .is_err()
        );
    }
    assert_eq!(run.summary.main_files, truth.counts.main_files);
    assert_eq!(run.report.items.len(), 2);
    let (correct, total) = time_accuracy(&run, &truth);
    assert_eq!((correct, total), (2, 2));
}

// ---------------------------------------------------------------------------
// Shuffled ZIP order and folder input
// ---------------------------------------------------------------------------

#[test]
fn shuffled_zip_order_is_detected_and_reported_not_matched_wrong() {
    let dir = generate_variant("multipart", "shuffled", &["--zip-order", "shuffled"]);
    let truth = load_truth(&dir);
    assert_eq!(truth.generator.zip_order, "shuffled");
    let run = run_dir(&dir);
    assert!(!run.report.order.is_monotone(), "{:?}", run.report.order);
    assert!(run.report.order.violations > 0);
    let items: Vec<ItemM1> = truth
        .items
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    let by_main: HashMap<&str, &mr_core::matcher::Item> = run
        .report
        .items
        .iter()
        .map(|i| (i.main.name.as_str(), i))
        .collect();
    let mut refused = 0;
    for want in &items {
        let got = by_main[want.main.name.as_str()];
        if want.day_group_size > 1 {
            assert_eq!(got.confidence, Confidence::None, "{}", want.id);
            assert_eq!(got.reason, MatchReason::ZipOrderNotMonotone, "{}", want.id);
            assert_eq!(got.date.precision, DatePrecision::Day, "{}", want.id);
            assert_eq!(got.status, ItemStatus::MetadataPartial);
            assert!(got.location.is_none());
            refused += 1;
        } else {
            assert_eq!(got.confidence, Confidence::Ordinal, "{}", want.id);
            assert_eq!(got.date.utc, rfc3339(&want.date_utc), "{}", want.id);
        }
    }
    assert_eq!(refused, truth.counts.items_in_multi_item_groups);
    assert_eq!(run.report.counts.metadata_partial, refused);
    // The honesty check: nothing got an exact time that is wrong.
    let (correct, total) = time_accuracy(&run, &truth);
    assert_eq!(correct, total - refused);
    println!(
        "shuffled: {} order violations over {} pairs; {refused} items refused positional matching",
        run.report.order.violations, run.report.order.checked
    );
}

#[test]
fn folder_input_gives_day_precision_to_multi_item_groups() {
    let dir = fixture_dir("multipart");
    let truth = load_truth(&dir);
    let folder = std::env::temp_dir().join(format!("mr-core-folder-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    for zip_path in zips_in(&dir) {
        let mut archive = zip::ZipArchive::new(fs::File::open(&zip_path).unwrap()).unwrap();
        archive.extract(&folder).unwrap();
    }
    let run = run_inputs(std::slice::from_ref(&folder), &MatchOptions::default());
    assert!(!run.summary.ordered);
    assert!(
        run.summary
            .issues
            .iter()
            .any(|i| i.code == Code::MatchPartial)
    );
    assert_eq!(run.summary.main_files, truth.counts.main_files);
    assert_eq!(run.summary.overlays, truth.counts.overlays);
    assert!(!run.report.order.applicable);
    let items: Vec<ItemM1> = truth
        .items
        .iter()
        .map(|v| serde_json::from_value(v.clone()).unwrap())
        .collect();
    let by_main: HashMap<&str, &mr_core::matcher::Item> = run
        .report
        .items
        .iter()
        .map(|i| (i.main.name.as_str(), i))
        .collect();
    for want in &items {
        let got = by_main[want.main.name.as_str()];
        if want.day_group_size > 1 {
            assert_eq!(got.confidence, Confidence::None, "{}", want.id);
            assert_eq!(got.reason, MatchReason::NoEntryOrder);
            assert_eq!(got.status, ItemStatus::MetadataPartial);
            assert_eq!(got.date.precision, DatePrecision::Day);
        } else {
            assert_eq!(got.confidence, Confidence::Ordinal, "{}", want.id);
            assert_eq!(got.date.utc, rfc3339(&want.date_utc));
            if let Some(o) = &want.overlay {
                assert_eq!(
                    got.overlay.as_ref().map(|e| e.name.as_str()),
                    Some(o.name.as_str())
                );
            }
        }
    }
    let (correct, total) = time_accuracy(&run, &truth);
    assert_eq!(correct, total - truth.counts.items_in_multi_item_groups);
    let _ = fs::remove_dir_all(&folder);
}

// ---------------------------------------------------------------------------
// Order agreement (Q-03) on a 2026a multipart export
// ---------------------------------------------------------------------------

#[test]
fn order_check_holds_on_ordered_2026a_export_and_fails_when_shuffled() {
    let dir = generate_variant("multipart", "2026a", &["--multipart-profile", "2026a"]);
    let truth = load_truth(&dir);
    assert_eq!(truth.format, "2026a");
    let run = run_dir(&dir);
    assert_eq!(run.summary.profile, Profile::P2026a);
    assert_eq!(run.report.counts.exact, truth.counts.items);
    assert_eq!(
        run.report.counts.location_uncertain, 0,
        "exact matches apply locations directly"
    );
    let (correct, total) = time_accuracy(&run, &truth);
    assert_eq!(correct, total);
    let agreement = order_agreement(&run.report);
    assert_eq!(agreement.exact_items, truth.counts.items);
    assert_eq!(agreement.days_checked, truth.counts.multi_item_groups);
    assert_eq!(agreement.days_agreeing, agreement.days_checked);
    assert_eq!(agreement.adjacent_agreeing, agreement.adjacent_pairs);
    assert_eq!(
        agreement.verdict(run.report.order.violations),
        OrderVerdict::Holds
    );
    println!(
        "order-check (ordered): {}/{} days agree, {}/{} adjacent pairs",
        agreement.days_agreeing,
        agreement.days_checked,
        agreement.adjacent_agreeing,
        agreement.adjacent_pairs
    );

    let dir = generate_variant(
        "multipart",
        "2026a-shuffled",
        &["--multipart-profile", "2026a", "--zip-order", "shuffled"],
    );
    let run = run_dir(&dir);
    // Exact matching does not depend on order, so every item still matches...
    assert_eq!(run.report.counts.exact, truth.counts.items);
    let (correct, total) = time_accuracy(&run, &truth);
    assert_eq!(correct, total);
    // ...and the measurement says the ordinal assumption would have failed.
    let agreement = order_agreement(&run.report);
    assert_eq!(agreement.days_checked, truth.counts.multi_item_groups);
    assert!(agreement.days_agreeing < agreement.days_checked);
    assert_eq!(
        agreement.verdict(run.report.order.violations),
        OrderVerdict::Fails
    );
    println!(
        "order-check (shuffled): {}/{} days agree, {}/{} adjacent pairs, {} cross-day violations",
        agreement.days_agreeing,
        agreement.days_checked,
        agreement.adjacent_agreeing,
        agreement.adjacent_pairs,
        run.report.order.violations
    );
}
