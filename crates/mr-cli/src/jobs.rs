//! The engine commands: `plan`, `run`, `resume`, `retry`, `add-part`,
//! `report`, `jobs`. They drive `mr-job` exactly as the app will.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use clap::{Args, ValueEnum};
use mr_job::{
    ClipRejoin, Control, ItemOutcome, Job, JobError, MediaCompositor, Options, Outcome,
    OverlayMode, Progress, ProgressSink, list_jobs, plan,
};
use serde_json::json;

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OverlayArg {
    BurnInKeepOriginals,
    BurnInOnly,
    SaveSeparate,
    Ignore,
}

impl From<OverlayArg> for OverlayMode {
    fn from(a: OverlayArg) -> OverlayMode {
        match a {
            OverlayArg::BurnInKeepOriginals => OverlayMode::BurnInKeepOriginals,
            OverlayArg::BurnInOnly => OverlayMode::BurnInOnly,
            OverlayArg::SaveSeparate => OverlayMode::SaveSeparate,
            OverlayArg::Ignore => OverlayMode::Ignore,
        }
    }
}

/// Every job option (FR-48) as a flag.
#[derive(Args, Debug, Clone)]
pub struct OptionArgs {
    /// Destination folder; output goes to `<dest>/Memories Rescue/`.
    #[arg(long)]
    pub dest: PathBuf,
    #[arg(long, value_enum, default_value = "burn-in-keep-originals")]
    pub overlay_mode: OverlayArg,
    /// Do not keep the untouched originals of composited items (FR-25).
    #[arg(long)]
    pub no_keep_originals: bool,
    /// IANA zone for items without GPS, e.g. Europe/London (default: system zone).
    #[arg(long)]
    pub home_tz: Option<String>,
    /// Worker threads (default: min(cores - 1, 4)).
    #[arg(long)]
    pub workers: Option<usize>,
    /// Output file name template: {date} {time} {shortid} {id} {ext}.
    #[arg(long, default_value = mr_job::options::DEFAULT_TEMPLATE)]
    pub template: String,
    /// Licensed: process every item (otherwise 100 per state directory, FR-44).
    #[arg(long)]
    pub unlocked: bool,
    /// Write the ordinal match's location even when the day group disagrees.
    #[arg(long)]
    pub apply_uncertain_locations: bool,
    /// "Maximise Windows compatibility": HEVC videos are saved as H.264 (FR-51).
    #[arg(long)]
    pub windows_compat: bool,
    /// Join consecutive clips into one extra video (FR-27); the clips are kept.
    #[arg(long)]
    pub clip_rejoin: bool,
    /// Clip-rejoin years, `2017-2021` or `all`.
    #[arg(long, default_value = "2017-2021")]
    pub clip_years: String,
    /// Clip groups not to join (FR-27 AC3), by group id as `plan` lists them.
    #[arg(long, value_delimiter = ',')]
    pub clip_skip: Vec<String>,
    /// Also pack the output into ZIP64 archives once every item is
    /// finished (FR-31); the folder tree is kept.
    #[arg(long)]
    pub zip_output: bool,
    /// Largest size of one ZIP archive: `fat32` (4 GiB minus one byte), or
    /// a size such as `700M`, `2G` or a byte count. Default: one archive.
    #[arg(long, value_name = "SIZE")]
    pub zip_volume: Option<String>,
    /// Start even when the disk-space precheck fails.
    #[arg(long)]
    pub ignore_space_check: bool,
    #[command(flatten)]
    pub test: TestArgs,
}

/// Fault-injection hooks; debug builds only.
#[derive(Args, Debug, Clone, Default)]
pub struct TestArgs {
    #[arg(long, hide = true, value_delimiter = ',')]
    pub test_fail_ids: Vec<String>,
    #[arg(long, hide = true, default_value_t = 0)]
    pub test_delay_ms: u64,
    #[arg(long, hide = true)]
    pub test_fake_free_bytes: Option<u64>,
    #[arg(long, hide = true)]
    pub test_recheck_every: Option<usize>,
    #[arg(long, hide = true, default_value_t = 0)]
    pub test_zip_delay_ms: u64,
}

