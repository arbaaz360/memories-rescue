"""The fixture definitions (SRS v0.2 §11.2): F-01 small-2026b, F-15 small-2026a,
F-02 small-legacy, F-10 weird-json. Each writes <out>/<name>/<zip> and <out>/<name>/truth.json.

truth.json layout (schema 1):

  schema, fixture, srs_id, format, generator{name, version, seed, zip_order,
  json_order, ffmpeg{available, ...}}, parts[{file, role, entries[]}],
  json{bom, crlf, indent, top_level_keys_before, top_level_keys_after},
  counts{...}, items[...]

Item fields depend on the fixture; see each builder. Every expected value is
written explicitly here, not derived through a second implementation of the
parser, so the Rust tests compare against an independent statement of intent.
"""

from __future__ import annotations

import datetime as dt
import hashlib

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


def _generator_block(ctx: Context) -> dict:
    ff = ctx.ffmpeg
    return {
        "name": GENERATOR_NAME,
        "version": GENERATOR_VERSION,
        "seed": ctx.seed,
        "zip_order": ctx.zip_order,
        "json_order": "newest_first",
        "ffmpeg": (
            {"available": True, "version": ff.version, "encoder": ff.encoder}
            if ff
            else {"available": False, "note": "ffmpeg not found or disabled with --no-video: video main files are skipped (main=null, main_skipped='ffmpeg_missing'); JSON entries are still written"}
        ),
    }


def _colour(rng) -> tuple[int, int, int]:
    return (rng.randrange(40, 220), rng.randrange(40, 220), rng.randrange(40, 220))


# ---------------------------------------------------------------------------
# F-01 small-2026
# ---------------------------------------------------------------------------

