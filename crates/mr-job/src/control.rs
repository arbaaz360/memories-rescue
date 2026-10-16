//! Pause, resume, cancel (FR-34) and progress reporting (FR-32).

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Run,
    Pause,
    Cancel,
}

/// Shared handle the UI uses to steer a running job. Clone it freely.
#[derive(Clone, Debug)]
pub struct Control {
    inner: Arc<(Mutex<Command>, Condvar)>,
}

impl Default for Control {
    fn default() -> Self {
        Self::new()
    }
}

impl Control {
    pub fn new() -> Control {
        Control {
            inner: Arc::new((Mutex::new(Command::Run), Condvar::new())),
        }
    }

    /// Finish the items in flight, then stop; `run` returns `Outcome::Paused`.
    pub fn pause(&self) {
        self.set(Command::Pause);
    }

    /// Clear a pause so that the next `run` proceeds.
    pub fn resume(&self) {
        self.set(Command::Run);
    }

    /// Finish the items in flight, then stop for good; completed output and
    /// job state are kept.
    pub fn cancel(&self) {
        self.set(Command::Cancel);
    }

    pub fn is_paused(&self) -> bool {
        *self.inner.0.lock().expect("control lock") == Command::Pause
    }

    pub fn is_cancelled(&self) -> bool {
        *self.inner.0.lock().expect("control lock") == Command::Cancel
    }

    /// `true` while dispatching may continue.
    pub fn may_dispatch(&self) -> bool {
        *self.inner.0.lock().expect("control lock") == Command::Run
    }

    fn set(&self, c: Command) {
        let (lock, cv) = &*self.inner;
        *lock.lock().expect("control lock") = c;
        cv.notify_all();
    }

    /// Block for up to `timeout` unless a command changes.
    pub fn wait(&self, timeout: Duration) {
        let (lock, cv) = &*self.inner;
        let guard = lock.lock().expect("control lock");
        let _ = cv.wait_timeout(guard, timeout).expect("control lock");
    }
}

/// Which stage an item is in (section 3.8 `item.stage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Pending,
    Extracted,
    Tagged,
    Composited,
    Written,
    Done,
    Failed,
    SkippedFreeLimit,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Pending => "pending",
            Stage::Extracted => "extracted",
            Stage::Tagged => "tagged",
            Stage::Composited => "composited",
            Stage::Written => "written",
            Stage::Done => "done",
            Stage::Failed => "failed",
            Stage::SkippedFreeLimit => "skipped_free_limit",
        }
    }

    pub fn parse(s: &str) -> Option<Stage> {
        Some(match s {
            "pending" => Stage::Pending,
            "extracted" => Stage::Extracted,
            "tagged" => Stage::Tagged,
            "composited" => Stage::Composited,
            "written" => Stage::Written,
            "done" => Stage::Done,
            "failed" => Stage::Failed,
            "skipped_free_limit" => Stage::SkippedFreeLimit,
            _ => return None,
        })
    }

    /// Needs (re)processing on the next run.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            Stage::Pending | Stage::Extracted | Stage::Tagged | Stage::Composited | Stage::Written
        )
    }
}

/// What one item ended up as, for the live list and the report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemOutcome {
    pub id: String,
    pub stage: Stage,
    pub media_type: Option<String>,
    pub output_path: Option<String>,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    pub seconds: f64,
}

/// A progress snapshot (FR-32), sent at least once per second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    /// 0.0 to 100.0.
    pub percent: f64,
    pub items_done: usize,
    pub items_failed: usize,
    pub items_total: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub stage: String,
    /// Items per second over the run so far.
    pub throughput: f64,
    /// Estimated seconds remaining; `None` until enough items have finished.
    pub eta_seconds: Option<f64>,
    /// The last few item outcomes, newest first.
    pub recent: Vec<ItemOutcome>,
    pub warnings: usize,
    pub failures: usize,
    pub elapsed_seconds: f64,
    pub sleep_prevented: bool,
}

