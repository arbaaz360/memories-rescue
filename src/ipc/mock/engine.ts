/**
 * MockBackend: the whole engine contract in TypeScript, for a plain browser
 * and for `tauri dev` before the Rust engine is wired. It serves the fake
 * export in ./scenario.ts and simulates a job on a sped-up clock: throughput,
 * ETA and byte counts are internally consistent (an "about 20 min" ETA at
 * 30x speed finishes in about 40 real seconds).
 *
 * Scenarios (URL `?mock=a,b`): `licensed`, `resume` (a paused job from two days
 * ago), `legacy` (links-only export), `diskfull` (engine pauses at item 600).
 * `?mock_speed=N` sets simulated seconds per real second (default 30).
 */

import type { Backend, Unlisten } from "../backend";
import { baseName, joinPath } from "../../format/format";
import { samplePaths, type Platform } from "../host";
import type {
  AppInfo,
  ClipGroup,
  CommandName,
  Commands,
  EngineEvents,
  ExportSummary,
  FolderNode,
  InputFile,
  IpcError,
  Issue,
  ItemOutcome,
  ItemStatus,
  JobOptions,
  JobReport,
  JobState,
  LicenseStatus,
  PlanItem,
  PlanPreview,
  ProgressEvent,
  ResumableJob,
  Settings,
  SpaceEstimate,
  Stage,
  StatusCounts,
  VolumeInfo,
} from "../types";
import {
  AVG_OVERLAY_BYTES,
  DATE_FIRST,
  DATE_LAST,
  EXPORT_BASE,
  LARGEST_ITEM_BYTES,
  MEDIA_SHARE,
  ORPHANS_WITHOUT_PART_3,
  PARTS,
  PROFILE,
  TOTAL_ENTRIES,
  fakeItem,
  firstIndexOfPart,
  partOf,
  rng,
  type FakeItem,
} from "./scenario";
import { mockThumbPath } from "./thumbs";

export const FREE_LIMIT = 100;
/** Keys the mock accepts: MOCK-XXXX-XXXX-XXXX with hex groups. Anything containing OFFLINE simulates no network. */
export const MOCK_KEY_PATTERN = /^MOCK(-[0-9A-F]{4}){3}$/;
export const MOCK_VALID_KEY = "MOCK-1234-5678-9ABC";
const PRODUCT_URL = "https://novabyteworks.gumroad.com/l/memories-rescue";

export interface MockConfig {
  scenarios?: readonly string[];
  /** Simulated seconds per real second. */
  speed?: number;
  tickMs?: number;
  /** Multiplier for command latencies (0 in tests). */
  latencyScale?: number;
  platform?: Platform;
  /** Real-time length of the indexing and matching stages before items start, in ms. */
  warmupMs?: number;
  /** Run the simulation on a timer (default). Tests set false and call tick() themselves. */
  autoTick?: boolean;
  now?: () => number;
}

/** Item costs in simulated seconds, already divided by the worker count. */
const COST = {
  photo: 0.12,
  photoComposite: 0.2,
  videoRemux: 0.45,
  videoComposite: 2.4,
  metadataOnlyPhoto: 0.05,
  metadataOnlyVideo: 0.3,
};

/** Items that fail the first time (fault removed on retry), by chronological index. 2944 sits in part 3. */
const FAILURE_INDICES: readonly number[] = [37, 813, 1702, 2944];
const PHOTO_FAILURES: readonly IpcError["code"][] = ["ZIP_ENTRY_CRC", "MEDIA_DECODE", "METADATA_WRITE"];

function failureCode(index: number, it: FakeItem): IpcError["code"] | undefined {
  const k = FAILURE_INDICES.indexOf(index);
  if (k < 0) return undefined;
  return it.media_type === "video" ? "FFMPEG_FAIL" : PHOTO_FAILURES[k % PHOTO_FAILURES.length];
}

function err(code: IpcError["code"], message: string, detail: string | null = null): IpcError {
  return { code, message, detail };
}

function emptyCounts(): StatusCounts {
  return { done: 0, metadata_partial: 0, location_uncertain: 0, missing_media: 0, failed: 0, skipped_free_limit: 0 };
}

interface PartAggregate {
  part: number;
  items: number;
  photos: number;
  videos: number;
  overlays: number;
  exact: number;
  ordinal: number;
  none: number;
  applied: number;
  uncertain: number;
  locNone: number;
  bytes: number;
  overlayBytes: number;
  photoCost: number;
  videoCost: number;
}

const aggregateCache = new Map<number, PartAggregate>();

/** Sums over a part's generated items, so every number the mock shows agrees with every other. */
export function aggregatePart(part: number): PartAggregate {
  const cached = aggregateCache.get(part);
  if (cached) return cached;
  const spec = PARTS.find((p) => p.part === part);
  if (!spec) throw new Error(`no part ${part}`);
  const agg: PartAggregate = {
    part,
    items: 0,
    photos: 0,
    videos: 0,
    overlays: 0,
    exact: 0,
    ordinal: 0,
    none: 0,
    applied: 0,
    uncertain: 0,
    locNone: 0,
    bytes: 0,
    overlayBytes: 0,
    photoCost: 0,
    videoCost: 0,
  };
  const start = firstIndexOfPart(part);
  for (let i = start; i < start + spec.mains; i++) {
    const it = fakeItem(i);
    agg.items += 1;
    if (it.media_type === "photo") agg.photos += 1;
    else agg.videos += 1;
    if (it.has_overlay) {
      agg.overlays += 1;
      agg.overlayBytes += it.bytes;
    }
    agg[it.confidence] += 1;
    if (it.location === "applied") agg.applied += 1;
    else if (it.location === "uncertain") agg.uncertain += 1;
    else agg.locNone += 1;
    agg.bytes += it.bytes;
  }
  aggregateCache.set(part, agg);
  return agg;
}

function sumAgg(parts: number[]): PartAggregate {
  const out = aggregatePart(PARTS[0].part);
  const total: PartAggregate = { ...out };
  for (const k of Object.keys(total) as (keyof PartAggregate)[]) total[k] = 0;
  for (const p of parts) {
    const a = aggregatePart(p);
    for (const k of Object.keys(total) as (keyof PartAggregate)[]) total[k] += a[k];
  }
  return total;
}

