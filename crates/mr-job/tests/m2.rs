//! M2 acceptance tests for the engine, over the synthetic fixtures.
//!
//! The kill-and-resume test (FR-33 AC1) lives in `crates/mr-cli/tests`
//! because it needs a process to kill.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use chrono::DateTime;
use common::*;
use little_exif::exif_tag::ExifTag;
use mr_job::{
    CollectingSink, CompositeError, CompositeOutcome, CompositeRequest, Compositor, Control,
    ItemOutcome, Job, OverlayMode, Progress, ProgressSink, SpaceAction, Stage, plan,
};
use mr_meta::native_mp4;
use mr_report::{RowKind, Status};

/// Items in F-01 on this machine: 50 with ffmpeg, 30 without (the CI runners).
fn f01_items() -> usize {
    truth("small-2026b")["counts"]["main_files"]
        .as_u64()
        .unwrap() as usize
}

fn done_items(job: &Job) -> Vec<mr_job::ItemRow> {
    job.items()
        .unwrap()
        .into_iter()
        .filter(|i| i.stage == Stage::Done)
        .collect()
}

fn exif_of(path: &Path) -> HashMap<String, String> {
    let bytes = fs::read(path).unwrap();
    let md = if path.extension().is_some_and(|e| e == "png") {
        mr_meta::native_png::read_png(path).unwrap()
    } else {
        little_exif::metadata::Metadata::new_from_vec(
            &bytes,
            little_exif::filetype::FileExtension::JPEG,
        )
        .unwrap()
    };
    let mut out = HashMap::new();
    for tag in &md {
        let (k, v) = match tag {
            ExifTag::DateTimeOriginal(s) => ("DateTimeOriginal", s.clone()),
            ExifTag::CreateDate(s) => ("CreateDate", s.clone()),
            ExifTag::ModifyDate(s) => ("ModifyDate", s.clone()),
            ExifTag::OffsetTimeOriginal(s) => ("OffsetTimeOriginal", s.clone()),
            ExifTag::OffsetTimeDigitized(s) => ("OffsetTimeDigitized", s.clone()),
            ExifTag::OffsetTime(s) => ("OffsetTime", s.clone()),
            ExifTag::GPSLatitudeRef(s) => ("GPSLatitudeRef", s.clone()),
            ExifTag::GPSDateStamp(s) => ("GPSDateStamp", s.clone()),
            _ => continue,
        };
        out.insert(k.to_string(), v.trim_end_matches('\0').to_string());
    }
    out
}

fn system_time(rfc3339: &str) -> SystemTime {
    let t = DateTime::parse_from_rfc3339(rfc3339).unwrap().timestamp();
    UNIX_EPOCH + Duration::from_secs(t as u64)
}

/// exiftool, when it is around: `MR_EXIFTOOL`, `third_party/`, or `PATH`.
fn exiftool() -> Option<mr_meta::exiftool::ExifTool> {
    let path = std::env::var_os("MR_EXIFTOOL")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| mr_meta::exiftool::locate(&repo_root()))?;
    mr_meta::exiftool::ExifTool::spawn(&path).ok()
}

// ---------------------------------------------------------------------------
// FR-15 to FR-20 and FR-30 on F-11 tz-edge (photos) and F-12 (videos)
// ---------------------------------------------------------------------------

#[test]
fn fr15_to_fr20_photo_fields_offsets_folders_and_file_times() {
    let e = env("tz");
    let t = truth("tz-edge");
    let mut o = options(&e.dest);
    o.home_tz = Some(t["home_zone"].as_str().unwrap().to_string());
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("tz-edge")), o).unwrap();
    let (outcome, _) = run(&mut job);
    assert!(
        matches!(outcome, mr_job::Outcome::Completed(_)),
        "{outcome:?}"
    );
    let by_key: HashMap<String, mr_job::ItemRow> = job
        .items()
        .unwrap()
        .into_iter()
        .map(|i| (i.plan.file_id.to_ascii_uppercase(), i))
        .collect();
    let mut oracle = exiftool();
    let mut checked = 0;
    for it in t["items"].as_array().unwrap() {
        let case = it["case"].as_str().unwrap();
        let exp = &it["expected"];
        let row = &by_key[&it["id"].as_str().unwrap().to_ascii_uppercase()];
        assert_eq!(row.stage, Stage::Done, "{case}");
        let rel = row.output_path.as_deref().unwrap();
        let path = job.output_root().join(rel);
        if exp["method"] == "gps_unknown_or_home" {
            continue;
        }
        // FR-19: method and offset; FR-30: folder of the local date.
        let want_method = exp["method"].as_str().unwrap();
        assert_eq!(row.plan.tz_method, want_method, "{case}: method");
        let local = exp["local"]
            .as_str()
            .unwrap()
            .replace('-', ":")
            .replacen('T', " ", 1);
        let local = format!(
            "{}:{}:{} {}",
            &local[0..4],
            &local[5..7],
            &local[8..10],
            &local[11..]
        );
        let offset = exp["offset"].as_str().unwrap();
        assert!(
            rel.starts_with(&format!("{}/", exp["local_folder"].as_str().unwrap())),
            "{case}: {rel} not in {}",
            exp["local_folder"]
        );
        // FR-15: EXIF date fields and offsets (incl. the Q-02 additions).
        let exif = exif_of(&path);
        for tag in ["DateTimeOriginal", "CreateDate", "ModifyDate"] {
            assert_eq!(exif.get(tag), Some(&local), "{case}: {tag}");
        }
        for tag in ["OffsetTimeOriginal", "OffsetTimeDigitized", "OffsetTime"] {
            assert_eq!(
                exif.get(tag).map(String::as_str),
                Some(offset),
                "{case}: {tag}"
            );
        }
        // FR-16: GPS when a location exists.
        assert_eq!(
            exif.contains_key("GPSLatitudeRef"),
            !it["location"].is_null(),
            "{case}: GPS presence"
        );
        // FR-20: file times equal the capture instant.
        let meta = fs::metadata(&path).unwrap();
        let want = system_time(it["date_utc"].as_str().unwrap());
        assert_eq!(meta.modified().unwrap(), want, "{case}: mtime");
        if cfg!(any(windows, target_os = "macos")) {
            assert_eq!(meta.created().unwrap(), want, "{case}: creation time");
        }
        if let Some(et) = oracle.as_mut() {
            let j = et.read_json(&path, &[]).unwrap();
            assert_eq!(
                j["ExifIFD:DateTimeOriginal"],
                local.as_str(),
                "{case}: exiftool"
            );
            assert_eq!(j["ExifIFD:OffsetTimeOriginal"], offset, "{case}: exiftool");
        }
        checked += 1;
    }
    assert!(checked >= 20);
    println!(
        "FR-15/16/19/20/30: {checked} tz-edge photos verified{}",
        if oracle.is_some() {
            " (and by exiftool)"
        } else {
            "; exiftool not found, oracle skipped"
        }
    );
}

