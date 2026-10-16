//! `analyze`, `plan`, `estimate_space`, `volume_info` and
//! `export_preview_report`: the dry run (FR-07, FR-13, FR-28, FR-29) mapped
//! from `mr_job::plan_for` onto the contract's shapes. Plans are made for the
//! compositor jobs will use, so the preview's warnings match what a run does:
//! captions burned in with the real compositor, `ENCODER_UNAVAILABLE` when
//! this computer has no working H.264 encoder (videos keep their caption
//! beside them), and the FR-51 HEVC notice as its own field.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use mr_job::{MediaKind, Plan, PlannedItem};

use crate::ipc::{
    ClipGroup, ConfidenceBreakdown, DateRange, ErrorCode, ExportCounts, ExportSummary, FolderNode,
    FreeLimitInfo, HevcNotice, IpcError, IpcResult, Issue, JobOptions, LocationBreakdown,
    LocationDecision, MatchConfidence, MediaType, PartInfo, PartStatus, PlanItem, PlanPreview,
    PlanTotals, ProfileCounts, SpaceEstimate, TimePrecision, VolumeInfo,
};

use super::{Engine, io_error, job_error, settings};

/// A plan kept for `export_preview_report` and the live space estimate.
#[derive(Clone)]
pub struct CachedPlan {
    pub inputs: Vec<PathBuf>,
    pub plan: std::sync::Arc<Plan>,
}

const SAMPLES: usize = 22;

impl Engine {
    fn plan_for(&self, inputs: &[PathBuf], options: &mr_job::Options) -> IpcResult<Plan> {
        mr_job::plan_for(inputs, options, self.compositor.as_ref()).map_err(job_error)
    }

    /// Why videos cannot be encoded on this computer, when the plan needs it
    /// (a caption to burn into a video, a clip group to join, or Windows
    /// compatibility with videos). The encoder check runs ffmpeg once and is
    /// cached for the session.
    fn video_encoder_problem(&self, plan: &Plan, o: &mr_job::Options) -> Option<String> {
        let media = self.media.as_ref()?;
        let videos = plan.items.iter().any(|i| i.kind == MediaKind::Video);
        let needs = videos
            && ((o.overlay_mode.burns_in()
                && plan
                    .items
                    .iter()
                    .any(|i| i.kind == MediaKind::Video && i.overlay.is_some()))
                || plan.clip_groups.iter().any(|g| g.enabled)
                || o.windows_compat);
        if !needs {
            return None;
        }
        media.encoder_plan().err().map(|e| {
            tracing::warn!("no video encoder: {}", e.code);
            e.detail
        })
    }

    pub fn analyze(&self, input_ids: &[String]) -> IpcResult<ExportSummary> {
        let inputs = self.input_paths(input_ids)?;
        let s = self.get_settings();
        let opts = settings::to_engine(
            &settings::options_from_settings(&s),
            &s.output_template,
            self.license.unlocked(),
        );
        let plan = self.plan_for(&inputs, &opts)?;
        let summary = self.summary_of(input_ids, &plan);
        *self.last_analysis.lock().expect("analysis") = Some(CachedPlan {
            inputs,
            plan: std::sync::Arc::new(plan),
        });
        Ok(summary)
    }