export function itemCost(it: FakeItem, options: JobOptions): number {
  const burnIn = !options.metadata_only && (options.overlay_mode === "burn_in_keep_originals" || options.overlay_mode === "burn_in_only");
  if (it.media_type === "photo") {
    if (options.metadata_only) return COST.metadataOnlyPhoto;
    return burnIn && it.has_overlay ? COST.photoComposite : COST.photo;
  }
  if (options.metadata_only) return COST.metadataOnlyVideo;
  return burnIn && it.has_overlay ? COST.videoComposite : COST.videoRemux;
}

function statusFor(it: FakeItem): ItemStatus {
  if (it.confidence === "none") return "metadata_partial";
  if (it.location === "uncertain") return "location_uncertain";
  return "done";
}

const STATUS_MESSAGES: Partial<Record<IpcError["code"], string>> = {
  ZIP_ENTRY_CRC: "CRC mismatch on memories/ entry",
  FFMPEG_FAIL: "ffmpeg exited with code 1 after the software retry",
  MEDIA_DECODE: "JPEG decoder: unexpected end of data",
  METADATA_WRITE: "EXIF write failed: APP1 segment too large",
};

interface InputRecord extends InputFile {
  legacy: boolean;
}

class MockJob {
  readonly id: string;
  options: JobOptions;
  inputIds: string[];
  parts: number[];
  state: JobState = "running";
  stage: Stage = "indexing";
  queue: number[] = [];
  cursor = 0;
  carry = 0;
  processed = 0;
  counts = emptyCounts();
  itemsTotal = 0;
  itemsDone = 0;
  bytesTotal = 0;
  bytesDone = 0;
  recent: ItemOutcome[] = [];
  failed: ItemOutcome[] = [];
  failedIdx: number[] = [];
  skipped: number[] = [];
  /** Items the free tier has covered; retries of these stay free (FR-44 counts the first 100 items). */
  freeSet = new Set<number>();
  remaining = { photo: 0, video: 0 };
  missingAccounted = false;
  faultsFixed = false;
  warmupLeftMs: number;
  postLeftMs = 0;
  ewma = { photo: COST.photo, video: COST.videoRemux };
  throughput = 0;
  doneThisRun = 0;
  pausing = false;
  pausedError: IpcError | null = null;
  createdAt: string;
  startedAt: string;
  finishedAt: string | null = null;
  diskFullAt: number | null;

  constructor(id: string, options: JobOptions, inputIds: string[], parts: number[], createdAt: string, warmupMs: number, diskFullAt: number | null) {
    this.id = id;
    this.options = options;
    this.inputIds = inputIds;
    this.parts = [...parts].sort((a, b) => a - b);
    this.createdAt = createdAt;
    this.startedAt = createdAt;
    this.warmupLeftMs = warmupMs;
    this.diskFullAt = diskFullAt;
    for (const p of this.parts) this.enqueuePart(p);
  }

  enqueuePart(part: number) {
    const spec = PARTS.find((s) => s.part === part);
    if (!spec) return;
    const start = firstIndexOfPart(part);
    const indices: number[] = [];
    for (let i = start; i < start + spec.mains; i++) {
      indices.push(i);
      this.bytesTotal += fakeItem(i).bytes;
    }
    this.push(indices);
    this.itemsTotal += spec.mains;
  }

  /** Queues items for (re)processing and keeps the per-type counts the ETA uses. */
  push(indices: number[]) {
    for (const i of indices) {
      this.queue.push(i);
      this.remaining[fakeItem(i).media_type] += 1;
    }
  }

  missingParts(): number[] {
    const max = Math.max(...PARTS.map((p) => p.part));
    const out: number[] = [];
    for (let n = 1; n <= max; n++) if (!this.parts.includes(n)) out.push(n);
    return out;
  }

  missingCount(): number {
    return PARTS.filter((p) => !this.parts.includes(p.part)).reduce((n, p) => n + p.mains, 0);
  }
}

export class MockBackend implements Backend {
  readonly kind = "mock" as const;
  readonly config: Required<Omit<MockConfig, "now">> & { now: () => number };
  private handlers = new Map<keyof EngineEvents, Set<(payload: never) => void>>();
  private inputs = new Map<string, InputRecord>();
  private nextInput = 1;
  private nextJob = 1;
  private jobs = new Map<string, MockJob>();
  private timer: ReturnType<typeof setInterval> | null = null;
  private settings: Settings;
  private license: LicenseStatus;
  private plans = new Map<string, { inputIds: string[]; options: JobOptions }>();
  private resumable: ResumableJob[] = [];
  /** Last path passed to open_path, for tests. */
  lastOpened: string | null = null;

  constructor(config: MockConfig = {}) {
    this.config = {
      scenarios: config.scenarios ?? [],
      speed: config.speed ?? 30,
      tickMs: config.tickMs ?? 250,
      latencyScale: config.latencyScale ?? 1,
      platform: config.platform ?? "windows",
      warmupMs: config.warmupMs ?? 2000,
      autoTick: config.autoTick ?? true,
      now: config.now ?? (() => Date.now()),
    };
    const paths = samplePaths(this.config.platform);
    const win = this.config.platform === "windows" || this.config.platform === "other";
    this.settings = {
      destination: win ? "C:\\Users\\you\\Pictures" : "/Users/you/Pictures",
      home_timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
      overlay_mode: "burn_in_keep_originals",
      keep_originals: true,
      clip_rejoin: true,
      clip_rejoin_years: { from: 2017, to: 2021 },
      apply_uncertain_locations: false,
      windows_compatibility: false,
      workers: null,
      output_template: "{YYYY-MM-DD}_{HHMMSS}_{shortid}",
      zip_output: false,
      zip_volume_bytes: null,
      update_checks: true,
      update_channel: "stable",
      telemetry: false,
      language: "system",
    };
    const licensed = this.has("licensed");
    this.license = {
      tier: licensed ? "licensed" : "free",
      key_masked: licensed ? "MOCK-****-****-9ABC" : null,
      verified_at: licensed ? new Date(this.config.now() - 86_400_000 * 12).toISOString() : null,
      last_check_at: licensed ? new Date(this.config.now() - 86_400_000 * 12).toISOString() : null,
      grace_until: null,
      product_url: PRODUCT_URL,
      price_display: null,
      free_limit: FREE_LIMIT,
    };
    if (this.has("resume")) {
      const dest = paths.destinations[0];
      this.resumable.push({
        job_id: "job-previous",
        output_path: joinPath(dest, "Memories Rescue"),
        created_at: new Date(this.config.now() - 2 * 86_400_000 - 3_600_000 * 5).toISOString(),
        state: "paused",
        items_done: 1950,
        items_total: PARTS.filter((p) => p.part !== 3).reduce((n, p) => n + p.mains, 0),
        input_names: PARTS.filter((p) => p.part !== 3).map((p) => p.file),
      });
    }
  }

