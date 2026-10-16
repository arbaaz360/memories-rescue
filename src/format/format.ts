/**
 * Locale-aware formatting for numbers, sizes, durations and dates. Everything
 * goes through Intl in the user's formatting locale (NFR-I18N-03); nothing here
 * joins strings by hand. Pure functions, unit-tested in format.test.ts.
 */

const BYTE_UNITS = ["kilobyte", "megabyte", "gigabyte", "terabyte", "petabyte"] as const;

/** Decimal units (macOS Finder) or binary units labelled the same way (Windows Explorer). */
export type ByteBase = 1000 | 1024;

const nfCache = new Map<string, Intl.NumberFormat>();

function nf(locale: string, options: Intl.NumberFormatOptions): Intl.NumberFormat {
  const key = `${locale}|${JSON.stringify(options)}`;
  let f = nfCache.get(key);
  if (!f) {
    f = new Intl.NumberFormat(locale, options);
    nfCache.set(key, f);
  }
  return f;
}

/**
 * "4.2 GB", "48 GB", "512 bytes". One decimal below 100 of a unit, none above.
 * `base` 1024 matches what Windows Explorer shows for the same drive.
 */
export function formatBytes(bytes: number, locale: string, base: ByteBase = 1000): string {
  const value = Math.max(0, bytes);
  if (value < base) {
    return nf(locale, { style: "unit", unit: "byte", unitDisplay: "long", maximumFractionDigits: 0 }).format(value);
  }
  let scaled = value / base;
  let unit = 0;
  while (scaled >= base && unit < BYTE_UNITS.length - 1) {
    scaled /= base;
    unit += 1;
  }
  // 999.96 would round up to "1,000 MB"; show it as the next unit instead.
  if (scaled >= 999.5 && unit < BYTE_UNITS.length - 1) {
    scaled /= base;
    unit += 1;
  }
  const digits = scaled < 100 ? 1 : 0;
  return nf(locale, {
    style: "unit",
    unit: BYTE_UNITS[unit],
    unitDisplay: "short",
    maximumFractionDigits: digits,
  }).format(scaled);
}

export function formatNumber(value: number, locale: string, options: Intl.NumberFormatOptions = {}): string {
  return nf(locale, options).format(value);
}

/** 0..100 in, "37%" out. */
export function formatPercent(pct: number, locale: string): string {
  const clamped = Math.min(100, Math.max(0, pct));
  return nf(locale, { style: "percent", maximumFractionDigits: 0 }).format(clamped / 100);
}

/** How an ETA or estimate is presented: under a minute, or hours and minutes. */
export type DurationParts = { kind: "under_minute" } | { kind: "duration"; hours: number; minutes: number };

/**
 * Rounds a remaining time so it reads as an estimate and does not flicker:
 * under 1 min is "less than a minute"; under 1 h to the minute; up to 10 h to
 * 5 minutes; beyond that to the hour.
 */
export function durationParts(seconds: number): DurationParts {
  const s = Math.max(0, seconds);
  if (s < 60) return { kind: "under_minute" };
  let totalMinutes: number;
  if (s < 3600) totalMinutes = Math.max(1, Math.round(s / 60));
  else if (s < 36000) totalMinutes = Math.round(s / 300) * 5;
  else totalMinutes = Math.round(s / 3600) * 60;
  return { kind: "duration", hours: Math.floor(totalMinutes / 60), minutes: totalMinutes % 60 };
}

/** "2 hr 5 min", "45 min", "3 hr". Returns null for under a minute (the caller has a sentence for that). */
export function formatDuration(seconds: number, locale: string): string | null {
  const parts = durationParts(seconds);
  if (parts.kind === "under_minute") return null;
  const pieces: string[] = [];
  if (parts.hours > 0) {
    pieces.push(nf(locale, { style: "unit", unit: "hour", unitDisplay: "short" }).format(parts.hours));
  }
  if (parts.minutes > 0 || parts.hours === 0) {
    pieces.push(nf(locale, { style: "unit", unit: "minute", unitDisplay: "short" }).format(parts.minutes));
  }
  return new Intl.ListFormat(locale, { type: "unit", style: "narrow" }).format(pieces);
}

interface WallClock {
  y: number;
  mo: number;
  d: number;
  h: number;
  mi: number;
  s: number;
  hasTime: boolean;
}

/** Reads the wall-clock fields of an ISO 8601 string, ignoring its offset. */
export function parseWallClock(iso: string): WallClock | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2})(?::(\d{2}))?)?/.exec(iso);
  if (!m) return null;
  return {
    y: Number(m[1]),
    mo: Number(m[2]),
    d: Number(m[3]),
    h: m[4] ? Number(m[4]) : 0,
    mi: m[5] ? Number(m[5]) : 0,
    s: m[6] ? Number(m[6]) : 0,
    hasTime: m[4] !== undefined,
  };
}

/**
 * A capture time in the place it was taken: "4 Jul 2019, 21:03" for
 * "2019-07-04T21:03:55+01:00", whatever the viewer's own time zone. With
 * day precision only the date is shown.
 */
export function formatCaptureTime(iso: string, locale: string, precision: "second" | "day" = "second"): string {
  const w = parseWallClock(iso);
  if (!w) return iso;
  const date = new Date(Date.UTC(w.y, w.mo - 1, w.d, w.h, w.mi, w.s));
  const options: Intl.DateTimeFormatOptions =
    precision === "day" || !w.hasTime
      ? { dateStyle: "medium", timeZone: "UTC" }
      : { dateStyle: "medium", timeStyle: "short", timeZone: "UTC" };
  return new Intl.DateTimeFormat(locale, options).format(date);
}

/** A calendar date ("2016-03-12") as "12 Mar 2016" in the user's locale. */
export function formatCalendarDate(isoDate: string, locale: string): string {
  return formatCaptureTime(isoDate, locale, "day");
}

/** An instant (UTC) in the viewer's own time zone, for job times. */
export function formatInstant(iso: string, locale: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  return new Intl.DateTimeFormat(locale, { dateStyle: "medium", timeStyle: "short" }).format(date);
}

/** Absolute coordinate with 4 decimals (about 11 m), as written to the files. */
export function formatCoordinate(value: number, locale: string): string {
  return nf(locale, { minimumFractionDigits: 4, maximumFractionDigits: 4 }).format(Math.abs(value));
}

/** Throughput for display: one decimal under 10, none above. */
export function roundRate(itemsPerSecond: number): { value: number; digits: number } {
  const v = Math.max(0, itemsPerSecond);
  return v < 10 ? { value: Math.round(v * 10) / 10, digits: 1 } : { value: Math.round(v), digits: 0 };
}

/** Which byte base the platform's file manager uses. */
export function byteBaseFor(platform: string): ByteBase {
  return platform === "windows" ? 1024 : 1000;
}

/** Joins a folder and a name with the platform's separator, for display and for open_path. */
export function joinPath(folder: string, ...names: string[]): string {
  const sep = folder.includes("\\") || /^[A-Za-z]:/.test(folder) ? "\\" : "/";
  let out = folder.replace(/[\\/]+$/, "");
  for (const n of names) out = `${out}${sep}${n}`;
  return out;
}

/** Last path segment, either separator. */
export function baseName(path: string): string {
  const trimmed = path.replace(/[\\/]+$/, "");
  const i = Math.max(trimmed.lastIndexOf("\\"), trimmed.lastIndexOf("/"));
  return i >= 0 ? trimmed.slice(i + 1) : trimmed;
}
