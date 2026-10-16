//! FR-13 format detection and messaging over F-01, F-02, F-03 and F-15:
//! AC1 (the legacy message with the steps to request a new export) and AC2
//! (the four classifications, and the ZIP-order note for `2026b`), plus F-03
//! `mixed-format` matched entry by entry against its truth.json.

mod common;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use common::{fixture_dir, zips_in};
use mr_core::discover::discover;
use mr_core::fields::DatePrecision;
use mr_core::index::{ExportSummary, Profile, build_index, load_json, summarize};
use mr_core::issue::Code;
use mr_core::matcher::{Confidence, MatchOptions, MatchReport, match_export};
use serde_json::Value;

struct Run {
    summary: ExportSummary,
    report: MatchReport,
}

fn run_inputs(inputs: &[PathBuf]) -> Run {
    let d = discover(inputs);
    assert_eq!(d.exports.len(), 1, "{:?}", d.exports);
    let export = &d.exports[0];
    let index = build_index(&d, export);
    let json = load_json(&d, &index);
    let summary = summarize(&d, export, &index, json.as_ref());
    let entries = json.as_ref().map(|j| j.entries.as_slice()).unwrap_or(&[]);
    let report = match_export(entries, &index, &MatchOptions::default());
    Run { summary, report }
}

fn truth(dir: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(dir.join("truth.json")).unwrap()).unwrap()
}

fn count(t: &Value, key: &str) -> usize {
    t["counts"][key]
        .as_u64()
        .unwrap_or_else(|| panic!("counts.{key}")) as usize
}

#[test]
fn fr13_ac2_each_fixture_gets_its_profile() {
    for (name, want) in [
        ("small-2026a", Profile::P2026a),
        ("small-2026b", Profile::P2026b),
        ("small-legacy", Profile::Legacy),
        ("mixed-format", Profile::Mixed),
    ] {
        let dir = fixture_dir(name);
        let run = run_inputs(&zips_in(&dir));
        assert_eq!(run.summary.profile, want, "{name}");
        assert_eq!(
            run.summary.profile.as_str(),
            truth(&dir)["format"].as_str().unwrap(),
            "{name}: truth.json format"
        );
    }
}

#[test]
fn fr13_ac1_legacy_message_has_the_request_steps() {
    let run = run_inputs(&zips_in(&fixture_dir("small-legacy")));
    let issue = run
        .summary
        .issues
        .iter()
        .find(|i| i.code == Code::LegacyLinksOnly)
        .expect("LEGACY_LINKS_ONLY");
    for needle in [
        "This export contains links rather than media. These links expire and Memories Rescue v1 does not download them. Please request a new export",
        "accounts.snapchat.com",
        "My Data",
        "Export your Memories",
        "Export JSON Files",
        "All Time",
        "up to 7 days",
    ] {
        assert!(
            issue.detail.contains(needle),
            "missing {needle:?} in {:?}",
            issue.detail
        );
    }
    assert!(
        !issue.detail.contains("https://") && !issue.detail.contains("http://"),
        "Snap's address is text, not a link"
    );
    assert_eq!(
        run.summary.profile_message().as_deref(),
        Some(issue.detail.as_str())
    );
}

#[test]
fn fr13_ac2_ordinal_exports_say_that_times_depend_on_zip_order() {
    let note = |name: &str| {
        run_inputs(&zips_in(&fixture_dir(name)))
            .summary
            .profile_message()
    };
    let b = note("small-2026b").expect("2026b note");
    assert!(b.contains("order of files inside the ZIP"), "{b}");
    assert!(
        b.contains("extracted folder") && b.contains("date only"),
        "{b}"
    );
    assert!(note("mixed-format").is_some(), "mixed has ordinal entries");
    assert_eq!(
        note("small-2026a"),
        None,
        "exact on sid/mid: nothing rests on order"
    );

    // An extracted folder has no entry order: the note gives way to the
    // MATCH_PARTIAL issue that says so.
    let folder = std::env::temp_dir().join(format!("mr-core-fr13-{}", std::process::id()));
    let _ = fs::remove_dir_all(&folder);
    fs::create_dir_all(&folder).unwrap();
    for zip in zips_in(&fixture_dir("small-2026b")) {
        zip::ZipArchive::new(fs::File::open(zip).unwrap())
            .unwrap()
            .extract(&folder)
            .unwrap();
    }
    let run = run_inputs(std::slice::from_ref(&folder));
    assert_eq!(run.summary.profile, Profile::P2026b);
    assert_eq!(run.summary.profile_message(), None);
    assert!(
        run.summary
            .issues
            .iter()
            .any(|i| i.code == Code::MatchPartial && i.detail.contains("Folder input"))
    );
    let _ = fs::remove_dir_all(&folder);
}

