/**
 * One JSON sample of every contract shape, produced by the mock engine, in
 * `src-tauri/tests/ipc-samples/`. The Rust test `tests/ipc_samples.rs`
 * round-trips each through the Rust types, so a field that exists on one
 * side only fails a test on one side or the other.
 *
 * `WRITE_IPC_SAMPLES=1 npm test` rewrites the files; otherwise this test
 * checks that the committed samples still have exactly the mock's keys.
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { MOCK_VALID_KEY, MockBackend } from "./mock/engine";
import type { EngineEvents, IpcError, WarningEvent } from "./types";
import { optionsFromSettings } from "../state/store";

const DIR = join(__dirname, "..", "..", "src-tauri", "tests", "ipc-samples");

async function collect(): Promise<Record<string, unknown>> {
  const m = new MockBackend({ latencyScale: 0, autoTick: false, warmupMs: 0, speed: 1000, platform: "windows", now: () => Date.UTC(2026, 9, 16, 12) });
  const events: Partial<{ [K in keyof EngineEvents]: EngineEvents[K] }> = {};
  for (const name of ["progress", "item", "job_state"] as const) {
    void m.on(name, (p) => {
      (events as Record<string, unknown>)[name] ??= p;
    });
  }
  const inputs = await m.call("add_inputs", {
    paths: [
      "C:\\Users\\you\\Downloads\\mydata~1729000000000.zip",
      "C:\\Users\\you\\Downloads\\mydata~1729000000000-2.zip",
      "C:\\Users\\you\\Downloads\\mydata~1729000000000-4.zip",
      "C:\\Users\\you\\Downloads\\IMG_2041.jpg",
    ],
  });
  const ids = inputs.filter((i) => i.status === "ok").map((i) => i.id);
  const settings = { ...(await m.call("get_settings", {})), home_timezone: "Europe/London" };
  const options = { ...optionsFromSettings(settings), destination: "D:\\Memories" };
  const summary = await m.call("analyze", { input_ids: ids });
  const plan = await m.call("plan", { input_ids: ids, options });
  const space = await m.call("estimate_space", { input_ids: ids, options });
  const volume = await m.call("volume_info", { path: "D:\\Memories" });
  await m.call("activate", { key: MOCK_VALID_KEY });
  const job = await m.call("start", { input_ids: ids, options });
  for (let i = 0; i < 400; i++) m.tick();
  const report = await m.call("get_report", { job_id: job });
  const resumable = await new MockBackend({ scenarios: ["resume"], latencyScale: 0, autoTick: false, now: () => Date.UTC(2026, 9, 16, 12) }).call("list_resumable_jobs", {});
  const warning: WarningEvent = { job_id: job, code: "LOCATION_UNCERTAIN", message: "Location not applied for 36 items.", count: 36 };
  const error: IpcError = { code: "DISK_FULL", message: "Not enough space on E:.", detail: "1.3 GB short" };
  return {
    input_files: inputs,
    export_summary: summary,
    plan_preview: plan,
    space_estimate: space,
    volume_info: volume,
    job_report: report,
    resumable_jobs: resumable,
    settings,
    diagnostics_preview: await m.call("diagnostics_preview", {}),
    update_info: await new MockBackend({ scenarios: ["update"], latencyScale: 0, autoTick: false }).call("check_update", { force: true }),
    license_status: await m.call("license_status", {}),
    app_info: await m.call("app_info", {}),
    progress_event: events.progress,
    item_event: events.item,
    job_state_event: events.job_state,
    warning_event: warning,
    ipc_error: error,
  };
}

/** The set of key paths in a JSON value; arrays contribute their elements' keys. */
function keyPaths(v: unknown, prefix = ""): Set<string> {
  const out = new Set<string>();
  if (Array.isArray(v)) {
    for (const x of v) for (const k of keyPaths(x, `${prefix}[]`)) out.add(k);
  } else if (v && typeof v === "object") {
    for (const [k, x] of Object.entries(v)) {
      out.add(`${prefix}.${k}`);
      for (const p of keyPaths(x, `${prefix}.${k}`)) out.add(p);
    }
  }
  return out;
}

describe("IPC samples for the Rust round-trip test", () => {
  it("are present and match the mock's shapes", async () => {
    const samples = await collect();
    if (process.env.WRITE_IPC_SAMPLES === "1") {
      mkdirSync(DIR, { recursive: true });
      for (const [name, value] of Object.entries(samples)) {
        writeFileSync(join(DIR, `${name}.json`), `${JSON.stringify(value, null, 2)}\n`);
      }
    }
    for (const [name, value] of Object.entries(samples)) {
      expect(value, name).toBeDefined();
      const file = join(DIR, `${name}.json`);
      expect(existsSync(file), `${file} (run WRITE_IPC_SAMPLES=1 npm test)`).toBe(true);
      const committed = JSON.parse(readFileSync(file, "utf-8")) as unknown;
      expect([...keyPaths(committed)].sort(), name).toEqual([...keyPaths(value)].sort());
    }
  });
});
