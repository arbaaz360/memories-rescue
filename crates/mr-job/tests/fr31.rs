//! FR-31 ZIP output: one archive or volumes under a size, every archive
//! read back by a second reader (the `zip` crate) and compared byte for byte
//! with the tree, the FAT32 preset and oversize files, interrupted packs at
//! every recorded state, pause during a pack, a changed tree packed again
//! next to someone else's file, and the FR-28 estimate and low space.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use common::*;
use mr_job::{
    CollectingSink, Control, ItemOutcome, Job, Outcome, Progress, ProgressSink, ZIP_VOLUME_FAT32,
    ZipVolume, layout, plan,
};
use sha2::{Digest, Sha256};

fn sha(bytes: &[u8]) -> String {
    mr_job::fingerprint::hex(&Sha256::digest(bytes))
}

/// The packable tree: every file under the output root except the report
/// and the job's state folder, keyed `Memories Rescue/<rel>`.
fn tree(job: &Job) -> BTreeMap<String, PathBuf> {
    files_under(job.output_root())
        .into_iter()
        .filter(|(rel, _)| !rel.starts_with(layout::REPORT_DIR))
        .map(|(rel, p)| (format!("{}/{rel}", layout::ROOT_DIR), p))
        .collect()
}

/// Every entry of an archive, read with the `zip` crate: name -> (bytes, stored).
fn entries(path: &Path) -> BTreeMap<String, (Vec<u8>, bool)> {
    let mut a = zip::ZipArchive::new(fs::File::open(path).unwrap()).unwrap();
    let mut out = BTreeMap::new();
    for i in 0..a.len() {
        let mut f = a.by_index(i).unwrap();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        let stored = f.compression() == zip::CompressionMethod::Stored;
        let name = f.name().unwrap().to_string();
        assert!(
            out.insert(name.clone(), (buf, stored)).is_none(),
            "duplicate {name}"
        );
    }
    out
}

fn zips_in_dest(dest: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dest)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".zip") || n.ends_with(".partial"))
        .collect();
    v.sort();
    v
}

fn zip_job(fixture: &str, tag: &str, limit: Option<u64>) -> (Env, Job) {
    let e = env(tag);
    let mut o = options(&e.dest);
    o.zip_output = true;
    o.zip_volume_bytes = limit;
    let job = Job::create(&e.state, &zips_in(&fixture_dir(fixture)), o).unwrap();
    (e, job)
}

fn completed(job: &mut Job) {
    let (outcome, sink) = run(job);
    assert!(
        matches!(outcome, Outcome::Completed(_)),
        "{outcome:?} {:?}",
        sink.issues.lock().unwrap()
    );
}

/// Every volume done, on disk at its recorded size, and the union of their
/// entries equal to the tree byte for byte, each stored once.
fn check_pack(job: &Job, dest: &Path) -> Vec<ZipVolume> {
    let vols = job.zip_volumes().unwrap();
    assert!(!vols.is_empty());
    let mut union: BTreeMap<String, (Vec<u8>, bool)> = BTreeMap::new();
    for v in &vols {
        assert_eq!(v.state, "done", "{v:?}");
        let path = dest.join(&v.name);
        assert_eq!(fs::metadata(&path).unwrap().len(), v.size, "{}", v.name);
        let e = entries(&path);
        assert_eq!(e.len(), v.files, "{}", v.name);
        for (k, val) in e {
            assert!(
                union.insert(k.clone(), val).is_none(),
                "{k} in two archives"
            );
        }
    }
    let t = tree(job);
    assert_eq!(
        union.keys().collect::<Vec<_>>(),
        t.keys().collect::<Vec<_>>(),
        "archives hold exactly the tree"
    );
    for (k, p) in &t {
        let (bytes, stored) = &union[k];
        assert!(*stored, "{k} stored");
        assert_eq!(sha(bytes), sha(&fs::read(p).unwrap()), "{k} bytes");
    }
    assert!(
        !zips_in_dest(dest).iter().any(|n| n.ends_with(".partial")),
        "no partial archive left"
    );
    vols
}

