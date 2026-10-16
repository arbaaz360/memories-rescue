import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Backend, Unlisten } from "./backend";
import { toIpcError, type CommandName, type Commands, type EngineEvents } from "./types";

/** `input_ids` -> `inputIds`: Tauri 2's default naming for top-level command arguments. */
export function camelArgs(args: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(args)) {
    out[k.replace(/_([a-z0-9])/g, (_, c: string) => c.toUpperCase())] = v;
  }
  return out;
}

export class TauriBackend implements Backend {
  readonly kind = "tauri" as const;

  async call<K extends CommandName>(command: K, args: Commands[K]["args"]): Promise<Commands[K]["result"]> {
    try {
      return await invoke<Commands[K]["result"]>(command, camelArgs(args as Record<string, unknown>));
    } catch (err) {
      throw toIpcError(err);
    }
  }

  async on<E extends keyof EngineEvents>(event: E, handler: (payload: EngineEvents[E]) => void): Promise<Unlisten> {
    return listen<EngineEvents[E]>(event, (e) => handler(e.payload));
  }
}
