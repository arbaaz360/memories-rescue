//! The engine commands: `plan`, `run`, `resume`, `retry`, `add-part`,
//! `report`, `jobs`. They drive `mr-job` exactly as the app will.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Mutex;

use clap::{Args, ValueEnum};
use mr_job::{
    ClipRejoin, Control, ItemOutcome, Job, JobError, Options, Outcome, OverlayMode, Progress,
    ProgressSink, list_jobs, plan,
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
    /// Licensed: process every item (otherwise the first 100, FR-44).
    #[arg(long)]
    pub unlocked: bool,
    /// Write the ordinal match's location even when the day group disagrees.
    #[arg(long)]
    pub apply_uncertain_locations: bool,
    /// "Maximise Windows compatibility" (H.264 output when compositing).
    #[arg(long)]
    pub windows_compat: bool,
    /// Rejoin consecutive clips (recorded; joining arrives with the media component).
    #[arg(long)]
    pub clip_rejoin: bool,
    /// Clip-rejoin years, `2017-2021` or `all`.
    #[arg(long, default_value = "2017-2021")]
    pub clip_years: String,
    /// Package the output as ZIP (recorded; not implemented yet).
    #[arg(long)]
    pub zip_output: bool,
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
        };
        o.zip_output = self.zip_output;
        o.ignore_space_check = self.ignore_space_check;
        o.test_hooks.fail_ids = self.test.test_fail_ids.clone();
        o.test_hooks.delay_ms = self.test.test_delay_ms;
        o.test_hooks.fake_free_bytes = self.test.test_fake_free_bytes;
        o.test_hooks.recheck_every = self.test.test_recheck_every;
        Ok(o)
    }
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
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata)
                .join("NovaByteWorks")
                .join("Memories Rescue");
        }
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
}

fn drive(job: &mut Job, quiet: bool, json: bool) -> ExitCode {
    let sink = ConsoleSink::new(quiet);
    match job.run(&sink, &Control::new()) {
        Ok(o) => {
            if matches!(o, Outcome::Completed(_)) {
                if let Err(e) = job.report() {
                    eprintln!("warning: report not written: {e}");
                }
            }
            print_outcome(job, &o, json);
            outcome_exit(&o)
        }
        Err(e) => fail(e),
    }
}

pub fn cmd_plan(
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
    let p = match plan(inputs, &options) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    if let Some(path) = export {
        if let Err(e) = std::fs::write(path, serde_json::to_vec_pretty(&p).expect("json")) {
            return fail(e);
        }
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

pub fn cmd_resume(state: &Path, id: &str, unlock: bool, quiet: bool, json: bool) -> ExitCode {
    let mut job = match Job::open(state, id) {
        Ok(j) => j,
        Err(e) => return fail(e),
    };
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
