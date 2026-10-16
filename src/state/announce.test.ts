import { describe, expect, it } from "vitest";
import { AnnounceThrottle, PROGRESS_ANNOUNCE_INTERVAL_MS } from "./announce";

describe("progress announcements (NFR-USE-03)", () => {
  it("allows at most one announcement per 10 s whatever the event rate", () => {
    const th = new AnnounceThrottle();
    let announced = 0;
    // Engine events 4 times a second for two minutes.
    for (let ms = 0; ms <= 120_000; ms += 250) if (th.allow(ms)) announced += 1;
    expect(PROGRESS_ANNOUNCE_INTERVAL_MS).toBe(10_000);
    expect(announced).toBe(13);
  });

  it("starts again after a reset", () => {
    const th = new AnnounceThrottle();
    expect(th.allow(0)).toBe(true);
    expect(th.allow(5_000)).toBe(false);
    th.reset();
    expect(th.allow(5_001)).toBe(true);
  });
});
