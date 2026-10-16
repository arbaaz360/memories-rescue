/**
 * IPC contract between the Memories Rescue window and the Rust engine.
 *
 * This file is the single source of truth for every command the screens call
 * and every event the engine emits. `docs/ipc-contract.md` describes the same
 * shapes in prose; keep the two in sync (the doc has a checklist). All JSON is
 * snake_case so the Rust side can derive `serde::{Serialize, Deserialize}`
 * without renames. Optional values are `T | null`, never missing keys, so the
 * Rust types can use plain `Option<T>`.
 *
 * Numbers are JSON numbers (f64 on the wire). Byte counts stay below 2^53.
 * Times are ISO 8601 strings; capture times carry a UTC offset.
 */

// ---------------------------------------------------------------------------
// Errors (SRS §8, exactly that table)
// ---------------------------------------------------------------------------

export const ERROR_CODES = [
  "INPUT_NOT_EXPORT",
  "PART_MISSING",
  "PART_TRUNCATED",
  "ZIP_ENTRY_CRC",
  "JSON_MISSING",
  "JSON_PARSE",
  "LEGACY_LINKS_ONLY",
  "MATCH_PARTIAL",
  "LOCATION_UNCERTAIN",
  "MEDIA_DECODE",
  "FFMPEG_FAIL",
  "ENCODER_UNAVAILABLE",
  "METADATA_WRITE",
  "DISK_FULL",
  "DISK_FULL_IMMINENT",
  "DEST_UNAVAILABLE",
  "PERMISSION_DENIED",
  "PATH_TOO_LONG",
  "FREE_LIMIT",
  "LICENSE_INVALID",
  "LICENSE_OFFLINE",
  "JOB_STATE_CORRUPT",
  "UNKNOWN",
] as const;

export type ErrorCode = (typeof ERROR_CODES)[number];

/** Every command rejects with this shape (Tauri serialises the `Err` value). */
export interface IpcError {
  code: ErrorCode;
  /** Plain-language summary in the engine's words; the UI prefers its own copy for the code. */
  message: string;
  /** Secondary detail (raw error, path with the home directory redacted), shown on request. */
  detail: string | null;
}

/** An issue attached to an export, a plan or a job: a code plus how many items it touches. */
export interface Issue {
  code: ErrorCode;
  message: string;
  /** Items affected, when countable. */
  count: number | null;
  /** Part the issue belongs to (1 = primary), when it is about a part. */
  part_number: number | null;
}

// ---------------------------------------------------------------------------
// Inputs (S1)
// ---------------------------------------------------------------------------

export type InputKind = "zip" | "folder" | "other";
export type InputRole = "primary" | "part" | "unknown" | "ignored";
export type InputStatus = "pending" | "ok" | "truncated" | "not_export" | "duplicate";

export interface InputFile {
  /** Stable id assigned by the engine; the UI never derives it from the path. */
  id: string;
  path: string;
  /** File or folder name for display. */
  name: string;
  size_bytes: number;
  kind: InputKind;
  /** Decided from content: the primary carries `json/memories_history.json`. */
  role: InputRole;
  /** 1 = primary, 2.. = extra parts, null until known or when ignored. */
  part_number: number | null;
  status: InputStatus;
  /** Set when `status` is `not_export`, `truncated` or `duplicate`. */
  issue: Issue | null;
}

// ---------------------------------------------------------------------------
// Export summary (S2, FR-07, FR-13)
// ---------------------------------------------------------------------------

export type FormatProfile = "2026a" | "2026b" | "legacy" | "mixed" | "unknown";

export type PartStatus = "ok" | "missing" | "truncated";

export interface PartInfo {
  part_number: number;
  /** Input that provides this part, null when missing. */
  input_id: string | null;
  name: string | null;
  size_bytes: number;
  status: PartStatus;
  /** Media entries indexed from this part (0 when missing). */
  entries: number;
}

/** How many JSON entries sit on each matching path of SRS §3.6. */
export interface ProfileCounts {
  exact: number;
  ordinal: number;
  day_only: number;
  /** Legacy entries: links only, no media to process. */
  metadata_only: number;
}

export interface ExportCounts {
  entries: number;
  photos: number;
  videos: number;
  overlays: number;
  orphan_overlays: number;
  with_location: number;
  unrecognised: number;
}

export interface DateRange {
  /** ISO 8601 calendar date (YYYY-MM-DD) or null when nothing was dated. */
  first: string | null;
  last: string | null;
}

