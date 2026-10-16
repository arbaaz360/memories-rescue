# IPC contract: window ↔ engine

This is the agreement between the Memories Rescue window (`src/`, Preact) and
the Rust engine behind the Tauri commands (`src-tauri/`). The window was built
and verified against it with an in-browser mock (`src/ipc/mock/`) before the
engine existed. The engine side is `src-tauri/src/commands.rs` (the Tauri
commands) over `src-tauri/src/engine/` (the adapter onto `mr-job`, `mr-core`
and `mr-report`), with the same shapes in `src-tauri/src/ipc.rs`.
`src/ipc/types.ts` is the file the window compiles against. **When one
changes, change the others in the same commit** (checklist at the end).

Conventions:

- Command names and every JSON key are `snake_case`, so the Rust side can
  `#[derive(Serialize, Deserialize)]` without renames.
- Top-level command parameters: Tauri 2 deserialises them from camelCase
  keys by default, so `TauriBackend` renames the top-level keys before
  `invoke` (`input_ids` becomes `inputIds`, `job_id` becomes `jobId`). The Rust
  side writes plain `#[tauri::command] fn plan(input_ids: Vec<String>, options: JobOptions)`.
  Struct fields inside the arguments (`options.keep_originals`) are not renamed.
- Optional values are `T | null` (`Option<T>`), never an absent key.
- Numbers are JSON numbers; byte counts stay under 2^53.
- Times are ISO 8601 strings. Capture times carry the resolved UTC offset
  (`2019-07-04T21:03:55+01:00`); instants are UTC (`...Z`).
- Every command can fail. The `Err` value is an `IpcError` (below); Tauri
  serialises it and `invoke` rejects with it. The window never parses message
  text; it switches on `code`.
- Commands are idempotent where it is cheap (`pause` on a paused job is a
  no-op, `remove_input` of an unknown id is a no-op).

## Error codes

Exactly the SRS §8 table, as string constants:

`INPUT_NOT_EXPORT` `PART_MISSING` `PART_TRUNCATED` `ZIP_ENTRY_CRC` `JSON_MISSING`
`JSON_PARSE` `LEGACY_LINKS_ONLY` `MATCH_PARTIAL` `LOCATION_UNCERTAIN` `MEDIA_DECODE`
`FFMPEG_FAIL` `ENCODER_UNAVAILABLE` `METADATA_WRITE` `DISK_FULL` `DISK_FULL_IMMINENT`
`DEST_UNAVAILABLE` `PERMISSION_DENIED` `PATH_TOO_LONG` `FREE_LIMIT` `LICENSE_INVALID`
`LICENSE_OFFLINE` `JOB_STATE_CORRUPT` `UNKNOWN`

```jsonc
// IpcError
{ "code": "DISK_FULL", "message": "Not enough space on D:", "detail": "need 3.2 GB more" }
```

The window has its own plain-language copy for every code (NFR-USE-02) and
shows `message` / `detail` only as secondary detail. A code outside the table
is treated as `UNKNOWN` with the raw code kept in `detail`
(`mr-core`'s internal `ENTRY_REFUSED` for zip-slip entries, for example, should
surface as `PART_TRUNCATED` or `UNKNOWN` at this boundary).

`Issue` is the same idea attached to a thing rather than thrown:

```jsonc
{ "code": "PART_MISSING", "message": "Part 3 appears to be missing.", "count": 250, "part_number": 3 }
```

## Commands

