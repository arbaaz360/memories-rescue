# mr-job: the engine API

`crates/mr-job` is the Memories Rescue engine as a library. The Tauri layer
and `mr-cli` drive it the same way; `crates/mr-cli/src/jobs.rs` is a complete,
working example of every call below. SRS references are to v0.2.

```text
inputs (ZIPs / folders) ──plan()──► Plan            dry run, writes nothing (FR-29)
                        ──Job::create()──► Job      state in <state>/jobs/<id>, marker in output
Job ──run(sink, control)──► Outcome                 blocks; progress ≥ 1/s to the sink
    ──retry_failed / add_part / reindex / unlock / set_options
    ──report()──► _report/report.html, report.csv, failed_items.json
```

## Threading

`Job::run` blocks the calling thread until the job completes, pauses, is
cancelled or is blocked. Call it from a background thread (in Tauri: a
`std::thread::spawn` or `tauri::async_runtime::spawn_blocking`), never the
UI thread. The engine runs its own worker pool inside `run`; the
`ProgressSink` is called from the thread that called `run`. `Control` is
cheap to clone and is how the UI thread steers the run. A `Job` is not
`Sync`; keep it on the thread that runs it, and open a second handle with
`Job::open` if the UI needs to read state while a run is in progress (SQLite
WAL allows concurrent readers).

## Options

```rust
let mut o = mr_job::Options::new(dest);      // defaults below
o.home_tz = Some("Europe/London".into());    // None: system zone, then UTC
o.unlocked = license_is_valid;               // false: first 100 items (FR-44)
```

| Field | Default | SRS |
|---|---|---|
| `dest: PathBuf` | required | Output goes to `<dest>/Memories Rescue/` (3.7) |
| `overlay_mode: OverlayMode` | `BurnInKeepOriginals` | FR-26: `BurnInKeepOriginals`, `BurnInOnly`, `SaveSeparate`, `Ignore` |
| `keep_originals: bool` | `true` | FR-25 |
| `home_tz: Option<String>` | `None` (system) | FR-19, FR-48 |
| `workers: Option<usize>` | `min(cores - 1, 4)` | 4.2; videos are limited to `min(2, cores / 2)` in flight |
| `output_template: String` | `{date}_{time}_{shortid}.{ext}` | 3.7; also `{id}` |
| `unlocked: bool` | `false` | FR-44 |
| `apply_uncertain_locations: bool` | `false` | 3.6 step 4, FR-48 |
| `windows_compat: bool` | `false` | FR-51: videos the compositor finds HEVC are saved as H.264 (`compat_transcode`); burn-ins are H.264 anyway |
| `clip_rejoin: ClipRejoin { enabled, year_range, skip_groups }` | off, `2017-2021`, none | FR-27: groups found in the dry run are joined after the items; `skip_groups` holds the group ids the user unticked (AC3) |
| `zip_output: bool` | `false` | FR-31: once every item is finished, the tree is also packed into ZIP64 archives in `dest` (see "ZIP output") |
| `zip_volume_bytes: Option<u64>` | `None` (one archive) | FR-31 volume size; `mr_job::ZIP_VOLUME_FAT32` (4 GiB - 1 byte) is the FAT32 preset |
| `ignore_space_check: bool` | `false` | FR-28 AC1 "continue anyway" |
| `test_hooks: TestHooks` | empty | debug builds only (fault injection) |

`Options` is `Serialize + Deserialize`, so the settings screen can persist it
as JSON.

## Dry run

```rust
let p: Plan = mr_job::plan(&inputs, &options)?;
```

Writes nothing. When the job will use a real compositor, call
`mr_job::plan_for(&inputs, &options, &compositor)` instead (or
`plan.adjust_for_compositor(&compositor)`): `COMPOSITE_PENDING` (overlays saved
beside the files) is only warned about with the pass-through. `Plan` holds
everything the Check & Summary and Preview screens (S2, S4) show:

| Field | What |
|---|---|
| `profile` | `2026a`, `2026b`, `legacy`, `mixed`, `unknown` (FR-13) |
| `parts: Vec<PlanPart>` | label, number, primary, truncated, zip |
| `missing_parts: Vec<u32>` | FR-03 |
| `totals: PlanTotals` | items, photos, videos, with_overlay, json_entries, exact / ordinal / none, second / day precision, location applied / uncertain, metadata_partial, missing_media, unmatched_entries, orphan_overlays, unrecognised, refused, duplicates, `to_process` and `skipped_free_limit` given the licence |
| `items: Vec<PlannedItem>` | every item with capture time (UTC, offset, local), precision, zone method, GPS written, confidence and reason, location decision, status, planned output path |
| `sample(n)` | the first `n` items in chronological order |
| `extras: Vec<ExtraRow>` | entries that are not items: missing media, unmatched entries, orphan overlays, unrecognised, refused, duplicates |
| `space: SpaceEstimate` | FR-28 formula, free space, `sufficient`, `shortfall_bytes`, `options` (change destination, disable originals, disable ZIP, with bytes saved) |
| `warnings: Vec<Warning>` | `{code, detail}`: `PART_MISSING`, `PART_TRUNCATED`, `ENTRY_REFUSED`, `JSON_MISSING`, `JSON_PARSE`, `LEGACY_LINKS_ONLY`, `MATCH_PARTIAL`, `LOCATION_UNCERTAIN`, `FREE_LIMIT`, `COMPOSITE_PENDING` (overlays exist and the mode burns in; `plan_for` with a `MediaCompositor` leaves it out), `HEVC_PLAYBACK` (FR-51: HEVC found in a sample of up to 16 videos, this Windows cannot play HEVC, and `windows_compat` is off; offer the option) |
| `clip_groups: Vec<ClipGroup>` | FR-27, with `clip_rejoin.enabled`: `id` (first clip's item key), `items` (keys in order), `start_utc`, `total_secs`, `output_rel` (`<first clip stem>-joined.<ext>`), `enabled` (false when in `skip_groups`). Videos are probed where they lie, streamed until `moov`; only exact-time videos in the year range with a neighbour within a minute are read |
| `date_range`, `tz_data_version`, `home_zone`, `output_root` | |

To "export the preview report" (FR-29 AC1), serialise the `Plan` (it is
`Serialize`), as `mr-cli plan --export file.json` does.

Errors: `JobError::NoExport(why)` (`INPUT_NOT_EXPORT`), `MultipleExports(n)`,
`TimeZone(name)`.

## Jobs

```rust
let mut job = Job::create(&state_dir, &inputs, options)?;   // plans, stores, writes the marker
let mut job = Job::open(&state_dir, &id)?;                   // integrity check; may rebuild
let id: Option<String> = mr_job::find_job_for_output(&state_dir, &dest);
let all: Vec<JobSummary> = mr_job::list_jobs(&state_dir);    // "Resume previous job" (S0)
```

- **State directory.** Pass the per-user app data folder (SRS 3.7):
  `%APPDATA%\NovaByteWorks\Memories Rescue` on Windows,
  `~/Library/Application Support/Memories Rescue` on macOS. In Tauri:
  `app.path().app_data_dir()`. A job lives in `<state>/jobs/<id>/` as
  `job.sqlite` (WAL) and `job.json` (inputs and options, for rebuilds).
- **Keyed by output folder.** `create` fails with
  `JobError::OutputInUse { job_id }` when `<dest>/Memories Rescue` already has a
  job; offer to resume it. The output folder holds only
  `.memories-rescue/job-id`. If that marker is there but the state is gone,
  `create` adopts the finished files instead of duplicating them.
- **Damaged state.** `open` runs `PRAGMA integrity_check`. If the file is not a
  database or fails the check, it is moved aside (`job.sqlite.corrupt-<time>`),
  the job is re-planned from `job.json`, and every planned output that exists
  and reads back its capture metadata is marked done (`JOB_STATE_CORRUPT`).
  `job.rebuilt()` returns the reason; `run` also reports it as an issue.

Reading state: `job.items()`, `job.counts()` (per stage), `job.summary()`,
`job.journal()` (`start`, `done`, `failed`, `requeue:<code>`, `adopted` per
attempt), `job.check_inputs()`, `job.output_root()`, `job.options()`.

## Running

```rust
let control = Control::new();             // clone it into the UI
let outcome = job.run(&sink, &control)?;
```

`Control`: `pause()` finishes the items in flight and returns
`Outcome::Paused`; `resume()` clears the pause so the next `run` proceeds;
`cancel()` finishes the items in flight and returns `Outcome::Cancelled`
(output and state are kept, temp is removed).

| `Outcome` | Meaning | UI |
|---|---|---|
| `Completed(RunStats)` | every item that may be processed is done or failed | Results (S6); call `report()` |
| `Paused(RunStats)` | `Control::pause` | show Resume |
| `Cancelled(RunStats)` | `Control::cancel` | back to Results |
| `Blocked { code, detail, stats }` | `DISK_FULL_IMMINENT` (re-check every 500 items found < 2 GB), `DISK_FULL`, `PERMISSION_DENIED`, `INPUT_UNAVAILABLE` | show the message; `run` again after the fix |
| `InsufficientSpace(SpaceEstimate)` | FR-28 AC1: nothing was started | shortfall dialog with `options`; retry with another `dest` (new job), `keep_originals = false`, or `ignore_space_check` |
| `InputsChanged(Vec<InputChange>)` | FR-33 AC2: a part changed or is missing | warn; `job.reindex()` (or `add_part`) then `run` |

`RunStats`: `done`, `failed`, `skipped_free_limit`, `pending`,
`processed_this_run`, `seconds`.

A missing destination drive does not end the run: the engine stops
dispatching, reports `DEST_UNAVAILABLE`, waits, reports `DEST_AVAILABLE` and
carries on (NFR-REL-06). Sleep is prevented while `run` is active (FR-36;
`Progress::sleep_prevented`).

### ProgressSink

```rust
pub trait ProgressSink: Send + Sync {
    fn progress(&self, p: &Progress);     // at least once per second
    fn item(&self, o: &ItemOutcome);      // every finished attempt
    fn issue(&self, code: &str, detail: &str);
}
```

`Progress`: `percent`, `items_done`, `items_failed`, `items_total`,
`bytes_done`, `bytes_total`, `stage` (`processing`, `finishing items in
progress`, `waiting for the output drive`, `blocked`, `done`), `throughput`
(items/s this run), `eta_seconds` (EWMA kept separately for photos and
videos, `None` until five items finished), `recent` (last ten
`ItemOutcome`s), `warnings`, `failures`, `elapsed_seconds`,
`sleep_prevented`. All types are `Serialize`, so a Tauri sink can forward
them as events unchanged:

```rust
struct TauriSink(tauri::AppHandle);
impl ProgressSink for TauriSink {
    fn progress(&self, p: &Progress) { let _ = self.0.emit("job-progress", p); }
    fn item(&self, o: &ItemOutcome)  { let _ = self.0.emit("job-item", o); }
    fn issue(&self, c: &str, d: &str) { let _ = self.0.emit("job-issue", (c, d)); }
}
```

`ItemOutcome`: `id` (item key), `stage` (`done` / `failed`), `media_type`,
`output_path` (relative), `error_code`, `error_detail`, `seconds`. A joined
clip group reports one `ItemOutcome` with `id` = `clip:<group id>` after the
items are done.

Issue codes sent to `issue`: `JOB_STATE_CORRUPT`, `INPUT_CHANGED`,
`DISK_FULL`, `DISK_FULL_IMMINENT`, `DEST_UNAVAILABLE`, `DEST_AVAILABLE`,
`PERMISSION_DENIED`, `INPUT_UNAVAILABLE`. Item error codes: `ZIP_ENTRY_CRC`,
`MEDIA_DECODE`, `METADATA_WRITE` (output kept without metadata), any code a
compositor returns (`FFMPEG_FAIL` and the like; original and overlay are
kept), `TEST_INJECTED` (tests), `UNKNOWN`. `mr_report::plain_message(code)`
gives the user-facing sentence from SRS section 8.

## After a run

```rust
job.retry_failed(Some(new_options))?;  // FR-39: only failed items go back to pending
job.add_part(&path)?;                  // FR-40: returns Replanned
job.reindex()?;                        // after InputsChanged
job.unlock()?;                         // FR-44: skipped_free_limit -> pending
job.set_options(options)?;             // home zone / template / uncertain locations re-plan
let files = job.report()?;             // FR-38
let data = job.report_data()?;         // the same rows, for an in-app Results table
let groups = job.clip_groups()?;       // FR-27: ClipRow { group, stage, output_path, error_code, ... }
```

`ClipRow::stage`: `pending`, `done`, `failed` (`CLIP_INCOMPLETE` when a clip
was not written, or the compositor's code), `skipped` (unticked) or
`declined` (no media support). A group whose clips' plans change is joined
again and its earlier file replaced.

`Replanned`: `items`, `new_items`, `requeued` (finished items whose time,
location or overlay changed because of the new part; they are written again
and their old files removed), `removed`, `missing_parts`. Finished items whose
plan did not change are never touched.

`report()` writes `<output>/_report/report.html`, `report.csv` and
`failed_items.json` and returns their paths. One row per item and per other
entry (FR-38 AC1); statuses `done`, `metadata_partial`, `location_uncertain`,
`missing_media`, `failed`, `skipped_free_limit`, `pending`,
`orphan_overlay`, `unrecognised`, `refused`, `duplicate`. Rows name parts by
file name and outputs relative to the output folder; free text is redacted of
URLs and absolute paths. Joined clip groups are rows of kind `joined_clip`
(item = group id, the joined file as output). An item's message also carries
the compositor's notes from its latest attempt (`OVERLAY_ASPECT_MISMATCH`,
`H264_COMPAT`, `ODD_SIZE_SCALED`, `AUDIO_REENCODED`), kept as `warning:`
journal events.

## ZIP output (FR-31)

With `zip_output`, `run` packs the job's recorded outputs (items, overlays,
`originals/`, joined clips; not `_report/` or `.memories-rescue/`) after the
last item and clip join, before it returns `Completed`. The folder tree is
kept. The archives go into `dest`, next to the `Memories Rescue` folder:

