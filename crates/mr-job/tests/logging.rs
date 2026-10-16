//! FR-41 engine side: the engine emits `tracing` events at its lifecycle
//! points, and nothing in them carries the home directory, a URL or a query
//! string. One test per binary, because it installs a global subscriber
//! (worker threads do not see a thread-local one).

mod common;

use std::sync::{Arc, Mutex};

use common::*;
use mr_job::Job;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

#[derive(Debug, Clone)]
struct Captured {
    level: Level,
    target: String,
    message: String,
    fields: Vec<(String, String)>,
}

impl Captured {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn text(&self) -> String {
        let mut s = format!("{} {} {}", self.level, self.target, self.message);
        for (k, v) in &self.fields {
            s.push_str(&format!(" {k}={v}"));
        }
        s
    }
}

#[derive(Default)]
struct Visitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl Visit for Visitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let v = format!("{value:?}");
        if field.name() == "message" {
            self.message = v;
        } else {
            self.fields.push((field.name().to_string(), v));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Captured>>>);

impl Subscriber for Capture {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut v = Visitor::default();
        event.record(&mut v);
        self.0.lock().unwrap().push(Captured {
            level: *event.metadata().level(),
            target: event.metadata().target().to_string(),
            message: v.message,
            fields: v.fields,
        });
    }
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}

#[test]
fn fr41_engine_events_are_emitted_and_redacted() {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(capture.clone()).expect("first subscriber");

    // F-15 carries sid/mid URLs in its JSON; two items fail on purpose.
    let t = truth("small-2026a");
    let ids: Vec<String> = t["items"].as_array().unwrap()[..2]
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    let inputs = zips_in(&fixture_dir("small-2026a"));
    let e = env("logging");
    let mut o = options(&e.dest);
    o.test_hooks.fail_ids = ids.clone();
    let mut job = Job::create(&e.state, &inputs, o).unwrap();
    let (_outcome, _sink) = run(&mut job);
    job.report().unwrap();
    // A second run of a finished job, and a part that does not exist.
    let (_outcome, _sink) = run(&mut job);
    assert!(job.add_part(&e.dest.join("no-such-part.zip")).is_err());

    let events = capture.0.lock().unwrap().clone();
    if std::env::var_os("MR_LOG_DUMP").is_some() {
        for c in &events {
            eprintln!("{}", c.text());
        }
    }
    let messages: Vec<&str> = events.iter().map(|c| c.message.as_str()).collect();
    for want in [
        "inputs discovered",
        "memories_history.json read",
        "plan",
        "job created",
        "run started",
        "progress",
        "item failed",
        "run finished",
        "report written",
    ] {
        assert!(
            messages.contains(&want),
            "no {want:?} event in {messages:?}"
        );
    }
    let failed: Vec<&Captured> = events
        .iter()
        .filter(|c| c.message == "item failed")
        .collect();
    if cfg!(debug_assertions) {
        assert_eq!(failed.len(), ids.len(), "one event per injected failure");
        assert!(failed.iter().all(|c| c.level == Level::WARN));
        assert!(
            failed
                .iter()
                .all(|c| c.field("code") == Some("TEST_INJECTED"))
        );
    }
    let finished: Vec<&Captured> = events
        .iter()
        .filter(|c| c.message == "run finished")
        .collect();
    assert_eq!(finished.len(), 2, "one per run");
    assert!(
        events.iter().all(|c| c.target.starts_with("mr_")),
        "engine targets only"
    );

    // Redaction: no home directory, no URL, no query string anywhere.
    let home = mr_core::redact::home().map(|h| h.to_string_lossy().into_owned());
    for c in &events {
        let text = c.text();
        let lower = text.to_lowercase();
        if let Some(h) = &home {
            let h = h.to_lowercase();
            assert!(
                !lower.contains(&h) && !lower.contains(&h.replace('\\', "/")),
                "home directory in {text}"
            );
        }
        assert!(!lower.contains("://"), "URL in {text}");
        for q in ["sid=", "mid=", "sig=", "expires=", "signature="] {
            assert!(!lower.contains(q), "query string in {text}");
        }
    }
    // FR-41 diagnostics: counts and codes, paths redacted, no item names.
    let diag = job.diagnostics().unwrap();
    let text = diag.to_string();
    assert_eq!(diag["job"], job.id());
    assert_eq!(diag["error_codes"]["TEST_INJECTED"], 2, "{text}");
    assert!(diag["counts"]["done"].as_u64().unwrap() > 0);
    if let Some(h) = mr_core::redact::home() {
        let h = h.to_string_lossy().to_lowercase().replace('\\', "\\\\");
        assert!(
            !text.to_lowercase().contains(&h),
            "home directory in diagnostics: {text}"
        );
    }
    for id in &ids {
        assert!(!text.contains(id.as_str()), "item id in diagnostics");
    }
    assert!(!text.contains("example.invalid"), "no JSON content");

    // Where the output folder is under the home directory (the temp folder
    // is, on Windows), its path is logged with `~`.
    let created = events.iter().find(|c| c.message == "job created").unwrap();
    let dest = created.field("dest").unwrap();
    assert!(dest.ends_with("Memories Rescue"), "{dest}");
    if let Some(h) = mr_core::redact::home()
        && e.dest.starts_with(h)
    {
        assert!(dest.starts_with('~'), "{dest}");
    }
}
