import { describe, expect, it } from "vitest";
import { FREE_LIMIT, MOCK_VALID_KEY, MockBackend } from "./engine";
import type { EngineEvents, IpcError, JobOptions, JobState } from "../types";
import { optionsFromSettings } from "../../state/store";

const paths = [
  "C:\\Users\\you\\Downloads\\mydata~1729000000000.zip",
  "C:\\Users\\you\\Downloads\\mydata~1729000000000-2.zip",
  "C:\\Users\\you\\Downloads\\mydata~1729000000000-4.zip",
  "C:\\Users\\you\\Downloads\\IMG_2041.jpg",
];

function engine(scenarios: string[] = []) {
  return new MockBackend({ scenarios, latencyScale: 0, autoTick: false, warmupMs: 0, speed: 1000, platform: "windows" });
}

async function setup(m: MockBackend, extra: string[] = []) {
  const inputs = await m.call("add_inputs", { paths: [...paths, ...extra] });
  const ids = inputs.filter((i) => i.status === "ok").map((i) => i.id);
  const settings = await m.call("get_settings", {});
  const options: JobOptions = { ...optionsFromSettings(settings), destination: "D:\\Memories" };
  return { inputs, ids, options };
}

function record(m: MockBackend) {
  const states: JobState[] = [];
  const items: EngineEvents["item"][] = [];
  let progress: EngineEvents["progress"] | null = null;
  void m.on("job_state", (e) => states.push(e.state));
  void m.on("item", (e) => items.push(e));
  void m.on("progress", (p) => (progress = p));
  return { states, items, last: () => progress };
}

function runUntil(m: MockBackend, states: JobState[], until: (s: JobState | undefined) => boolean, max = 5000) {
  for (let i = 0; i < max && !until(states.at(-1)); i++) m.tick();
}

const settled = (s: JobState | undefined) => s !== undefined && s !== "running";

describe("inputs and analysis", () => {
  it("detects roles, ignores a non-export and reports part 3 missing", async () => {
    const m = engine();
    const { inputs, ids } = await setup(m);
    expect(inputs.map((i) => [i.role, i.part_number, i.status])).toEqual([
      ["primary", 1, "ok"],
      ["part", 2, "ok"],
      ["part", 4, "ok"],
      ["ignored", null, "not_export"],
    ]);
    const s = await m.call("analyze", { input_ids: ids });
    expect(s.missing_parts).toEqual([3]);
    expect(s.parts.find((p) => p.part_number === 3)?.status).toBe("missing");
    expect(s.issues.map((i) => i.code)).toContain("PART_MISSING");
    expect(s.counts.entries).toBe(4812);
    expect(s.counts.photos + s.counts.videos).toBe(4812 - 1262);
    expect(s.profile).toBe("mixed");
  });

  it("adding the missing part completes the export", async () => {
    const m = engine();
    const { ids } = await setup(m);
    const more = await m.call("add_inputs", { paths: ["C:\\Users\\you\\Downloads\\mydata~1729000000000-3.zip"] });
    const all = [...ids, more.at(-1)?.id ?? ""];
    const s = await m.call("analyze", { input_ids: all });
    expect(s.missing_parts).toEqual([]);
    expect(s.counts.photos + s.counts.videos).toBe(4812);
  });

  it("blocks a links-only export", async () => {
    const m = engine(["legacy"]);
    const { ids } = await setup(m);
    const s = await m.call("analyze", { input_ids: ids });
    expect(s.profile).toBe("legacy");
    expect(s.blocked).toBe(true);
  });
});