    fn summary_of(&self, input_ids: &[String], plan: &Plan) -> ExportSummary {
        let reg = self.inputs.lock().expect("inputs");
        let t = &plan.totals;
        let mut entries_per_part: BTreeMap<u32, u64> = BTreeMap::new();
        for it in &plan.items {
            if let Some(n) = it.main.part_number {
                *entries_per_part.entry(n).or_default() += 1;
            }
        }
        let max_part = plan
            .parts
            .iter()
            .filter_map(|p| p.number)
            .chain(plan.missing_parts.iter().copied())
            .max()
            .unwrap_or(1);
        let mut parts = Vec::new();
        for n in 1..=max_part {
            if let Some(p) = plan.parts.iter().find(|p| p.number == Some(n)) {
                let input = reg.list.iter().find(|i| {
                    Path::new(&i.path) == Path::new(&p.path)
                        || Path::new(&p.path).starts_with(&i.path)
                });
                parts.push(PartInfo {
                    part_number: n,
                    input_id: input.map(|i| i.id.clone()),
                    name: Some(p.label.clone()),
                    size_bytes: std::fs::metadata(&p.path)
                        .map(|m| if m.is_file() { m.len() } else { 0 })
                        .ok()
                        .filter(|s| *s > 0)
                        .or(input.map(|i| i.size_bytes))
                        .unwrap_or(0),
                    status: if p.truncated {
                        PartStatus::Truncated
                    } else {
                        PartStatus::Ok
                    },
                    entries: entries_per_part.get(&n).copied().unwrap_or(0),
                });
            } else if plan.missing_parts.contains(&n) {
                parts.push(PartInfo {
                    part_number: n,
                    input_id: None,
                    name: None,
                    size_bytes: 0,
                    status: PartStatus::Missing,
                    entries: 0,
                });
            }
        }
        let legacy = plan.profile == "legacy";
        let usable: Vec<String> = input_ids
            .iter()
            .filter(|id| reg.get(id).is_some())
            .cloned()
            .collect();
        let total_bytes = usable
            .iter()
            .filter_map(|id| reg.get(id))
            .map(|i| i.size_bytes)
            .sum();
        ExportSummary {
            export_id: format!(
                "exp-{}",
                plan.output_root.to_string_lossy().len() + plan.inputs.len()
            ),
            input_ids: usable,
            profile: plan.profile.clone(),
            profile_counts: ProfileCounts {
                exact: t.exact as u64,
                ordinal: t.ordinal as u64,
                day_only: t.none as u64,
                metadata_only: if legacy { t.json_entries as u64 } else { 0 },
            },
            parts,
            missing_parts: plan.missing_parts.clone(),
            counts: ExportCounts {
                entries: t.json_entries as u64,
                photos: t.photos as u64,
                videos: t.videos as u64,
                overlays: t.with_overlay as u64,
                orphan_overlays: t.orphan_overlays as u64,
                with_location: plan
                    .items
                    .iter()
                    .filter(|i| i.location_decision != "none")
                    .count() as u64,
                unrecognised: t.unrecognised as u64,
            },
            date_range: DateRange {
                first: plan.date_range.map(|(a, _)| a.date_naive().to_string()),
                last: plan.date_range.map(|(_, b)| b.date_naive().to_string()),
            },
            total_bytes,
            issues: issues_of(plan, false),
            blocked: legacy || plan.items.is_empty(),
            hevc_notice: hevc_notice_of(plan),
        }
    }

    pub fn plan(&self, input_ids: &[String], options: &JobOptions) -> IpcResult<PlanPreview> {
        let inputs = self.input_paths(input_ids)?;
        let s = self.get_settings();
        let opts = settings::to_engine(options, &s.output_template, self.license.unlocked());
        let plan = self.plan_for(&inputs, &opts)?;
        let plan_id = uuid::Uuid::new_v4().to_string();
        let samples = pick_samples(&plan);
        let thumbs = self.write_thumbnails(&plan_id, &inputs, &samples);
        // Captions are burned in by the real compositor. Without one (no
        // sidecar) they are saved beside the files and the preview says so;
        // without a working video encoder only the videos' captions are.
        let burn_in = opts.overlay_mode.burns_in() && self.burns_in();
        let encoder_problem = self.video_encoder_problem(&plan, &opts);
        let t = &plan.totals;
        let video_overlays = plan
            .items
            .iter()
            .filter(|i| i.kind == MediaKind::Video && i.overlay.is_some())
            .count() as u64;
        let composited = match (burn_in, &encoder_problem) {
            (false, _) => 0,
            (true, Some(_)) => t.with_overlay as u64 - video_overlays,
            (true, None) => t.with_overlay as u64,
        };
        let mut warnings = issues_of(&plan, true);
        if let Some(detail) = &encoder_problem {
            warnings.push(Issue {
                code: ErrorCode::EncoderUnavailable,
                message: detail.clone(),
                count: Some(video_overlays).filter(|n| *n > 0),
                part_number: None,
            });
        }
        let mut location = LocationBreakdown::default();
        for it in &plan.items {
            match it.location_decision.as_str() {
                "applied" => location.applied += 1,
                "uncertain" => location.uncertain += 1,
                _ => location.none += 1,
            }
        }
        let preview = PlanPreview {
            plan_id: plan_id.clone(),
            totals: PlanTotals {
                items: t.items as u64,
                photos: t.photos as u64,
                videos: t.videos as u64,
                overlays_to_composite: composited,
                originals_kept: if opts.keep_originals
                    && opts.overlay_mode == mr_job::OverlayMode::BurnInKeepOriginals
                {
                    composited
                } else {
                    0
                },
                missing_media: t.missing_media as u64,
                unrecognised: t.unrecognised as u64,
            },
            confidence: ConfidenceBreakdown {
                exact: t.exact as u64,
                ordinal: t.ordinal as u64,
                none: t.none as u64,
            },
            location,
            tree: tree_of(&plan, &opts),
            samples: samples
                .iter()
                .zip(thumbs)
                .map(|(it, thumb)| plan_item(it, thumb))
                .collect(),
            clip_groups: plan
                .clip_groups
                .iter()
                .map(|g| clip_group(g, &plan))
                .collect(),
            warnings,
            space: space_of(&plan.space, &opts.dest),
            estimated_seconds: estimate_seconds(&plan, &opts),
            free_limit: FreeLimitInfo {
                applies: !opts.unlocked,
                limit: mr_job::FREE_TIER_ITEMS as u64,
                items_beyond: t.skipped_free_limit as u64,
            },
            hevc_notice: hevc_notice_of(&plan),
        };
        let cached = CachedPlan {
            inputs,
            plan: std::sync::Arc::new(plan),
        };
        self.plans
            .lock()
            .expect("plans")
            .insert(plan_id, cached.clone());
        *self.last_analysis.lock().expect("analysis") = Some(cached);
        Ok(preview)
    }

