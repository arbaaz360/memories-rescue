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

/// A burn-in that is not byte-identical from one run to the next, like a
/// real re-encode: the photo with a run counter appended.
struct ReencodingCompositor(std::sync::atomic::AtomicU32);

impl mr_job::Compositor for ReencodingCompositor {
    fn composite(
        &self,
        req: &mr_job::CompositeRequest<'_>,
    ) -> Result<mr_job::CompositeOutcome, mr_job::CompositeError> {
        let mut bytes = fs::read(req.main).unwrap();
        let n = self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // JPEG readers ignore bytes after EOI; the hash does not.
        bytes.extend_from_slice(format!("run {n}").as_bytes());
        fs::write(req.out, bytes).unwrap();
        Ok(mr_job::CompositeOutcome::Composited)
    }
}

fn db_of(job: &Job) -> rusqlite::Connection {
    rusqlite::Connection::open(job.state_dir_of_job().join("job.sqlite")).unwrap()
}

/// Rewind an item to "renamed into place, checkpoint lost": pending, its
/// record cleared, the intent of that attempt recorded.
fn lose_checkpoint(job: &Job, row: &mr_job::ItemRow) {
    let w = mr_job::process::Written {
        output_rel: row.output_path.clone().unwrap(),
        output_sha256: row.output_sha256.clone().unwrap(),
        overlay_rel: row.overlay_output.clone(),
        originals_main: row.originals_main.clone(),
        originals_overlay: row.originals_overlay.clone(),
        composite_pending: row.composite_pending,
        hashes: row.output_hashes.clone().into_iter().collect(),
        ..Default::default()
    };
    db_of(job)
        .execute(
            "UPDATE item SET stage = 'pending', output_path = NULL, output_sha256 = NULL,
                overlay_output = NULL, originals_main = NULL, originals_overlay = NULL,
                output_hashes = NULL, intent = ?2 WHERE id = ?1",
            rusqlite::params![row.key(), serde_json::to_string(&w).unwrap()],
        )
        .unwrap();
}

#[test]
fn item3_a_lost_checkpoint_is_finished_from_its_intent_without_a_duplicate() {
    let inputs = zips_in(&fixture_dir("small-2026b"));
    let e = env("intent");
    let mut job = Job::create(&e.state, &inputs, options(&e.dest)).unwrap();
    job.set_compositor(std::sync::Arc::new(
        ReencodingCompositor(Default::default()),
    ));
    run(&mut job);
    let composited: Vec<mr_job::ItemRow> = job
        .items()
        .unwrap()
        .into_iter()
        .filter(|r| r.originals_main.is_some())
        .collect();
    assert!(composited.len() >= 2, "the fixture has burned-in items");
    let files_before = files_under(job.output_root()).len();
    // One item lost its checkpoint after every rename; another after the
    // sidecars only (its main never made it).
    let (whole, partial) = (&composited[0], &composited[1]);
    lose_checkpoint(&job, whole);
    lose_checkpoint(&job, partial);
    fs::remove_file(layout::to_fs(
        job.output_root(),
        partial.output_path.as_ref().unwrap(),
    ))
    .unwrap();
    let starts_before = job
        .journal()
        .unwrap()
        .iter()
        .filter(|e| e.item == whole.key() && e.event == "start")
        .count();

    let (_o, _s) = run(&mut job);
    let after = |k: &str| {
        job.items()
            .unwrap()
            .into_iter()
            .find(|r| r.key() == k)
            .unwrap()
    };
    let w = after(whole.key());
    assert_eq!(w.stage, Stage::Done);
    assert_eq!(
        w.output_path, whole.output_path,
        "the placed file is accepted"
    );
    assert_eq!(
        job.journal()
            .unwrap()
            .iter()
            .filter(|e| e.item == whole.key() && e.event == "start")
            .count(),
        starts_before,
        "not redone"
    );
    let p = after(partial.key());
    assert_eq!(p.stage, Stage::Done);
    assert_eq!(p.output_path, partial.output_path, "same name: no _2");
    assert_eq!(
        p.originals_main, partial.originals_main,
        "its sidecars were ours and replaced"
    );
    assert_eq!(
        files_under(job.output_root()).len(),
        files_before,
        "no duplicates, no orphans"
    );
    assert!(
        !files_under(job.output_root())
            .keys()
            .any(|k| k.contains("_2.")),
        "no suffixed copies"
    );
}

#[test]
fn item5_a_user_edited_sidecar_is_neither_overwritten_nor_deleted() {
    let inputs = zips_in(&fixture_dir("small-2026b"));
    let e = env("sidecar");
    let mut o = options(&e.dest);
    o.overlay_mode = mr_job::OverlayMode::SaveSeparate;
    let mut job = Job::create(&e.state, &inputs, o).unwrap();
    run(&mut job);
    let rows: Vec<mr_job::ItemRow> = job
        .items()
        .unwrap()
        .into_iter()
        .filter(|r| r.overlay_output.is_some())
        .collect();
    let (edited, untouched) = (&rows[0], &rows[1]);
    for r in [edited, untouched] {
        assert!(
            r.output_hashes
                .contains_key(r.overlay_output.as_ref().unwrap()),
            "a sidecar's hash is recorded"
        );
    }
    let sidecar = layout::to_fs(job.output_root(), edited.overlay_output.as_ref().unwrap());
    let mut bytes = fs::read(&sidecar).unwrap();
    bytes.extend_from_slice(b"user edit");
    fs::write(&sidecar, &bytes).unwrap();

    // A rewrite under the same names (an item sent back to the queue).
    db_of(&job)
        .execute(
            "UPDATE item SET stage = 'pending' WHERE id IN (?1, ?2)",
            rusqlite::params![edited.key(), untouched.key()],
        )
        .unwrap();
    run(&mut job);
    assert_eq!(
        fs::read(&sidecar).unwrap(),
        bytes,
        "the user's edit is kept"
    );
    let e2 = job
        .items()
        .unwrap()
        .into_iter()
        .find(|r| r.key() == edited.key())
        .unwrap();
    assert_ne!(
        e2.overlay_output, edited.overlay_output,
        "the new sidecar took another name"
    );
    let u2 = job
        .items()
        .unwrap()
        .into_iter()
        .find(|r| r.key() == untouched.key())
        .unwrap();
    assert_eq!(
        u2.overlay_output, untouched.overlay_output,
        "our own unedited sidecar is reused"
    );

    // A rewrite under new names (the plan changed): the edited sidecar of
    // the earlier write is not ours any more and is left in place.
    db_of(&job)
        .execute(
            "UPDATE item SET stage = 'pending', previous_outputs = ?2, overlay_output = NULL WHERE id = ?1",
            rusqlite::params![
                edited.key(),
                serde_json::to_string(&vec![edited.overlay_output.clone().unwrap()]).unwrap()
            ],
        )
        .unwrap();
    run(&mut job);
    assert_eq!(fs::read(&sidecar).unwrap(), bytes, "not deleted either");
}