export interface ExportSummary {
  export_id: string;
  input_ids: string[];
  profile: FormatProfile;
  profile_counts: ProfileCounts;
  parts: PartInfo[];
  /** Part numbers detected as missing, e.g. [3]. */
  missing_parts: number[];
  counts: ExportCounts;
  date_range: DateRange;
  total_bytes: number;
  /** PART_MISSING, PART_TRUNCATED, JSON_*, LEGACY_LINKS_ONLY, MATCH_PARTIAL, LOCATION_UNCERTAIN ... */
  issues: Issue[];
  /** True when nothing can be processed (legacy-only, or no media at all). */
  blocked: boolean;
}

// ---------------------------------------------------------------------------
// Options (S3, FR-26, FR-48)
// ---------------------------------------------------------------------------

export type OverlayMode = "burn_in_keep_originals" | "burn_in_only" | "separate_file" | "ignore";

export interface YearRange {
  from: number;
  to: number;
}

export interface JobOptions {
  /** Absolute path chosen by the user; the engine appends `Memories Rescue/`. */
  destination: string;
  overlay_mode: OverlayMode;
  keep_originals: boolean;
  clip_rejoin: boolean;
  /** null = all years. */
  clip_rejoin_years: YearRange | null;
  apply_uncertain_locations: boolean;
  windows_compatibility: boolean;
  zip_output: boolean;
  /** null = one archive; otherwise split at this size (4 GB option for FAT32). */
  zip_volume_bytes: number | null;
  /** IANA zone, e.g. "Europe/London". */
  home_timezone: string;
  /** FR-21: metadata and timestamps only, no compositing, no rejoin. */
  metadata_only: boolean;
  /** null = engine default (min(cores - 1, 4)). */
  workers: number | null;
  /** Clip groups the user unticked in the preview (FR-27 AC3). */
  excluded_clip_groups: string[];
}

export interface VolumeInfo {
  path: string;
  /** Drive letter or mount point for messages ("D:" / "/Volumes/USB"). */
  volume: string;
  free_bytes: number;
  total_bytes: number;
  writable: boolean;
}

/** FR-28: final output + originals + peak temp + optional ZIP, against free space. */
export interface SpaceEstimate {
  output_bytes: number;
  originals_bytes: number;
  temp_bytes: number;
  zip_bytes: number;
  total_bytes: number;
  free_bytes: number;
  /** free >= total * 1.1 */
  sufficient: boolean;
  shortfall_bytes: number;
  volume: string;
}

// ---------------------------------------------------------------------------
// Plan / dry run (S4, FR-29)
// ---------------------------------------------------------------------------

export type MatchConfidence = "exact" | "ordinal" | "none";
export type LocationDecision = "applied" | "none" | "uncertain";
export type MediaType = "photo" | "video";
export type TimePrecision = "second" | "day";

export interface PlanItem {
  item_id: string;
  media_type: MediaType;
  source_name: string;
  /** Relative to `<destination>/Memories Rescue/`, e.g. "2019/07/2019-07-04_210355_3F2A9C1E.mp4". */
  planned_path: string;
  /** ISO 8601 with offset in the resolved local time, null when undated. */
  capture_time: string | null;
  time_precision: TimePrecision;
  latitude: number | null;
  longitude: number | null;
  match_confidence: MatchConfidence;
  location_decision: LocationDecision;
  has_overlay: boolean;
  /** Absolute path of a small JPEG/PNG under the app cache dir, served through the asset protocol; null when none. */
  thumbnail_path: string | null;
}

export interface ClipGroup {
  group_id: string;
  item_ids: string[];
  /** Combined duration in seconds. */
  total_seconds: number;
  planned_path: string;
  /** Capture time of the first clip, ISO 8601 with offset. */
  start_time: string;
}

export interface FolderNode {
  name: string;
  children: FolderNode[];
  /** Files directly inside this folder (not counting children). */
  file_count: number;
  /** Up to three file names to show under the folder. */
  sample_files: string[];
}

export interface PlanTotals {
  items: number;
  photos: number;
  videos: number;
  overlays_to_composite: number;
  originals_kept: number;
  /** JSON entries with no media in the given parts (missing part or legacy). */
  missing_media: number;
  unrecognised: number;
}

export interface ConfidenceBreakdown {
  exact: number;
  ordinal: number;
  none: number;
}

export interface LocationBreakdown {
  applied: number;
  none: number;
  uncertain: number;
}

export interface FreeLimitInfo {
  /** False when licensed. */
  applies: boolean;
  limit: number;
  /** Items that would be skipped without a licence. */
  items_beyond: number;
}