    /// FR-28 without re-matching: the last plan of these inputs, re-estimated for `options`.
    pub fn estimate_space(
        &self,
        input_ids: &[String],
        options: &JobOptions,
    ) -> IpcResult<SpaceEstimate> {
        let inputs = self.input_paths(input_ids)?;
        let s = self.get_settings();
        let opts = settings::to_engine(options, &s.output_template, self.license.unlocked());
        let cached = self
            .last_analysis
            .lock()
            .expect("analysis")
            .clone()
            .filter(|c| c.inputs == inputs);
        let plan = match cached {
            Some(c) => c.plan,
            None => {
                let p = std::sync::Arc::new(self.plan_for(&inputs, &opts)?);
                *self.last_analysis.lock().expect("analysis") = Some(CachedPlan {
                    inputs,
                    plan: p.clone(),
                });
                p
            }
        };
        Ok(space_of(&estimate(&plan.items, &opts), &opts.dest))
    }

    pub fn export_preview_report(&self, plan_id: &str) -> IpcResult<String> {
        let cached = self
            .plans
            .lock()
            .expect("plans")
            .get(plan_id)
            .cloned()
            .ok_or_else(|| {
                IpcError::unknown("That preview is no longer available; preview again.")
            })?;
        let plan = &cached.plan;
        let data = preview_report_data(plan);
        let html = mr_report::render_html(&data);
        let dir = plan.output_root.join(mr_job::layout::REPORT_DIR);
        let (dir, path) = match std::fs::create_dir_all(&dir) {
            Ok(()) => (dir.clone(), dir.join("preview-report.html")),
            Err(_) => {
                let d = self.paths.state_dir.join("previews");
                std::fs::create_dir_all(&d).map_err(|e| io_error(&d, &e))?;
                (d.clone(), d.join(format!("preview-report-{plan_id}.html")))
            }
        };
        let tmp = dir.join(".preview-report.tmp");
        std::fs::File::create(&tmp)
            .and_then(|mut f| f.write_all(html.as_bytes()))
            .map_err(|e| io_error(&tmp, &e))?;
        std::fs::rename(&tmp, &path).map_err(|e| io_error(&path, &e))?;
        self.issue_path(&path);
        Ok(path.to_string_lossy().into_owned())
    }

