//! Planning: everything a dry run shows (FR-29) and everything a job needs
//! to start. Reads indexes and the JSON; writes nothing.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset, Utc};
use mr_core::discover::{Discovery, Part, discover};
use mr_core::fields::{DateField, DatePrecision, MediaType};
use mr_core::index::{EntryRef, build_index, load_json, summarize};
use mr_core::matcher::{MatchOptions, UnmatchedReason, match_export};
use mr_meta::fields::Capture;
use mr_tz::Resolver;
use serde::{Deserialize, Serialize};

use crate::compositor::MediaKind;
use crate::error::{JobError, Result};
use crate::layout;
use crate::options::{FREE_TIER_ITEMS, Options, OverlayMode};
use crate::space::{METADATA_OVERHEAD, SpaceEstimate, SpaceInputs, available_space};

/// A file inside one export part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRef {
    /// The part's path as discovered (a ZIP file or an extracted folder).
    pub part_path: String,
    /// The part's file or folder name, for messages and the report.
    pub part_label: String,
    pub part_number: Option<u32>,
    pub entry_index: usize,
    /// Name inside the export, e.g. `memories/2021-12-25_A81B-main.jpg`.
    pub entry_name: String,
    pub size: u64,
    pub crc32: Option<u32>,
}

impl SourceRef {
    fn of(d: &Discovery, e: &EntryRef) -> SourceRef {
        let part: &Part = d.part(e.part);
        SourceRef {
            part_path: part.path.to_string_lossy().into_owned(),
            part_label: part.label.clone(),
            part_number: part.number(),
            entry_index: e.index,
            entry_name: e.name.clone(),
            size: e.size,
            crc32: e.crc32,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Precision {
    Second,
    Day,
}

impl Precision {
    pub fn as_str(self) -> &'static str {
        match self {
            Precision::Second => "second",
            Precision::Day => "day",
        }
    }

    pub fn parse(s: &str) -> Precision {
        if s == "day" {
            Precision::Day
        } else {
            Precision::Second
        }
    }
}

/// One media item as the job will process it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlannedItem {
    /// `YYYY-MM-DD_ID` (section 3.8 `item.id`).
    pub key: String,
    /// The filename ID as written.
    pub file_id: String,
    pub json_index: Option<usize>,
    pub kind: MediaKind,
    /// Lower-case extension of the main file.
    pub ext: String,
    pub main: SourceRef,
    pub overlay: Option<SourceRef>,
    pub capture_utc: DateTime<Utc>,
    pub precision: Precision,
    pub offset_seconds: i32,
    /// `gps`, `home`, `system`, `gps_unknown` or `utc` (FR-19).
    pub tz_method: String,
    pub tz_zone: Option<String>,
    /// Written GPS; `None` when there is none or it was not applied.
    pub gps: Option<(f64, f64)>,
    /// `exact`, `ordinal` or `none`.
    pub confidence: String,
    pub match_reason: String,
    /// `applied`, `none` or `uncertain`.
    pub location_decision: String,
    /// `matched`, `metadata_partial` or `location_uncertain`.
    pub status: String,
    /// Position in chronological order (FR-44).
    pub chrono_rank: usize,
    /// Output path relative to the output root, `/`-separated.
    pub output_rel: String,
}

impl PlannedItem {
    pub fn offset(&self) -> FixedOffset {
        FixedOffset::east_opt(self.offset_seconds).unwrap_or(FixedOffset::east_opt(0).unwrap())
    }

    pub fn local(&self) -> DateTime<FixedOffset> {
        self.capture_utc.with_timezone(&self.offset())
    }

    pub fn capture(&self) -> Capture {
        Capture {
            utc: self.capture_utc,
            offset: self.offset(),
            gps: self.gps,
        }
    }

    pub fn is_video(&self) -> bool {
        self.kind == MediaKind::Video
    }