impl OptionArgs {
    pub fn to_options(&self) -> Result<Options, String> {
        let mut o = Options::new(&self.dest);
        o.overlay_mode = self.overlay_mode.into();
        o.keep_originals = !self.no_keep_originals;
        o.home_tz = self.home_tz.clone();
        o.workers = self.workers;
        o.output_template = self.template.clone();
        o.unlocked = self.unlocked;
        o.apply_uncertain_locations = self.apply_uncertain_locations;
        o.windows_compat = self.windows_compat;
        o.clip_rejoin = ClipRejoin {
            enabled: self.clip_rejoin,
            year_range: parse_years(&self.clip_years)?,
            skip_groups: self.clip_skip.clone(),
        };
        o.zip_output = self.zip_output || self.zip_volume.is_some();
        o.zip_volume_bytes = self.zip_volume.as_deref().map(parse_size).transpose()?;
        o.ignore_space_check = self.ignore_space_check;
        o.test_hooks.fail_ids = self.test.test_fail_ids.clone();
        o.test_hooks.delay_ms = self.test.test_delay_ms;
        o.test_hooks.fake_free_bytes = self.test.test_fake_free_bytes;
        o.test_hooks.recheck_every = self.test.test_recheck_every;
        o.test_hooks.zip_delay_ms = self.test.test_zip_delay_ms;
        Ok(o)
    }
}

/// `fat32`, `700M`, `2G`, `1.5G` or a byte count (K, M, G are powers of 1024).
pub fn parse_size(s: &str) -> Result<u64, String> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("fat32") {
        return Ok(mr_job::ZIP_VOLUME_FAT32);
    }
    let (num, mul) = match t.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&t[..t.len() - 1], 1u64 << 10),
        Some('M') => (&t[..t.len() - 1], 1 << 20),
        Some('G') => (&t[..t.len() - 1], 1 << 30),
        _ => (t, 1),
    };
    let v: f64 = num
        .trim()
        .parse()
        .map_err(|_| format!("--zip-volume: {s:?} is not a size (try fat32, 700M or 2G)"))?;
    let bytes = (v * mul as f64) as u64;
    if bytes < 64 << 10 {
        return Err(format!("--zip-volume: {s:?} is smaller than 64K"));
    }
    Ok(bytes)
}

fn parse_years(s: &str) -> Result<Option<(i32, i32)>, String> {
    if s.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| format!("--clip-years: expected `YYYY-YYYY` or `all`, got {s:?}"))?;
    let a: i32 = a.trim().parse().map_err(|_| format!("bad year {a:?}"))?;
    let b: i32 = b.trim().parse().map_err(|_| format!("bad year {b:?}"))?;
    Ok(Some((a.min(b), a.max(b))))
}

/// The per-user state directory (SRS 3.7): `%APPDATA%\NovaByteWorks\Memories Rescue`
/// on Windows, `~/Library/Application Support/Memories Rescue` on macOS.
pub fn default_state_dir() -> PathBuf {
    if cfg!(windows)
        && let Some(appdata) = std::env::var_os("APPDATA")
    {
        return PathBuf::from(appdata)
            .join("NovaByteWorks")
            .join("Memories Rescue");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Memories Rescue")
    } else {
        home.join(".local/share/memories-rescue")
    }
}

/// Prints progress to stderr once per callback (the engine calls at most
/// once per second) and issues as they happen.
static PROCESS_START: OnceLock<Instant> = OnceLock::new();
static FIRST_ITEM: OnceLock<Instant> = OnceLock::new();
static LAST_OUTCOME: Mutex<Option<serde_json::Value>> = Mutex::new(None);

/// When this process started, for `MR_BENCH_JSON`.
pub fn process_start() -> Instant {
    *PROCESS_START.get_or_init(Instant::now)
}

