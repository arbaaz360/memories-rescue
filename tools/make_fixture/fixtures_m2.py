"""M2 fixtures (SRS v0.2 section 11.2): F-11 tz-edge, F-12 unicode-paths,
F-13 free-limit.

All three are single-part 2026b exports with real tiny JPEG photos (Pillow)
and, where videos appear, the small real MP4s from crates/mr-meta/tests/data
so the metadata writer and the engine can process them end to end.

F-11's truth.json carries, per item, the IANA zone the point lies in and the
local time and UTC offset computed with Python's zoneinfo, so the Rust
resolver (tzf-rs + chrono-tz) is checked against an independent tzdb.
"""

from __future__ import annotations

import datetime as dt
from pathlib import Path
from zoneinfo import ZoneInfo

import media
from common import (
    GENERATOR_NAME,
    GENERATOR_VERSION,
    INDEX_HTML,
    MEMORIES_HTML,
    NO_LOCATION_FIELD,
    TRUTH_SCHEMA,
    UTC,
    Context,
    ZipBuilder,
    export_date,
    filename_date,
    iso_utc,
    json_bytes,
    location_field,
    media_type_field,
    new_id,
    part_name,
    random_instant,
    random_location,
    reset_dir,
    write_truth,
)

SMALL = (270, 480)
HOME_ZONE = "Asia/Kolkata"

# (case, utc, lat, lon, iana zone the point is in; None for no GPS)
TZ_CASES = [
    ("ac1_new_york_summer", "2020-07-01T12:00:00Z", 40.7128, -74.0060, "America/New_York"),
    ("ac1_london_summer", "2020-07-01T12:00:00Z", 51.5074, -0.1278, "Europe/London"),
    ("ac1_no_gps_home_zone", "2020-07-01T12:00:00Z", None, None, None),
    ("ny_spring_forward_before", "2021-03-14T06:59:59Z", 40.7128, -74.0060, "America/New_York"),
    ("ny_spring_forward_after", "2021-03-14T07:00:00Z", 40.7128, -74.0060, "America/New_York"),
    ("ny_fall_back_before", "2021-11-07T05:59:59Z", 40.7128, -74.0060, "America/New_York"),
    ("ny_fall_back_after", "2021-11-07T06:00:00Z", 40.7128, -74.0060, "America/New_York"),
    ("london_spring_before", "2021-03-28T00:59:59Z", 51.5074, -0.1278, "Europe/London"),
    ("london_spring_after", "2021-03-28T01:00:00Z", 51.5074, -0.1278, "Europe/London"),
    ("london_autumn_before", "2021-10-31T00:59:59Z", 51.5074, -0.1278, "Europe/London"),
    ("london_autumn_after", "2021-10-31T01:00:00Z", 51.5074, -0.1278, "Europe/London"),
    ("date_line_kiritimati_next_day", "2021-01-01T11:00:00Z", 1.8721, -157.4278, "Pacific/Kiritimati"),
    ("date_line_pago_pago_previous_day", "2021-01-01T10:30:00Z", -14.2756, -170.7020, "Pacific/Pago_Pago"),
    ("apia_summer_plus_14", "2021-01-15T12:00:00Z", -13.8333, -171.7500, "Pacific/Apia"),
    ("kathmandu_plus_0545", "2021-06-01T12:00:00Z", 27.7172, 85.3240, "Asia/Kathmandu"),
    ("kolkata_plus_0530", "2021-06-01T12:00:00Z", 22.5726, 88.3639, "Asia/Kolkata"),
    ("chatham_plus_1345", "2021-01-15T12:00:00Z", -43.9500, -176.5500, "Pacific/Chatham"),
    ("adelaide_plus_1030", "2021-01-15T12:00:00Z", -34.9285, 138.6007, "Australia/Adelaide"),
    ("st_johns_minus_0230", "2021-07-01T12:00:00Z", 47.5615, -52.7126, "America/St_Johns"),
    ("lord_howe_plus_11", "2021-01-15T12:00:00Z", -31.5500, 159.0833, "Australia/Lord_Howe"),
    ("honolulu_minus_10", "2021-07-01T12:00:00Z", 21.3069, -157.8583, "Pacific/Honolulu"),
    ("tokyo_crosses_month", "2021-03-31T15:30:00Z", 35.6762, 139.6503, "Asia/Tokyo"),
    ("sydney_crosses_year", "2020-12-31T14:30:00Z", -33.8688, 151.2093, "Australia/Sydney"),
    ("mid_atlantic_ocean", "2021-06-01T12:00:00Z", 0.0001, -30.0, None),
]


def _tiny_mp4(ctx: Context, index: int) -> bytes | None:
    """One of the real tiny MP4s shipped with mr-meta, alternating layouts."""
    root = Path(__file__).resolve().parents[2]
    names = ["tiny-moov-first.mp4", "tiny-moov-last.mp4"]
    path = root / "crates" / "mr-meta" / "tests" / "data" / names[index % 2]
    if path.is_file():
        return path.read_bytes()
    return None