#[test]
fn fr17_fr18_video_times_keys_and_unicode_long_destination() {
    // F-12: a destination with non-ASCII characters, deep enough to pass 260 characters.
    let base = scratch("unicode");
    let mut dest = base.join("Fotos \u{2013} Caf\u{e9} \u{2615} \u{5199}\u{771f}");
    while dest.to_string_lossy().len() < 230 {
        dest = dest.join("tr\u{e8}s-long-dossier-de-sauvegarde");
    }
    fs::create_dir_all(&dest).unwrap();
    let state = base.join("state");
    let mut job = Job::create(
        &state,
        &zips_in(&fixture_dir("unicode-paths")),
        options(&dest),
    )
    .unwrap();
    let (outcome, sink) = run(&mut job);
    assert!(
        matches!(outcome, mr_job::Outcome::Completed(_)),
        "{outcome:?} {:?}",
        sink.items
            .lock()
            .unwrap()
            .iter()
            .filter(|i| i.error_code.is_some())
            .collect::<Vec<_>>()
    );
    let mut longest = 0;
    let mut videos = 0;
    for row in done_items(&job) {
        let rel = row.output_path.clone().unwrap();
        let path = mr_job::layout::to_fs(job.output_root(), &rel);
        longest = longest.max(job.output_root().join(&rel).to_string_lossy().len());
        assert!(path.is_file(), "{}", path.display());
        if row.plan.is_video() {
            let cap = row.plan.capture();
            let s = native_mp4::summarize(&path).unwrap();
            assert_eq!(s.mvhd_creation, Some(cap.qt_seconds()), "FR-17 mvhd");
            assert!(
                s.tkhd_creations.iter().all(|t| *t == cap.qt_seconds()),
                "FR-17 tkhd"
            );
            assert!(
                s.mdhd_creations.iter().all(|t| *t == cap.qt_seconds()),
                "FR-17 mdhd"
            );
            let keys: HashMap<_, _> = s.keys.iter().cloned().collect();
            assert_eq!(
                keys.get("com.apple.quicktime.creationdate"),
                Some(&cap.iso8601_local()),
                "FR-17 creationdate"
            );
            assert_eq!(
                keys.get("com.apple.quicktime.location.ISO6709").cloned(),
                cap.iso6709(),
                "FR-18 ISO 6709"
            );
            assert_eq!(s.xyz, cap.iso6709(), "FR-18 xyz");
            if let Some(iso) = cap.iso6709() {
                assert!(
                    iso.len() == 18 && iso.ends_with('/'),
                    "four decimals: {iso}"
                );
            }
            videos += 1;
        }
    }
    assert!(videos >= 3, "{videos} videos");
    assert!(longest > 260, "deep path exercised ({longest} characters)");
    // NFR-I18N-04: the job is found from an NFD spelling of the same folder.
    let nfd: String =
        unicode_normalization::UnicodeNormalization::nfd(dest.to_string_lossy().as_ref()).collect();
    assert_eq!(
        mr_job::find_job_for_output(&state, Path::new(&nfd)).as_deref(),
        Some(job.id())
    );
    println!("F-12: {videos} videos verified; longest output path {longest} characters");
}

// ---------------------------------------------------------------------------
// F-04: the big run: FR-30, FR-38 AC1, NFR-PERF-02, NFR-PERF-06, NFR-REL-01, NFR-PRV-09
// ---------------------------------------------------------------------------

struct FirstItem {
    started: Instant,
    first: Mutex<Option<Duration>>,
    inner: CollectingSink,
}

impl ProgressSink for FirstItem {
    fn progress(&self, p: &Progress) {
        self.inner.progress(p);
    }
    fn item(&self, o: &ItemOutcome) {
        let mut f = self.first.lock().unwrap();
        if f.is_none() {
            *f = Some(self.started.elapsed());
        }
        self.inner.item(o);
    }
    fn issue(&self, c: &str, d: &str) {
        self.inner.issue(c, d);
    }
}

fn rss() -> Option<usize> {
    memory_stats::memory_stats().map(|m| m.physical_mem)
}

