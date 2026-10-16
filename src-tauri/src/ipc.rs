//! The window ↔ engine contract in Rust: the same shapes as `src/ipc/types.ts`
//! and `docs/ipc-contract.md`. snake_case JSON, `Option<T>` for every
//! `T | null` (always serialised, never skipped). `tests/ipc_samples.rs` checks
//! these types against JSON produced by the TypeScript mock.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Errors (SRS §8)
// ---------------------------------------------------------------------------

/// Exactly the codes of the SRS §8 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InputNotExport,
    PartMissing,
    PartTruncated,
    ZipEntryCrc,
    JsonMissing,
    JsonParse,
    LegacyLinksOnly,
    MatchPartial,
    LocationUncertain,
    MediaDecode,
    FfmpegFail,
    EncoderUnavailable,
    MetadataWrite,
    DiskFull,
    DiskFullImminent,
    DestUnavailable,
    PermissionDenied,
    PathTooLong,
    FreeLimit,
    LicenseInvalid,
    LicenseOffline,
    JobStateCorrupt,
    Unknown,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 23] = [
        ErrorCode::InputNotExport,
        ErrorCode::PartMissing,
        ErrorCode::PartTruncated,
        ErrorCode::ZipEntryCrc,
        ErrorCode::JsonMissing,
        ErrorCode::JsonParse,
        ErrorCode::LegacyLinksOnly,
        ErrorCode::MatchPartial,
        ErrorCode::LocationUncertain,
        ErrorCode::MediaDecode,
        ErrorCode::FfmpegFail,
        ErrorCode::EncoderUnavailable,
        ErrorCode::MetadataWrite,
        ErrorCode::DiskFull,
        ErrorCode::DiskFullImminent,
        ErrorCode::DestUnavailable,
        ErrorCode::PermissionDenied,
        ErrorCode::PathTooLong,
        ErrorCode::FreeLimit,
        ErrorCode::LicenseInvalid,
        ErrorCode::LicenseOffline,
        ErrorCode::JobStateCorrupt,
        ErrorCode::Unknown,
    ];

    /// Maps a code from the engine crates onto the contract. Codes the SRS
    /// table does not have become their nearest §8 code; anything unknown is
    /// `UNKNOWN` (the caller keeps the raw code in `detail`).
    pub fn from_engine(code: &str) -> ErrorCode {
        match code {
            "INPUT_NOT_EXPORT" => ErrorCode::InputNotExport,
            "PART_MISSING" | "INPUT_CHANGED" | "INPUT_UNAVAILABLE" => ErrorCode::PartMissing,
            "PART_TRUNCATED" | "ENTRY_REFUSED" => ErrorCode::PartTruncated,
            "ZIP_ENTRY_CRC" => ErrorCode::ZipEntryCrc,
            "JSON_MISSING" => ErrorCode::JsonMissing,
            "JSON_PARSE" => ErrorCode::JsonParse,
            "LEGACY_LINKS_ONLY" => ErrorCode::LegacyLinksOnly,
            "MATCH_PARTIAL" => ErrorCode::MatchPartial,
            "LOCATION_UNCERTAIN" => ErrorCode::LocationUncertain,
            "MEDIA_DECODE" => ErrorCode::MediaDecode,
            "FFMPEG_FAIL" => ErrorCode::FfmpegFail,
            "ENCODER_UNAVAILABLE" => ErrorCode::EncoderUnavailable,
            "METADATA_WRITE" => ErrorCode::MetadataWrite,
            "DISK_FULL" => ErrorCode::DiskFull,
            "DISK_FULL_IMMINENT" => ErrorCode::DiskFullImminent,
            "DEST_UNAVAILABLE" => ErrorCode::DestUnavailable,
            "PERMISSION_DENIED" => ErrorCode::PermissionDenied,
            "PATH_TOO_LONG" => ErrorCode::PathTooLong,
            "FREE_LIMIT" => ErrorCode::FreeLimit,
            "LICENSE_INVALID" => ErrorCode::LicenseInvalid,
            "LICENSE_OFFLINE" => ErrorCode::LicenseOffline,
            "JOB_STATE_CORRUPT" => ErrorCode::JobStateCorrupt,
            _ => ErrorCode::Unknown,
        }
    }
}