def _generator_block(ctx: Context, extra: dict | None = None) -> dict:
    block = {
        "name": GENERATOR_NAME,
        "version": GENERATOR_VERSION,
        "seed": ctx.seed,
        "zip_order": ctx.zip_order,
        "json_order": "newest_first",
        "ffmpeg": {"available": False, "note": "not used: videos are the tiny real MP4s from crates/mr-meta/tests/data"},
    }
    if extra:
        block.update(extra)
    return block


def _local_expectation(utc: dt.datetime, zone: str) -> dict:
    local = utc.astimezone(ZoneInfo(zone))
    offset = local.utcoffset()
    assert offset is not None
    secs = int(offset.total_seconds())
    sign = "+" if secs >= 0 else "-"
    a = abs(secs)
    return {
        "zone": zone,
        "offset_seconds": secs,
        "offset": f"{sign}{a // 3600:02d}:{(a % 3600) // 60:02d}",
        "local": local.strftime("%Y-%m-%dT%H:%M:%S"),
        "local_date": local.strftime("%Y-%m-%d"),
        "local_folder": local.strftime("%Y/%m"),
        "dst": bool(local.dst()),
    }


def build_tz_edge(ctx: Context) -> dict:
    """F-11: one photo per timezone case, all in the 2026b profile."""
    name = "tz-edge"
    rng = ctx.rng(name)
    out = ctx.out_dir / name
    reset_dir(out)
    drafts = []
    for case, utc_s, lat, lon, zone in TZ_CASES:
        utc = dt.datetime.strptime(utc_s, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=UTC)
        drafts.append({"case": case, "when": utc, "lat": lat, "lon": lon, "zone": zone, "id": new_id(rng)})
    drafts.sort(key=lambda d: d["when"], reverse=True)

    zipb = ZipBuilder()
    entries = []
    items = []
    for i, d in enumerate(drafts):
        date_str = filename_date(d["when"])
        main_name = f"memories/{date_str}_{d['id']}-main.jpg"
        has_gps = d["lat"] is not None
        loc_field = location_field(d["lat"], d["lon"]) if has_gps else NO_LOCATION_FIELD
        entry = {"Date": export_date(d["when"]), "Media Type": "Image", "Location": loc_field, "Download Link": "", "Media Download Url": "N/A"}
        entries.append(entry)
        colour = (rng.randrange(40, 220), rng.randrange(40, 220), rng.randrange(40, 220))
        zipb.add_media(main_name, media.make_photo(d["id"], colour, "jpg", entry["Date"], SMALL))
        if d["zone"]:
            expected = {"method": "gps", **_local_expectation(d["when"], d["zone"])}
        elif has_gps:
            expected = {"method": "gps_unknown_or_home", "zone": None, "note": "ocean point: the boundary dataset may give an Etc/GMT zone or nothing; the home zone is the fallback"}
        else:
            expected = {"method": "home", **_local_expectation(d["when"], HOME_ZONE)}
        items.append(
            {
                "json_index": i,
                "case": d["case"],
                "id": d["id"],
                "date_utc": iso_utc(d["when"]),
                "date_field": entry["Date"],
                "media_type": "photo",
                "ext": "jpg",
                "main": main_name,
                "overlay": None,
                "location": {"lat": d["lat"], "lon": d["lon"]} if has_gps else None,
                "expected": expected,
            }
        )
    zipb.add_header("index.html", INDEX_HTML)
    zipb.add_header("html/memories_history.html", MEMORIES_HTML)
    zipb.add_header("json/memories_history.json", json_bytes({"Saved Media": entries}))
    zip_path = out / part_name()
    order = zipb.write(zip_path, ctx.zip_order, rng)
    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": "F-11",
        "format": "2026b",
        "generator": _generator_block(ctx),
        "home_zone": HOME_ZONE,
        "parts": [{"file": zip_path.name, "role": "primary", "entries": order}],
        "json": {"bom": False, "crlf": False, "indent": 2, "top_level_keys_before": [], "top_level_keys_after": []},
        "counts": {
            "json_entries": len(items),
            "items": len(items),
            "photos": len(items),
            "videos": 0,
            "overlays": 0,
            "with_location": sum(1 for d in drafts if d["lat"] is not None),
            "with_url": 0,
            "with_tokens": 0,
            "media_files": len(items),
            "main_files": len(items),
            "main_files_skipped": 0,
        },
        "items": items,
    }
    write_truth(out / "truth.json", truth)
    ctx.log(f"{name}: {len(items)} timezone cases, home zone {HOME_ZONE}")
    return truth


