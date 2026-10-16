//! FR-44 per install: the free tier counts the items of the install, not
//! of a job, so a new destination gives no new allowance
//! (`mr_job::free`).

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::*;
use mr_job::control::Stage;
use mr_job::free::{self, FREE_FILE, FreeSet, Seal};
use mr_job::{
    CollectingSink, Control, FREE_TIER_ITEMS, ItemOutcome, Job, Outcome, Progress, ProgressSink,
    RunStats,
};

fn free_options(dest: &Path) -> mr_job::Options {
    let mut o = options(dest);
    o.unlocked = false;
    o
}

fn completed(job: &mut Job) -> RunStats {
    match run(job).0 {
        Outcome::Completed(s) => s,
        other => panic!("{other:?}"),
    }
}

fn keys_in(job: &Job, stage: Stage) -> BTreeSet<String> {
    job.items()
        .unwrap()
        .into_iter()
        .filter(|i| i.stage == stage)
        .map(|i| i.plan.key)
        .collect()
}

/// The keys a dry run says the free tier would process, fitted to the
/// install's set as the app does.
fn preview(state: &Path, fixture: &str, dest: &Path) -> (usize, usize, BTreeSet<String>) {
    let o = free_options(dest);
    let mut p = mr_job::plan(&zips_in(&fixture_dir(fixture)), &o).unwrap();
    p.fit_free_tier(&o, &free::for_state(state));
    let keys = p.free_items.clone().unwrap();
    assert_eq!(keys.len(), p.totals.to_process);
    (p.totals.to_process, p.totals.skipped_free_limit, keys)
}

/// Items with media in `fixture` (fewer on machines without ffmpeg, whose
/// fixtures have no videos).
fn items_of(fixture: &str) -> usize {
    let o = free_options(&std::env::temp_dir());
    mr_job::plan(&zips_in(&fixture_dir(fixture)), &o)
        .unwrap()
        .items
        .len()
}

/// Pauses the run once `after` items have an outcome.
struct PauseAfter {
    control: Control,
    after: usize,
    seen: AtomicUsize,
}

impl ProgressSink for PauseAfter {
    fn progress(&self, _: &Progress) {}
    fn item(&self, _: &ItemOutcome) {
        if self.seen.fetch_add(1, Ordering::SeqCst) + 1 == self.after {
            self.control.pause();
        }
    }
    fn issue(&self, _: &str, _: &str) {}
}

/// The keychain, shared across "restarts" of the app.
#[derive(Default)]
struct MemSeal(Mutex<Option<String>>);

struct SealOf(Arc<MemSeal>);

impl Seal for SealOf {
    fn get(&self) -> Option<String> {
        self.0.0.lock().unwrap().clone()
    }
    fn set(&self, v: &str) -> Result<(), String> {
        *self.0.0.lock().unwrap() = Some(v.to_string());
        Ok(())
    }
}

