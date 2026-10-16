//! M3 engine tests: the real compositor (`MediaCompositor`, mr-composite)
//! plugged into the job engine.
//!
//! * F-07 `rotation`: every item burned in, upright, originals kept; the
//!   report names `overlay_aspect_mismatch` on exactly the mismatched items.
//! * F-08 `clips`: the dry run finds the section 4.5 groups (default years
//!   and all years; unticking, FR-27 AC3); a run joins them with the first
//!   clip's metadata, keeps the clips, and a second run does nothing.
//! * F-09 `codecs`: "Maximise Windows compatibility" turns HEVC into H.264;
//!   the FR-51 notice appears in the plan when HEVC cannot be played.
//!
//! Needs the M3 fixtures (ffmpeg with libx264 at generation) and, for runs,
//! an ffmpeg (the staged sidecar or one on PATH); skips cleanly without.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Once, OnceLock};

use common::fixtures::run_generator;
use common::{env, options, repo_root, zips_in};
use mr_job::{Job, MediaCompositor, Options, Outcome, Plan, Stage, plan};
use serde_json::Value;

static GENERATE_M3: Once = Once::new();

/// `fixtures/<name>` and its truth, or `None` when it was generated without
/// media (no ffmpeg with libx264 at generation; the CI runners).
fn m3_fixture(name: &str) -> Option<(PathBuf, Value)> {
    GENERATE_M3.call_once(|| {
        let root = repo_root();
        if ["rotation", "clips", "codecs"]
            .iter()
            .any(|n| !root.join("fixtures").join(n).join("truth.json").is_file())
        {
            run_generator(
                &root,
                &[
                    "--fixture",
                    "rotation",
                    "--fixture",
                    "clips",
                    "--fixture",
                    "codecs",
                    "--quiet",
                ],
            );
        }
    });
    let dir = repo_root().join("fixtures").join(name);
    let truth: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("truth.json")).unwrap()).unwrap();
    if truth.get("skipped").and_then(Value::as_str).is_some()
        || truth
            .get("videos_skipped")
            .and_then(Value::as_str)
            .is_some()
    {
        eprintln!("skip: fixture {name} was generated without its videos");
        return None;
    }
    Some((dir, truth))
}

fn runs(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
        && Command::new(path)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// The staged LGPL sidecar, else an ffmpeg on PATH.
fn ffmpeg() -> Option<PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let exe = if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            };
            let bundled = repo_root().join("third_party").join("ffmpeg").join(exe);
            if runs(&bundled) {
                return Some(bundled);
            }
            std::env::split_paths(&std::env::var_os("PATH")?)
                .map(|d| d.join(exe))
                .find(|p| runs(p))
        })
        .clone()
}

macro_rules! need_ffmpeg {
    () => {
        match ffmpeg() {
            Some(f) => f,
            None => {
                eprintln!("skip: no ffmpeg");
                return;
            }
        }
    };
}

fn run(job: &mut Job) -> (Outcome, mr_job::CollectingSink) {
    let sink = mr_job::CollectingSink::default();
    let outcome = job.run(&sink, &mr_job::Control::new()).expect("run");
    (outcome, sink)
}

/// Plan item key -> filename ID (the truth's `id`), upper-cased.
fn ids_by_key(p: &Plan) -> HashMap<String, String> {
    p.items
        .iter()
        .map(|i| (i.key.clone(), i.file_id.to_ascii_uppercase()))
        .collect()
}

fn truth_groups(v: &Value) -> Vec<Vec<String>> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|g| {
            g.as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_ascii_uppercase())
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// F-07: burn-in through the seam, warnings in the report
// ---------------------------------------------------------------------------

