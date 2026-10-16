//! The engine adapter on the synthetic fixtures (`tools/make_fixture`):
//! what the window would see from each command and event, end to end through
//! mr-job, without a window. A fake Gumroad on loopback stands in for the
//! licence server (SRS §9.3, the test double the SRS asks for).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, Once};
use std::time::{Duration, Instant};

use memories_rescue_lib::engine::license::{
    DEFAULT_ACTIVATION_LIMIT, GumroadConfig, MemorySecrets,
};
use memories_rescue_lib::engine::{CollectingSink, Engine, Paths};
use memories_rescue_lib::ipc::*;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

static FIXTURES: Once = Once::new();

/// The fixtures `tools/make_fixture --all` writes; generated once if missing.
fn fixture(name: &str) -> PathBuf {
    FIXTURES.call_once(|| {
        let root = repo();
        let needed = [
            "missing-part",
            "multipart",
            "free-limit",
            "small-2026b",
            "small-legacy",
        ];
        if needed
            .iter()
            .all(|n| root.join("fixtures").join(n).join("truth.json").is_file())
        {
            return;
        }
        for python in ["python", "python3", "py"] {
            if Command::new(python)
                .args(["tools/make_fixture", "--all", "--quiet"])
                .current_dir(&root)
                .status()
                .is_ok_and(|s| s.success())
            {
                return;
            }
        }
        panic!("fixtures missing and tools/make_fixture could not run");
    });
    repo().join("fixtures").join(name)
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

fn env_with(gumroad: GumroadConfig) -> Env {
    let root = std::env::temp_dir().join(format!("mr-engine-{}", uuid::Uuid::new_v4().simple()));
    let paths = Paths {
        state_dir: root.join("state"),
        cache_dir: root.join("cache"),
        pictures_dir: root.join("pictures"),
        desktop_dir: root.join("desktop"),
        resource_dir: None,
        log_dir: root.join("logs"),
        // The pass-through compositor: these tests are about the adapter;
        // tests/media.rs runs the real one.
        ffmpeg: None,
    };
    for d in [&paths.pictures_dir, &paths.desktop_dir] {
        std::fs::create_dir_all(d).unwrap();
    }
    let sink = Arc::new(CollectingSink::default());
    let engine = Engine::new(
        paths,
        sink.clone(),
        Arc::new(MemorySecrets::default()),
        gumroad,
    );
    let dest = root.join("out");
    std::fs::create_dir_all(&dest).unwrap();
    Env {
        engine,
        sink,
        root,
        dest,
    }
}

fn env() -> Env {
    env_with(GumroadConfig {
        product_id: None,
        verify_url: String::new(),
        product_url: "https://example.invalid".into(),
        price_display: None,
        activation_limit: DEFAULT_ACTIVATION_LIMIT,
    })
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

fn usable_ids(inputs: &[InputFile]) -> Vec<String> {
    inputs
        .iter()
        .filter(|i| i.status == InputStatus::Ok)
        .map(|i| i.id.clone())
        .collect()
}

/// Waits for the job's next `job_state` event with one of `states` after `from` events.
fn wait_state(
    env: &Env,
    job: &str,
    from: usize,
    states: &[JobState],
) -> (JobState, Option<IpcError>, usize) {
    let end = Instant::now() + Duration::from_secs(180);
    loop {
        let all = env.sink.of("job_state");
        for (i, v) in all.iter().enumerate().skip(from) {
            let e: JobStateEvent = serde_json::from_value(v.clone()).unwrap();
            if e.job_id == job && states.contains(&e.state) {
                return (e.state, e.error, i + 1);
            }
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for {states:?}; got {all:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn items_for(env: &Env, job: &str) -> Vec<ItemOutcome> {
    env.sink
        .of("item")
        .into_iter()
        .map(|v| serde_json::from_value::<ItemEvent>(v).unwrap())
        .filter(|e| e.job_id == job)
        .map(|e| e.item)
        .collect()
}

#[test]
fn inputs_and_analysis_report_the_missing_part() {
    let env = env();
    let mut paths = zips(&fixture("missing-part"));
    paths.push(repo().join("README.md").to_string_lossy().into_owned());
    let inputs = env.engine.add_inputs(&paths).unwrap();
    assert_eq!(inputs.len(), 4);
    let readme = inputs.iter().find(|i| i.name == "README.md").unwrap();
    assert_eq!(
        (readme.role, readme.status),
        (InputRole::Ignored, InputStatus::NotExport)
    );
    let primary = inputs
        .iter()
        .find(|i| i.name == "mydata~1760000000000.zip")
        .unwrap();
    assert_eq!(
        (primary.role, primary.part_number),
        (InputRole::Primary, Some(1))
    );
    // The renamed part is numbered from its content.
    let renamed = inputs.iter().find(|i| i.name == "mydata (1).zip").unwrap();
    assert_eq!(
        (renamed.role, renamed.part_number),
        (InputRole::Part, Some(2))
    );
    assert!(inputs.iter().all(|i| i.size_bytes > 0));

    let s = env.engine.analyze(&usable_ids(&inputs)).unwrap();
    assert_eq!(s.missing_parts, vec![3]);
    assert_eq!(
        s.parts
            .iter()
            .map(|p| (p.part_number, p.status))
            .collect::<Vec<_>>(),
        vec![
            (1, PartStatus::Ok),
            (2, PartStatus::Ok),
            (3, PartStatus::Missing),
            (4, PartStatus::Ok)
        ]
    );
    let missing = s
        .issues
        .iter()
        .find(|i| i.code == ErrorCode::PartMissing)
        .unwrap();
    assert_eq!(missing.part_number, Some(3));
    assert_eq!(missing.count, Some(250), "F-05: 250 mains lost");
    assert_eq!(s.counts.orphan_overlays, 77, "F-05: 77 orphan overlays");
    assert!(!s.blocked);
    assert!(s.date_range.first.is_some());

    env.engine.remove_input(&readme.id);
    assert!(
        env.engine
            .add_inputs(&[])
            .unwrap()
            .iter()
            .all(|i| i.name != "README.md")
    );
}

#[test]
fn a_links_only_export_is_blocked() {
    let env = env();
    let inputs = env
        .engine
        .add_inputs(&zips(&fixture("small-legacy")))
        .unwrap();
    let s = env.engine.analyze(&usable_ids(&inputs)).unwrap();
    assert_eq!(s.profile, "legacy");
    assert!(s.blocked);
}

#[test]
fn plan_preview_is_consistent_and_writes_only_thumbnails() {
    let env = env();
    let inputs = env.engine.add_inputs(&zips(&fixture("multipart"))).unwrap();
    let ids = usable_ids(&inputs);
    let o = options(&env);
    let p = env.engine.plan(&ids, &o).unwrap();
    let t = &p.totals;
    assert_eq!(t.photos + t.videos, t.items);
    assert_eq!(
        p.confidence.exact + p.confidence.ordinal + p.confidence.none,
        t.items
    );
    assert_eq!(
        p.location.applied + p.location.none + p.location.uncertain,
        t.items
    );
    assert_eq!(t.missing_media, 0);
    assert!(p.samples.len() >= 20);
    let photo_thumbs: Vec<&PlanItem> = p
        .samples
        .iter()
        .filter(|s| s.media_type == MediaType::Photo)
        .collect();
    assert!(!photo_thumbs.is_empty());
    for s in &photo_thumbs {
        let thumb = s
            .thumbnail_path
            .as_ref()
            .expect("photo samples get a thumbnail");
        assert!(
            Path::new(thumb).starts_with(env.root.join("cache").join("thumbnails")),
            "{thumb}"
        );
        assert!(std::fs::metadata(thumb).unwrap().len() > 0);
    }
    let months: u64 = p
        .tree
        .children
        .iter()
        .flat_map(|y| y.children.iter())
        .map(|m| m.file_count)
        .sum();
    assert_eq!(months, t.items, "every planned file sits in a month folder");
    assert_eq!(
        p.free_limit,
        FreeLimitInfo {
            applies: true,
            limit: 100,
            items_beyond: t.items - 100
        }
    );
    // Burn-in without a compositor yet: said plainly in the preview.
    assert!(
        p.warnings
            .iter()
            .any(|w| w.code == ErrorCode::EncoderUnavailable),
        "{:?}",
        p.warnings
    );
    // ...and those overlays are not counted as captions added.
    assert_eq!((t.overlays_to_composite, t.originals_kept), (0, 0));
    // FR-29 AC1: nothing in the output folder but the preview report, and only on request.
    assert_eq!(std::fs::read_dir(&env.dest).unwrap().count(), 0);
    let report = env.engine.export_preview_report(&p.plan_id).unwrap();
    assert!(report.ends_with("preview-report.html"));
    assert!(std::fs::read_to_string(&report).unwrap().contains("<html"));

    let space = env.engine.estimate_space(&ids, &o).unwrap();
    let lean = env
        .engine
        .estimate_space(
            &ids,
            &JobOptions {
                keep_originals: false,
                overlay_mode: OverlayMode::BurnInOnly,
                ..o.clone()
            },
        )
        .unwrap();
    assert!(lean.total_bytes < space.total_bytes);
    assert_eq!(
        space.total_bytes,
        space.output_bytes + space.originals_bytes + space.temp_bytes + space.zip_bytes
    );
    let v = env.engine.volume_info(&env.dest.to_string_lossy()).unwrap();
    assert!(v.free_bytes > 0 && v.total_bytes >= v.free_bytes && v.writable);
}

#[test]
fn free_tier_stops_at_100_and_later_finishes_partial() {
    let env = env();
    let inputs = env
        .engine
        .add_inputs(&zips(&fixture("free-limit")))
        .unwrap();
    let ids = usable_ids(&inputs);
    let job = env.engine.start(&ids, &options(&env)).unwrap();
    let (_, _, seen) = wait_state(&env, &job, 0, &[JobState::Running]);
    let (state, _, seen) = wait_state(
        &env,
        &job,
        seen,
        &[
            JobState::FreeLimitReached,
            JobState::Done,
            JobState::Partial,
            JobState::Failed,
        ],
    );
    assert_eq!(state, JobState::FreeLimitReached);
    let processed = items_for(&env, &job)
        .into_iter()
        .filter(|i| i.status != ItemStatus::SkippedFreeLimit)
        .count();
    assert_eq!(
        processed, 100,
        "FR-44 AC1: exactly 100 outputs without a licence"
    );
    let progress: Vec<ProgressEvent> = env
        .sink
        .of("progress")
        .into_iter()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect();
    assert!(
        progress
            .iter()
            .any(|p| p.job_id == job && p.items_total == 100)
    );

    // Waiting for unlock counts as resumable (Welcome offers it), as in the mock.
    let listed = env.engine.list_resumable_jobs();
    let mine = listed.iter().find(|j| j.job_id == job).expect("listed");
    assert_eq!(
        (mine.state, mine.items_done, mine.items_total),
        (JobState::FreeLimitReached, 100, 250)
    );

    env.engine.resume(&job).unwrap();
    let (state, _, _) = wait_state(&env, &job, seen, &[JobState::Partial, JobState::Done]);
    assert_eq!(state, JobState::Partial);
    let r = env.engine.get_report(&job).unwrap();
    assert_eq!(r.counts.skipped_free_limit, 150);
    assert_eq!(
        r.counts.done + r.counts.metadata_partial + r.counts.location_uncertain + r.counts.failed,
        100
    );
    assert!(r.can_unlock && !r.can_add_part);
    assert!(
        r.report_html_path
            .as_deref()
            .is_some_and(|p| Path::new(p).is_file())
    );
    assert_eq!(r.state, JobState::Partial);
    // Progress counts the items saved with a warning; the last event releases sleep prevention.
    let last = env
        .sink
        .of("progress")
        .into_iter()
        .map(|v| serde_json::from_value::<ProgressEvent>(v).unwrap())
        .rfind(|p| p.job_id == job)
        .unwrap();
    assert_eq!(
        last.warnings,
        r.counts.metadata_partial + r.counts.location_uncertain
    );
    assert!(!last.sleep_prevented);
}

/// A loopback stand-in for Gumroad's verify endpoint: `GOOD-KEY` is valid
/// and used once, `LIMIT-KEY` is valid and already used 5 times, anything
/// else is unknown. Records every request body.
fn fake_gumroad() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/v2/licenses/verify",
        listener.local_addr().unwrap()
    );
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut len = 0usize;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                if line == "\r\n" {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
                line.clear();
            }
            let mut body = vec![0u8; len];
            let _ = reader.read_exact(&mut body);
            let body = String::from_utf8_lossy(&body).into_owned();
            let limit = body.contains("license_key=LIMIT-KEY");
            let good = limit || body.contains("license_key=GOOD-KEY");
            seen.lock().unwrap().push(body);
            let json = if limit {
                r#"{"success":true,"uses":5,"purchase":{"refunded":false,"chargebacked":false}}"#
            } else if good {
                r#"{"success":true,"uses":1,"purchase":{"refunded":false,"chargebacked":false}}"#
            } else {
                r#"{"success":false,"message":"That license does not exist for the provided product."}"#
            };
            let status = if good { "200 OK" } else { "404 Not Found" };
            let mut s = stream;
            let _ = write!(
                s,
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                json.len()
            );
        }
    });
    (url, bodies)
}

#[test]
fn activation_unlocks_the_rest_and_only_activation_counts_a_use() {
    let (url, bodies) = fake_gumroad();
    let env = env_with(GumroadConfig {
        product_id: Some("prod-123".into()),
        verify_url: url,
        product_url: "https://example.invalid".into(),
        price_display: None,
        activation_limit: DEFAULT_ACTIVATION_LIMIT,
    });
    let inputs = env
        .engine
        .add_inputs(&zips(&fixture("free-limit")))
        .unwrap();
    let job = env
        .engine
        .start(&usable_ids(&inputs), &options(&env))
        .unwrap();
    let (state, _, seen) = wait_state(
        &env,
        &job,
        0,
        &[
            JobState::FreeLimitReached,
            JobState::Done,
            JobState::Partial,
        ],
    );
    assert_eq!(state, JobState::FreeLimitReached);

    assert_eq!(
        env.engine.license.activate("WRONG").unwrap_err().code,
        ErrorCode::LicenseInvalid
    );
    // Q-06: an unknown key is refused by the first, non-counting request.
    assert!(
        bodies
            .lock()
            .unwrap()
            .iter()
            .all(|b| b.contains("increment_uses_count=false"))
    );
    // A key already on 5 computers is refused before it costs a use.
    let n = bodies.lock().unwrap().len();
    let limit = env.engine.license.activate("LIMIT-KEY").unwrap_err();
    assert_eq!(limit.code, ErrorCode::LicenseInvalid);
    assert_eq!(limit.detail.as_deref(), Some("activation_limit:5"));
    assert!(
        bodies.lock().unwrap()[n..]
            .iter()
            .all(|b| b.contains("increment_uses_count=false"))
    );
    assert_eq!(env.engine.license.status().tier, LicenseTier::Free);

    let n = bodies.lock().unwrap().len();
    let status = env.engine.license.activate("GOOD-KEY").unwrap();
    assert_eq!(status.tier, LicenseTier::Licensed);
    assert_eq!(status.key_masked.as_deref(), Some("****-****-****-DKEY"));
    assert_eq!(status.activation_limit, 5);
    // The first activation counts one use: a check, then the one increment.
    let sent: Vec<String> = bodies.lock().unwrap()[n..].to_vec();
    assert_eq!(sent.len(), 2, "{sent:?}");
    assert!(sent[0].contains("increment_uses_count=false"));
    assert!(sent[1].contains("increment_uses_count=true"));
    assert!(sent.iter().all(|b| b.contains("product_id=prod-123")));

    let before = items_for(&env, &job).len();
    env.engine.resume(&job).unwrap();
    let (state, _, _) = wait_state(&env, &job, seen, &[JobState::Done, JobState::Partial]);
    let after = items_for(&env, &job);
    assert_eq!(
        after.len() - before,
        150,
        "FR-44 AC1: the remaining 150, the first 100 not redone"
    );
    let r = env.engine.get_report(&job).unwrap();
    assert_eq!(r.counts.skipped_free_limit, 0);
    assert_eq!(
        state,
        if r.counts.failed > 0 {
            JobState::Partial
        } else {
            JobState::Done
        }
    );

    // §9.3 rule 3: a re-check never increments the use count.
    let n = bodies.lock().unwrap().len();
    let v = memories_rescue_lib::engine::license::verify(
        &env.engine.license.config.verify_url,
        "prod-123",
        "GOOD-KEY",
        false,
    );
    assert_eq!(v, memories_rescue_lib::engine::license::Verdict::Valid);
    assert!(bodies.lock().unwrap()[n].contains("increment_uses_count=false"));
    // No server: offline, not invalid.
    let dead = memories_rescue_lib::engine::license::verify(
        "http://127.0.0.1:9/v2/licenses/verify",
        "p",
        "k",
        false,
    );
    assert!(matches!(
        dead,
        memories_rescue_lib::engine::license::Verdict::Unreachable(_)
    ));
    assert_eq!(env.engine.license.deactivate().tier, LicenseTier::Free);
}

#[test]
fn adding_the_missing_part_processes_only_its_items() {
    let (url, _) = fake_gumroad();
    let env = env_with(GumroadConfig {
        product_id: Some("p".into()),
        verify_url: url,
        product_url: "u".into(),
        price_display: None,
        activation_limit: DEFAULT_ACTIVATION_LIMIT,
    });
    env.engine.license.activate("GOOD-KEY").unwrap();
    let inputs = env
        .engine
        .add_inputs(&zips(&fixture("missing-part")))
        .unwrap();
    let job = env
        .engine
        .start(&usable_ids(&inputs), &options(&env))
        .unwrap();
    let (_, _, seen) = wait_state(&env, &job, 0, &[JobState::Done, JobState::Partial]);
    let r1 = env.engine.get_report(&job).unwrap();
    assert!(r1.can_add_part);
    assert_eq!(r1.missing_parts, vec![3]);
    let first = items_for(&env, &job).len();

    let part3 = fixture("multipart").join("mydata~1760000000000-3.zip");
    let added = env.engine.add_part(&job, &part3.to_string_lossy()).unwrap();
    assert_eq!(added.part_number, Some(3));
    assert_eq!(
        env.engine.get_report(&job).unwrap().state,
        r1.state,
        "add_part does not start anything"
    );
    env.engine.resume(&job).unwrap();
    wait_state(&env, &job, seen, &[JobState::Done, JobState::Partial]);
    let r2 = env.engine.get_report(&job).unwrap();
    assert_eq!(r2.missing_parts, Vec::<u32>::new());
    assert!(!r2.can_add_part);
    let new_items = items_for(&env, &job).len() - first;
    // FR-40 AC1: only what part 3 brings, plus finished items whose overlay sat in part 3.
    assert!(new_items >= 250, "{new_items}");
    assert!(
        new_items < first,
        "{new_items} vs {first}: earlier items were not redone wholesale"
    );
}

#[test]
fn settings_persist_and_diagnostics_hold_no_export_content() {
    let env = env();
    let mut s = env.engine.get_settings();
    assert!(s.update_checks, "update checks are on by default (§10.2)");
    assert_eq!(s.update_channel, UpdateChannel::Stable);
    assert_eq!(s.destination, env.root.join("pictures").to_string_lossy());
    s.update_channel = UpdateChannel::Beta;
    s.output_template = "{date}_{shortid}.{ext}".into();
    env.engine.set_settings(s.clone()).unwrap();
    let bad = Settings {
        output_template: "{nope}".into(),
        ..s.clone()
    };
    assert!(env.engine.set_settings(bad).is_err());
    let again = Engine::new(
        env.engine.paths.clone(),
        env.sink.clone(),
        Arc::new(MemorySecrets::default()),
        env.engine.license.config.clone(),
    );
    assert_eq!(again.get_settings(), s);

    let inputs = env
        .engine
        .add_inputs(&zips(&fixture("small-2026b")))
        .unwrap();
    let job = env
        .engine
        .start(&usable_ids(&inputs), &options(&env))
        .unwrap();
    wait_state(
        &env,
        &job,
        0,
        &[
            JobState::Done,
            JobState::Partial,
            JobState::FreeLimitReached,
        ],
    );
    // FR-41: a log line the way the app writes it (home as ~, no URLs).
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap();
    {
        use memories_rescue_lib::logging::{FILES, MAX_FILE, Redactor, RotatingFile, SharedLog};
        use tracing_subscriber::fmt::MakeWriter;
        let log = SharedLog::new(
            RotatingFile::open(&env.engine.paths.log_dir, MAX_FILE, FILES).unwrap(),
            Redactor::new(&home),
        );
        let mut w = log.make_writer();
        writeln!(
            w,
            "INFO read {home}/Downloads/mydata~1.zip; GET https://cf-st.sc-cdn.net/a.mp4?sid=SECRET"
        )
        .unwrap();
    }
    // Shown before saving: exactly what the ZIP will hold.
    let preview = env.engine.diagnostics_preview();
    assert!(preview.destination.ends_with(".zip"));
    assert!(Path::new(&preview.destination).starts_with(env.root.join("desktop")));
    let kinds: Vec<&str> = preview.files.iter().map(|f| f.kind.as_str()).collect();
    for k in ["logs", "app", "settings", "jobs", "journal"] {
        assert!(kinds.contains(&k), "{k} missing from {kinds:?}");
    }
    assert!(preview.files.iter().all(|f| f.bytes > 0));
    let zip_path = env.engine.export_diagnostics().unwrap();
    assert_eq!(zip_path, preview.destination);
    let mut z = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
    let names: Vec<String> = (0..z.len())
        .map(|i| z.by_index(i).unwrap().name().unwrap().to_string())
        .collect();
    let listed: Vec<String> = preview.files.iter().map(|f| f.name.clone()).collect();
    assert_eq!(names, listed);
    assert!(names.contains(&"logs/memories-rescue.log".to_string()));
    let key = regex::Regex::new(r"\d{4}-\d{2}-\d{2}_[0-9A-F]{8}-").unwrap();
    for (i, name) in names.iter().enumerate() {
        let mut text = String::new();
        z.by_index(i).unwrap().read_to_string(&mut text).unwrap();
        assert!(!text.contains("memories/"), "{name}: names an export entry");
        assert!(!text.contains("Download Link"), "{name}: JSON content");
        assert!(
            !text.contains("SECRET") && !text.contains("sc-cdn"),
            "{name}: a URL"
        );
        assert!(!text.contains(&home), "{name}: the home folder");
        assert!(!text.to_lowercase().contains("telemetry"), "{name}");
        if name.starts_with("journal-") {
            // Items by number, never by key (capture date + export file ID).
            assert!(text.contains("\"item-1\""), "{name}");
            assert!(!key.is_match(&text), "{name}: an item key");
        }
    }
    let info = env.engine.app_info();
    assert_eq!(info.support_email, "support@novabyteworks.com");
    assert_eq!(
        info.privacy_policy_url,
        "https://novabyteworks.com/privacy-memories-rescue"
    );
    assert!(info.terms_url.starts_with("https://novabyteworks.com/"));
    assert!(info.faq_url.starts_with("https://novabyteworks.com/"));
    assert!(!env.engine.any_job_running());
}