#[test]
fn f04_full_run_layout_report_memory_and_resume_start() {
    let e = env("f04");
    let dir = fixture_dir("multipart");
    let t = truth("multipart");
    let inputs = zips_in(&dir);
    let before: Vec<String> = inputs.iter().map(|p| sha256(p)).collect();
    let rss_before = rss();

    let mut o = options(&e.dest);
    o.test_hooks.delay_ms = 0;
    let mut job = Job::create(&e.state, &inputs, o).unwrap();
    // Pause after 200 items, then measure how fast a reopened job resumes.
    let control = Control::new();
    let c2 = control.clone();
    let pauser = Arc::new(Mutex::new(0usize));
    struct PauseAfter {
        n: usize,
        count: Arc<Mutex<usize>>,
        control: Control,
    }
    impl ProgressSink for PauseAfter {
        fn progress(&self, _: &Progress) {}
        fn item(&self, _: &ItemOutcome) {
            let mut c = self.count.lock().unwrap();
            *c += 1;
            if *c == self.n {
                self.control.pause();
            }
        }
        fn issue(&self, _: &str, _: &str) {}
    }
    let started = Instant::now();
    let outcome = job
        .run(
            &PauseAfter {
                n: 200,
                count: pauser.clone(),
                control: c2,
            },
            &control,
        )
        .unwrap();
    assert!(matches!(outcome, mr_job::Outcome::Paused(_)), "{outcome:?}");
    let first_leg = started.elapsed();
    let id = job.id().to_string();
    drop(job);

    // NFR-PERF-06: time from opening the saved job to the first finished item.
    let t0 = Instant::now();
    let mut job = Job::open(&e.state, &id).unwrap();
    let sink = FirstItem {
        started: t0,
        first: Mutex::new(None),
        inner: CollectingSink::default(),
    };
    let peak = Arc::new(Mutex::new(rss().unwrap_or(0)));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let peak = peak.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                if let Some(r) = rss() {
                    let mut p = peak.lock().unwrap();
                    *p = (*p).max(r);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })
    };
    let outcome = job.run(&sink, &Control::new()).unwrap();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    sampler.join().unwrap();
    let resume_first = sink.first.lock().unwrap().unwrap();
    let total = started.elapsed();
    let mr_job::Outcome::Completed(stats) = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(stats.done, 1000);
    assert_eq!(stats.failed, 0);

    // No done item was started again after it finished.
    let mut finished: HashSet<String> = HashSet::new();
    for ev in job.journal().unwrap() {
        match ev.event.as_str() {
            "done" => {
                finished.insert(ev.item);
            }
            "start" => assert!(!finished.contains(&ev.item), "{} processed twice", ev.item),
            _ => {}
        }
    }

    // FR-30: every output sits in the folder of its local capture date; no duplicates.
    let files = files_under(job.output_root());
    let mut outputs = 0;
    for row in done_items(&job) {
        let rel = row.output_path.unwrap();
        let folder = row.plan.local().format("%Y/%m").to_string();
        assert!(rel.starts_with(&format!("{folder}/")), "{rel} vs {folder}");
        assert!(!rel.contains("_2."), "unexpected collision suffix: {rel}");
        let path = job.output_root().join(&rel);
        assert_eq!(sha256(&path), row.output_sha256.unwrap(), "{rel}");
        outputs += 1;
    }
    let overlays = t["counts"]["overlays"].as_u64().unwrap() as usize;
    assert_eq!(outputs, 1000);
    let media_files = files.keys().filter(|k| !k.starts_with("_report")).count();
    assert_eq!(
        media_files,
        1000 + overlays,
        "outputs plus overlays saved beside"
    );

    // NFR-REL-01: inputs untouched. NFR-PRV-09: temp gone.
    let after: Vec<String> = inputs.iter().map(|p| sha256(p)).collect();
    assert_eq!(before, after, "inputs must never be modified");
    assert!(!job.output_root().join(".memories-rescue/tmp").exists());

    // FR-38 AC1 on the complete export.
    let files_r = job.report().unwrap();
    assert!(files_r.html.is_file() && files_r.csv.is_file() && files_r.failed_items.is_file());
    let data = job.report_data().unwrap();
    assert_every_entry_once(
        &data,
        &dir,
        t["counts"]["json_entries"].as_u64().unwrap() as usize,
    );

    let peak = *peak.lock().unwrap();
    println!(
        "F-04: 1000 items in {:.1} s (first leg {:.1} s); resume to first finished item {:.0} ms (NFR-PERF-06 budget 10 s at 50k); peak RSS {:.0} MB (before {:.0} MB; NFR-PERF-02 budget 1.5 GB)",
        total.as_secs_f64(),
        first_leg.as_secs_f64(),
        resume_first.as_secs_f64() * 1000.0,
        peak as f64 / 1e6,
        rss_before.unwrap_or(0) as f64 / 1e6
    );
    assert!(resume_first < Duration::from_secs(10));
    assert!(peak < 1_500_000_000);
}

/// FR-38 AC1: every JSON entry and every listable media entry is in exactly one row.
fn assert_every_entry_once(data: &mr_report::ReportData, fixture: &Path, json_entries: usize) {
    let mut json_seen: BTreeMap<usize, usize> = BTreeMap::new();
    let mut entry_seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    for r in &data.rows {
        if let Some(i) = r.json_index {
            *json_seen.entry(i).or_default() += 1;
        }
        let part = r.source_part.clone().unwrap_or_default();
        if let Some(e) = &r.source_entry {
            *entry_seen.entry((part.clone(), e.clone())).or_default() += 1;
        }
        if let Some(e) = &r.overlay_entry {
            // Overlays can sit in another part; key by name only below.
            *entry_seen.entry(("*".into(), e.clone())).or_default() += 1;
        }
    }
    for i in 0..json_entries {
        assert_eq!(json_seen.get(&i).copied(), Some(1), "JSON entry {i}");
    }
    let d = mr_core::discover::discover(&zips_in(fixture));
    let mut names_once: BTreeMap<String, usize> = BTreeMap::new();
    for ((_, name), n) in &entry_seen {
        *names_once.entry(name.clone()).or_default() += n;
    }
    for part in &d.parts {
        for e in part.entries() {
            if !e.name.starts_with("memories/")
                && !e.name.contains("..")
                && !e.name.starts_with('/')
                && !e.name.contains(':')
            {
                continue;
            }
            if !e.usable && !part.refused().iter().any(|(r, _)| r.name == e.name) {
                continue; // the entry cut by a truncation: its JSON row says missing_media
            }
            assert_eq!(
                names_once.get(&e.name).copied(),
                Some(1),
                "{} in {} appears {:?} times",
                e.name,
                part.label,
                names_once.get(&e.name)
            );
        }
    }
}

#[test]
fn fr38_report_rows_on_missing_truncated_and_zipslip_exports() {
    for (fixture, check) in [
        ("missing-part", "missing"),
        ("truncated", "crc"),
        ("zipslip", "refused"),
    ] {
        let e = env(fixture);
        let dir = fixture_dir(fixture);
        let t = truth(fixture);
        let mut job = Job::create(&e.state, &zips_in(&dir), options(&e.dest)).unwrap();
        let (outcome, _) = run(&mut job);
        assert!(
            matches!(outcome, mr_job::Outcome::Completed(_)),
            "{fixture}: {outcome:?}"
        );
        let data = job.report_data().unwrap();
        assert_every_entry_once(
            &data,
            &dir,
            t["counts"]["json_entries"].as_u64().unwrap() as usize,
        );
        let count = |s: Status| data.rows.iter().filter(|r| r.status() == s).count();
        match check {
            "missing" => {
                assert_eq!(count(Status::MissingMedia), 250, "{fixture}");
                assert_eq!(count(Status::OrphanOverlay), 77, "{fixture}");
                assert_eq!(data.missing_parts, [3]);
                let html = fs::read_to_string(job.report().unwrap().html).unwrap();
                assert!(html.contains("Part 3 appears to be missing."));
            }
            "crc" => {
                // NFR-REL-03: the damaged entry fails alone; everything else is done.
                let failed: Vec<_> = data
                    .rows
                    .iter()
                    .filter(|r| r.status() == Status::Failed)
                    .collect();
                assert_eq!(failed.len(), 1, "{failed:?}");
                assert_eq!(failed[0].error_code.as_deref(), Some("ZIP_ENTRY_CRC"));
                assert_eq!(
                    failed[0].source_entry.as_deref(),
                    t["corrupt_entry"]["name"].as_str()
                );
                let files = job.report().unwrap();
                let fj: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(files.failed_items).unwrap()).unwrap();
                assert_eq!(fj["failed"].as_array().unwrap().len(), 1);
                let finished = count(Status::Done)
                    + count(Status::LocationUncertain)
                    + data
                        .rows
                        .iter()
                        .filter(|r| {
                            r.kind == Some(RowKind::Item) && r.status() == Status::MetadataPartial
                        })
                        .count();
                assert_eq!(
                    finished,
                    t["counts"]["main_files"].as_u64().unwrap() as usize - 1
                );
            }
            "refused" => {
                assert_eq!(count(Status::Refused), 6);
                assert_eq!(count(Status::Done), 2);
                // Nothing outside the output folder was written by refused entries.
                let files = files_under(job.output_root());
                assert!(files.keys().all(|k| !k.contains("..")));
            }
            _ => unreachable!(),
        }
        // Plain language: no absolute paths or links in any report file.
        let files = job.report().unwrap();
        for f in [files.html, files.csv, files.failed_items] {
            let text = fs::read_to_string(&f).unwrap();
            let home = e.dest.to_string_lossy().replace('\\', "/");
            assert!(
                !text.replace('\\', "/").contains(&home),
                "{}: absolute path leaked",
                f.display()
            );
            assert!(!text.contains("http"), "{}: link leaked", f.display());
        }
        let rows_kinds: BTreeSet<_> = data.rows.iter().map(|r| format!("{:?}", r.kind)).collect();
        assert!(rows_kinds.contains(&format!("{:?}", Some(RowKind::Item))));
    }
}

