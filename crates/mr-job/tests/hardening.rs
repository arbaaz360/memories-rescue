//! Engine hardening from the 2026-10-16 review: the output folder's marker
//! is untrusted (item 2), links in the engine's own folders are refused
//! (item 8), crash-safe intents (item 3), sidecar ownership by hash
//! (item 5), and leftover ZIP partials (item 9).

mod common;

use std::fs;
use std::path::Path;

use common::*;
use mr_job::{Job, Outcome, Stage, find_job_for_output, layout};

fn marker(dest: &Path) -> std::path::PathBuf {
    dest.join(layout::ROOT_DIR)
        .join(layout::STATE_DIR)
        .join(layout::MARKER_FILE)
}

fn write_marker(dest: &Path, text: &str) {
    let m = marker(dest);
    fs::create_dir_all(m.parent().unwrap()).unwrap();
    fs::write(m, text).unwrap();
}

#[test]
fn item2_a_hostile_marker_is_ignored() {
    let inputs = zips_in(&fixture_dir("small-2026a"));
    let base = scratch("marker");
    let state = base.join("state");
    fs::create_dir_all(&state).unwrap();
    // A sentinel the traversal would reach.
    let outside = base.join("outside");
    fs::create_dir_all(outside.join("jobs")).unwrap();
    for (i, bad) in [
        "../outside",
        "..\\..\\outside",
        "/etc",
        "C:\\Windows",
        "\\\\attacker.invalid\\share\\x",
        "7d1c8f4e-9a2b-4c3d-8e5f-0a1b2c3d4e5f/../../x",
        "\u{e9}\u{2615}\u{5199}",
        "",
    ]
    .iter()
    .enumerate()
    {
        let dest = base.join(format!("dest{i}"));
        write_marker(&dest, bad);
        assert_eq!(find_job_for_output(&state, &dest), None, "{bad:?}");
        let job = Job::create(&state, &inputs, options(&dest)).unwrap();
        assert!(
            mr_job::job::valid_job_id(job.id()).is_some(),
            "{bad:?} gave id {}",
            job.id()
        );
        assert_ne!(job.id(), *bad);
        assert!(
            job.state_dir_of_job().starts_with(state.join("jobs")),
            "{bad:?}"
        );
        assert_eq!(
            fs::read_to_string(marker(&dest)).unwrap().trim(),
            job.id(),
            "the marker is rewritten"
        );
    }
    assert!(Job::open(&state, "../outside").is_err());
    assert!(Job::open(&state, "\\\\attacker.invalid\\share").is_err());
    assert!(fs::read_dir(outside.join("jobs")).unwrap().next().is_none());
}

#[test]
fn item2_a_copied_library_does_not_reuse_the_other_folders_job() {
    let inputs = zips_in(&fixture_dir("small-2026a"));
    let e = env("copied");
    let mut a = Job::create(&e.state, &inputs, options(&e.dest)).unwrap();
    run(&mut a);
    let a_id = a.id().to_string();
    // Copy the whole library, marker included, to another folder.
    let dest2 = e.dest.parent().unwrap().join("copy");
    copy_tree(
        &e.dest.join(layout::ROOT_DIR),
        &dest2.join(layout::ROOT_DIR),
    );
    assert_eq!(
        find_job_for_output(&e.state, &dest2),
        None,
        "the marker names another folder's job"
    );
    let b = Job::create(&e.state, &inputs, options(&dest2)).unwrap();
    assert_ne!(b.id(), a_id, "a new job, not the original's id");
    let done = b
        .items()
        .unwrap()
        .iter()
        .filter(|i| i.stage == Stage::Done)
        .count();
    assert_eq!(
        done,
        a.items().unwrap().len(),
        "the copied outputs are adopted after verifying"
    );
    // The original job still belongs to its own folder.
    assert_eq!(find_job_for_output(&e.state, &e.dest), Some(a_id));
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap().flatten() {
        let p = entry.path();
        let t = to.join(entry.file_name());
        if p.is_dir() {
            copy_tree(&p, &t);
        } else {
            fs::copy(&p, &t).unwrap();
        }
    }
}

/// A directory link: a junction on Windows (no privilege needed), a
/// symlink elsewhere.
fn link_dir(link: &Path, target: &Path) {
    #[cfg(windows)]
    {
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "mklink /J");
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[test]
fn item8_a_junction_at_the_state_folder_is_refused_and_nothing_is_deleted() {
    let inputs = zips_in(&fixture_dir("small-2026a"));
    let e = env("junction");
    let mut job = Job::create(&e.state, &inputs, options(&e.dest)).unwrap();
    // Replace .memories-rescue by a link to a folder with a "tmp" of value.
    let victim = e.dest.parent().unwrap().join("victim");
    fs::create_dir_all(victim.join(layout::TMP_DIR)).unwrap();
    fs::write(
        victim.join(layout::TMP_DIR).join("precious.txt"),
        b"keep me",
    )
    .unwrap();
    let state_dir = e.dest.join(layout::ROOT_DIR).join(layout::STATE_DIR);
    let marker_text = fs::read_to_string(state_dir.join(layout::MARKER_FILE)).unwrap();
    fs::remove_dir_all(&state_dir).unwrap();
    link_dir(&state_dir, &victim);
    fs::write(victim.join(layout::MARKER_FILE), marker_text).unwrap();

    let (outcome, sink) = run(&mut job);
    assert!(
        matches!(&outcome, Outcome::Blocked { code, .. } if code == "PERMISSION_DENIED"),
        "{outcome:?}"
    );
    assert!(
        sink.issues
            .lock()
            .unwrap()
            .iter()
            .any(|(c, _)| c == "PERMISSION_DENIED")
    );
    assert!(job.clean_temp().is_err());
    assert_eq!(
        fs::read(victim.join(layout::TMP_DIR).join("precious.txt")).unwrap(),
        b"keep me",
        "nothing behind the link was touched"
    );
    // A new job for that folder is refused too.
    let e2 = env("junction-new");
    let err = Job::create(&e2.state, &inputs, options(&e.dest)).unwrap_err();
    assert!(err.to_string().contains("link"), "{err}");
    assert!(layout::is_redirected(&state_dir));
}
