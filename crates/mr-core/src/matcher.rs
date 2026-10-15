//! JSON-to-file matching (FR-11) and main/overlay pairing (FR-12) as SRS
//! v0.2 section 3.6 defines them.
//!
//! 1. **Exact**: an entry whose `Download Link` carries `sid`/`mid` is tied
//!    to the main file whose ID equals either value (upper-cased).
//! 2. **Ordinal**: the remaining entries and files are grouped by
//!    `(UTC date, media type)`; a group of one is unambiguous; a larger group
//!    is matched by position, JSON array order against ZIP entry order, and
//!    only when the input has an entry order (ZIP, not folder) and the ZIP
//!    order is consistent with the JSON order across days.
//! 3. **None**: everything else gets the filename date at 12:00:00 UTC and
//!    the report status `metadata_partial`.
//! 4. **Location policy**: an ordinal item in a group of more than one gets
//!    GPS only when every entry of the group has a location within 100 m of
//!    the first; otherwise the item is `location_uncertain`, with
//!    day-precision time and no GPS unless the caller opts in.
//!
//! The ZIP-order consistency check is this module's addition: a shuffled
//! archive cannot be told from an ordered one within a day, but across days
//! the filename dates must run in the JSON's direction. If they do not, the
//! ordering assumption is false for this export and positional matching is
//! refused rather than applied wrongly.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use chrono::NaiveDate;

use crate::entry::ParsedEntry;
use crate::fields::{
    CaptureDate, DateField, DatePrecision, Location, MediaType, resolve_date, resolve_media_type,
};
use crate::index::{EntryRef, MediaFile, MergedIndex};

/// Radius within which a day group's locations count as one place.
pub const UNCERTAIN_LOCATION_RADIUS_M: f64 = 100.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Confidence {
    Exact,
    Ordinal,
    None,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Exact => "exact",
            Confidence::Ordinal => "ordinal",
            Confidence::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocationDecision {
    Applied,
    None,
    Uncertain,
}

impl LocationDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            LocationDecision::Applied => "applied",
            LocationDecision::None => "none",
            LocationDecision::Uncertain => "uncertain",
        }
    }
}

/// Report status of an item after matching (before any processing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemStatus {
    Matched,
    MetadataPartial,
    LocationUncertain,
}

impl ItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemStatus::Matched => "matched",
            ItemStatus::MetadataPartial => "metadata_partial",
            ItemStatus::LocationUncertain => "location_uncertain",
        }
    }
}

/// Why an item got its confidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchReason {
    ExactToken,
    OrdinalSingle,
    OrdinalPosition,
    GroupSizeMismatch { entries: usize, files: usize },
    NoEntryOrder,
    ZipOrderNotMonotone,
    NoJsonEntry,
    NoJson,
}

impl fmt::Display for MatchReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MatchReason::ExactToken => {
                f.write_str("sid/mid in Download Link equals the filename ID")
            }
            MatchReason::OrdinalSingle => f.write_str("only item of its day and type"),
            MatchReason::OrdinalPosition => {
                f.write_str("position within the day, ZIP order against JSON order")
            }
            MatchReason::GroupSizeMismatch { entries, files } => {
                write!(f, "day has {entries} JSON entries but {files} files")
            }
            MatchReason::NoEntryOrder => f.write_str("folder input has no entry order"),
            MatchReason::ZipOrderNotMonotone => {
                f.write_str("ZIP entry order does not follow the JSON order across days")
            }
            MatchReason::NoJsonEntry => f.write_str("no JSON entry for this day and type"),
            MatchReason::NoJson => f.write_str("export has no memories_history.json"),
        }
    }
}

/// One media item with its match decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Filename ID as written.
    pub id: String,
    pub key: (NaiveDate, String),
    pub main: EntryRef,
    pub overlay: Option<EntryRef>,
    /// ZIP entry order of the main file (part number, entry index); `None`
    /// for folder input.
    pub main_order: Option<(u32, usize)>,
    pub media_type: Option<MediaType>,
    pub json_index: Option<usize>,
    pub date: CaptureDate,
    pub location: Option<Location>,
    pub location_decision: LocationDecision,
    pub confidence: Confidence,
    pub reason: MatchReason,
    pub status: ItemStatus,
}

/// Whether ZIP entry order is consistent with JSON order across days.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OrderCheck {
    /// `false` for folder input or fewer than two ordered files.
    pub applicable: bool,
    /// Direction of the JSON array.
    pub newest_first: bool,
    /// Adjacent file pairs on different days that were compared.
    pub checked: usize,
    /// Pairs whose dates run against the JSON direction.
    pub violations: usize,
}

