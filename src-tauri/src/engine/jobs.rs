//! Jobs (FR-32 to FR-40, FR-44): `start`, `pause`, `resume`, `cancel`,
//! `retry_failed`, `add_part`, `get_report`, `list_resumable_jobs`.
//!
//! Each run happens on its own thread, which opens the job, runs
//! `mr_job::Job::run` with a sink that turns engine progress into the
//! contract's events, and maps the outcome onto a `job_state`. A `Job` is not
//! `Sync`, so commands that read state open a second handle (SQLite WAL).
//!
//! Free tier: mr-job marks everything after the first 100 items
//! `skipped_free_limit` up front and a run completes after those 100. The
//! first time that happens the job reports `free_limit_reached` and waits;
//! `resume` then unlocks and continues (licence present) or finishes the job
//! as `partial` (no licence), as the contract describes.
//!
//! Every run uses the engine's compositor (`Job::set_compositor`). A job
//! that earlier finished items without burning their captions in (no
//! sidecar then, or an older version) has them queued again on its next run
//! (`Job::requeue_composite_pending`), so resuming it finishes the captions.
//! Joined clips (FR-27) are reported per group in `JobReport::clips`; their
//! `clip:<group id>` outcomes from mr-job are not items and are not sent as
//! `item` events.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use mr_job::{Job, JobError, Outcome};

use crate::ipc::{
    ClipOutcome, ClipStatus, ErrorCode, InputFile, IpcError, IpcResult, Issue, ItemEvent,
    ItemOutcome, ItemStatus, JobOptions, JobReport, JobState, JobStateEvent, MediaType,
    ProgressEvent, ResumableJob, Stage, StatusCounts, WarningEvent, events,
};

use super::analysis::media_type;
use super::{Engine, file_name, job_error, now_utc, redact_home, settings};

pub struct JobHandle {
    pub id: String,
    control: Mutex<mr_job::Control>,
    state: Mutex<JobState>,
    running: AtomicBool,
    awaiting_unlock: AtomicBool,
    free_limit_ack: AtomicBool,
    error: Mutex<Option<IpcError>>,
    finished_at: Mutex<Option<String>>,
}

impl JobHandle {
    fn new(id: &str, state: JobState) -> JobHandle {
        JobHandle {
            id: id.to_string(),
            control: Mutex::new(mr_job::Control::new()),
            state: Mutex::new(state),
            running: AtomicBool::new(false),
            awaiting_unlock: AtomicBool::new(false),
            free_limit_ack: AtomicBool::new(false),
            error: Mutex::new(None),
            finished_at: Mutex::new(None),
        }
    }