// ---------------------------------------------------------------------------
// FR-05 AC1
// ---------------------------------------------------------------------------

#[test]
fn fr05_zip_and_folder_input_give_identical_outputs_for_exact_and_single_matches() {
    for fixture in ["small-2026a", "small-2026b"] {
        let dir = fixture_dir(fixture);
        let zip_env = env(&format!("{fixture}-zip"));
        let mut a = Job::create(&zip_env.state, &zips_in(&dir), options(&zip_env.dest)).unwrap();
        run(&mut a);
        let folder_env = env(&format!("{fixture}-folder"));
        let folder = folder_env.state.parent().unwrap().join("extracted");
        extract_to_folder(&dir, &folder);
        let mut b = Job::create(
            &folder_env.state,
            std::slice::from_ref(&folder),
            options(&folder_env.dest),
        )
        .unwrap();
        run(&mut b);
        let hashes = |j: &Job| -> HashMap<String, (String, String, String)> {
            j.items()
                .unwrap()
                .into_iter()
                .filter(|i| i.stage == Stage::Done)
                .map(|i| {
                    (
                        i.plan.key.clone(),
                        (
                            i.output_sha256.clone().unwrap(),
                            i.plan.confidence.clone(),
                            i.plan.match_reason.clone(),
                        ),
                    )
                })
                .collect()
        };
        let (ha, hb) = (hashes(&a), hashes(&b));
        let mut compared = 0;
        for (key, (sha_a, conf, reason)) in &ha {
            let single = conf == "ordinal" && reason.contains("only item");
            if conf == "exact" || single {
                let (sha_b, conf_b, _) = &hb[key];
                assert_eq!(conf, conf_b, "{fixture} {key}: confidence");
                assert_eq!(sha_a, sha_b, "{fixture} {key}: output bytes");
                compared += 1;
            }
        }
        assert!(compared >= 20, "{fixture}: {compared} compared");
        println!("FR-05 AC1 {fixture}: {compared} items byte-identical from ZIP and folder");
    }
}

// ---------------------------------------------------------------------------
// FR-28, FR-29
// ---------------------------------------------------------------------------

#[test]
fn fr28_precheck_blocks_with_shortfall_and_options() {
    let e = env("space");
    let mut o = options(&e.dest);
    o.test_hooks.fake_free_bytes = Some(10_000);
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("small-2026b")), o).unwrap();
    let (outcome, sink) = run(&mut job);
    let mr_job::Outcome::InsufficientSpace(est) = outcome else {
        panic!("{outcome:?}")
    };
    assert!(!est.sufficient);
    assert_eq!(est.shortfall_bytes, est.threshold_bytes - 10_000);
    assert_eq!(
        est.threshold_bytes,
        est.required_bytes + est.required_bytes.div_ceil(10)
    );
    let actions: Vec<SpaceAction> = est.options.iter().map(|o| o.action).collect();
    assert!(actions.contains(&SpaceAction::ChangeDestination));
    assert!(
        actions.contains(&SpaceAction::DisableKeepOriginals),
        "{actions:?}"
    );
    assert!(
        sink.issues
            .lock()
            .unwrap()
            .iter()
            .any(|(c, _)| c == "DISK_FULL")
    );
    assert!(files_under(job.output_root()).is_empty(), "nothing written");
    assert_eq!(job.counts().unwrap().get("done"), None);
}

#[test]
fn fr28_recheck_pauses_when_space_runs_low() {
    let e = env("lowspace");
    let mut o = options(&e.dest);
    o.test_hooks.fake_free_bytes = Some(1_000_000_000);
    o.test_hooks.recheck_every = Some(10);
    o.workers = Some(2);
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("small-2026b")), o).unwrap();
    let (outcome, _) = run(&mut job);
    let mr_job::Outcome::Blocked { code, stats, .. } = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(code, "DISK_FULL_IMMINENT");
    assert!((10..=12).contains(&stats.done), "{stats:?}");
    assert_eq!(stats.pending, f01_items() - stats.done);
}

#[test]
fn fr29_dry_run_writes_nothing_but_reports_everything() {
    let base = scratch("dry");
    let dest = base.join("dest");
    fs::create_dir_all(&dest).unwrap();
    let mut o = options(&dest);
    o.unlocked = false;
    let t = truth("multipart");
    let p = plan(&zips_in(&fixture_dir("multipart")), &o).unwrap();
    assert_eq!(
        fs::read_dir(&dest).unwrap().count(),
        0,
        "dry run wrote nothing"
    );
    assert_eq!(p.totals.items, 1000);
    assert_eq!(p.totals.json_entries, 1000);
    assert_eq!(
        p.totals.with_overlay,
        t["counts"]["overlays"].as_u64().unwrap() as usize
    );
    assert_eq!(p.totals.ordinal, 1000);
    assert_eq!(
        p.totals.location_uncertain,
        t["counts"]["expected_location_uncertain"].as_u64().unwrap() as usize
    );
    assert_eq!(p.totals.to_process, 100, "free tier shown");
    assert_eq!(p.totals.skipped_free_limit, 900);
    assert_eq!(p.parts.len(), 4);
    assert!(p.clip_groups.is_empty());
    assert!(p.space.required_bytes > 0);
    let sample = p.sample(10);
    assert_eq!(sample.len(), 10);
    for s in sample {
        assert!(
            s.output_rel
                .starts_with(&s.local().format("%Y/%m/").to_string())
        );
    }
    assert!(p.warnings.iter().any(|w| w.code == "FREE_LIMIT"));
    assert!(p.warnings.iter().any(|w| w.code == "LOCATION_UNCERTAIN"));
}

