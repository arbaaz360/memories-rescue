/**
 * The screen flow of SRS §7.1 as a pure reducer. The store dispatches user
 * actions and engine `job_state` events here; side effects (commands) live in
 * the store. Unit-tested in flow.test.ts.
 *
 *   S0 Welcome -> S1 Add files -> S2 Check -> S3 Options -> S4 Preview -> S5 Processing -> S6 Results
 *                                              S3 ---------------------------^
 *   S6 -- Retry failed --> S5        S6 -- Add missing part --> S2 (job mode) -- Continue --> S5
 *   S5 -- free limit --> Unlock dialog -- Activate --> S5 (resumed) / Later --> S6
 */

import type { JobState } from "../ipc/types";

export type Screen = "welcome" | "add_files" | "summary" | "options" | "preview" | "processing" | "results";
export type Page = "settings" | "about";
export type UnlockReason = "free_limit" | "results" | "header";

export interface FlowState {
  screen: Screen;
  /** Settings or About, shown over the flow; the flow underneath keeps its place. */
  page: Page | null;
  unlock: UnlockReason | null;
  jobId: string | null;
  jobState: JobState | null;
  /** S2 reached from S6 "Add missing part": Continue resumes the job instead of going to S3. */
  summaryMode: "new" | "add_part";
}

export const initialFlow: FlowState = {
  screen: "welcome",
  page: null,
  unlock: null,
  jobId: null,
  jobState: null,
  summaryMode: "new",
};

export type FlowEvent =
  | { type: "get_started" }
  | { type: "resume_previous"; jobId: string }
  | { type: "inputs_continue"; usable: number }
  | { type: "summary_continue"; blocked: boolean }
  | { type: "back" }
  | { type: "preview" }
  | { type: "job_started"; jobId: string }
  | { type: "job_state"; jobId: string; state: JobState }
  | { type: "retry_started" }
  | { type: "add_part_to_job" }
  | { type: "job_resumed" }
  | { type: "open_unlock"; reason: UnlockReason }
  | { type: "close_unlock" }
  | { type: "open_page"; page: Page }
  | { type: "close_page" }
  | { type: "new_job" };

export const TERMINAL_STATES: readonly JobState[] = ["done", "partial", "failed", "cancelled"];

export function isTerminal(state: JobState | null): boolean {
  return state !== null && TERMINAL_STATES.includes(state);
}

/** The step a screen shows in the progress indicator (Welcome has none). */
export const STEPS: readonly Screen[] = ["add_files", "summary", "options", "preview", "processing", "results"];

export function transition(state: FlowState, event: FlowEvent): FlowState {
  switch (event.type) {
    case "get_started":
      return state.screen === "welcome" ? { ...state, screen: "add_files" } : state;

    case "resume_previous":
      return state.screen === "welcome"
        ? { ...state, screen: "processing", jobId: event.jobId, jobState: "running", summaryMode: "new" }
        : state;

    case "inputs_continue":
      return state.screen === "add_files" && event.usable > 0 ? { ...state, screen: "summary", summaryMode: "new" } : state;

    case "summary_continue":
      if (state.screen !== "summary" || event.blocked) return state;
      return state.summaryMode === "add_part"
        ? { ...state, screen: "processing", jobState: "running", summaryMode: "new" }
        : { ...state, screen: "options" };

    case "back":
      switch (state.screen) {
        case "add_files":
          return { ...state, screen: "welcome" };
        case "summary":
          return state.summaryMode === "add_part"
            ? { ...state, screen: "results", summaryMode: "new" }
            : { ...state, screen: "add_files" };
        case "options":
          return { ...state, screen: "summary" };
        case "preview":
          return { ...state, screen: "options" };
        default:
          return state;
      }

    case "preview":
      return state.screen === "options" ? { ...state, screen: "preview" } : state;

    case "job_started":
      return state.screen === "options" || state.screen === "preview"
        ? { ...state, screen: "processing", jobId: event.jobId, jobState: "running" }
        : state;

    case "job_state": {
      if (event.jobId !== state.jobId) return state;
      const next: FlowState = { ...state, jobState: event.state };
      if (event.state === "free_limit_reached") {
        return state.screen === "processing" ? { ...next, unlock: "free_limit" } : next;
      }
      if (event.state === "running") {
        // Unlocked and resumed, retried, or a part added: back to the progress screen.
        const unlock = state.unlock === "free_limit" || state.unlock === "results" ? null : state.unlock;
        if (state.screen === "results") return { ...next, screen: "processing", unlock };
        return { ...next, unlock };
      }
      if (isTerminal(event.state) && state.screen === "processing") {
        return { ...next, screen: "results", unlock: state.unlock === "free_limit" ? null : state.unlock };
      }
      return next;
    }

    case "retry_started":
      return state.screen === "results" ? { ...state, screen: "processing", jobState: "running" } : state;

    case "add_part_to_job":
      return state.screen === "results" && state.jobId ? { ...state, screen: "summary", summaryMode: "add_part" } : state;

    case "job_resumed":
      if (state.screen === "results" || (state.screen === "summary" && state.summaryMode === "add_part")) {
        return { ...state, screen: "processing", jobState: "running", summaryMode: "new", unlock: null };
      }
      return { ...state, unlock: state.unlock === "free_limit" ? null : state.unlock };

    case "open_unlock":
      return { ...state, unlock: event.reason };

    case "close_unlock":
      return { ...state, unlock: null };

    case "open_page":
      return { ...state, page: event.page };

    case "close_page":
      return { ...state, page: null };

    case "new_job":
      return { ...initialFlow, screen: "add_files" };
  }
}
