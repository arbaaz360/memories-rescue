/**
 * The fake export the mock engine serves: a four-part `mydata~1729000000000`
 * export with 4,812 memories from 2016 to 2026. Part 3 is missing unless the
 * user adds it. Every count below adds up, so the summary, the plan, the
 * progress and the report agree with each other the way the real engine's will.
 */

import type { FormatProfile } from "../types";

export interface PartSpec {
  part: number;
  file: string;
  zip_bytes: number;
  mains: number;
  photos: number;
  videos: number;
  overlays: number;
  exact: number;
  ordinal: number;
  none: number;
  loc_applied: number;
  loc_uncertain: number;
  /** Mains per year inside this part (chronological; parts follow each other in time). */
  years: Record<number, number>;
}

export const EXPORT_BASE = "mydata~1729000000000";
export const PROFILE: FormatProfile = "mixed";

export const PARTS: readonly PartSpec[] = [
  {
    part: 1,
    file: `${EXPORT_BASE}.zip`,
    zip_bytes: 2_147_381_274,
    mains: 1305,
    photos: 912,
    videos: 393,
    overlays: 322,
    exact: 498,
    ordinal: 728,
    none: 79,
    loc_applied: 1071,
    loc_uncertain: 12,
    years: { 2016: 210, 2017: 520, 2018: 575 },
  },
  {
    part: 2,
    file: `${EXPORT_BASE}-2.zip`,
    zip_bytes: 2_146_902_113,
    mains: 1290,
    photos: 873,
    videos: 417,
    overlays: 291,
    exact: 502,
    ordinal: 712,
    none: 76,
    loc_applied: 1050,
    loc_uncertain: 14,
    years: { 2019: 690, 2020: 600 },
  },
  {
    part: 3,
    file: `${EXPORT_BASE}-3.zip`,
    zip_bytes: 2_147_105_880,
    mains: 1262,
    photos: 855,
    videos: 407,
    overlays: 318,
    exact: 488,
    ordinal: 717,
    none: 57,
    loc_applied: 1029,
    loc_uncertain: 9,
    years: { 2020: 380, 2021: 520, 2022: 362 },
  },
  {
    part: 4,
    file: `${EXPORT_BASE}-4.zip`,
    zip_bytes: 1_612_447_091,
    mains: 955,
    photos: 620,
    videos: 335,
    overlays: 273,
    exact: 362,
    ordinal: 517,
    none: 76,
    loc_applied: 786,
    loc_uncertain: 6,
    years: { 2023: 300, 2024: 280, 2025: 230, 2026: 145 },
  },
];

export const TOTAL_ENTRIES = PARTS.reduce((n, p) => n + p.mains, 0); // 4,812
export const DATE_FIRST = "2016-03-12";
export const DATE_LAST = "2026-08-30";
/** Overlays in present parts whose main sits in part 3. */
export const ORPHANS_WITHOUT_PART_3 = 23;
/** Largest single item, for the FR-28 temp estimate. */
export const LARGEST_ITEM_BYTES = 182_000_000;
/** The share of a part's size that is media rather than JSON/HTML. */
export const MEDIA_SHARE = 0.995;
export const AVG_OVERLAY_BYTES = 350_000;

export interface Place {
  name: string;
  lat: number;
  lon: number;
  /** UTC offset used for capture times there (summer/winter ignored in the mock). */
  offset: string;
}

export const PLACES: readonly Place[] = [
  { name: "London", lat: 51.5074, lon: -0.1278, offset: "+01:00" },
  { name: "New York", lat: 40.7128, lon: -74.006, offset: "-04:00" },
  { name: "Mumbai", lat: 19.076, lon: 72.8777, offset: "+05:30" },
  { name: "Lisbon", lat: 38.7223, lon: -9.1393, offset: "+01:00" },
  { name: "Sydney", lat: -33.8688, lon: 151.2093, offset: "+10:00" },
  { name: "Mexico City", lat: 19.4326, lon: -99.1332, offset: "-06:00" },
  { name: "Berlin", lat: 52.52, lon: 13.405, offset: "+02:00" },
  { name: "Tokyo", lat: 35.6762, lon: 139.6503, offset: "+09:00" },
  { name: "Cape Town", lat: -33.9249, lon: 18.4241, offset: "+02:00" },
  { name: "São Paulo", lat: -23.5505, lon: -46.6333, offset: "-03:00" },
];