  private has(scenario: string): boolean {
    return this.config.scenarios.includes(scenario);
  }

  // -------------------------------------------------------------------------
  // Backend
  // -------------------------------------------------------------------------

  async on<E extends keyof EngineEvents>(event: E, handler: (payload: EngineEvents[E]) => void): Promise<Unlisten> {
    let set = this.handlers.get(event);
    if (!set) {
      set = new Set();
      this.handlers.set(event, set);
    }
    set.add(handler as (payload: never) => void);
    return () => set.delete(handler as (payload: never) => void);
  }

  private emit<E extends keyof EngineEvents>(event: E, payload: EngineEvents[E]) {
    for (const h of this.handlers.get(event) ?? []) (h as (p: EngineEvents[E]) => void)(payload);
  }

  async call<K extends CommandName>(command: K, args: Commands[K]["args"]): Promise<Commands[K]["result"]> {
    const handler = this.commands[command] as (a: Commands[K]["args"]) => Commands[K]["result"] | Promise<Commands[K]["result"]>;
    await this.delay(LATENCY[command] ?? 80);
    // Copies, so the UI never holds the mock's own objects.
    return structuredClone(await handler(args));
  }

  private delay(ms: number): Promise<void> {
    const scaled = ms * this.config.latencyScale;
    return scaled <= 0 ? Promise.resolve() : new Promise((r) => setTimeout(r, scaled));
  }

  /** One handler per command; the mapped type makes a missing command a compile error. */
  private readonly commands: { [K in CommandName]: (args: Commands[K]["args"]) => Commands[K]["result"] | Promise<Commands[K]["result"]> } = {
    add_inputs: ({ paths }) => this.addInputs(paths),
    remove_input: ({ id }) => {
      this.inputs.delete(id);
      return null;
    },
    analyze: ({ input_ids }) => this.analyze(input_ids),
    plan: ({ input_ids, options }) => this.plan(input_ids, options),
    export_preview_report: ({ plan_id }) => {
      const p = this.plans.get(plan_id);
      if (!p) throw err("UNKNOWN", "unknown plan");
      return joinPath(p.options.destination, "Memories Rescue", "_report", "preview-report.html");
    },
    estimate_space: ({ input_ids, options }) => this.estimate(this.presentParts(input_ids), options),
    volume_info: ({ path }) => this.volume(path),
    start: ({ input_ids, options }) => this.start(input_ids, options),
    pause: ({ job_id }) => this.pause(job_id),
    resume: ({ job_id }) => this.resume(job_id),
    cancel: ({ job_id }) => this.cancel(job_id),
    retry_failed: ({ job_id, options }) => this.retry(job_id, options),
    add_part: ({ job_id, path }) => this.addPart(job_id, path),
    get_report: ({ job_id }) => this.report(this.job(job_id)),
    list_resumable_jobs: () => this.listResumable(),
    get_settings: () => this.settings,
    set_settings: ({ settings }) => {
      this.settings = { ...settings };
      return this.settings;
    },
    telemetry_preview: () => ({
      app_version: __APP_VERSION__,
      os_family: this.config.platform,
      items_processed_bucket: "1k-10k",
      error_code_counts: { FFMPEG_FAIL: 1, MEDIA_DECODE: 1, METADATA_WRITE: 1, ZIP_ENTRY_CRC: 1 },
      crash_signatures: [],
    }),
    license_status: () => this.license,
    activate: ({ key }) => this.activate(key),
    deactivate: () => {
      this.license = { ...this.license, tier: "free", key_masked: null, verified_at: null, last_check_at: null, grace_until: null };
      return this.license;
    },
    open_path: ({ path }) => {
      this.lastOpened = path;
      console.info(`[mock] open_path ${path}`);
      return null;
    },
    export_diagnostics: () => {
      const win = this.config.platform === "windows" || this.config.platform === "other";
      const day = new Date(this.config.now()).toISOString().slice(0, 10);
      return win
        ? `C:\\Users\\you\\Desktop\\memories-rescue-diagnostics-${day}.zip`
        : `/Users/you/Desktop/memories-rescue-diagnostics-${day}.zip`;
    },
    app_info: () => this.appInfo(),
    sidecar_versions: () => {
      throw err("UNKNOWN", "The bundled tools only run inside the app, not in the browser preview.");
    },
  };

  // -------------------------------------------------------------------------
  // Inputs and analysis
  // -------------------------------------------------------------------------

  private addInputs(paths: string[]): InputFile[] {
    for (const path of paths) {
      if ([...this.inputs.values()].some((i) => i.path === path)) continue;
      const name = baseName(path);
      const id = `in-${this.nextInput++}`;
      const lower = name.toLowerCase();
      const ext = /\.([a-z0-9]+)$/.exec(lower)?.[1] ?? null;
      const record: InputRecord = {
        id,
        path,
        name,
        size_bytes: 0,
        kind: ext === "zip" ? "zip" : ext === null ? "folder" : "other",
        role: "unknown",
        part_number: null,
        status: "ok",
        issue: null,
        legacy: false,
      };
      if (record.kind === "folder" && lower === EXPORT_BASE.toLowerCase()) {
        record.role = "primary";
        record.part_number = 1;
        record.size_bytes = PARTS[0].zip_bytes;
      } else if (record.kind === "zip" && partOf(name) !== null) {
        const part = partOf(name) as number;
        const spec = PARTS.find((p) => p.part === part);
        record.part_number = part;
        record.role = part === 1 ? "primary" : "part";
        record.size_bytes = spec?.zip_bytes ?? 0;
        record.legacy = this.has("legacy");
        if (record.legacy) record.size_bytes = part === 1 ? 21_400_000 : 4_100_000;
        const twin = [...this.inputs.values()].find((i) => i.part_number === part && i.status === "ok");
        if (twin) {
          record.status = "duplicate";
          record.issue = { code: "INPUT_NOT_EXPORT", message: `Same file as ${twin.name}; read once.`, count: null, part_number: part };
        }
      } else {
        record.role = "ignored";
        record.status = "not_export";
        record.size_bytes = ext === "zip" ? 48_211_009 : 3_480_115;
        record.issue = { code: "INPUT_NOT_EXPORT", message: "This doesn't look like a Snapchat export.", count: null, part_number: null };
      }
      this.inputs.set(id, record);
    }
    return [...this.inputs.values()].map(({ legacy: _legacy, ...rest }) => rest);
  }