#[test]
fn f07_burn_in_through_the_media_compositor() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = m3_fixture("rotation") else {
        return;
    };
    let e = env("m3-f07");
    // With the real compositor the dry run does not promise pending overlays.
    let media = MediaCompositor::new(&ffmpeg);
    let warned = |p: &Plan| p.warnings.iter().any(|w| w.code == "COMPOSITE_PENDING");
    assert!(warned(&plan(&zips_in(&dir), &options(&e.dest)).unwrap()));
    assert!(!warned(
        &mr_job::plan_for(&zips_in(&dir), &options(&e.dest), &media).unwrap()
    ));
    let mut job = Job::create(&e.state, &zips_in(&dir), options(&e.dest)).unwrap();
    job.set_compositor(Arc::new(MediaCompositor::new(&ffmpeg)));
    let (outcome, _) = run(&mut job);
    assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");

    let by_id: HashMap<String, &Value> = truth["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["id"].as_str().unwrap().to_ascii_uppercase(), i))
        .collect();
    let report = job.report_data().unwrap();
    let messages: HashMap<String, Option<String>> = report
        .rows
        .iter()
        .filter_map(|r| r.item.clone().map(|k| (k, r.message.clone())))
        .collect();
    for row in job.items().unwrap() {
        let item = by_id[&row.plan.file_id.to_ascii_uppercase()];
        let case = item["case"].as_str().unwrap();
        assert_eq!(
            row.stage,
            Stage::Done,
            "{case}: {:?}",
            row.last_error_detail
        );
        assert!(!row.composite_pending, "{case}: burned in");
        assert!(
            row.originals_main.is_some() && row.originals_overlay.is_some(),
            "{case}: FR-25"
        );
        let out = job.output_root().join(row.output_path.as_ref().unwrap());
        let info = mr_composite::probe(&out).unwrap();
        let want = (
            item["display_size"][0].as_u64().unwrap() as u32,
            item["display_size"][1].as_u64().unwrap() as u32,
        );
        assert_eq!(info.display_size(), Some(want), "{case}");
        if let Some(v) = &info.video {
            assert_eq!((v.codec.as_str(), v.rotation), ("avc1", 0), "{case}");
        }
        // The report says so on exactly the items whose overlay did not fit.
        let mismatch = item["placement"]["aspect_mismatch"].as_bool().unwrap();
        let said = messages[&row.plan.key]
            .as_deref()
            .is_some_and(|m| m.contains("fitted to the width"));
        assert_eq!(said, mismatch, "{case}: {:?}", messages[&row.plan.key]);
    }
}

// ---------------------------------------------------------------------------
// F-08: clip groups in the plan, joins in the run
// ---------------------------------------------------------------------------

fn clip_options(dest: &Path) -> Options {
    let mut o = options(dest);
    o.clip_rejoin.enabled = true;
    o
}

#[test]
fn f08_plan_finds_the_clip_groups() {
    let Some((dir, truth)) = m3_fixture("clips") else {
        return;
    };
    let e = env("m3-f08-plan");
    let inputs = zips_in(&dir);

    // Off by default: no probing, no groups.
    let off = plan(&inputs, &options(&e.dest)).unwrap();
    assert!(off.clip_groups.is_empty());

    let t = std::time::Instant::now();
    let p = plan(&inputs, &clip_options(&e.dest)).unwrap();
    eprintln!(
        "plan with clip detection: {} items, {} groups in {} ms",
        p.items.len(),
        p.clip_groups.len(),
        t.elapsed().as_millis()
    );
    let ids = ids_by_key(&p);
    let found: Vec<Vec<String>> = p
        .clip_groups
        .iter()
        .map(|g| g.items.iter().map(|k| ids[k].clone()).collect())
        .collect();
    assert_eq!(found, truth_groups(&truth["expected_groups"]["default"]));
    for g in &p.clip_groups {
        assert!(g.enabled);
        assert_eq!(g.id, g.items[0]);
        assert!(g.output_rel.ends_with("-joined.mp4"), "{}", g.output_rel);
        let first = p.items.iter().find(|i| i.key == g.id).unwrap();
        assert_eq!(
            g.output_rel,
            first.output_rel.replace(".mp4", "-joined.mp4"),
            "named after the first clip"
        );
    }
    let ac1 = &p.clip_groups[found
        .iter()
        .position(|g| *g == truth_groups(&Value::Array(vec![truth["cases"]["ac1"].clone()]))[0])
        .unwrap()];
    assert!((ac1.total_secs - 24.0).abs() <= 0.1, "{}", ac1.total_secs);

    // All years (the 2023 group and the one across New Year 2022 join too).
    let mut o = clip_options(&e.dest);
    o.clip_rejoin.year_range = None;
    let all = plan(&inputs, &o).unwrap();
    let ids = ids_by_key(&all);
    let found_all: Vec<Vec<String>> = all
        .clip_groups
        .iter()
        .map(|g| g.items.iter().map(|k| ids[k].clone()).collect())
        .collect();
    assert_eq!(
        found_all,
        truth_groups(&truth["expected_groups"]["all_years"])
    );

    // FR-27 AC3: an unticked group stays in the plan, disabled.
    let mut o = clip_options(&e.dest);
    o.clip_rejoin.skip_groups = vec![ac1.id.clone()];
    let unticked = plan(&inputs, &o).unwrap();
    let g = unticked
        .clip_groups
        .iter()
        .find(|g| g.id == ac1.id)
        .unwrap();
    assert!(!g.enabled);
    assert_eq!(
        unticked.clip_groups.iter().filter(|g| g.enabled).count(),
        p.clip_groups.len() - 1
    );
}

