//! Shared helpers for the engine tests.
#![allow(dead_code)]

pub mod fixtures;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[allow(unused_imports)]
pub use fixtures::{fixture_dir, generate_variant, repo_root, zips_in};
use mr_job::{CollectingSink, Control, Job, Options, Outcome};
use serde_json::Value;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A fresh scratch directory per call.
pub fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("mr-job-{}-{tag}-{n}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

pub struct Env {
    pub state: PathBuf,
    pub dest: PathBuf,
}

pub fn env(tag: &str) -> Env {
    let base = scratch(tag);
    let state = base.join("state");
    let dest = base.join("dest");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&dest).unwrap();
    Env { state, dest }
}

pub fn options(dest: &Path) -> Options {
    let mut o = Options::new(dest);
    o.home_tz = Some("Europe/London".into());
    o.unlocked = true;
    o
}

pub fn truth(name: &str) -> Value {
    let p = fixture_dir(name).join("truth.json");
    serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap()
}

pub fn run(job: &mut Job) -> (Outcome, CollectingSink) {
    let sink = CollectingSink::default();
    let outcome = job.run(&sink, &Control::new()).expect("run");
    (outcome, sink)
}

/// Every regular file under `root`, relative with `/`, excluding the job's
/// hidden state folder.
pub fn files_under(root: &Path) -> BTreeMap<String, PathBuf> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel.starts_with(".memories-rescue") {
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

pub fn sha256(path: &Path) -> String {
    mr_job::fingerprint::sha256_file(path).unwrap()
}

/// Extract every ZIP of a fixture into one folder (an "extracted export").
pub fn extract_to_folder(fixture: &Path, out: &Path) {
    fs::create_dir_all(out).unwrap();
    for z in zips_in(fixture) {
        let mut a = zip::ZipArchive::new(fs::File::open(&z).unwrap()).unwrap();
        a.extract(out).unwrap();
    }
}