  private usable(inputIds: string[]): InputRecord[] {
    return inputIds
      .map((id) => this.inputs.get(id))
      .filter((i): i is InputRecord => !!i && (i.status === "ok" || i.status === "truncated") && i.part_number !== null);
  }

  private presentParts(inputIds: string[]): number[] {
    return [...new Set(this.usable(inputIds).map((i) => i.part_number as number))].sort((a, b) => a - b);
  }

  private analyze(inputIds: string[]): ExportSummary {
    const usable = this.usable(inputIds);
    if (usable.length === 0) throw err("INPUT_NOT_EXPORT", "None of these files is a Snapchat export.");
    const present = this.presentParts(inputIds);
    const max = Math.max(...present);
    const missing: number[] = [];
    for (let n = 1; n <= max; n++) if (!present.includes(n)) missing.push(n);
    const agg = sumAgg(present);
    const all = sumAgg(PARTS.map((p) => p.part));
    const hasPrimary = present.includes(1);
    const legacy = usable.some((u) => u.legacy);
    const folderInput = usable.some((u) => u.kind === "folder");
    const issues: Issue[] = [];
    const parts = Array.from({ length: max }, (_, i) => i + 1).map((n) => {
      const input = usable.find((u) => u.part_number === n) ?? null;
      return {
        part_number: n,
        input_id: input?.id ?? null,
        name: input?.name ?? null,
        size_bytes: input?.size_bytes ?? 0,
        status: input ? (input.status === "truncated" ? ("truncated" as const) : ("ok" as const)) : ("missing" as const),
        entries: input ? aggregatePart(n).items : 0,
      };
    });

    if (legacy) {
      issues.push({ code: "LEGACY_LINKS_ONLY", message: "This export contains links instead of media.", count: TOTAL_ENTRIES, part_number: null });
      return {
        export_id: `exp-${EXPORT_BASE}`,
        input_ids: usable.map((u) => u.id),
        profile: "legacy",
        profile_counts: { exact: 0, ordinal: 0, day_only: 0, metadata_only: TOTAL_ENTRIES },
        parts,
        missing_parts: [],
        counts: { entries: TOTAL_ENTRIES, photos: 0, videos: 0, overlays: 0, orphan_overlays: 0, with_location: all.applied + all.uncertain, unrecognised: 0 },
        date_range: { first: DATE_FIRST, last: DATE_LAST },
        total_bytes: usable.reduce((n, u) => n + u.size_bytes, 0),
        issues,
        blocked: true,
      };
    }

    for (const n of missing) {
      issues.push({
        code: "PART_MISSING",
        message: `Part ${n} appears to be missing.`,
        count: hasPrimary ? aggregatePart(n).items : null,
        part_number: n,
      });
    }
    if (!hasPrimary) issues.push({ code: "JSON_MISSING", message: "Exact times aren't available; using dates from filenames.", count: agg.items, part_number: 1 });
    if (agg.none > 0 && hasPrimary) issues.push({ code: "MATCH_PARTIAL", message: `Exact time unknown for ${agg.none} items.`, count: agg.none, part_number: null });
    if (folderInput) issues.push({ code: "MATCH_PARTIAL", message: "Extracted folder: exact times need the original ZIP.", count: null, part_number: 1 });
    if (agg.uncertain > 0 && hasPrimary) issues.push({ code: "LOCATION_UNCERTAIN", message: `Location not applied for ${agg.uncertain} items because the match was uncertain.`, count: agg.uncertain, part_number: null });

    const orphans = !present.includes(3) && (present.includes(2) || present.includes(4)) ? ORPHANS_WITHOUT_PART_3 : 0;
    return {
      export_id: `exp-${EXPORT_BASE}`,
      input_ids: usable.map((u) => u.id),
      profile: hasPrimary ? PROFILE : "unknown",
      profile_counts: hasPrimary
        ? { exact: all.exact, ordinal: all.ordinal, day_only: all.none, metadata_only: 0 }
        : { exact: 0, ordinal: 0, day_only: agg.items, metadata_only: 0 },
      parts,
      missing_parts: missing,
      counts: {
        entries: hasPrimary ? TOTAL_ENTRIES : 0,
        photos: agg.photos,
        videos: agg.videos,
        overlays: agg.overlays,
        orphan_overlays: orphans,
        with_location: hasPrimary ? agg.applied + agg.uncertain : 0,
        unrecognised: 0,
      },
      date_range: { first: DATE_FIRST, last: DATE_LAST },
      total_bytes: usable.reduce((n, u) => n + u.size_bytes, 0),
      issues,
      blocked: false,
    };
  }

  // -------------------------------------------------------------------------
  // Space and volumes
  // -------------------------------------------------------------------------

  private volume(path: string): VolumeInfo {
    const p = path.toUpperCase();
    const gb = 1_000_000_000;
    if (p.startsWith("E:") || p.startsWith("/VOLUMES/USB")) {
      return { path, volume: p.startsWith("E:") ? "E:" : "USB", free_bytes: 10.9 * gb, total_bytes: 31.9 * gb, writable: true };
    }
    if (p.startsWith("D:")) return { path, volume: "D:", free_bytes: 412.7 * gb, total_bytes: 931.5 * gb, writable: true };
    if (p.startsWith("C:")) return { path, volume: "C:", free_bytes: 61.4 * gb, total_bytes: 476.3 * gb, writable: true };
    return { path, volume: "Macintosh HD", free_bytes: 88.2 * gb, total_bytes: 494.4 * gb, writable: true };
  }