    /// Small preview images for sample photos, copied out of the export into
    /// the app cache (the asset protocol scope). Videos get none.
    fn write_thumbnails(
        &self,
        plan_id: &str,
        inputs: &[PathBuf],
        samples: &[&PlannedItem],
    ) -> Vec<Option<String>> {
        let dir = self.paths.cache_dir.join("thumbnails").join(plan_id);
        if std::fs::create_dir_all(&dir).is_err() {
            return vec![None; samples.len()];
        }
        prune_thumbnails(&self.paths.cache_dir.join("thumbnails"), plan_id);
        let d = mr_core::discover(inputs);
        samples
            .iter()
            .map(|it| {
                if it.kind != MediaKind::Photo {
                    return None;
                }
                let part = d
                    .parts
                    .iter()
                    .find(|p| p.path.to_string_lossy() == it.main.part_path)?;
                let entry = part.find(&it.main.entry_name)?;
                if entry.size > 25_000_000 {
                    return None;
                }
                let out = dir.join(format!("{}.{}", it.key, it.ext));
                part.read_entry(&entry, |r| {
                    let mut f = std::fs::File::create(&out)?;
                    std::io::copy(r, &mut f)?;
                    Ok(())
                })
                .ok()?;
                Some(out.to_string_lossy().into_owned())
            })
            .collect()
    }

    pub fn volume_info(&self, path: &str) -> IpcResult<VolumeInfo> {
        let p = PathBuf::from(path);
        let existing = existing_ancestor(&p).ok_or_else(|| {
            IpcError::new(
                ErrorCode::DestUnavailable,
                "The output drive is not available.",
            )
            .with_detail(path.to_string())
        })?;
        let free = fs4::available_space(&existing).map_err(|e| io_error(&existing, &e))?;
        let total = fs4::total_space(&existing).map_err(|e| io_error(&existing, &e))?;
        Ok(VolumeInfo {
            path: path.to_string(),
            volume: volume_label(&p),
            free_bytes: free,
            total_bytes: total,
            writable: writable(&existing),
        })
    }
}

/// Keeps the five newest preview folders.
fn prune_thumbnails(root: &Path, keep: &str) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let mut dirs: Vec<(std::time::SystemTime, PathBuf)> = rd
        .flatten()
        .filter(|e| e.file_name().to_string_lossy() != keep)
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.0));
    for (_, d) in dirs.into_iter().skip(4) {
        let _ = std::fs::remove_dir_all(d);
    }
}

pub fn existing_ancestor(p: &Path) -> Option<PathBuf> {
    let mut cur = Some(p);
    while let Some(c) = cur {
        if c.exists() {
            return Some(c.to_path_buf());
        }
        cur = c.parent();
    }
    None
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".mr-write-probe-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// "D:" on Windows, the volume name under /Volumes on macOS, else the system disk.
pub fn volume_label(p: &Path) -> String {
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    if b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return format!("{}:", (b[0] as char).to_ascii_uppercase());
    }
    if let Some(rest) = s.strip_prefix("/Volumes/") {
        return rest.split('/').next().unwrap_or(rest).to_string();
    }
    if cfg!(target_os = "macos") {
        "Macintosh HD".to_string()
    } else {
        "/".to_string()
    }
}

/// Same formula as mr-job's FR-28 estimate, for other options on the same items.
fn estimate(items: &[PlannedItem], o: &mr_job::Options) -> mr_job::SpaceEstimate {
    use mr_job::space::{METADATA_OVERHEAD, SpaceInputs, available_space};
    let mut inputs = SpaceInputs {
        workers: o.worker_count(),
        zip_output: o.zip_output,
        ..SpaceInputs::default()
    };
    for it in items
        .iter()
        .filter(|i| o.unlocked || i.chrono_rank < mr_job::FREE_TIER_ITEMS)
    {
        let overlay = it.overlay.as_ref().map(|x| x.size).unwrap_or(0);
        inputs.final_bytes += it.main.size + METADATA_OVERHEAD;
        if o.overlay_mode != mr_job::OverlayMode::Ignore {
            inputs.final_bytes += overlay;
        }
        if o.keep_originals
            && o.overlay_mode == mr_job::OverlayMode::BurnInKeepOriginals
            && it.overlay.is_some()
        {
            inputs.originals_bytes += it.main.size + overlay + METADATA_OVERHEAD;
        }
        inputs.largest_item = inputs.largest_item.max(it.main.size + overlay);
    }
    mr_job::SpaceEstimate::compute(
        &inputs,
        available_space(&mr_job::layout::output_root(&o.dest)),
    )
}

