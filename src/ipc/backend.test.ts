import { describe, expect, it } from "vitest";
import { isCommandMissing, probeSidecars, selectBackend, type Backend } from "./backend";
import { camelArgs } from "./tauri";
import type { CommandName } from "./types";

function fakeTauri(answers: Partial<Record<CommandName, () => Promise<unknown>>>): Backend {
  return {
    kind: "tauri",
    call: ((command: CommandName) => {
      const a = answers[command];
      return a ? a() : Promise.reject(`Command ${command} not found`);
    }) as Backend["call"],
    on: async () => () => undefined,
  };
}

const mock: Backend = { kind: "mock", call: (() => Promise.reject(new Error("unused"))) as Backend["call"], on: async () => () => undefined };

describe("selectBackend", () => {
  it("uses the mock in a plain browser without touching Tauri", async () => {
    let created = false;
    const s = await selectBackend({
      isTauri: () => false,
      createTauri: () => {
        created = true;
        return fakeTauri({});
      },
      createMock: () => mock,
    });
    expect(s.backend?.kind).toBe("mock");
    expect(s.reason).toBe("no_tauri");
    expect(created).toBe(false);
  });

  it("uses Tauri when the engine answers app_info", async () => {
    const s = await selectBackend({
      isTauri: () => true,
      createTauri: () => fakeTauri({ app_info: async () => ({ version: "1" }) }),
      createMock: () => mock,
    });
    expect(s.backend?.kind).toBe("tauri");
    expect(s.reason).toBe("engine");
  });

  it("falls back to the mock inside Tauri while the engine command does not exist", async () => {
    const s = await selectBackend({
      isTauri: () => true,
      createTauri: () => fakeTauri({ sidecar_versions: async () => ({ ffmpeg: "x", exiftool: "y" }) }),
      createMock: () => mock,
    });
    expect(s.backend?.kind).toBe("mock");
    expect(s.reason).toBe("engine_missing");
    expect(s.detail).toContain("app_info not found");
  });

  it("falls back to the mock when the engine errors or hangs", async () => {
    const erroring = await selectBackend({
      isTauri: () => true,
      createTauri: () => fakeTauri({ app_info: () => Promise.reject({ code: "UNKNOWN", message: "boom", detail: null }) }),
      createMock: () => mock,
    });
    expect(erroring.reason).toBe("engine_error");
    const hanging = await selectBackend({
      isTauri: () => true,
      createTauri: () => fakeTauri({ app_info: () => new Promise(() => undefined) }),
      createMock: () => mock,
      timeoutMs: 20,
    });
    expect(hanging.backend?.kind).toBe("mock");
    expect(hanging.detail).toContain("timed out");
  });
});

describe("release builds never fall back to the mock", () => {
  it("gives no backend outside Tauri", async () => {
    const s = await selectBackend({ isTauri: () => false, createTauri: () => fakeTauri({}), createMock: null });
    expect(s.backend).toBeNull();
    expect(s.reason).toBe("no_tauri");
  });

  it("gives no backend when the engine does not answer, so the window shows the error screen", async () => {
    const missing = await selectBackend({ isTauri: () => true, createTauri: () => fakeTauri({}), createMock: null });
    expect(missing.backend).toBeNull();
    expect(missing.reason).toBe("engine_missing");
    const hanging = await selectBackend({
      isTauri: () => true,
      createTauri: () => fakeTauri({ app_info: () => new Promise(() => undefined) }),
      createMock: null,
      timeoutMs: 20,
    });
    expect(hanging.backend).toBeNull();
    expect(hanging.reason).toBe("engine_error");
  });

  it("still uses the engine when it answers", async () => {
    const s = await selectBackend({ isTauri: () => true, createTauri: () => fakeTauri({ app_info: async () => ({}) }), createMock: null });
    expect(s.backend?.kind).toBe("tauri");
  });

  it("loads the mock lazily in development", async () => {
    let loaded = 0;
    const s = await selectBackend({
      isTauri: () => false,
      createTauri: () => fakeTauri({}),
      createMock: async () => {
        loaded += 1;
        return mock;
      },
    });
    expect(loaded).toBe(1);
    expect(s.backend?.kind).toBe("mock");
  });
});

describe("probeSidecars", () => {
  it("reports versions from the shell, unavailable outside Tauri, failures as text", async () => {
    expect(await probeSidecars(false, () => fakeTauri({}))).toEqual({ status: "unavailable" });
    expect(await probeSidecars(true, () => fakeTauri({ sidecar_versions: async () => ({ ffmpeg: "ffmpeg version n9", exiftool: "13.59" }) }))).toEqual({
      status: "ok",
      versions: { ffmpeg: "ffmpeg version n9", exiftool: "13.59" },
    });
    const failed = await probeSidecars(true, () => fakeTauri({ sidecar_versions: () => Promise.reject("ffmpeg: No such file") }));
    expect(failed).toEqual({ status: "failed", message: "ffmpeg: No such file" });
  });
});

describe("Tauri argument naming", () => {
  it("camel-cases top-level keys only", () => {
    expect(camelArgs({ input_ids: ["a"], options: { keep_originals: true }, job_id: "j", key: "k" })).toEqual({
      inputIds: ["a"],
      options: { keep_originals: true },
      jobId: "j",
      key: "k",
    });
  });

  it("recognises Tauri's missing-command rejection", () => {
    expect(isCommandMissing("Command app_info not found")).toBe(true);
    expect(isCommandMissing({ code: "DISK_FULL", message: "Not enough space", detail: null })).toBe(false);
  });
});
