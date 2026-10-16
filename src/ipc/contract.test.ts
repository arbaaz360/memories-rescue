/**
 * Keeps docs/ipc-contract.md and src/ipc/types.ts in sync: every command and
 * event in the types is in the document's tables, and the error codes are the
 * SRS §8 list in both places.
 */
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { COMMAND_NAMES, ERROR_CODES, EVENT_NAMES, toIpcError, type CommandName, type Commands } from "./types";
import { MockBackend } from "./mock/engine";

const doc = readFileSync(join(__dirname, "..", "..", "docs", "ipc-contract.md"), "utf-8");

describe("IPC contract document", () => {
  it("lists every command in its command table", () => {
    const commands = doc.slice(doc.indexOf("## Commands"), doc.indexOf("## Events"));
    const table = [...commands.matchAll(/^\| `([a-z_]+)` \|/gm)].map((m) => m[1]);
    expect([...table].sort()).toEqual([...COMMAND_NAMES].sort());
  });

  it("lists every event in its event table", () => {
    const section = doc.slice(doc.indexOf("## Events"), doc.indexOf("## Job state machine"));
    const events = [...section.matchAll(/^\| `([a-z_]+)` \|/gm)].map((m) => m[1]);
    expect([...events].sort()).toEqual([...EVENT_NAMES].sort());
  });

  it("has exactly the SRS §8 error codes", () => {
    const section = doc.slice(doc.indexOf("## Error codes"), doc.indexOf("```jsonc"));
    const codes = [...section.matchAll(/`([A-Z_]+)`/g)].map((m) => m[1]);
    expect(codes).toEqual([...ERROR_CODES]);
    expect(ERROR_CODES).toHaveLength(23);
  });

  it("names a command table that matches the Commands type (compile-time) and the mock implements all of them", async () => {
    const m = new MockBackend({ latencyScale: 0, autoTick: false });
    const exercised: CommandName[] = [];
    const calls: { [K in CommandName]?: Commands[K]["args"] } = {
      list_resumable_jobs: {},
      get_settings: {},
      license_status: {},
      app_info: {},
      diagnostics_preview: {},
      check_update: { force: true },
      export_diagnostics: {},
      volume_info: { path: "D:\\" },
      add_inputs: { paths: [] },
      remove_input: { id: "none" },
      open_path: { path: "D:\\" },
    };
    for (const [name, args] of Object.entries(calls) as [CommandName, never][]) {
      await m.call(name, args);
      exercised.push(name);
    }
    await expect(m.call("sidecar_versions", {})).rejects.toMatchObject({ code: "UNKNOWN" });
    expect(exercised.length).toBeGreaterThan(5);
  });
});

describe("toIpcError", () => {
  it("keeps known codes and maps unknown ones to UNKNOWN with the raw code in detail", () => {
    expect(toIpcError({ code: "DISK_FULL", message: "m", detail: null })).toEqual({ code: "DISK_FULL", message: "m", detail: null });
    expect(toIpcError({ code: "ENTRY_REFUSED", message: "m", detail: "x" })).toEqual({ code: "UNKNOWN", message: "m", detail: "ENTRY_REFUSED: x" });
    expect(toIpcError("Command plan not found")).toEqual({ code: "UNKNOWN", message: "Command plan not found", detail: null });
  });
});