/// Append one JSON line to `MR_BENCH_JSON`, if set: wall time, time to the
/// first finished item (NFR-PERF-06), peak resident memory, the outcome.
pub fn bench_line(command: &str, started: Instant, code: ExitCode) {
    let Some(path) = std::env::var_os("MR_BENCH_JSON") else {
        return;
    };
    let v = json!({
        "command": command,
        "elapsed_s": started.elapsed().as_secs_f64(),
        "first_item_s": FIRST_ITEM.get().map(|t| t.duration_since(started).as_secs_f64()),
        "peak_rss_bytes": mr_job::platform::peak_rss_bytes(),
        "exit_ok": code == ExitCode::SUCCESS,
        "outcome": LAST_OUTCOME.lock().ok().and_then(|o| o.clone()),
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{v}");
    }
}

/// `mr-cli bench verify`: see the command's help.
pub fn bench_verify(state: &Path, id: &str) -> ExitCode {
    use std::collections::{BTreeMap, HashMap, HashSet};
    let job = match Job::open(state, id) {
        Ok(j) => j,
        Err(e) => return fail(e),
    };
    let items = match job.items() {
        Ok(i) => i,
        Err(e) => return fail(e),
    };
    let root = job.output_root().to_path_buf();
    let mut stages: BTreeMap<String, usize> = BTreeMap::new();
    let mut expected: HashSet<String> = HashSet::new();
    let (mut sha_mismatch, mut metadata_fail, mut missing_output) = (Vec::new(), Vec::new(), 0);
    for i in &items {
        *stages.entry(i.stage.as_str().to_string()).or_default() += 1;
        for rel in i.recorded_outputs() {
            expected.insert(rel);
        }
        if i.stage != mr_job::Stage::Done {
            continue;
        }
        let Some(rel) = &i.output_path else {
            missing_output += 1;
            continue;
        };
        let path = mr_job::layout::to_fs(&root, rel);
        match mr_job::fingerprint::sha256_file(&path) {
            Ok(h) if Some(&h) == i.output_sha256.as_ref() => {}
            _ => sha_mismatch.push(rel.clone()),
        }
        if let Err(e) = mr_job::process::verify(&i.plan.ext, &path, &i.plan.capture()) {
            metadata_fail.push(format!("{rel}: {e}"));
        }
    }
    if let Ok(groups) = job.clip_groups() {
        expected.extend(
            groups
                .into_iter()
                .filter(|g| g.stage == "done")
                .filter_map(|g| g.output_path),
        );
    }
    let mut actual: HashSet<String> = HashSet::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p
                .strip_prefix(&root)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            if rel.starts_with(mr_job::layout::STATE_DIR)
                || rel.starts_with(mr_job::layout::REPORT_DIR)
            {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else {
                actual.insert(rel);
            }
        }
    }
    let extra: Vec<&String> = actual.difference(&expected).collect();
    let missing: Vec<&String> = expected.difference(&actual).collect();
    // Journal: starts per item, and starts of items already finished.
    let mut finished: HashSet<String> = HashSet::new();
    let mut starts: HashMap<String, usize> = HashMap::new();
    let mut redone_after_done = 0usize;
    for ev in job.journal().unwrap_or_default() {
        match ev.event.as_str() {
            "done" => {
                finished.insert(ev.item);
            }
            "start" => {
                if finished.contains(&ev.item) {
                    redone_after_done += 1;
                }
                *starts.entry(ev.item).or_default() += 1;
            }
            _ => {}
        }
    }
    let total_starts: usize = starts.values().sum();
    let restarted = starts.values().filter(|&&n| n > 1).count();
    let report_rows = job.report_data().map(|d| d.rows.len()).unwrap_or(0);
    let tmp_left = root
        .join(mr_job::layout::STATE_DIR)
        .join(mr_job::layout::TMP_DIR)
        .read_dir()
        .map(|mut d| d.next().is_some())
        .unwrap_or(false);
    let v = json!({
        "job": id,
        "items": items.len(),
        "stages": stages,
        "report_rows": report_rows,
        "files_expected": expected.len(),
        "files_on_disk": actual.len(),
        "extra_files": extra.len(),
        "missing_files": missing.len(),
        "extra_sample": extra.iter().take(5).collect::<Vec<_>>(),
        "missing_sample": missing.iter().take(5).collect::<Vec<_>>(),
        "done_without_output": missing_output,
        "sha_mismatch": sha_mismatch.len(),
        "metadata_fail": metadata_fail.len(),
        "metadata_fail_sample": metadata_fail.iter().take(5).collect::<Vec<_>>(),
        "journal_starts": total_starts,
        "items_started_more_than_once": restarted,
        "finished_items_started_again": redone_after_done,
        "temp_left": tmp_left,
        "zip_volumes": job.zip_volumes().unwrap_or_default().len(),
    });
    println!("{}", serde_json::to_string_pretty(&v).expect("json"));
    let ok = extra.is_empty()
        && missing.is_empty()
        && sha_mismatch.is_empty()
        && metadata_fail.is_empty()
        && redone_after_done == 0;
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub struct ConsoleSink {
    pub quiet: bool,
    last_len: Mutex<usize>,
}

impl ConsoleSink {
    pub fn new(quiet: bool) -> ConsoleSink {
        ConsoleSink {
            quiet,
            last_len: Mutex::new(0),
        }
    }
}

impl ProgressSink for ConsoleSink {
    fn progress(&self, p: &Progress) {
        if self.quiet {
            return;
        }
        let eta = p
            .eta_seconds
            .map(|s| format!(", about {:.0} s left", s))
            .unwrap_or_default();
        let line = format!(
            "[{:5.1}%] {}/{} done, {} failed, {:.1} items/s{eta} ({}){}",
            p.percent,
            p.items_done,
            p.items_total,
            p.items_failed,
            p.throughput,
            p.stage,
            if p.sleep_prevented {
                ""
            } else {
                " [sleep not prevented]"
            }
        );
        let mut last = self.last_len.lock().unwrap();
        let pad = last.saturating_sub(line.len());
        eprint!("\r{line}{}", " ".repeat(pad));
        *last = line.len();
        let _ = std::io::stderr().flush();
    }

    fn item(&self, o: &ItemOutcome) {
        if FIRST_ITEM.get().is_none() {
            let t = *FIRST_ITEM.get_or_init(Instant::now);
            // Written at once, so a run killed later still leaves it (NFR-PERF-06).
            if let Some(path) = std::env::var_os("MR_BENCH_JSON")
                && let Ok(mut f) = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
            {
                let v = json!({
                    "event": "first_item",
                    "first_item_s": t.duration_since(process_start()).as_secs_f64(),
                });
                let _ = writeln!(f, "{v}");
            }
        }
        if self.quiet {
            return;
        }
        if let Some(code) = &o.error_code {
            eprintln!(
                "\n  {} {}: {}",
                o.id,
                code,
                o.error_detail.as_deref().unwrap_or("")
            );
        }
    }

    fn issue(&self, code: &str, detail: &str) {
        if !self.quiet {
            eprintln!("\n! {code}: {detail}");
        }
    }
}

fn fail(e: impl std::fmt::Display) -> ExitCode {
    eprintln!("error: {e}");
    ExitCode::FAILURE
}

/// Exit codes: 0 completed, 1 error, 2 blocked or needs the user, 3 paused/cancelled.
fn outcome_exit(o: &Outcome) -> ExitCode {
    match o {
        Outcome::Completed(_) => ExitCode::SUCCESS,
        Outcome::Paused(_) | Outcome::Cancelled(_) => ExitCode::from(3),
        _ => ExitCode::from(2),
    }
}

fn print_outcome(job: &Job, o: &Outcome, json: bool) {
    if json {
        let v = json!({
            "job": job.id(),
            "outcome": o.status(),
            "detail": o,
            "counts": job.counts().unwrap_or_default(),
            "output_root": job.output_root(),
            "zip_volumes": job.zip_volumes().unwrap_or_default(),
        });
        println!("{}", serde_json::to_string_pretty(&v).expect("json"));
        return;
    }
    eprintln!();
    match o {
        Outcome::Completed(s) | Outcome::Paused(s) | Outcome::Cancelled(s) => println!(
            "{}: {} done, {} failed, {} skipped (free limit), {} pending; {} processed in {} s",
            o.status(),
            s.done,
            s.failed,
            s.skipped_free_limit,
            s.pending,
            s.processed_this_run,
            s.seconds
        ),
        Outcome::Blocked {
            code,
            detail,
            stats,
        } => println!(
            "blocked ({code}): {detail}; {} done, {} pending. Fix it, then `mr-cli resume {}`.",
            stats.done,
            stats.pending,
            job.id()
        ),
        Outcome::InsufficientSpace(est) => {
            println!(
                "not enough space: need {} more (required {}, free {}).",
                mr_job::space::gb(est.shortfall_bytes),
                mr_job::space::gb(est.required_bytes),
                est.free_bytes
                    .map(mr_job::space::gb)
                    .unwrap_or_else(|| "unknown".into())
            );
            for opt in &est.options {
                println!(
                    "  option: {:?} (saves {})",
                    opt.action,
                    mr_job::space::gb(opt.saves_bytes)
                );
            }
        }
        Outcome::InputsChanged(changes) => {
            println!("inputs changed since the job started:");
            for c in changes {
                println!("  {c:?}");
            }
            println!(
                "run `mr-cli add-part {} <path>` or re-run with the same files.",
                job.id()
            );
        }
    }
    println!("output: {}", job.output_root().display());
    for v in job.zip_volumes().unwrap_or_default() {
        match v.state.as_str() {
            "done" => println!(
                "zip: {} ({} files, {}){}",
                v.name,
                v.files,
                mr_job::space::gb(v.size),
                if v.oversize {
                    ", larger than the volume size"
                } else {
                    ""
                }
            ),
            _ => println!(
                "zip: {} {} ({}: {})",
                v.name,
                v.state,
                v.error_code.as_deref().unwrap_or("-"),
                v.error_detail.as_deref().unwrap_or("")
            ),
        }
    }
}

/// The ffmpeg for burn-in, compatibility and clip joins, named by
/// `MR_FFMPEG` (the app passes its sidecar the same way). Without it,
/// overlays are saved beside the files (`composite_pending`), as before.
fn sidecar_ffmpeg() -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os("MR_FFMPEG")?);
    std::fs::metadata(&p)
        .is_ok_and(|m| m.is_file() && m.len() > 0)
        .then_some(p)
}