    pub fn state(&self) -> JobState {
        *self.state.lock().expect("state")
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

enum Prep {
    None,
    Unlock,
    Retry(Option<Box<mr_job::Options>>),
}

fn is_terminal(s: JobState) -> bool {
    matches!(
        s,
        JobState::Done | JobState::Partial | JobState::Failed | JobState::Cancelled
    )
}

impl Engine {
    fn set_state(&self, h: &JobHandle, state: JobState, error: Option<IpcError>) {
        *h.state.lock().expect("state") = state;
        *h.error.lock().expect("error") = error.clone();
        if is_terminal(state) {
            *h.finished_at.lock().expect("finished") = Some(now_utc());
        } else {
            *h.finished_at.lock().expect("finished") = None;
        }
        self.emit(
            events::JOB_STATE,
            &JobStateEvent {
                job_id: h.id.clone(),
                state,
                error,
                at: now_utc(),
            },
        );
    }

    fn handle(&self, job_id: &str) -> IpcResult<Arc<JobHandle>> {
        if let Some(h) = self.jobs.lock().expect("jobs").get(job_id) {
            return Ok(h.clone());
        }
        // A job from an earlier session (Resume previous job).
        let known = mr_job::list_jobs(self.state_dir())
            .into_iter()
            .any(|j| j.id == job_id);
        if !known {
            return Err(
                IpcError::unknown("That job no longer exists.").with_detail(job_id.to_string())
            );
        }
        let h = Arc::new(JobHandle::new(job_id, JobState::Paused));
        self.jobs
            .lock()
            .expect("jobs")
            .insert(job_id.to_string(), h.clone());
        Ok(h)
    }

    fn open(&self, job_id: &str) -> IpcResult<Job> {
        Job::open(self.state_dir(), job_id).map_err(job_error)
    }

    /// True while any job is processing; updates never install then (§10.2).
    pub fn any_job_running(&self) -> bool {
        self.jobs
            .lock()
            .expect("jobs")
            .values()
            .any(|h| h.is_running())
    }

    /// Items an earlier run finished with the caption beside them, queued
    /// again now that captions can be burned in. Videos are only queued
    /// when this computer can encode them (else they would be declined
    /// again on every run); photos never need ffmpeg.
    fn requeue_captions(&self, job: &mut Job) -> IpcResult<usize> {
        let Some(media) = &self.media else {
            return Ok(0);
        };
        if !job.options().overlay_mode.burns_in() {
            return Ok(0);
        }
        let rows = job.items().map_err(job_error)?;
        let pending: Vec<&mr_job::ItemRow> = rows
            .iter()
            .filter(|r| r.stage == mr_job::control::Stage::Done && r.composite_pending)
            .collect();
        if pending.is_empty() {
            return Ok(0);
        }
        let photos = pending
            .iter()
            .any(|r| r.plan.kind == mr_job::MediaKind::Photo);
        if !photos && media.encoder_plan().is_err() {
            return Ok(0);
        }
        let n = job.requeue_composite_pending().map_err(job_error)?;
        tracing::info!("{n} finished items queued again to burn their captions in");
        Ok(n)
    }

    fn engine_options(&self, o: &JobOptions) -> mr_job::Options {
        let s = self.get_settings();
        settings::to_engine(o, &s.output_template, self.license.unlocked())
    }

    pub fn start(
        self: &Arc<Self>,
        input_ids: &[String],
        options: &JobOptions,
    ) -> IpcResult<String> {
        let inputs = self.input_paths(input_ids)?;
        let opts = self.engine_options(options);
        let id = match Job::create(self.state_dir(), &inputs, opts.clone()) {
            Ok(job) => job.id().to_string(),
            // The folder already holds a job: continue that library with these inputs.
            Err(JobError::OutputInUse { job_id }) => {
                let mut job = self.open(&job_id)?;
                let have: Vec<PathBuf> = job.inputs().to_vec();
                for p in &inputs {
                    let abs = mr_job::layout::absolute(p);
                    if !have.contains(&abs) {
                        job.add_part(&abs).map_err(job_error)?;
                    }
                }
                job.set_options(opts).map_err(job_error)?;
                job_id
            }
            Err(e) => return Err(job_error(e)),
        };
        self.remember_destination(&options.destination);
        let h = Arc::new(JobHandle::new(&id, JobState::Running));
        self.jobs
            .lock()
            .expect("jobs")
            .insert(id.clone(), h.clone());
        self.spawn_run(h, Prep::None);
        Ok(id)
    }

    pub fn pause(&self, job_id: &str) -> IpcResult<()> {
        let h = self.handle(job_id)?;
        if h.is_running() {
            h.control.lock().expect("control").pause();
        }
        Ok(())
    }

    pub fn cancel(&self, job_id: &str) -> IpcResult<()> {
        let h = self.handle(job_id)?;
        if h.is_running() {
            h.control.lock().expect("control").cancel();
        } else if !is_terminal(h.state()) {
            h.awaiting_unlock.store(false, Ordering::SeqCst);
            if let Ok(job) = self.open(job_id) {
                let _ = job.report();
            }
            self.set_state(&h, JobState::Cancelled, None);
        }
        Ok(())
    }

    pub fn resume(self: &Arc<Self>, job_id: &str) -> IpcResult<()> {
        let h = self.handle(job_id)?;
        if h.is_running() {
            h.control.lock().expect("control").resume();
            return Ok(());
        }
        if h.awaiting_unlock.swap(false, Ordering::SeqCst) {
            if self.license.unlocked() {
                self.spawn_run(h, Prep::Unlock);
            } else {
                // "Later": the rest stay skipped_free_limit and the job ends partial.
                h.free_limit_ack.store(true, Ordering::SeqCst);
                if let Ok(job) = self.open(job_id) {
                    let _ = job.report();
                }
                self.set_state(&h, JobState::Partial, None);
            }
            return Ok(());
        }
        self.spawn_run(h, Prep::None);
        Ok(())
    }

    pub fn retry_failed(
        self: &Arc<Self>,
        job_id: &str,
        options: Option<&JobOptions>,
    ) -> IpcResult<()> {
        let h = self.handle(job_id)?;
        if h.is_running() {
            return Err(
                IpcError::unknown("This job is already running.").with_detail(job_id.to_string())
            );
        }
        let opts = options.map(|o| Box::new(self.engine_options(o)));
        self.spawn_run(h, Prep::Retry(opts));
        Ok(())
    }

    pub fn add_part(&self, job_id: &str, path: &str) -> IpcResult<InputFile> {
        let h = self.handle(job_id)?;
        if h.is_running() {
            return Err(
                IpcError::unknown("Add the part when the job is not running.")
                    .with_detail(job_id.to_string()),
            );
        }
        let p = PathBuf::from(path);
        let probe = mr_core::discover(std::slice::from_ref(&p));
        if probe.parts.is_empty() {
            return Err(IpcError::new(
                ErrorCode::InputNotExport,
                "This file isn't a part of a Snapchat export.",
            )
            .with_detail(file_name(&p)));
        }
        let mut job = self.open(job_id)?;
        job.add_part(&p).map_err(job_error)?;
        Ok(self
            .input_by_path(&p)
            .unwrap_or_else(|| self.register_input(&p)))
    }

    fn spawn_run(self: &Arc<Self>, h: Arc<JobHandle>, prep: Prep) {
        if h.running.swap(true, Ordering::SeqCst) {
            return;
        }
        *h.control.lock().expect("control") = mr_job::Control::new();
        self.set_state(&h, JobState::Running, None);
        let engine = Arc::clone(self);
        std::thread::Builder::new()
            .name(format!("job-{}", &h.id[..8.min(h.id.len())]))
            .spawn(move || {
                let result = engine.run_job(&h, prep);
                h.running.store(false, Ordering::SeqCst);
                match result {
                    Ok((state, error)) => engine.set_state(&h, state, error),
                    Err(e) => engine.set_state(&h, JobState::Failed, Some(e)),
                }
            })
            .expect("spawn job thread");
    }

    fn run_job(&self, h: &JobHandle, prep: Prep) -> IpcResult<(JobState, Option<IpcError>)> {
        let mut job = self.open(&h.id)?;
        job.set_compositor(self.compositor.clone());
        // Debug builds: hooks may change between runs (inject, then remove a fault).
        if cfg!(debug_assertions) {
            let hooks = super::test_hooks();
            if job.options().test_hooks != hooks {
                let mut o = job.options().clone();
                o.test_hooks = hooks;
                job.set_options(o).map_err(job_error)?;
            }
        }
        self.requeue_captions(&mut job)?;
        match prep {
            Prep::None => {}
            Prep::Unlock => {
                job.unlock().map_err(job_error)?;
            }
            Prep::Retry(o) => {
                job.retry_failed(o.map(|b| *b)).map_err(job_error)?;
            }
        }
        // A licence activated since the job started covers its skipped items too.
        if self.license.unlocked() && !job.options().unlocked {
            job.unlock().map_err(job_error)?;
        }
        if let Some(why) = job.rebuilt() {
            self.emit(
                events::WARNING,
                &WarningEvent {
                    job_id: Some(h.id.clone()),
                    code: ErrorCode::JobStateCorrupt,
                    message: why.to_string(),
                    count: None,
                },
            );
        }
        let rows = job.items().map_err(job_error)?;
        // Items already saved with the date only or without a location (earlier runs).
        let earlier_warnings = rows
            .iter()
            .filter(|r| {
                r.stage == mr_job::control::Stage::Done
                    && matches!(
                        r.plan.status.as_str(),
                        "metadata_partial" | "location_uncertain"
                    )
            })
            .count() as u64;
        let lookup: HashMap<String, ItemInfo> = rows
            .into_iter()
            .map(|r| {
                let p = r.plan;
                let name = p
                    .main
                    .entry_name
                    .rsplit('/')
                    .next()
                    .unwrap_or(&p.main.entry_name)
                    .to_string();
                (
                    p.key.clone(),
                    ItemInfo {
                        name,
                        status: p.status.clone(),
                        media: media_type(p.kind),
                    },
                )
            })
            .collect();
        let sink = RunSink {
            engine: self,
            handle: h,
            root: job.output_root().to_path_buf(),
            lookup,
            warnings: AtomicU64::new(earlier_warnings),
            last: Mutex::new(None),
        };
        let control = h.control.lock().expect("control").clone();
        let outcome = job.run(&sink, &control);
        sink.finish();
        let outcome = outcome.map_err(job_error)?;
        let unlocked = job.options().unlocked;
        Ok(match outcome {
            Outcome::Completed(stats) => {
                let _ = job.report();
                if stats.skipped_free_limit > 0
                    && !unlocked
                    && !h.free_limit_ack.load(Ordering::SeqCst)
                {
                    h.awaiting_unlock.store(true, Ordering::SeqCst);
                    (JobState::FreeLimitReached, None)
                } else if stats.failed > 0 || stats.skipped_free_limit > 0 {
                    (JobState::Partial, None)
                } else {
                    (JobState::Done, None)
                }
            }
            Outcome::Paused(_) => (JobState::Paused, None),
            Outcome::Cancelled(_) => {
                let _ = job.report();
                (JobState::Cancelled, None)
            }
            Outcome::Blocked { code, detail, .. } => {
                let c = ErrorCode::from_engine(&code);
                (
                    JobState::Paused,
                    Some(
                        IpcError::new(c, mr_report::plain_message(&code))
                            .with_detail(redact_home(&detail)),
                    ),
                )
            }
            Outcome::InsufficientSpace(est) => (
                JobState::Paused,
                Some(
                    IpcError::new(ErrorCode::DiskFull, "Not enough space on the destination.")
                        .with_detail(format!(
                            "need {} more",
                            mr_job::space::gb(est.shortfall_bytes)
                        )),
                ),
            ),
            Outcome::InputsChanged(changes) => (
                JobState::Paused,
                Some(
                    IpcError::new(
                        ErrorCode::PartMissing,
                        "An export part changed or is missing since the job started.",
                    )
                    .with_detail(redact_home(&format!("{changes:?}"))),
                ),
            ),
        })
    }

    pub fn get_report(&self, job_id: &str) -> IpcResult<JobReport> {
        let job = self.open(job_id)?;
        let data = job.report_data().map_err(job_error)?;
        let summary = job.summary().map_err(job_error)?;
        let mut counts = StatusCounts::default();
        let mut failures = Vec::new();
        let mut items_total = 0u64;
        let root = job.output_root().to_path_buf();
        for r in &data.rows {
            use mr_report::{RowKind, Status};
            let is_item = r.kind == Some(RowKind::Item);
            match r.status {
                Some(Status::Done) if is_item => counts.done += 1,
                Some(Status::MetadataPartial) if is_item => counts.metadata_partial += 1,
                Some(Status::LocationUncertain) if is_item => counts.location_uncertain += 1,
                Some(Status::Failed) if is_item => {
                    counts.failed += 1;
                    if failures.len() < 200 {
                        failures.push(ItemOutcome {
                            item_id: r.item.clone().unwrap_or_default(),
                            name: r
                                .source_entry
                                .as_deref()
                                .and_then(|e| e.rsplit('/').next())
                                .unwrap_or("")
                                .to_string(),
                            media_type: if r.media_type.as_deref() == Some("video") {
                                MediaType::Video
                            } else {
                                MediaType::Photo
                            },
                            status: ItemStatus::Failed,
                            error_code: r.error_code.as_deref().map(ErrorCode::from_engine),
                            message: r.message.clone(),
                            output_path: r
                                .output_path
                                .as_ref()
                                .map(|p| root.join(p).to_string_lossy().into_owned()),
                            finished_at: now_utc(),
                        });
                    }
                }
                Some(Status::SkippedFreeLimit) if is_item => counts.skipped_free_limit += 1,
                Some(Status::MissingMedia) => counts.missing_media += 1,
                _ => {}
            }
            if is_item || r.status == Some(mr_report::Status::MissingMedia) {
                items_total += 1;
            }
        }
        let bytes_done = job
            .items()
            .map_err(job_error)?
            .iter()
            .filter(|r| r.stage == mr_job::control::Stage::Done)
            .map(|r| r.plan.main.size)
            .sum();
        let h = self.jobs.lock().expect("jobs").get(job_id).cloned();
        let state = match &h {
            Some(h) => h.state(),
            None => state_of_summary(&summary.status, &counts),
        };
        let clips = job
            .clip_groups()
            .map_err(job_error)?
            .into_iter()
            .map(|c| ClipOutcome {
                group_id: c.group.id.clone(),
                clip_count: c.group.items.len() as u64,
                total_seconds: c.group.total_secs,
                status: match c.stage.as_str() {
                    "done" => ClipStatus::Done,
                    "failed" => ClipStatus::Failed,
                    "skipped" => ClipStatus::Skipped,
                    "declined" => ClipStatus::Declined,
                    _ => ClipStatus::Pending,
                },
                output_path: c.output_path.as_ref().map(|p| {
                    mr_job::layout::to_fs(&root, p)
                        .to_string_lossy()
                        .into_owned()
                }),
                error_code: c.error_code.as_deref().map(ErrorCode::from_engine),
            })
            .collect();
        let report_dir = root.join(mr_job::layout::REPORT_DIR);
        let existing = |name: &str| {
            let p = report_dir.join(name);
            p.exists().then(|| p.to_string_lossy().into_owned())
        };
        Ok(JobReport {
            job_id: job_id.to_string(),
            state,
            output_path: root.to_string_lossy().into_owned(),
            report_html_path: existing("report.html"),
            report_csv_path: existing("report.csv"),
            started_at: summary.created_utc.clone(),
            finished_at: h.and_then(|h| h.finished_at.lock().expect("finished").clone()),
            items_total,
            bytes_done,
            missing_parts: data.missing_parts.clone(),
            issues: data
                .missing_parts
                .iter()
                .map(|n| Issue {
                    code: ErrorCode::PartMissing,
                    message: format!("Part {n} appears to be missing."),
                    count: None,
                    part_number: Some(*n),
                })
                .collect(),
            can_retry: counts.failed > 0,
            can_unlock: counts.skipped_free_limit > 0,
            can_add_part: !data.missing_parts.is_empty(),
            counts,
            failures,
            options: settings::from_engine(job.options()),
            clips,
        })
    }

    pub fn list_resumable_jobs(&self) -> Vec<ResumableJob> {
        let live = self.jobs.lock().expect("jobs").clone();
        let mut out: Vec<ResumableJob> = mr_job::list_jobs(self.state_dir())
            .into_iter()
            .filter_map(|j| {
                let get = |k: &str| j.counts.get(k).copied().unwrap_or(0) as u64;
                let open = get("pending")
                    + get("extracted")
                    + get("tagged")
                    + get("composited")
                    + get("written")
                    + get("skipped_free_limit");
                let state = match live.get(&j.id) {
                    Some(h) if h.is_running() || is_terminal(h.state()) => return None,
                    Some(h) => h.state(),
                    None if j.status == "cancelled" || open == 0 => return None,
                    None if get("pending") == 0 && get("skipped_free_limit") > 0 => {
                        JobState::FreeLimitReached
                    }
                    None => JobState::Paused,
                };
                Some(ResumableJob {
                    job_id: j.id.clone(),
                    output_path: j.output_root.clone(),
                    created_at: j.created_utc.clone(),
                    state,
                    items_done: get("done") + get("failed"),
                    items_total: j.counts.values().map(|v| *v as u64).sum(),
                    input_names: j.inputs.iter().map(|p| file_name(p)).collect(),
                })
            })
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        out
    }
}

fn state_of_summary(status: &str, c: &StatusCounts) -> JobState {
    match status {
        "completed" if c.failed > 0 || c.skipped_free_limit > 0 => JobState::Partial,
        "completed" => JobState::Done,
        "cancelled" => JobState::Cancelled,
        _ => JobState::Paused,
    }
}

struct ItemInfo {
    name: String,
    status: String,
    media: MediaType,
}

struct RunSink<'a> {
    engine: &'a Engine,
    handle: &'a JobHandle,
    root: PathBuf,
    lookup: HashMap<String, ItemInfo>,
    /// Items saved with the date only or without a location, this job so far.
    warnings: AtomicU64,
    last: Mutex<Option<ProgressEvent>>,
}

impl RunSink<'_> {
    /// The run has returned: one last progress with sleep prevention released.
    fn finish(&self) {
        if let Some(mut p) = self.last.lock().expect("progress").take() {
            p.sleep_prevented = false;
            p.throughput_items_per_s = 0.0;
            p.eta_seconds = None;
            p.warnings = self.warnings.load(Ordering::SeqCst);
            self.engine.emit(events::PROGRESS, &p);
        }
    }