/// Receives progress from a running job. Implement it on the UI side; the
/// engine calls it from its coordinator thread.
pub trait ProgressSink: Send + Sync {
    fn progress(&self, progress: &Progress);
    fn item(&self, outcome: &ItemOutcome);
    /// A non-fatal condition worth showing (an SRS section 8 code and text).
    fn issue(&self, code: &str, detail: &str);
}

/// A sink that keeps everything it is told, for tests and the CLI.
#[derive(Default, Debug)]
pub struct CollectingSink {
    pub progress: Mutex<Vec<Progress>>,
    pub items: Mutex<Vec<ItemOutcome>>,
    pub issues: Mutex<Vec<(String, String)>>,
}

impl ProgressSink for CollectingSink {
    fn progress(&self, p: &Progress) {
        self.progress.lock().expect("sink").push(p.clone());
    }

    fn item(&self, o: &ItemOutcome) {
        self.items.lock().expect("sink").push(o.clone());
    }

    fn issue(&self, code: &str, detail: &str) {
        self.issues
            .lock()
            .expect("sink")
            .push((code.to_string(), detail.to_string()));
    }
}

/// Exponentially weighted moving average of seconds per item, one per
/// media type (FR-32 "kept separately for photos and videos").
#[derive(Debug, Clone)]
pub struct Eta {
    photo: Option<f64>,
    video: Option<f64>,
    alpha: f64,
    samples: usize,
}

impl Default for Eta {
    fn default() -> Self {
        Eta {
            photo: None,
            video: None,
            alpha: 0.2,
            samples: 0,
        }
    }
}

impl Eta {
    pub fn record(&mut self, is_video: bool, seconds: f64) {
        let slot = if is_video {
            &mut self.video
        } else {
            &mut self.photo
        };
        *slot = Some(match *slot {
            None => seconds,
            Some(prev) => prev + self.alpha * (seconds - prev),
        });
        self.samples += 1;
    }

    /// Seconds remaining for the given counts of unfinished items, divided
    /// by the worker count; `None` before five samples.
    pub fn remaining(&self, photos_left: usize, videos_left: usize, workers: usize) -> Option<f64> {
        if self.samples < 5 {
            return None;
        }
        let per_photo = self.photo.or(self.video).unwrap_or(0.0);
        let per_video = self.video.or(self.photo).unwrap_or(0.0);
        let total = per_photo * photos_left as f64 + per_video * videos_left as f64;
        Some(total / workers.max(1) as f64)
    }
}

/// Ticks at most once per `interval`.
pub struct Ticker {
    last: Instant,
    interval: Duration,
}

impl Ticker {
    pub fn new(interval: Duration) -> Ticker {
        Ticker {
            last: Instant::now() - interval,
            interval,
        }
    }

    pub fn due(&mut self) -> bool {
        if self.last.elapsed() >= self.interval {
            self.last = Instant::now();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_transitions() {
        let c = Control::new();
        assert!(c.may_dispatch());
        c.pause();
        assert!(c.is_paused() && !c.may_dispatch());
        c.resume();
        assert!(c.may_dispatch());
        c.cancel();
        assert!(c.is_cancelled());
        let c2 = c.clone();
        assert!(c2.is_cancelled(), "clones share state");
    }

    #[test]
    fn eta_is_separate_per_type_and_needs_samples() {
        let mut eta = Eta::default();
        for _ in 0..4 {
            eta.record(false, 0.1);
        }
        assert!(eta.remaining(10, 0, 1).is_none());
        eta.record(true, 2.0);
        let r = eta.remaining(10, 2, 2).unwrap();
        assert!((r - (0.1 * 10.0 + 2.0 * 2.0) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn stage_round_trip() {
        for s in [
            Stage::Pending,
            Stage::Extracted,
            Stage::Tagged,
            Stage::Composited,
            Stage::Written,
            Stage::Done,
            Stage::Failed,
            Stage::SkippedFreeLimit,
        ] {
            assert_eq!(Stage::parse(s.as_str()), Some(s));
        }
        assert!(Stage::Extracted.is_open());
        assert!(!Stage::Done.is_open());
        assert!(!Stage::SkippedFreeLimit.is_open());
    }
}