fn drive(job: &mut Job, quiet: bool, json: bool) -> ExitCode {
    if let Some(ffmpeg) = sidecar_ffmpeg() {
        if !quiet {
            eprintln!("media: {}", ffmpeg.display());
        }
        job.set_compositor(Arc::new(MediaCompositor::new(ffmpeg)));
    } else if !quiet {
        eprintln!("media: no ffmpeg (set MR_FFMPEG); overlays are saved beside the files");
    }
    let sink = ConsoleSink::new(quiet);
    match job.run(&sink, &Control::new()) {
        Ok(o) => {
            if let Ok(mut last) = LAST_OUTCOME.lock() {
                *last = Some(json!({
                    "job": job.id(),
                    "status": o.status(),
                    "stats": o.stats(),
                    "counts": job.counts().unwrap_or_default(),
                }));
            }
            if matches!(o, Outcome::Completed(_))
                && let Err(e) = job.report()
            {
                eprintln!("warning: report not written: {e}");
            }
            print_outcome(job, &o, json);
            outcome_exit(&o)
        }
        Err(e) => fail(e),
    }
}

pub fn cmd_plan(
    state: &Path,
    inputs: &[PathBuf],
    opts: &OptionArgs,
    json: bool,
    export: Option<&Path>,
    limit: usize,
) -> ExitCode {
    let options = match opts.to_options() {
        Ok(o) => o,
        Err(e) => return fail(e),
    };
    // With MR_FFMPEG the run will burn overlays in, so the plan's warnings
    // say so (no COMPOSITE_PENDING).
    let planned = match sidecar_ffmpeg() {
        Some(ffmpeg) => mr_job::plan_for(inputs, &options, &MediaCompositor::new(ffmpeg)),
        None => plan(inputs, &options),
    };
    let mut p = match planned {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    p.fit_free_tier(&options, &mr_job::free::for_state(state));
    if let Some(path) = export
        && let Err(e) = std::fs::write(path, serde_json::to_vec_pretty(&p).expect("json"))
    {
        return fail(e);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&p).expect("json"));
        return ExitCode::SUCCESS;
    }
    let t = &p.totals;
    println!(
        "Dry run: nothing is written{}.",
        if export.is_some() {
            " except the preview file"
        } else {
            ""
        }
    );
    println!("Output folder: {}", p.output_root.display());
    println!(
        "Format profile: {}; parts {}; missing parts {:?}",
        p.profile,
        p.parts.len(),
        p.missing_parts
    );
    println!(
        "Items {} ({} photos, {} videos, {} with overlays); JSON entries {}",
        t.items, t.photos, t.videos, t.with_overlay, t.json_entries
    );
    println!(
        "Match: exact {}, ordinal {}, none {}; exact times {}, day only {}; GPS applied {}, uncertain {}",
        t.exact,
        t.ordinal,
        t.none,
        t.second_precision,
        t.day_precision,
        t.location_applied,
        t.location_uncertain
    );
    println!(
        "Not in the export {}, unmatched entries {}, orphan overlays {}, unrecognised {}, refused {}, duplicates {}",
        t.missing_media,
        t.unmatched_entries,
        t.orphan_overlays,
        t.unrecognised,
        t.refused,
        t.duplicates
    );
    println!(
        "This run would process {} items; {} need the unlock.",
        t.to_process, t.skipped_free_limit
    );
    let s = &p.space;
    println!(
        "Space: needs {} (output {}, originals {}, temp {}, ZIP {}); free {}; {}",
        mr_job::space::gb(s.required_bytes),
        mr_job::space::gb(s.final_bytes),
        mr_job::space::gb(s.originals_bytes),
        mr_job::space::gb(s.peak_temp_bytes),
        mr_job::space::gb(s.zip_bytes),
        s.free_bytes
            .map(mr_job::space::gb)
            .unwrap_or_else(|| "unknown".into()),
        if s.sufficient { "enough" } else { "NOT ENOUGH" }
    );
    for w in &p.warnings {
        println!("! {}: {}", w.code, w.detail);
    }
    if !p.clip_groups.is_empty() {
        println!("Clip groups (FR-27; skip one with --clip-skip <id>):");
        for g in &p.clip_groups {
            println!(
                "  {} {} clips, {:.1} s -> {}{}",
                g.id,
                g.items.len(),
                g.total_secs,
                g.output_rel,
                if g.enabled { "" } else { "  (skipped)" }
            );
        }
    }
    println!("Sample (chronological):");
    for it in p.sample(limit) {
        println!(
            "  {} {} {:<7} {:<8} {} -> {}",
            it.local().format("%Y-%m-%d %H:%M:%S %:z"),
            it.precision.as_str(),
            it.confidence,
            it.location_decision,
            it.main.entry_name,
            it.output_rel
        );
    }
    ExitCode::SUCCESS
}