| Command | Arguments | Result | Notes |
|---|---|---|---|
| `add_inputs` | `paths: string[]` | `InputFile[]` (the full current list) | FR-01. Must return within 2 s for 20 ZIPs: read central directories only. Non-exports come back with `status: "not_export"` and an `INPUT_NOT_EXPORT` issue; the window shows them as ignored. Byte-identical duplicates come back once with `status: "duplicate"` on the later one. |
| `remove_input` | `id` | `null` | |
| `analyze` | `input_ids: string[]` | `ExportSummary` | FR-02..FR-07, FR-13. Groups parts, detects gaps, classifies the profile, indexes, parses JSON, matches. The window calls it when S2 opens and again after inputs change. |
| `plan` | `input_ids`, `options: JobOptions` | `PlanPreview` | FR-29 dry run. Writes nothing. Not limited by the free tier. Returns a `plan_id` the window can pass to `export_preview_report`. |
| `export_preview_report` | `plan_id` | `string` (path written) | Writes `preview-report.html` next to where the job report would go, or to the app data dir if the destination is not writable. |
| `estimate_space` | `input_ids`, `options` | `SpaceEstimate` | FR-28, cheap (no matching): used by S3 on every option change. `sufficient` is `free >= total * 1.1`. |
| `volume_info` | `path` | `VolumeInfo` | Free and total bytes of the volume holding `path`, plus whether it is writable. Used by the S3 free-space gauge. |
| `start` | `input_ids`, `options` | `string` (job id) | Creates the job in app data, starts the worker pool, returns at once. Progress arrives by events. If the destination already holds a job (one job per output folder, SRS 3.7), that job continues instead: inputs it does not have are added as parts, its options are updated, and its id is returned. |
| `pause` | `job_id` | `null` | FR-34: in-flight items finish, then `job_state: paused`. |
| `resume` | `job_id` | `null` | Resumes a paused job, a job in `free_limit_reached` (see below), a job listed by `list_resumable_jobs` that is not in memory (loads state from app data first), or a finished job after `add_part` (processes only the newly available items) or after `activate` (processes the `skipped_free_limit` items). |
| `cancel` | `job_id` | `null` | Keeps completed output and state; ends with `job_state: cancelled`. |
| `retry_failed` | `job_id`, `options: JobOptions \| null` | `null` | FR-39: re-queues only `failed` items; `options` overrides the job's options when given. Job goes back to `running`. |
| `add_part` | `job_id`, `path` | `InputFile` | FR-40: indexes the new part and attaches it to the job; the job's state does not change. The window then shows S2 again (`analyze` over the job's inputs, which now include the new one) and calls `resume`, which processes only the items the part made available. Rejects with `INPUT_NOT_EXPORT` if the file is not a part of this export. |
| `get_report` | `job_id` | `JobReport` | Valid at any time after `start`; final once the job is `done`, `partial`, `failed` or `cancelled`. |
| `list_resumable_jobs` | – | `ResumableJob[]` | Jobs in app data that are not finished (`paused`, `running` at crash time, `free_limit_reached`). Newest first. Powers "Resume previous job" on S0. |
| `get_settings` | – | `Settings` | |
| `set_settings` | `settings: Settings` | `Settings` (as stored) | Whole object each time. |
| `diagnostics_preview` | – | `DiagnosticsPreview` | FR-41, NFR-PRV-06: the files Export diagnostics would put in the ZIP (name, size, kind) and where it would go. The window shows this before anything is written. |
| `check_update` | `force: boolean` | `UpdateInfo \| null` | §10.2: asks the update feed of the chosen channel (`latest.json` for stable, `latest-beta.json` for beta). Without `force` it answers `null` when update checks are off. Network errors are not errors here: `null`. |
| `install_update` | – | `null` | §10.2: downloads, verifies the signature, installs and restarts. Only after the user agreed in the window, and refused with `UNKNOWN` (detail `job running`) while any job runs. |
| `license_status` | – | `LicenseStatus` | Never touches the network. |
| `activate` | `key` | `LicenseStatus` | §9: one online check with Gumroad (`increment_uses_count` only here). `LICENSE_INVALID` for a wrong or disabled key, `LICENSE_OFFLINE` when the check could not be made. The key goes to the OS keychain. Q-06: a key already used on `activation_limit` computers is refused with `LICENSE_INVALID` and detail `activation_limit:<n>` (checked first without incrementing, so a refused attempt costs nothing). Deactivation is local only: Gumroad's decrement needs the seller's token. |
| `deactivate` | – | `LicenseStatus` | Forgets the key on this device. |
| `open_path` | `path` | `null` | Opens a file or folder with the OS (output folder, `report.html`, the licence files). The engine checks the path exists. |
| `export_diagnostics` | – | `string` (ZIP path) | FR-41: the files `diagnostics_preview` listed: the rotating logs (home folder as `~`, no URLs or query strings), app facts, settings without the key, job summaries and journals. No media and no JSON content. Written to the user's Desktop (or Downloads). |
| `app_info` | – | `AppInfo` | Version, OS family, paths of the licence files and third-party notices, and the outside addresses from `src/config/links.json` (privacy, terms, FAQ, ffmpeg source, website, support email). |
| `sidecar_versions` | – | `SidecarVersions` | Runs `ffmpeg -version` and, when bundled, `exiftool -ver` (`exiftool: null` otherwise: release builds ship ffmpeg only, Q-02); also the window's smoke probe at startup. Fails only when ffmpeg does not answer. |

