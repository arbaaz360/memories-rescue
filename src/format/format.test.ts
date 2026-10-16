import { describe, expect, it } from "vitest";
import {
  baseName,
  byteBaseFor,
  durationParts,
  formatBytes,
  formatCaptureTime,
  formatCoordinate,
  formatDuration,
  formatPercent,
  joinPath,
  roundRate,
} from "./format";

describe("formatBytes", () => {
  it("uses decimal units by default and one decimal below 100", () => {
    expect(formatBytes(4_250_000_000, "en-US")).toBe("4.3 GB");
    expect(formatBytes(48_300_000_000, "en-US")).toBe("48.3 GB");
    expect(formatBytes(412_700_000_000, "en-US")).toBe("413 GB");
    expect(formatBytes(1_500, "en-US")).toBe("1.5 kB");
  });

  it("uses binary steps for Windows, labelled the way Explorer labels them", () => {
    expect(byteBaseFor("windows")).toBe(1024);
    expect(byteBaseFor("macos")).toBe(1000);
    expect(formatBytes(1024 ** 3 * 2, "en-US", 1024)).toBe("2 GB");
    expect(formatBytes(1024 ** 2 * 1.5, "en-US", 1024)).toBe("1.5 MB");
  });

  it("never shows 1,000 of a unit", () => {
    expect(formatBytes(999_960_000, "en-US")).toBe("1 GB");
  });

  it("spells out small sizes and clamps negatives", () => {
    expect(formatBytes(512, "en-US")).toBe("512 bytes");
    expect(formatBytes(-5, "en-US")).toBe("0 bytes");
  });

  it("follows the user's locale", () => {
    // German puts a no-break space between number and unit.
    expect(formatBytes(4_250_000_000, "de-DE")).toBe("4,3 GB");
  });
});

describe("ETA rounding and duration", () => {
  it("says under a minute below 60 s", () => {
    expect(durationParts(0)).toEqual({ kind: "under_minute" });
    expect(durationParts(59)).toEqual({ kind: "under_minute" });
    expect(formatDuration(30, "en-US")).toBeNull();
  });

  it("rounds to the minute under an hour", () => {
    expect(durationParts(61)).toEqual({ kind: "duration", hours: 0, minutes: 1 });
    expect(durationParts(1_190)).toEqual({ kind: "duration", hours: 0, minutes: 20 });
    expect(formatDuration(1_190, "en-US")).toBe("20 min");
  });

  it("rounds to 5 minutes between 1 and 10 hours", () => {
    expect(durationParts(3_600 + 7 * 60)).toEqual({ kind: "duration", hours: 1, minutes: 5 });
    expect(formatDuration(2 * 3_600 + 4 * 60, "en-US")).toBe("2 hr 5 min");
    expect(formatDuration(3_600 * 3 + 60, "en-US")).toBe("3 hr");
  });

  it("rounds to the hour beyond 10 hours", () => {
    expect(durationParts(11.6 * 3_600)).toEqual({ kind: "duration", hours: 12, minutes: 0 });
  });

  it("is stable while the estimate wobbles by a few seconds", () => {
    const shown = new Set([2_390, 2_400, 2_410].map((s) => formatDuration(s, "en-US")));
    expect(shown.size).toBe(1);
  });

  it("rounds throughput to one decimal below 10", () => {
    expect(roundRate(2.437)).toEqual({ value: 2.4, digits: 1 });
    expect(roundRate(14.6)).toEqual({ value: 15, digits: 0 });
  });

  it("formats percentages from 0..100", () => {
    expect(formatPercent(37.4, "en-US")).toBe("37%");
    expect(formatPercent(120, "en-US")).toBe("100%");
  });
});

describe("capture times", () => {
  it("shows the wall clock of the place, not of the viewer", () => {
    expect(formatCaptureTime("2019-07-04T21:03:55+01:00", "en-GB")).toBe("4 Jul 2019, 21:03");
    expect(formatCaptureTime("2019-07-04T21:03:55-05:00", "en-US")).toBe("Jul 4, 2019, 9:03 PM");
  });

  it("shows the date only at day precision", () => {
    expect(formatCaptureTime("2019-07-04T12:00:00+00:00", "en-GB", "day")).toBe("4 Jul 2019");
    expect(formatCaptureTime("2019-07-04", "en-US")).toBe("Jul 4, 2019");
  });

  it("returns unparseable input unchanged", () => {
    expect(formatCaptureTime("yesterday", "en-US")).toBe("yesterday");
  });

  it("formats coordinates to 4 decimals without sign", () => {
    expect(formatCoordinate(-0.12776, "en-US")).toBe("0.1278");
    expect(formatCoordinate(51.5, "de-DE")).toBe("51,5000");
  });
});

describe("paths", () => {
  it("joins with the separator the folder uses", () => {
    expect(joinPath("D:\\Memories\\", "Memories Rescue")).toBe("D:\\Memories\\Memories Rescue");
    expect(joinPath("/Users/you/Pictures", "Memories Rescue", "_report")).toBe("/Users/you/Pictures/Memories Rescue/_report");
  });

  it("takes the last segment of either kind", () => {
    expect(baseName("C:\\Users\\you\\Downloads\\mydata~1.zip")).toBe("mydata~1.zip");
    expect(baseName("/Volumes/USB/")).toBe("USB");
  });
});