#[test]
fn the_same_export_into_a_second_folder_gets_exactly_the_same_first_100() {
    let e = env("free-two-folders");
    let inputs = zips_in(&fixture_dir("free-limit"));
    let mut first = Job::create(&e.state, &inputs, free_options(&e.dest.join("one"))).unwrap();
    let s = completed(&mut first);
    assert_eq!((s.done, s.skipped_free_limit), (100, 150));
    let first_done = keys_in(&first, Stage::Done);

    // The preview of the second folder already says so.
    let (to_process, skipped, keys) = preview(&e.state, "free-limit", &e.dest.join("two"));
    assert_eq!((to_process, skipped), (100, 150));
    assert_eq!(keys, first_done, "the preview names the same 100");

    let mut second = Job::create(&e.state, &inputs, free_options(&e.dest.join("two"))).unwrap();
    assert_eq!(
        keys_in(&second, Stage::Pending),
        first_done,
        "staged before the run"
    );
    let s = completed(&mut second);
    assert_eq!((s.done, s.skipped_free_limit), (100, 150));
    assert_eq!(keys_in(&second, Stage::Done), first_done);
    assert_eq!(free::for_state(&e.state).used(), FREE_TIER_ITEMS);
    let text = fs::read_to_string(e.state.join(FREE_FILE)).unwrap();
    assert!(
        !first_done.iter().any(|k| text.contains(k.as_str())),
        "the set holds hashes, not item keys"
    );

    // Unlock and resume: the rest only, nothing redone, and licensed work
    // does not touch the free set.
    let boundary = second.journal().unwrap().last().unwrap().seq;
    assert_eq!(second.unlock().unwrap(), 150);
    let s = completed(&mut second);
    assert_eq!(
        (s.done, s.processed_this_run, s.skipped_free_limit),
        (250, 150, 0)
    );
    let restarted: BTreeSet<String> = second
        .journal()
        .unwrap()
        .into_iter()
        .filter(|ev| ev.seq > boundary && ev.event == "start")
        .map(|ev| ev.item)
        .collect();
    assert_eq!(restarted.len(), 150);
    assert!(
        restarted.is_disjoint(&first_done),
        "the first 100 were not redone"
    );
    assert_eq!(free::for_state(&e.state).used(), FREE_TIER_ITEMS);
}

#[test]
fn a_different_export_after_60_used_gets_the_40_left() {
    let e = env("free-sixty");
    let mut o = free_options(&e.dest.join("big"));
    o.workers = Some(1);
    let mut big = Job::create(&e.state, &zips_in(&fixture_dir("free-limit")), o).unwrap();
    let sink = PauseAfter {
        control: Control::new(),
        after: 60,
        seen: AtomicUsize::new(0),
    };
    let outcome = big.run(&sink, &sink.control).unwrap();
    assert!(matches!(outcome, Outcome::Paused(_)), "{outcome:?}");
    assert_eq!(keys_in(&big, Stage::Done).len(), 60);
    assert_eq!(free::for_state(&e.state).used(), 60);

    // multipart shares no memories with free-limit: the 40 places left.
    let n = items_of("multipart");
    assert!(n > 40, "{n}");
    let (to_process, skipped, keys) = preview(&e.state, "multipart", &e.dest.join("small"));
    assert_eq!((to_process, skipped), (40, n - 40));
    let mut small = Job::create(
        &e.state,
        &zips_in(&fixture_dir("multipart")),
        free_options(&e.dest.join("small")),
    )
    .unwrap();
    let s = completed(&mut small);
    assert_eq!((s.done, s.skipped_free_limit), (40, n - 40));
    assert_eq!(
        keys_in(&small, Stage::Done),
        keys,
        "the earliest 40, as previewed"
    );
    assert_eq!(free::for_state(&e.state).used(), 100);

    // The paused job resumes with nothing left to spend: its 60 stay done,
    // the rest are skipped, and nothing is redone.
    let s = completed(&mut big);
    assert_eq!(
        (s.done, s.processed_this_run, s.skipped_free_limit),
        (60, 0, 190)
    );
    // A third export gets nothing new.
    let (to_process, skipped, _) = preview(&e.state, "mixed-format", &e.dest.join("third"));
    assert_eq!((to_process, skipped), (0, items_of("mixed-format")));
}

#[test]
fn the_same_memories_from_a_newer_export_format_stay_free() {
    // small-2026a and small-2026b hold the same 50 memories (same file ids).
    let e = env("free-redownload");
    let mut a = Job::create(
        &e.state,
        &zips_in(&fixture_dir("small-2026a")),
        free_options(&e.dest.join("a")),
    )
    .unwrap();
    let n = items_of("small-2026a");
    assert_eq!(completed(&mut a).done, n);
    let mut big = Job::create(
        &e.state,
        &zips_in(&fixture_dir("free-limit")),
        free_options(&e.dest.join("big")),
    )
    .unwrap();
    assert_eq!(completed(&mut big).done, 100 - n, "the places left");
    let mut b = Job::create(
        &e.state,
        &zips_in(&fixture_dir("small-2026b")),
        free_options(&e.dest.join("b")),
    )
    .unwrap();
    let s = completed(&mut b);
    assert_eq!(
        (s.done, s.skipped_free_limit),
        (items_of("small-2026b"), 0),
        "already in the set"
    );
}