def build_small_2026(ctx: Context, profile: str) -> dict:
    """50 items, 1 part, 30 photos / 20 videos, 15 overlays (30%), 35 with GPS
    (70%), media bundled under memories/. Two profiles (SRS v0.2 section 3.3):

    * ``2026b`` (F-01 ``small-2026b``): URL fields empty, ``N/A`` or absent.
    * ``2026a`` (F-15 ``small-2026a``): ``Download Link`` present on a synthetic
      host, carrying ``sid`` and ``mid`` equal to the filename UUID.
    """
    if profile == "2026b":
        name, srs_id = "small-2026b", "F-01"
    elif profile == "2026a":
        name, srs_id = "small-2026a", "F-15"
    else:
        raise ValueError(profile)
    rng = ctx.rng("small-2026")  # same items in both profiles, only the URL fields differ
    out = ctx.out_dir / name
    reset_dir(out)

    n = 50
    start = dt.datetime(2017, 1, 1, tzinfo=UTC)
    end = dt.datetime(2026, 6, 30, tzinfo=UTC)
    drafts = []
    for i in range(n):
        drafts.append(
            {
                "id": new_id(rng),
                "when": random_instant(rng, start, end),
                "kind": "video" if i % 5 in (1, 3) else "photo",  # 20 videos
                "overlay": i % 10 in (2, 5, 8),  # 15 overlays
                "gps": i % 10 not in (0, 4, 7),  # 35 with location
                "png": i % 9 == 4,  # a few PNG mains among the photos
                "colour": _colour(rng),
                "loc": random_location(rng),
            }
        )
    drafts.sort(key=lambda d: d["when"], reverse=True)  # newest first, like the export

    url_variants_2026b = [
        {"Download Link": "", "Media Download Url": "N/A"},
        {"Download Link": "N/A", "Media Download Url": ""},
        {"Media Download Url": ""},
        {"Download Link": ""},
        {},
    ]
    uid = new_id(rng, upper=False)
    ts = int(dt.datetime(2026, 5, 1, tzinfo=UTC).timestamp())

    zipb = ZipBuilder()
    entries = []
    items = []
    skipped = 0
    for i, d in enumerate(drafts):
        ext = "mp4" if d["kind"] == "video" else ("png" if d["png"] else "jpg")
        date_str = filename_date(d["when"])
        main_name = f"memories/{date_str}_{d['id']}-main.{ext}"
        overlay_name = f"memories/{date_str}_{d['id']}-overlay.png" if d["overlay"] else None
        loc_field = location_field(*d["loc"]) if d["gps"] else NO_LOCATION_FIELD

        entry = {
            "Date": export_date(d["when"]),
            "Media Type": media_type_field(d["kind"]),
            "Location": loc_field,
        }
        if profile == "2026b":
            entry.update(url_variants_2026b[i % len(url_variants_2026b)])
            sid = mid = None
        else:
            sid = mid = d["id"]
            sig = hashlib.sha1(f"{d['id']}{ctx.seed}".encode()).hexdigest()
            entry["Download Link"] = (
                f"https://app.example.invalid/dmd/memories?uid={uid}&sid={sid}&mid={mid}&ts={ts}&sig={sig}"
            )
            if i % 2 == 0:
                entry["Media Download Url"] = "N/A"
        entries.append(entry)

        main_skipped = None
        if d["kind"] == "video":
            if ctx.ffmpeg:
                zipb.add_media(main_name, media.make_video(ctx.ffmpeg, d["id"]))
            else:
                main_skipped = "ffmpeg_missing"
                skipped += 1
        else:
            zipb.add_media(main_name, media.make_photo(d["id"], d["colour"], ext, export_date(d["when"])))
        if overlay_name:
            zipb.add_media(overlay_name, media.make_overlay(d["id"], f"caption for {d['id'][:8]}"))

        items.append(
            {
                "json_index": i,
                "id": d["id"],
                "date_utc": iso_utc(d["when"]),
                "date_field": entry["Date"],
                "filename_date": date_str,
                "media_type": d["kind"],
                "media_type_field": entry["Media Type"],
                "ext": ext,
                "main": None if main_skipped else main_name,
                "main_skipped": main_skipped,
                "overlay": overlay_name,
                "location": {"lat": d["loc"][0], "lon": d["loc"][1]} if d["gps"] else None,
                "location_field": loc_field,
                "download_link_field": entry.get("Download Link"),
                "media_download_url_field": entry.get("Media Download Url"),
                "sid": sid,
                "mid": mid,
                "zip_index_main": None,
                "zip_index_overlay": None,
            }
        )

    zipb.add_header("index.html", INDEX_HTML)
    zipb.add_header("html/memories_history.html", MEMORIES_HTML)
    zipb.add_header("json/memories_history.json", json_bytes({"Saved Media": entries}))
    zip_path = out / part_name()
    order = zipb.write(zip_path, ctx.zip_order, rng)
    position = {name_: idx for idx, name_ in enumerate(order)}
    for item in items:
        if item["main"]:
            item["zip_index_main"] = position[item["main"]]
        if item["overlay"]:
            item["zip_index_overlay"] = position[item["overlay"]]

    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": srs_id,
        "format": profile,
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
            "with_url": n if profile == "2026a" else 0,
            "with_tokens": n if profile == "2026a" else 0,
            "media_files": len(zipb.media),
            "main_files": len(zipb.media) - sum(1 for d in drafts if d["overlay"]),
            "main_files_skipped": skipped,
        },
        "items": items,
    }
    write_truth(out / "truth.json", truth)
    ctx.log(f"{name}: {zip_path.name} with {len(order)} entries, {skipped} video mains skipped")
    return truth


def build_small_2026b(ctx: Context) -> dict:
    return build_small_2026(ctx, "2026b")


def build_small_2026a(ctx: Context) -> dict:
    return build_small_2026(ctx, "2026a")


# ---------------------------------------------------------------------------
# F-02 small-legacy
# ---------------------------------------------------------------------------

