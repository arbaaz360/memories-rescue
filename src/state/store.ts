/**
 * Application state as Preact signals, and every action the screens trigger.
 * The flow (which screen) is the pure reducer in ./flow.ts; this file runs the
 * engine commands around it and turns engine events into state.
 */

import { batch, computed, signal } from "@preact/signals";
import type { Backend, SelectionReason, SidecarProbe, Unlisten } from "../ipc/backend";
import type { Host } from "../ipc/host";
import {
  toIpcError,
  type AppInfo,
  type DiagnosticsPreview,
  type UpdateInfo,
  type ExportSummary,
  type InputFile,
  type IpcError,
  type ItemStatus,
  type JobOptions,
  type JobReport,
  type LicenseStatus,
  type LinkName,
  type PlanPreview,
  type ProgressEvent,
  type ResumableJob,
  type Settings,
  type SpaceEstimate,
  type StatusCounts,
  type VolumeInfo,
  type WarningEvent,
} from "../ipc/types";
import { createI18n, type I18n } from "../i18n/l10n";
import { durationParts, formatDuration, formatPercent, joinPath } from "../format/format";
import { AnnounceThrottle } from "./announce";
import { initialFlow, isTerminal, transition, type FlowEvent, type FlowState, type Page, type UnlockReason } from "./flow";

export interface BootInfo {
  reason: SelectionReason;
  detail: string | null;
}

function emptyCounts(): StatusCounts {
  return { done: 0, metadata_partial: 0, location_uncertain: 0, missing_media: 0, failed: 0, skipped_free_limit: 0 };
}

export function optionsFromSettings(s: Settings): JobOptions {
  return {
    destination: s.destination,
    overlay_mode: s.overlay_mode,
    keep_originals: s.keep_originals,
    clip_rejoin: s.clip_rejoin,
    clip_rejoin_years: s.clip_rejoin_years,
    apply_uncertain_locations: s.apply_uncertain_locations,
    windows_compatibility: s.windows_compatibility,
    zip_output: s.zip_output,
    zip_volume_bytes: s.zip_volume_bytes,
    home_timezone: s.home_timezone,
    metadata_only: false,
    workers: s.workers,
    excluded_clip_groups: [],
  };
}

export class AppStore {
  readonly flow = signal<FlowState>(initialFlow);
  readonly i18n = signal<I18n>(createI18n("system", navigator.languages ?? [navigator.language]));

  readonly inputs = signal<InputFile[]>([]);
  readonly ignoredNotice = signal<string[]>([]);
  readonly inputsBusy = signal(false);
  readonly inputsError = signal<IpcError | null>(null);
  readonly dragging = signal(false);

  readonly summary = signal<ExportSummary | null>(null);
  readonly summaryBusy = signal(false);
  readonly summaryError = signal<IpcError | null>(null);

  readonly settings = signal<Settings | null>(null);
  readonly options = signal<JobOptions | null>(null);
  readonly volume = signal<VolumeInfo | null>(null);
  readonly space = signal<SpaceEstimate | null>(null);
  readonly optionsError = signal<IpcError | null>(null);

  readonly plan = signal<PlanPreview | null>(null);
  readonly planBusy = signal(false);
  readonly planError = signal<IpcError | null>(null);
  readonly previewReportPath = signal<string | null>(null);

  readonly jobInputIds = signal<string[]>([]);
  readonly jobOptions = signal<JobOptions | null>(null);
  readonly progress = signal<ProgressEvent | null>(null);
  readonly tally = signal<StatusCounts>(emptyCounts());
  readonly jobError = signal<IpcError | null>(null);
  readonly jobBusy = signal(false);
  readonly report = signal<JobReport | null>(null);
  readonly outputPath = signal<string | null>(null);
  readonly diagnosticsPath = signal<string | null>(null);
  /** What Export diagnostics would save; shown in a dialog before saving (FR-41). */
  readonly diagnosticsPreview = signal<DiagnosticsPreview | null>(null);
  readonly diagnosticsBusy = signal(false);
  /** An update the feed offers (§10.2); null when none or dismissed. */
  readonly update = signal<UpdateInfo | null>(null);
  readonly updateBusy = signal(false);
  readonly updateError = signal<IpcError | null>(null);
  /** Result of the last "Check now" in Settings: "none" when up to date. */
  readonly updateCheck = signal<"idle" | "checking" | "none" | "found">("idle");
  /** FR-51 notice dismissed for this session. */
  readonly hevcDismissed = signal(false);
  readonly startBusy = signal(false);

