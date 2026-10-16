//! Jobs: create, open, run, retry, add a part, report (FR-33 to FR-40, FR-44).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use crossbeam_channel::{RecvTimeoutError, Sender};
use mr_core::discover::{Discovery, discover};
use mr_report::{PartInfo, ReportData, ReportFiles, Row, RowKind, Status, plain_message, redact};
use serde::{Deserialize, Serialize};

use crate::compositor::{Compositor, PassThrough};
use crate::control::{Control, Eta, ItemOutcome, Progress, ProgressSink, Stage, Ticker};
use crate::error::{JobError, Result};
use crate::fingerprint::{self, InputChange};
use crate::layout;
use crate::options::{FREE_TIER_ITEMS, Options};
use crate::plan::{
    ClipGroup, ExtraKind, ExtraRow, Naming, Plan, PlannedItem, estimate_space, plan_with,
};
use crate::platform::KeepAwake;
use crate::process::{Failure, GroupWork, WorkSettings, Worker, Written, adopt_check};
use crate::space::{MIN_FREE_WHILE_RUNNING, SpaceEstimate, available_space, gb};
use crate::state::{ClipRow, Db, Finished, ItemRow, JobRow, JournalEvent, OpenError, PartRow};

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const JOBS_DIR: &str = "jobs";
const DB_FILE: &str = "job.sqlite";
const JOB_FILE: &str = "job.json";
const RECHECK_EVERY: usize = 500;

/// Counts after a run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStats {
    pub done: usize,
    pub failed: usize,
    pub skipped_free_limit: usize,
    pub pending: usize,
    /// Items finished (done or failed) during this call.
    pub processed_this_run: usize,
    pub seconds: u64,
}

/// How `run` ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Outcome {
    /// Every item that may be processed is done or failed.
    Completed(RunStats),
    /// `Control::pause`: in-flight items finished, then stopped.
    Paused(RunStats),
    /// `Control::cancel`: in-flight items finished; output and state kept.
    Cancelled(RunStats),
    /// A condition that needs the user: `DISK_FULL_IMMINENT`, `DISK_FULL`,
    /// `PERMISSION_DENIED`, `INPUT_UNAVAILABLE`.
    Blocked {
        code: String,
        detail: String,
        stats: RunStats,
    },
    /// FR-28 AC1: not enough space; nothing was started.
    InsufficientSpace(SpaceEstimate),
    /// FR-33 AC2: an input part changed or vanished; call `reindex` or
    /// `add_part`, then run again.
    InputsChanged(Vec<InputChange>),
}

impl Outcome {
    pub fn status(&self) -> &'static str {
        match self {
            Outcome::Completed(_) => "completed",
            Outcome::Paused(_) => "paused",
            Outcome::Cancelled(_) => "cancelled",
            Outcome::Blocked { .. } => "blocked",
            Outcome::InsufficientSpace(_) => "insufficient_space",
            Outcome::InputsChanged(_) => "inputs_changed",
        }
    }

    pub fn stats(&self) -> Option<&RunStats> {
        match self {
            Outcome::Completed(s) | Outcome::Paused(s) | Outcome::Cancelled(s) => Some(s),
            Outcome::Blocked { stats, .. } => Some(stats),
            _ => None,
        }
    }
}

/// What `add_part`, `reindex` or an options change did to the item list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replanned {
    pub items: usize,
    pub new_items: usize,
    /// Finished items whose time, location or overlay changed; written again.
    pub requeued: Vec<String>,
    pub removed: usize,
    pub missing_parts: Vec<u32>,
}

/// One line of `list_jobs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobSummary {
    pub id: String,
    pub created_utc: String,
    pub output_root: String,
    pub status: String,
    pub profile: String,
    pub inputs: Vec<PathBuf>,
    pub counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JobFile {
    id: String,
    created_utc: String,
    inputs: Vec<PathBuf>,
    options: Options,
    output_root: PathBuf,
}

fn job_dir(state_dir: &Path, id: &str) -> PathBuf {
    state_dir.join(JOBS_DIR).join(id)
}

fn marker_path(root: &Path) -> PathBuf {
    root.join(layout::STATE_DIR).join(layout::MARKER_FILE)
}

/// Every job in a state directory.
pub fn list_jobs(state_dir: &Path) -> Vec<JobSummary> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(state_dir.join(JOBS_DIR)) else {
        return out;
    };
    for e in rd.flatten() {
        let id = e.file_name().to_string_lossy().into_owned();
        if let Some((row, counts)) = crate::state::peek(&e.path().join(DB_FILE)) {
            out.push(JobSummary {
                id,
                created_utc: row.created_utc,
                output_root: row.output_path,
                status: row.status,
                profile: row.format_profile,
                inputs: row.inputs,
                counts,
            });
        } else if let Ok(text) = fs::read_to_string(e.path().join(JOB_FILE))
            && let Ok(jf) = serde_json::from_str::<JobFile>(&text)
        {
            out.push(JobSummary {
                id,
                created_utc: jf.created_utc,
                output_root: jf.output_root.to_string_lossy().into_owned(),
                status: "state_unreadable".into(),
                profile: String::new(),
                inputs: jf.inputs,
                counts: BTreeMap::new(),
            });
        }
    }
    out.sort_by(|a, b| a.created_utc.cmp(&b.created_utc));
    out
}

/// The job that writes to `dest`, if any (the output folder's marker, else
/// the state directory's records).
pub fn find_job_for_output(state_dir: &Path, dest: &Path) -> Option<String> {
    let root = layout::output_root(dest);
    if let Ok(id) = fs::read_to_string(marker_path(&root)) {
        let id = id.trim().to_string();
        if job_dir(state_dir, &id).exists() {
            return Some(id);
        }
    }
    let key = layout::root_key(&root);
    list_jobs(state_dir)
        .into_iter()
        .find(|j| layout::root_key(Path::new(&j.output_root)) == key)
        .map(|j| j.id)
}

/// A job: its state file, options and inputs.
pub struct Job {
    id: String,
    dir: PathBuf,
    db: Db,
    row: JobRow,
    root: PathBuf,
    compositor: Arc<dyn Compositor>,
    rebuilt: Option<String>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("root", &self.root)
            .field("status", &self.row.status)
            .finish()
    }
}

fn now() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn video_workers() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    (cores / 2).clamp(1, 2)
}