#[test]
fn fr31_one_archive_holds_exactly_the_tree_and_a_rerun_writes_nothing() {
    let (e, mut job) = zip_job("small-2026b", "zip-one", None);
    completed(&mut job);
    let vols = check_pack(&job, &e.dest);
    assert_eq!(vols.len(), 1);
    assert_eq!(vols[0].name, "Memories Rescue.zip");
    assert_eq!(zips_in_dest(&e.dest), vec!["Memories Rescue.zip"]);
    // The report folder and the job's state are not packed.
    let names = entries(&e.dest.join("Memories Rescue.zip"));
    assert!(
        names
            .keys()
            .all(|n| !n.contains("/_report/") && !n.contains(".memories-rescue"))
    );
    let notes = job.report_data().unwrap().notes;
    assert!(
        notes
            .iter()
            .any(|n| n.starts_with("Packed into 1 ZIP file ")),
        "{notes:?}"
    );
    let before = fs::metadata(e.dest.join("Memories Rescue.zip"))
        .unwrap()
        .modified()
        .unwrap();
    completed(&mut job);
    let after = fs::metadata(e.dest.join("Memories Rescue.zip"))
        .unwrap()
        .modified()
        .unwrap();
    assert_eq!(before, after, "a finished pack is not written again");
}

#[test]
fn fr31_volumes_stay_at_or_under_the_size() {
    let limit = 1 << 20;
    let (e, mut job) = zip_job("free-limit", "zip-vol", Some(limit));
    completed(&mut job);
    let vols = check_pack(&job, &e.dest);
    assert!(vols.len() >= 3, "{} volumes", vols.len());
    for (i, v) in vols.iter().enumerate() {
        assert_eq!(v.seq, i + 1);
        assert_eq!(v.name, format!("Memories Rescue-{:03}.zip", i + 1));
        assert!(v.size <= limit, "{} is {} bytes", v.name, v.size);
        assert!(!v.oversize);
    }
}

#[test]
fn fr31_fat32_preset_and_a_file_larger_than_the_volume() {
    assert_eq!(ZIP_VOLUME_FAT32, 4_294_967_295, "the largest FAT32 file");
    let largest = tree_largest("small-2026b");
    let limit = largest / 2;
    let (e, mut job) = zip_job("small-2026b", "zip-over", Some(limit));
    completed(&mut job);
    let vols = check_pack(&job, &e.dest);
    let over: Vec<&ZipVolume> = vols.iter().filter(|v| v.oversize).collect();
    assert!(!over.is_empty());
    for v in &vols {
        if v.oversize {
            assert_eq!(v.files, 1, "an oversize file is alone");
        } else {
            assert!(v.size <= limit, "{v:?}");
        }
    }
    let notes = job.report_data().unwrap().notes;
    assert!(
        notes
            .iter()
            .any(|n| n.contains("larger than the volume size")),
        "{notes:?}"
    );
}

fn tree_largest(fixture: &str) -> u64 {
    let (_e, mut job) = {
        let e = env("zip-probe");
        let job = Job::create(&e.state, &zips_in(&fixture_dir(fixture)), options(&e.dest)).unwrap();
        (e, job)
    };
    completed(&mut job);
    tree(&job)
        .values()
        .map(|p| fs::metadata(p).unwrap().len())
        .max()
        .unwrap()
}

