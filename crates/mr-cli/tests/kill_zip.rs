//! FR-31 resumability under hard kills: the process is killed at random
//! points while it packs ZIP volumes (each packed file slowed by a debug-only
//! hook), resumed each time, and the result must be what an uninterrupted
//! pack gives: every archive recorded, on disk at its recorded size, read
//! back by the `zip` crate byte for byte equal to the tree, nothing
//! half-written left in the destination.

#[path = "../../mr-core/tests/common/mod.rs"]
mod fixtures;

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mr_job::Job;

const KILLS: usize = 8;

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

fn tree(root: &Path) -> BTreeMap<String, PathBuf> {
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
                out.insert(format!("Memories Rescue/{rel}"), p);
            }
        }
    }
    out
}

fn mr_cli(bin: &str, args: &[&str], dest: &Path, state: &Path, first: bool) -> std::process::Child {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    if first {
        cmd.arg("--dest").arg(dest);
    }
    cmd.arg("--state-dir")
        .arg(state)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn mr-cli")
}

/// A volume size that splits the packed output of `inputs` into at least
/// `n` archives, from a probe run and the engine's own partition.
fn volume_for(inputs: &[PathBuf], n: usize) -> (u64, usize) {
    use mr_job::zipout::{self, PackFile};
    let base = std::env::temp_dir().join(format!("mr-cli-kill-zip-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let mut o = mr_job::Options::new(base.join("dest"));
    o.home_tz = Some("Europe/London".into());
    o.unlocked = true;
    let mut job = Job::create(&base.join("state"), inputs, o).unwrap();
    job.run(&mr_job::CollectingSink::default(), &mr_job::Control::new())
        .unwrap();
    let files: Vec<PackFile> = tree(job.output_root())
        .into_iter()
        .map(|(entry, p)| PackFile {
            rel: entry["Memories Rescue/".len()..].to_string(),
            entry,
            size: std::fs::metadata(&p).unwrap().len(),
            mtime: 0,
        })
        .collect();
    let floor =
        (files.iter().map(PackFile::cost).max().unwrap() + zipout::VOLUME_OVERHEAD).max(64 << 10);
    let total: u64 = files.iter().map(PackFile::cost).sum();
    let mut limit = (total / (n as u64 + 1)).max(floor);
    while zipout::partition(&files, Some(limit)).len() < n && limit > floor {
        limit = (limit * 9 / 10).max(floor);
    }
    let _ = std::fs::remove_dir_all(&base);
    (limit, files.len())
}

/// Main files in F-01 on this machine (fewer without ffmpeg).
fn fixtures_items() -> usize {
    let dir = fixtures::fixture_dir("small-2026b");
    let truth: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("truth.json")).unwrap()).unwrap();
    truth["counts"]["main_files"].as_u64().unwrap() as usize
}

#[test]
fn fr31_kill_while_packing_then_resume_cleanly() {
    if !cfg!(debug_assertions) {
        return; // the delay hook is debug-only
    }
    let bin = env!("CARGO_BIN_EXE_mr-cli");
    let inputs = fixtures::zips_in(&fixtures::fixture_dir("small-2026b"));
    let base = std::env::temp_dir().join(format!("mr-cli-kill-zip-{}", std::process::id()));
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

    // A volume size giving at least 5 archives on this machine's fixture
    // (the CI runners have no ffmpeg, so theirs has no videos).
    let (volume, files) = volume_for(&inputs, 5);
    let volume = volume.to_string();
    // About 4 s of packing, whatever the file count, so the kills land in it.
    let delay = (4000 / files.max(1)).clamp(25, 200).to_string();
    let mut run_args: Vec<String> = vec!["run".into()];
    run_args.extend(inputs.iter().map(|p| p.to_string_lossy().into_owned()));
    run_args.extend(
        [
            "--home-tz",
            "Europe/London",
            "--unlocked",
            "--zip-volume",
            &volume,
            "--test-zip-delay-ms",
            &delay,
            "--quiet",
        ]
        .map(String::from),
    );
    let run_refs: Vec<&str> = run_args.iter().map(String::as_str).collect();
    let mut child = mr_cli(bin, &run_refs, &dest, &state, true);
    let mut kills = 0;
    let mut id: Option<String> = None;
    for round in 0..=KILLS {
        if round > 0 {
            let job = id.as_deref().expect("job id known after the first round");
            child = mr_cli(bin, &["resume", job, "--quiet"], &dest, &state, false);
        }
        if round == 0 {
            // Let the items finish first, so that every kill lands in the pack.
            let items = fixtures_items();
            let deadline = Instant::now() + Duration::from_secs(120);
            loop {
                let jobs = mr_job::list_jobs(&state);
                if let Some(j) = jobs.first() {
                    id = Some(j.id.clone());
                    if j.counts.get("done").copied().unwrap_or(0) >= items {
                        break;
                    }
                }
                if child.try_wait().unwrap().is_some() || Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        if round < KILLS {
            std::thread::sleep(Duration::from_millis(rng.between(150, 700)));
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "mr-cli failed: {status}");
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
    let id = id.expect("a job was created");
    // Finish whatever the last round left.
    let status = mr_cli(bin, &["resume", &id, "--quiet"], &dest, &state, false)
        .wait()
        .unwrap();
    assert!(status.success());

    let job = Job::open(&state, &id).unwrap();
    let vols = job.zip_volumes().unwrap();
    assert!(vols.len() >= 4, "{} volumes", vols.len());
    let mut union: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for v in &vols {
        assert_eq!(v.state, "done", "{v:?}");
        let path = dest.join(&v.name);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), v.size);
        let mut a = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        for i in 0..a.len() {
            let mut f = a.by_index(i).unwrap();
            let name = f.name().unwrap().to_string();
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).unwrap();
            assert!(union.insert(name.clone(), buf).is_none(), "{name} twice");
        }
    }
    let t = tree(job.output_root());
    assert_eq!(
        union.keys().collect::<Vec<_>>(),
        t.keys().collect::<Vec<_>>()
    );
    for (k, p) in &t {
        assert!(union[k] == std::fs::read(p).unwrap(), "{k} differs");
    }
    let mut in_dest: Vec<String> = std::fs::read_dir(&dest)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != "Memories Rescue")
        .collect();
    in_dest.sort();
    let mut names: Vec<String> = vols.iter().map(|v| v.name.clone()).collect();
    names.sort();
    assert_eq!(
        in_dest, names,
        "exactly the recorded archives, no partial files"
    );
    println!(
        "FR-31: {kills} kills while packing (seed {seed}); {} archives, {} files verified, {:.1} s",
        vols.len(),
        t.len(),
        started.elapsed().as_secs_f64()
    );
    assert!(kills >= 3, "too few kill points exercised ({kills})");
}