impl Job {
    /// Plan `inputs` and create a job writing to `options.dest`. Fails with
    /// [`JobError::OutputInUse`] when the output folder already has a job.
    pub fn create(state_dir: &Path, inputs: &[PathBuf], options: Options) -> Result<Job> {
        let inputs: Vec<PathBuf> = inputs.iter().map(|p| layout::absolute(p)).collect();
        let mut options = options;
        options.dest = layout::absolute(&options.dest);
        let root = layout::output_root(&options.dest);
        let mut adopt: Option<String> = None;
        if let Some(existing) = find_job_for_output(state_dir, &options.dest) {
            if job_dir(state_dir, &existing).join(DB_FILE).exists() {
                return Err(JobError::OutputInUse { job_id: existing });
            }
            adopt = Some(existing);
        } else if let Ok(id) = fs::read_to_string(marker_path(&root)) {
            // Output from a job whose state is gone: adopt its files.
            adopt = Some(id.trim().to_string());
        }
        let naming = Naming {
            keep: HashMap::new(),
            avoid_existing: adopt.is_none(),
            own_files: Default::default(),
        };
        let (plan, _d) = plan_with(&inputs, &options, &naming)?;
        let id = adopt
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let dir = job_dir(state_dir, &id);
        fs::create_dir_all(&dir).map_err(|e| JobError::io(&dir, e))?;
        let db = Db::create(&dir.join(DB_FILE))?;
        let row = JobRow {
            id: id.clone(),
            created_utc: now(),
            app_version: APP_VERSION.to_string(),
            options: options.clone(),
            input_fingerprint: String::new(),
            output_path: root.to_string_lossy().into_owned(),
            format_profile: plan.profile.clone(),
            status: "created".into(),
            inputs: inputs.clone(),
            root_key: layout::root_key(&root),
            missing_parts: plan.missing_parts.clone(),
            warnings: plan.warnings.clone(),
            tz_data_version: plan.tz_data_version.clone(),
            home_zone: plan.home_zone.clone(),
        };
        let mut job = Job {
            id: id.clone(),
            dir,
            db,
            row,
            root: root.clone(),
            compositor: Arc::new(PassThrough),
            rebuilt: None,
        };
        job.store_plan(&plan, None)?;
        if adopt.is_some() {
            let n = job.adopt_outputs()?;
            job.rebuilt = Some(format!(
                "the saved progress for this output folder was missing; rebuilt from {n} finished files"
            ));
        }
        job.apply_free_tier()?;
        job.write_job_file()?;
        let marker = marker_path(&root);
        fs::create_dir_all(marker.parent().expect("marker dir"))
            .map_err(|e| JobError::io(&marker, e))?;
        fs::write(&marker, format!("{id}\n")).map_err(|e| JobError::io(&marker, e))?;
        Ok(job)
    }

    /// Open a job. A damaged state file is rebuilt from the output folder
    /// (`JOB_STATE_CORRUPT`); see [`Job::rebuilt`].
    pub fn open(state_dir: &Path, id: &str) -> Result<Job> {
        let dir = job_dir(state_dir, id);
        if !dir.is_dir() {
            return Err(JobError::NoSuchJob(id.to_string()));
        }
        let job = match Db::open(&dir.join(DB_FILE)) {
            Ok(db) => {
                let row = db.job()?;
                Job {
                    id: id.to_string(),
                    dir,
                    root: PathBuf::from(&row.output_path),
                    db,
                    row,
                    compositor: Arc::new(PassThrough),
                    rebuilt: None,
                }
            }
            Err(OpenError::Corrupt(why)) => Self::rebuild(&dir, id, &why)?,
            Err(OpenError::Other(e)) => {
                if dir.join(JOB_FILE).is_file() {
                    Self::rebuild(&dir, id, "the state file is missing")?
                } else {
                    return Err(e);
                }
            }
        };
        Ok(job)
    }

    fn rebuild(dir: &Path, id: &str, why: &str) -> Result<Job> {
        let text = fs::read_to_string(dir.join(JOB_FILE)).map_err(|_| {
            JobError::State(format!(
                "job state is damaged ({why}) and job.json is missing; it cannot be rebuilt"
            ))
        })?;
        let jf: JobFile = serde_json::from_str(&text)
            .map_err(|e| JobError::State(format!("job.json unreadable: {e}")))?;
        let stamp = Utc::now().format("%Y%m%dT%H%M%S").to_string();
        for suffix in ["", "-wal", "-shm"] {
            let p = dir.join(format!("{DB_FILE}{suffix}"));
            if p.exists() {
                let _ = fs::rename(&p, dir.join(format!("{DB_FILE}{suffix}.corrupt-{stamp}")));
            }
        }
        let db = Db::create(&dir.join(DB_FILE))?;
        let naming = Naming {
            keep: HashMap::new(),
            avoid_existing: false,
            own_files: Default::default(),
        };
        let (plan, _) = plan_with(&jf.inputs, &jf.options, &naming)?;
        let row = JobRow {
            id: id.to_string(),
            created_utc: jf.created_utc.clone(),
            app_version: APP_VERSION.to_string(),
            options: jf.options.clone(),
            input_fingerprint: String::new(),
            output_path: jf.output_root.to_string_lossy().into_owned(),
            format_profile: plan.profile.clone(),
            status: "rebuilt".into(),
            inputs: jf.inputs.clone(),
            root_key: layout::root_key(&jf.output_root),
            missing_parts: plan.missing_parts.clone(),
            warnings: plan.warnings.clone(),
            tz_data_version: plan.tz_data_version.clone(),
            home_zone: plan.home_zone.clone(),
        };
        let mut job = Job {
            id: id.to_string(),
            dir: dir.to_path_buf(),
            db,
            row,
            root: jf.output_root.clone(),
            compositor: Arc::new(PassThrough),
            rebuilt: None,
        };
        job.store_plan(&plan, None)?;
        let n = job.adopt_outputs()?;
        job.apply_free_tier()?;
        job.rebuilt = Some(format!(
            "saved progress was damaged ({why}); rebuilt from {n} finished files in the output folder"
        ));
        Ok(job)
    }