pub fn cmd_run(
    state: &Path,
    inputs: &[PathBuf],
    opts: &OptionArgs,
    quiet: bool,
    json: bool,
) -> ExitCode {
    let options = match opts.to_options() {
        Ok(o) => o,
        Err(e) => return fail(e),
    };
    let mut job = match Job::create(state, inputs, options) {
        Ok(j) => j,
        Err(JobError::OutputInUse { job_id }) => {
            eprintln!(
                "This output folder already has job {job_id}. Continue it with `mr-cli resume {job_id}`."
            );
            return ExitCode::from(2);
        }
        Err(e) => return fail(e),
    };
    println!("job {}", job.id());
    let _ = std::io::stdout().flush();
    drive(&mut job, quiet, json)
}

/// `zip`: `Some(volume size)` turns ZIP output on for the job first.
pub fn cmd_resume(
    state: &Path,
    id: &str,
    unlock: bool,
    zip: Option<Option<u64>>,
    quiet: bool,
    json: bool,
) -> ExitCode {
    let mut job = match Job::open(state, id) {
        Ok(j) => j,
        Err(e) => return fail(e),
    };
    if let Some(volume) = zip {
        let mut o = job.options().clone();
        o.zip_output = true;
        o.zip_volume_bytes = volume;
        if let Err(e) = job.set_options(o) {
            return fail(e);
        }
    }
    if let Some(why) = job.rebuilt() {
        eprintln!("note: {why}");
    }
    if unlock {
        match job.unlock() {
            Ok(n) => eprintln!("unlocked: {n} more items queued"),
            Err(e) => return fail(e),
        }
    }
    println!("job {}", job.id());
    let _ = std::io::stdout().flush();
    drive(&mut job, quiet, json)
}