/// Every command rejects with this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpcError {
    pub code: ErrorCode,
    pub message: String,
    pub detail: Option<String>,
}

impl IpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> IpcError {
        IpcError {
            code,
            message: message.into(),
            detail: None,
        }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> IpcError {
        self.detail = Some(detail.into());
        self
    }

    pub fn unknown(message: impl Into<String>) -> IpcError {
        IpcError::new(ErrorCode::Unknown, message)
    }
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for IpcError {}

pub type IpcResult<T> = Result<T, IpcError>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub code: ErrorCode,
    pub message: String,
    pub count: Option<u64>,
    pub part_number: Option<u32>,
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Zip,
    Folder,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputRole {
    Primary,
    Part,
    Unknown,
    Ignored,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputStatus {
    Pending,
    Ok,
    Truncated,
    NotExport,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputFile {
    pub id: String,
    pub path: String,
    pub name: String,
    pub size_bytes: u64,
    pub kind: InputKind,
    pub role: InputRole,
    pub part_number: Option<u32>,
    pub status: InputStatus,
    pub issue: Option<Issue>,
}

// ---------------------------------------------------------------------------
// Export summary
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PartStatus {
    Ok,
    Missing,
    Truncated,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PartInfo {
    pub part_number: u32,
    pub input_id: Option<String>,
    pub name: Option<String>,
    pub size_bytes: u64,
    pub status: PartStatus,
    pub entries: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileCounts {
    pub exact: u64,
    pub ordinal: u64,
    pub day_only: u64,
    pub metadata_only: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportCounts {
    pub entries: u64,
    pub photos: u64,
    pub videos: u64,
    pub overlays: u64,
    pub orphan_overlays: u64,
    pub with_location: u64,
    pub unrecognised: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DateRange {
    pub first: Option<String>,
    pub last: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExportSummary {
    pub export_id: String,
    pub input_ids: Vec<String>,
    /// `2026a`, `2026b`, `legacy`, `mixed`, `unknown`.
    pub profile: String,
    pub profile_counts: ProfileCounts,
    pub parts: Vec<PartInfo>,
    pub missing_parts: Vec<u32>,
    pub counts: ExportCounts,
    pub date_range: DateRange,
    pub total_bytes: u64,
    pub issues: Vec<Issue>,
    pub blocked: bool,
    /// FR-51: HEVC videos this computer cannot play, from a sample.
    pub hevc_notice: Option<HevcNotice>,
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayMode {
    BurnInKeepOriginals,
    BurnInOnly,
    SeparateFile,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct YearRange {
    pub from: i32,
    pub to: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobOptions {
    pub destination: String,
    pub overlay_mode: OverlayMode,
    pub keep_originals: bool,
    pub clip_rejoin: bool,
    pub clip_rejoin_years: Option<YearRange>,
    pub apply_uncertain_locations: bool,
    pub windows_compatibility: bool,
    pub zip_output: bool,
    pub zip_volume_bytes: Option<u64>,
    pub home_timezone: String,
    pub metadata_only: bool,
    pub workers: Option<u32>,
    pub excluded_clip_groups: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VolumeInfo {
    pub path: String,
    pub volume: String,
    pub free_bytes: u64,
    pub total_bytes: u64,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceEstimate {
    pub output_bytes: u64,
    pub originals_bytes: u64,
    pub temp_bytes: u64,
    pub zip_bytes: u64,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub sufficient: bool,
    pub shortfall_bytes: u64,
    pub volume: String,
}

// ---------------------------------------------------------------------------
// Plan / dry run
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchConfidence {
    Exact,
    Ordinal,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocationDecision {
    Applied,
    None,
    Uncertain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaType {
    Photo,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimePrecision {
    Second,
    Day,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanItem {
    pub item_id: String,
    pub media_type: MediaType,
    pub source_name: String,
    pub planned_path: String,
    pub capture_time: Option<String>,
    pub time_precision: TimePrecision,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub match_confidence: MatchConfidence,
    pub location_decision: LocationDecision,
    pub has_overlay: bool,
    pub thumbnail_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipGroup {
    pub group_id: String,
    pub item_ids: Vec<String>,
    pub total_seconds: f64,
    pub planned_path: String,
    pub start_time: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FolderNode {
    pub name: String,
    pub children: Vec<FolderNode>,
    pub file_count: u64,
    pub sample_files: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanTotals {
    pub items: u64,
    pub photos: u64,
    pub videos: u64,
    pub overlays_to_composite: u64,
    pub originals_kept: u64,
    pub missing_media: u64,
    pub unrecognised: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceBreakdown {
    pub exact: u64,
    pub ordinal: u64,
    pub none: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LocationBreakdown {
    pub applied: u64,
    pub none: u64,
    pub uncertain: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FreeLimitInfo {
    pub applies: bool,
    pub limit: u64,
    pub items_beyond: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanPreview {
    pub plan_id: String,
    pub totals: PlanTotals,
    pub confidence: ConfidenceBreakdown,
    pub location: LocationBreakdown,
    pub tree: FolderNode,
    pub samples: Vec<PlanItem>,
    pub clip_groups: Vec<ClipGroup>,
    pub warnings: Vec<Issue>,
    pub space: SpaceEstimate,
    pub estimated_seconds: f64,
    pub free_limit: FreeLimitInfo,
    pub hevc_notice: Option<HevcNotice>,
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Paused,
    Done,
    Partial,
    Failed,
    Cancelled,
    FreeLimitReached,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Idle,
    Processing,
    Finishing,
    WaitingForDrive,
    Blocked,
    Done,
    Indexing,
    Matching,
    Extracting,
    Compositing,
    Tagging,
    Writing,
    Verifying,
    Rejoining,
    Zipping,
    Reporting,
}

impl Stage {
    /// mr-job reports its stage as text.
    pub fn from_engine(stage: &str) -> Stage {
        match stage {
            "processing" => Stage::Processing,
            "finishing items in progress" => Stage::Finishing,
            "waiting for the output drive" => Stage::WaitingForDrive,
            "blocked" => Stage::Blocked,
            "done" => Stage::Done,
            _ => Stage::Processing,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Done,
    MetadataPartial,
    LocationUncertain,
    MissingMedia,
    Failed,
    SkippedFreeLimit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemOutcome {
    pub item_id: String,
    pub name: String,
    pub media_type: MediaType,
    pub status: ItemStatus,
    pub error_code: Option<ErrorCode>,
    pub message: Option<String>,
    pub output_path: Option<String>,
    pub finished_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressEvent {
    pub job_id: String,
    pub overall_pct: f64,
    pub items_done: u64,
    pub items_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub stage: Stage,
    pub throughput_items_per_s: f64,
    pub eta_seconds: Option<f64>,
    pub warnings: u64,
    pub failures: u64,
    pub recent_items: Vec<ItemOutcome>,
    pub sleep_prevented: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemEvent {
    pub job_id: String,
    pub item: ItemOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobStateEvent {
    pub job_id: String,
    pub state: JobState,
    pub error: Option<IpcError>,
    pub at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarningEvent {
    pub job_id: Option<String>,
    pub code: ErrorCode,
    pub message: String,
    pub count: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusCounts {
    pub done: u64,
    pub metadata_partial: u64,
    pub location_uncertain: u64,
    pub missing_media: u64,
    pub failed: u64,
    pub skipped_free_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobReport {
    pub job_id: String,
    pub state: JobState,
    pub output_path: String,
    pub report_html_path: Option<String>,
    pub report_csv_path: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub items_total: u64,
    pub bytes_done: u64,
    pub counts: StatusCounts,
    pub missing_parts: Vec<u32>,
    pub failures: Vec<ItemOutcome>,
    pub issues: Vec<Issue>,
    pub can_retry: bool,
    pub can_unlock: bool,
    pub can_add_part: bool,
    pub options: JobOptions,
    /// FR-27: joined clip groups.
    pub clips: Vec<ClipOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResumableJob {
    pub job_id: String,
    pub output_path: String,
    pub created_at: String,
    pub state: JobState,
    pub items_done: u64,
    pub items_total: u64,
    pub input_names: Vec<String>,
}

// ---------------------------------------------------------------------------
// Settings, licence, app info
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateChannel {
    Stable,
    Beta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub destination: String,
    pub home_timezone: String,
    pub overlay_mode: OverlayMode,
    pub keep_originals: bool,
    pub clip_rejoin: bool,
    pub clip_rejoin_years: Option<YearRange>,
    pub apply_uncertain_locations: bool,
    pub windows_compatibility: bool,
    pub workers: Option<u32>,
    pub output_template: String,
    pub zip_output: bool,
    pub zip_volume_bytes: Option<u64>,
    pub update_checks: bool,
    pub update_channel: UpdateChannel,
    pub language: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LicenseTier {
    Free,
    Licensed,
    Grace,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LicenseStatus {
    pub tier: LicenseTier,
    pub key_masked: Option<String>,
    pub verified_at: Option<String>,
    pub last_check_at: Option<String>,
    pub grace_until: Option<String>,
    pub product_url: String,
    pub price_display: Option<String>,
    pub free_limit: u64,
    /// Q-06: computers one key may be activated on (build configuration).
    pub activation_limit: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub version: String,
    pub os_family: String,
    pub licenses_path: String,
    pub third_party_notices_path: String,
    pub ffmpeg_source_url: String,
    pub privacy_policy_url: String,
    pub support_email: String,
    pub website_url: String,
    pub terms_url: String,
    pub faq_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HevcNotice {
    /// HEVC videos found in the sample.
    pub hevc: u64,
    /// Videos sampled (up to 16).
    pub sampled: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipStatus {
    Pending,
    Done,
    Failed,
    Skipped,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipOutcome {
    /// mr-job's group id (the first clip's item key); what `excluded_clip_groups` lists.
    pub group_id: String,
    pub clip_count: u64,
    pub total_seconds: f64,
    pub status: ClipStatus,
    pub output_path: Option<String>,
    pub error_code: Option<ErrorCode>,
}

/// An update the feed offers (§10.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub date: Option<String>,
    pub notes: Option<String>,
    pub channel: UpdateChannel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsFile {
    pub name: String,
    pub bytes: u64,
    /// `logs`, `app`, `settings`, `jobs`, `journal`.
    pub kind: String,
}

/// What Export diagnostics will save, shown before saving (FR-41, NFR-PRV-06).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsPreview {
    pub destination: String,
    pub files: Vec<DiagnosticsFile>,
}

/// What `sidecar_versions` returns to the window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SidecarVersions {
    /// First line of `ffmpeg -version`, e.g. `ffmpeg version n9.0.2-28-g669f5ab083-20261015 ...`.
    pub ffmpeg: String,
    /// Output of `exiftool -ver`, e.g. `13.59`; `None` when it is not
    /// bundled (Q-02: release builds write metadata in Rust and ship ffmpeg only).
    pub exiftool: Option<String>,
}

/// Event names, as the window listens for them.
pub mod events {
    pub const PROGRESS: &str = "progress";
    pub const ITEM: &str = "item";
    pub const JOB_STATE: &str = "job_state";
    pub const WARNING: &str = "warning";
    pub const UPDATE_AVAILABLE: &str = "update_available";
}