// ---------------------------------------------------------------------------
// FR-33 AC2, JOB_STATE_CORRUPT, FR-34, FR-39, FR-40, FR-44, DEST_UNAVAILABLE
// ---------------------------------------------------------------------------

#[test]
fn fr33_ac2_changed_input_is_detected_then_reindexed() {
    let e = env("changed");
    let src = zips_in(&fixture_dir("small-2026b"));
    let copy = e.state.parent().unwrap().join("input.zip");
    fs::copy(&src[0], &copy).unwrap();
    let mut job = Job::create(&e.state, std::slice::from_ref(&copy), options(&e.dest)).unwrap();
    filetime_set(&copy, 1_600_000_000);
    let (outcome, sink) = run(&mut job);
    let mr_job::Outcome::InputsChanged(changes) = outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(changes.len(), 1);
    assert!(
        sink.issues
            .lock()
            .unwrap()
            .iter()
            .any(|(c, _)| c == "INPUT_CHANGED")
    );
    job.reindex().unwrap();
    let (outcome, _) = run(&mut job);
    assert!(
        matches!(outcome, mr_job::Outcome::Completed(_)),
        "{outcome:?}"
    );
}

fn filetime_set(path: &Path, unix: u64) {
    let t = UNIX_EPOCH + Duration::from_secs(unix);
    mr_job::platform::set_file_times(path, t).unwrap();
}

#[test]
fn job_state_corrupt_is_rebuilt_from_the_output_folder() {
    let e = env("corrupt");
    let mut o = options(&e.dest);
    o.workers = Some(1);
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("small-2026b")), o).unwrap();
    let control = Control::new();
    struct PauseAt(usize, Mutex<usize>, Control);
    impl ProgressSink for PauseAt {
        fn progress(&self, _: &Progress) {}
        fn item(&self, _: &ItemOutcome) {
            let mut n = self.1.lock().unwrap();
            *n += 1;
            if *n == self.0 {
                self.2.pause();
            }
        }
        fn issue(&self, _: &str, _: &str) {}
    }
    let outcome = job
        .run(&PauseAt(20, Mutex::new(0), control.clone()), &control)
        .unwrap();
    assert!(matches!(outcome, mr_job::Outcome::Paused(_)));
    let done_before = done_items(&job).len();
    let id = job.id().to_string();
    let db = job.state_dir_of_job().join("job.sqlite");
    drop(job);
    fs::write(&db, b"this is not a database any more").unwrap();
    let _ = fs::remove_file(db.with_extension("sqlite-wal"));
    let _ = fs::remove_file(db.with_extension("sqlite-shm"));

    let mut job = Job::open(&e.state, &id).unwrap();
    assert!(job.rebuilt().is_some(), "JOB_STATE_CORRUPT rebuild");
    assert_eq!(
        done_items(&job).len(),
        done_before,
        "finished files adopted by hash and read-back"
    );
    let (outcome, sink) = run(&mut job);
    assert!(
        matches!(outcome, mr_job::Outcome::Completed(_)),
        "{outcome:?}"
    );
    assert!(
        sink.issues
            .lock()
            .unwrap()
            .iter()
            .any(|(c, _)| c == "JOB_STATE_CORRUPT")
    );
    let files = files_under(job.output_root());
    assert!(
        files.keys().all(|k| !k.contains("_2.")),
        "no duplicates after rebuild"
    );
    assert_eq!(done_items(&job).len(), f01_items());
    assert!(
        job.report_data()
            .unwrap()
            .notes
            .iter()
            .any(|n| n.contains("rebuilt"))
    );
}

#[test]
fn fr34_pause_resume_and_cancel() {
    let e = env("pause");
    let mut o = options(&e.dest);
    o.test_hooks.delay_ms = 30;
    o.workers = Some(2);
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("small-2026b")), o).unwrap();
    // Pause and cancel are triggered by finished items, not by timers, so
    // the test does not depend on machine speed.
    struct After {
        n: usize,
        seen: Mutex<usize>,
        control: Control,
        cancel: bool,
        inner: CollectingSink,
    }
    impl ProgressSink for After {
        fn progress(&self, p: &Progress) {
            self.inner.progress(p);
        }
        fn item(&self, o: &ItemOutcome) {
            let mut seen = self.seen.lock().unwrap();
            *seen += 1;
            if *seen == self.n {
                if self.cancel {
                    self.control.cancel();
                } else {
                    self.control.pause();
                }
            }
            self.inner.item(o);
        }
        fn issue(&self, c: &str, d: &str) {
            self.inner.issue(c, d);
        }
    }
    let control = Control::new();
    let sink = After {
        n: 5,
        seen: Mutex::new(0),
        control: control.clone(),
        cancel: false,
        inner: CollectingSink::default(),
    };
    let outcome = job.run(&sink, &control).unwrap();
    let mr_job::Outcome::Paused(stats) = &outcome else {
        panic!("{outcome:?}")
    };
    assert!(stats.done > 0 && stats.done < f01_items(), "{stats:?}");
    assert!(
        !sink.inner.progress.lock().unwrap().is_empty(),
        "progress reported"
    );
    // Resume.
    control.resume();
    let sink = After {
        n: 5,
        seen: Mutex::new(0),
        control: control.clone(),
        cancel: true,
        inner: CollectingSink::default(),
    };
    let outcome = job.run(&sink, &control).unwrap();
    let mr_job::Outcome::Cancelled(stats2) = &outcome else {
        panic!("{outcome:?}")
    };
    assert!(
        stats2.done >= stats.done + 5,
        "resumed work was kept: {stats2:?} after {stats:?}"
    );
    assert!(
        !job.output_root().join(".memories-rescue/tmp").exists(),
        "temp removed on cancel"
    );
    // A fresh control finishes it.
    let (outcome, _) = run(&mut job);
    assert!(matches!(outcome, mr_job::Outcome::Completed(_)));
    assert_eq!(done_items(&job).len(), f01_items());
}