def build_small_legacy(ctx: Context) -> dict:
    """50 JSON entries in the legacy shape: signed, long-expired dummy URLs on
    a reserved .invalid host, and no media bundled at all."""
    name = "small-legacy"
    rng = ctx.rng(name)
    out = ctx.out_dir / name
    reset_dir(out)

    n = 50
    start = dt.datetime(2016, 1, 1, tzinfo=UTC)
    end = dt.datetime(2020, 12, 31, tzinfo=UTC)
    uid = new_id(rng, upper=False)
    drafts = []
    for i in range(n):
        drafts.append(
            {
                "sid": new_id(rng, upper=False),
                "mid": new_id(rng, upper=False),
                "when": random_instant(rng, start, end),
                "kind": "video" if i % 5 in (1, 3) else "photo",
                "gps": i % 10 not in (0, 4, 7),
                "loc": random_location(rng),
            }
        )
    drafts.sort(key=lambda d: d["when"], reverse=True)

    expired_ts = int(dt.datetime(2021, 3, 1, tzinfo=UTC).timestamp())
    entries = []
    items = []
    for i, d in enumerate(drafts):
        ext = "mp4" if d["kind"] == "video" else "jpg"
        sig = hashlib.sha1(f"{d['sid']}{d['mid']}{ctx.seed}".encode()).hexdigest()
        download_link = (
            f"https://app.example.invalid/dmd/memories?uid={uid}&sid={d['sid']}&mid={d['mid']}"
            f"&ts={expired_ts}&sig={sig}"
        )
        media_url = (
            f"https://cdn.example.invalid/dmd/memories/{d['mid']}.{ext}"
            f"?Expires={expired_ts}&Signature={sig[:20]}&Key-Pair-Id=EXAMPLEKEY"
        )
        loc_field = location_field(*d["loc"]) if d["gps"] else NO_LOCATION_FIELD
        entry = {
            "Date": export_date(d["when"]),
            "Media Type": media_type_field(d["kind"]),
            "Location": loc_field,
            "Download Link": download_link,
            "Media Download Url": media_url,
        }
        entries.append(entry)
        items.append(
            {
                "json_index": i,
                "date_utc": iso_utc(d["when"]),
                "date_field": entry["Date"],
                "media_type": d["kind"],
                "media_type_field": entry["Media Type"],
                "location": {"lat": d["loc"][0], "lon": d["loc"][1]} if d["gps"] else None,
                "location_field": loc_field,
                "download_link": download_link,
                "media_download_url": media_url,
                "download_link_redacted": "https://app.example.invalid/dmd/memories?<redacted>",
                "sid": d["sid"],
                "mid": d["mid"],
            }
        )

    zipb = ZipBuilder()
    zipb.add_header("index.html", INDEX_HTML)
    zipb.add_header("html/memories_history.html", MEMORIES_HTML)
    zipb.add_header("json/memories_history.json", json_bytes({"Saved Media": entries}))
    zip_path = out / part_name()
    order = zipb.write(zip_path, ctx.zip_order, rng)

    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": "F-02",
        "format": "legacy",
        "generator": _generator_block(ctx),
        "parts": [{"file": zip_path.name, "role": "primary", "entries": order}],
        "json": {"bom": False, "crlf": False, "indent": 2, "top_level_keys_before": [], "top_level_keys_after": []},
        "counts": {
            "json_entries": n,
            "items": n,
            "photos": sum(1 for d in drafts if d["kind"] == "photo"),
            "videos": sum(1 for d in drafts if d["kind"] == "video"),
            "overlays": 0,
            "with_location": sum(1 for d in drafts if d["gps"]),
            "with_url": n,
            "media_files": 0,
            "main_files": 0,
            "main_files_skipped": 0,
        },
        "items": items,
    }
    write_truth(out / "truth.json", truth)
    ctx.log(f"{name}: {zip_path.name} with {len(order)} entries, no media")
    return truth


# ---------------------------------------------------------------------------
# F-10 weird-json
# ---------------------------------------------------------------------------

def _case(name: str, fields: dict, **expected) -> dict:
    exp = {
        "date_utc": None,
        "date_kind": "missing",  # instant | day_only | unparseable | missing
        "media_type": "unknown",  # photo | video | unknown
        "location": None,
        "location_reason": "missing",  # found | missing | unrecognised | null_island | out_of_range
        "download_link": None,
        "media_download_url": None,
        "sid": None,
        "mid": None,
        "unknown_fields": [],
        "non_string_fields": [],
    }
    exp.update(expected)
    return {"case": name, "fields": fields, "expected": exp}