#[test]
fn f03_mixed_format_matches_every_entry_on_its_own_path() {
    let dir = fixture_dir("mixed-format");
    let t = truth(&dir);
    assert_eq!(
        t["generator"]["zip_order"], "json",
        "expected values assume JSON order"
    );
    let run = run_inputs(&zips_in(&dir));
    let s = &run.summary;

    // FR-13: the summary counts entries per matching path.
    assert_eq!(s.json_entries, count(&t, "json_entries"));
    assert_eq!(s.profile_counts.exact, count(&t, "path_exact"));
    assert_eq!(s.profile_counts.ordinal, count(&t, "path_ordinal"));
    assert_eq!(s.profile_counts.metadata_only, 0, "media is bundled");
    assert_eq!(s.main_files, count(&t, "main_files"));
    assert_eq!(s.overlays, count(&t, "overlays"));
    assert!(run.report.order.is_monotone(), "{:?}", run.report.order);

    let items: Vec<&Value> = t["items"].as_array().unwrap().iter().collect();
    let by_main: HashMap<&str, &Value> = items
        .iter()
        .filter_map(|v| v["main"].as_str().map(|m| (m, *v)))
        .collect();
    assert_eq!(run.report.items.len(), by_main.len());
    let (mut exact, mut ordinal) = (0, 0);
    for item in &run.report.items {
        let want = by_main
            .get(item.main.name.as_str())
            .unwrap_or_else(|| panic!("unexpected item {}", item.main.name));
        let expected = want["expected_match"].as_str().unwrap();
        match item.confidence {
            Confidence::Exact => exact += 1,
            Confidence::Ordinal => ordinal += 1,
            other => panic!("{}: {other:?}", item.main.name),
        }
        assert_eq!(
            match item.confidence {
                Confidence::Exact => "exact",
                _ => "ordinal",
            },
            expected,
            "{} built as {}",
            item.main.name,
            want["built_as"]
        );
        assert_eq!(
            item.json_index,
            Some(want["json_index"].as_u64().unwrap() as usize),
            "{}",
            item.main.name
        );
        let at: DateTime<Utc> = DateTime::parse_from_rfc3339(want["date_utc"].as_str().unwrap())
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(item.date.utc, at, "{}", item.main.name);
        assert_eq!(
            item.date.precision,
            DatePrecision::Second,
            "{}",
            item.main.name
        );
        assert_eq!(
            item.overlay.as_ref().map(|o| o.name.as_str()),
            want["overlay"].as_str(),
            "{}",
            item.main.name
        );
    }
    assert_eq!(exact, count(&t, "expected_exact"));
    assert_eq!(ordinal, count(&t, "expected_ordinal"));

    // The same-day ordinal pair is resolved by position, not left at day
    // precision.
    let pair: Vec<&&Value> = items.iter().filter(|v| v["ordinal_pair"] == true).collect();
    assert_eq!(pair.len(), 2);

    let missing: BTreeSet<usize> = run.report.missing_media().map(|u| u.json_index).collect();
    let want_missing: BTreeSet<usize> = items
        .iter()
        .filter(|v| v["expected_match"] == "missing_media")
        .map(|v| v["json_index"].as_u64().unwrap() as usize)
        .collect();
    assert_eq!(missing, want_missing);
    assert_eq!(missing.len(), count(&t, "expected_missing_media"));
    assert!(run.report.orphan_overlays.is_empty());
}