    /// Write a plan's parts, items, extras and job fields. With `rows`, the
    /// given item rows replace the planned defaults.
    fn store_plan(&mut self, plan: &Plan, rows: Option<Vec<ItemRow>>) -> Result<()> {
        let mut parts = Vec::new();
        for p in &plan.parts {
            let fp = fingerprint::fingerprint(Path::new(&p.path))
                .map_err(|e| JobError::io(&p.path, e))?;
            parts.push(PartRow {
                fingerprint: fp,
                label: p.label.clone(),
                number: p.number,
                primary: p.primary,
                truncated: p.truncated,
                zip: p.zip,
            });
        }
        self.row.input_fingerprint = fingerprint::combined(
            &parts
                .iter()
                .map(|p| p.fingerprint.clone())
                .collect::<Vec<_>>(),
        );
        self.row.format_profile = plan.profile.clone();
        self.row.missing_parts = plan.missing_parts.clone();
        self.row.warnings = plan.warnings.clone();
        self.row.tz_data_version = plan.tz_data_version.clone();
        self.row.home_zone = plan.home_zone.clone();
        self.db.put_job(&self.row)?;
        self.db.replace_parts(&parts)?;
        let rows = rows.unwrap_or_else(|| {
            plan.items
                .iter()
                .cloned()
                .map(|p| ItemRow::new(p, Stage::Pending))
                .collect()
        });
        self.db.upsert_items(&rows)?;
        self.db.replace_extras(&plan.extras)?;
        // Clip groups (FR-27): a group is joined again when a member's plan
        // changed, so its signature is the members' signatures.
        let sigs: HashMap<&str, String> = plan
            .items
            .iter()
            .map(|p| (p.key.as_str(), p.signature()))
            .collect();
        let groups: Vec<(ClipGroup, String)> = plan
            .clip_groups
            .iter()
            .map(|g| {
                let sig = g
                    .items
                    .iter()
                    .map(|k| sigs.get(k.as_str()).cloned().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join(";");
                (g.clone(), sig)
            })
            .collect();
        self.db.replace_clip_groups(&groups)?;
        Ok(())
    }

    /// Clip groups (FR-27) and their join state, in capture order.
    pub fn clip_groups(&self) -> Result<Vec<ClipRow>> {
        self.db.clip_rows()
    }

    /// Mark items whose planned output already exists and verifies as done.
    fn adopt_outputs(&mut self) -> Result<usize> {
        let mut changed = Vec::new();
        for mut row in self.db.items()? {
            if row.stage == Stage::Done {
                continue;
            }
            if let Some(sha) = adopt_check(&self.root, &row) {
                row.stage = Stage::Done;
                row.output_path = Some(row.plan.output_rel.clone());
                row.output_sha256 = Some(sha);
                let beside = layout::overlay_beside(&row.plan.output_rel);
                if row.plan.overlay.is_some() && layout::to_fs(&self.root, &beside).is_file() {
                    row.overlay_output = Some(beside);
                    row.composite_pending = true;
                }
                let om = layout::original_main(&row.plan.output_rel, &row.plan.ext);
                if layout::to_fs(&self.root, &om).is_file() {
                    row.originals_main = Some(om);
                    let oo = layout::original_overlay(&row.plan.output_rel);
                    if layout::to_fs(&self.root, &oo).is_file() {
                        row.originals_overlay = Some(oo);
                    }
                }
                row.done_utc = Some(now());
                changed.push(row);
            }
        }
        for row in &changed {
            self.db.journal_event(row.key(), "adopted")?;
        }
        self.db.upsert_items(&changed)?;
        Ok(changed.len())
    }

    fn write_job_file(&self) -> Result<()> {
        let jf = JobFile {
            id: self.id.clone(),
            created_utc: self.row.created_utc.clone(),
            inputs: self.row.inputs.clone(),
            options: self.row.options.clone(),
            output_root: self.root.clone(),
        };
        let path = self.dir.join(JOB_FILE);
        let tmp = self.dir.join("job.json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(&jf).expect("json"))
            .and_then(|()| fs::rename(&tmp, &path))
            .map_err(|e| JobError::io(&path, e))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn options(&self) -> &Options {
        &self.row.options
    }

    pub fn inputs(&self) -> &[PathBuf] {
        &self.row.inputs
    }

    /// `<dest>/Memories Rescue`.
    pub fn output_root(&self) -> &Path {
        &self.root
    }

    pub fn state_dir_of_job(&self) -> &Path {
        &self.dir
    }

    /// Why the state was rebuilt on open, if it was (`JOB_STATE_CORRUPT`).
    pub fn rebuilt(&self) -> Option<&str> {
        self.rebuilt.as_deref()
    }

    /// Use a real compositor instead of the pass-through default.
    pub fn set_compositor(&mut self, compositor: Arc<dyn Compositor>) {
        self.compositor = compositor;
    }

    pub fn items(&self) -> Result<Vec<ItemRow>> {
        self.db.items()
    }

    pub fn counts(&self) -> Result<BTreeMap<String, usize>> {
        self.db.counts()
    }

    /// The item journal (`start`, `done`, `failed`, `requeue:...`, `adopted`).
    pub fn journal(&self) -> Result<Vec<JournalEvent>> {
        self.db.journal()
    }

    pub fn summary(&self) -> Result<JobSummary> {
        Ok(JobSummary {
            id: self.id.clone(),
            created_utc: self.row.created_utc.clone(),
            output_root: self.row.output_path.clone(),
            status: self.row.status.clone(),
            profile: self.row.format_profile.clone(),
            inputs: self.row.inputs.clone(),
            counts: self.db.counts()?,
        })
    }

    /// Inputs that changed since they were recorded (FR-33 AC2).
    pub fn check_inputs(&self) -> Result<Vec<InputChange>> {
        Ok(fingerprint::changes(
            &self
                .db
                .parts()?
                .into_iter()
                .map(|p| p.fingerprint)
                .collect::<Vec<_>>(),
        ))
    }

    /// Change the options. A change of home zone, template or the
    /// uncertain-location setting re-plans the items not yet written.
    pub fn set_options(&mut self, options: Options) -> Result<Option<Replanned>> {
        let mut options = options;
        options.dest = self.row.options.dest.clone();
        let replan = options.home_tz != self.row.options.home_tz
            || options.output_template != self.row.options.output_template
            || options.apply_uncertain_locations != self.row.options.apply_uncertain_locations;
        self.row.options = options;
        self.db.put_job(&self.row)?;
        self.write_job_file()?;
        let r = if replan { Some(self.replan()?) } else { None };
        self.apply_free_tier()?;
        Ok(r)
    }

    /// Unlock (FR-44): the items skipped by the free tier become pending.
    pub fn unlock(&mut self) -> Result<usize> {
        let before = self.db.items_in(Stage::SkippedFreeLimit)?.len();
        let mut o = self.row.options.clone();
        o.unlocked = true;
        self.set_options(o)?;
        Ok(before - self.db.items_in(Stage::SkippedFreeLimit)?.len())
    }

    /// First 100 items in chronological order without a licence (FR-44).
    fn apply_free_tier(&mut self) -> Result<()> {
        let items = self.db.items()?;
        let mut to_pending = Vec::new();
        let mut to_skipped = Vec::new();
        if self.row.options.unlocked {
            to_pending.extend(
                items
                    .iter()
                    .filter(|i| i.stage == Stage::SkippedFreeLimit)
                    .map(|i| i.key().to_string()),
            );
        } else {
            let used = items
                .iter()
                .filter(|i| matches!(i.stage, Stage::Done | Stage::Failed))
                .count();
            let mut allowed = FREE_TIER_ITEMS.saturating_sub(used);
            let mut open: Vec<&ItemRow> = items
                .iter()
                .filter(|i| matches!(i.stage, Stage::Pending | Stage::SkippedFreeLimit))
                .collect();
            open.sort_by_key(|i| i.plan.chrono_rank);
            for i in open {
                if allowed > 0 {
                    allowed -= 1;
                    if i.stage != Stage::Pending {
                        to_pending.push(i.key().to_string());
                    }
                } else if i.stage != Stage::SkippedFreeLimit {
                    to_skipped.push(i.key().to_string());
                }
            }
        }
        self.db.set_stage(&to_pending, Stage::Pending)?;
        self.db.set_stage(&to_skipped, Stage::SkippedFreeLimit)?;
        Ok(())
    }

    /// FR-39: re-queue only the failed items, optionally with new options.
    pub fn retry_failed(&mut self, options: Option<Options>) -> Result<usize> {
        if let Some(o) = options {
            self.set_options(o)?;
        }
        let failed: Vec<String> = self
            .db
            .items_in(Stage::Failed)?
            .into_iter()
            .map(|i| i.key().to_string())
            .collect();
        self.db.set_stage(&failed, Stage::Pending)?;
        Ok(failed.len())
    }

    /// Queue the items that were finished without burn-in (`composite_pending`,
    /// the pass-through compositor) so that a real compositor set with
    /// [`Job::set_compositor`] composites them. Their earlier files are
    /// replaced or removed when they are written again. Returns how many.
    pub fn requeue_composite_pending(&mut self) -> Result<usize> {
        let mut rows = Vec::new();
        for mut r in self.db.items()? {
            if r.stage == Stage::Done && r.composite_pending {
                r.previous_outputs = r.recorded_outputs();
                r.stage = Stage::Pending;
                rows.push(r);
            }
        }
        self.db.upsert_items(&rows)?;
        Ok(rows.len())
    }

    /// FR-40: add a part that was missing; only what it brings is processed.
    pub fn add_part(&mut self, path: &Path) -> Result<Replanned> {
        let path = layout::absolute(path);
        if !path.exists() {
            return Err(JobError::io(
                &path,
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such file or folder"),
            ));
        }
        if !self.row.inputs.contains(&path) {
            self.row.inputs.push(path);
        }
        let r = self.replan()?;
        self.write_job_file()?;
        Ok(r)
    }

    /// Accept changed inputs (FR-33 AC2): re-index and keep finished work.
    pub fn reindex(&mut self) -> Result<Replanned> {
        let r = self.replan()?;
        self.write_job_file()?;
        Ok(r)
    }

    fn replan(&mut self) -> Result<Replanned> {
        let existing: HashMap<String, ItemRow> = self
            .db
            .items()?
            .into_iter()
            .map(|r| (r.key().to_string(), r))
            .collect();
        let written = |r: &ItemRow| r.stage == Stage::Done || r.output_path.is_some();
        let mut keep: HashMap<String, String> = existing
            .values()
            .filter(|r| written(r))
            .map(|r| {
                (
                    r.key().to_string(),
                    r.output_path
                        .clone()
                        .unwrap_or_else(|| r.plan.output_rel.clone()),
                )
            })
            .collect();
        let options = self.row.options.clone();
        let inputs = self.row.inputs.clone();
        let naming = Naming {
            keep: keep.clone(),
            avoid_existing: true,
            own_files: Default::default(),
        };
        let (mut plan, _) = plan_with(&inputs, &options, &naming)?;
        // Finished items whose bytes would change get new names: plan again
        // without their kept names.
        let changed: Vec<String> = plan
            .items
            .iter()
            .filter(|p| {
                existing
                    .get(&p.key)
                    .is_some_and(|old| written(old) && old.plan.signature() != p.signature())
            })
            .map(|p| p.key.clone())
            .collect();
        if !changed.is_empty() {
            let mut own_files = std::collections::HashSet::new();
            for k in &changed {
                keep.remove(k);
                if let Some(old) = existing.get(k) {
                    own_files.extend(old.recorded_outputs().into_iter().map(|r| r.to_lowercase()));
                }
            }
            let naming = Naming {
                keep,
                avoid_existing: true,
                own_files,
            };
            plan = plan_with(&inputs, &options, &naming)?.0;
        }
        let mut result = Replanned {
            items: plan.items.len(),
            missing_parts: plan.missing_parts.clone(),
            ..Replanned::default()
        };
        let mut rows = Vec::with_capacity(plan.items.len());
        for p in &plan.items {
            match existing.get(&p.key) {
                None => {
                    result.new_items += 1;
                    rows.push(ItemRow::new(p.clone(), Stage::Pending));
                }
                Some(old) if changed.contains(&p.key) => {
                    let mut r = old.clone();
                    r.previous_outputs = old.recorded_outputs();
                    r.plan = p.clone();
                    r.stage = Stage::Pending;
                    result.requeued.push(p.key.clone());
                    rows.push(r);
                }
                Some(old) => {
                    let mut r = old.clone();
                    r.plan = p.clone();
                    rows.push(r);
                }
            }
        }
        let planned: std::collections::HashSet<&str> =
            plan.items.iter().map(|p| p.key.as_str()).collect();
        let gone: Vec<String> = existing
            .values()
            .filter(|r| !planned.contains(r.key()) && !written(r))
            .map(|r| r.key().to_string())
            .collect();
        result.removed = gone.len();
        self.db.delete_items(&gone)?;
        self.store_plan(&plan, Some(rows))?;
        self.apply_free_tier()?;
        Ok(result)
    }

    /// Remove temp directories (NFR-PRV-09). Only while holding the run lock.
    fn clean_tmp(&self) {
        let tmp = self.root.join(layout::STATE_DIR).join(layout::TMP_DIR);
        let _ = fs::remove_dir_all(layout::long_path(&tmp));
    }

    /// Take the job's run lock: one `run` at a time per job, across processes.
    /// Released when the returned file is dropped (or the process dies).
    fn run_lock(&self) -> Result<fs::File> {
        let path = self.dir.join("run.lock");
        let file = fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| JobError::io(&path, e))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(fs::TryLockError::WouldBlock) => Err(JobError::Busy {
                job_id: self.id.clone(),
            }),
            Err(fs::TryLockError::Error(e)) => Err(JobError::io(&path, e)),
        }
    }

    /// Remove temp files a crash left behind (NFR-PRV-09), for the app to
    /// call at start-up. Does nothing while the job is running elsewhere.
    pub fn clean_temp(&self) -> Result<()> {
        let _lock = self.run_lock()?;
        self.clean_tmp();
        Ok(())
    }

    fn stats(&self, processed: usize, started: Instant) -> Result<RunStats> {
        let c = self.db.counts()?;
        let get = |k: &str| c.get(k).copied().unwrap_or(0);
        Ok(RunStats {
            done: get("done"),
            failed: get("failed"),
            skipped_free_limit: get("skipped_free_limit"),
            pending: get("pending"),
            processed_this_run: processed,
            seconds: started.elapsed().as_secs(),
        })
    }

    /// Process every pending item (section 4.1). Blocks until the job
    /// completes, pauses, is cancelled or is blocked; progress goes to `sink`.
    pub fn run(&mut self, sink: &dyn ProgressSink, control: &Control) -> Result<Outcome> {
        let started = Instant::now();
        let _lock = self.run_lock()?;
        // Temp left by a crash is removed at the next start (NFR-PRV-09).
        self.clean_tmp();
        if let Some(why) = &self.rebuilt {
            sink.issue("JOB_STATE_CORRUPT", why);
        }
        let changes = self.check_inputs()?;
        if !changes.is_empty() {
            sink.issue(
                "INPUT_CHANGED",
                &format!(
                    "{} export part(s) changed or are missing since the job started; re-index them to continue",
                    changes.len()
                ),
            );
            self.db.set_status("inputs_changed")?;
            return Ok(Outcome::InputsChanged(changes));
        }
        self.apply_free_tier()?;
        let open = self.db.items_in(Stage::Pending)?;
        let options = self.row.options.clone();
        if !options.ignore_space_check && !open.is_empty() {
            let plans: Vec<PlannedItem> = open.iter().map(|r| r.plan.clone()).collect();
            let est = estimate_space(&plans, &options, &self.root, |_| true);
            if !est.sufficient {
                sink.issue(
                    "DISK_FULL",
                    &format!(
                        "Not enough space on the output drive: need {} more.",
                        gb(est.shortfall_bytes)
                    ),
                );
                return Ok(Outcome::InsufficientSpace(est));
            }
        }
        let joins_pending = self.db.clip_rows()?.iter().any(|g| g.stage == "pending");
        if open.is_empty() && !joins_pending {
            let stats = self.stats(0, started)?;
            self.db.set_status("completed")?;
            return Ok(Outcome::Completed(stats));
        }
        if !options.dest.exists() {
            fs::create_dir_all(&options.dest).map_err(|e| JobError::io(&options.dest, e))?;
        }

        let d = discover(&self.row.inputs);
        let part_by_path: HashMap<String, usize> = d
            .parts
            .iter()
            .map(|p| (p.path.to_string_lossy().into_owned(), p.id))
            .collect();
        let settings = WorkSettings {
            overlay_mode: options.overlay_mode,
            keep_originals: options.keep_originals,
            windows_compat: options.windows_compat,
            hooks: options.test_hooks.clone(),
            dest: options.dest.clone(),
        };
        let workers = options.worker_count();
        let compositor = self.compositor.clone();
        let root = self.root.clone();
        self.db.set_status("running")?;
        let awake = KeepAwake::new("Memories Rescue is repairing your export");
        let awake_active = awake.active();

        let outcome = if open.is_empty() {
            Ok(Outcome::Completed(self.stats(0, started)?))
        } else {
            std::thread::scope(|scope| -> Result<Outcome> {
                let (job_tx, job_rx) = crossbeam_channel::bounded::<ItemRow>(workers);
                let (res_tx, res_rx) = crossbeam_channel::unbounded::<(
                    String,
                    bool,
                    std::result::Result<Written, Failure>,
                    f64,
                )>();
                for _ in 0..workers {
                    let job_rx = job_rx.clone();
                    let res_tx = res_tx.clone();
                    let d = &d;
                    let part_by_path = &part_by_path;
                    let compositor = &*compositor;
                    let settings = &settings;
                    let root = root.clone();
                    scope.spawn(move || {
                        let mut worker = Worker::new(d, part_by_path, root, compositor, settings);
                        for row in job_rx.iter() {
                            let t = Instant::now();
                            let r =
                                std::panic::catch_unwind(AssertUnwindSafe(|| worker.process(&row)))
                                    .unwrap_or_else(|_| {
                                        Err(Failure::Item {
                                            code: "UNKNOWN".into(),
                                            detail: "internal error while processing this item"
                                                .into(),
                                        })
                                    });
                            let _ = res_tx.send((
                                row.plan.key.clone(),
                                row.plan.is_video(),
                                r,
                                t.elapsed().as_secs_f64(),
                            ));
                        }
                    });
                }
                drop(res_tx);
                drop(job_rx);
                let result = self.coordinate(
                    &job_tx,
                    &res_rx,
                    sink,
                    control,
                    open,
                    workers,
                    awake_active,
                    started,
                );
                drop(job_tx);
                result
            })
        };
        // FR-27: join the clip groups once every item has been processed.
        if joins_pending && matches!(outcome, Ok(Outcome::Completed(_))) {
            self.rejoin_pass(sink, control, &d, &part_by_path, &settings, &*compositor)?;
        }
        drop(awake);
        let outcome = outcome?;
        self.db.set_status(outcome.status())?;
        self.row.status = outcome.status().to_string();
        self.db.checkpoint();
        if matches!(outcome, Outcome::Completed(_) | Outcome::Cancelled(_)) {
            self.clean_tmp();
        }
        Ok(outcome)
    }

    #[allow(clippy::too_many_arguments)]
    fn coordinate(
        &mut self,
        job_tx: &Sender<ItemRow>,
        res_rx: &crossbeam_channel::Receiver<(
            String,
            bool,
            std::result::Result<Written, Failure>,
            f64,
        )>,
        sink: &dyn ProgressSink,
        control: &Control,
        open: Vec<ItemRow>,
        workers: usize,
        awake_active: bool,
        started: Instant,
    ) -> Result<Outcome> {
        let options = self.row.options.clone();
        let hooks = options.test_hooks.clone();
        let hooks_on = hooks.enabled();
        let video_limit = video_workers();
        let mut photos: VecDeque<ItemRow> = VecDeque::new();
        let mut videos: VecDeque<ItemRow> = VecDeque::new();
        let mut bytes_total = 0u64;
        for r in open {
            bytes_total += r.plan.main.size + r.plan.overlay.as_ref().map(|o| o.size).unwrap_or(0);
            if r.plan.is_video() {
                videos.push_back(r);
            } else {
                photos.push_back(r);
            }
        }
        let counts = self.db.counts()?;
        let get = |k: &str| counts.get(k).copied().unwrap_or(0);
        let mut done = get("done");
        let mut failed = get("failed");
        let total = done + failed + photos.len() + videos.len();
        let mut in_flight: HashMap<String, (ItemRow, bool)> = HashMap::new();
        let mut videos_in_flight = 0usize;
        let mut eta = Eta::default();
        let mut ticker = Ticker::new(Duration::from_secs(1));
        let mut recent: VecDeque<ItemOutcome> = VecDeque::new();
        let mut processed = 0usize;
        let mut since_check = 0usize;
        let mut bytes_done = 0u64;
        let mut warnings = 0usize;
        let mut blocked: Option<(String, String)> = None;
        let mut offline_until: Option<Instant> = None;
        let mut offline_reported = false;
        let recheck_every = hooks
            .recheck_every
            .filter(|_| hooks_on)
            .unwrap_or(RECHECK_EVERY)
            .max(1);

        loop {
            let simulated_offline = offline_until.is_some_and(|t| Instant::now() < t);
            let dest_ok = options.dest.exists() && !simulated_offline;
            if !dest_ok && !offline_reported {
                sink.issue(
                    "DEST_UNAVAILABLE",
                    "The output drive was disconnected. The job continues when it is back.",
                );
                offline_reported = true;
            } else if dest_ok && offline_reported {
                sink.issue("DEST_AVAILABLE", "The output drive is back; continuing.");
                offline_reported = false;
            }

            while dest_ok
                && blocked.is_none()
                && control.may_dispatch()
                && in_flight.len() < workers
            {
                let video_ok = videos_in_flight < video_limit;
                let take_video = match (photos.front(), videos.front()) {
                    (Some(p), Some(v)) => video_ok && v.plan.chrono_rank < p.plan.chrono_rank,
                    (None, Some(_)) => video_ok,
                    _ => false,
                };
                let next = if take_video {
                    videos.pop_front()
                } else {
                    photos.pop_front()
                };
                let Some(mut row) = next else { break };
                row.attempts = self.db.mark_started(row.key())?;
                let is_video = row.plan.is_video();
                if is_video {
                    videos_in_flight += 1;
                }
                in_flight.insert(row.key().to_string(), (row.clone(), is_video));
                if job_tx.send(row).is_err() {
                    return Err(JobError::State("worker pool stopped".into()));
                }
            }

            if in_flight.is_empty() {
                let stats = || -> Result<RunStats> { self.stats(processed, started) };
                if let Some((code, detail)) = blocked.take() {
                    let s = stats()?;
                    sink.progress(&self.progress_now(
                        done,
                        failed,
                        processed,
                        total,
                        bytes_done,
                        bytes_total,
                        &eta,
                        &photos,
                        &videos,
                        workers,
                        &recent,
                        warnings,
                        started,
                        "blocked",
                        awake_active,
                    ));
                    return Ok(Outcome::Blocked {
                        code,
                        detail,
                        stats: s,
                    });
                }
                if control.is_cancelled() {
                    return Ok(Outcome::Cancelled(stats()?));
                }
                if control.is_paused() {
                    return Ok(Outcome::Paused(stats()?));
                }
                if photos.is_empty() && videos.is_empty() {
                    sink.progress(&self.progress_now(
                        done,
                        failed,
                        processed,
                        total,
                        bytes_done,
                        bytes_total,
                        &eta,
                        &photos,
                        &videos,
                        workers,
                        &recent,
                        warnings,
                        started,
                        "done",
                        awake_active,
                    ));
                    return Ok(Outcome::Completed(stats()?));
                }
                if !dest_ok {
                    if ticker.due() {
                        sink.progress(&self.progress_now(
                            done,
                            failed,
                            processed,
                            total,
                            bytes_done,
                            bytes_total,
                            &eta,
                            &photos,
                            &videos,
                            workers,
                            &recent,
                            warnings,
                            started,
                            "waiting for the output drive",
                            awake_active,
                        ));
                    }
                    control.wait(Duration::from_millis(250));
                    continue;
                }
            }

            match res_rx.recv_timeout(Duration::from_millis(100)) {
                Ok((key, is_video, result, secs)) => {
                    let Some((row, _)) = in_flight.remove(&key) else {
                        continue;
                    };
                    if is_video {
                        videos_in_flight -= 1;
                    }
                    let outcome = match result {
                        Ok(w) => {
                            let stage = if w.kept_with_error.is_some() {
                                Stage::Failed
                            } else {
                                Stage::Done
                            };
                            let (code, detail) = w.kept_with_error.clone().unzip();
                            self.db.mark_finished(
                                &key,
                                &Finished {
                                    stage: Some(stage),
                                    error_code: code.clone(),
                                    error_detail: detail.clone(),
                                    output_path: Some(w.output_rel.clone()),
                                    output_sha256: Some(w.output_sha256.clone()),
                                    overlay_output: w.overlay_rel.clone(),
                                    originals_main: w.originals_main.clone(),
                                    originals_overlay: w.originals_overlay.clone(),
                                    composite_pending: w.composite_pending,
                                    seconds: secs,
                                },
                            )?;
                            for (c, d) in &w.warnings {
                                self.db.journal_event(&key, &format!("warning:{c}|{d}"))?;
                            }
                            if stage == Stage::Done {
                                done += 1;
                            } else {
                                failed += 1;
                            }
                            processed += 1;
                            since_check += 1;
                            bytes_done += w.bytes;
                            eta.record(is_video, secs);
                            ItemOutcome {
                                id: key.clone(),
                                stage,
                                media_type: Some(if is_video { "video" } else { "photo" }.into()),
                                output_path: Some(w.output_rel),
                                error_code: code,
                                error_detail: detail,
                                seconds: secs,
                            }
                        }
                        Err(Failure::Item { code, detail }) => {
                            self.db.mark_finished(
                                &key,
                                &Finished {
                                    stage: Some(Stage::Failed),
                                    error_code: Some(code.clone()),
                                    error_detail: Some(detail.clone()),
                                    seconds: secs,
                                    ..Finished::default()
                                },
                            )?;
                            failed += 1;
                            processed += 1;
                            since_check += 1;
                            eta.record(is_video, secs);
                            ItemOutcome {
                                id: key.clone(),
                                stage: Stage::Failed,
                                media_type: Some(if is_video { "video" } else { "photo" }.into()),
                                output_path: None,
                                error_code: Some(code),
                                error_detail: Some(detail),
                                seconds: secs,
                            }
                        }
                        Err(Failure::Job { code, detail }) => {
                            self.db.mark_requeued(&key, &code)?;
                            let mut row = row;
                            row.stage = Stage::Pending;
                            if is_video {
                                videos.push_front(row);
                            } else {
                                photos.push_front(row);
                            }
                            if code != "DEST_UNAVAILABLE" && blocked.is_none() {
                                sink.issue(&code, &detail);
                                blocked = Some((code, detail));
                            }
                            continue;
                        }
                    };
                    if outcome.error_code.is_some() {
                        warnings += 1;
                    }
                    sink.item(&outcome);
                    recent.push_front(outcome);
                    recent.truncate(10);

                    if hooks_on
                        && let Some(after) = hooks.dest_offline_after
                        && processed == after
                        && offline_until.is_none()
                    {
                        offline_until = Some(
                            Instant::now() + Duration::from_secs(hooks.dest_offline_secs.max(1)),
                        );
                    }
                    if since_check >= recheck_every {
                        since_check = 0;
                        let free = hooks
                            .fake_free_bytes
                            .filter(|_| hooks_on)
                            .or_else(|| available_space(&self.root));
                        if let Some(free) = free
                            && free < MIN_FREE_WHILE_RUNNING
                            && blocked.is_none()
                        {
                            let detail = format!(
                                "Only {} left on the output drive; paused before it fills up.",
                                gb(free)
                            );
                            sink.issue("DISK_FULL_IMMINENT", &detail);
                            blocked = Some(("DISK_FULL_IMMINENT".into(), detail));
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(JobError::State("worker pool stopped unexpectedly".into()));
                }
            }

            if ticker.due() {
                let stage = if blocked.is_some() || !control.may_dispatch() {
                    "finishing items in progress"
                } else {
                    "processing"
                };
                sink.progress(&self.progress_now(
                    done,
                    failed,
                    processed,
                    total,
                    bytes_done,
                    bytes_total,
                    &eta,
                    &photos,
                    &videos,
                    workers,
                    &recent,
                    warnings,
                    started,
                    stage,
                    awake_active,
                ));
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn progress_now(
        &self,
        done: usize,
        failed: usize,
        processed: usize,
        total: usize,
        bytes_done: u64,
        bytes_total: u64,
        eta: &Eta,
        photos: &VecDeque<ItemRow>,
        videos: &VecDeque<ItemRow>,
        workers: usize,
        recent: &VecDeque<ItemOutcome>,
        warnings: usize,
        started: Instant,
        stage: &str,
        awake: bool,
    ) -> Progress {
        let finished = done + failed;
        let elapsed = started.elapsed().as_secs_f64();
        Progress {
            percent: if total == 0 {
                100.0
            } else {
                finished as f64 * 100.0 / total as f64
            },
            items_done: done,
            items_failed: failed,
            items_total: total,
            bytes_done,
            bytes_total,
            stage: stage.to_string(),
            throughput: if elapsed > 0.0 {
                processed as f64 / elapsed
            } else {
                0.0
            },
            eta_seconds: eta.remaining(photos.len(), videos.len(), workers),
            recent: recent.iter().cloned().collect(),
            warnings,
            failures: failed,
            elapsed_seconds: elapsed,
            sleep_prevented: awake,
        }
    }

    /// The report rows (FR-38): one per item and one per other entry.
    /// FR-27: join every pending clip group whose clips are all done, one
    /// group at a time on this thread (each join already runs ffmpeg). A
    /// group with a failed clip is `failed` (`CLIP_INCOMPLETE`); one the
    /// compositor declines is `declined`; a drive problem stops the pass and
    /// leaves the rest pending for the next run.
    #[allow(clippy::too_many_arguments)]
    fn rejoin_pass(
        &mut self,
        sink: &dyn ProgressSink,
        control: &Control,
        d: &Discovery,
        part_by_path: &HashMap<String, usize>,
        settings: &WorkSettings,
        compositor: &dyn Compositor,
    ) -> Result<()> {
        let groups: Vec<ClipRow> = self
            .db
            .clip_rows()?
            .into_iter()
            .filter(|g| g.stage == "pending")
            .collect();
        if groups.is_empty() {
            return Ok(());
        }
        let items: HashMap<String, ItemRow> = self
            .db
            .items()?
            .into_iter()
            .map(|r| (r.key().to_string(), r))
            .collect();
        let mut worker = Worker::new(d, part_by_path, self.root.clone(), compositor, settings);
        for g in groups {
            if control.is_cancelled() || control.is_paused() {
                break;
            }
            let id = g.group.id.clone();
            let members: Option<Vec<ItemRow>> = g
                .group
                .items
                .iter()
                .map(|k| items.get(k).cloned())
                .collect();
            let Some(members) = members else {
                self.db.finish_clip(
                    &id,
                    "failed",
                    None,
                    Some((
                        "CLIP_INCOMPLETE",
                        "a clip of this group is no longer in the export",
                    )),
                )?;
                continue;
            };
            if members.iter().any(|m| m.stage.is_open()) {
                continue;
            }
            if members.iter().any(|m| m.stage != Stage::Done) {
                self.db.finish_clip(
                    &id,
                    "failed",
                    None,
                    Some(("CLIP_INCOMPLETE", "a clip of this group was not written")),
                )?;
                continue;
            }
            let work = GroupWork {
                id: id.clone(),
                output_rel: g.group.output_rel.clone(),
                members,
                previous: g.output_path.clone().map(|p| (p, g.output_sha256.clone())),
            };
            let t = Instant::now();
            let result = std::panic::catch_unwind(AssertUnwindSafe(|| worker.process_group(&work)))
                .unwrap_or_else(|_| {
                    Err(Failure::Item {
                        code: "UNKNOWN".into(),
                        detail: "internal error while joining clips".into(),
                    })
                });
            let (stage, output, code, detail) = match result {
                Ok(Some(w)) => {
                    for (c, dt) in &w.warnings {
                        self.db
                            .journal_event(&format!("clip:{id}"), &format!("warning:{c}|{dt}"))?;
                    }
                    self.db.finish_clip(
                        &id,
                        "done",
                        Some((&w.output_rel, &w.output_sha256)),
                        None,
                    )?;
                    (Stage::Done, Some(w.output_rel), None, None)
                }
                Ok(None) => {
                    self.db.finish_clip(&id, "declined", None, None)?;
                    continue;
                }
                Err(Failure::Item { code, detail }) => {
                    self.db
                        .finish_clip(&id, "failed", None, Some((&code, &detail)))?;
                    (Stage::Failed, None, Some(code), Some(detail))
                }
                Err(Failure::Job { code, detail }) => {
                    sink.issue(&code, &detail);
                    break;
                }
            };
            sink.item(&ItemOutcome {
                id: format!("clip:{id}"),
                stage,
                media_type: Some("video".into()),
                output_path: output,
                error_code: code,
                error_detail: detail,
                seconds: t.elapsed().as_secs_f64(),
            });
        }
        Ok(())
    }

    pub fn report_data(&self) -> Result<ReportData> {
        let mut rows = Vec::new();
        let warnings = self.db.item_warnings()?;
        let items = self.db.items()?;
        let local_of: HashMap<String, String> = items
            .iter()
            .map(|r| {
                (
                    r.plan.key.clone(),
                    r.plan.local().format("%Y-%m-%d %H:%M:%S %:z").to_string(),
                )
            })
            .collect();
        for r in items {
            let p = &r.plan;
            let status = match r.stage {
                Stage::Done => match p.status.as_str() {
                    "metadata_partial" => Status::MetadataPartial,
                    "location_uncertain" => Status::LocationUncertain,
                    _ => Status::Done,
                },
                Stage::Failed => Status::Failed,
                Stage::SkippedFreeLimit => Status::SkippedFreeLimit,
                _ => Status::Pending,
            };
            let message = match (&r.last_error_code, &r.last_error_detail, r.stage) {
                (Some(code), detail, Stage::Failed) => Some(match detail {
                    Some(d) => format!("{} ({})", plain_message(code), redact(d)),
                    None => plain_message(code).to_string(),
                }),
                (_, _, Stage::SkippedFreeLimit) => Some(plain_message("FREE_LIMIT").to_string()),
                _ if r.composite_pending => Some(plain_message("COMPOSITE_PENDING").to_string()),
                _ if p.status == "metadata_partial" => {
                    Some(plain_message("MATCH_PARTIAL").to_string())
                }
                _ if p.status == "location_uncertain" => {
                    Some(plain_message("LOCATION_UNCERTAIN").to_string())
                }
                _ => None,
            };
            // Compositor notes (overlay_aspect_mismatch, h264_compat, ...).
            let message = match (message, warnings.get(&p.key)) {
                (m, None) => m,
                (m, Some(w)) => {
                    let mut texts: Vec<&str> = Vec::new();
                    for (code, _) in w {
                        let t = plain_message(&code.to_ascii_uppercase());
                        if !texts.contains(&t) {
                            texts.push(t);
                        }
                    }
                    let notes = texts.join(" ");
                    Some(match m {
                        Some(m) => format!("{m} {notes}"),
                        None => notes,
                    })
                }
            };
            let local = p.local();
            rows.push(Row {
                status: Some(status),
                kind: Some(RowKind::Item),
                item: Some(p.key.clone()),
                json_index: p.json_index,
                media_type: Some(
                    match p.kind {
                        crate::compositor::MediaKind::Photo => "photo",
                        crate::compositor::MediaKind::Video => "video",
                    }
                    .into(),
                ),
                source_part: Some(p.main.part_label.clone()),
                source_entry: Some(p.main.entry_name.clone()),
                overlay_entry: p.overlay.as_ref().map(|o| o.entry_name.clone()),
                output_path: if matches!(r.stage, Stage::Done | Stage::Failed) {
                    r.output_path.clone()
                } else {
                    None
                },
                capture_local: Some(local.format("%Y-%m-%d %H:%M:%S %:z").to_string()),
                time_precision: Some(p.precision.as_str().to_string()),
                match_confidence: Some(p.confidence.clone()),
                location: Some(p.location_decision.clone()),
                error_code: if r.stage == Stage::Failed {
                    r.last_error_code.clone()
                } else {
                    None
                },
                message,
                attempts: Some(r.attempts),
                composite_pending: r.composite_pending,
            });
        }
        for g in self.db.clip_rows()? {
            let (status, message) = match g.stage.as_str() {
                "done" => (
                    Status::Done,
                    format!(
                        "{} ({} clips, {:.0} s)",
                        plain_message("CLIP_JOINED"),
                        g.group.items.len(),
                        g.group.total_secs
                    ),
                ),
                "failed" => (
                    Status::Failed,
                    match (&g.error_code, &g.error_detail) {
                        (Some(c), Some(d)) => format!("{} ({})", plain_message(c), redact(d)),
                        (Some(c), None) => plain_message(c).to_string(),
                        _ => plain_message("UNKNOWN").to_string(),
                    },
                ),
                "declined" => (Status::Failed, plain_message("CLIP_DECLINED").to_string()),
                "skipped" => continue,
                _ => (Status::Pending, plain_message("CLIP_PENDING").to_string()),
            };
            rows.push(Row {
                status: Some(status),
                kind: Some(RowKind::JoinedClip),
                item: Some(g.group.id.clone()),
                media_type: Some("video".into()),
                output_path: g.output_path.clone(),
                capture_local: local_of.get(&g.group.id).cloned(),
                error_code: g.error_code.clone(),
                message: Some(message),
                attempts: Some(g.attempts),
                ..Row::default()
            });
        }
        for e in self.db.extras()? {
            rows.push(extra_row(&e));
        }
        let parts = self
            .db
            .parts()?
            .into_iter()
            .map(|p| PartInfo {
                label: p.label,
                number: p.number,
                primary: p.primary,
                truncated: p.truncated,
            })
            .collect();
        // The composite note reflects what happened, not what the plan
        // expected: none when every overlay was burned in.
        let pending = self
            .db
            .items()?
            .iter()
            .filter(|i| i.composite_pending)
            .count();
        let mut notes: Vec<String> = self
            .row
            .warnings
            .iter()
            .filter(|w| w.code != "PART_MISSING" && w.code != crate::plan::COMPOSITE_PENDING)
            .map(|w| w.detail.clone())
            .collect();
        if pending > 0 {
            notes.push(format!(
                "{pending} items have their overlay saved next to the file; burning it in needs the overlay component."
            ));
        }
        if let Some(why) = &self.rebuilt {
            notes.push(format!("Saved progress was rebuilt: {why}."));
        }
        Ok(ReportData {
            job_id: self.id.clone(),
            app_version: APP_VERSION.to_string(),
            generated_utc: now(),
            profile: self.row.format_profile.clone(),
            parts,
            missing_parts: self.row.missing_parts.clone(),
            notes,
            rows,
        })
    }

    /// Write `_report/report.html`, `report.csv` and `failed_items.json`.
    pub fn report(&self) -> Result<ReportFiles> {
        let data = self.report_data()?;
        let dir = layout::long_path(&self.root.join(layout::REPORT_DIR));
        mr_report::write_report(&dir, &data).map_err(|e| JobError::io(&dir, e))
    }
}

fn extra_row(e: &ExtraRow) -> Row {
    let status = match e.kind {
        ExtraKind::MissingMedia => Status::MissingMedia,
        ExtraKind::UnmatchedEntry => Status::MetadataPartial,
        ExtraKind::OrphanOverlay => Status::OrphanOverlay,
        ExtraKind::Unrecognised => Status::Unrecognised,
        ExtraKind::Refused => Status::Refused,
        ExtraKind::Duplicate => Status::Duplicate,
    };
    let kind = if e.json_index.is_some() && e.source.is_none() {
        RowKind::JsonEntry
    } else {
        RowKind::File
    };
    Row {
        status: Some(status),
        kind: Some(kind),
        json_index: e.json_index,
        media_type: e.media_type.clone(),
        source_part: e.source.as_ref().map(|s| s.part_label.clone()),
        source_entry: e.source.as_ref().map(|s| s.entry_name.clone()),
        capture_local: e.date_utc.clone(),
        message: Some(e.detail.clone()),
        ..Row::default()
    }
}