pub fn space_of(s: &mr_job::SpaceEstimate, dest: &Path) -> SpaceEstimate {
    SpaceEstimate {
        output_bytes: s.final_bytes,
        originals_bytes: s.originals_bytes,
        temp_bytes: s.peak_temp_bytes,
        zip_bytes: s.zip_bytes,
        total_bytes: s.required_bytes,
        // Unknown free space never blocks (mr-job's rule); report it as plenty.
        free_bytes: s.free_bytes.unwrap_or(u64::MAX / 4),
        sufficient: s.sufficient,
        shortfall_bytes: s.shortfall_bytes,
        volume: volume_label(dest),
    }
}

/// Metadata-only throughput of NFR-PERF-03 (20 photos/s, 5 videos/s) for what will run.
fn estimate_seconds(plan: &Plan, o: &mr_job::Options) -> f64 {
    let run: Vec<&PlannedItem> = plan
        .items
        .iter()
        .filter(|i| o.unlocked || i.chrono_rank < mr_job::FREE_TIER_ITEMS)
        .collect();
    let photos = run.iter().filter(|i| i.kind == MediaKind::Photo).count() as f64;
    let videos = run.len() as f64 - photos;
    (photos / 20.0 + videos / 5.0 + 5.0).round()
}

/// Plan warnings as contract issues; FREE_LIMIT is shown from `free_limit` instead.
fn issues_of(plan: &Plan, for_preview: bool) -> Vec<Issue> {
    let t = &plan.totals;
    let mut out = Vec::new();
    for n in &plan.missing_parts {
        out.push(Issue {
            code: ErrorCode::PartMissing,
            message: format!("Part {n} appears to be missing."),
            count: (plan.missing_parts.len() == 1).then_some(t.missing_media as u64),
            part_number: Some(*n),
        });
    }
    for w in &plan.warnings {
        let (code, count, part) = match w.code.as_str() {
            // FREE_LIMIT and HEVC_PLAYBACK have fields of their own.
            "PART_MISSING" | "FREE_LIMIT" | "HEVC_PLAYBACK" => continue,
            "COMPOSITE_PENDING" if !for_preview => continue,
            // Overlays cannot be burned in yet: they are saved beside the files.
            "COMPOSITE_PENDING" => (
                ErrorCode::EncoderUnavailable,
                Some(t.with_overlay as u64),
                None,
            ),
            "MATCH_PARTIAL" => (
                ErrorCode::MatchPartial,
                Some(t.metadata_partial as u64).filter(|n| *n > 0),
                None,
            ),
            "LOCATION_UNCERTAIN" => (
                ErrorCode::LocationUncertain,
                Some(t.location_uncertain as u64),
                None,
            ),
            "PART_TRUNCATED" => (ErrorCode::PartTruncated, None, part_number_in(&w.detail)),
            "ENTRY_REFUSED" => (
                ErrorCode::PartTruncated,
                Some(t.refused as u64),
                part_number_in(&w.detail),
            ),
            "JSON_MISSING" => (ErrorCode::JsonMissing, Some(t.items as u64), None),
            other => (ErrorCode::from_engine(other), None, None),
        };
        if out
            .iter()
            .any(|i: &Issue| i.code == code && i.count == count && code != ErrorCode::PartTruncated)
        {
            continue;
        }
        out.push(Issue {
            code,
            message: w.detail.clone(),
            count,
            part_number: part,
        });
    }
    out
}