pub fn cmd_retry(state: &Path, id: &str, quiet: bool, json: bool) -> ExitCode {
    let mut job = match Job::open(state, id) {
        Ok(j) => j,
        Err(e) => return fail(e),
    };
    let mut options = job.options().clone();
    options.test_hooks.fail_ids.clear();
    match job.retry_failed(Some(options)) {
        Ok(n) => eprintln!("{n} failed items queued again"),
        Err(e) => return fail(e),
    }
    drive(&mut job, quiet, json)
}

pub fn cmd_add_part(state: &Path, id: &str, path: &Path, json: bool) -> ExitCode {
    let mut job = match Job::open(state, id) {
        Ok(j) => j,
        Err(e) => return fail(e),
    };
    match job.add_part(path) {
        Ok(r) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&r).expect("json"));
            } else {
                println!(
                    "added {}: {} new items, {} written again (better time, location or overlay), {} removed; missing parts now {:?}. Continue with `mr-cli resume {}`.",
                    path.display(),
                    r.new_items,
                    r.requeued.len(),
                    r.removed,
                    r.missing_parts,
                    job.id()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

pub fn cmd_report(state: &Path, id: &str, json: bool) -> ExitCode {
    let job = match Job::open(state, id) {
        Ok(j) => j,
        Err(e) => return fail(e),
    };
    match job.report() {
        Ok(files) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "html": files.html, "csv": files.csv, "failed_items": files.failed_items,
                        "counts": job.counts().unwrap_or_default(),
                    }))
                    .expect("json")
                );
            } else {
                println!("{}", files.html.display());
                println!("{}", files.csv.display());
                println!("{}", files.failed_items.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

pub fn cmd_jobs(state: &Path, json: bool) -> ExitCode {
    let jobs = list_jobs(state);
    if json {
        println!("{}", serde_json::to_string_pretty(&jobs).expect("json"));
        return ExitCode::SUCCESS;
    }
    if jobs.is_empty() {
        println!("no jobs in {}", state.display());
    }
    for j in jobs {
        let counts: Vec<String> = j.counts.iter().map(|(k, v)| format!("{k} {v}")).collect();
        println!(
            "{}  {}  {:<16} {:<7} {}  [{}]",
            j.id,
            j.created_utc,
            j.status,
            j.profile,
            j.output_root,
            counts.join(", ")
        );
    }
    ExitCode::SUCCESS
}