#[test]
fn deleting_the_store_gives_no_new_allowance_while_the_seal_stands() {
    let e = env("free-deleted");
    let seal = Arc::new(MemSeal::default());
    let install = || {
        free::install(
            &e.state,
            FreeSet::open(
                e.state.join(FREE_FILE),
                b"install secret",
                Some(Box::new(SealOf(seal.clone()))),
            ),
        )
    };
    install();
    let inputs = zips_in(&fixture_dir("free-limit"));
    let mut first = Job::create(&e.state, &inputs, free_options(&e.dest.join("one"))).unwrap();
    assert_eq!(completed(&mut first).done, 100);

    // Deleted, then the app restarts: the count stands; nothing is free to redo.
    fs::remove_file(e.state.join(FREE_FILE)).unwrap();
    install();
    let mut second = Job::create(&e.state, &inputs, free_options(&e.dest.join("two"))).unwrap();
    let s = completed(&mut second);
    assert_eq!((s.done, s.skipped_free_limit), (0, 250));

    // Unlocking still finishes it.
    assert_eq!(second.unlock().unwrap(), 250);
    let s = completed(&mut second);
    assert_eq!((s.done, s.skipped_free_limit), (250, 0));
}

#[test]
fn an_edited_store_without_its_seal_is_read_as_missing() {
    let e = env("free-edited");
    let install = || {
        free::install(
            &e.state,
            FreeSet::open(e.state.join(FREE_FILE), b"install secret", None),
        )
    };
    install();
    let small = zips_in(&fixture_dir("small-2026a"));
    let mut first = Job::create(&e.state, &small, free_options(&e.dest.join("one"))).unwrap();
    assert_eq!(completed(&mut first).done, items_of("small-2026a"));
    let path = e.state.join(FREE_FILE);
    let text = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        text.replacen("\"items\": [", "\"items\": [\"0000000000000000\", ", 1),
    )
    .unwrap();
    install();
    assert_eq!(
        free::for_state(&e.state).used(),
        0,
        "not provably ours: ignored"
    );
    let (to_process, _, _) = preview(&e.state, "free-limit", &e.dest.join("two"));
    assert_eq!(to_process, 100);
}

#[test]
fn two_jobs_running_at_once_share_one_allowance() {
    let e = env("free-parallel");
    free::install(&e.state, FreeSet::memory());
    let mut big = Job::create(
        &e.state,
        &zips_in(&fixture_dir("free-limit")),
        free_options(&e.dest.join("big")),
    )
    .unwrap();
    let mut small = Job::create(
        &e.state,
        &zips_in(&fixture_dir("small-2026a")),
        free_options(&e.dest.join("small")),
    )
    .unwrap();
    let n = items_of("small-2026a");
    // Both were staged against the same 100 places.
    assert_eq!(keys_in(&big, Stage::Pending).len(), 100);
    assert_eq!(keys_in(&small, Stage::Pending).len(), n);
    let (a, b) = std::thread::scope(|s| {
        let a = s.spawn(|| {
            big.run(&CollectingSink::default(), &Control::new())
                .unwrap()
        });
        let b = s.spawn(|| {
            small
                .run(&CollectingSink::default(), &Control::new())
                .unwrap()
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    let (Outcome::Completed(a), Outcome::Completed(b)) = (a, b) else {
        panic!("not completed")
    };
    assert_eq!(a.done + b.done, 100, "{} + {}", a.done, b.done);
    assert_eq!(a.done + a.skipped_free_limit, 250);
    assert_eq!(b.done + b.skipped_free_limit, n);
    assert_eq!(free::for_state(&e.state).used(), 100);
}
