import { describe, expect, it } from "vitest";
import { initialFlow, transition, type FlowEvent, type FlowState } from "./flow";

function run(events: FlowEvent[], from: FlowState = initialFlow): FlowState {
  return events.reduce(transition, from);
}

const toOptions: FlowEvent[] = [
  { type: "get_started" },
  { type: "inputs_continue", usable: 3 },
  { type: "summary_continue", blocked: false },
];

describe("primary flow (SRS §7.1)", () => {
  it("walks S0 to S6 through the preview", () => {
    const s = run([
      ...toOptions,
      { type: "preview" },
      { type: "job_started", jobId: "j1" },
      { type: "job_state", jobId: "j1", state: "running" },
      { type: "job_state", jobId: "j1", state: "done" },
    ]);
    expect(s.screen).toBe("results");
    expect(s.jobState).toBe("done");
    expect(s.jobId).toBe("j1");
  });

  it("can start straight from S3", () => {
    const s = run([...toOptions, { type: "job_started", jobId: "j1" }]);
    expect(s.screen).toBe("processing");
  });

  it("does not leave S1 without a usable input", () => {
    const s = run([{ type: "get_started" }, { type: "inputs_continue", usable: 0 }]);
    expect(s.screen).toBe("add_files");
  });

  it("does not leave S2 when the export is blocked (legacy links only)", () => {
    const s = run([{ type: "get_started" }, { type: "inputs_continue", usable: 1 }, { type: "summary_continue", blocked: true }]);
    expect(s.screen).toBe("summary");
  });

  it("goes back one screen at a time and stops at Welcome", () => {
    let s = run([...toOptions, { type: "preview" }]);
    const seen: string[] = [];
    for (let i = 0; i < 5; i++) {
      s = transition(s, { type: "back" });
      seen.push(s.screen);
    }
    expect(seen).toEqual(["options", "summary", "add_files", "welcome", "welcome"]);
  });

  it("ignores events that do not belong to the current screen", () => {
    const s = run([{ type: "preview" }, { type: "job_started", jobId: "x" }, { type: "retry_started" }]);
    expect(s).toEqual(initialFlow);
  });

  it("ignores job_state events of another job", () => {
    const s = run([...toOptions, { type: "job_started", jobId: "j1" }, { type: "job_state", jobId: "other", state: "done" }]);
    expect(s.screen).toBe("processing");
    expect(s.jobState).toBe("running");
  });
});

describe("free-limit interruption", () => {
  const atLimit = run([...toOptions, { type: "job_started", jobId: "j1" }, { type: "job_state", jobId: "j1", state: "free_limit_reached" }]);

  it("opens the Unlock dialog over S5", () => {
    expect(atLimit.screen).toBe("processing");
    expect(atLimit.unlock).toBe("free_limit");
  });

  it("closes it and stays on S5 when the job resumes after activation", () => {
    const s = transition(atLimit, { type: "job_state", jobId: "j1", state: "running" });
    expect(s.screen).toBe("processing");
    expect(s.unlock).toBeNull();
  });

  it("goes to S6 when the user chooses Later and the job finishes partial", () => {
    const later = transition(atLimit, { type: "close_unlock" });
    const s = transition(later, { type: "job_state", jobId: "j1", state: "partial" });
    expect(s.screen).toBe("results");
    expect(s.unlock).toBeNull();
  });

  it("Unlock to finish from S6 returns to S5 when the job runs again", () => {
    const results = run([{ type: "close_unlock" }, { type: "job_state", jobId: "j1", state: "partial" }, { type: "open_unlock", reason: "results" }], atLimit);
    const s = transition(results, { type: "job_state", jobId: "j1", state: "running" });
    expect(s.screen).toBe("processing");
    expect(s.unlock).toBeNull();
  });
});

describe("from S6", () => {
  const results = run([...toOptions, { type: "job_started", jobId: "j1" }, { type: "job_state", jobId: "j1", state: "partial" }]);

  it("Retry failed goes back to S5", () => {
    expect(transition(results, { type: "retry_started" }).screen).toBe("processing");
  });

  it("Add missing part goes to S2 in job mode, whose Continue resumes the job on S5", () => {
    const summary = transition(results, { type: "add_part_to_job" });
    expect(summary.screen).toBe("summary");
    expect(summary.summaryMode).toBe("add_part");
    const resumed = transition(summary, { type: "summary_continue", blocked: false });
    expect(resumed.screen).toBe("processing");
    expect(resumed.summaryMode).toBe("new");
  });

  it("Back from S2 in job mode returns to S6", () => {
    const s = run([{ type: "add_part_to_job" }, { type: "back" }], results);
    expect(s.screen).toBe("results");
  });

  it("New job starts over on S1 with nothing from the old job", () => {
    const s = transition(results, { type: "new_job" });
    expect(s).toEqual({ ...initialFlow, screen: "add_files" });
  });
});

describe("resume and pages", () => {
  it("Resume previous job goes from S0 straight to S5", () => {
    const s = transition(initialFlow, { type: "resume_previous", jobId: "old" });
    expect(s.screen).toBe("processing");
    expect(s.jobId).toBe("old");
  });

  it("Settings and About sit over the flow without moving it", () => {
    const options = run(toOptions);
    const s = run([{ type: "open_page", page: "settings" }, { type: "open_page", page: "about" }], options);
    expect(s.page).toBe("about");
    expect(s.screen).toBe("options");
    expect(transition(s, { type: "close_page" })).toEqual(options);
  });

  it("a job that finishes while a page is open still lands on S6", () => {
    const s = run([...toOptions, { type: "job_started", jobId: "j" }, { type: "open_page", page: "settings" }, { type: "job_state", jobId: "j", state: "done" }]);
    expect(s.screen).toBe("results");
    expect(s.page).toBe("settings");
  });
});