/** Deterministic PRNG (mulberry32) so every run of the mock looks the same. */
export function rng(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** 8-hex-digit short id like the ones in Snapchat filenames. */
export function shortId(n: number): string {
  return ((Math.imul(n + 1, 2654435761) >>> 0).toString(16).toUpperCase()).padStart(8, "0");
}

const pad = (n: number, w = 2) => String(n).padStart(w, "0");

export interface FakeItem {
  index: number;
  part: number;
  id: string;
  media_type: "photo" | "video";
  /** Wall-clock capture time in the place, ISO 8601 with offset. */
  capture_time: string;
  year: number;
  month: number;
  place: Place | null;
  confidence: "exact" | "ordinal" | "none";
  location: "applied" | "none" | "uncertain";
  has_overlay: boolean;
  bytes: number;
  source_name: string;
  planned_path: string;
}

/**
 * The i-th item (0-based, chronological across all parts). Generated on demand
 * so the mock never holds 4,812 objects it does not need.
 */
export function fakeItem(index: number): FakeItem {
  let offset = index;
  let spec = PARTS[0];
  for (const p of PARTS) {
    if (offset < p.mains) {
      spec = p;
      break;
    }
    offset -= p.mains;
  }
  // Year inside the part.
  let yearOffset = offset;
  let year = Number(Object.keys(spec.years)[0]);
  for (const [y, n] of Object.entries(spec.years)) {
    if (yearOffset < n) {
      year = Number(y);
      break;
    }
    yearOffset -= n;
  }
  const inYear = spec.years[year] ?? 1;
  const r = rng(index * 7919 + 17);
  const month = Math.min(12, 1 + Math.floor((yearOffset / inYear) * 12));
  const day = 1 + Math.floor(r() * 28);
  const hour = 8 + Math.floor(r() * 14);
  const minute = Math.floor(r() * 60);
  const second = Math.floor(r() * 60);
  const isVideo = r() < spec.videos / spec.mains;
  const conf = r();
  const confidence: FakeItem["confidence"] =
    conf < spec.exact / spec.mains ? "exact" : conf < (spec.exact + spec.ordinal) / spec.mains ? "ordinal" : "none";
  const locRoll = r();
  const location: FakeItem["location"] =
    confidence === "none"
      ? "none"
      : locRoll < spec.loc_uncertain / spec.mains
        ? "uncertain"
        : locRoll < (spec.loc_uncertain + spec.loc_applied) / spec.mains
          ? "applied"
          : "none";
  const place = location === "none" ? null : PLACES[Math.floor(r() * PLACES.length)];
  const offsetText = place?.offset ?? "+00:00";
  const id = shortId(index);
  const date = `${year}-${pad(month)}-${pad(day)}`;
  const time = `${pad(hour)}${pad(minute)}${pad(second)}`;
  const ext = isVideo ? "mp4" : "jpg";
  const capture =
    confidence === "none"
      ? `${date}T12:00:00+00:00`
      : `${date}T${pad(hour)}:${pad(minute)}:${pad(second)}${offsetText}`;
  const bytes = isVideo ? Math.round(1_800_000 + r() * 6_400_000) : Math.round(380_000 + r() * 520_000);
  const plannedTime = confidence === "none" ? "120000" : time;
  return {
    index,
    part: spec.part,
    id,
    media_type: isVideo ? "video" : "photo",
    capture_time: capture,
    year,
    month,
    place,
    confidence,
    location,
    has_overlay: r() < spec.overlays / spec.mains,
    bytes,
    source_name: `${date}_${id}-main.${ext}`,
    planned_path: `${year}/${pad(month)}/${date}_${plannedTime}_${id}.${ext}`,
  };
}

/** Index of the first item of a part in chronological order. */
export function firstIndexOfPart(part: number): number {
  let n = 0;
  for (const p of PARTS) {
    if (p.part === part) return n;
    n += p.mains;
  }
  return n;
}

export function partOf(name: string): number | null {
  const lower = name.toLowerCase();
  if (!lower.startsWith(EXPORT_BASE.toLowerCase())) return null;
  const rest = lower.slice(EXPORT_BASE.length);
  if (rest === ".zip" || rest === "" || rest === " (1).zip") return 1;
  const m = /^-(\d+)(?: \(\d+\))?\.zip$/.exec(rest);
  if (m) {
    const n = Number(m[1]);
    return n >= 1 && n <= PARTS.length ? n : null;
  }
  return null;
}