#[test]
fn f08_run_joins_the_groups_with_the_first_clips_metadata() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = m3_fixture("clips") else {
        return;
    };
    let e = env("m3-f08-run");
    let mut o = clip_options(&e.dest);
    // Untick one group to see that it is left alone (FR-27 AC3).
    let p = plan(&zips_in(&dir), &o).unwrap();
    let ids = ids_by_key(&p);
    let skipped = p
        .clip_groups
        .iter()
        .find(|g| {
            ids[&g.id]
                == truth["cases"]["year_2023"][0]
                    .as_str()
                    .unwrap()
                    .to_ascii_uppercase()
        })
        .map(|g| g.id.clone());
    assert!(skipped.is_none(), "2023 is outside the default years");
    let b_group = p
        .clip_groups
        .iter()
        .find(|g| {
            g.items.len() == 2
                && ids[&g.items[0]]
                    != truth["cases"]["ac1"][0]
                        .as_str()
                        .unwrap()
                        .to_ascii_uppercase()
        })
        .unwrap()
        .id
        .clone();
    o.clip_rejoin.skip_groups = vec![b_group.clone()];

    let mut job = Job::create(&e.state, &zips_in(&dir), o).unwrap();
    job.set_compositor(Arc::new(MediaCompositor::new(&ffmpeg)));
    let (outcome, sink) = run(&mut job);
    assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");

    let items: HashMap<String, mr_job::ItemRow> = job
        .items()
        .unwrap()
        .into_iter()
        .map(|r| (r.plan.key.clone(), r))
        .collect();
    assert!(
        items.values().all(|r| r.stage == Stage::Done),
        "every clip is written as an item"
    );
    let groups = job.clip_groups().unwrap();
    assert_eq!(groups.len(), p.clip_groups.len());
    let mut joined = 0;
    for g in &groups {
        if g.group.id == b_group {
            assert_eq!(g.stage, "skipped");
            assert!(g.output_path.is_none());
            continue;
        }
        assert_eq!(
            g.stage, "done",
            "{:?}: {:?} {:?}",
            g.group.items, g.error_code, g.error_detail
        );
        joined += 1;
        let out = job.output_root().join(g.output_path.as_ref().unwrap());
        let info = mr_composite::probe(&out).unwrap();
        let sum: f64 = g
            .group
            .items
            .iter()
            .map(|k| {
                let r = &items[k];
                mr_composite::probe(&job.output_root().join(r.output_path.as_ref().unwrap()))
                    .unwrap()
                    .duration_secs
            })
            .sum();
        assert!(
            (info.duration_secs - sum).abs() <= 0.1,
            "{}: {} s vs clips {} s",
            g.group.id,
            info.duration_secs,
            sum
        );
        // Section 4.5: the joined file carries the first clip's metadata.
        let first = &items[&g.group.items[0]];
        let summary = mr_meta::native_mp4::summarize(&out).unwrap();
        assert_eq!(
            summary.mvhd_creation,
            Some(first.plan.capture().qt_seconds())
        );
        let keys: BTreeMap<String, String> = summary.keys.into_iter().collect();
        assert_eq!(
            keys.get("com.apple.quicktime.creationdate"),
            Some(&first.plan.capture().iso8601_local())
        );
        // A rotated group joined by stream copy keeps its rotation; any
        // other output is upright.
        let v = info.video.unwrap();
        assert_eq!(v.display_size(), (288, 512), "{}", g.group.id);
    }
    assert!(joined >= 5, "{joined}");
    let clip_events = sink
        .items
        .lock()
        .unwrap()
        .iter()
        .filter(|o| o.id.starts_with("clip:"))
        .count();
    assert_eq!(clip_events, joined, "one ItemOutcome per joined group");
    let report = job.report_data().unwrap();
    assert_eq!(
        report
            .rows
            .iter()
            .filter(|r| r.kind == Some(mr_report::RowKind::JoinedClip))
            .count(),
        joined
    );

    // A second run finds nothing to do and leaves the joined files alone.
    let before: Vec<Option<String>> = groups.iter().map(|g| g.output_sha256.clone()).collect();
    let (again, sink2) = run(&mut job);
    assert!(matches!(again, Outcome::Completed(_)), "{again:?}");
    assert!(sink2.items.lock().unwrap().is_empty());
    let after: Vec<Option<String>> = job
        .clip_groups()
        .unwrap()
        .iter()
        .map(|g| g.output_sha256.clone())
        .collect();
    assert_eq!(before, after);
}