```text
<dest>/Memories Rescue.zip                      one archive (no volume size)
<dest>/Memories Rescue-001.zip, -002.zip, ...   with zip_volume_bytes
```

Entries are `Memories Rescue/YYYY/MM/...`, stored (media is compressed
already), ZIP64 where needed, with the DOS time in local time plus an
extended-timestamp field (0x5455) holding the exact UTC time, so extraction
restores each file's capture time. Each archive is written as
`<name>.partial`, read back with mr-core's ZIP reader (central directory,
names, sizes, CRCs against the source bytes, every entry read to the end),
then renamed. A single file larger than the volume size gets an archive of
its own (`oversize`).

```rust
for v in job.zip_volumes()? {   // ZipVolume { seq, name, files, data_bytes, size, state, error_code, error_detail, oversize }
    // state: "done", or "failed" with ZIP_WRITE / ZIP_VERIFY; a later run retries failed ones
}
```

During the pack `Progress::stage` is `packing ZIP file 2 of 5`, with
`bytes_done` / `bytes_total` and the ETA over the archive bytes; item counts
stay at their final values. Pause and cancel take effect within one 1 MiB
chunk; the archive being written is removed and redone on the next run.
Interruptions resume: finished archives are kept (recorded with size and
time), and a crash between verification and the final rename is recognised.
When the recorded outputs change (retry, added part, unlock), the next run
removes the archives it made, while they are unchanged, and packs again. A
file that is not the job's own at an archive's name makes it `_2.zip`.