Commands added beyond the original list, each because a screen needs it:
`export_preview_report` (S4 button), `estimate_space` and `volume_info` (S3 live
gauge), `app_info` (About), `diagnostics_preview` (Results), `check_update` and
`install_update` (the update notice and Settings). There is no telemetry in
v1.0 (Q-12): no command, no setting, nothing sent.

### Shapes

```ts
InputFile {
  id, path, name, size_bytes,
  kind: "zip" | "folder" | "other",
  role: "primary" | "part" | "unknown" | "ignored",
  part_number: number | null,          // 1 = primary
  status: "pending" | "ok" | "truncated" | "not_export" | "duplicate",
  issue: Issue | null
}

ExportSummary {
  export_id, input_ids: string[],
  profile: "2026a" | "2026b" | "legacy" | "mixed" | "unknown",
  profile_counts: { exact, ordinal, day_only, metadata_only },   // entries per §3.6 path
  parts: PartInfo[],                   // { part_number, input_id|null, name|null, size_bytes, status: ok|missing|truncated, entries }
  missing_parts: number[],
  counts: { entries, photos, videos, overlays, orphan_overlays, with_location, unrecognised },
  date_range: { first: "YYYY-MM-DD" | null, last },
  total_bytes,
  issues: Issue[],
  blocked: boolean,                    // legacy-only or nothing to process
  hevc_notice: { hevc, sampled } | null   // FR-51: HEVC videos this computer cannot play, from a sample of up to 16
}

JobOptions {
  destination,                         // user's folder; engine appends "Memories Rescue/"
  overlay_mode: "burn_in_keep_originals" | "burn_in_only" | "separate_file" | "ignore",
  keep_originals, clip_rejoin, clip_rejoin_years: { from, to } | null,
  apply_uncertain_locations, windows_compatibility,
  zip_output, zip_volume_bytes: number | null,
  home_timezone,                       // IANA
  metadata_only, workers: number | null,
  excluded_clip_groups: string[]
}

SpaceEstimate { output_bytes, originals_bytes, temp_bytes, zip_bytes, total_bytes,
                free_bytes, sufficient, shortfall_bytes, volume }

PlanPreview {
  plan_id,
  totals: { items, photos, videos, overlays_to_composite, originals_kept, missing_media, unrecognised },
  confidence: { exact, ordinal, none },
  location: { applied, none, uncertain },
  tree: FolderNode,                    // { name, children, file_count, sample_files[≤3] } rooted at "Memories Rescue"
  samples: PlanItem[],                 // ≤ 24 spread across the date range
  clip_groups: ClipGroup[],            // { group_id, item_ids, total_seconds, planned_path, start_time }
  warnings: Issue[],
  space: SpaceEstimate,
  estimated_seconds,
  free_limit: { applies, limit, items_beyond },
  hevc_notice: { hevc, sampled } | null   // null once windows_compatibility is on
}

PlanItem { item_id, media_type: "photo"|"video", source_name, planned_path,
           capture_time | null, time_precision: "second"|"day",
           latitude | null, longitude | null,
           match_confidence: "exact"|"ordinal"|"none",
           location_decision: "applied"|"none"|"uncertain",
           has_overlay, thumbnail_path | null }

JobReport {
  job_id, state, output_path, report_html_path | null, report_csv_path | null,
  started_at, finished_at | null, items_total, bytes_done,
  counts: { done, metadata_partial, location_uncertain, missing_media, failed, skipped_free_limit },
  missing_parts: number[], failures: ItemOutcome[≤200], issues: Issue[],
  can_retry, can_unlock, can_add_part, options: JobOptions,
  clips: ClipOutcome[]                 // { group_id, clip_count, total_seconds, status: pending|done|failed|skipped|declined, output_path | null, error_code | null }
}

ResumableJob { job_id, output_path, created_at, state, items_done, items_total, input_names }

Settings { destination,                // last used; first default = the user's Pictures folder
           home_timezone, overlay_mode, keep_originals, clip_rejoin, clip_rejoin_years,
           apply_uncertain_locations, windows_compatibility, workers | null,
           output_template,              // mr-job tokens {date} {time} {shortid} {id} {ext}, default "{date}_{time}_{shortid}.{ext}"
           zip_output, zip_volume_bytes | null,
           update_checks, update_channel: "stable"|"beta", language }

LicenseStatus { tier: "free"|"licensed"|"grace", key_masked | null, verified_at | null,
                last_check_at | null, grace_until | null, product_url, price_display | null, free_limit,
                activation_limit }     // Q-06, build configuration (MR_ACTIVATION_LIMIT, default 5)

AppInfo { version, os_family, licenses_path, third_party_notices_path, ffmpeg_source_url,
          privacy_policy_url, support_email, website_url, terms_url, faq_url }

UpdateInfo { version, current_version, date | null, notes | null, channel: "stable"|"beta" }

DiagnosticsPreview { destination, files: { name, bytes, kind: "logs"|"app"|"settings"|"jobs"|"journal" }[] }
```