    /// What determines the bytes written; when it changes for a finished
    /// item, the item must be written again.
    pub fn signature(&self) -> String {
        format!(
            "{}|{}|{}|{:?}|{}|{}",
            self.capture_utc.timestamp(),
            self.precision.as_str(),
            self.offset_seconds,
            self.gps
                .map(|(a, b)| ((a * 1e6).round() as i64, (b * 1e6).round() as i64)),
            self.main.entry_name,
            self.overlay
                .as_ref()
                .map(|o| o.entry_name.as_str())
                .unwrap_or("")
        )
    }
}

/// A report row that is not a media item (FR-38 AC1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtraKind {
    /// A JSON entry with no file in the export (`missing_media`).
    MissingMedia,
    /// A JSON entry whose file could not be told apart from others.
    UnmatchedEntry,
    OrphanOverlay,
    Unrecognised,
    Refused,
    Duplicate,
}

impl ExtraKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ExtraKind::MissingMedia => "missing_media",
            ExtraKind::UnmatchedEntry => "unmatched_entry",
            ExtraKind::OrphanOverlay => "orphan_overlay",
            ExtraKind::Unrecognised => "unrecognised",
            ExtraKind::Refused => "refused",
            ExtraKind::Duplicate => "duplicate",
        }
    }

    pub fn parse(s: &str) -> Option<ExtraKind> {
        Some(match s {
            "missing_media" => ExtraKind::MissingMedia,
            "unmatched_entry" => ExtraKind::UnmatchedEntry,
            "orphan_overlay" => ExtraKind::OrphanOverlay,
            "unrecognised" => ExtraKind::Unrecognised,
            "refused" => ExtraKind::Refused,
            "duplicate" => ExtraKind::Duplicate,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtraRow {
    pub kind: ExtraKind,
    pub json_index: Option<usize>,
    pub source: Option<SourceRef>,
    /// `photo` / `video` for JSON entries.
    pub media_type: Option<String>,
    /// The JSON `Date`, normalised, for entry rows.
    pub date_utc: Option<String>,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPart {
    pub path: String,
    pub label: String,
    pub number: Option<u32>,
    pub primary: bool,
    pub truncated: bool,
    pub zip: bool,
}

/// A condition shown in the preview (SRS section 8 code and plain text).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTotals {
    pub items: usize,
    pub photos: usize,
    pub videos: usize,
    pub with_overlay: usize,
    pub json_entries: usize,
    pub exact: usize,
    pub ordinal: usize,
    pub none: usize,
    pub second_precision: usize,
    pub day_precision: usize,
    pub location_applied: usize,
    pub location_uncertain: usize,
    pub metadata_partial: usize,
    pub missing_media: usize,
    pub unmatched_entries: usize,
    pub orphan_overlays: usize,
    pub unrecognised: usize,
    pub refused: usize,
    pub duplicates: usize,
    /// Items processed now, given the licence state (FR-44).
    pub to_process: usize,
    pub skipped_free_limit: usize,
}

/// A clip rejoin group (FR-27, SRS section 4.5): consecutive clips that
/// are joined into one extra video; the clips are still written as items.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipGroup {
    /// The first clip's item key; what `ClipRejoin::skip_groups` lists.
    #[serde(default)]
    pub id: String,
    /// Item keys in order.
    pub items: Vec<String>,
    #[serde(default)]
    pub start_utc: Option<DateTime<Utc>>,
    /// Sum of the clip durations: the joined video's length.
    #[serde(default)]
    pub total_secs: f64,
    /// Planned output of the joined video, relative to the output root.
    #[serde(default)]
    pub output_rel: String,
    /// False when the user unticked the group (FR-27 AC3).
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

/// The dry-run result (FR-29) and the starting point of a job.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub output_root: PathBuf,
    pub inputs: Vec<PathBuf>,
    /// `2026a`, `2026b`, `legacy`, `mixed` or `unknown` (FR-13).
    pub profile: String,
    pub parts: Vec<PlanPart>,
    pub missing_parts: Vec<u32>,
    pub items: Vec<PlannedItem>,
    pub extras: Vec<ExtraRow>,
    pub totals: PlanTotals,
    pub space: SpaceEstimate,
    pub warnings: Vec<Warning>,
    pub clip_groups: Vec<ClipGroup>,
    pub date_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    /// Version of the bundled time zone boundaries.
    pub tz_data_version: String,
    pub home_zone: Option<String>,
}

impl Plan {
    /// Fit the warnings to the compositor the job will use: with one that
    /// can burn in, overlays are not left pending, so `COMPOSITE_PENDING`
    /// is dropped.
    pub fn adjust_for_compositor(&mut self, compositor: &dyn crate::compositor::Compositor) {
        if compositor.can_burn_in() {
            self.warnings.retain(|w| w.code != COMPOSITE_PENDING);
        }
    }

    /// The first `n` items in chronological order, with planned output paths.
    pub fn sample(&self, n: usize) -> Vec<&PlannedItem> {
        let mut v: Vec<&PlannedItem> = self.items.iter().collect();
        v.sort_by_key(|i| i.chrono_rank);
        v.truncate(n);
        v
    }
}

/// How output names are chosen.
#[derive(Debug, Clone, Default)]
pub(crate) struct Naming {
    /// Items whose names are already fixed (finished or recorded outputs).
    pub keep: HashMap<String, String>,
    /// Skip names of files that already exist in the output folder.
    pub avoid_existing: bool,
    /// Existing files that do not count as taken: the job's own earlier
    /// outputs of items that are about to be written again (lower-case).
    pub own_files: HashSet<String>,
}

/// The warning that overlays will be saved beside the files instead of
/// burned in (pass-through compositor).
pub const COMPOSITE_PENDING: &str = "COMPOSITE_PENDING";

/// [`plan`] for a job that will use `compositor`: the same analysis, with
/// warnings that match what that compositor does.
pub fn plan_for(
    inputs: &[PathBuf],
    options: &Options,
    compositor: &dyn crate::compositor::Compositor,
) -> Result<Plan> {
    let mut p = plan(inputs, options)?;
    p.adjust_for_compositor(compositor);
    Ok(p)
}

/// The dry run (FR-29): analyse the inputs and report the planned output
/// without writing anything. Not limited by the free tier. Warnings assume
/// the pass-through compositor; use [`plan_for`] when a real one is set.
pub fn plan(inputs: &[PathBuf], options: &Options) -> Result<Plan> {
    let naming = Naming {
        keep: HashMap::new(),
        avoid_existing: true,
        own_files: HashSet::new(),
    };
    Ok(plan_with(inputs, options, &naming)?.0)
}

pub(crate) fn plan_with(
    inputs: &[PathBuf],
    options: &Options,
    naming: &Naming,
) -> Result<(Plan, Discovery)> {
    if let Some(problem) = options.template_problem() {
        return Err(JobError::InvalidOptions(problem));
    }
    let d = discover(inputs);
    let export = match d.exports.len() {
        0 => {
            let why = if d.ignored.is_empty() {
                "no inputs".to_string()
            } else {
                d.ignored
                    .iter()
                    .map(|i| {
                        format!(
                            "{}: {}",
                            i.path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default(),
                            i.detail
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            return Err(JobError::NoExport(why));
        }
        1 => &d.exports[0],
        n => return Err(JobError::MultipleExports(n)),
    };
    let resolver = Resolver::new(options.home_tz.as_deref())
        .map_err(|_| JobError::TimeZone(options.home_tz.clone().unwrap_or_default()))?;
    let index = build_index(&d, export);
    let json = load_json(&d, &index);
    let summary = summarize(&d, export, &index, json.as_ref());
    let entries = json.as_ref().map(|j| j.entries.as_slice()).unwrap_or(&[]);
    let report = match_export(
        entries,
        &index,
        &MatchOptions {
            apply_uncertain_locations: options.apply_uncertain_locations,
        },
    );
    let root = layout::output_root(&options.dest);

    // Items with times and zones.
    let mut items: Vec<PlannedItem> = Vec::with_capacity(report.items.len());
    for it in &report.items {
        let gps = it.location.map(|l| (l.lat, l.lon));
        let (utc, resolved, precision) = match it.date.precision {
            DatePrecision::Second => (
                it.date.utc,
                resolver.resolve(it.date.utc, gps),
                Precision::Second,
            ),
            DatePrecision::Day => {
                let (utc, r) = resolver.resolve_day(it.key.0, gps);
                (utc, r, Precision::Day)
            }
        };
        let ext = it
            .main
            .name
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let kind = match it.media_type {
            Some(MediaType::Video) => MediaKind::Video,
            _ => MediaKind::Photo,
        };
        items.push(PlannedItem {
            key: layout::item_key(it.key.0, &it.id),
            file_id: it.id.clone(),
            json_index: it.json_index,
            kind,
            ext,
            main: SourceRef::of(&d, &it.main),
            overlay: it.overlay.as_ref().map(|o| SourceRef::of(&d, o)),
            capture_utc: utc,
            precision,
            offset_seconds: resolved.offset_seconds(),
            tz_method: resolved.method.tag().to_string(),
            tz_zone: resolved.method.zone().map(str::to_string),
            gps,
            confidence: it.confidence.as_str().to_string(),
            match_reason: it.reason.to_string(),
            location_decision: it.location_decision.as_str().to_string(),
            status: it.status.as_str().to_string(),
            chrono_rank: 0,
            output_rel: String::new(),
        });
    }

    // Chronological rank (FR-44): capture instant, then key.
    let mut order: Vec<usize> = (0..items.len()).collect();
    order.sort_by(|&a, &b| {
        (items[a].capture_utc, &items[a].key).cmp(&(items[b].capture_utc, &items[b].key))
    });
    for (rank, &i) in order.iter().enumerate() {
        items[i].chrono_rank = rank;
    }

    assign_names(&mut items, &root, options, naming);

    // Clip groups (FR-27) and the HEVC playback notice (FR-51) read the
    // video entries where they lie; nothing is extracted.
    let clip_groups = crate::clips::detect(&items, &d, options);
    let hevc_notice = crate::clips::hevc_notice(&items, &d, options);

    // Report extras.
    let mut extras = Vec::new();
    for u in &report.unmatched {
        let entry = entries.iter().find(|e| e.index == u.json_index);
        let date_utc = entry.and_then(|e| match e.date {
            Some(DateField::Instant(t)) => Some(t.to_rfc3339()),
            Some(DateField::DayOnly(d)) => Some(d.to_string()),
            _ => None,
        });
        let media_type = entry.and_then(|e| match e.media_type {
            mr_core::fields::MediaTypeHint::Photo => Some("photo".to_string()),
            mr_core::fields::MediaTypeHint::Video => Some("video".to_string()),
            mr_core::fields::MediaTypeHint::Unknown => None,
        });
        let (kind, detail) = match u.reason {
            UnmatchedReason::NoFile => (
                ExtraKind::MissingMedia,
                "no file for this entry in the export",
            ),
            UnmatchedReason::TokenNotFound => (
                ExtraKind::MissingMedia,
                "the file this entry names is not in the export",
            ),
            UnmatchedReason::GroupSizeMismatch => (
                ExtraKind::UnmatchedEntry,
                "this day has a different number of entries and files; files got the date only",
            ),
            UnmatchedReason::NoDate => (ExtraKind::UnmatchedEntry, "entry without a usable date"),
        };
        extras.push(ExtraRow {
            kind,
            json_index: Some(u.json_index),
            source: None,
            media_type,
            date_utc,
            detail: detail.to_string(),
        });
    }
    for o in &report.orphan_overlays {
        extras.push(ExtraRow {
            kind: ExtraKind::OrphanOverlay,
            json_index: None,
            source: Some(SourceRef::of(&d, o)),
            media_type: None,
            date_utc: None,
            detail: "overlay whose photo or video is not in the export; not output".into(),
        });
    }
    for e in &index.unrecognised {
        extras.push(ExtraRow {
            kind: ExtraKind::Unrecognised,
            json_index: None,
            source: Some(SourceRef::of(&d, e)),
            media_type: None,
            date_utc: None,
            detail: "file name not recognised; not output".into(),
        });
    }
    for (e, reason) in &index.refused {
        extras.push(ExtraRow {
            kind: ExtraKind::Refused,
            json_index: None,
            source: Some(SourceRef::of(&d, e)),
            media_type: None,
            date_utc: None,
            detail: format!("refused for safety: {reason}"),
        });
    }
    for (kept, dropped) in &index.duplicates {
        extras.push(ExtraRow {
            kind: ExtraKind::Duplicate,
            json_index: None,
            source: Some(SourceRef::of(&d, dropped)),
            media_type: None,
            date_utc: None,
            detail: format!("same file as {} in {}", kept.name, d.part(kept.part).label),
        });
    }
    for dup in &report.duplicate_mains {
        extras.push(ExtraRow {
            kind: ExtraKind::Duplicate,
            json_index: None,
            source: Some(SourceRef::of(&d, dup)),
            media_type: None,
            date_utc: None,
            detail: "another file has the same date and ID; not output".into(),
        });
    }

    // Totals.
    let mut t = PlanTotals {
        items: items.len(),
        json_entries: summary.json_entries,
        ..PlanTotals::default()
    };
    for it in &items {
        match it.kind {
            MediaKind::Photo => t.photos += 1,
            MediaKind::Video => t.videos += 1,
        }
        if it.overlay.is_some() {
            t.with_overlay += 1;
        }
        match it.confidence.as_str() {
            "exact" => t.exact += 1,
            "ordinal" => t.ordinal += 1,
            _ => t.none += 1,
        }
        match it.precision {
            Precision::Second => t.second_precision += 1,
            Precision::Day => t.day_precision += 1,
        }
        if it.gps.is_some() {
            t.location_applied += 1;
        }
        match it.status.as_str() {
            "metadata_partial" => t.metadata_partial += 1,
            "location_uncertain" => t.location_uncertain += 1,
            _ => {}
        }
    }
    for e in &extras {
        match e.kind {
            ExtraKind::MissingMedia => t.missing_media += 1,
            ExtraKind::UnmatchedEntry => t.unmatched_entries += 1,
            ExtraKind::OrphanOverlay => t.orphan_overlays += 1,
            ExtraKind::Unrecognised => t.unrecognised += 1,
            ExtraKind::Refused => t.refused += 1,
            ExtraKind::Duplicate => t.duplicates += 1,
        }
    }
    t.to_process = if options.unlocked {
        items.len()
    } else {
        items.len().min(FREE_TIER_ITEMS)
    };
    t.skipped_free_limit = items.len() - t.to_process;

    // Warnings.
    let mut warnings: Vec<Warning> = summary
        .issues
        .iter()
        .map(|i| Warning {
            code: i.code.as_str().to_string(),
            detail: i.detail.clone(),
        })
        .collect();
    if report.order.applicable && !report.order.is_monotone() {
        warnings.push(Warning {
            code: "MATCH_PARTIAL".into(),
            detail: format!(
                "The order of files in the ZIP does not follow the order of the data file ({} of {} day changes run backwards); exact times are only given where the export ties a file to its entry.",
                report.order.violations, report.order.checked
            ),
        });
    }
    if t.location_uncertain > 0 {
        warnings.push(Warning {
            code: "LOCATION_UNCERTAIN".into(),
            detail: format!(
                "Location not applied for {} items because the match was uncertain.",
                t.location_uncertain
            ),
        });
    }
    if t.skipped_free_limit > 0 {
        warnings.push(Warning {
            code: "FREE_LIMIT".into(),
            detail: format!(
                "The free version processes the first {FREE_TIER_ITEMS} items; {} more need the unlock.",
                t.skipped_free_limit
            ),
        });
    }
    warnings.extend(hevc_notice);
    if t.with_overlay > 0 && options.overlay_mode.burns_in() {
        warnings.push(Warning {
            code: COMPOSITE_PENDING.into(),
            detail: format!(
                "{} items have overlays. Until the overlay component is installed they are saved next to the file instead of burned in.",
                t.with_overlay
            ),
        });
    }

    let space = estimate_space(&items, options, &root, 0, |i| {
        options.unlocked || i.chrono_rank < FREE_TIER_ITEMS
    });

    let parts = d
        .parts_of(export)
        .map(|p| PlanPart {
            path: p.path.to_string_lossy().into_owned(),
            label: p.label.clone(),
            number: p.number(),
            primary: p.has_json,
            truncated: p.is_truncated(),
            zip: p.is_zip(),
        })
        .collect();

    let plan = Plan {
        output_root: root,
        inputs: inputs.to_vec(),
        profile: summary.profile.as_str().to_string(),
        parts,
        missing_parts: summary.missing_parts.clone(),
        items,
        extras,
        totals: t,
        space,
        warnings,
        clip_groups,
        date_range: summary.date_range,
        tz_data_version: resolver.boundary_data_version().to_string(),
        home_zone: resolver.home_zone().map(str::to_string),
    };
    let codes: Vec<&str> = plan.warnings.iter().map(|w| w.code.as_str()).collect();
    tracing::info!(
        profile = %plan.profile,
        parts = plan.parts.len(),
        missing_parts = plan.missing_parts.len(),
        items = plan.totals.items,
        exact = plan.totals.exact,
        ordinal = plan.totals.ordinal,
        none = plan.totals.none,
        missing_media = plan.totals.missing_media,
        to_process = plan.totals.to_process,
        required_bytes = plan.space.required_bytes,
        free_bytes = plan.space.free_bytes,
        warnings = %codes.join(","),
        "plan"
    );
    Ok((plan, d))
}

/// Output names in local capture order: kept names first, then the rest with
/// `_2`, `_3` suffixes on collisions (case-insensitive, and against files
/// already on disk when asked).
fn assign_names(items: &mut [PlannedItem], root: &Path, options: &Options, naming: &Naming) {
    let mut taken: HashSet<String> = HashSet::new();
    for it in items.iter_mut() {
        if let Some(rel) = naming.keep.get(&it.key) {
            it.output_rel = rel.clone();
            taken.insert(rel.to_lowercase());
        }
    }
    let mut order: Vec<usize> = (0..items.len())
        .filter(|&i| items[i].output_rel.is_empty())
        .collect();
    order.sort_by(|&a, &b| {
        (items[a].local(), &items[a].key).cmp(&(items[b].local(), &items[b].key))
    });
    for i in order {
        let local = items[i].local();
        let name = layout::render_name(
            &options.output_template,
            local,
            &items[i].file_id,
            &items[i].ext,
        );
        let folder = layout::folder_of(local);
        let mut n = 1;
        loop {
            let candidate_name = if n == 1 {
                name.clone()
            } else {
                layout::with_suffix(&name, n)
            };
            let rel = layout::join_rel(&folder, &candidate_name);
            let lower = rel.to_lowercase();
            let free = !taken.contains(&lower)
                && !(naming.avoid_existing
                    && !naming.own_files.contains(&lower)
                    && layout::to_fs(root, &rel).exists());
            if free {
                taken.insert(rel.to_lowercase());
                items[i].output_rel = rel;
                break;
            }
            n += 1;
        }
    }
}

/// FR-28 for the items `selected` picks.
/// What one item writes, as the FR-28 estimate counts it: (output bytes,
/// `originals/` bytes, files).
pub(crate) fn item_output_bytes(it: &PlannedItem, options: &Options) -> (u64, u64, u64) {
    let overlay = it.overlay.as_ref().map(|o| o.size).unwrap_or(0);
    let mut final_bytes = it.main.size + METADATA_OVERHEAD;
    let mut files = 1;
    if options.overlay_mode != OverlayMode::Ignore && it.overlay.is_some() {
        final_bytes += overlay;
        if !options.overlay_mode.burns_in() {
            files += 1;
        }
    }
    let mut originals = 0;
    if options.keep_originals
        && options.overlay_mode == OverlayMode::BurnInKeepOriginals
        && it.overlay.is_some()
    {
        originals = it.main.size + overlay + METADATA_OVERHEAD;
        files += 2;
    }
    (final_bytes, originals, files)
}

/// FR-28 for `items` (those `selected`). `zip_carry_bytes` is what earlier
/// runs wrote that the ZIP output must hold as well.
pub(crate) fn estimate_space(
    items: &[PlannedItem],
    options: &Options,
    root: &Path,
    zip_carry_bytes: u64,
    selected: impl Fn(&PlannedItem) -> bool,
) -> SpaceEstimate {
    let mut inputs = SpaceInputs {
        workers: options.worker_count(),
        zip_output: options.zip_output,
        zip_carry_bytes,
        ..SpaceInputs::default()
    };
    for it in items.iter().filter(|i| selected(i)) {
        let overlay = it.overlay.as_ref().map(|o| o.size).unwrap_or(0);
        let (final_bytes, originals, files) = item_output_bytes(it, options);
        inputs.final_bytes += final_bytes;
        inputs.originals_bytes += originals;
        inputs.zip_entries += files;
        inputs.largest_item = inputs.largest_item.max(it.main.size + overlay);
    }
    let free = options
        .test_hooks
        .fake_free_bytes
        .filter(|_| options.test_hooks.enabled())
        .or_else(|| available_space(root));
    SpaceEstimate::compute(&inputs, free)
}