#[test]
fn fr31_an_interrupted_pack_resumes_from_every_recorded_state() {
    let (e, mut job) = zip_job("small-2026b", "zip-resume", Some(300_000));
    completed(&mut job);
    let vols = check_pack(&job, &e.dest);
    assert!(vols.len() >= 4, "{} volumes", vols.len());
    let mtime = |n: &str| fs::metadata(e.dest.join(n)).unwrap().modified().unwrap();
    let before: Vec<_> = vols.iter().map(|v| mtime(&v.name)).collect();
    let db = rusqlite::Connection::open(job.state_dir_of_job().join("job.sqlite")).unwrap();
    // 2: failed (its file gone). 3: verified, killed before the rename.
    // 4: never recorded, with a stale partial left behind.
    fs::remove_file(e.dest.join(&vols[1].name)).unwrap();
    db.execute("UPDATE zip_volume SET state = 'failed' WHERE seq = 2", [])
        .unwrap();
    let v3 = e.dest.join(&vols[2].name);
    fs::rename(&v3, mr_job::zipout::partial_of(&v3)).unwrap();
    db.execute("UPDATE zip_volume SET state = 'verified' WHERE seq = 3", [])
        .unwrap();
    let v4 = e.dest.join(&vols[3].name);
    fs::remove_file(&v4).unwrap();
    fs::write(mr_job::zipout::partial_of(&v4), b"half an archive").unwrap();
    db.execute("DELETE FROM zip_volume WHERE seq = 4", [])
        .unwrap();
    drop(db);
    std::thread::sleep(std::time::Duration::from_millis(1100));

    completed(&mut job);
    let again = check_pack(&job, &e.dest);
    assert_eq!(again.len(), vols.len());
    for (i, v) in again.iter().enumerate() {
        let rewritten = mtime(&v.name) != before[i];
        assert_eq!(
            rewritten,
            i == 1 || i == 3,
            "volume {} rewritten: {rewritten}",
            i + 1
        );
    }
}

/// Pauses the job at the first progress report of the ZIP stage.
struct PauseWhilePacking {
    control: Control,
    inner: CollectingSink,
}

impl ProgressSink for PauseWhilePacking {
    fn progress(&self, p: &Progress) {
        if p.stage.starts_with("packing ZIP") {
            self.control.pause();
        }
        self.inner.progress(p);
    }
    fn item(&self, o: &ItemOutcome) {
        self.inner.item(o);
    }
    fn issue(&self, c: &str, d: &str) {
        self.inner.issue(c, d);
    }
}

#[test]
fn fr31_pause_during_a_pack_then_resume() {
    if !cfg!(debug_assertions) {
        return; // the delay hook is debug-only
    }
    let e = env("zip-pause");
    let mut o = options(&e.dest);
    o.zip_output = true;
    o.zip_volume_bytes = Some(400_000);
    o.test_hooks.zip_delay_ms = 60;
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("small-2026b")), o.clone()).unwrap();
    let control = Control::new();
    let sink = PauseWhilePacking {
        control: control.clone(),
        inner: CollectingSink::default(),
    };
    let outcome = job.run(&sink, &control).unwrap();
    assert!(matches!(outcome, Outcome::Paused(_)), "{outcome:?}");
    assert!(
        !zips_in_dest(&e.dest)
            .iter()
            .any(|n| n.ends_with(".partial")),
        "the archive being written is removed on pause"
    );
    let done_before = job
        .zip_volumes()
        .unwrap()
        .iter()
        .filter(|v| v.state == "done")
        .count();
    o.test_hooks.zip_delay_ms = 0;
    job.set_options(o).unwrap();
    completed(&mut job);
    let total = check_pack(&job, &e.dest).len();
    assert!(
        done_before < total,
        "paused part-way: {done_before} of {total}"
    );
}

