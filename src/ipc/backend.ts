/**
 * The window talks to the engine only through `Backend`. Two implementations:
 * `TauriBackend` (invoke/listen with the contract names, `./tauri.ts`) and
 * `MockBackend` (pure TypeScript, runs in a plain browser, `./mock/engine.ts`).
 *
 * Selection at startup (`selectBackend`):
 * 1. Not inside Tauri: mock.
 * 2. Inside Tauri: probe `app_info`. If it answers, Tauri. If it fails
 *    (typically "command app_info not found" while the engine is not wired),
 *    mock, and the reason is kept for the dev badge and About.
 * A release build passes `createMock: null`: it never runs on the mock, and
 * a failed probe gives `backend: null`, which the window shows as a plain
 * error screen.
 * `sidecar_versions` is probed separately and never blocks the first paint.
 */

import { toIpcError, type CommandName, type Commands, type EngineEvents, type SidecarVersions } from "./types";

export type Unlisten = () => void;

export interface Backend {
  readonly kind: "tauri" | "mock";
  call<K extends CommandName>(command: K, args: Commands[K]["args"]): Promise<Commands[K]["result"]>;
  on<E extends keyof EngineEvents>(event: E, handler: (payload: EngineEvents[E]) => void): Promise<Unlisten>;
}

export type SelectionReason = "no_tauri" | "engine" | "engine_missing" | "engine_error";

export interface Selection {
  /** null in a release build when the engine did not answer: show the error screen. */
  backend: Backend | null;
  reason: SelectionReason;
  /** Message of the failed probe, when the mock was chosen inside Tauri. */
  detail: string | null;
}

export interface SelectDeps {
  isTauri: () => boolean;
  createTauri: () => Backend;
  /** null in release builds: the mock is not even bundled there. */
  createMock: (() => Backend | Promise<Backend>) | null;
  /** How long the engine probe may take before the mock takes over. */
  timeoutMs?: number;
}

export function isCommandMissing(err: unknown): boolean {
  const text = typeof err === "string" ? err : toIpcError(err).message;
  return /not found|unknown command|not allowed/i.test(text);
}

function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`timed out after ${ms} ms`)), ms);
    p.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      (e: unknown) => {
        clearTimeout(timer);
        reject(e);
      },
    );
  });
}

export async function selectBackend(deps: SelectDeps): Promise<Selection> {
  const mock = async () => (deps.createMock ? await deps.createMock() : null);
  if (!deps.isTauri()) return { backend: await mock(), reason: "no_tauri", detail: null };
  const tauri = deps.createTauri();
  try {
    await withTimeout(tauri.call("app_info", {}), deps.timeoutMs ?? 3000);
    return { backend: tauri, reason: "engine", detail: null };
  } catch (err) {
    const detail = toIpcError(err).message || String(err);
    return { backend: await mock(), reason: isCommandMissing(err) ? "engine_missing" : "engine_error", detail };
  }
}

export type SidecarProbe =
  | { status: "unavailable" }
  | { status: "ok"; versions: SidecarVersions }
  | { status: "failed"; message: string };

/** The startup smoke call. Only meaningful inside Tauri; the shell answers it even without the engine. */
export async function probeSidecars(isTauri: boolean, tauri: () => Backend): Promise<SidecarProbe> {
  if (!isTauri) return { status: "unavailable" };
  try {
    const versions = await tauri().call("sidecar_versions", {});
    return { status: "ok", versions };
  } catch (err) {
    return { status: "failed", message: toIpcError(err).message || String(err) };
  }
}

/** True inside a Tauri webview (`window.__TAURI_INTERNALS__` is injected before any script runs). */
export function detectTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