#[test]
fn fr39_retry_processes_exactly_the_injected_failures() {
    let e = env("retry");
    let t = truth("small-2026b");
    let ids: Vec<String> = t["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| !i["main"].is_null())
        .take(5)
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    let mut o = options(&e.dest);
    o.test_hooks.fail_ids = ids.clone();
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("small-2026b")), o.clone()).unwrap();
    let (outcome, _) = run(&mut job);
    let mr_job::Outcome::Completed(stats) = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(stats.failed, 5);
    let boundary = job.journal().unwrap().last().unwrap().seq;
    let mut fixed = o;
    fixed.test_hooks.fail_ids.clear();
    assert_eq!(job.retry_failed(Some(fixed)).unwrap(), 5);
    let (outcome, _) = run(&mut job);
    assert!(matches!(outcome, mr_job::Outcome::Completed(_)));
    let restarted: BTreeSet<String> = job
        .journal()
        .unwrap()
        .into_iter()
        .filter(|ev| ev.seq > boundary && ev.event == "start")
        .map(|ev| ev.item)
        .collect();
    let want: BTreeSet<String> = job
        .items()
        .unwrap()
        .into_iter()
        .filter(|i| {
            ids.iter()
                .any(|id| id.eq_ignore_ascii_case(&i.plan.file_id))
        })
        .map(|i| i.plan.key)
        .collect();
    assert_eq!(
        restarted, want,
        "exactly the five failed items were processed again"
    );
    assert_eq!(
        done_items(&job).len(),
        t["counts"]["main_files"].as_u64().unwrap() as usize
    );
}

#[test]
fn fr40_adding_the_missing_part_processes_only_what_it_brings() {
    let e = env("addpart");
    let mut job = Job::create(
        &e.state,
        &zips_in(&fixture_dir("missing-part")),
        options(&e.dest),
    )
    .unwrap();
    let (outcome, _) = run(&mut job);
    assert!(matches!(outcome, mr_job::Outcome::Completed(_)));
    assert_eq!(done_items(&job).len(), 750);
    let boundary = job.journal().unwrap().last().unwrap().seq;
    let part3 = fixture_dir("multipart").join("mydata~1760000000000-3.zip");
    let r = job.add_part(&part3).unwrap();
    assert_eq!(r.new_items, 250, "{r:?}");
    assert!(r.missing_parts.is_empty());
    let (outcome, _) = run(&mut job);
    assert!(
        matches!(outcome, mr_job::Outcome::Completed(_)),
        "{outcome:?}"
    );
    let items = job.items().unwrap();
    let part3_label = "mydata~1760000000000-3.zip";
    let touches_part3: BTreeSet<String> = items
        .iter()
        .filter(|i| {
            i.plan.main.part_label == part3_label
                || i.plan
                    .overlay
                    .as_ref()
                    .is_some_and(|o| o.part_label == part3_label)
        })
        .map(|i| i.plan.key.clone())
        .collect();
    let restarted: BTreeSet<String> = job
        .journal()
        .unwrap()
        .into_iter()
        .filter(|ev| ev.seq > boundary && ev.event == "start")
        .map(|ev| ev.item)
        .collect();
    let requeued: BTreeSet<String> = r.requeued.iter().cloned().collect();
    let expected: BTreeSet<String> = touches_part3.union(&requeued).cloned().collect();
    assert_eq!(
        restarted, expected,
        "only items with a file in part 3, plus re-matched ones"
    );
    assert_eq!(done_items(&job).len(), 1000);
    let files = files_under(job.output_root());
    assert!(files.keys().all(|k| !k.contains("_2.")), "no duplicates");
    println!(
        "FR-40: {} items processed after adding part 3 ({} with a file in part 3, {} re-matched)",
        restarted.len(),
        touches_part3.len(),
        requeued.len()
    );
}

#[test]
fn fr44_free_tier_then_unlock_processes_only_the_remainder() {
    let e = env("free");
    let t = truth("free-limit");
    let mut o = options(&e.dest);
    o.unlocked = false;
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("free-limit")), o).unwrap();
    let (outcome, _) = run(&mut job);
    let mr_job::Outcome::Completed(stats) = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(stats.done, 100);
    assert_eq!(stats.skipped_free_limit, 150);
    let first_100: BTreeSet<String> = t["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["chronological_rank"].as_u64().unwrap() < 100)
        .map(|i| i["id"].as_str().unwrap().to_ascii_uppercase())
        .collect();
    let done: BTreeSet<String> = done_items(&job)
        .into_iter()
        .map(|i| i.plan.file_id.to_ascii_uppercase())
        .collect();
    assert_eq!(done, first_100, "the first 100 in chronological order");
    let data = job.report_data().unwrap();
    assert_eq!(
        data.rows
            .iter()
            .filter(|r| r.status() == Status::SkippedFreeLimit)
            .count(),
        150
    );
    let boundary = job.journal().unwrap().last().unwrap().seq;
    assert_eq!(job.unlock().unwrap(), 150);
    let (outcome, _) = run(&mut job);
    let mr_job::Outcome::Completed(stats) = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(stats.done, 250);
    assert_eq!(stats.processed_this_run, 150);
    let restarted: Vec<String> = job
        .journal()
        .unwrap()
        .into_iter()
        .filter(|ev| ev.seq > boundary && ev.event == "start")
        .map(|ev| ev.item)
        .collect();
    assert_eq!(restarted.len(), 150);
    let first_keys: BTreeSet<String> = job
        .items()
        .unwrap()
        .into_iter()
        .filter(|i| first_100.contains(&i.plan.file_id.to_ascii_uppercase()))
        .map(|i| i.plan.key)
        .collect();
    assert!(
        restarted.iter().all(|k| !first_keys.contains(k)),
        "the first 100 were not redone"
    );
}

#[test]
fn dest_unavailable_pauses_then_resumes_by_itself() {
    let e = env("offline");
    let mut o = options(&e.dest);
    o.test_hooks.dest_offline_after = Some(5);
    o.test_hooks.dest_offline_secs = 2;
    o.workers = Some(2);
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("small-2026b")), o).unwrap();
    let t0 = Instant::now();
    let (outcome, sink) = run(&mut job);
    assert!(
        matches!(outcome, mr_job::Outcome::Completed(_)),
        "{outcome:?}"
    );
    let issues: Vec<String> = sink
        .issues
        .lock()
        .unwrap()
        .iter()
        .map(|(c, _)| c.clone())
        .collect();
    assert!(
        issues.contains(&"DEST_UNAVAILABLE".to_string()),
        "{issues:?}"
    );
    assert!(issues.contains(&"DEST_AVAILABLE".to_string()), "{issues:?}");
    assert!(t0.elapsed() >= Duration::from_secs(2));
    assert_eq!(done_items(&job).len(), f01_items());
    let waiting = sink
        .progress
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.stage == "waiting for the output drive");
    assert!(waiting);
}