def _build_simple(ctx: Context, name: str, srs_id: str, n: int, start: dt.datetime, end: dt.datetime, video_every: int | None) -> dict:
    rng = ctx.rng(name)
    out = ctx.out_dir / name
    reset_dir(out)
    drafts = []
    for i in range(n):
        drafts.append(
            {
                "id": new_id(rng),
                "when": random_instant(rng, start, end),
                "kind": "video" if video_every and i % video_every == 0 else "photo",
                "overlay": i % 10 in (2, 5, 8),
                "gps": i % 10 not in (0, 4, 7),
                "colour": (rng.randrange(40, 220), rng.randrange(40, 220), rng.randrange(40, 220)),
                "loc": random_location(rng),
            }
        )
    drafts.sort(key=lambda d: d["when"], reverse=True)
    # One place per UTC day so ordinal groups keep their times (see fixtures_m1).
    groups: dict[tuple[str, str], list[dict]] = {}
    for d in drafts:
        groups.setdefault((filename_date(d["when"]), d["kind"]), []).append(d)
    for members in groups.values():
        base = members[0]
        for m in members[1:]:
            m["gps"] = base["gps"]
            m["loc"] = (round(base["loc"][0] + rng.uniform(-0.0003, 0.0003), 5), round(base["loc"][1] + rng.uniform(-0.0003, 0.0003), 5))

    zipb = ZipBuilder()
    entries = []
    items = []
    skipped = 0
    for i, d in enumerate(drafts):
        ext = "mp4" if d["kind"] == "video" else "jpg"
        date_str = filename_date(d["when"])
        main_name = f"memories/{date_str}_{d['id']}-main.{ext}"
        overlay_name = f"memories/{date_str}_{d['id']}-overlay.png" if d["overlay"] else None
        loc_field = location_field(*d["loc"]) if d["gps"] else NO_LOCATION_FIELD
        entry = {"Date": export_date(d["when"]), "Media Type": media_type_field(d["kind"]), "Location": loc_field, "Download Link": "", "Media Download Url": "N/A"}
        entries.append(entry)
        main_skipped = None
        if d["kind"] == "video":
            clip = _tiny_mp4(ctx, i)
            if clip is None:
                main_skipped = "tiny_mp4_missing"
                skipped += 1
            else:
                zipb.add_media(main_name, clip)
        else:
            zipb.add_media(main_name, media.make_photo(d["id"], d["colour"], "jpg", entry["Date"], SMALL))
        if overlay_name:
            zipb.add_media(overlay_name, media.make_overlay(d["id"], f"caption {d['id'][:8]}", SMALL))
        # Chronological rank (oldest first) decides the free tier (FR-44).
        items.append(
            {
                "json_index": i,
                "id": d["id"],
                "date_utc": iso_utc(d["when"]),
                "media_type": d["kind"],
                "ext": ext,
                "main": None if main_skipped else main_name,
                "main_skipped": main_skipped,
                "overlay": overlay_name,
                "location": {"lat": d["loc"][0], "lon": d["loc"][1]} if d["gps"] else None,
                "day_group_size": len(groups[(date_str, d["kind"])]),
                "chronological_rank": None,
            }
        )
    for rank, item in enumerate(sorted(items, key=lambda it: it["date_utc"])):
        item["chronological_rank"] = rank
    zipb.add_header("index.html", INDEX_HTML)
    zipb.add_header("html/memories_history.html", MEMORIES_HTML)
    zipb.add_header("json/memories_history.json", json_bytes({"Saved Media": entries}))
    zip_path = out / part_name()
    order = zipb.write(zip_path, ctx.zip_order, rng)
    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": srs_id,
        "format": "2026b",
        "generator": _generator_block(ctx),
        "parts": [{"file": zip_path.name, "role": "primary", "entries": order}],
        "json": {"bom": False, "crlf": False, "indent": 2, "top_level_keys_before": [], "top_level_keys_after": []},
        "counts": {
            "json_entries": n,
            "items": n,
            "photos": sum(1 for d in drafts if d["kind"] == "photo"),
            "videos": sum(1 for d in drafts if d["kind"] == "video"),
            "overlays": sum(1 for d in drafts if d["overlay"]),
            "with_location": sum(1 for d in drafts if d["gps"]),
            "with_url": 0,
            "with_tokens": 0,
            "media_files": len(zipb.media),
            "main_files": n - skipped,
            "main_files_skipped": skipped,
            "free_tier_items": min(100, n),
        },
        "items": items,
    }
    write_truth(out / "truth.json", truth)
    ctx.log(f"{name}: {n} items, {truth['counts']['videos']} videos, {truth['counts']['overlays']} overlays")
    return truth


def build_unicode_paths(ctx: Context) -> dict:
    """F-12: a small export; the tests choose non-ASCII and over-long destinations."""
    return _build_simple(ctx, "unicode-paths", "F-12", 12, dt.datetime(2019, 1, 1, tzinfo=UTC), dt.datetime(2024, 12, 31, tzinfo=UTC), video_every=4)


def build_free_limit(ctx: Context) -> dict:
    """F-13: 250 items, so the free tier (first 100 chronologically) is visible."""
    return _build_simple(ctx, "free-limit", "F-13", 250, dt.datetime(2017, 1, 1, tzinfo=UTC), dt.datetime(2026, 6, 30, tzinfo=UTC), video_every=5)


BUILDERS_M2 = {
    "tz-edge": build_tz_edge,
    "unicode-paths": build_unicode_paths,
    "free-limit": build_free_limit,
}
