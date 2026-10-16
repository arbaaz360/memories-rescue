//! `mr-cli`: the Memories Rescue core and engine from a terminal, the way
//! the app drives them.
//!
//! * `analyze <inputs...>` (alias `index`): discovery, merged index, the
//!   FR-07 summary and the per-item match table.
//! * `order-check <inputs...>`: on a `2026a` export, per day, how often ZIP
//!   entry order agrees with JSON order (Q-03).
//! * `plan <inputs...> --dest D`: the dry run (FR-29).
//! * `run <inputs...> --dest D`: create a job and process it.
//! * `resume <job>`, `retry <job>`, `add-part <job> <path>`, `report <job>`, `jobs`.
//! * `bench index <inputs...>`: FR-06 / H-05 timings and peak memory, as JSON.
//!
//! With `MR_BENCH_JSON=<file>` set, every command appends one JSON line to
//! that file when it ends: wall time, time to the first finished item, peak
//! resident memory and the run outcome (`docs/perf/README.md`).
//!
//! Job state lives in `--state-dir` (default: the per-user app data folder).

mod jobs;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mr_core::discover::{Discovery, Export, discover};
use mr_core::fields::{DatePrecision, MediaType};
use mr_core::index::{ExportSummary, MergedIndex, Profile, build_index, load_json, summarize};
use mr_core::matcher::{
    Item, MatchOptions, MatchReport, OrderAgreement, OrderVerdict, match_export, order_agreement,
};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(
    name = "mr-cli",
    version,
    about = "Memories Rescue core, from the command line"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Analyse export ZIPs or folders: FR-07 summary and per-item match table.
    #[command(alias = "index")]
    Analyze {
        /// Export ZIP files and/or extracted folders.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
        /// The "Apply uncertain locations" setting (default off).
        #[arg(long)]
        apply_uncertain_locations: bool,
        /// Limit the item table to the first N rows (text mode).
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Measure, per day, whether ZIP entry order agrees with JSON order on a
    /// 2026a export (sid/mid give the ground truth). Validates the ordinal
    /// matching assumption of SRS section 3.6 (Q-03).
    OrderCheck {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Dry run (FR-29): totals, match breakdown, sample output paths,
    /// warnings and the space estimate. Writes nothing unless --export is given.
    Plan {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[command(flatten)]
        options: jobs::OptionArgs,
        /// Write the full preview as JSON to this file.
        #[arg(long)]
        export: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        /// Sample size in text mode.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Create a job for these inputs and process it.
    Run {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        #[command(flatten)]
        options: jobs::OptionArgs,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
    },
    /// Continue a job (after a pause, a crash, or with --unlocked).
    Resume {
        job: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Unlock the full version for this job first (FR-44).
        #[arg(long)]
        unlocked: bool,
        /// Turn ZIP output on for this job (FR-31); finished jobs are packed.
        #[arg(long)]
        zip_output: bool,
        /// ZIP volume size, as for `run` (implies --zip-output).
        #[arg(long, value_name = "SIZE")]
        zip_volume: Option<String>,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
    },
    /// Process the failed items of a job again (FR-39).
    Retry {
        job: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        quiet: bool,
        #[arg(long)]
        json: bool,
    },
    /// Add an export part that was missing (FR-40); then `resume`.
    AddPart {
        job: String,
        path: PathBuf,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Write the report (FR-38) into the job's output folder.
    Report {
        job: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// List the jobs in the state directory.
    Jobs {
        #[arg(long)]
        state_dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Benchmarks (docs/perf/README.md).
    Bench {
        #[command(subcommand)]
        what: Bench,
    },
}

#[derive(Subcommand)]
enum Bench {
    /// Discovery, merged index, JSON parse and matching over the inputs,
    /// timed, with peak memory (FR-06 AC1, H-05). Prints one JSON object.
    Index {
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
    },
    /// Check a job's output after a soak run (H-01, H-02): every finished
    /// item's file hashes to its record and reads back its metadata, the
    /// output folder holds exactly the recorded files, and from the journal,
    /// no finished item was started again. Prints one JSON object.
    Verify {
        job: String,
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
}

struct Run<'a> {
    export: &'a Export,
    summary: ExportSummary,
    report: MatchReport,
}

fn run_export<'a>(d: &'a Discovery, export: &'a Export, opts: &MatchOptions) -> Run<'a> {
    let index: MergedIndex = build_index(d, export);
    let json = load_json(d, &index);
    let summary = summarize(d, export, &index, json.as_ref());
    let entries = json.as_ref().map(|j| j.entries.as_slice()).unwrap_or(&[]);
    let report = match_export(entries, &index, opts);
    Run {
        export,
        summary,
        report,
    }
}

fn main() -> ExitCode {
    let started = jobs::process_start();
    let cli = Cli::parse();
    let name = command_name(&cli.command);
    let code = dispatch(cli);
    jobs::bench_line(name, started, code);
    code
}

fn command_name(c: &Command) -> &'static str {
    match c {
        Command::Analyze { .. } => "analyze",
        Command::OrderCheck { .. } => "order-check",
        Command::Plan { .. } => "plan",
        Command::Run { .. } => "run",
        Command::Resume { .. } => "resume",
        Command::Retry { .. } => "retry",
        Command::AddPart { .. } => "add-part",
        Command::Report { .. } => "report",
        Command::Jobs { .. } => "jobs",
        Command::Bench { .. } => "bench",
    }
}

fn bench_index(inputs: &[PathBuf]) -> ExitCode {
    use std::time::Instant;
    let t = Instant::now();
    let d = discover(inputs);
    let discover_s = t.elapsed().as_secs_f64();
    let Some(export) = d.exports.first() else {
        eprintln!("no export found");
        return ExitCode::FAILURE;
    };
    let t1 = Instant::now();
    let index = build_index(&d, export);
    let index_s = t1.elapsed().as_secs_f64();
    let t2 = Instant::now();
    let json = load_json(&d, &index);
    let json_s = t2.elapsed().as_secs_f64();
    let t3 = Instant::now();
    let summary = summarize(&d, export, &index, json.as_ref());
    let entries = json.as_ref().map(|j| j.entries.as_slice()).unwrap_or(&[]);
    let report = match_export(entries, &index, &MatchOptions::default());
    let match_s = t3.elapsed().as_secs_f64();
    let v = json!({
        "parts": summary.parts,
        "json_entries": summary.json_entries,
        "main_files": summary.main_files,
        "overlays": summary.overlays,
        "matched": report.items.len(),
        "profile": summary.profile.as_str(),
        "discover_s": discover_s,
        "index_s": index_s,
        "json_s": json_s,
        "match_s": match_s,
        "total_s": t.elapsed().as_secs_f64(),
        "peak_rss_bytes": mr_job::platform::peak_rss_bytes(),
    });
    println!("{}", serde_json::to_string_pretty(&v).expect("json"));
    ExitCode::SUCCESS
}

fn dispatch(cli: Cli) -> ExitCode {
    let state = |dir: &Option<PathBuf>| dir.clone().unwrap_or_else(jobs::default_state_dir);
    match cli.command {
        Command::Bench {
            what: Bench::Index { inputs },
        } => bench_index(&inputs),
        Command::Bench {
            what: Bench::Verify { job, state_dir },
        } => jobs::bench_verify(&state(&state_dir), &job),
        Command::Plan {
            inputs,
            options,
            export,
            json,
            limit,
        } => jobs::cmd_plan(&inputs, &options, json, export.as_deref(), limit),
        Command::Run {
            inputs,
            options,
            state_dir,
            quiet,
            json,
        } => jobs::cmd_run(&state(&state_dir), &inputs, &options, quiet, json),
        Command::Resume {
            job,
            state_dir,
            unlocked,
            zip_output,
            zip_volume,
            quiet,
            json,
        } => {
            let zip = match zip_volume.as_deref().map(jobs::parse_size).transpose() {
                Ok(v) => (zip_output || v.is_some()).then_some(v),
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::from(2);
                }
            };
            jobs::cmd_resume(&state(&state_dir), &job, unlocked, zip, quiet, json)
        }
        Command::Retry {
            job,
            state_dir,
            quiet,
            json,
        } => jobs::cmd_retry(&state(&state_dir), &job, quiet, json),
        Command::AddPart {
            job,
            path,
            state_dir,
            json,
        } => jobs::cmd_add_part(&state(&state_dir), &job, &path, json),
        Command::Report {
            job,
            state_dir,
            json,
        } => jobs::cmd_report(&state(&state_dir), &job, json),
        Command::Jobs { state_dir, json } => jobs::cmd_jobs(&state(&state_dir), json),
        Command::Analyze {
            inputs,
            json,
            apply_uncertain_locations,
            limit,
        } => {
            let d = discover(&inputs);
            let opts = MatchOptions {
                apply_uncertain_locations,
            };
            let runs: Vec<Run<'_>> = d.exports.iter().map(|e| run_export(&d, e, &opts)).collect();
            if json {
                let value = json!({
                    "ignored": d.ignored.iter().map(|i| json!({"path": i.path, "detail": i.detail})).collect::<Vec<_>>(),
                    "exports": runs.iter().map(|r| export_json(&d, r)).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&value).expect("json"));
            } else {
                print_index_text(&d, &runs, limit);
            }
            if runs.is_empty() {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Command::OrderCheck { inputs, json } => {
            let d = discover(&inputs);
            let opts = MatchOptions::default();
            let runs: Vec<Run<'_>> = d.exports.iter().map(|e| run_export(&d, e, &opts)).collect();
            let checks: Vec<(OrderAgreement, OrderVerdict)> = runs
                .iter()
                .map(|r| {
                    let a = order_agreement(&r.report);
                    let v = a.verdict(r.report.order.violations);
                    (a, v)
                })
                .collect();
            if json {
                let value = json!({
                    "exports": runs.iter().zip(&checks).map(|(r, (a, v))| order_json(r, a, *v)).collect::<Vec<_>>()
                });
                println!("{}", serde_json::to_string_pretty(&value).expect("json"));
            } else {
                for (run, (a, v)) in runs.iter().zip(&checks) {
                    print_order_text(run, a, *v);
                }
            }
            let measured = checks.iter().any(|(_, v)| *v != OrderVerdict::Unmeasurable);
            if runs.is_empty() || !measured {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
    }
}

// ---------------------------------------------------------------------------
// index
// ---------------------------------------------------------------------------

fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn part_label(d: &Discovery, id: usize) -> String {
    let p = d.part(id);
    match p.number() {
        Some(n) => format!("{} (part {n})", p.label),
        None => p.label.clone(),
    }
}

fn media_type_name(m: Option<MediaType>) -> &'static str {
    match m {
        Some(MediaType::Photo) => "photo",
        Some(MediaType::Video) => "video",
        None => "?",
    }
}

fn print_index_text(d: &Discovery, runs: &[Run<'_>], limit: usize) {
    println!(
        "Inputs: {} part(s) recognised, {} ignored",
        d.parts.len(),
        d.ignored.len()
    );
    for i in &d.ignored {
        println!("  ignored {}: {}", i.path.display(), i.detail);
    }
    if runs.is_empty() {
        println!("No export found.");
        return;
    }
    for run in runs {
        let s = &run.summary;
        let e = run.export;
        println!();
        println!(
            "Export {}: {}",
            e.id + 1,
            e.base.as_deref().unwrap_or("(unnamed)")
        );
        let parts: Vec<String> = d.parts_of(e).map(|p| part_label(d, p.id)).collect();
        println!("  Parts ({}): {}", s.parts, parts.join(", "));
        if !s.missing_parts.is_empty() {
            println!("  Missing parts: {:?}", s.missing_parts);
        }
        if s.truncated_parts > 0 {
            println!("  Truncated parts: {}", s.truncated_parts);
        }
        println!(
            "  Profile: {}   paths: exact {}, ordinal {}, metadata-only {}",
            s.profile.as_str(),
            s.profile_counts.exact,
            s.profile_counts.ordinal,
            s.profile_counts.metadata_only
        );
        // FR-13 AC2; the legacy message is printed with the issues.
        if let Some(note) = s.profile_message().filter(|_| s.profile != Profile::Legacy) {
            println!("  Note: {note}");
        }
        println!(
            "  JSON entries {}; main files {} ({} photos, {} videos); overlays {}; unrecognised {}; refused {}; duplicates {}",
            s.json_entries,
            s.main_files,
            s.photos,
            s.videos,
            s.overlays,
            s.unrecognised,
            s.refused,
            s.duplicates
        );
        match s.date_range {
            Some((lo, hi)) => println!(
                "  Date range: {} to {}; entries with location: {}",
                lo.format("%Y-%m-%d %H:%M:%S UTC"),
                hi.format("%Y-%m-%d %H:%M:%S UTC"),
                s.with_location
            ),
            None => println!(
                "  Date range: none; entries with location: {}",
                s.with_location
            ),
        }
        println!(
            "  Size: total {}, media {}; entry order: {}",
            human_bytes(s.total_bytes),
            human_bytes(s.media_bytes),
            if s.ordered {
                "yes (ZIP)"
            } else {
                "no (folder input)"
            }
        );
        for issue in &s.issues {
            println!("  ! {issue}");
        }
        let r = &run.report;
        println!(
            "  Match: exact {}, ordinal {}, none {}; location applied {}, none {}, uncertain {}; metadata_partial {}; missing media {}; orphan overlays {}; duplicate mains {}; token disagreements {}",
            r.counts.exact,
            r.counts.ordinal,
            r.counts.none,
            r.counts.location_applied,
            r.counts.location_none,
            r.counts.location_uncertain,
            r.counts.metadata_partial,
            r.missing_media().count(),
            r.orphan_overlays.len(),
            r.duplicate_mains.len(),
            r.token_disagreements
        );
        let o = &r.order;
        println!(
            "  ZIP order check: {} ({} day-crossing pairs, {} violations; JSON {})",
            if !o.applicable {
                "not applicable"
            } else if o.is_monotone() {
                "consistent with JSON order"
            } else {
                "NOT consistent with JSON order: positional matching refused"
            },
            o.checked,
            o.violations,
            if o.newest_first {
                "newest first"
            } else {
                "oldest first"
            }
        );
        println!();
        println!(
            "  {:>5}  {:<7}  {:<19}  {:<4}  {:<9}  {:<18}  {:<8}  {:<4}  main [part]",
            "json#", "conf", "capture (UTC)", "prec", "location", "status", "id", "ovl"
        );
        for item in r.items.iter().take(limit) {
            println!("  {}", item_row(d, item));
        }
        if r.items.len() > limit {
            println!(
                "  ... {} more items (use --limit or --json)",
                r.items.len() - limit
            );
        }
        if !r.unmatched.is_empty() {
            println!(
                "  Unmatched JSON entries: {} (first: {:?})",
                r.unmatched.len(),
                r.unmatched
                    .iter()
                    .take(5)
                    .map(|u| (u.json_index, u.reason))
                    .collect::<Vec<_>>()
            );
        }
    }
}

fn item_row(d: &Discovery, item: &Item) -> String {
    let prec = match item.date.precision {
        DatePrecision::Second => "sec",
        DatePrecision::Day => "day",
    };
    format!(
        "{:>5}  {:<7}  {:<19}  {:<4}  {:<9}  {:<18}  {:<8}  {:<4}  {} [{}]",
        item.json_index
            .map(|i| i.to_string())
            .unwrap_or_else(|| "-".into()),
        item.confidence.as_str(),
        item.date.utc.format("%Y-%m-%d %H:%M:%S"),
        prec,
        item.location_decision.as_str(),
        item.status.as_str(),
        &item.id[..item.id.len().min(8)],
        if item.overlay.is_some() { "yes" } else { "no" },
        item.main.name,
        d.part(item.main.part).label
    )
}

fn export_json(d: &Discovery, run: &Run<'_>) -> Value {
    let s = &run.summary;
    let r = &run.report;
    json!({
        "base": run.export.base,
        "parts": d.parts_of(run.export).map(|p| json!({
            "label": p.label,
            "path": p.path,
            "number": p.number(),
            "primary": p.has_json,
            "zip": p.is_zip(),
            "truncated": p.is_truncated(),
            "media_count": p.media_count,
            "attached_by_content": run.export.attached_by_content.contains(&p.id),
        })).collect::<Vec<_>>(),
        "summary": {
            "profile": s.profile.as_str(),
            "paths": {"exact": s.profile_counts.exact, "ordinal": s.profile_counts.ordinal, "metadata_only": s.profile_counts.metadata_only},
            "profile_message": s.profile_message(),
            "parts": s.parts,
            "missing_parts": s.missing_parts,
            "truncated_parts": s.truncated_parts,
            "json_entries": s.json_entries,
            "main_files": s.main_files,
            "photos": s.photos,
            "videos": s.videos,
            "overlays": s.overlays,
            "unrecognised": s.unrecognised,
            "refused": s.refused,
            "duplicates": s.duplicates,
            "date_range": s.date_range.map(|(lo, hi)| json!([lo.to_rfc3339(), hi.to_rfc3339()])),
            "with_location": s.with_location,
            "total_bytes": s.total_bytes,
            "media_bytes": s.media_bytes,
            "ordered": s.ordered,
            "issues": s.issues.iter().map(|i| json!({"code": i.code.as_str(), "detail": i.detail})).collect::<Vec<_>>(),
        },
        "match": {
            "counts": {
                "exact": r.counts.exact, "ordinal": r.counts.ordinal, "none": r.counts.none,
                "location_applied": r.counts.location_applied, "location_none": r.counts.location_none,
                "location_uncertain": r.counts.location_uncertain, "metadata_partial": r.counts.metadata_partial,
                "missing_media": r.missing_media().count(), "orphan_overlays": r.orphan_overlays.len(),
                "duplicate_mains": r.duplicate_mains.len(), "token_disagreements": r.token_disagreements,
            },
            "order": {"applicable": r.order.applicable, "monotone": r.order.is_monotone(), "newest_first": r.order.newest_first, "checked": r.order.checked, "violations": r.order.violations},
            "items": r.items.iter().map(|i| json!({
                "json_index": i.json_index,
                "id": i.id,
                "main": {"part": d.part(i.main.part).label, "entry": i.main.index, "name": i.main.name, "size": i.main.size},
                "overlay": i.overlay.as_ref().map(|o| json!({"part": d.part(o.part).label, "entry": o.index, "name": o.name})),
                "media_type": media_type_name(i.media_type),
                "capture_utc": i.date.utc.to_rfc3339(),
                "time_precision": match i.date.precision { DatePrecision::Second => "second", DatePrecision::Day => "day" },
                "location": i.location.map(|l| json!({"lat": l.lat, "lon": l.lon})),
                "location_decision": i.location_decision.as_str(),
                "confidence": i.confidence.as_str(),
                "reason": i.reason.to_string(),
                "status": i.status.as_str(),
            })).collect::<Vec<_>>(),
            "unmatched": r.unmatched.iter().map(|u| json!({"json_index": u.json_index, "reason": format!("{:?}", u.reason)})).collect::<Vec<_>>(),
            "orphan_overlays": r.orphan_overlays.iter().map(|o| &o.name).collect::<Vec<_>>(),
        }
    })
}

// ---------------------------------------------------------------------------
// order-check
// ---------------------------------------------------------------------------

fn verdict_text(v: OrderVerdict) -> &'static str {
    match v {
        OrderVerdict::Unmeasurable => {
            "UNMEASURABLE: no day with two or more exactly matched items."
        }
        OrderVerdict::Holds => "HOLDS: ZIP entry order follows JSON order on every day measured.",
        OrderVerdict::MostlyHolds => {
            "MOSTLY HOLDS: a few days disagree; ordinal matching would be wrong for those items."
        }
        OrderVerdict::Fails => {
            "FAILS: ZIP entry order does not follow JSON order; ordinal matching must not be used."
        }
    }
}

fn order_json(run: &Run<'_>, a: &OrderAgreement, v: OrderVerdict) -> Value {
    json!({
        "base": run.export.base,
        "profile": run.summary.profile.as_str(),
        "verdict": format!("{v:?}"),
        "exact_items": a.exact_items,
        "total_items": run.report.items.len(),
        "json_newest_first": run.report.order.newest_first,
        "cross_day_pairs": run.report.order.checked,
        "cross_day_violations": run.report.order.violations,
        "days_checked": a.days_checked,
        "days_agreeing": a.days_agreeing,
        "items_in_checked_days": a.items_in_checked_days,
        "adjacent_pairs": a.adjacent_pairs,
        "adjacent_agreeing": a.adjacent_agreeing,
        "days": a.days.iter().map(|d| json!({
            "day": format!("{}/{}", d.date, media_type_name(d.media_type)),
            "items": d.items, "agrees": d.agrees,
            "adjacent_pairs": d.adjacent_pairs, "adjacent_agreeing": d.adjacent_agreeing,
            "json_order": d.json_order, "zip_order": d.zip_order,
        })).collect::<Vec<_>>(),
    })
}

fn print_order_text(run: &Run<'_>, a: &OrderAgreement, v: OrderVerdict) {
    let s = &run.summary;
    let o = &run.report.order;
    println!(
        "Export: {} ({} parts, profile {})",
        run.export.base.as_deref().unwrap_or("(unnamed)"),
        s.parts,
        s.profile.as_str()
    );
    println!(
        "  Exact (sid/mid) items: {} of {}",
        a.exact_items,
        run.report.items.len()
    );
    if a.exact_items == 0 {
        println!("  Cannot measure: no entry carries sid/mid, so there is no ground truth here.");
        println!("  This check needs a 2026a export (Download Link present).");
        return;
    }
    println!(
        "  JSON array order: {}",
        if o.newest_first {
            "newest first"
        } else {
            "oldest first"
        }
    );
    println!(
        "  Across days: {} day-crossing adjacent file pairs, {} against the JSON direction",
        o.checked, o.violations
    );
    if a.days_checked == 0 {
        println!(
            "  Within days: no day has two or more exact items, so the within-day order cannot be measured."
        );
        println!("  Verdict: {}", verdict_text(v));
        return;
    }
    let pct = |x: usize, y: usize| {
        if y == 0 {
            100.0
        } else {
            x as f64 * 100.0 / y as f64
        }
    };
    println!(
        "  Within days: {} of {} multi-item days agree exactly ({:.1}%); {} of {} adjacent pairs in ZIP order ({:.1}%); {} items involved",
        a.days_agreeing,
        a.days_checked,
        pct(a.days_agreeing, a.days_checked),
        a.adjacent_agreeing,
        a.adjacent_pairs,
        pct(a.adjacent_agreeing, a.adjacent_pairs),
        a.items_in_checked_days
    );
    println!("  Verdict (SRS 3.6 step 2 assumption): {}", verdict_text(v));
    let disagreeing: Vec<_> = a.days.iter().filter(|d| !d.agrees).collect();
    if !disagreeing.is_empty() {
        println!("  Disagreeing days (first 20):");
        for d in disagreeing.iter().take(20) {
            println!(
                "    {}/{}: {} items; JSON {:?} vs ZIP {:?}",
                d.date,
                media_type_name(d.media_type),
                d.items,
                d.json_order,
                d.zip_order
            );
        }
    }
}