// ---------------------------------------------------------------------------
// FR-25, FR-26 and the compositor seam
// ---------------------------------------------------------------------------

/// A stand-in compositor: "burns in" by copying the main file.
struct CopyCompositor;

impl Compositor for CopyCompositor {
    fn composite(&self, req: &CompositeRequest<'_>) -> Result<CompositeOutcome, CompositeError> {
        fs::copy(req.main, req.out).map_err(|e| CompositeError {
            code: "FFMPEG_FAIL".into(),
            detail: e.to_string(),
        })?;
        Ok(CompositeOutcome::Composited)
    }
}

/// A compositor that always fails (the FFMPEG_FAIL fallback).
struct FailingCompositor;

impl Compositor for FailingCompositor {
    fn composite(&self, _: &CompositeRequest<'_>) -> Result<CompositeOutcome, CompositeError> {
        Err(CompositeError {
            code: "FFMPEG_FAIL".into(),
            detail: "simulated".into(),
        })
    }
}

#[test]
fn fr25_fr26_overlay_modes_originals_and_the_compositor_seam() {
    let dir = fixture_dir("small-2026b");
    let t = truth("small-2026b");
    let with_overlay = t["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| !i["overlay"].is_null() && !i["main"].is_null())
        .count();
    type Case = (&'static str, OverlayMode, bool, Option<Arc<dyn Compositor>>);
    let cases: Vec<Case> = vec![
        ("pass-through", OverlayMode::BurnInKeepOriginals, true, None),
        (
            "composited+originals",
            OverlayMode::BurnInKeepOriginals,
            true,
            Some(Arc::new(CopyCompositor)),
        ),
        (
            "composited, originals off",
            OverlayMode::BurnInKeepOriginals,
            false,
            Some(Arc::new(CopyCompositor)),
        ),
        (
            "burn-in only",
            OverlayMode::BurnInOnly,
            true,
            Some(Arc::new(CopyCompositor)),
        ),
        (
            "save separate",
            OverlayMode::SaveSeparate,
            true,
            Some(Arc::new(CopyCompositor)),
        ),
        (
            "ignore",
            OverlayMode::Ignore,
            true,
            Some(Arc::new(CopyCompositor)),
        ),
        (
            "compositor fails",
            OverlayMode::BurnInKeepOriginals,
            true,
            Some(Arc::new(FailingCompositor)),
        ),
    ];
    for (name, mode, keep, compositor) in cases {
        let e = env("modes");
        let mut o = options(&e.dest);
        o.overlay_mode = mode;
        o.keep_originals = keep;
        let mut job = Job::create(&e.state, &zips_in(&dir), o).unwrap();
        if let Some(c) = compositor.clone() {
            job.set_compositor(c);
        }
        let (outcome, _) = run(&mut job);
        assert!(
            matches!(outcome, mr_job::Outcome::Completed(_)),
            "{name}: {outcome:?}"
        );
        let files = files_under(job.output_root());
        let originals = files.keys().filter(|k| k.contains("/originals/")).count();
        let beside = files
            .keys()
            .filter(|k| k.ends_with("-overlay.png") && !k.contains("/originals/"))
            .count();
        let pending = job
            .items()
            .unwrap()
            .iter()
            .filter(|i| i.composite_pending)
            .count();
        let failed = job
            .items()
            .unwrap()
            .iter()
            .filter(|i| i.stage == Stage::Failed)
            .count();
        match name {
            "pass-through" => {
                assert_eq!(
                    (originals, beside, pending),
                    (0, with_overlay, with_overlay),
                    "{name}"
                );
            }
            "composited+originals" => {
                assert_eq!(
                    (originals, beside, pending),
                    (2 * with_overlay, 0, 0),
                    "{name}: FR-25 AC1"
                );
            }
            "composited, originals off" | "burn-in only" => {
                assert_eq!((originals, beside), (0, 0), "{name}: no originals/ folder");
                assert!(!files.keys().any(|k| k.contains("originals")));
            }
            "save separate" => {
                assert_eq!((originals, beside, pending), (0, with_overlay, 0), "{name}")
            }
            "ignore" => assert_eq!((originals, beside), (0, 0), "{name}"),
            "compositor fails" => {
                assert_eq!(failed, with_overlay, "{name}: reported failed, output kept");
                assert_eq!(
                    beside, with_overlay,
                    "{name}: original plus separate overlay"
                );
                let kept = job
                    .items()
                    .unwrap()
                    .into_iter()
                    .filter(|i| i.stage == Stage::Failed)
                    .all(|i| {
                        i.output_path.is_some()
                            && i.last_error_code.as_deref() == Some("FFMPEG_FAIL")
                    });
                assert!(kept);
            }
            _ => unreachable!(),
        }
    }
}

// ---------------------------------------------------------------------------
// Section 3.7 collisions: a file already in the output folder is never overwritten
// ---------------------------------------------------------------------------

#[test]
fn foreign_files_in_the_output_folder_get_a_suffix_not_overwritten() {
    let e = env("foreign");
    let inputs = zips_in(&fixture_dir("small-2026b"));
    let o = options(&e.dest);
    // Learn the planned name of the first item, then put a stranger's file there.
    let p = plan(&inputs, &o).unwrap();
    let first = p.sample(1)[0].clone();
    let squatter = mr_job::layout::to_fs(&p.output_root, &first.output_rel);
    fs::create_dir_all(squatter.parent().unwrap()).unwrap();
    fs::write(&squatter, b"the user's own file").unwrap();
    let mut job = Job::create(&e.state, &inputs, o).unwrap();
    let (outcome, _) = run(&mut job);
    assert!(matches!(outcome, mr_job::Outcome::Completed(_)));
    assert_eq!(
        fs::read(&squatter).unwrap(),
        b"the user's own file",
        "never overwritten"
    );
    let row = job
        .items()
        .unwrap()
        .into_iter()
        .find(|i| i.plan.key == first.key)
        .unwrap();
    let rel = row.output_path.unwrap();
    assert_eq!(
        rel,
        mr_job::layout::join_rel(
            mr_job::layout::split_rel(&first.output_rel).0,
            &mr_job::layout::with_suffix(mr_job::layout::split_rel(&first.output_rel).1, 2),
        )
    );
    assert!(job.output_root().join(&rel).is_file());
}

#[test]
fn a_job_runs_once_at_a_time_and_open_does_not_touch_its_temp() {
    let e = env("busy");
    let mut o = options(&e.dest);
    o.test_hooks.delay_ms = 50;
    o.workers = Some(1);
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("small-2026b")), o).unwrap();
    let id = job.id().to_string();
    let state = e.state.clone();
    let control = Control::new();
    let c = control.clone();
    let runner = std::thread::spawn(move || {
        let out = job.run(&CollectingSink::default(), &c).unwrap();
        (job, out)
    });
    std::thread::sleep(Duration::from_millis(400));
    // A second handle can read state while the first one runs...
    let t_open = Instant::now();
    let mut second = Job::open(&state, &id).unwrap();
    println!("second open took {} ms", t_open.elapsed().as_millis());
    assert_eq!(second.items().unwrap().len(), f01_items());
    // ...but cannot run it or clear its temp.
    assert!(matches!(
        second.run(&CollectingSink::default(), &Control::new()),
        Err(mr_job::JobError::Busy { .. })
    ));
    assert!(matches!(
        second.clean_temp(),
        Err(mr_job::JobError::Busy { .. })
    ));
    control.cancel();
    let (_job, out) = runner.join().unwrap();
    assert!(matches!(out, mr_job::Outcome::Cancelled(_)), "{out:?}");
    // Now it may run again, from the other handle.
    let (out, _) = run(&mut second);
    assert!(matches!(out, mr_job::Outcome::Completed(_)), "{out:?}");
    assert_eq!(done_items(&second).len(), f01_items());
}

