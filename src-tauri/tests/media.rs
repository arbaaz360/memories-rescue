//! The engine adapter with the real compositor: mr-composite on the bundled
//! ffmpeg sidecar, as the app runs it (Q-04: software h264_mf, 1.3x bitrate).
//!
//! * F-07 `rotation`: every caption burned in, upright, originals kept.
//! * F-08 `clips`: the preview's clip group ids are mr-job's
//!   `ClipGroup::id`; one sent back unticked (`excluded_clip_groups`) is
//!   left as clips, the others are joined, and the report says which.
//! * F-09 `codecs`: Windows compatibility makes HEVC H.264; the FR-51
//!   notice reaches the summary and the preview, and goes once it is on.
//! * An older job finished without burn-in has its captions done on resume.
//!
//! Skips (with a note) when the sidecar is a placeholder or a fixture was
//! generated without its videos.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use memories_rescue_lib::engine::license::{
    DEFAULT_ACTIVATION_LIMIT, GumroadConfig, MemorySecrets,
};
use memories_rescue_lib::engine::{CollectingSink, Engine, Paths};
use memories_rescue_lib::ipc::*;
use serde_json::Value;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn runs(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
        && std::process::Command::new(path)
            .arg("-version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// The sidecar the app bundles (`binaries/ffmpeg-<triple>`), else the staged one.
fn ffmpeg() -> Option<PathBuf> {
    let exe = std::env::consts::EXE_SUFFIX;
    [
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!("ffmpeg-{}{exe}", env!("TARGET_TRIPLE"))),
        repo()
            .join("third_party")
            .join("ffmpeg")
            .join(format!("ffmpeg{exe}")),
    ]
    .into_iter()
    .find(|p| runs(p))
}

/// A media fixture and its truth, if it was generated with its videos.
fn fixture(name: &str) -> Option<(PathBuf, Value)> {
    let dir = repo().join("fixtures").join(name);
    let truth: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("truth.json")).ok()?).ok()?;
    let with_videos = truth["generator"]["ffmpeg"]["available"].as_bool() == Some(true);
    if !with_videos {
        eprintln!("skip: fixture {name} was generated without its videos");
        return None;
    }
    Some((dir, truth))
}

macro_rules! need {
    ($name:expr) => {
        match (ffmpeg(), fixture($name)) {
            (Some(f), Some(fx)) => (f, fx),
            (None, _) => {
                eprintln!("skip: no ffmpeg sidecar (python tools/sidecars.py fetch)");
                return;
            }
            (_, None) => return,
        }
    };
}

fn zips(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "zip"))
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

