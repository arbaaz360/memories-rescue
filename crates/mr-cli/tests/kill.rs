//! FR-33 AC1, scaled to F-04 (1,000 items over four parts): kill the
//! process at random points 20 times, resume each time, then check by hash
//! and read-back that no output is duplicated or corrupt and, from the job
//! journal, that no finished item was ever processed again.

#[path = "../../mr-core/tests/common/mod.rs"]
mod fixtures;

use std::collections::{BTreeMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mr_job::{Job, Stage};

const KILLS: usize = 20;

/// A small xorshift so the kill points differ per run without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn between(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next() % (hi - lo)
    }
}

fn files_under(root: &Path) -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with(".memories-rescue") || rel.starts_with("_report") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                out.insert(rel, p);
            }
        }
    }
    out
}

#[test]
fn fr33_ac1_kill_twenty_times_then_finish_with_no_duplicates_or_rework() {
    let bin = env!("CARGO_BIN_EXE_mr-cli");
    let fixture = fixtures::fixture_dir("multipart");
    let inputs = fixtures::zips_in(&fixture);
    let base = std::env::temp_dir().join(format!("mr-cli-kill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let state = base.join("state");
    let dest = base.join("dest");
    std::fs::create_dir_all(&dest).unwrap();
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as u64
        | 1;
    let mut rng = Rng(seed);
    let started = Instant::now();
    let mut id: Option<String> = None;
    let mut kills = 0;
    let mut early_finish = false;

    for round in 0..=KILLS {
        let mut cmd = Command::new(bin);
        match &id {
            None => {
                cmd.arg("run").args(&inputs).args([
                    "--home-tz",
                    "Europe/London",
                    "--unlocked",
                    "--workers",
                    "4",
                    "--test-delay-ms",
                    "40",
                    "--quiet",
                ]);
                cmd.arg("--dest").arg(&dest).arg("--state-dir").arg(&state);
            }
            Some(job) => {
                cmd.args(["resume", job, "--quiet"])
                    .arg("--state-dir")
                    .arg(&state);
            }
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn mr-cli");
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = tx.send(line);
            let mut rest = String::new();
            while reader.read_line(&mut rest).unwrap_or(0) > 0 {
                rest.clear();
            }
        });
        let line = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("mr-cli printed its job id");
        let job = line
            .trim()
            .strip_prefix("job ")
            .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
            .to_string();
        if let Some(prev) = &id {
            assert_eq!(prev, &job);
        }
        id = Some(job);
        if round < KILLS {
            std::thread::sleep(Duration::from_millis(rng.between(150, 700)));
            if let Some(status) = child.try_wait().unwrap() {
                // Finished before this kill point: nothing left to interrupt.
                assert!(status.success(), "mr-cli failed: {status}");
                early_finish = true;
                break;
            }
            child.kill().unwrap();
            let _ = child.wait();
            kills += 1;
        } else {
            let status = child.wait().unwrap();
            assert!(status.success(), "final resume failed: {status}");
        }
    }
    if early_finish {
        // Make sure the job really is finished.
        let status = Command::new(bin)
            .args(["resume", id.as_ref().unwrap(), "--quiet"])
            .arg("--state-dir")
            .arg(&state)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }
    let elapsed = started.elapsed();

    let job = Job::open(&state, id.as_ref().unwrap()).unwrap();
    assert!(job.rebuilt().is_none(), "state survived every kill");
    let items = job.items().unwrap();
    assert_eq!(items.len(), 1000);
    assert!(
        items.iter().all(|i| i.stage == Stage::Done),
        "every item done"
    );

    // No finished item was processed again.
    let mut finished: HashSet<String> = HashSet::new();
    let mut starts = 0;
    let mut restarts_in_flight = 0;
    let mut seen_start: HashSet<String> = HashSet::new();
    for ev in job.journal().unwrap() {
        match ev.event.as_str() {
            "done" => {
                finished.insert(ev.item);
            }
            "start" => {
                starts += 1;
                assert!(
                    !finished.contains(&ev.item),
                    "{} processed again after it was done",
                    ev.item
                );
                if !seen_start.insert(ev.item.clone()) {
                    restarts_in_flight += 1;
                }
            }
            _ => {}
        }
    }

    // No duplicate or corrupt output: exactly the recorded files, each one
    // hashing to its record and reading back its capture metadata.
    let root = job.output_root().to_path_buf();
    let files = files_under(&root);
    let mut expected: HashSet<String> = HashSet::new();
    for i in &items {
        let rel = i.output_path.clone().unwrap();
        let path = root.join(&rel);
        assert_eq!(
            mr_job::fingerprint::sha256_file(&path).unwrap(),
            i.output_sha256.clone().unwrap(),
            "{rel}"
        );
        mr_job::process::verify(&i.plan.ext, &path, &i.plan.capture())
            .unwrap_or_else(|e| panic!("{rel}: {e}"));
        expected.insert(rel);
        if let Some(o) = &i.overlay_output {
            expected.insert(o.clone());
        }
    }
    let actual: HashSet<String> = files.keys().cloned().collect();
    let extra: Vec<&String> = actual.difference(&expected).collect();
    let missing: Vec<&String> = expected.difference(&actual).collect();
    assert!(
        extra.is_empty(),
        "files nobody recorded (duplicates?): {extra:?}"
    );
    assert!(missing.is_empty(), "recorded files missing: {missing:?}");
    assert!(!root.join(".memories-rescue/tmp").exists(), "temp cleaned");

    println!(
        "FR-33 AC1: {kills} kills (seed {seed}), {starts} item starts for 1000 items ({restarts_in_flight} in-flight items redone), 0 finished items redone, {} files verified, {:.1} s",
        files.len(),
        elapsed.as_secs_f64()
    );
    assert!(kills >= 10, "too few kill points exercised ({kills})");
}