impl OrderCheck {
    /// `true` when positional matching within a day may be trusted.
    pub fn is_monotone(&self) -> bool {
        self.applicable && self.violations == 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupOutcome {
    Single,
    Ordinal,
    LocationUncertain,
    SizeMismatch,
    NoOrder,
    OrderUnverified,
    FilesOnly,
    EntriesOnly,
}

/// One `(UTC date, media type)` group of the ordinal step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupReport {
    pub date: NaiveDate,
    pub media_type: Option<MediaType>,
    pub entries: usize,
    pub files: usize,
    pub outcome: GroupOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnmatchedReason {
    /// No file for this entry at all (`missing_media`).
    NoFile,
    /// Its `sid`/`mid` names a file that is not in the export.
    TokenNotFound,
    /// Its day group has a different number of files.
    GroupSizeMismatch,
    NoDate,
}

/// A JSON entry that ended up without a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmatched {
    pub json_index: usize,
    pub reason: UnmatchedReason,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MatchCounts {
    pub exact: usize,
    pub ordinal: usize,
    pub none: usize,
    pub location_applied: usize,
    pub location_none: usize,
    pub location_uncertain: usize,
    pub metadata_partial: usize,
}

#[derive(Debug, Default)]
pub struct MatchReport {
    pub items: Vec<Item>,
    pub unmatched: Vec<Unmatched>,
    pub orphan_overlays: Vec<EntryRef>,
    /// Main files whose `(date, id)` was already taken; not output.
    pub duplicate_mains: Vec<EntryRef>,
    /// Entries whose `sid` and `mid` disagree, or only one of which matched.
    pub token_disagreements: usize,
    pub order: OrderCheck,
    pub groups: Vec<GroupReport>,
    pub counts: MatchCounts,
}

impl MatchReport {
    /// JSON entries with no media at all.
    pub fn missing_media(&self) -> impl Iterator<Item = &Unmatched> {
        self.unmatched.iter().filter(|u| {
            matches!(
                u.reason,
                UnmatchedReason::NoFile | UnmatchedReason::TokenNotFound
            )
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct MatchOptions {
    /// The "Apply uncertain locations" setting (FR-48), default off.
    pub apply_uncertain_locations: bool,
}

struct Prep<'a> {
    entry: &'a ParsedEntry,
    date: Option<NaiveDate>,
    media_type: Option<MediaType>,
    sid: Option<String>,
    mid: Option<String>,
}

/// Great-circle distance in metres.
pub fn distance_m(a: Location, b: Location) -> f64 {
    const R: f64 = 6_371_000.0;
    let (lat1, lat2) = (a.lat.to_radians(), b.lat.to_radians());
    let dlat = lat2 - lat1;
    let dlon = (b.lon - a.lon).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * R * h.sqrt().asin()
}

enum GroupLocation {
    AllNone,
    Consistent,
    Uncertain,
}

fn group_location(entries: &[&ParsedEntry]) -> GroupLocation {
    let locations: Vec<Option<Location>> = entries.iter().map(|e| e.location).collect();
    if locations.iter().all(Option::is_none) {
        return GroupLocation::AllNone;
    }
    let Some(first) = locations[0] else {
        return GroupLocation::Uncertain;
    };
    let consistent = locations
        .iter()
        .all(|l| l.is_some_and(|l| distance_m(first, l) <= UNCERTAIN_LOCATION_RADIUS_M));
    if consistent {
        GroupLocation::Consistent
    } else {
        GroupLocation::Uncertain
    }
}

/// Direction of the JSON array: newest first unless the dates say otherwise.
pub fn json_newest_first(entries: &[ParsedEntry]) -> bool {
    let instants: Vec<_> = entries
        .iter()
        .filter_map(|e| match e.date {
            Some(DateField::Instant(t)) => Some(t),
            _ => None,
        })
        .collect();
    match (instants.first(), instants.last()) {
        (Some(a), Some(b)) if a != b => a > b,
        _ => true,
    }
}

/// Compare ZIP entry order with the JSON direction across days.
pub fn check_order(index: &MergedIndex, newest_first: bool) -> OrderCheck {
    let ordered: Vec<&MediaFile> = index.mains.iter().filter(|f| f.order.is_some()).collect();
    let mut check = OrderCheck {
        applicable: index.ordered && ordered.len() > 1,
        newest_first,
        checked: 0,
        violations: 0,
    };
    for pair in ordered.windows(2) {
        let (a, b) = (pair[0].parsed.date, pair[1].parsed.date);
        if a == b {
            continue;
        }
        check.checked += 1;
        let wrong = if newest_first { b > a } else { b < a };
        if wrong {
            check.violations += 1;
        }
    }
    check
}

/// Match every JSON entry to a media file and pair overlays with mains.
pub fn match_export(
    entries: &[ParsedEntry],
    index: &MergedIndex,
    opts: &MatchOptions,
) -> MatchReport {
    let mut report = MatchReport::default();

    // Pairing by (date, upper id). The first file of a key wins.
    let mut main_by_key: HashMap<(NaiveDate, String), usize> = HashMap::new();
    let mut main_by_id: HashMap<String, usize> = HashMap::new();
    let mut main_slots: Vec<Option<&MediaFile>> = Vec::with_capacity(index.mains.len());
    for (i, file) in index.mains.iter().enumerate() {
        let key = file.key();
        if main_by_key.contains_key(&key) {
            report.duplicate_mains.push(file.entry.clone());
            main_slots.push(None);
            continue;
        }
        main_by_id.entry(key.1.clone()).or_insert(i);
        main_by_key.insert(key, i);
        main_slots.push(Some(file));
    }
    let mut overlay_by_key: HashMap<(NaiveDate, String), &MediaFile> = HashMap::new();
    for file in &index.overlays {
        let key = file.key();
        if overlay_by_key.contains_key(&key) || !main_by_key.contains_key(&key) {
            report.orphan_overlays.push(file.entry.clone());
        } else {
            overlay_by_key.insert(key, file);
        }
    }

    let preps: Vec<Prep<'_>> = entries
        .iter()
        .map(|entry| {
            let tokens = entry.url_tokens();
            Prep {
                entry,
                date: match entry.date {
                    Some(DateField::Instant(t)) => Some(t.date_naive()),
                    Some(DateField::DayOnly(d)) => Some(d),
                    _ => None,
                },
                media_type: resolve_media_type(entry.media_type, None),
                sid: tokens.sid,
                mid: tokens.mid,
            }
        })
        .collect();

    let mut used_main = vec![false; index.mains.len()];
    let mut used_entry = vec![false; preps.len()];
    let newest_first = json_newest_first(entries);
    report.order = check_order(index, newest_first);

    let mut items: Vec<Item> = Vec::new();

    // Step 1: exact matches on sid/mid.
    for (ei, prep) in preps.iter().enumerate() {
        if prep.sid.is_none() && prep.mid.is_none() {
            continue;
        }
        let candidates: Vec<&String> = [prep.sid.as_ref(), prep.mid.as_ref()]
            .into_iter()
            .flatten()
            .collect();
        let hits: Vec<usize> = candidates
            .iter()
            .filter_map(|t| main_by_id.get(t.as_str()).copied())
            .collect();
        let disagree = match (&prep.sid, &prep.mid) {
            (Some(s), Some(m)) => s != m || hits.len() == 1,
            _ => false,
        };
        if disagree {
            report.token_disagreements += 1;
        }
        let Some(&mi) = hits.first() else {
            used_entry[ei] = true;
            report.unmatched.push(Unmatched {
                json_index: prep.entry.index,
                reason: UnmatchedReason::TokenNotFound,
            });
            continue;
        };
        if used_main[mi] {
            used_entry[ei] = true;
            report.unmatched.push(Unmatched {
                json_index: prep.entry.index,
                reason: UnmatchedReason::TokenNotFound,
            });
            continue;
        }
        let file = main_slots[mi].expect("unused slot is a kept file");
        used_main[mi] = true;
        used_entry[ei] = true;
        let overlay = overlay_by_key.get(&file.key()).map(|o| o.entry.clone());
        items.push(matched_item(
            file,
            overlay,
            prep,
            Confidence::Exact,
            MatchReason::ExactToken,
            LocationDecision::Applied,
            false,
        ));
    }

    // Step 2: group the rest by (UTC date, media type).
    type Key = (NaiveDate, Option<MediaType>);
    let mut groups: BTreeMap<Key, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
    for (ei, prep) in preps.iter().enumerate() {
        if used_entry[ei] {
            continue;
        }
        let Some(date) = prep.date else {
            report.unmatched.push(Unmatched {
                json_index: prep.entry.index,
                reason: UnmatchedReason::NoDate,
            });
            continue;
        };
        groups
            .entry((date, prep.media_type))
            .or_default()
            .0
            .push(ei);
    }
    for (mi, slot) in main_slots.iter().enumerate() {
        let Some(file) = slot else { continue };
        if used_main[mi] {
            continue;
        }
        groups
            .entry((file.parsed.date, file.media_type()))
            .or_default()
            .1
            .push(mi);
    }

    let no_json = entries.is_empty();
    for ((date, media_type), (eids, mids)) in groups {
        let ne = eids.len();
        let nf = mids.len();
        let outcome;
        if ne == 0 {
            outcome = GroupOutcome::FilesOnly;
            for mi in &mids {
                let file = main_slots[*mi].expect("kept");
                let overlay = overlay_by_key.get(&file.key()).map(|o| o.entry.clone());
                let reason = if no_json {
                    MatchReason::NoJson
                } else {
                    MatchReason::NoJsonEntry
                };
                items.push(unmatched_item(file, overlay, reason));
            }
        } else if nf == 0 {
            outcome = GroupOutcome::EntriesOnly;
            for ei in &eids {
                report.unmatched.push(Unmatched {
                    json_index: preps[*ei].entry.index,
                    reason: UnmatchedReason::NoFile,
                });
            }
        } else if ne == 1 && nf == 1 {
            outcome = GroupOutcome::Single;
            let file = main_slots[mids[0]].expect("kept");
            let overlay = overlay_by_key.get(&file.key()).map(|o| o.entry.clone());
            items.push(matched_item(
                file,
                overlay,
                &preps[eids[0]],
                Confidence::Ordinal,
                MatchReason::OrdinalSingle,
                LocationDecision::Applied,
                false,
            ));
        } else if ne == nf {
            let blocker = if !index.ordered {
                Some((MatchReason::NoEntryOrder, GroupOutcome::NoOrder))
            } else if !report.order.is_monotone() {
                Some((
                    MatchReason::ZipOrderNotMonotone,
                    GroupOutcome::OrderUnverified,
                ))
            } else {
                None
            };
            if let Some((reason, o)) = blocker {
                outcome = o;
                for mi in &mids {
                    let file = main_slots[*mi].expect("kept");
                    let overlay = overlay_by_key.get(&file.key()).map(|e| e.entry.clone());
                    items.push(unmatched_item(file, overlay, reason.clone()));
                }
                for ei in &eids {
                    report.unmatched.push(Unmatched {
                        json_index: preps[*ei].entry.index,
                        reason: UnmatchedReason::GroupSizeMismatch,
                    });
                }
            } else {
                // JSON array order against ZIP entry order.
                let mut eids = eids.clone();
                eids.sort_by_key(|&ei| preps[ei].entry.index);
                let mut mids = mids.clone();
                mids.sort_by_key(|&mi| main_slots[mi].expect("kept").order);
                let group_entries: Vec<&ParsedEntry> =
                    eids.iter().map(|&ei| preps[ei].entry).collect();
                let (decision, uncertain) = match group_location(&group_entries) {
                    GroupLocation::AllNone => (LocationDecision::None, false),
                    GroupLocation::Consistent => (LocationDecision::Applied, false),
                    GroupLocation::Uncertain => (LocationDecision::Uncertain, true),
                };
                outcome = if uncertain {
                    GroupOutcome::LocationUncertain
                } else {
                    GroupOutcome::Ordinal
                };
                for (ei, mi) in eids.iter().zip(&mids) {
                    let file = main_slots[*mi].expect("kept");
                    let overlay = overlay_by_key.get(&file.key()).map(|o| o.entry.clone());
                    items.push(matched_item(
                        file,
                        overlay,
                        &preps[*ei],
                        Confidence::Ordinal,
                        MatchReason::OrdinalPosition,
                        decision,
                        uncertain && !opts.apply_uncertain_locations,
                    ));
                }
            }
        } else {
            outcome = GroupOutcome::SizeMismatch;
            for mi in &mids {
                let file = main_slots[*mi].expect("kept");
                let overlay = overlay_by_key.get(&file.key()).map(|o| o.entry.clone());
                items.push(unmatched_item(
                    file,
                    overlay,
                    MatchReason::GroupSizeMismatch {
                        entries: ne,
                        files: nf,
                    },
                ));
            }
            for ei in &eids {
                report.unmatched.push(Unmatched {
                    json_index: preps[*ei].entry.index,
                    reason: UnmatchedReason::GroupSizeMismatch,
                });
            }
        }
        report.groups.push(GroupReport {
            date,
            media_type,
            entries: ne,
            files: nf,
            outcome,
        });
    }

    for item in &items {
        match item.confidence {
            Confidence::Exact => report.counts.exact += 1,
            Confidence::Ordinal => report.counts.ordinal += 1,
            Confidence::None => report.counts.none += 1,
        }
        match item.location_decision {
            LocationDecision::Applied => report.counts.location_applied += 1,
            LocationDecision::None => report.counts.location_none += 1,
            LocationDecision::Uncertain => report.counts.location_uncertain += 1,
        }
        if item.status == ItemStatus::MetadataPartial {
            report.counts.metadata_partial += 1;
        }
    }
    items.sort_by(|a, b| {
        a.main
            .part
            .cmp(&b.main.part)
            .then(a.main.index.cmp(&b.main.index))
    });
    report.items = items;
    report.unmatched.sort_by_key(|u| u.json_index);
    report
}

#[allow(clippy::too_many_arguments)]
fn matched_item(
    file: &MediaFile,
    overlay: Option<EntryRef>,
    prep: &Prep<'_>,
    confidence: Confidence,
    reason: MatchReason,
    decision: LocationDecision,
    drop_time_and_location: bool,
) -> Item {
    let entry = prep.entry;
    let date = if drop_time_and_location {
        resolve_date(None, Some(file.parsed.date)).expect("filename date")
    } else {
        resolve_date(entry.date, Some(file.parsed.date)).expect("filename date")
    };
    let location = match decision {
        LocationDecision::Applied => entry.location,
        LocationDecision::None => None,
        LocationDecision::Uncertain => {
            if drop_time_and_location {
                None
            } else {
                entry.location
            }
        }
    };
    let status = match decision {
        LocationDecision::Uncertain => ItemStatus::LocationUncertain,
        _ if date.precision == DatePrecision::Day && confidence == Confidence::None => {
            ItemStatus::MetadataPartial
        }
        _ => ItemStatus::Matched,
    };
    let (date_key, id_key) = file.key();
    Item {
        id: file.parsed.id.clone(),
        key: (date_key, id_key),
        main: file.entry.clone(),
        overlay,
        main_order: file.order,
        media_type: file
            .media_type()
            .or(resolve_media_type(entry.media_type, Some(&file.parsed.ext))),
        json_index: Some(entry.index),
        date,
        location,
        location_decision: if decision == LocationDecision::Applied && entry.location.is_none() {
            LocationDecision::None
        } else {
            decision
        },
        confidence,
        reason,
        status,
    }
}

fn unmatched_item(file: &MediaFile, overlay: Option<EntryRef>, reason: MatchReason) -> Item {
    Item {
        id: file.parsed.id.clone(),
        key: file.key(),
        main: file.entry.clone(),
        overlay,
        main_order: file.order,
        media_type: file.media_type(),
        json_index: None,
        date: resolve_date(None, Some(file.parsed.date)).expect("filename date"),
        location: None,
        location_decision: LocationDecision::None,
        confidence: Confidence::None,
        reason,
        status: ItemStatus::MetadataPartial,
    }
}

// ---------------------------------------------------------------------------
// Order agreement (Q-03): measure the ordinal assumption where ground truth exists
// ---------------------------------------------------------------------------

/// One `(UTC day, media type)` group of exactly matched items, JSON order
/// against ZIP order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayAgreement {
    pub date: NaiveDate,
    pub media_type: Option<MediaType>,
    pub items: usize,
    /// The two orders are identical.
    pub agrees: bool,
    pub adjacent_pairs: usize,
    pub adjacent_agreeing: usize,
    /// Short IDs in JSON order.
    pub json_order: Vec<String>,
    /// The same IDs in ZIP order.
    pub zip_order: Vec<String>,
}

/// How often ZIP entry order agrees with JSON order within a day, measured
/// on exactly matched items (`sid`/`mid` give the ground truth). This is the
/// check SRS v0.2 asks for before M1 (Q-03, FR-11 AC3) on a real export.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrderAgreement {
    pub exact_items: usize,
    pub days_checked: usize,
    pub days_agreeing: usize,
    pub items_in_checked_days: usize,
    pub adjacent_pairs: usize,
    pub adjacent_agreeing: usize,
    pub days: Vec<DayAgreement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderVerdict {
    /// No exact items, or no day with two of them.
    Unmeasurable,
    /// Every measured day agrees and no cross-day pair runs backwards.
    Holds,
    /// At least 95% of adjacent pairs agree and no cross-day pair runs backwards.
    MostlyHolds,
    Fails,
}

impl OrderAgreement {
    pub fn verdict(&self, cross_day_violations: usize) -> OrderVerdict {
        if self.exact_items == 0 || self.days_checked == 0 {
            OrderVerdict::Unmeasurable
        } else if self.days_agreeing == self.days_checked && cross_day_violations == 0 {
            OrderVerdict::Holds
        } else if self.adjacent_agreeing * 100 >= self.adjacent_pairs * 95
            && cross_day_violations == 0
        {
            OrderVerdict::MostlyHolds
        } else {
            OrderVerdict::Fails
        }
    }
}

fn short_id(id: &str) -> String {
    id[..id.len().min(8)].to_string()
}

/// Compare JSON order with ZIP order on every day that has two or more
/// exactly matched items.
pub fn order_agreement(report: &MatchReport) -> OrderAgreement {
    let exact: Vec<&Item> = report
        .items
        .iter()
        .filter(|i| i.confidence == Confidence::Exact && i.main_order.is_some())
        .collect();
    let mut groups: BTreeMap<(NaiveDate, Option<MediaType>), Vec<&Item>> = BTreeMap::new();
    for item in &exact {
        groups
            .entry((item.key.0, item.media_type))
            .or_default()
            .push(item);
    }
    let mut agreement = OrderAgreement {
        exact_items: exact.len(),
        ..OrderAgreement::default()
    };
    for ((date, media_type), mut items) in groups {
        if items.len() < 2 {
            continue;
        }
        items.sort_by_key(|i| i.json_index);
        let json_order: Vec<String> = items.iter().map(|i| short_id(&i.id)).collect();
        let mut by_zip = items.clone();
        by_zip.sort_by_key(|i| i.main_order);
        let zip_order: Vec<String> = by_zip.iter().map(|i| short_id(&i.id)).collect();
        let agrees = json_order == zip_order;
        let adjacent_pairs = items.len() - 1;
        let adjacent_agreeing = items
            .windows(2)
            .filter(|pair| pair[0].main_order < pair[1].main_order)
            .count();
        agreement.days_checked += 1;
        agreement.items_in_checked_days += items.len();
        agreement.adjacent_pairs += adjacent_pairs;
        agreement.adjacent_agreeing += adjacent_agreeing;
        if agrees {
            agreement.days_agreeing += 1;
        }
        agreement.days.push(DayAgreement {
            date,
            media_type,
            items: items.len(),
            agrees,
            adjacent_pairs,
            adjacent_agreeing,
            json_order,
            zip_order,
        });
    }
    agreement
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::{RawEntry, parse_entry};
    use crate::filename::parse_entry_path;
    use chrono::{DateTime, Utc};
    use serde_json::{Value, json};

    fn entries(v: Value) -> Vec<ParsedEntry> {
        match v {
            Value::Array(items) => items
                .into_iter()
                .enumerate()
                .map(|(i, v)| match v {
                    Value::Object(m) => parse_entry(&RawEntry::from_json_object(i, m)),
                    _ => unreachable!(),
                })
                .collect(),
            _ => unreachable!(),
        }
    }

    fn file(part: u32, index: usize, name: &str, ordered: bool) -> MediaFile {
        let full = format!("memories/{name}");
        MediaFile {
            parsed: parse_entry_path(&full).unwrap(),
            entry: EntryRef {
                part: part as usize,
                index,
                name: full,
                size: 1,
                crc32: Some(0),
            },
            order: ordered.then_some((part, index)),
        }
    }

    fn index(mains: &[&str], overlays: &[&str], ordered: bool) -> MergedIndex {
        let mut idx = MergedIndex {
            ordered,
            ..MergedIndex::default()
        };
        for (i, name) in mains.iter().enumerate() {
            idx.mains.push(file(1, i, name, ordered));
        }
        for (i, name) in overlays.iter().enumerate() {
            idx.overlays.push(file(2, i, name, ordered));
        }
        idx
    }

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn exact_match_on_sid_and_overlay_pairing_across_parts() {
        let idx = index(
            &["2021-12-25_A81B-main.jpg", "2021-12-25_C0DE-main.mp4"],
            &["2021-12-25_a81b-overlay.png", "2020-01-01_ZZZZ-overlay.png"],
            true,
        );
        let es = entries(json!([
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image",
             "Location": "Latitude, Longitude: 51.5, -0.1",
             "Download Link": "https://x.invalid/?sid=a81b&mid=A81B"},
            {"Date": "2021-12-25 09:00:00 UTC", "Media Type": "Video",
             "Download Link": "https://x.invalid/?sid=C0DE&mid=FFFF"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert_eq!(r.items.len(), 2);
        let a = &r.items[0];
        assert_eq!(a.confidence, Confidence::Exact);
        assert_eq!(a.json_index, Some(0));
        assert_eq!(a.date.utc, utc("2021-12-25T18:42:07Z"));
        assert_eq!(a.date.precision, DatePrecision::Second);
        assert_eq!(a.location_decision, LocationDecision::Applied);
        assert!(a.location.is_some());
        assert_eq!(
            a.overlay.as_ref().unwrap().name,
            "memories/2021-12-25_a81b-overlay.png"
        );
        assert_eq!(a.status, ItemStatus::Matched);
        let b = &r.items[1];
        assert_eq!(b.confidence, Confidence::Exact);
        assert_eq!(
            b.location_decision,
            LocationDecision::None,
            "no location in the entry"
        );
        assert_eq!(
            r.token_disagreements, 1,
            "sid and mid differ on the second entry"
        );
        assert_eq!(r.orphan_overlays.len(), 1);
        assert_eq!(r.counts.exact, 2);
        assert!(r.unmatched.is_empty());
    }

    #[test]
    fn token_without_file_is_missing_media_not_ordinal() {
        let idx = index(&["2021-12-25_A81B-main.jpg"], &[], true);
        let es = entries(json!([
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image",
             "Download Link": "https://x.invalid/?sid=DEAD&mid=DEAD"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert_eq!(
            r.unmatched,
            vec![Unmatched {
                json_index: 0,
                reason: UnmatchedReason::TokenNotFound
            }]
        );
        assert_eq!(r.items[0].confidence, Confidence::None);
        assert_eq!(r.items[0].reason, MatchReason::NoJsonEntry);
        assert_eq!(r.items[0].date.precision, DatePrecision::Day);
        assert_eq!(r.items[0].status, ItemStatus::MetadataPartial);
    }

    #[test]
    fn ordinal_single_and_positional_with_consistent_locations() {
        // Newest-first JSON; ZIP order follows it.
        let idx = index(
            &[
                "2021-12-25_B-main.jpg",
                "2021-12-25_A-main.jpg",
                "2021-12-24_V-main.mp4",
                "2021-12-20_S-main.jpg",
            ],
            &[],
            true,
        );
        let es = entries(json!([
            {"Date": "2021-12-25 20:00:00 UTC", "Media Type": "Image", "Location": "Latitude, Longitude: 51.5000, -0.1000"},
            {"Date": "2021-12-25 10:00:00 UTC", "Media Type": "Image", "Location": "Latitude, Longitude: 51.5005, -0.1000"},
            {"Date": "2021-12-24 12:00:00 UTC", "Media Type": "Video", "Location": "Latitude, Longitude: 40.7, -74.0"},
            {"Date": "2021-12-20 08:00:00 UTC", "Media Type": "Image"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert!(r.order.is_monotone(), "{:?}", r.order);
        assert!(r.order.newest_first);
        let by_id: HashMap<&str, &Item> = r.items.iter().map(|i| (i.id.as_str(), i)).collect();
        let b = by_id["B"];
        assert_eq!((b.confidence, b.json_index), (Confidence::Ordinal, Some(0)));
        assert_eq!(b.reason, MatchReason::OrdinalPosition);
        assert_eq!(b.date.utc, utc("2021-12-25T20:00:00Z"));
        assert_eq!(b.location_decision, LocationDecision::Applied);
        let a = by_id["A"];
        assert_eq!(a.json_index, Some(1));
        assert_eq!(a.date.utc, utc("2021-12-25T10:00:00Z"));
        let v = by_id["V"];
        assert_eq!(
            (v.confidence, v.reason.clone()),
            (Confidence::Ordinal, MatchReason::OrdinalSingle)
        );
        assert!(v.location.is_some());
        let s = by_id["S"];
        assert_eq!(s.reason, MatchReason::OrdinalSingle);
        assert_eq!(s.location_decision, LocationDecision::None);
        assert_eq!(s.status, ItemStatus::Matched);
        assert_eq!(r.counts.ordinal, 4);
        assert!(
            r.groups
                .iter()
                .any(|g| g.outcome == GroupOutcome::Ordinal && g.entries == 2)
        );
    }

    #[test]
    fn far_apart_locations_in_a_day_group_are_uncertain() {
        let idx = index(
            &["2021-12-25_B-main.jpg", "2021-12-25_A-main.jpg"],
            &[],
            true,
        );
        let es = entries(json!([
            {"Date": "2021-12-25 20:00:00 UTC", "Media Type": "Image", "Location": "Latitude, Longitude: 51.5, -0.1"},
            {"Date": "2021-12-25 10:00:00 UTC", "Media Type": "Image", "Location": "Latitude, Longitude: 51.6, -0.1"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        for item in &r.items {
            assert_eq!(item.confidence, Confidence::Ordinal);
            assert_eq!(item.location_decision, LocationDecision::Uncertain);
            assert_eq!(item.status, ItemStatus::LocationUncertain);
            assert_eq!(
                item.date.precision,
                DatePrecision::Day,
                "time dropped to the day"
            );
            assert_eq!(item.date.utc, utc("2021-12-25T12:00:00Z"));
            assert!(item.location.is_none());
        }
        assert_eq!(r.counts.location_uncertain, 2);
        // Opting in keeps the ordinal time and location, still flagged.
        let r = match_export(
            &es,
            &idx,
            &MatchOptions {
                apply_uncertain_locations: true,
            },
        );
        let b = r.items.iter().find(|i| i.id == "B").unwrap();
        assert_eq!(b.date.utc, utc("2021-12-25T20:00:00Z"));
        assert!(b.location.is_some());
        assert_eq!(b.location_decision, LocationDecision::Uncertain);
        assert_eq!(b.status, ItemStatus::LocationUncertain);
        // Mixed located / unlocated is uncertain too.
        let es = entries(json!([
            {"Date": "2021-12-25 20:00:00 UTC", "Media Type": "Image", "Location": "Latitude, Longitude: 51.5, -0.1"},
            {"Date": "2021-12-25 10:00:00 UTC", "Media Type": "Image"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert!(
            r.items
                .iter()
                .all(|i| i.location_decision == LocationDecision::Uncertain)
        );
        // Nobody located: ordinal time kept, no GPS, not flagged.
        let es = entries(json!([
            {"Date": "2021-12-25 20:00:00 UTC", "Media Type": "Image"},
            {"Date": "2021-12-25 10:00:00 UTC", "Media Type": "Image"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert!(
            r.items
                .iter()
                .all(|i| i.location_decision == LocationDecision::None
                    && i.status == ItemStatus::Matched
                    && i.date.precision == DatePrecision::Second)
        );
    }

    #[test]
    fn folder_input_gives_none_for_multi_item_groups_only() {
        let idx = index(
            &[
                "2021-12-25_A-main.jpg",
                "2021-12-25_B-main.jpg",
                "2021-12-24_V-main.mp4",
            ],
            &[],
            false,
        );
        let es = entries(json!([
            {"Date": "2021-12-25 20:00:00 UTC", "Media Type": "Image"},
            {"Date": "2021-12-25 10:00:00 UTC", "Media Type": "Image"},
            {"Date": "2021-12-24 12:00:00 UTC", "Media Type": "Video"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert!(!r.order.applicable);
        let by_id: HashMap<&str, &Item> = r.items.iter().map(|i| (i.id.as_str(), i)).collect();
        assert_eq!(by_id["A"].confidence, Confidence::None);
        assert_eq!(by_id["A"].reason, MatchReason::NoEntryOrder);
        assert_eq!(by_id["A"].status, ItemStatus::MetadataPartial);
        assert_eq!(by_id["V"].confidence, Confidence::Ordinal);
        assert_eq!(r.unmatched.len(), 2);
        assert!(
            r.unmatched
                .iter()
                .all(|u| u.reason == UnmatchedReason::GroupSizeMismatch)
        );
    }

    #[test]
    fn shuffled_zip_order_is_detected_and_positional_matching_refused() {
        // Dates in ZIP order go 25, 20, 25, 24: not monotone.
        let idx = index(
            &[
                "2021-12-25_B-main.jpg",
                "2021-12-20_S-main.jpg",
                "2021-12-25_A-main.jpg",
                "2021-12-24_V-main.mp4",
            ],
            &[],
            true,
        );
        let es = entries(json!([
            {"Date": "2021-12-25 20:00:00 UTC", "Media Type": "Image"},
            {"Date": "2021-12-25 10:00:00 UTC", "Media Type": "Image"},
            {"Date": "2021-12-24 12:00:00 UTC", "Media Type": "Video"},
            {"Date": "2021-12-20 08:00:00 UTC", "Media Type": "Image"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert!(!r.order.is_monotone());
        assert_eq!(r.order.violations, 1);
        let by_id: HashMap<&str, &Item> = r.items.iter().map(|i| (i.id.as_str(), i)).collect();
        assert_eq!(by_id["A"].reason, MatchReason::ZipOrderNotMonotone);
        assert_eq!(by_id["A"].confidence, Confidence::None);
        assert_eq!(by_id["B"].date.precision, DatePrecision::Day);
        // Singles are still fine.
        assert_eq!(by_id["V"].confidence, Confidence::Ordinal);
        assert_eq!(by_id["S"].confidence, Confidence::Ordinal);
    }

    #[test]
    fn group_size_mismatch_and_files_without_json() {
        let idx = index(
            &[
                "2021-12-25_A-main.jpg",
                "2021-12-25_B-main.jpg",
                "2021-12-25_C-main.jpg",
            ],
            &[],
            true,
        );
        let es = entries(json!([
            {"Date": "2021-12-25 20:00:00 UTC", "Media Type": "Image"},
            {"Date": "2021-12-25 10:00:00 UTC", "Media Type": "Image"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert!(r.items.iter().all(|i| i.confidence == Confidence::None
            && i.reason
                == MatchReason::GroupSizeMismatch {
                    entries: 2,
                    files: 3
                }));
        assert_eq!(r.counts.metadata_partial, 3);
        assert_eq!(r.unmatched.len(), 2);
        let r = match_export(&[], &idx, &MatchOptions::default());
        assert!(r.items.iter().all(|i| i.reason == MatchReason::NoJson));
    }

    #[test]
    fn legacy_entries_have_no_files() {
        let idx = index(&[], &[], true);
        let es = entries(json!([
            {"Date": "2018-05-12 09:15:30 UTC", "Media Type": "Video", "Download Link": "https://x.invalid/a?sig=1"}
        ]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert!(r.items.is_empty());
        assert_eq!(r.missing_media().count(), 1);
    }

    #[test]
    fn duplicate_main_keys_keep_the_first() {
        let mut idx = index(&["2021-12-25_A-main.jpg"], &[], true);
        idx.mains.push(file(2, 0, "2021-12-25_a-main.jpg", true));
        let es = entries(json!([{"Date": "2021-12-25 20:00:00 UTC", "Media Type": "Image"}]));
        let r = match_export(&es, &idx, &MatchOptions::default());
        assert_eq!(r.items.len(), 1);
        assert_eq!(r.duplicate_mains.len(), 1);
        assert_eq!(r.items[0].confidence, Confidence::Ordinal);
    }

    #[test]
    fn distance() {
        let london = Location {
            lat: 51.50735,
            lon: -0.12776,
        };
        let near = Location {
            lat: 51.50800,
            lon: -0.12776,
        };
        assert!((distance_m(london, near) - 72.3).abs() < 1.0);
        let paris = Location {
            lat: 48.8566,
            lon: 2.3522,
        };
        assert!((distance_m(london, paris) / 1000.0 - 343.5).abs() < 1.0);
    }
}