describe("plan (dry run)", () => {
  it("has mixed confidence, two clip groups and totals that add up", async () => {
    const m = engine();
    const { ids, options } = await setup(m);
    const p = await m.call("plan", { input_ids: ids, options });
    expect(p.clip_groups).toHaveLength(2);
    expect(p.confidence.exact).toBeGreaterThan(0);
    expect(p.confidence.ordinal).toBeGreaterThan(0);
    expect(p.confidence.none).toBeGreaterThan(0);
    expect(p.confidence.exact + p.confidence.ordinal + p.confidence.none).toBe(p.totals.items);
    expect(p.location.applied + p.location.none + p.location.uncertain).toBe(p.totals.items);
    expect(p.totals.photos + p.totals.videos).toBe(p.totals.items);
    expect(p.totals.missing_media).toBe(1262);
    expect(p.samples.length).toBeGreaterThan(20);
    expect(p.samples.some((x) => x.match_confidence === "none")).toBe(true);
    expect(p.samples.some((x) => x.location_decision === "uncertain")).toBe(true);
    expect(p.free_limit).toEqual({ applies: true, limit: FREE_LIMIT, items_beyond: p.totals.items - FREE_LIMIT });
  });

  it("estimates space per FR-28 and flags a small drive", async () => {
    const m = engine();
    const { ids, options } = await setup(m);
    const big = await m.call("estimate_space", { input_ids: ids, options });
    expect(big.sufficient).toBe(true);
    expect(big.total_bytes).toBe(big.output_bytes + big.originals_bytes + big.temp_bytes + big.zip_bytes);
    const small = await m.call("estimate_space", { input_ids: ids, options: { ...options, destination: "E:\\" } });
    expect(small.sufficient).toBe(false);
    expect(small.shortfall_bytes).toBeGreaterThan(0);
    const leaner = await m.call("estimate_space", { input_ids: ids, options: { ...options, destination: "E:\\", keep_originals: false, overlay_mode: "burn_in_only" } });
    expect(leaner.sufficient).toBe(true);
  });
});

describe("processing", () => {
  it("stops at the free limit after exactly 100 items, then finishes partial with the rest skipped", async () => {
    const m = engine();
    const { ids, options } = await setup(m);
    const rec = record(m);
    const job = await m.call("start", { input_ids: ids, options });
    runUntil(m, rec.states, settled);
    expect(rec.states.at(-1)).toBe("free_limit_reached");
    const processed = rec.items.filter((e) => e.item.status !== "skipped_free_limit");
    expect(processed).toHaveLength(FREE_LIMIT);
    expect(rec.last()?.items_done).toBe(FREE_LIMIT);

    await m.call("resume", { job_id: job });
    expect(rec.states.at(-1)).toBe("partial");
    const report = await m.call("get_report", { job_id: job });
    expect(report.counts.skipped_free_limit).toBe(3550 - FREE_LIMIT);
    expect(report.counts.missing_media).toBe(1262);
    expect(report.can_unlock).toBe(true);
    expect(report.can_add_part).toBe(true);
  });

  it("after unlocking, resume processes only the skipped items", async () => {
    const m = engine();
    const { ids, options } = await setup(m);
    const rec = record(m);
    const job = await m.call("start", { input_ids: ids, options });
    runUntil(m, rec.states, settled);
    await m.call("resume", { job_id: job }); // Later
    await m.call("activate", { key: MOCK_VALID_KEY });
    const before = rec.items.length;
    await m.call("resume", { job_id: job });
    runUntil(m, rec.states, settled);
    const again = rec.items.slice(before);
    expect(again).toHaveLength(3550 - FREE_LIMIT);
    const report = await m.call("get_report", { job_id: job });
    expect(report.counts.skipped_free_limit).toBe(0);
    expect(report.counts.done + report.counts.metadata_partial + report.counts.location_uncertain + report.counts.failed).toBe(3550);
  });

  it("emits a usable ETA, a few failures, and Retry failed redoes exactly those", async () => {
    const m = engine(["licensed"]);
    const { ids, options } = await setup(m);
    const rec = record(m);
    const job = await m.call("start", { input_ids: ids, options });
    let sawEta = false;
    for (let i = 0; i < 5000 && !settled(rec.states.at(-1)); i++) {
      m.tick();
      const p = rec.last();
      if (p && p.items_done > 200 && p.items_done < 3000 && p.eta_seconds !== null && p.eta_seconds > 0) sawEta = true;
    }
    expect(sawEta).toBe(true);
    expect(rec.states.at(-1)).toBe("partial");
    const report = await m.call("get_report", { job_id: job });
    expect(report.counts.failed).toBe(3);
    expect(report.failures.map((f) => f.error_code)).toHaveLength(3);
    const before = rec.items.length;
    await m.call("retry_failed", { job_id: job, options: null });
    runUntil(m, rec.states, settled);
    const retried = rec.items.slice(before);
    expect(retried.map((e) => e.item.item_id).sort()).toEqual(report.failures.map((f) => f.item_id).sort());
    expect(retried.every((e) => e.item.status !== "failed")).toBe(true);
    expect(rec.states.at(-1)).toBe("done");
  });

  it("add_part leaves the state alone; resume then processes only that part", async () => {
    const m = engine(["licensed"]);
    const { ids, options } = await setup(m);
    const rec = record(m);
    const job = await m.call("start", { input_ids: ids, options });
    runUntil(m, rec.states, settled);
    await m.call("retry_failed", { job_id: job, options: null });
    runUntil(m, rec.states, (s) => s === "done");
    const added = await m.call("add_part", { job_id: job, path: "C:\\Users\\you\\Downloads\\mydata~1729000000000-3.zip" });
    expect(added.part_number).toBe(3);
    expect(rec.states.at(-1)).toBe("done");
    const before = rec.items.length;
    await m.call("resume", { job_id: job });
    runUntil(m, rec.states, (s) => s === "done" || s === "partial");
    const fromPart3 = rec.items.slice(before).filter((e) => e.item.status !== "missing_media");
    expect(fromPart3).toHaveLength(1262);
    const report = await m.call("get_report", { job_id: job });
    expect(report.counts.missing_media).toBe(0);
    expect(report.can_add_part).toBe(false);
  });

  it("pauses after in-flight items and resumes", async () => {
    const m = engine(["licensed"]);
    const { ids, options } = await setup(m);
    const rec = record(m);
    const job = await m.call("start", { input_ids: ids, options });
    m.tick();
    await m.call("pause", { job_id: job });
    await new Promise((r) => setTimeout(r, 5));
    expect(rec.states.at(-1)).toBe("paused");
    const done = rec.last()?.items_done ?? 0;
    m.tick();
    expect(rec.last()?.items_done ?? 0).toBe(done);
    await m.call("resume", { job_id: job });
    expect(rec.states.at(-1)).toBe("running");
  });

  it("refuses to start when the destination is too small", async () => {
    const m = engine();
    const { ids, options } = await setup(m);
    await expect(m.call("start", { input_ids: ids, options: { ...options, destination: "E:\\" } })).rejects.toMatchObject({ code: "DISK_FULL" });
  });
});