// ---------------------------------------------------------------------------
// F-09: Windows compatibility mode and the FR-51 notice
// ---------------------------------------------------------------------------

#[test]
fn f09_windows_compat_turns_hevc_into_h264() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = m3_fixture("codecs") else {
        return;
    };
    let hevc_cases: Vec<String> = truth["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| matches!(i["expected"]["codec"].as_str(), Some("hvc1" | "hev1")))
        .map(|i| i["id"].as_str().unwrap().to_ascii_uppercase())
        .collect();
    if hevc_cases.is_empty() {
        eprintln!("skip: F-09 has no HEVC cases (no libx265 at generation)");
        return;
    }
    for compat in [false, true] {
        let e = env(if compat {
            "m3-f09-compat"
        } else {
            "m3-f09-plain"
        });
        let mut o = options(&e.dest);
        o.windows_compat = compat;
        let mut job = Job::create(&e.state, &zips_in(&dir), o).unwrap();
        job.set_compositor(Arc::new(MediaCompositor::new(&ffmpeg)));
        let (outcome, _) = run(&mut job);
        assert!(matches!(outcome, Outcome::Completed(_)), "{outcome:?}");
        let report = job.report_data().unwrap();
        for row in job.items().unwrap() {
            let out = job.output_root().join(row.output_path.as_ref().unwrap());
            let v = mr_composite::probe(&out).unwrap().video.unwrap();
            let is_hevc_source = hevc_cases.contains(&row.plan.file_id.to_ascii_uppercase());
            let message = report
                .rows
                .iter()
                .find(|r| r.item.as_deref() == Some(row.plan.key.as_str()))
                .and_then(|r| r.message.clone());
            if compat && is_hevc_source {
                assert_eq!(
                    (v.codec.as_str(), v.rotation),
                    ("avc1", 0),
                    "{}",
                    row.plan.key
                );
                assert!(
                    message.as_deref().is_some_and(|m| m.contains("H.264")),
                    "{message:?}"
                );
            } else if is_hevc_source {
                assert!(
                    matches!(v.codec.as_str(), "hvc1" | "hev1"),
                    "untouched without the option"
                );
            } else {
                assert_eq!(v.codec, "avc1", "H.264 sources are never re-encoded");
                assert!(message.is_none(), "{message:?}");
            }
        }
    }
}

#[test]
fn fr51_hevc_notice_in_the_plan() {
    let Some((dir, _)) = m3_fixture("codecs") else {
        return;
    };
    let e = env("m3-fr51");
    let inputs = zips_in(&dir);
    let has = |p: &Plan| p.warnings.iter().any(|w| w.code == "HEVC_PLAYBACK");

    let mut o = options(&e.dest);
    o.test_hooks.force_hevc_unplayable = true;
    let p = plan(&inputs, &o).unwrap();
    let w = p.warnings.iter().find(|w| w.code == "HEVC_PLAYBACK");
    assert!(w.is_some(), "{:?}", p.warnings);
    eprintln!("{}", w.unwrap().detail);

    o.windows_compat = true;
    assert!(
        !has(&plan(&inputs, &o).unwrap()),
        "no notice once the option is on"
    );

    // This machine's own answer.
    let playable = mr_job::media::hevc_playback_available();
    assert_eq!(has(&plan(&inputs, &options(&e.dest)).unwrap()), !playable);
}
