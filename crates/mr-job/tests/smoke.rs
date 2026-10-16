mod common;

use common::*;
use mr_job::{Job, Outcome};

#[test]
fn smoke_small_2026b_end_to_end() {
    let e = env("smoke");
    let dir = fixture_dir("small-2026b");
    let mut job = Job::create(&e.state, &zips_in(&dir), options(&e.dest)).unwrap();
    let (outcome, sink) = run(&mut job);
    println!("outcome: {outcome:?}");
    for (c, d) in sink.issues.lock().unwrap().iter() {
        println!("issue {c}: {d}");
    }
    for o in sink
        .items
        .lock()
        .unwrap()
        .iter()
        .filter(|o| o.error_code.is_some())
    {
        println!(
            "item {} {:?} {:?} {:?}",
            o.id, o.stage, o.error_code, o.error_detail
        );
    }
    let files = files_under(job.output_root());
    println!(
        "{} files, e.g. {:?}",
        files.len(),
        files.keys().take(5).collect::<Vec<_>>()
    );
    assert!(matches!(outcome, Outcome::Completed(_)));
    let report = job.report().unwrap();
    println!("report: {}", report.html.display());
}