export interface PlanPreview {
  plan_id: string;
  totals: PlanTotals;
  confidence: ConfidenceBreakdown;
  location: LocationBreakdown;
  tree: FolderNode;
  samples: PlanItem[];
  clip_groups: ClipGroup[];
  warnings: Issue[];
  space: SpaceEstimate;
  estimated_seconds: number;
  free_limit: FreeLimitInfo;
}

// ---------------------------------------------------------------------------
// Jobs (S5, S6)
// ---------------------------------------------------------------------------

export type JobState =
  | "running"
  | "paused"
  | "done"
  | "partial"
  | "failed"
  | "cancelled"
  | "free_limit_reached";

export type Stage =
  | "idle"
  | "indexing"
  | "matching"
  | "extracting"
  | "compositing"
  | "tagging"
  | "writing"
  | "verifying"
  | "rejoining"
  | "zipping"
  | "reporting";

export type ItemStatus =
  | "done"
  | "metadata_partial"
  | "location_uncertain"
  | "missing_media"
  | "failed"
  | "skipped_free_limit";

export interface ItemOutcome {
  item_id: string;
  name: string;
  media_type: MediaType;
  status: ItemStatus;
  error_code: ErrorCode | null;
  message: string | null;
  output_path: string | null;
  /** ISO 8601 instant. */
  finished_at: string;
}

export interface ProgressEvent {
  job_id: string;
  /** 0..100 */
  overall_pct: number;
  items_done: number;
  items_total: number;
  bytes_done: number;
  bytes_total: number;
  stage: Stage;
  /** EWMA over photos and videos together (FR-32). */
  throughput_items_per_s: number;
  /** null until enough has been processed to estimate. */
  eta_seconds: number | null;
  warnings: number;
  failures: number;
  /** Newest first, at most 20. */
  recent_items: ItemOutcome[];
  /** FR-36: true while a sleep-prevention assertion is held. */
  sleep_prevented: boolean;
}

export interface ItemEvent {
  job_id: string;
  item: ItemOutcome;
}

export interface JobStateEvent {
  job_id: string;
  state: JobState;
  /** Set for `failed`, and for `paused` when the engine paused itself (DISK_FULL_IMMINENT, DEST_UNAVAILABLE ...). */
  error: IpcError | null;
  /** ISO 8601 instant. */
  at: string;
}

export interface WarningEvent {
  job_id: string | null;
  code: ErrorCode;
  message: string;
  count: number | null;
}

export interface EngineEvents {
  progress: ProgressEvent;
  item: ItemEvent;
  job_state: JobStateEvent;
  warning: WarningEvent;
}

export type StatusCounts = Record<ItemStatus, number>;

export interface JobReport {
  job_id: string;
  state: JobState;
  output_path: string;
  report_html_path: string | null;
  report_csv_path: string | null;
  started_at: string;
  finished_at: string | null;
  items_total: number;
  bytes_done: number;
  counts: StatusCounts;
  missing_parts: number[];
  /** At most 200, newest first; the HTML report has the full list. */
  failures: ItemOutcome[];
  issues: Issue[];
  can_retry: boolean;
  can_unlock: boolean;
  can_add_part: boolean;
  options: JobOptions;
}

export interface ResumableJob {
  job_id: string;
  output_path: string;
  created_at: string;
  state: JobState;
  items_done: number;
  items_total: number;
  input_names: string[];
}

// ---------------------------------------------------------------------------
// Settings (FR-48), licence (§9), app info
// ---------------------------------------------------------------------------

export type UpdateChannel = "stable" | "beta";

export interface Settings {
  home_timezone: string;
  overlay_mode: OverlayMode;
  keep_originals: boolean;
  clip_rejoin: boolean;
  clip_rejoin_years: YearRange | null;
  apply_uncertain_locations: boolean;
  windows_compatibility: boolean;
  workers: number | null;
  /** §3.7 template, default "{YYYY-MM-DD}_{HHMMSS}_{shortid}". */
  output_template: string;
  zip_output: boolean;
  zip_volume_bytes: number | null;
  update_checks: boolean;
  update_channel: UpdateChannel;
  /** Default false (NFR-PRV-04). */
  telemetry: boolean;
  /** BCP 47 tag or "system". */
  language: string;
}

/** NFR-PRV-04: exactly what an opted-in telemetry upload would contain. */
export interface TelemetryPayload {
  app_version: string;
  os_family: string;
  /** e.g. "1k-10k" */
  items_processed_bucket: string;
  error_code_counts: Record<string, number>;
  crash_signatures: string[];
}