Thumbnails: `thumbnail_path` is an absolute path to an image the engine
writes under the app cache directory (`$APPCACHE/thumbnails/<plan_id>/…`). The
window loads it through Tauri's asset protocol (`convertFileSrc`), which
`tauri.conf.json` scopes to `$APPCACHE/thumbnails/**` and nothing else. Today
the engine copies the photo itself out of the export (the web view scales it)
and sends `null` for videos; the five newest preview folders are kept.

As built today: `clip_groups` is empty until clip detection lands in mr-job
(FR-27); `estimated_seconds` uses the NFR-PERF-03 rates (20 photos/s, 5
videos/s) for the items that will run; while no compositor is installed,
`warnings` carries `ENCODER_UNAVAILABLE` with the number of overlays, which
are then saved beside their files instead of burned in.

## Events

The engine emits these with `app.emit(name, payload)`. The window subscribes
with `listen(name)` for the life of the app and filters on `job_id`. A job
runs on its own thread, so `job_state` (even `done`, for a tiny job) can
arrive before `start` has returned the id: the window keeps the latest state
per job id and applies it once the id is known.

| Event | Payload | Cadence |
|---|---|---|
| `progress` | `ProgressEvent` | At least once per second while running (FR-32), and once after every state change. Coalesce; never more than ~10/s. |
| `item` | `ItemEvent { job_id, item: ItemOutcome }` | Once per item when it reaches a final status. |
| `job_state` | `JobStateEvent { job_id, state, error, at }` | On every transition. |
| `warning` | `WarningEvent { job_id \| null, code, message, count }` | Non-fatal conditions worth a notice (overlay aspect mismatch, re-check of the licence failed, ...). |
| `update_available` | `UpdateInfo` | When the 24-hour check (or the one at launch) finds an update and update checks are on. |

```ts
ProgressEvent {
  job_id, overall_pct (0..100), items_done, items_total, bytes_done, bytes_total,
  stage: "idle"|"processing"|"finishing"|"waiting_for_drive"|"blocked"|"done"     // mr-job's stages
       | "indexing"|"matching"|"extracting"|"compositing"|"tagging"|"writing"|"verifying"|"rejoining"|"zipping"|"reporting",  // finer, optional
  throughput_items_per_s,              // EWMA, photos and videos combined
  eta_seconds | null,                  // null until the estimate is usable (after ~10 items)
  warnings, failures,                  // running counts
  recent_items: ItemOutcome[],         // newest first, ≤ 20
  sleep_prevented                      // FR-36
}

ItemOutcome { item_id, name, media_type, status: ItemStatus, error_code | null,
              message | null, output_path | null, finished_at }

ItemStatus = "done" | "metadata_partial" | "location_uncertain" | "missing_media" | "failed" | "skipped_free_limit"
```

`items_total` counts the items with media this run may process (on the free
tier, the first 100). `missing_media` entries are not processed; they appear
only in the report. After an unlock, a retry or an added part, the counts
cover the whole job again.

Engine codes outside SRS 8 are mapped before they reach the window:
`ENTRY_REFUSED` → `PART_TRUNCATED`; `INPUT_UNAVAILABLE`, `INPUT_CHANGED` →
`PART_MISSING`; `COMPOSITE_PENDING` → `ENCODER_UNAVAILABLE` (preview warning);
`DEST_UNAVAILABLE` → `job_state: paused` with that error, `DEST_AVAILABLE` →
`job_state: running`; anything else → `UNKNOWN`.