  private estimate(parts: number[], options: JobOptions): SpaceEstimate {
    const agg = sumAgg(parts);
    const burnIn = !options.metadata_only && (options.overlay_mode === "burn_in_keep_originals" || options.overlay_mode === "burn_in_only");
    const media = agg.bytes * MEDIA_SHARE;
    let output = media;
    if (burnIn) output += agg.overlays * AVG_OVERLAY_BYTES;
    if (options.overlay_mode === "separate_file" && !options.metadata_only) output += agg.overlays * AVG_OVERLAY_BYTES;
    if (options.clip_rejoin && !options.metadata_only) output += 48_000_000;
    const originals = burnIn && options.keep_originals ? agg.overlayBytes + agg.overlays * AVG_OVERLAY_BYTES : 0;
    const workers = options.workers ?? 4;
    const temp = LARGEST_ITEM_BYTES * workers * 2;
    const zip = options.zip_output ? output + originals : 0;
    const total = output + originals + temp + zip;
    const vol = this.volume(options.destination || this.settings.destination);
    const needed = total * 1.1;
    return {
      output_bytes: Math.round(output),
      originals_bytes: Math.round(originals),
      temp_bytes: temp,
      zip_bytes: Math.round(zip),
      total_bytes: Math.round(total),
      free_bytes: vol.free_bytes,
      sufficient: vol.free_bytes >= needed,
      shortfall_bytes: Math.max(0, Math.round(needed - vol.free_bytes)),
      volume: vol.volume,
    };
  }

  // -------------------------------------------------------------------------
  // Plan (dry run)
  // -------------------------------------------------------------------------

  private plan(inputIds: string[], options: JobOptions): PlanPreview {
    const summary = this.analyze(inputIds);
    if (summary.blocked) throw err("LEGACY_LINKS_ONLY", "This export contains links instead of media.");
    const parts = this.presentParts(inputIds);
    const agg = sumAgg(parts);
    const burnIn = !options.metadata_only && (options.overlay_mode === "burn_in_keep_originals" || options.overlay_mode === "burn_in_only");
    const indices: number[] = [];
    for (const p of parts) {
      const start = firstIndexOfPart(p);
      const n = aggregatePart(p).items;
      for (let i = start; i < start + n; i++) indices.push(i);
    }

    // Folder tree: Memories Rescue / YYYY / MM (+ originals) / files, plus _report.
    const years = new Map<number, Map<number, FakeItem[]>>();
    let seconds = 0;
    for (const i of indices) {
      const it = fakeItem(i);
      seconds += itemCost(it, options);
      let months = years.get(it.year);
      if (!months) {
        months = new Map();
        years.set(it.year, months);
      }
      const list = months.get(it.month) ?? [];
      list.push(it);
      months.set(it.month, list);
    }
    const tree: FolderNode = {
      name: "Memories Rescue",
      file_count: 0,
      sample_files: [],
      children: [
        ...[...years.entries()]
          .sort((a, b) => a[0] - b[0])
          .map(([year, months]) => ({
            name: String(year),
            file_count: 0,
            sample_files: [],
            children: [...months.entries()]
              .sort((a, b) => a[0] - b[0])
              .map(([month, items]) => {
                const composited = burnIn && options.keep_originals ? items.filter((x) => x.has_overlay) : [];
                return {
                  name: String(month).padStart(2, "0"),
                  file_count: items.length,
                  sample_files: items.slice(0, 3).map((x) => baseName(x.planned_path)),
                  children:
                    composited.length > 0
                      ? [{ name: "originals", children: [], file_count: composited.length * 2, sample_files: composited.slice(0, 2).map((x) => x.source_name) }]
                      : [],
                };
              }),
          })),
        { name: "_report", children: [], file_count: 3, sample_files: ["report.html", "report.csv", "failed_items.json"] },
      ],
    };

    // Samples: evenly spread, plus the first day-only and the first uncertain-location item.
    const picks = new Set<number>();
    const want = 22;
    for (let k = 0; k < want; k++) picks.add(indices[Math.floor((k * (indices.length - 1)) / (want - 1))]);
    const firstNone = indices.find((i) => fakeItem(i).confidence === "none");
    const firstUncertain = indices.find((i) => fakeItem(i).location === "uncertain");
    if (firstNone !== undefined) picks.add(firstNone);
    if (firstUncertain !== undefined) picks.add(firstUncertain);
    const samples: PlanItem[] = [...picks]
      .sort((a, b) => a - b)
      .map((i) => {
        const it = fakeItem(i);
        const showLocation = it.place && (it.location === "applied" || (it.location === "uncertain" && options.apply_uncertain_locations));
        return {
          item_id: it.id,
          media_type: it.media_type,
          source_name: it.source_name,
          planned_path: it.planned_path,
          capture_time: it.capture_time,
          time_precision: it.confidence === "none" || (it.location === "uncertain" && !options.apply_uncertain_locations) ? "day" : "second",
          latitude: it.place ? it.place.lat : null,
          longitude: it.place ? it.place.lon : null,
          match_confidence: it.confidence,
          location_decision: showLocation ? it.location : it.location === "uncertain" ? "uncertain" : "none",
          has_overlay: it.has_overlay,
          thumbnail_path: mockThumbPath(i, it.media_type),
        };
      });

    const clipGroups: ClipGroup[] = [];
    const inRange = (y: number) => !options.clip_rejoin_years || (y >= options.clip_rejoin_years.from && y <= options.clip_rejoin_years.to);
    if (options.clip_rejoin && !options.metadata_only) {
      if (parts.includes(1) && inRange(2018)) {
        clipGroups.push({
          group_id: "clip-2018-06-02-7C1E9A40",
          item_ids: ["7C1E9A40", "7C1E9A41", "7C1E9A42"],
          total_seconds: 24.0,
          planned_path: "2018/06/2018-06-02_140311_7C1E9A40_joined.mp4",
          start_time: "2018-06-02T14:03:11+01:00",
        });
      }
      if (parts.includes(2) && inRange(2019)) {
        clipGroups.push({
          group_id: "clip-2019-12-24-B3D0F218",
          item_ids: ["B3D0F218", "B3D0F219", "B3D0F21A", "B3D0F21B"],
          total_seconds: 37.6,
          planned_path: "2019/12/2019-12-24_194205_B3D0F218_joined.mp4",
          start_time: "2019-12-24T19:42:05-05:00",
        });
      }
    }

    const warnings = summary.issues.filter((i) => i.code !== "JSON_MISSING");
    const planId = `plan-${this.plans.size + 1}`;
    this.plans.set(planId, { inputIds, options });
    const missingMedia = summary.missing_parts.reduce((n, p) => n + aggregatePart(p).items, 0);
    const postSeconds = (clipGroups.length > 0 ? 20 : 0) + (options.zip_output ? 240 : 0) + 15;
    return {
      plan_id: planId,
      totals: {
        items: agg.items,
        photos: agg.photos,
        videos: agg.videos,
        overlays_to_composite: burnIn ? agg.overlays : 0,
        originals_kept: burnIn && options.keep_originals ? agg.overlays : 0,
        missing_media: missingMedia,
        unrecognised: 0,
      },
      confidence: { exact: agg.exact, ordinal: agg.ordinal, none: agg.none },
      location: { applied: agg.applied, none: agg.locNone, uncertain: agg.uncertain },
      tree,
      samples,
      clip_groups: clipGroups,
      warnings,
      space: this.estimate(parts, options),
      estimated_seconds: Math.round(seconds + postSeconds),
      free_limit: {
        applies: this.license.tier === "free",
        limit: FREE_LIMIT,
        items_beyond: this.license.tier === "free" ? Math.max(0, agg.items - FREE_LIMIT) : 0,
      },
    };
  }