describe("licence and settings", () => {
  it("accepts the one mock key format and rejects others", async () => {
    const m = engine();
    const ok = await m.call("activate", { key: ` ${MOCK_VALID_KEY.toLowerCase()} ` });
    expect(ok.tier).toBe("licensed");
    expect(ok.key_masked).toBe("MOCK-****-****-9ABC");
    await expect(m.call("activate", { key: "A1B2C3D4-E5F6A7B8-C9D0E1F2-A3B4C5D6" })).rejects.toMatchObject({ code: "LICENSE_INVALID" } satisfies Partial<IpcError>);
    await expect(m.call("activate", { key: "MOCK-OFFLINE" })).rejects.toMatchObject({ code: "LICENSE_OFFLINE" });
    const off = await m.call("deactivate", {});
    expect(off.tier).toBe("free");
  });

  it("keeps settings in memory and returns copies", async () => {
    const m = engine();
    const s = await m.call("get_settings", {});
    expect(s.update_checks).toBe(true);
    expect(s.apply_uncertain_locations).toBe(false);
    await m.call("set_settings", { settings: { ...s, update_checks: false, language: "en-US" } });
    const again = await m.call("get_settings", {});
    expect(again.update_checks).toBe(false);
    again.update_checks = true;
    expect((await m.call("get_settings", {})).update_checks).toBe(false);
  });

  it("lists a resumable job only in the resume scenario", async () => {
    expect(await engine().call("list_resumable_jobs", {})).toEqual([]);
    const jobs = await engine(["resume"]).call("list_resumable_jobs", {});
    expect(jobs).toHaveLength(1);
    expect(jobs[0].state).toBe("paused");
  });
});