#[test]
fn fr31_a_changed_tree_is_packed_again_beside_someone_elses_file() {
    let t = truth("small-2026b");
    let ids: Vec<String> = t["items"].as_array().unwrap()[..2]
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    let e = env("zip-change");
    let foreign = e.dest.join("Memories Rescue.zip");
    fs::write(&foreign, b"not ours").unwrap();
    let mut o = options(&e.dest);
    o.zip_output = true;
    o.test_hooks.fail_ids = ids;
    let mut job = Job::create(&e.state, &zips_in(&fixture_dir("small-2026b")), o.clone()).unwrap();
    completed(&mut job);
    let first = check_pack(&job, &e.dest);
    assert_eq!(
        first[0].name, "Memories Rescue_2.zip",
        "the taken name gets a suffix"
    );
    let files_first = first[0].files;

    o.test_hooks.fail_ids.clear();
    job.retry_failed(Some(o)).unwrap();
    completed(&mut job);
    let second = check_pack(&job, &e.dest);
    assert_eq!(second.len(), 1);
    assert!(
        second[0].files > files_first,
        "the retried items are in the new archive"
    );
    assert_eq!(
        fs::read(&foreign).unwrap(),
        b"not ours",
        "someone else's file is untouched"
    );
    assert_eq!(
        zips_in_dest(&e.dest),
        vec!["Memories Rescue.zip", "Memories Rescue_2.zip"],
        "the earlier archive was replaced, not kept beside the new one"
    );
}

#[test]
fn fr28_the_estimate_counts_the_zip_and_low_space_stops_the_pack() {
    let inputs = zips_in(&fixture_dir("small-2026b"));
    let e = env("zip-space");
    let mut o = options(&e.dest);
    let without = plan(&inputs, &o).unwrap().space;
    o.zip_output = true;
    let with = plan(&inputs, &o).unwrap().space;
    assert_eq!(without.zip_bytes, 0);
    assert!(
        with.zip_bytes >= with.final_bytes + with.originals_bytes,
        "{with:?}"
    );
    assert!(with.required_bytes > without.required_bytes);
    if !cfg!(debug_assertions) {
        return; // the free-space hook is debug-only
    }
    // Precheck (FR-28 AC1): enough for the files, not for the files and the ZIP.
    o.test_hooks.fake_free_bytes = Some(without.threshold_bytes);
    let mut job = Job::create(&e.state, &inputs, o.clone()).unwrap();
    let (outcome, _) = run(&mut job);
    let Outcome::InsufficientSpace(est) = &outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        est.options
            .iter()
            .any(|x| x.action == mr_job::SpaceAction::DisableZip)
    );
    // Items written, then the pack meets a nearly full drive: it stops
    // before writing anything (FR-28 AC2) and finishes once there is room.
    o.test_hooks.fake_free_bytes = None;
    o.zip_output = false;
    job.set_options(o.clone()).unwrap();
    completed(&mut job);
    o.zip_output = true;
    o.test_hooks.fake_free_bytes = Some(1 << 30);
    o.ignore_space_check = true;
    job.set_options(o.clone()).unwrap();
    let (outcome, _) = run(&mut job);
    assert!(
        matches!(&outcome, Outcome::Blocked { code, .. } if code == "DISK_FULL_IMMINENT"),
        "{outcome:?}"
    );
    assert!(
        zips_in_dest(&e.dest).is_empty(),
        "nothing written while space is short"
    );
    o.test_hooks.fake_free_bytes = None;
    o.ignore_space_check = false;
    job.set_options(o).unwrap();
    completed(&mut job);
    check_pack(&job, &e.dest);
}

#[test]
fn fr31_unicode_long_destination() {
    let base = scratch("zip-unicode");
    let mut dest = base.join("Fotos \u{2013} Caf\u{e9} \u{2615} \u{5199}\u{771f}");
    while dest.to_string_lossy().len() < 230 {
        dest = dest.join("tr\u{e8}s-long-dossier-de-sauvegarde");
    }
    fs::create_dir_all(&dest).unwrap();
    let mut o = options(&dest);
    o.zip_output = true;
    let mut job = Job::create(
        &base.join("state"),
        &zips_in(&fixture_dir("unicode-paths")),
        o,
    )
    .unwrap();
    completed(&mut job);
    check_pack(&job, &dest);
}
