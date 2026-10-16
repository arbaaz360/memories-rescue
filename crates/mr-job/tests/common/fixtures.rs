//! Shared harness for the fixture-driven integration tests.
#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

/// Every fixture `tools/make_fixture --all` produces.
pub const ALL_FIXTURES: [&str; 11] = [
    "small-2026b",
    "small-2026a",
    "small-legacy",
    "weird-json",
    "multipart",
    "missing-part",
    "truncated",
    "zipslip",
    "tz-edge",
    "unicode-paths",
    "free-limit",
];

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

pub fn fixture_dir(name: &str) -> PathBuf {
    ensure_fixtures();
    repo_root().join("fixtures").join(name)
}

static GENERATE: Once = Once::new();

/// Generate every fixture once per test binary if any is missing.
pub fn ensure_fixtures() {
    GENERATE.call_once(|| {
        let root = repo_root();
        let missing: Vec<&str> = ALL_FIXTURES
            .iter()
            .copied()
            .filter(|n| !root.join("fixtures").join(n).join("truth.json").is_file())
            .collect();
        if missing.is_empty() {
            return;
        }
        eprintln!("fixtures missing ({missing:?}); running tools/make_fixture --all");
        run_generator(&root, &["--all", "--quiet"]);
    });
}

/// Run `tools/make_fixture` with the given arguments from the repo root.
pub fn run_generator(root: &Path, args: &[&str]) {
    for python in ["python", "python3", "py"] {
        let result = Command::new(python)
            .arg("tools/make_fixture")
            .args(args)
            .current_dir(root)
            .status();
        match result {
            Ok(status) if status.success() => return,
            Ok(status) => panic!("{python} tools/make_fixture {args:?} failed with {status}"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => panic!("could not run {python}: {e}"),
        }
    }
    panic!(
        "no Python interpreter found; run `python tools/make_fixture --all` (needs Pillow) and retry"
    );
}

/// Generate one fixture variant into a scratch directory, e.g. with
/// `--zip-order shuffled`, and return the fixture's directory.
pub fn generate_variant(fixture: &str, tag: &str, extra_args: &[&str]) -> PathBuf {
    let out = std::env::temp_dir().join(format!("mr-job-fixtures-{}-{tag}", std::process::id()));
    let mut args = vec!["--fixture", fixture, "--quiet", "--out"];
    let out_s = out.to_string_lossy().into_owned();
    args.push(&out_s);
    args.extend_from_slice(extra_args);
    run_generator(&repo_root(), &args);
    out.join(fixture)
}

/// Every `.zip` in a fixture directory, sorted by name.
pub fn zips_in(dir: &Path) -> Vec<PathBuf> {
    let mut zips: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "zip"))
        .collect();
    zips.sort();
    zips
}