#[test]
fn composite_pending_items_are_composited_once_a_compositor_is_available() {
    let e = env("pending");
    let mut job = Job::create(
        &e.state,
        &zips_in(&fixture_dir("small-2026b")),
        options(&e.dest),
    )
    .unwrap();
    run(&mut job);
    let pending = job
        .items()
        .unwrap()
        .iter()
        .filter(|i| i.composite_pending)
        .count();
    assert!(pending > 0);
    let before = files_under(job.output_root());
    assert_eq!(
        before
            .keys()
            .filter(|k| k.ends_with("-overlay.png"))
            .count(),
        pending
    );
    let boundary = job.journal().unwrap().last().unwrap().seq;
    job.set_compositor(Arc::new(CopyCompositor));
    assert_eq!(job.requeue_composite_pending().unwrap(), pending);
    let (outcome, _) = run(&mut job);
    assert!(
        matches!(outcome, mr_job::Outcome::Completed(_)),
        "{outcome:?}"
    );
    let restarted = job
        .journal()
        .unwrap()
        .into_iter()
        .filter(|ev| ev.seq > boundary && ev.event == "start")
        .count();
    assert_eq!(restarted, pending, "only the pending items were processed");
    assert_eq!(
        job.items()
            .unwrap()
            .iter()
            .filter(|i| i.composite_pending)
            .count(),
        0
    );
    let after = files_under(job.output_root());
    let beside = after
        .keys()
        .filter(|k| k.ends_with("-overlay.png") && !k.contains("/originals/"))
        .count();
    let originals = after.keys().filter(|k| k.contains("/originals/")).count();
    assert_eq!(
        beside, 0,
        "overlays saved beside were removed after burn-in"
    );
    assert_eq!(
        originals,
        2 * pending,
        "originals/ kept for every composited item"
    );
    assert!(after.keys().all(|k| !k.contains("_2.")), "no duplicates");
}

#[test]
fn an_output_the_user_edited_is_never_overwritten_or_removed() {
    let e = env("edited");
    let mut job = Job::create(
        &e.state,
        &zips_in(&fixture_dir("small-2026b")),
        options(&e.dest),
    )
    .unwrap();
    run(&mut job);
    let victim = job
        .items()
        .unwrap()
        .into_iter()
        .find(|i| i.composite_pending)
        .expect("an item with an overlay");
    let rel = victim.output_path.clone().unwrap();
    let path = job.output_root().join(&rel);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"edited by the user");
    fs::write(&path, &bytes).unwrap();
    // Write the pending items again (with a compositor): the edited file must survive.
    job.set_compositor(Arc::new(CopyCompositor));
    job.requeue_composite_pending().unwrap();
    let (outcome, _) = run(&mut job);
    assert!(matches!(outcome, mr_job::Outcome::Completed(_)));
    assert_eq!(fs::read(&path).unwrap(), bytes, "the user's edit is kept");
    let row = job
        .items()
        .unwrap()
        .into_iter()
        .find(|i| i.plan.key == victim.plan.key)
        .unwrap();
    let new_rel = row.output_path.unwrap();
    assert_ne!(new_rel, rel, "the new output went next to it");
    assert!(new_rel.contains("_2."), "{new_rel}");
    // Untouched outputs were replaced in place, not duplicated.
    let files = files_under(job.output_root());
    assert_eq!(files.keys().filter(|k| k.contains("_2.")).count(), 1);
}

#[test]
fn composite_pending_warning_and_note_follow_the_compositor() {
    let inputs = zips_in(&fixture_dir("small-2026b"));
    let e = env("pending-note");
    let o = options(&e.dest);
    let codes = |p: &mr_job::Plan| {
        p.warnings
            .iter()
            .map(|w| w.code.clone())
            .collect::<Vec<_>>()
    };
    let pass = plan(&inputs, &o).unwrap();
    assert!(codes(&pass).contains(&"COMPOSITE_PENDING".to_string()));
    let real = mr_job::plan_for(&inputs, &o, &CopyCompositor).unwrap();
    assert!(!codes(&real).contains(&"COMPOSITE_PENDING".to_string()));
    assert_eq!(
        mr_job::plan_for(&inputs, &o, &mr_job::PassThrough)
            .unwrap()
            .warnings,
        pass.warnings
    );
    // The report note reflects what happened.
    let note = |job: &Job| {
        job.report_data()
            .unwrap()
            .notes
            .iter()
            .any(|n| n.contains("overlay saved next to the file"))
    };
    let mut job = Job::create(&e.state, &inputs, o.clone()).unwrap();
    run(&mut job);
    assert!(note(&job), "pass-through leaves overlays beside the files");
    let e2 = env("composited-note");
    let mut job2 = Job::create(&e2.state, &inputs, options(&e2.dest)).unwrap();
    job2.set_compositor(Arc::new(CopyCompositor));
    run(&mut job2);
    assert!(
        !note(&job2),
        "nothing pending once every overlay was burned in"
    );
}