  // -------------------------------------------------------------------------
  // Jobs
  // -------------------------------------------------------------------------

  private job(id: string): MockJob {
    const j = this.jobs.get(id);
    if (!j) throw err("UNKNOWN", `No job ${id}`);
    return j;
  }

  private start(inputIds: string[], options: JobOptions): string {
    const summary = this.analyze(inputIds);
    if (summary.blocked) throw err("LEGACY_LINKS_ONLY", "This export contains links instead of media.");
    const est = this.estimate(this.presentParts(inputIds), options);
    if (!est.sufficient) throw err("DISK_FULL", `Not enough space on ${est.volume}.`, `${est.shortfall_bytes} bytes short`);
    const id = `job-${this.nextJob++}`;
    const job = new MockJob(
      id,
      structuredClone(options),
      inputIds,
      this.presentParts(inputIds),
      new Date(this.config.now()).toISOString(),
      this.config.warmupMs,
      this.has("diskfull") ? 600 : null,
    );
    this.jobs.set(id, job);
    this.settings = { ...this.settings, destination: options.destination };
    this.setState(job, "running", null);
    this.ensureTimer();
    return id;
  }

  private pause(jobId: string): null {
    const job = this.job(jobId);
    if (job.state !== "running" || job.pausing) return null;
    job.pausing = true;
    // In-flight items finish first (FR-34).
    setTimeout(() => {
      job.pausing = false;
      if (job.state === "running") this.setState(job, "paused", null);
    }, 400 * this.config.latencyScale);
    return null;
  }

  private resume(jobId: string): null {
    let job = this.jobs.get(jobId);
    if (!job) job = this.loadResumable(jobId);
    if (job.state === "running") return null;
    if (job.state === "free_limit_reached" && this.license.tier === "free") {
      this.skipRest(job);
      this.finish(job);
      return null;
    }
    if (job.state === "partial" || job.state === "done" || job.state === "cancelled") {
      // After unlocking, the skipped items; after add_part, the newly available ones (already queued).
      if (this.license.tier !== "free" && job.skipped.length > 0) {
        job.push(job.skipped);
        job.itemsDone -= job.skipped.length;
        job.counts.skipped_free_limit -= job.skipped.length;
        job.skipped = [];
      }
      if (job.cursor >= job.queue.length) return null;
      job.finishedAt = null;
      job.missingAccounted = false;
      job.counts.missing_media = 0;
    }
    job.pausedError = null;
    job.doneThisRun = 0;
    this.setState(job, "running", null);
    this.ensureTimer();
    return null;
  }

  private cancel(jobId: string): null {
    const job = this.job(jobId);
    if (job.state !== "running" && job.state !== "paused" && job.state !== "free_limit_reached") return null;
    setTimeout(() => {
      job.finishedAt = new Date(this.config.now()).toISOString();
      job.stage = "idle";
      this.setState(job, "cancelled", null);
      this.emitProgress(job);
    }, 300 * this.config.latencyScale);
    job.state = "paused"; // stop processing at once
    return null;
  }

  private retry(jobId: string, options: JobOptions | null): null {
    const job = this.job(jobId);
    if (job.failed.length === 0) return null;
    if (options) job.options = structuredClone(options);
    for (const i of job.failedIdx) job.bytesDone -= fakeItem(i).bytes;
    job.push(job.failedIdx);
    job.failedIdx = [];
    job.itemsDone -= job.failed.length;
    job.counts.failed -= job.failed.length;
    job.failed = [];
    job.faultsFixed = true;
    job.finishedAt = null;
    job.missingAccounted = false;
    job.counts.missing_media = 0;
    job.doneThisRun = 0;
    job.warmupLeftMs = 300;
    this.setState(job, "running", null);
    this.ensureTimer();
    return null;
  }

  private addPart(jobId: string, path: string): InputFile {
    const job = this.job(jobId);
    const part = partOf(baseName(path));
    if (part === null) throw err("INPUT_NOT_EXPORT", "This file isn't a part of this export.");
    const [input] = this.addInputs([path]).filter((i) => i.path === path);
    if (!job.parts.includes(part)) {
      job.parts.push(part);
      job.parts.sort((a, b) => a - b);
      job.inputIds.push(input.id);
      job.enqueuePart(part);
    }
    return input;
  }

  private loadResumable(jobId: string): MockJob {
    const prev = this.resumable.find((r) => r.job_id === jobId);
    if (!prev) throw err("UNKNOWN", `No job ${jobId}`);
    const parts = PARTS.filter((p) => p.part !== 3).map((p) => p.part);
    const ids = this.addInputs(PARTS.filter((p) => p.part !== 3).map((p) => samplePaths(this.config.platform).join(samplePaths(this.config.platform).downloads, p.file)))
      .filter((i) => i.part_number !== null && i.status === "ok")
      .map((i) => i.id);
    const options: JobOptions = {
      destination: prev.output_path.replace(/[\\/]Memories Rescue$/, ""),
      overlay_mode: this.settings.overlay_mode,
      keep_originals: this.settings.keep_originals,
      clip_rejoin: this.settings.clip_rejoin,
      clip_rejoin_years: this.settings.clip_rejoin_years,
      apply_uncertain_locations: this.settings.apply_uncertain_locations,
      windows_compatibility: this.settings.windows_compatibility,
      zip_output: this.settings.zip_output,
      zip_volume_bytes: this.settings.zip_volume_bytes,
      home_timezone: this.settings.home_timezone,
      metadata_only: false,
      workers: this.settings.workers,
      excluded_clip_groups: [],
    };
    const job = new MockJob(jobId, options, ids, parts, prev.created_at, 600, null);
    // Replay the work done before the app was closed, without events.
    while (job.processed < prev.items_done) this.processOne(job, false);
    job.doneThisRun = 0;
    job.state = "paused";
    this.jobs.set(jobId, job);
    this.resumable = this.resumable.filter((r) => r.job_id !== jobId);
    return job;
  }