    fn outcome(&self, o: &mr_job::ItemOutcome) -> ItemOutcome {
        use mr_job::control::Stage as S;
        let info = self.lookup.get(&o.id);
        let status = match o.stage {
            S::Failed => ItemStatus::Failed,
            S::SkippedFreeLimit => ItemStatus::SkippedFreeLimit,
            _ => match info.map(|i| i.status.as_str()) {
                Some("metadata_partial") => ItemStatus::MetadataPartial,
                Some("location_uncertain") => ItemStatus::LocationUncertain,
                _ => ItemStatus::Done,
            },
        };
        ItemOutcome {
            item_id: o.id.clone(),
            name: info.map(|i| i.name.clone()).unwrap_or_else(|| o.id.clone()),
            media_type: info.map(|i| i.media).unwrap_or(
                if o.media_type.as_deref() == Some("video") {
                    MediaType::Video
                } else {
                    MediaType::Photo
                },
            ),
            status,
            error_code: match status {
                ItemStatus::Failed => Some(
                    o.error_code
                        .as_deref()
                        .map(ErrorCode::from_engine)
                        .unwrap_or(ErrorCode::Unknown),
                ),
                ItemStatus::MetadataPartial => Some(ErrorCode::MatchPartial),
                ItemStatus::LocationUncertain => Some(ErrorCode::LocationUncertain),
                _ => None,
            },
            message: o.error_detail.as_deref().map(redact_home),
            output_path: o.output_path.as_ref().map(|p| {
                mr_job::layout::to_fs(&self.root, p)
                    .to_string_lossy()
                    .into_owned()
            }),
            finished_at: now_utc(),
        }
    }
}