  readonly license = signal<LicenseStatus | null>(null);
  readonly unlockBusy = signal(false);
  readonly unlockError = signal<IpcError | null>(null);

  readonly resumable = signal<ResumableJob[]>([]);
  readonly appInfo = signal<AppInfo | null>(null);
  readonly sidecars = signal<SidecarProbe | null>(null);
  readonly notices = signal<WarningEvent[]>([]);
  readonly announcement = signal<{ text: string; n: number }>({ text: "", n: 0 });

  readonly usableInputs = computed(() =>
    this.inputs.value.filter((i) => (i.status === "ok" || i.status === "truncated") && i.role !== "ignored"),
  );

  private readonly throttle = new AnnounceThrottle();
  private unlisteners: Unlisten[] = [];
  /** Latest job_state per job, including events that arrive before `start` has returned the id. */
  private readonly lastStates = new Map<string, { state: NonNullable<FlowState["jobState"]>; error: IpcError | null }>();
  private estimateTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    readonly backend: Backend,
    readonly host: Host,
    readonly boot: BootInfo,
    private readonly now: () => number = () => Date.now(),
  ) {}

  get t() {
    return this.i18n.value.t;
  }

  // ---------------------------------------------------------------------------
  // Startup
  // ---------------------------------------------------------------------------

  async init(): Promise<void> {
    this.unlisteners.push(
      await this.backend.on("progress", (p) => this.onProgress(p)),
      await this.backend.on("item", (e) => {
        if (e.job_id !== this.flow.value.jobId) return;
        const next = { ...this.tally.value };
        next[e.item.status as ItemStatus] += 1;
        this.tally.value = next;
      }),
      await this.backend.on("job_state", (e) => {
        this.lastStates.set(e.job_id, { state: e.state, error: e.error });
        void this.onJobState(e.job_id, e.state, e.error);
      }),
      await this.backend.on("warning", (w) => {
        this.notices.value = [...this.notices.value.filter((n) => n.code !== w.code), w];
      }),
      await this.backend.on("update_available", (u) => {
        this.update.value = u;
      }),
    );
    const [settings, license, resumable, appInfo] = await Promise.all([
      this.backend.call("get_settings", {}),
      this.backend.call("license_status", {}),
      this.backend.call("list_resumable_jobs", {}),
      this.backend.call("app_info", {}),
    ]);
    batch(() => {
      this.settings.value = settings;
      this.license.value = license;
      this.resumable.value = resumable;
      this.appInfo.value = appInfo;
    });
    this.applyLanguage(settings.language);
    // §10.2: check at launch unless turned off (the engine repeats it every 24 h).
    if (settings.update_checks) void this.checkForUpdate(false);
  }

  dispose(): void {
    for (const u of this.unlisteners) u();
    this.unlisteners = [];
  }

  applyLanguage(setting: string): void {
    this.i18n.value = createI18n(setting, navigator.languages ?? [navigator.language]);
    if (typeof document === "undefined") return;
    document.documentElement.lang = this.i18n.value.language;
    document.documentElement.dir = this.i18n.value.dir;
  }

  dispatch(event: FlowEvent): void {
    this.flow.value = transition(this.flow.value, event);
  }

  announce(text: string): void {
    this.announcement.value = { text, n: this.announcement.value.n + 1 };
  }

  // ---------------------------------------------------------------------------
  // S0 Welcome
  // ---------------------------------------------------------------------------

  getStarted(): void {
    this.dispatch({ type: "get_started" });
  }

  async resumePrevious(job: ResumableJob): Promise<void> {
    this.jobBusy.value = true;
    try {
      batch(() => {
        this.tally.value = emptyCounts();
        this.progress.value = null;
        this.report.value = null;
        this.outputPath.value = job.output_path;
      });
      this.dispatch({ type: "resume_previous", jobId: job.job_id });
      await this.backend.call("resume", { job_id: job.job_id });
      const report = await this.backend.call("get_report", { job_id: job.job_id });
      batch(() => {
        this.report.value = report;
        this.jobOptions.value = report.options;
        this.outputPath.value = report.output_path;
      });
    } catch (e) {
      this.jobError.value = toIpcError(e);
    } finally {
      this.jobBusy.value = false;
    }
  }

  // ---------------------------------------------------------------------------
  // S1 Add export files
  // ---------------------------------------------------------------------------

  async addPaths(paths: string[]): Promise<void> {
    if (paths.length === 0) return;
    this.inputsBusy.value = true;
    this.inputsError.value = null;
    try {
      const before = new Set(this.inputs.value.map((i) => i.id));
      const list = await this.backend.call("add_inputs", { paths });
      const added = list.filter((i) => !before.has(i.id));
      const ignored = added.filter((i) => i.status === "not_export").map((i) => i.name);
      batch(() => {
        this.inputs.value = list;
        this.ignoredNotice.value = ignored;
        // The export changed: the old analysis and plan no longer apply.
        this.summary.value = null;
        this.plan.value = null;
      });
      const t = this.t;
      this.announce(
        ignored.length > 0
          ? t("inputs-added-with-ignored", { count: added.length - ignored.length, ignored: ignored.length })
          : t("inputs-added", { count: added.length }),
      );
    } catch (e) {
      this.inputsError.value = toIpcError(e);
    } finally {
      this.inputsBusy.value = false;
    }
  }

  async addFiles(): Promise<void> {
    const t = this.t;
    const paths = await this.host.pickExportFiles({ title: t("picker-files-title"), filterName: t("picker-files-filter") });
    await this.addPaths(paths);
  }

  async addFolders(): Promise<void> {
    const paths = await this.host.pickExportFolders(this.t("picker-folder-title"));
    await this.addPaths(paths);
  }

  async removeInput(input: InputFile): Promise<void> {
    try {
      await this.backend.call("remove_input", { id: input.id });
      batch(() => {
        this.inputs.value = this.inputs.value.filter((i) => i.id !== input.id);
        this.ignoredNotice.value = this.ignoredNotice.value.filter((n) => n !== input.name);
        this.summary.value = null;
        this.plan.value = null;
      });
      this.announce(this.t("inputs-removed", { name: input.name }));
    } catch (e) {
      this.inputsError.value = toIpcError(e);
    }
  }

  continueFromInputs(): void {
    this.dispatch({ type: "inputs_continue", usable: this.usableInputs.value.length });
    if (this.flow.value.screen === "summary") void this.analyze(this.usableInputs.value.map((i) => i.id));
  }

  // ---------------------------------------------------------------------------
  // S2 Check & summary
  // ---------------------------------------------------------------------------

  async analyze(inputIds: string[]): Promise<void> {
    this.summaryBusy.value = true;
    this.summaryError.value = null;
    try {
      this.summary.value = await this.backend.call("analyze", { input_ids: inputIds });
    } catch (e) {
      this.summaryError.value = toIpcError(e);
    } finally {
      this.summaryBusy.value = false;
    }
  }

  /** S2 "Add missing part": a new input in the normal flow, or `add_part` on the job from S6. */
  async addMissingPart(): Promise<void> {
    const t = this.t;
    const paths = await this.host.pickExportFiles({ title: t("picker-part-title"), filterName: t("picker-files-filter") });
    if (paths.length === 0) return;
    if (this.flow.value.summaryMode === "add_part" && this.flow.value.jobId) {
      await this.addPartToJob(paths);
      return;
    }
    await this.addPaths(paths);
    await this.analyze(this.usableInputs.value.map((i) => i.id));
  }

  private async addPartToJob(paths: string[]): Promise<void> {
    const jobId = this.flow.value.jobId;
    if (!jobId) return;
    this.summaryBusy.value = true;
    try {
      const ids = [...this.jobInputIds.value];
      for (const path of paths) {
        const input = await this.backend.call("add_part", { job_id: jobId, path });
        if (!ids.includes(input.id)) ids.push(input.id);
        this.inputs.value = [...this.inputs.value.filter((i) => i.id !== input.id), input];
      }
      this.jobInputIds.value = ids;
      this.summaryBusy.value = false;
      await this.analyze(ids);
    } catch (e) {
      this.summaryError.value = toIpcError(e);
      this.summaryBusy.value = false;
    }
  }

  async continueFromSummary(): Promise<void> {
    const summary = this.summary.value;
    if (!summary) return;
    if (this.flow.value.summaryMode === "add_part") {
      const jobId = this.flow.value.jobId;
      if (!jobId) return;
      this.resetJobView();
      this.dispatch({ type: "summary_continue", blocked: summary.blocked });
      await this.resumeJob(jobId);
      return;
    }
    this.dispatch({ type: "summary_continue", blocked: summary.blocked });
    if (this.flow.value.screen === "options") this.enterOptions();
  }

  back(): void {
    this.dispatch({ type: "back" });
  }

  // ---------------------------------------------------------------------------
  // S3 Options
  // ---------------------------------------------------------------------------

  private enterOptions(): void {
    if (!this.options.value && this.settings.value) this.options.value = optionsFromSettings(this.settings.value);
    void this.refreshVolume();
    this.scheduleEstimate(0);
  }

  setOptions(patch: Partial<JobOptions>): void {
    const current = this.options.value;
    if (!current) return;
    this.options.value = { ...current, ...patch };
    this.plan.value = null;
    if (patch.destination !== undefined) void this.refreshVolume();
    this.scheduleEstimate(150);
  }

  async chooseDestination(): Promise<void> {
    const current = this.options.value?.destination ?? null;
    const picked = await this.host.pickDestination(this.t("picker-destination-title"), current);
    if (picked) this.setOptions({ destination: picked });
  }

  private async refreshVolume(): Promise<void> {
    const dest = this.options.value?.destination;
    if (!dest) {
      this.volume.value = null;
      return;
    }
    try {
      this.volume.value = await this.backend.call("volume_info", { path: dest });
    } catch (e) {
      this.optionsError.value = toIpcError(e);
    }
  }

  private scheduleEstimate(ms: number): void {
    if (this.estimateTimer !== null) clearTimeout(this.estimateTimer);
    this.estimateTimer = setTimeout(() => void this.estimate(), ms);
  }

  private async estimate(): Promise<void> {
    const options = this.options.value;
    if (!options) return;
    try {
      this.space.value = await this.backend.call("estimate_space", {
        input_ids: this.usableInputs.value.map((i) => i.id),
        options,
      });
      this.optionsError.value = null;
    } catch (e) {
      this.optionsError.value = toIpcError(e);
    }
  }

  readonly canStart = computed(() => {
    const o = this.options.value;
    const s = this.space.value;
    return !!o && o.destination.length > 0 && !!s && s.sufficient && !this.summary.value?.blocked;
  });

  // ---------------------------------------------------------------------------
  // S4 Preview
  // ---------------------------------------------------------------------------

  async preview(): Promise<void> {
    this.dispatch({ type: "preview" });
    const options = this.options.value;
    if (!options) return;
    this.planBusy.value = true;
    this.planError.value = null;
    this.previewReportPath.value = null;
    try {
      this.plan.value = await this.backend.call("plan", { input_ids: this.usableInputs.value.map((i) => i.id), options });
    } catch (e) {
      this.planError.value = toIpcError(e);
    } finally {
      this.planBusy.value = false;
    }
  }

  toggleClipGroup(groupId: string, included: boolean): void {
    const o = this.options.value;
    if (!o) return;
    const excluded = new Set(o.excluded_clip_groups);
    if (included) excluded.delete(groupId);
    else excluded.add(groupId);
    // Not setOptions: unticking a group does not invalidate the preview.
    this.options.value = { ...o, excluded_clip_groups: [...excluded] };
  }

  async exportPreviewReport(): Promise<void> {
    const plan = this.plan.value;
    if (!plan) return;
    try {
      this.previewReportPath.value = await this.backend.call("export_preview_report", { plan_id: plan.plan_id });
      this.announce(this.t("preview-report-saved"));
    } catch (e) {
      this.planError.value = toIpcError(e);
    }
  }

  // ---------------------------------------------------------------------------
  // S5 Processing
  // ---------------------------------------------------------------------------

  private resetJobView(): void {
    batch(() => {
      this.progress.value = null;
      this.tally.value = emptyCounts();
      this.jobError.value = null;
      this.report.value = null;
      this.diagnosticsPath.value = null;
    });
    this.throttle.reset();
  }

  async start(): Promise<void> {
    const options = this.options.value;
    if (!options || !this.canStart.value) return;
    this.startBusy.value = true;
    this.optionsError.value = null;
    try {
      const inputIds = this.usableInputs.value.map((i) => i.id);
      this.resetJobView();
      const jobId = await this.backend.call("start", { input_ids: inputIds, options });
      batch(() => {
        this.jobInputIds.value = inputIds;
        this.jobOptions.value = options;
        this.outputPath.value = joinPath(options.destination, "Memories Rescue");
      });
      this.dispatch({ type: "job_started", jobId });
      this.replayState(jobId);
      if (this.settings.value && this.settings.value.destination !== options.destination) {
        this.settings.value = { ...this.settings.value, destination: options.destination };
      }
      const report = await this.backend.call("get_report", { job_id: jobId });
      this.outputPath.value = report.output_path;
    } catch (e) {
      const err = toIpcError(e);
      if (this.flow.value.screen === "preview") this.planError.value = err;
      else this.optionsError.value = err;
    } finally {
      this.startBusy.value = false;
    }
  }

  private onProgress(p: ProgressEvent): void {
    if (p.job_id !== this.flow.value.jobId) return;
    this.progress.value = p;
    const flow = this.flow.value;
    if (flow.screen !== "processing" || flow.jobState !== "running") return;
    if (!this.throttle.allow(this.now())) return;
    const i18n = this.i18n.value;
    const eta = p.eta_seconds;
    const etaKind = eta === null ? "unknown" : durationParts(eta).kind === "under_minute" ? "soon" : "known";
    this.announce(
      i18n.t("processing-announce", {
        pct: formatPercent(p.overall_pct, i18n.formatLocale),
        done: p.items_done,
        total: p.items_total,
        etaKind,
        eta: eta === null ? "" : (formatDuration(eta, i18n.formatLocale) ?? ""),
      }),
    );
  }

  /**
   * The engine runs the job on its own thread and may report a state (even
   * `done`, for a tiny job) before `start` returns the id; apply it now.
   */
  private replayState(jobId: string): void {
    const last = this.lastStates.get(jobId);
    if (last && last.state !== "running") void this.onJobState(jobId, last.state, last.error);
  }

  private async onJobState(jobId: string, state: FlowState["jobState"], error: IpcError | null): Promise<void> {
    if (!state) return;
    const wasScreen = this.flow.value.screen;
    this.dispatch({ type: "job_state", jobId, state });
    if (jobId !== this.flow.value.jobId) return;
    const t = this.t;
    if (state === "paused") {
      this.jobError.value = error;
      this.announce(error ? t("processing-announce-paused-error") : t("processing-announce-paused"));
    } else if (state === "running") {
      this.jobError.value = null;
      if (wasScreen === "processing") this.announce(t("processing-announce-running"));
    } else if (state === "free_limit_reached") {
      this.announce(t("processing-announce-free-limit", { limit: this.license.value?.free_limit ?? 100 }));
    } else if (isTerminal(state)) {
      const report = await this.loadReport();
      const counts = report?.counts;
      const saved = counts ? counts.done + counts.metadata_partial + counts.location_uncertain : 0;
      const problems = counts ? counts.failed + counts.missing_media + counts.skipped_free_limit : 0;
      this.announce(t(`results-announce-${state}`, { saved, problems }));
      if (state !== "cancelled") {
        void this.host.notify(t("notify-title", { state }), t("notify-body", { state, saved, problems }));
      }
      this.resumable.value = await this.backend.call("list_resumable_jobs", {}).catch(() => this.resumable.value);
    }
  }

  async pause(): Promise<void> {
    await this.jobCommand("pause");
  }

  async resume(): Promise<void> {
    await this.jobCommand("resume");
  }

  async cancel(): Promise<void> {
    await this.jobCommand("cancel");
  }

  private async jobCommand(command: "pause" | "resume" | "cancel"): Promise<void> {
    const jobId = this.flow.value.jobId;
    if (!jobId) return;
    this.jobBusy.value = true;
    try {
      await this.backend.call(command, { job_id: jobId });
    } catch (e) {
      this.jobError.value = toIpcError(e);
    } finally {
      this.jobBusy.value = false;
    }
  }

  private async resumeJob(jobId: string): Promise<void> {
    try {
      await this.backend.call("resume", { job_id: jobId });
      this.dispatch({ type: "job_resumed" });
    } catch (e) {
      this.jobError.value = toIpcError(e);
    }
  }

  async openOutput(): Promise<void> {
    const path = this.report.value?.output_path ?? this.outputPath.value;
    if (path) await this.openPath(path);
  }

  /** Opens one of the app's own outside addresses, by name (`open_link`). */
  async openLink(link: LinkName): Promise<void> {
    try {
      await this.backend.call("open_link", { link });
    } catch (e) {
      this.jobError.value = toIpcError(e);
    }
  }

  async openPath(path: string): Promise<void> {
    try {
      await this.backend.call("open_path", { path });
    } catch (e) {
      this.jobError.value = toIpcError(e);
    }
  }

  // ---------------------------------------------------------------------------
  // S6 Results
  // ---------------------------------------------------------------------------

  async loadReport(): Promise<JobReport | null> {
    const jobId = this.flow.value.jobId;
    if (!jobId) return null;
    try {
      const report = await this.backend.call("get_report", { job_id: jobId });
      this.report.value = report;
      return report;
    } catch (e) {
      this.jobError.value = toIpcError(e);
      return null;
    }
  }

  async retryFailed(): Promise<void> {
    const jobId = this.flow.value.jobId;
    if (!jobId) return;
    this.jobBusy.value = true;
    try {
      this.resetJobView();
      await this.backend.call("retry_failed", { job_id: jobId, options: null });
      this.dispatch({ type: "retry_started" });
    } catch (e) {
      this.jobError.value = toIpcError(e);
    } finally {
      this.jobBusy.value = false;
    }
  }

  /** S6 "Add missing part": back to S2 for this job (§7.1), with the picked part attached. */
  async addMissingPartFromResults(): Promise<void> {
    const t = this.t;
    const paths = await this.host.pickExportFiles({ title: t("picker-part-title"), filterName: t("picker-files-filter") });
    if (paths.length === 0) return;
    this.dispatch({ type: "add_part_to_job" });
    await this.addPartToJob(paths);
  }

  /** First step of Export diagnostics: list what the ZIP will hold (NFR-PRV-06). */
  async previewDiagnostics(): Promise<void> {
    this.diagnosticsBusy.value = true;
    try {
      this.diagnosticsPreview.value = await this.backend.call("diagnostics_preview", {});
    } catch (e) {
      this.jobError.value = toIpcError(e);
    } finally {
      this.diagnosticsBusy.value = false;
    }
  }

  closeDiagnostics(): void {
    this.diagnosticsPreview.value = null;
  }

  async exportDiagnostics(): Promise<void> {
    this.diagnosticsBusy.value = true;
    try {
      this.diagnosticsPath.value = await this.backend.call("export_diagnostics", {});
      this.diagnosticsPreview.value = null;
      this.announce(this.t("results-diagnostics-saved"));
    } catch (e) {
      this.jobError.value = toIpcError(e);
    } finally {
      this.diagnosticsBusy.value = false;
    }
  }

  async newJob(): Promise<void> {
    for (const input of this.inputs.value) {
      await this.backend.call("remove_input", { id: input.id }).catch(() => null);
    }
    batch(() => {
      this.inputs.value = [];
      this.ignoredNotice.value = [];
      this.summary.value = null;
      this.plan.value = null;
      this.options.value = null;
      this.space.value = null;
      this.jobInputIds.value = [];
      this.jobOptions.value = null;
      this.outputPath.value = null;
      this.notices.value = [];
    });
    this.resetJobView();
    this.dispatch({ type: "new_job" });
  }

  // ---------------------------------------------------------------------------
  // Unlock dialog
  // ---------------------------------------------------------------------------

  openUnlock(reason: UnlockReason): void {
    this.unlockError.value = null;
    this.dispatch({ type: "open_unlock", reason });
  }

  async buy(): Promise<void> {
    await this.openLink("purchase");
  }

  async activate(key: string): Promise<boolean> {
    this.unlockBusy.value = true;
    this.unlockError.value = null;
    try {
      this.license.value = await this.backend.call("activate", { key });
      this.announce(this.t("unlock-activated"));
      const { unlock, jobId, jobState } = this.flow.value;
      this.dispatch({ type: "close_unlock" });
      if (jobId && (unlock === "free_limit" || unlock === "results" || jobState === "free_limit_reached")) {
        if (unlock === "results") this.resetJobView();
        await this.resumeJob(jobId);
      }
      return true;
    } catch (e) {
      this.unlockError.value = toIpcError(e);
      return false;
    } finally {
      this.unlockBusy.value = false;
    }
  }

  /** "Later": close the dialog; at the free limit the engine then finishes with the rest skipped. */
  async later(): Promise<void> {
    const { unlock, jobId } = this.flow.value;
    this.dispatch({ type: "close_unlock" });
    if (unlock === "free_limit" && jobId) {
      try {
        await this.backend.call("resume", { job_id: jobId });
      } catch (e) {
        this.jobError.value = toIpcError(e);
      }
    }
  }

  async deactivate(): Promise<void> {
    try {
      this.license.value = await this.backend.call("deactivate", {});
    } catch (e) {
      this.unlockError.value = toIpcError(e);
    }
  }

  // ---------------------------------------------------------------------------
  // Settings and About
  // ---------------------------------------------------------------------------

  openPage(page: Page): void {
    this.dispatch({ type: "open_page", page });
  }

  closePage(): void {
    this.dispatch({ type: "close_page" });
  }

  async saveSettings(next: Settings): Promise<void> {
    const saved = await this.backend.call("set_settings", { settings: next });
    const languageChanged = saved.language !== this.settings.value?.language;
    this.settings.value = saved;
    if (languageChanged) this.applyLanguage(saved.language);
    // Options not yet used for a job follow the new defaults.
    if (this.options.value && this.flow.value.screen !== "processing" && this.flow.value.screen !== "results") {
      this.options.value = { ...optionsFromSettings(saved), destination: this.options.value.destination };
      this.scheduleEstimate(0);
    }
    this.announce(this.t("settings-saved"));
  }

  // ---------------------------------------------------------------------------
  // Updates (§10.2)
  // ---------------------------------------------------------------------------

  /** `force` is "Check now" in Settings; otherwise the engine honours the update-checks setting. */
  async checkForUpdate(force: boolean): Promise<void> {
    if (force) this.updateCheck.value = "checking";
    try {
      const found = await this.backend.call("check_update", { force });
      this.update.value = found;
      if (force) this.updateCheck.value = found ? "found" : "none";
    } catch (e) {
      // A failed check changes nothing; it is shown only when the user asked.
      if (force) {
        this.updateCheck.value = "idle";
        this.updateError.value = toIpcError(e);
      }
    }
  }

  /** True while a job runs: updates never install then (§10.2). */
  readonly jobRunning = computed(() => this.flow.value.jobState === "running");

  async installUpdate(): Promise<void> {
    if (this.jobRunning.value) return;
    this.updateBusy.value = true;
    this.updateError.value = null;
    try {
      await this.backend.call("install_update", {});
    } catch (e) {
      this.updateError.value = toIpcError(e);
    } finally {
      this.updateBusy.value = false;
    }
  }

  dismissUpdate(): void {
    this.update.value = null;
  }

  dismissNotice(code: string): void {
    this.notices.value = this.notices.value.filter((n) => n.code !== code);
  }
}