struct Env {
    engine: Arc<Engine>,
    sink: Arc<CollectingSink>,
    root: PathBuf,
    dest: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn paths_in(root: &Path, ffmpeg: Option<PathBuf>) -> Paths {
    Paths {
        state_dir: root.join("state"),
        cache_dir: root.join("cache"),
        pictures_dir: root.join("pictures"),
        desktop_dir: root.join("desktop"),
        resource_dir: None,
        log_dir: root.join("logs"),
        ffmpeg,
    }
}

fn engine_on(paths: Paths) -> (Arc<Engine>, Arc<CollectingSink>) {
    let sink = Arc::new(CollectingSink::default());
    let engine = Engine::new(
        paths,
        sink.clone(),
        Arc::new(MemorySecrets::default()),
        GumroadConfig {
            product_id: None,
            verify_url: String::new(),
            product_url: "https://example.invalid".into(),
            price_display: None,
            activation_limit: DEFAULT_ACTIVATION_LIMIT,
        },
    );
    (engine, sink)
}

fn env(ffmpeg: Option<PathBuf>) -> Env {
    let root = std::env::temp_dir().join(format!("mr-media-{}", uuid::Uuid::new_v4().simple()));
    let paths = paths_in(&root, ffmpeg);
    for d in [&paths.pictures_dir, &paths.desktop_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let (engine, sink) = engine_on(paths);
    let dest = root.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    Env {
        engine,
        sink,
        root,
        dest,
    }
}

fn options(env: &Env) -> JobOptions {
    JobOptions {
        destination: env.dest.to_string_lossy().into_owned(),
        overlay_mode: OverlayMode::BurnInKeepOriginals,
        keep_originals: true,
        clip_rejoin: false,
        clip_rejoin_years: None,
        apply_uncertain_locations: false,
        windows_compatibility: false,
        zip_output: false,
        zip_volume_bytes: None,
        home_timezone: "Europe/London".into(),
        metadata_only: false,
        workers: Some(2),
        excluded_clip_groups: Vec::new(),
    }
}

fn ids(env: &Env, dir: &Path) -> Vec<String> {
    env.engine
        .add_inputs(&zips(dir))
        .unwrap()
        .iter()
        .filter(|i| i.status == InputStatus::Ok)
        .map(|i| i.id.clone())
        .collect()
}

fn wait_end(sink: &CollectingSink, job: &str, from: usize) -> (JobState, usize) {
    let end = Instant::now() + Duration::from_secs(600);
    loop {
        let all = sink.of("job_state");
        for (i, v) in all.iter().enumerate().skip(from) {
            let e: JobStateEvent = serde_json::from_value(v.clone()).unwrap();
            if e.job_id == job
                && matches!(
                    e.state,
                    JobState::Done | JobState::Partial | JobState::Failed | JobState::Cancelled
                )
            {
                return (e.state, i + 1);
            }
        }
        assert!(Instant::now() < end, "timed out; got {all:?}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn rows(env_state: &Path, job: &str) -> Vec<mr_job::ItemRow> {
    mr_job::Job::open(env_state, job).unwrap().items().unwrap()
}

#[test]
fn f07_captions_are_burned_in_upright_with_originals_kept() {
    let (ffmpeg, (dir, truth)) = need!("rotation");
    let env = env(Some(ffmpeg));
    assert!(env.engine.burns_in());
    let ids = ids(&env, &dir);
    let o = options(&env);
    let p = env.engine.plan(&ids, &o).unwrap();
    assert!(
        !p.warnings
            .iter()
            .any(|w| w.code == ErrorCode::EncoderUnavailable),
        "{:?}",
        p.warnings
    );
    let n = truth["counts"]["overlays"].as_u64().unwrap();
    assert_eq!(
        (p.totals.overlays_to_composite, p.totals.originals_kept),
        (n, n)
    );

    let job = env.engine.start(&ids, &o).unwrap();
    let (state, _) = wait_end(&env.sink, &job, 0);
    assert_eq!(state, JobState::Done);
    let r = env.engine.get_report(&job).unwrap();
    assert_eq!(r.counts.done, truth["counts"]["items"].as_u64().unwrap());
    assert!(r.clips.is_empty());
    let root = PathBuf::from(&r.output_path);
    let by_id: std::collections::HashMap<String, &Value> = truth["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| (i["id"].as_str().unwrap().to_ascii_uppercase(), i))
        .collect();
    for row in rows(&env.engine.paths.state_dir, &job) {
        let item = by_id[&row.plan.file_id.to_ascii_uppercase()];
        let case = item["case"].as_str().unwrap();
        assert!(!row.composite_pending, "{case}: burned in");
        assert!(row.originals_main.is_some(), "{case}: FR-25 original kept");
        let out = mr_job::layout::to_fs(&root, row.output_path.as_ref().unwrap());
        let info = mr_composite::probe(&out).unwrap();
        let want = (
            item["display_size"][0].as_u64().unwrap() as u32,
            item["display_size"][1].as_u64().unwrap() as u32,
        );
        assert_eq!(info.display_size(), Some(want), "{case}: upright");
        if let Some(v) = &info.video {
            assert_eq!((v.codec.as_str(), v.rotation), ("avc1", 0), "{case}");
        }
    }
}

#[test]
fn f08_clip_group_ids_survive_the_round_trip_through_the_window() {
    let (ffmpeg, (dir, truth)) = need!("clips");
    let env = env(Some(ffmpeg.clone()));
    let ids = ids(&env, &dir);
    let o = JobOptions {
        clip_rejoin: true,
        clip_rejoin_years: Some(YearRange {
            from: 2017,
            to: 2021,
        }),
        ..options(&env)
    };
    let p = env.engine.plan(&ids, &o).unwrap();
    let expected = truth["expected_groups"]["default"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(p.clip_groups.len(), expected);
    // The ids are mr-job's own: its plan, for the same inputs and options.
    let inputs: Vec<PathBuf> = zips(&dir).into_iter().map(PathBuf::from).collect();
    let mut engine_opts = mr_job::Options::new(&env.dest);
    engine_opts.clip_rejoin = mr_job::ClipRejoin {
        enabled: true,
        year_range: Some((2017, 2021)),
        skip_groups: Vec::new(),
    };
    engine_opts.home_tz = Some("Europe/London".into());
    let theirs = mr_job::plan_for(
        &inputs,
        &engine_opts,
        &memories_rescue_lib::engine::compositor_for(&ffmpeg),
    )
    .unwrap();
    let mine: Vec<&str> = p.clip_groups.iter().map(|g| g.group_id.as_str()).collect();
    let mr: Vec<&str> = theirs.clip_groups.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(mine, mr);
    for g in &p.clip_groups {
        assert_eq!(g.group_id, g.item_ids[0]);
        assert!(g.total_seconds > 0.0 && g.planned_path.ends_with("-joined.mp4"));
    }

    // The window sends the unticked group back by that id.
    let unticked = p.clip_groups[1].group_id.clone();
    let job = env
        .engine
        .start(
            &ids,
            &JobOptions {
                excluded_clip_groups: vec![unticked.clone()],
                ..o
            },
        )
        .unwrap();
    let (state, _) = wait_end(&env.sink, &job, 0);
    let r = env.engine.get_report(&job).unwrap();
    assert_eq!(state, JobState::Done, "{:?}", r.failures);
    assert_eq!(r.counts.done, 33, "every clip is still saved on its own");
    assert_eq!(r.clips.len(), expected);
    for c in &r.clips {
        let planned = p
            .clip_groups
            .iter()
            .find(|g| g.group_id == c.group_id)
            .expect("same ids in the report");
        assert_eq!(c.clip_count as usize, planned.item_ids.len());
        if c.group_id == unticked {
            assert_eq!(c.status, ClipStatus::Skipped);
            assert!(c.output_path.is_none());
        } else {
            assert_eq!(c.status, ClipStatus::Done, "{c:?}");
            let out = c.output_path.as_ref().unwrap();
            assert!(
                out.ends_with("-joined.mp4") && Path::new(out).is_file(),
                "{out}"
            );
        }
    }
    assert!(
        env.sink.of("item").iter().all(|v| !v["item"]["item_id"]
            .as_str()
            .unwrap_or("")
            .starts_with("clip:")),
        "joins are not items"
    );
    // The report on disk lists the joined videos too.
    let html = std::fs::read_to_string(r.report_html_path.unwrap()).unwrap();
    assert!(html.contains("joined"), "report mentions joined clips");
}

#[test]
fn f09_windows_compatibility_makes_hevc_h264_and_the_notice_goes() {
    let (ffmpeg, (dir, _)) = need!("codecs");
    // Debug builds read MR_TEST_HOOKS; force "this computer can't play HEVC".
    // SAFETY: set once, before anything in this test reads it; other tests in
    // this binary do not look at the HEVC notice.
    unsafe { std::env::set_var("MR_TEST_HOOKS", r#"{"force_hevc_unplayable":true}"#) };
    let env = env(Some(ffmpeg));
    let ids = ids(&env, &dir);
    let summary = env.engine.analyze(&ids).unwrap();
    let notice = summary.hevc_notice.expect("FR-51 notice in the summary");
    assert!(
        notice.hevc >= 1 && notice.sampled >= notice.hevc,
        "{notice:?}"
    );
    assert!(
        !summary.issues.iter().any(|i| i.message.contains("HEVC")),
        "the notice is its own field, not an issue"
    );
    let o = options(&env);
    assert!(env.engine.plan(&ids, &o).unwrap().hevc_notice.is_some());
    let compat = JobOptions {
        windows_compatibility: true,
        ..o
    };
    assert_eq!(env.engine.plan(&ids, &compat).unwrap().hevc_notice, None);

    let job = env.engine.start(&ids, &compat).unwrap();
    let (state, _) = wait_end(&env.sink, &job, 0);
    let r = env.engine.get_report(&job).unwrap();
    assert_eq!(state, JobState::Done, "{:?}", r.failures);
    let root = PathBuf::from(&r.output_path);
    for row in rows(&env.engine.paths.state_dir, &job) {
        let out = mr_job::layout::to_fs(&root, row.output_path.as_ref().unwrap());
        let info = mr_composite::probe(&out).unwrap();
        let codec = info
            .video
            .as_ref()
            .map(|v| v.codec.clone())
            .unwrap_or_default();
        assert!(
            !matches!(codec.as_str(), "hvc1" | "hev1"),
            "{}: {codec}",
            out.display()
        );
    }
}

#[test]
fn an_older_job_finishes_its_captions_when_it_resumes() {
    let (ffmpeg, (dir, truth)) = need!("rotation");
    // First run without the compositor (as before this version): captions beside.
    let env = env(None);
    assert!(!env.engine.burns_in());
    let ids = ids(&env, &dir);
    let job = env.engine.start(&ids, &options(&env)).unwrap();
    assert_eq!(wait_end(&env.sink, &job, 0).0, JobState::Done);
    let items = truth["counts"]["items"].as_u64().unwrap() as usize;
    let before = rows(&env.engine.paths.state_dir, &job);
    assert_eq!(before.iter().filter(|r| r.composite_pending).count(), items);

    // The new version opens the same state with the real compositor.
    let (engine, sink) = engine_on(paths_in(&env.root, Some(ffmpeg)));
    engine.resume(&job).unwrap();
    let (state, _) = wait_end(&sink, &job, 0);
    assert_eq!(state, JobState::Done);
    let after = rows(&engine.paths.state_dir, &job);
    assert!(
        after.iter().all(|r| !r.composite_pending),
        "captions burned in"
    );
    assert_eq!(after.len(), items);
}
