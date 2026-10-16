import { describe, expect, it } from "vitest";
import type { Backend } from "../ipc/backend";
import { BrowserHost } from "../ipc/host";
import { MockBackend } from "../ipc/mock/engine";
import type { CommandName, Commands, EngineEvents } from "../ipc/types";
import { AppStore } from "./store";

/**
 * The real engine runs a job on its own thread, so a small job can report
 * `done` before `start` has returned its id to the window.
 */
class EarlyFinish implements Backend {
  readonly kind = "tauri" as const;
  private readonly inner = new MockBackend({ latencyScale: 0, autoTick: false, platform: "windows" });
  private handlers: { [K in keyof EngineEvents]?: ((p: EngineEvents[K]) => void)[] } = {};

  async call<K extends CommandName>(command: K, args: Commands[K]["args"]): Promise<Commands[K]["result"]> {
    if (command === "start") {
      for (const h of this.handlers.job_state ?? []) h({ job_id: "job-fast", state: "running", error: null, at: "" });
      for (const h of this.handlers.job_state ?? []) h({ job_id: "job-fast", state: "done", error: null, at: "" });
      return "job-fast" as Commands[K]["result"];
    }
    if (command === "get_report") {
      const report: Commands["get_report"]["result"] = {
        job_id: "job-fast",
        state: "done",
        output_path: "D:\Memories\Memories Rescue",
        report_html_path: null,
        report_csv_path: null,
        started_at: "",
        finished_at: null,
        items_total: 1,
        bytes_done: 1,
        counts: { done: 1, metadata_partial: 0, location_uncertain: 0, missing_media: 0, failed: 0, skipped_free_limit: 0 },
        missing_parts: [],
        failures: [],
        issues: [],
        can_retry: false,
        can_unlock: false,
        can_add_part: false,
        options: (await this.inner.call("get_settings", {})) as never,
        clips: [],
      };
      return report as Commands[K]["result"];
    }
    return this.inner.call(command, args);
  }

  async on<E extends keyof EngineEvents>(event: E, handler: (payload: EngineEvents[E]) => void) {
    (this.handlers[event] ??= [] as never[]).push(handler as never);
    return () => undefined;
  }
}

/** The browser stand-in without its notification (no window in Node). */
class QuietHost extends BrowserHost {
  notified: string[] = [];
  override async notify(title: string): Promise<void> {
    this.notified.push(title);
  }
}

describe("store and an engine that is faster than the window", () => {
  it("still lands on Results when done arrives before start returns", async () => {
    const backend = new EarlyFinish();
    const host = new QuietHost("windows");
    const store = new AppStore(backend, host, { reason: "engine", detail: null });
    await store.init();
    store.getStarted();
    await store.addFiles();
    store.continueFromInputs();
    for (let i = 0; i < 50 && !store.summary.value; i++) await new Promise((r) => setTimeout(r, 0));
    await store.continueFromSummary();
    for (let i = 0; i < 50 && !store.space.value; i++) await new Promise((r) => setTimeout(r, 10));
    expect(store.flow.value.screen).toBe("options");
    await store.start();
    for (let i = 0; i < 50 && store.flow.value.screen !== "results"; i++) await new Promise((r) => setTimeout(r, 0));
    expect(store.flow.value.jobId).toBe("job-fast");
    expect(store.flow.value.jobState).toBe("done");
    expect(store.flow.value.screen).toBe("results");
    expect(host.notified).toEqual(["Your memories are ready"]);
  });
});