  private listResumable(): ResumableJob[] {
    const live = [...this.jobs.values()]
      .filter((j) => j.state === "paused" || j.state === "free_limit_reached")
      .map((j) => ({
        job_id: j.id,
        output_path: joinPath(j.options.destination, "Memories Rescue"),
        created_at: j.createdAt,
        state: j.state,
        items_done: j.itemsDone,
        items_total: j.itemsTotal,
        input_names: j.inputIds.map((id) => this.inputs.get(id)?.name ?? id),
      }));
    return [...live, ...this.resumable].sort((a, b) => b.created_at.localeCompare(a.created_at));
  }

  // -------------------------------------------------------------------------
  // Simulation
  // -------------------------------------------------------------------------

  private ensureTimer() {
    if (this.timer !== null || !this.config.autoTick) return;
    this.timer = setInterval(() => this.tick(), this.config.tickMs);
  }

  /** Advances every running job by one tick. Public so tests can drive it without timers. */
  tick() {
    let anyRunning = false;
    for (const job of this.jobs.values()) {
      if (job.state !== "running") continue;
      anyRunning = true;
      this.step(job, this.config.tickMs);
    }
    if (!anyRunning && this.timer !== null) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  private step(job: MockJob, realMs: number) {
    if (job.pausing) {
      this.emitProgress(job);
      return;
    }
    if (job.warmupLeftMs > 0) {
      job.warmupLeftMs -= realMs;
      job.stage = job.warmupLeftMs > this.config.warmupMs / 2 ? "indexing" : "matching";
      this.emitProgress(job);
      return;
    }
    const simDt = (realMs / 1000) * this.config.speed;
    let budget = simDt + job.carry;
    let finished = 0;
    while (job.cursor < job.queue.length) {
      const next = job.queue[job.cursor];
      if (this.license.tier === "free" && !job.freeSet.has(next) && job.freeSet.size >= FREE_LIMIT) {
        job.carry = 0;
        job.stage = "idle";
        this.setState(job, "free_limit_reached", null);
        this.emitProgress(job);
        return;
      }
      if (job.diskFullAt !== null && job.processed >= job.diskFullAt) {
        job.diskFullAt = null;
        job.stage = "idle";
        job.pausedError = err("DISK_FULL_IMMINENT", "Less than 2 GB free on the destination.", null);
        this.setState(job, "paused", job.pausedError);
        this.emitProgress(job);
        return;
      }
      const it = fakeItem(next);
      const jitter = 0.7 + rng(next * 31 + 7)() * 0.6;
      const cost = itemCost(it, job.options) * jitter;
      if (budget < cost) break;
      budget -= cost;
      const kind = it.media_type;
      // FR-32: separate moving averages for photos and videos.
      job.ewma[kind] = job.ewma[kind] * 0.9 + cost * 0.1;
      this.processOne(job, true);
      finished += 1;
      job.stage = it.has_overlay && job.options.overlay_mode.startsWith("burn_in") && !job.options.metadata_only ? "compositing" : kind === "video" ? "writing" : "tagging";
    }
    job.carry = budget;
    if (job.cursor < job.queue.length) {
      job.throughput = job.throughput === 0 ? finished / simDt : job.throughput * 0.8 + (finished / simDt) * 0.2;
      this.emitProgress(job);
      return;
    }
    // Post-processing stages, in real time.
    if (job.postLeftMs === 0 && !job.finishedAt) {
      const groups = job.options.clip_rejoin && !job.options.metadata_only ? 1 : 0;
      job.postLeftMs = (groups ? 900 : 0) + (job.options.zip_output ? 1800 : 0) + 600;
    }
    job.postLeftMs -= realMs;
    const zipMs = job.options.zip_output ? 1800 : 0;
    job.stage = job.postLeftMs > 600 + zipMs ? "rejoining" : job.postLeftMs > 600 ? "zipping" : "reporting";
    if (job.postLeftMs > 0) {
      this.emitProgress(job);
      return;
    }
    job.postLeftMs = 0;
    this.finish(job);
  }

  /** Processes the item at the cursor. `live` emits events; replay does not. */
  private processOne(job: MockJob, live: boolean) {
    const index = job.queue[job.cursor];
    job.cursor += 1;
    const it = fakeItem(index);
    job.remaining[it.media_type] -= 1;
    job.freeSet.add(index);
    const failCode = job.faultsFixed ? undefined : failureCode(index, it);
    const status: ItemStatus = failCode ? "failed" : statusFor(it);
    const outcome: ItemOutcome = {
      item_id: it.id,
      name: it.source_name,
      media_type: it.media_type,
      status,
      error_code: failCode ?? (status === "metadata_partial" ? "MATCH_PARTIAL" : status === "location_uncertain" ? "LOCATION_UNCERTAIN" : null),
      message: failCode ? (STATUS_MESSAGES[failCode] ?? null) : null,
      output_path: failCode ? null : joinPath(job.options.destination, "Memories Rescue", ...it.planned_path.split("/")),
      finished_at: new Date(this.config.now()).toISOString(),
    };
    job.processed += 1;
    job.doneThisRun += 1;
    job.itemsDone += 1;
    job.bytesDone += it.bytes;
    job.counts[status] += 1;
    if (status === "failed") {
      job.failed.push(outcome);
      job.failedIdx.push(index);
    }
    job.recent.unshift(outcome);
    if (job.recent.length > 20) job.recent.length = 20;
    if (live) this.emit("item", { job_id: job.id, item: outcome });
  }

  private skipRest(job: MockJob) {
    while (job.cursor < job.queue.length) {
      const index = job.queue[job.cursor];
      job.cursor += 1;
      const it = fakeItem(index);
      job.remaining[it.media_type] -= 1;
      job.skipped.push(index);
      job.counts.skipped_free_limit += 1;
      job.itemsDone += 1;
      this.emit("item", {
        job_id: job.id,
        item: {
          item_id: it.id,
          name: it.source_name,
          media_type: it.media_type,
          status: "skipped_free_limit",
          error_code: "FREE_LIMIT",
          message: null,
          output_path: null,
          finished_at: new Date(this.config.now()).toISOString(),
        },
      });
    }
  }

  private finish(job: MockJob) {
    if (!job.missingAccounted) {
      job.missingAccounted = true;
      job.counts.missing_media = job.missingCount();
    }
    job.stage = "idle";
    job.finishedAt = new Date(this.config.now()).toISOString();
    const state: JobState = job.counts.failed > 0 || job.counts.skipped_free_limit > 0 ? "partial" : "done";
    this.setState(job, state, null);
    this.emitProgress(job);
  }

  private setState(job: MockJob, state: JobState, error: IpcError | null) {
    job.state = state;
    this.emit("job_state", { job_id: job.id, state, error, at: new Date(this.config.now()).toISOString() });
  }

  private eta(job: MockJob): number | null {
    if (job.doneThisRun < 10) return null;
    let photos = job.remaining.photo;
    let videos = job.remaining.video;
    if (this.license.tier === "free") {
      const left = Math.max(0, FREE_LIMIT - job.freeSet.size);
      const ratio = photos + videos === 0 ? 0 : Math.min(1, left / (photos + videos));
      photos *= ratio;
      videos *= ratio;
    }
    const post = (job.options.clip_rejoin ? 20 : 0) + (job.options.zip_output ? 240 : 0) + 15;
    return Math.round(photos * job.ewma.photo + videos * job.ewma.video + post);
  }

  private emitProgress(job: MockJob) {
    const running = job.state === "running";
    const finished = job.state === "done" || job.state === "partial" || job.state === "cancelled" || job.state === "failed";
    const pct = job.itemsTotal === 0 ? 0 : (job.itemsDone / job.itemsTotal) * 100;
    const progress: ProgressEvent = {
      job_id: job.id,
      overall_pct: finished && job.state !== "cancelled" ? 100 : Math.min(99, pct),
      items_done: job.itemsDone,
      items_total: job.itemsTotal,
      bytes_done: job.bytesDone,
      bytes_total: job.bytesTotal,
      stage: job.stage,
      throughput_items_per_s: running ? job.throughput : 0,
      eta_seconds: running ? this.eta(job) : null,
      warnings: job.counts.metadata_partial + job.counts.location_uncertain,
      failures: job.counts.failed,
      recent_items: job.recent.slice(0, 20),
      sleep_prevented: running,
    };
    this.emit("progress", progress);
  }

  private report(job: MockJob): JobReport {
    const finished = job.finishedAt !== null;
    const out = joinPath(job.options.destination, "Memories Rescue");
    const issues: Issue[] = job.missingParts().map((n) => ({
      code: "PART_MISSING" as const,
      message: `Part ${n} appears to be missing.`,
      count: aggregatePart(n).items,
      part_number: n,
    }));
    return {
      job_id: job.id,
      state: job.state,
      output_path: out,
      report_html_path: finished ? joinPath(out, "_report", "report.html") : null,
      report_csv_path: finished ? joinPath(out, "_report", "report.csv") : null,
      started_at: job.startedAt,
      finished_at: job.finishedAt,
      items_total: job.itemsTotal + (finished ? job.counts.missing_media : 0),
      bytes_done: job.bytesDone,
      counts: { ...job.counts },
      missing_parts: job.missingParts(),
      failures: job.failed.slice(0, 200),
      issues,
      can_retry: job.counts.failed > 0,
      can_unlock: job.counts.skipped_free_limit > 0,
      can_add_part: job.missingParts().length > 0,
      options: structuredClone(job.options),
    };
  }

  // -------------------------------------------------------------------------
  // Licence and app info
  // -------------------------------------------------------------------------

  private activate(key: string): LicenseStatus {
    const k = key.trim().toUpperCase();
    if (k.includes("OFFLINE")) throw err("LICENSE_OFFLINE", "Couldn't reach Gumroad.", "network unreachable (mock)");
    if (!MOCK_KEY_PATTERN.test(k)) throw err("LICENSE_INVALID", "That key wasn't recognised.");
    const now = new Date(this.config.now()).toISOString();
    this.license = {
      ...this.license,
      tier: "licensed",
      key_masked: `MOCK-****-****-${k.slice(-4)}`,
      verified_at: now,
      last_check_at: now,
      grace_until: null,
    };
    return this.license;
  }

  private appInfo(): AppInfo {
    const win = this.config.platform === "windows" || this.config.platform === "other";
    const root = win ? "C:\\Users\\you\\AppData\\Local\\Memories Rescue" : "/Applications/Memories Rescue.app/Contents/Resources";
    return {
      version: __APP_VERSION__,
      os_family: this.config.platform,
      licenses_path: joinPath(root, "LICENSES"),
      third_party_notices_path: joinPath(root, "THIRD_PARTY_NOTICES.md"),
      ffmpeg_source_url: "https://novabyteworks.com/memories-rescue/ffmpeg-source",
      privacy_policy_url: "https://novabyteworks.com/memories-rescue/privacy",
      support_email: "support@novabyteworks.com",
      website_url: "https://novabyteworks.com",
    };
  }
}

/** Simulated latencies so loading states are visible in the browser. */
const LATENCY: Partial<Record<CommandName, number>> = {
  add_inputs: 250,
  analyze: 900,
  plan: 1300,
  estimate_space: 60,
  volume_info: 40,
  start: 300,
  activate: 900,
  export_diagnostics: 700,
  export_preview_report: 500,
};

/** Reads `?mock=...` and `?mock_speed=...` from the page URL. */
export function mockConfigFromUrl(search: string, platform: Platform): MockConfig {
  const params = new URLSearchParams(search);
  const scenarios = (params.get("mock") ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  const speed = Number(params.get("mock_speed"));
  return { scenarios, speed: Number.isFinite(speed) && speed > 0 ? speed : undefined, platform };
}