export type LicenseTier = "free" | "licensed" | "grace";

export interface LicenseStatus {
  tier: LicenseTier;
  /** e.g. "****-****-****-A1B2", null on the free tier. */
  key_masked: string | null;
  verified_at: string | null;
  last_check_at: string | null;
  /** Set on `grace`: when the 14-day grace period ends (§9.3 rule 4). */
  grace_until: string | null;
  product_url: string;
  /** Display string from build configuration, null until the price is final (Q-05). */
  price_display: string | null;
  free_limit: number;
}

export interface AppInfo {
  version: string;
  os_family: string;
  licenses_path: string;
  third_party_notices_path: string;
  ffmpeg_source_url: string;
  privacy_policy_url: string;
  support_email: string;
  website_url: string;
}

export interface SidecarVersions {
  ffmpeg: string;
  exiftool: string;
}

// ---------------------------------------------------------------------------
// Command table (names and signatures, for the Tauri binding and the mock)
// ---------------------------------------------------------------------------

/**
 * Every command, its argument object and its result. Argument keys are the
 * Tauri command parameter names (snake_case; Tauri 2 accepts snake_case keys
 * for `#[tauri::command]` parameters).
 */
export interface Commands {
  add_inputs: { args: { paths: string[] }; result: InputFile[] };
  remove_input: { args: { id: string }; result: null };
  analyze: { args: { input_ids: string[] }; result: ExportSummary };
  plan: { args: { input_ids: string[]; options: JobOptions }; result: PlanPreview };
  export_preview_report: { args: { plan_id: string }; result: string };
  estimate_space: { args: { input_ids: string[]; options: JobOptions }; result: SpaceEstimate };
  volume_info: { args: { path: string }; result: VolumeInfo };
  start: { args: { input_ids: string[]; options: JobOptions }; result: string };
  pause: { args: { job_id: string }; result: null };
  resume: { args: { job_id: string }; result: null };
  cancel: { args: { job_id: string }; result: null };
  retry_failed: { args: { job_id: string; options: JobOptions | null }; result: null };
  add_part: { args: { job_id: string; path: string }; result: InputFile };
  get_report: { args: { job_id: string }; result: JobReport };
  list_resumable_jobs: { args: Record<string, never>; result: ResumableJob[] };
  get_settings: { args: Record<string, never>; result: Settings };
  set_settings: { args: { settings: Settings }; result: Settings };
  telemetry_preview: { args: Record<string, never>; result: TelemetryPayload };
  license_status: { args: Record<string, never>; result: LicenseStatus };
  activate: { args: { key: string }; result: LicenseStatus };
  deactivate: { args: Record<string, never>; result: LicenseStatus };
  open_path: { args: { path: string }; result: null };
  export_diagnostics: { args: Record<string, never>; result: string };
  app_info: { args: Record<string, never>; result: AppInfo };
  sidecar_versions: { args: Record<string, never>; result: SidecarVersions };
}

export type CommandName = keyof Commands;

export const COMMAND_NAMES: readonly CommandName[] = [
  "add_inputs",
  "remove_input",
  "analyze",
  "plan",
  "export_preview_report",
  "estimate_space",
  "volume_info",
  "start",
  "pause",
  "resume",
  "cancel",
  "retry_failed",
  "add_part",
  "get_report",
  "list_resumable_jobs",
  "get_settings",
  "set_settings",
  "telemetry_preview",
  "license_status",
  "activate",
  "deactivate",
  "open_path",
  "export_diagnostics",
  "app_info",
  "sidecar_versions",
];

export const EVENT_NAMES: readonly (keyof EngineEvents)[] = ["progress", "item", "job_state", "warning"];

export function isIpcError(value: unknown): value is IpcError {
  if (typeof value !== "object" || value === null) return false;
  const v = value as Record<string, unknown>;
  return typeof v.code === "string" && typeof v.message === "string";
}

/** Normalise anything a command rejected with into an `IpcError`. Unknown codes become `UNKNOWN` with the raw code kept in `detail`. */
export function toIpcError(value: unknown): IpcError {
  if (isIpcError(value)) {
    const known = (ERROR_CODES as readonly string[]).includes(value.code);
    if (known) return { code: value.code, message: value.message, detail: value.detail ?? null };
    const raw: string = value.code;
    return { code: "UNKNOWN", message: value.message, detail: value.detail ? `${raw}: ${value.detail}` : raw };
  }
  const message = value instanceof Error ? value.message : typeof value === "string" ? value : "";
  return { code: "UNKNOWN", message, detail: null };
}