## Job state machine

```
start ──► running ──► done            (every item done / metadata_partial / location_uncertain / missing_media)
             │   └──► partial         (some failed, or some skipped_free_limit)
             │   └──► failed          (the job itself could not continue: JOB_STATE_CORRUPT, PERMISSION_DENIED on the root ...)
             ├─ pause ─► paused ─ resume ─► running
             ├─ engine pause (DISK_FULL_IMMINENT, DEST_UNAVAILABLE, PERMISSION_DENIED) ─► paused (with error) ─ resume ─► running
             ├─ cancel ─► cancelled
             └─ 100th item on the free tier ─► free_limit_reached
                       ├─ activate OK, then resume ─► running (the remaining items)
                       └─ resume without a licence ─► partial (remaining items become skipped_free_limit)
retry_failed on done | partial | cancelled ─► running
add_part (state unchanged), then resume on done | partial | cancelled ─► running (only the newly available items)
resume on a ResumableJob ─► running
```

Free limit (FR-44): on the free tier the engine processes the first 100 items
in chronological order, emits `job_state: free_limit_reached` and waits. The
window shows the Unlock dialog over S5. **Activate** then `resume` continues
with item 101. **Later** calls `resume` without a licence; the engine then
marks the rest `skipped_free_limit`, finishes as `partial`, and the report
says `can_unlock: true` so S6 can offer "Unlock to finish", which is
`activate` followed by `resume` on the same job id. Dry runs are never
limited.

Completion: the window shows a system notification on `done`, `partial` and
`failed` (tauri-plugin-notification); the engine does not.

## Settings versus options

`Settings` are the defaults (FR-48). S3 starts from `get_settings()` mapped
onto `JobOptions` plus the chosen destination; a job carries its own
`options` so a later settings change never alters a running or resumable job.

## Licence (§9)

`activate` is the only call that talks to Gumroad from the window's point of
view. The Gumroad product comes from build configuration
(`MR_GUMROAD_PRODUCT_ID`, `MR_GUMROAD_PRODUCT_URL`, `MR_PRICE_DISPLAY` at build
time; debug builds also read them, and `MR_GUMROAD_VERIFY_URL`, at run time).
A build without a product id answers `activate` with `LICENSE_OFFLINE`. Background re-checks (≤ every 30 days, `increment_uses_count=false`) are
the engine's business; they never emit anything unless the licence is revoked,
in which case `license_status` reports `tier: "grace"` with `grace_until` and
the engine emits one `warning` with code `LICENSE_INVALID`.

## What the window does without the engine

These are not commands; the window uses Tauri plugins directly and the mock
replaces them in a plain browser: file and folder pickers (`tauri-plugin-dialog`,
`open` only), drag-and-drop (`getCurrentWebview().onDragDropEvent`), opening
URLs (`tauri-plugin-opener`, allowlisted URLs only), the completion notification
(`tauri-plugin-notification`), and thumbnail URLs (`convertFileSrc`). The
capability file lists exactly these.

## Backend selection at startup

1. No `__TAURI_INTERNALS__` on `window` → plain browser → mock engine + browser host.
2. Tauri present: call `sidecar_versions`. If it rejects, the shell is broken; the window still starts on the mock and shows the error on About.
3. Then call `app_info`. If it answers → Tauri engine. If it fails with Tauri's "command not found" → the engine is not wired yet → mock engine + Tauri host (real pickers, opener, notifications) and, in dev builds, a "mock engine" badge.

## Keeping the two in sync

When you change this file or `src/ipc/types.ts`:

- [ ] the command table above matches `Commands` in `types.ts` (names, argument keys, result types)
- [ ] `ERROR_CODES` matches the §8 table and the list above
- [ ] every event payload field is in both places
- [ ] the mock (`src/ipc/mock/engine.ts`) still type-checks (`npm run build`) and its tests pass (`npm test`)
- [ ] the Rust types in `src-tauri/src/ipc.rs` are updated and `cargo clippy -D warnings` is green
- [ ] `src/config/links.json` holds every outside address (Rust reads it with `include_str!`)
- [ ] `WRITE_IPC_SAMPLES=1 npm test` rewrote `src-tauri/tests/ipc-samples/`, and `cargo test -p memories-rescue --test ipc_samples` passes (the Rust types round-trip what the mock produces)