impl mr_job::ProgressSink for RunSink<'_> {
    fn progress(&self, p: &mr_job::Progress) {
        let event = ProgressEvent {
            job_id: self.handle.id.clone(),
            overall_pct: p.percent,
            items_done: (p.items_done + p.items_failed) as u64,
            items_total: p.items_total as u64,
            bytes_done: p.bytes_done,
            bytes_total: p.bytes_total,
            stage: Stage::from_engine(&p.stage),
            throughput_items_per_s: p.throughput,
            eta_seconds: p.eta_seconds,
            warnings: self.warnings.load(Ordering::SeqCst),
            failures: p.failures as u64,
            recent_items: p.recent.iter().rev().map(|o| self.outcome(o)).collect(),
            sleep_prevented: p.sleep_prevented,
        };
        self.engine.emit(events::PROGRESS, &event);
        *self.last.lock().expect("progress") = Some(event);
    }

    fn item(&self, o: &mr_job::ItemOutcome) {
        // Not the group id in the log: it holds a capture date and an export file ID.
        if o.id.starts_with("clip:") {
            tracing::info!(
                "clip group joined: {:?} {}",
                o.stage,
                o.error_code.as_deref().unwrap_or("")
            );
            return;
        }
        let item = self.outcome(o);
        if matches!(
            item.status,
            ItemStatus::MetadataPartial | ItemStatus::LocationUncertain
        ) {
            self.warnings.fetch_add(1, Ordering::SeqCst);
        }
        self.engine.emit(
            events::ITEM,
            &ItemEvent {
                job_id: self.handle.id.clone(),
                item,
            },
        );
    }

    fn issue(&self, code: &str, detail: &str) {
        match code {
            "DEST_UNAVAILABLE" => {
                let e = IpcError::new(
                    ErrorCode::DestUnavailable,
                    "The output drive was disconnected.",
                )
                .with_detail(redact_home(detail));
                self.engine
                    .set_state(self.handle, JobState::Paused, Some(e));
            }
            "DEST_AVAILABLE" => self.engine.set_state(self.handle, JobState::Running, None),
            // These end the run; the outcome carries them as the paused state's error.
            "DISK_FULL" | "DISK_FULL_IMMINENT" | "PERMISSION_DENIED" | "INPUT_UNAVAILABLE"
            | "INPUT_CHANGED" => {}
            other => self.engine.emit(
                events::WARNING,
                &WarningEvent {
                    job_id: Some(self.handle.id.clone()),
                    code: ErrorCode::from_engine(other),
                    message: redact_home(detail),
                    count: None,
                },
            ),
        }
    }
}