Space: the FR-28 estimate counts the ZIP as final + originals + a header
allowance per file, plus, on a later run, everything earlier runs wrote
(the pack holds the whole job). The pack checks free space again before it
starts (`DISK_FULL`, `Blocked`) and stops at the 2 GB floor while writing
(`DISK_FULL_IMMINENT`). The report's notes list the archives, failures and
oversize archives.

## Logging (FR-41)

The engine emits [`tracing`](https://docs.rs/tracing) events and installs
nothing: the app sets the subscriber, writes the rotating file (50 MB cap)
and builds the diagnostics ZIP. Without a subscriber the events cost nothing.

```rust
// src-tauri, once at start-up (tracing-subscriber and a rolling writer are the app's choice)
tracing_subscriber::fmt().with_writer(rotating_file).with_max_level(Level::INFO).init();
```

Every path and free-text value goes through `mr_core::redact::path` or
`mr_core::redact::text` before it reaches an event: the home directory
(`USERPROFILE` / `HOME`, case-insensitive on Windows, either separator, with
or without `\\?\`) becomes `~`, URLs become `<link removed>`, and a stray
query string becomes `?<query removed>`. GPS coordinates, captions, JSON
content and media bytes are never logged; item keys (`YYYY-MM-DD_ID`) and
archive entry names are. Use the same helpers for anything the app logs
itself.

| Target | Level | Message | Fields |
|---|---|---|---|
| `mr_core::discover` | info | `inputs discovered` | `inputs`, `parts`, `exports`, `ignored` |
| | info | `input ignored (INPUT_NOT_EXPORT)` | `path`, `detail` |
| | warn | `part issue` | `part`, `code` (`PART_TRUNCATED`, `ENTRY_REFUSED`, ...), `detail` |
| `mr_core::zipindex` | warn, info | `ZIP central directory unreadable; ...`, `ZIP recovered` | `path`, `detail` / `entries`, `truncated` |
| `mr_core::index` | info, warn | `memories_history.json read`, `... stopped parsing`, `... entries skipped` | `entries`, `exact`, `ordinal`, `links`; `code` = `JSON_PARSE` |
| `mr_job::plan` | info | `plan` | `profile`, `parts`, `missing_parts`, `items`, `exact`, `ordinal`, `none`, `missing_media`, `to_process`, `required_bytes`, `free_bytes`, `warnings` (codes) |
| `mr_job::job` | info | `job created`, `run started`, `run finished`, `re-planned`, `part added`, `failed items queued for retry`, `unlocked: ...`, `report written`, `joining clips` | `job`, `dest`, counts, `outcome`, `seconds` |
| | info | `zip output started`, `zip volume written and verified`, `zip output finished`, `output changed since the last ZIP; packing again` | `volumes`, `to_write`, `files`, `missing`, `bytes`, `volume_bytes`, `volume`, `oversize`, `failed` |
| | warn | `job state rebuilt from the output folder` | `code` = `JOB_STATE_CORRUPT`, `detail`, `adopted` |
| | error | `run stopped by an error` | `error` |
| `mr_job::control` | info | `progress` (on every stage change and at most every 30 s) | `stage`, `done`, `failed`, `total`, `percent`, `items_per_s`, `eta_s` |
| | warn | `item failed` | `item`, `code`, `detail`, `kept` (output written anyway) |
| | warn, info | `issue`, `issue cleared` | `code` (`DISK_FULL_IMMINENT`, `DEST_UNAVAILABLE`, ...), `detail` |
| | debug | `item done` | `item`, `seconds` |
| `mr_job::job` | info | `item warning` | `item`, `code` (`overlay_aspect_mismatch`, `h264_compat`, ...), `detail` |
| `mr_job::process` | debug | `output name taken by another file; using a suffix` | `name`, `suffix` |

`crates/mr-job/tests/logging.rs` runs a job under a capturing subscriber and
checks that these events arrive and that none carries the home directory, a
URL or a query string (`MR_LOG_DUMP=1` prints them).

## The compositor seam

```rust
pub trait Compositor: Send + Sync {
    fn composite(&self, req: &CompositeRequest<'_>) -> Result<CompositeOutcome, CompositeError>;
    fn compat_transcode(&self, req: &TranscodeRequest<'_>) -> Result<CompositeOutcome, CompositeError> { NotSupported }
    fn rejoin(&self, req: &RejoinRequest<'_>) -> Result<CompositeOutcome, CompositeError> { NotSupported }
    fn name(&self) -> &str { "compositor" }
}
// The real one (mr-composite), bound to the ffmpeg sidecar:
job.set_compositor(Arc::new(mr_job::MediaCompositor::new(sidecar_ffmpeg)));
```

`MediaCompositor` (`crates/mr-job/src/media.rs`) burns overlays into photos
(no ffmpeg needed) and videos, makes HEVC videos H.264 when asked, and joins
clip groups; `media.encoder_plan()` tells the UI before a run whether video
work is possible. When no H.264 encoder works (`ENCODER_UNAVAILABLE`, e.g. a
Windows N edition without the Media Feature Pack) it declines video work
(`NotSupported`) instead of failing items, so those items are written with
their overlay beside them and `composite_pending`, ready for
`requeue_composite_pending` once the component is installed.
`mr_job::media::hevc_playback_available()` answers FR-51 for the UI.
`mr-cli` uses it when the `MR_FFMPEG` environment variable names an ffmpeg.

`CompositeRequest`: `kind` (`Photo` / `Video`), `main`, `overlay`, `out`
(all inside the item's temp folder on the destination volume), `ext`,
`windows_compat`, `item_key`. Return `Composited` (or
`CompositedWithWarnings(vec![CompositeWarning { code, detail }])`, which the
report shows) after writing `out`;
the engine then writes metadata onto the composite, verifies it, and with
`BurnInKeepOriginals` + `keep_originals` saves `originals/<stem>-main.<ext>`
(metadata fixed) and `originals/<stem>-overlay.png`. Return `NotSupported`
to decline: the engine copies the main file, saves the overlay beside it as
`<stem>-overlay.png` and marks the item `composite_pending`. Return an
error with an SRS code (`FFMPEG_FAIL`, `MEDIA_DECODE`, `ENCODER_UNAVAILABLE`):
the item is reported `failed` with that code, and the original plus the
separate overlay are still written. The default is `PassThrough`
(`NotSupported`). The compositor runs on worker threads, several at once.

`compat_transcode` gets `TranscodeRequest { main, out, ext, item_key }` for
every video that was not composited when `windows_compat` is on; it returns
`NotSupported` for anything that needs no conversion. `rejoin` gets
`RejoinRequest { group_id, segments: [RejoinSegment { main, overlay }], out }`
on the coordinator thread after all items are done; the engine writes the
first clip's metadata onto the result and places it beside the clips.

When a real compositor becomes available for a job that already ran with the
pass-through, `job.requeue_composite_pending()` queues the items marked
`composite_pending`; the next `run` composites them, writes `originals/` as
configured and removes the overlays that were saved beside the files.

## What happens to one item

1. Temp folder `<output>/.memories-rescue/tmp/<key>/` on the destination volume.
2. Main (and overlay) streamed out of the part; CRC checked (`ZIP_ENTRY_CRC`).
3. Compositor, if the mode burns in; otherwise, for videos with
   `windows_compat`, the H.264 conversion when the compositor says it is needed.
4. Metadata: JPEG `DateTimeOriginal`, `CreateDate`, `ModifyDate`,
   `OffsetTimeOriginal`, `OffsetTimeDigitized`, `OffsetTime`, GPS with refs
   and UTC stamps; PNG the same in an `eXIf` chunk; MP4/MOV `mvhd`/`tkhd`/
   `mdhd` times, `mdta` Keys `com.apple.quicktime.creationdate` and
   `...location.ISO6709` (four decimals), `©xyz`.
5. Read back and compared (NFR-REL-05); on failure the file is copied
   without metadata and the item is `failed` (`METADATA_WRITE`).
6. File modification and creation time set to the capture instant (FR-20).
7. fsync, then atomic rename into `YYYY/MM/` (FR-35). A target that already
   holds the same bytes is accepted (crash after rename); this item's own
   earlier file is replaced; anyone else's file makes the name `_2`, `_3`.
8. Checkpoint in SQLite (FR-33), temp removed.

## Output layout (SRS 3.7)

```text
<dest>/Memories Rescue/
  YYYY/MM/<YYYY-MM-DD>_<HHMMSS>_<SHORTID>.<ext>      local capture time
  YYYY/MM/<stem>-overlay.png                         overlay beside (pass-through, save separate)
  YYYY/MM/originals/<stem>-main.<ext>, -overlay.png  composited + keep originals
  YYYY/MM/<first clip stem>-joined.<ext>             clip rejoin (FR-27); the clips stay
  _report/report.html, report.csv, failed_items.json
  .memories-rescue/job-id                            marker
  .memories-rescue/tmp/                              per-item temp, removed after use
```

Day-precision items (no exact time) are placed at 12:00 local time on the
filename date, so the folder date equals the filename date in every zone.
Paths of 240 characters or more get the `\\?\` prefix on Windows; the output
folder key is compared in NFC (and case-folded on Windows and macOS).