fn part_number_in(detail: &str) -> Option<u32> {
    let i = detail.find("Part ")? + 5;
    detail[i..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// About 22 items spread over the date range, plus the first day-only and the
/// first uncertain-location item so the preview shows every kind of outcome.
fn pick_samples(plan: &Plan) -> Vec<&PlannedItem> {
    let mut sorted: Vec<&PlannedItem> = plan.items.iter().collect();
    sorted.sort_by_key(|i| i.chrono_rank);
    if sorted.len() <= SAMPLES + 2 {
        return sorted;
    }
    let mut picks: Vec<usize> = (0..SAMPLES)
        .map(|k| k * (sorted.len() - 1) / (SAMPLES - 1))
        .collect();
    if let Some(i) = sorted
        .iter()
        .position(|i| i.precision == mr_job::Precision::Day)
    {
        picks.push(i);
    }
    if let Some(i) = sorted
        .iter()
        .position(|i| i.location_decision == "uncertain")
    {
        picks.push(i);
    }
    picks.sort_unstable();
    picks.dedup();
    picks.into_iter().map(|i| sorted[i]).collect()
}

fn plan_item(it: &PlannedItem, thumbnail: Option<String>) -> PlanItem {
    PlanItem {
        item_id: it.key.clone(),
        media_type: media_type(it.kind),
        source_name: it
            .main
            .entry_name
            .rsplit('/')
            .next()
            .unwrap_or(&it.main.entry_name)
            .to_string(),
        planned_path: it.output_rel.clone(),
        capture_time: Some(it.local().to_rfc3339()),
        time_precision: match it.precision {
            mr_job::Precision::Second => TimePrecision::Second,
            mr_job::Precision::Day => TimePrecision::Day,
        },
        latitude: it.gps.map(|g| g.0),
        longitude: it.gps.map(|g| g.1),
        match_confidence: match it.confidence.as_str() {
            "exact" => MatchConfidence::Exact,
            "ordinal" => MatchConfidence::Ordinal,
            _ => MatchConfidence::None,
        },
        location_decision: match it.location_decision.as_str() {
            "applied" => LocationDecision::Applied,
            "uncertain" => LocationDecision::Uncertain,
            _ => LocationDecision::None,
        },
        has_overlay: it.overlay.is_some(),
        thumbnail_path: thumbnail,
    }
}

pub fn media_type(k: MediaKind) -> MediaType {
    match k {
        MediaKind::Photo => MediaType::Photo,
        MediaKind::Video => MediaType::Video,
    }
}

/// `group_id` is mr-job's `ClipGroup::id` (the first clip's item key):
/// the window sends the unticked ones back as `excluded_clip_groups`, which
/// become `ClipRejoin::skip_groups`, which mr-job matches against that id.
fn clip_group(g: &mr_job::plan::ClipGroup, plan: &Plan) -> ClipGroup {
    let first = plan.items.iter().find(|i| i.key == g.id);
    ClipGroup {
        group_id: g.id.clone(),
        item_ids: g.items.clone(),
        total_seconds: g.total_secs,
        planned_path: g.output_rel.clone(),
        start_time: first
            .map(|i| i.local().to_rfc3339())
            .or_else(|| g.start_utc.map(|t| t.to_rfc3339()))
            .unwrap_or_default(),
    }
}

/// FR-51: mr-job's `HEVC_PLAYBACK` warning ("3 of 16 sampled videos are
/// HEVC, ...") as the contract's notice.
pub fn hevc_notice_of(plan: &Plan) -> Option<HevcNotice> {
    let w = plan.warnings.iter().find(|w| w.code == "HEVC_PLAYBACK")?;
    let mut numbers = w
        .detail
        .split(|c: char| !c.is_ascii_digit())
        .filter(|n| !n.is_empty())
        .filter_map(|n| n.parse::<u64>().ok());
    let hevc = numbers.next()?;
    let sampled = numbers.next().unwrap_or(hevc).max(hevc);
    Some(HevcNotice { hevc, sampled })
}

/// Memories Rescue / YYYY / MM (+ originals) with counts and three names each, plus _report.
fn tree_of(plan: &Plan, o: &mr_job::Options) -> FolderNode {
    let originals = o.keep_originals && o.overlay_mode == mr_job::OverlayMode::BurnInKeepOriginals;
    let mut years: BTreeMap<String, BTreeMap<String, (Vec<String>, u64)>> = BTreeMap::new();
    let mut sorted: Vec<&PlannedItem> = plan.items.iter().collect();
    sorted.sort_by_key(|i| i.chrono_rank);
    for it in sorted {
        let mut parts = it.output_rel.split('/');
        let (Some(y), Some(m), Some(name)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let month = years
            .entry(y.to_string())
            .or_default()
            .entry(m.to_string())
            .or_default();
        if month.0.len() < 3 {
            month.0.push(name.to_string());
        }
        if originals && it.overlay.is_some() {
            month.1 += 1;
        }
    }
    let mut children: Vec<FolderNode> = years
        .into_iter()
        .map(|(y, months)| FolderNode {
            name: y,
            file_count: 0,
            sample_files: Vec::new(),
            children: months
                .into_iter()
                .map(|(m, (names, orig))| FolderNode {
                    name: m,
                    file_count: 0,
                    sample_files: names,
                    children: if orig > 0 {
                        vec![FolderNode {
                            name: mr_job::layout::ORIGINALS_DIR.into(),
                            children: Vec::new(),
                            file_count: orig * 2,
                            sample_files: Vec::new(),
                        }]
                    } else {
                        Vec::new()
                    },
                })
                .collect(),
        })
        .collect();
    // File counts per month.
    for y in &mut children {
        for m in &mut y.children {
            let prefix = format!("{}/{}/", y.name, m.name);
            m.file_count = plan
                .items
                .iter()
                .filter(|i| i.output_rel.starts_with(&prefix))
                .count() as u64;
        }
    }
    children.push(FolderNode {
        name: mr_job::layout::REPORT_DIR.into(),
        children: Vec::new(),
        file_count: 3,
        sample_files: vec![
            "report.html".into(),
            "report.csv".into(),
            "failed_items.json".into(),
        ],
    });
    FolderNode {
        name: mr_job::layout::ROOT_DIR.into(),
        children,
        file_count: 0,
        sample_files: Vec::new(),
    }
}

/// The plan as report rows (status pending), for the preview report (FR-29 AC1).
fn preview_report_data(plan: &Plan) -> mr_report::ReportData {
    use mr_report::{Row, RowKind, Status};
    let mut rows: Vec<Row> = plan
        .items
        .iter()
        .map(|p| Row {
            status: Some(Status::Pending),
            kind: Some(RowKind::Item),
            item: Some(p.key.clone()),
            json_index: p.json_index,
            media_type: Some(match p.kind {
                MediaKind::Photo => "photo".into(),
                MediaKind::Video => "video".into(),
            }),
            source_part: Some(p.main.part_label.clone()),
            source_entry: Some(p.main.entry_name.clone()),
            overlay_entry: p.overlay.as_ref().map(|o| o.entry_name.clone()),
            output_path: Some(p.output_rel.clone()),
            capture_local: Some(p.local().format("%Y-%m-%d %H:%M:%S %:z").to_string()),
            time_precision: Some(p.precision.as_str().to_string()),
            match_confidence: Some(p.confidence.clone()),
            location: Some(p.location_decision.clone()),
            message: match p.status.as_str() {
                "metadata_partial" => Some(mr_report::plain_message("MATCH_PARTIAL").to_string()),
                "location_uncertain" => {
                    Some(mr_report::plain_message("LOCATION_UNCERTAIN").to_string())
                }
                _ => None,
            },
            ..Row::default()
        })
        .collect();
    for e in &plan.extras {
        rows.push(Row {
            status: Some(match e.kind {
                mr_job::ExtraKind::MissingMedia => Status::MissingMedia,
                mr_job::ExtraKind::UnmatchedEntry => Status::MetadataPartial,
                mr_job::ExtraKind::OrphanOverlay => Status::OrphanOverlay,
                mr_job::ExtraKind::Unrecognised => Status::Unrecognised,
                mr_job::ExtraKind::Refused => Status::Refused,
                mr_job::ExtraKind::Duplicate => Status::Duplicate,
            }),
            kind: Some(if e.json_index.is_some() && e.source.is_none() {
                RowKind::JsonEntry
            } else {
                RowKind::File
            }),
            json_index: e.json_index,
            media_type: e.media_type.clone(),
            source_part: e.source.as_ref().map(|s| s.part_label.clone()),
            source_entry: e.source.as_ref().map(|s| s.entry_name.clone()),
            capture_local: e.date_utc.clone(),
            message: Some(e.detail.clone()),
            ..Row::default()
        });
    }
    mr_report::ReportData {
        job_id: "preview".into(),
        app_version: env!("CARGO_PKG_VERSION").into(),
        generated_utc: super::now_utc(),
        profile: plan.profile.clone(),
        parts: plan
            .parts
            .iter()
            .map(|p| mr_report::PartInfo {
                label: p.label.clone(),
                number: p.number,
                primary: p.primary,
                truncated: p.truncated,
            })
            .collect(),
        missing_parts: plan.missing_parts.clone(),
        notes: plan
            .warnings
            .iter()
            .filter(|w| w.code != "PART_MISSING")
            .map(|w| w.detail.clone())
            .collect(),
        rows,
    }
}