def build_weird_json(ctx: Context) -> dict:
    """Every tolerance in FR-10 and §3.5 in one file: BOM, CRLF, tab indent,
    unknown top-level keys before and after the array, unknown entry fields,
    missing/odd Location, 0.0 and out-of-range coordinates, alternate date
    formats, non-string values, escaped text. No media."""
    name = "weird-json"
    rng = ctx.rng(name)
    out = ctx.out_dir / name
    reset_dir(out)

    london = {"lat": 51.50735, "lon": -0.12776}
    london_field = "Latitude, Longitude: 51.50735, -0.12776"
    sydney = {"lat": -33.8688, "lon": 151.2093}
    base = "2021-12-25T18:42:07Z"
    sid = "3f2a9c1e-5b7d-4e1f-9a2b-0c3d4e5f6a7b"
    mid = "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"
    link = f"https://app.example.invalid/dmd/memories?uid=u&sid={sid}&mid={mid}&ts=1&sig=abc"
    e_acute = chr(0xE9)
    grinning = chr(0x1F600)

    cases = [
        _case(
            "canonical_with_unknown_fields",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": london_field,
             "Download Link": "", "Media Download Url": "N/A", "Favourite": True, "Caption": "hello"},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
            unknown_fields=["Favourite", "Caption"],
        ),
        _case(
            "iso_z",
            {"Date": "2021-12-25T18:42:07Z", "Media Type": "Video", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="video", location=london, location_reason="found",
        ),
        _case(
            "iso_positive_offset",
            {"Date": "2021-12-25T19:42:07+01:00", "Media Type": "Image", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "iso_negative_offset_no_colon",
            {"Date": "2021-12-25T13:42:07-0500", "Media Type": "Image", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "iso_fractional_seconds",
            {"Date": "2021-12-25T18:42:07.250Z", "Media Type": "Image", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "space_separator_no_zone",
            {"Date": "2021-12-25 18:42:07", "Media Type": "Image", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "gmt_suffix",
            {"Date": "2021-12-25 18:42:07 GMT", "Media Type": "Image", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "date_only",
            {"Date": "2021-12-25", "Media Type": "Image", "Location": london_field},
            date_utc=None, date_kind="day_only", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "date_garbage",
            {"Date": "yesterday afternoon", "Media Type": "Image", "Location": london_field},
            date_kind="unparseable", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "date_missing",
            {"Media Type": "Image", "Location": london_field},
            date_kind="missing", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "date_non_string",
            {"Date": 1640457727, "Media Type": "Image", "Location": london_field},
            date_kind="missing", media_type="photo", location=london, location_reason="found",
            non_string_fields=["Date"],
        ),
        _case(
            "location_missing",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image"},
            date_utc=base, date_kind="instant", media_type="photo", location_reason="missing",
        ),
        _case(
            "location_empty",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": ""},
            date_utc=base, date_kind="instant", media_type="photo", location_reason="missing",
        ),
        _case(
            "location_null_island",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": NO_LOCATION_FIELD},
            date_utc=base, date_kind="instant", media_type="photo", location_reason="null_island",
        ),
        _case(
            "location_lat_out_of_range",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": "Latitude, Longitude: 91.0, 10.0"},
            date_utc=base, date_kind="instant", media_type="photo", location_reason="out_of_range",
        ),
        _case(
            "location_lon_out_of_range",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": "Latitude, Longitude: 10.0, -181.5"},
            date_utc=base, date_kind="instant", media_type="photo", location_reason="out_of_range",
        ),
        _case(
            "location_unrecognised_text",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": "Somewhere nice"},
            date_utc=base, date_kind="instant", media_type="photo", location_reason="unrecognised",
        ),
        _case(
            "location_non_string",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": 0},
            date_utc=base, date_kind="instant", media_type="photo", location_reason="missing",
            non_string_fields=["Location"],
        ),
        _case(
            "location_southern_eastern",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Video", "Location": "Latitude, Longitude: -33.8688, 151.2093"},
            date_utc=base, date_kind="instant", media_type="video", location=sydney, location_reason="found",
        ),
        _case(
            "media_type_photo_word",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Photo", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "media_type_lowercase",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "video", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="video", location=london, location_reason="found",
        ),
        _case(
            "media_type_unknown_word",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Live Photo", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="unknown", location=london, location_reason="found",
        ),
        _case(
            "media_type_missing",
            {"Date": "2021-12-25 18:42:07 UTC", "Location": london_field},
            date_utc=base, date_kind="instant", media_type="unknown", location=london, location_reason="found",
        ),
        _case(
            "urls_na_and_missing",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": london_field, "Download Link": "N/A"},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "urls_legacy_tokens",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Video", "Location": london_field,
             "Download Link": link, "Media Download Url": "https://cdn.example.invalid/m/x.mp4?Expires=1&Signature=s"},
            date_utc=base, date_kind="instant", media_type="video", location=london, location_reason="found",
            download_link=link, media_download_url="https://cdn.example.invalid/m/x.mp4?Expires=1&Signature=s",
            sid=sid, mid=mid,
        ),
        _case(
            "url_non_string_and_null",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": london_field,
             "Download Link": 42, "Media Download Url": None},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
            non_string_fields=["Download Link"],
        ),
        _case(
            "nested_unknown_structures",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": london_field,
             "Extra": {"a": [1, 2, {"b": None, "c": "]}"}], "d": {"e": []}}, "Tags": ["x", "y"]},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
            unknown_fields=["Extra", "Tags"],
        ),
        _case(
            "escaped_text_with_brackets_and_unicode",
            {"Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image", "Location": london_field,
             "Caption": 'quote " backslash \\ brackets ]} caf' + e_acute + " " + grinning},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
            unknown_fields=["Caption"],
        ),
        _case(
            "key_casing_and_padding",
            {"date": "2021-12-25 18:42:07 UTC", " Media Type ": "Image", "LOCATION": london_field},
            date_utc=base, date_kind="instant", media_type="photo", location=london, location_reason="found",
        ),
        _case(
            "completely_empty_entry",
            {},
            date_kind="missing", media_type="unknown", location_reason="missing",
        ),
    ]
    rng.shuffle(cases)  # order must not matter
    for i, c in enumerate(cases):
        c["json_index"] = i

    document = {
        "Export Info": {"Version": 2, "Generated": "2026-10-01", "Note": "synthetic fixture, keys before the array"},
        "Count": len(cases),
        "Saved Media": [c["fields"] for c in cases],
        "Trailer": [1, 2, 3],
    }
    payload = json_bytes(document, bom=True, crlf=True, indent="\t")

    zipb = ZipBuilder()
    zipb.add_header("index.html", INDEX_HTML)
    zipb.add_header("json/memories_history.json", payload)
    zip_path = out / part_name()
    order = zipb.write(zip_path, ctx.zip_order, rng)

    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": "F-10",
        "format": "2026",
        "generator": _generator_block(ctx),
        "parts": [{"file": zip_path.name, "role": "primary", "entries": order}],
        "json": {
            "bom": True,
            "crlf": True,
            "indent": "tab",
            "top_level_keys_before": ["Export Info", "Count"],
            "top_level_keys_after": ["Trailer"],
        },
        "counts": {
            "json_entries": len(cases),
            "items": len(cases),
            "photos": sum(1 for c in cases if c["expected"]["media_type"] == "photo"),
            "videos": sum(1 for c in cases if c["expected"]["media_type"] == "video"),
            "unknown_type": sum(1 for c in cases if c["expected"]["media_type"] == "unknown"),
            "overlays": 0,
            "with_location": sum(1 for c in cases if c["expected"]["location"] is not None),
            "with_instant": sum(1 for c in cases if c["expected"]["date_kind"] == "instant"),
            "with_url": sum(1 for c in cases if c["expected"]["download_link"] or c["expected"]["media_download_url"]),
            "media_files": 0,
            "main_files": 0,
            "main_files_skipped": 0,
        },
        "items": cases,
    }
    write_truth(out / "truth.json", truth)
    ctx.log(f"{name}: {zip_path.name} with {len(cases)} entries (BOM, CRLF, tabs)")
    return truth


BUILDERS = {
    "small-2026b": build_small_2026b,
    "small-2026a": build_small_2026a,
    "small-legacy": build_small_legacy,
    "weird-json": build_weird_json,
}
