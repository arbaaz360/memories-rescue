"""M1 fixtures (SRS v0.2 section 11.2): F-04 multipart, F-05 missing-part,
F-06 truncated, F-14 zipslip.

F-04, F-05 and F-06 share one generated export (the "multipart family"):
1,000 items in the 2026b profile over four parts, every overlay in a different
part from its main, part 2 renamed to `mydata (1).zip`. F-05 drops part 3,
F-06 cuts part 2 at 70% and corrupts one entry of part 1. Video main files are
deterministic placeholders (an `ftyp` box plus random bytes), not real clips,
because these fixtures exercise indexing and matching only.

Expected match decisions in truth.json are computed here, in Python, from the
facts (day-group sizes and locations) with the rules of SRS section 3.6, so
the Rust matcher is checked against an independent reading of the spec.
"""

from __future__ import annotations

import datetime as dt
import hashlib
import io
import math
import zipfile

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

N_ITEMS = 1000
N_PARTS = 4
SMALL = (270, 480)
UNCERTAIN_DAYS = 3  # multi-item days whose locations are deliberately far apart
UNCERTAIN_RADIUS_M = 100.0


def _distance_m(a, b) -> float:
    r = 6_371_000.0
    lat1, lat2 = math.radians(a[0]), math.radians(b[0])
    dlat = lat2 - lat1
    dlon = math.radians(b[1] - a[1])
    h = math.sin(dlat / 2) ** 2 + math.cos(lat1) * math.cos(lat2) * math.sin(dlon / 2) ** 2
    return 2 * r * math.asin(math.sqrt(h))


_TINY_MP4_CACHE: dict[int, bytes | None] = {}


def _placeholder_video(rng, item_id: str) -> bytes:
    """A real tiny MP4 (from crates/mr-meta/tests/data, alternating moov-first
    and moov-last layouts) so the metadata writer can process it; a bare
    ftyp stub if those files are not present."""
    from pathlib import Path

    which = rng.randrange(2)
    if which not in _TINY_MP4_CACHE:
        root = Path(__file__).resolve().parents[2]
        name = ["tiny-moov-first.mp4", "tiny-moov-last.mp4"][which]
        path = root / "crates" / "mr-meta" / "tests" / "data" / name
        _TINY_MP4_CACHE[which] = path.read_bytes() if path.is_file() else None
    clip = _TINY_MP4_CACHE[which]
    if clip is not None:
        return clip
    head = bytes([0, 0, 0, 0x18]) + b"ftypmp42" + bytes(4) + b"mp42isom"
    body = rng.randbytes(4096 + rng.randrange(2048))
    return head + b"placeholder " + item_id.encode() + bytes(1) + body


def _generator_block(ctx: Context, extra: dict | None = None) -> dict:
    block = {
        "name": GENERATOR_NAME,
        "version": GENERATOR_VERSION,
        "seed": ctx.seed,
        "zip_order": ctx.zip_order,
        "json_order": "newest_first",
        "ffmpeg": {"available": False, "note": "not used: video mains are deterministic placeholders in this fixture"},
    }
    if extra:
        block.update(extra)
    return block


def _part_file_name(number: int) -> str:
    """Part 2 is renamed the way a browser renames a second download."""
    if number == 1:
        return part_name()
    if number == 2:
        return "mydata (1).zip"
    return part_name(f"-{number}")


def _expected(day_group_size: int, group_locations: list, has_location: bool, profile: str = "2026b") -> dict:
    """SRS v0.2 section 3.6 steps 1, 2 and 4 for an ordered export."""
    if profile == "2026a":
        return {
            "confidence": "exact",
            "time_precision": "second",
            "location_decision": "applied" if has_location else "none",
            "status": "matched",
        }
    if day_group_size == 1:
        return {
            "confidence": "ordinal",
            "time_precision": "second",
            "location_decision": "applied" if has_location else "none",
            "status": "matched",
        }
    if all(loc is None for loc in group_locations):
        return {"confidence": "ordinal", "time_precision": "second", "location_decision": "none", "status": "matched"}
    first = group_locations[0]
    consistent = first is not None and all(
        loc is not None and _distance_m(first, loc) <= UNCERTAIN_RADIUS_M for loc in group_locations
    )
    if consistent:
        return {"confidence": "ordinal", "time_precision": "second", "location_decision": "applied", "status": "matched"}
    return {
        "confidence": "ordinal",
        "time_precision": "day",
        "location_decision": "uncertain",
        "status": "location_uncertain",
    }


def build_multipart_family(ctx: Context) -> dict:
    """Generate the shared export once per run. Returns
    {parts: {number: {file, bytes, entries}}, items, truth_base}."""
    if "multipart" in ctx.cache:
        return ctx.cache["multipart"]
    rng = ctx.rng("multipart")
    start = dt.datetime(2017, 1, 1, tzinfo=UTC)
    end = dt.datetime(2026, 6, 30, tzinfo=UTC)
    drafts = []
    for i in range(N_ITEMS):
        drafts.append(
            {
                "id": new_id(rng),
                "when": random_instant(rng, start, end),
                "kind": "video" if i % 5 in (1, 3) else "photo",
                "overlay": i % 10 in (2, 5, 8),
                "gps": i % 10 not in (0, 4, 7),
                "png": i % 9 == 4,
                "colour": (rng.randrange(40, 220), rng.randrange(40, 220), rng.randrange(40, 220)),
                "loc": random_location(rng),
            }
        )
    drafts.sort(key=lambda d: d["when"], reverse=True)

    # Same-day, same-type items share a place (and a GPS yes/no), except on a
    # few designated days whose items are kilometres apart.
    groups: dict[tuple[str, str], list[dict]] = {}
    for d in drafts:
        groups.setdefault((filename_date(d["when"]), d["kind"]), []).append(d)
    uncertain_days: list[str] = []
    for key, members in sorted(groups.items()):
        if len(members) < 2:
            continue
        base = members[0]
        if len(uncertain_days) < UNCERTAIN_DAYS:
            uncertain_days.append(f"{key[0]}/{key[1]}")
            for m in members:
                m["gps"] = True
                m["loc"] = (round(base["loc"][0] + rng.uniform(0.02, 0.05) * rng.choice((-1, 1)), 5), round(base["loc"][1], 5))
            continue
        for m in members:
            m["gps"] = base["gps"]
            m["loc"] = (round(base["loc"][0] + rng.uniform(-0.0003, 0.0003), 5), round(base["loc"][1] + rng.uniform(-0.0003, 0.0003), 5))
    for d in drafts:
        d["day_group"] = f"{filename_date(d['when'])}/{d['kind']}"
        d["day_group_size"] = len(groups[(filename_date(d["when"]), d["kind"])])

    profile = ctx.multipart_profile
    uid = new_id(rng, upper=False)
    ts = int(dt.datetime(2026, 5, 1, tzinfo=UTC).timestamp())
    builders = {n: ZipBuilder() for n in range(1, N_PARTS + 1)}
    chunk = N_ITEMS // N_PARTS
    entries = []
    items = []
    url_variants = [
        {"Download Link": "", "Media Download Url": "N/A"},
        {"Download Link": "N/A", "Media Download Url": ""},
        {"Media Download Url": ""},
        {},
    ]
    for i, d in enumerate(drafts):
        ext = "mp4" if d["kind"] == "video" else ("png" if d["png"] else "jpg")
        date_str = filename_date(d["when"])
        main_part = min(i // chunk, N_PARTS - 1) + 1
        overlay_part = main_part % N_PARTS + 1
        main_name = f"memories/{date_str}_{d['id']}-main.{ext}"
        overlay_name = f"memories/{date_str}_{d['id']}-overlay.png" if d["overlay"] else None
        loc_field = location_field(*d["loc"]) if d["gps"] else NO_LOCATION_FIELD
        entry = {"Date": export_date(d["when"]), "Media Type": media_type_field(d["kind"]), "Location": loc_field}
        if profile == "2026a":
            sid = mid = d["id"]
            sig = hashlib.sha1(f"{d['id']}{ctx.seed}".encode()).hexdigest()
            entry["Download Link"] = f"https://app.example.invalid/dmd/memories?uid={uid}&sid={sid}&mid={mid}&ts={ts}&sig={sig}"
        else:
            sid = mid = None
            entry.update(url_variants[i % len(url_variants)])
        entries.append(entry)
        if d["kind"] == "video":
            builders[main_part].add_media(main_name, _placeholder_video(rng, d["id"]))
        else:
            builders[main_part].add_media(main_name, media.make_photo(d["id"], d["colour"], ext, entry["Date"], SMALL))
        if overlay_name:
            builders[overlay_part].add_media(overlay_name, media.make_overlay(d["id"], f"caption {d['id'][:8]}", SMALL))
        group_locs = [m["loc"] if m["gps"] else None for m in groups[(date_str, d["kind"])]]
        items.append(
            {
                "json_index": i,
                "id": d["id"],
                "date_utc": iso_utc(d["when"]),
                "date_field": entry["Date"],
                "filename_date": date_str,
                "media_type": d["kind"],
                "ext": ext,
                "main": {"part": main_part, "name": main_name, "zip_index": None},
                "overlay": {"part": overlay_part, "name": overlay_name, "zip_index": None} if overlay_name else None,
                "location": {"lat": d["loc"][0], "lon": d["loc"][1]} if d["gps"] else None,
                "sid": sid,
                "mid": mid,
                "day_group": d["day_group"],
                "day_group_size": d["day_group_size"],
                "expected": _expected(d["day_group_size"], group_locs, d["gps"], profile),
            }
        )
    builders[1].add_header("index.html", INDEX_HTML)
    builders[1].add_header("html/memories_history.html", MEMORIES_HTML)
    builders[1].add_header("json/memories_history.json", json_bytes({"Saved Media": entries}))

    parts = {}
    for n, b in builders.items():
        data, order = b.to_bytes(ctx.zip_order, rng)
        parts[n] = {"number": n, "file": _part_file_name(n), "renamed": n == 2, "role": "primary" if n == 1 else "extra", "bytes": data, "entries": order}
        position = {name: idx for idx, name in enumerate(order)}
        for item in items:
            if item["main"]["part"] == n:
                item["main"]["zip_index"] = position[item["main"]["name"]]
            if item["overlay"] and item["overlay"]["part"] == n:
                item["overlay"]["zip_index"] = position[item["overlay"]["name"]]

    counts = {
        "json_entries": N_ITEMS,
        "items": N_ITEMS,
        "photos": sum(1 for d in drafts if d["kind"] == "photo"),
        "videos": sum(1 for d in drafts if d["kind"] == "video"),
        "overlays": sum(1 for d in drafts if d["overlay"]),
        "with_location": sum(1 for d in drafts if d["gps"]),
        "with_url": N_ITEMS if profile == "2026a" else 0,
        "with_tokens": N_ITEMS if profile == "2026a" else 0,
        "media_files": sum(len(b.media) for b in builders.values()),
        "main_files": N_ITEMS,
        "main_files_skipped": 0,
        "multi_item_groups": sum(1 for g in groups.values() if len(g) > 1),
        "items_in_multi_item_groups": sum(len(g) for g in groups.values() if len(g) > 1),
        "expected_location_uncertain": sum(1 for it in items if it["expected"]["status"] == "location_uncertain"),
        "expected_second_precision": sum(1 for it in items if it["expected"]["time_precision"] == "second"),
    }
    family = {"parts": parts, "items": items, "counts": counts, "uncertain_days": uncertain_days, "profile": profile}
    ctx.cache["multipart"] = family
    return family


def _write_family(ctx: Context, name: str, srs_id: str, family: dict, parts_out: dict[int, bytes], extra: dict) -> dict:
    out = ctx.out_dir / name
    reset_dir(out)
    parts_truth = []
    for n in sorted(family["parts"]):
        p = family["parts"][n]
        present = n in parts_out
        if present:
            (out / p["file"]).write_bytes(parts_out[n])
        parts_truth.append({"number": n, "file": p["file"], "role": p["role"], "renamed": p["renamed"], "present": present, "entries": p["entries"]})
    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": srs_id,
        "format": family["profile"],
        "generator": _generator_block(ctx),
        "parts": parts_truth,
        "json": {"bom": False, "crlf": False, "indent": 2, "top_level_keys_before": [], "top_level_keys_after": []},
        "counts": dict(family["counts"]),
        "uncertain_days": family["uncertain_days"],
        "items": [dict(it) for it in family["items"]],
    }
    truth.update(extra)
    return truth


def build_multipart(ctx: Context) -> dict:
    family = build_multipart_family(ctx)
    truth = _write_family(ctx, "multipart", "F-04", family, {n: p["bytes"] for n, p in family["parts"].items()}, {})
    write_truth(ctx.out_dir / "multipart" / "truth.json", truth)
    ctx.log(f"multipart: {N_ITEMS} items over {N_PARTS} parts, part 2 renamed, {truth['counts']['overlays']} overlays in other parts")
    return truth


def build_missing_part(ctx: Context) -> dict:
    family = build_multipart_family(ctx)
    missing = 3
    truth = _write_family(ctx, "missing-part", "F-05", family, {n: p["bytes"] for n, p in family["parts"].items() if n != missing}, {})
    lost_mains = 0
    lost_overlays = 0
    orphan_overlays = []
    for it in truth["items"]:
        if it["main"]["part"] == missing:
            it["expected"] = {"status": "missing_media"}
            lost_mains += 1
            if it["overlay"]:
                orphan_overlays.append(it["overlay"]["name"])
        elif it["overlay"] and it["overlay"]["part"] == missing:
            it["overlay_present"] = False
            lost_overlays += 1
    truth["missing_part"] = {"number": missing, "lost_mains": lost_mains, "lost_overlays": lost_overlays, "orphan_overlays": orphan_overlays}
    truth["counts"]["main_files"] = N_ITEMS - lost_mains
    truth["counts"]["media_files"] = sum(len(p["entries"]) - (3 if n == 1 else 0) for n, p in family["parts"].items() if n != missing)
    truth["counts"]["overlays"] = truth["counts"]["overlays"] - lost_overlays
    write_truth(ctx.out_dir / "missing-part" / "truth.json", truth)
    ctx.log(f"missing-part: part {missing} absent, {lost_mains} mains and {lost_overlays} overlays lost, {len(orphan_overlays)} orphan overlays")
    return truth


def build_truncated(ctx: Context) -> dict:
    family = build_multipart_family(ctx)
    parts_out = {n: p["bytes"] for n, p in family["parts"].items()}

    # Part 2 cut at 70%.
    full = parts_out[2]
    cut = int(len(full) * 0.7)
    parts_out[2] = full[:cut]
    readable, partial, lost = [], None, []
    with zipfile.ZipFile(io.BytesIO(full)) as zf:
        for info in zf.infolist():
            local_header_len = 30 + len(info.filename.encode("utf-8")) + len(info.extra)
            end = info.header_offset + local_header_len + info.compress_size
            if end <= cut:
                readable.append(info.filename)
            elif info.header_offset + local_header_len <= cut and partial is None:
                partial = info.filename
            else:
                lost.append(info.filename)

    # One CRC-corrupt entry in part 1: flip a byte inside a stored jpg.
    part1 = bytearray(parts_out[1])
    corrupt = None
    with zipfile.ZipFile(io.BytesIO(bytes(part1))) as zf:
        for info in zf.infolist():
            if info.filename.endswith("-main.jpg") and info.compress_type == zipfile.ZIP_STORED and info.compress_size > 400:
                data_start = info.header_offset + 30 + len(info.filename.encode("utf-8")) + len(info.extra)
                part1[data_start + 200] ^= 0xFF
                corrupt = {"name": info.filename, "crc32": info.CRC, "flipped_offset": data_start + 200}
                break
    parts_out[1] = bytes(part1)

    truth = _write_family(ctx, "truncated", "F-06", family, parts_out, {})
    lost_set = set(lost) | ({partial} if partial else set())
    lost_mains = 0
    lost_overlays = 0
    for it in truth["items"]:
        if it["main"]["part"] == 2 and it["main"]["name"] in lost_set:
            it["expected"] = {"status": "missing_media"}
            lost_mains += 1
        if it["overlay"] and it["overlay"]["part"] == 2 and it["overlay"]["name"] in lost_set:
            it["overlay_present"] = False
            lost_overlays += 1
        if corrupt and it["main"]["name"] == corrupt["name"]:
            it["crc_corrupt"] = True
    truth["truncated_part"] = {"number": 2, "cut_bytes": cut, "full_bytes": len(full), "readable": readable, "partial": partial, "lost": lost}
    truth["corrupt_entry"] = {"part": 1, **corrupt} if corrupt else None
    truth["counts"]["main_files"] = N_ITEMS - lost_mains
    truth["counts"]["overlays"] = truth["counts"]["overlays"] - lost_overlays
    truth["counts"]["media_files"] = truth["counts"]["media_files"] - len(lost_set)
    write_truth(ctx.out_dir / "truncated" / "truth.json", truth)
    ctx.log(f"truncated: part 2 cut at {cut}/{len(full)} bytes, {len(readable)} readable, 1 partial, {len(lost)} lost; corrupt entry in part 1")
    return truth


def build_zipslip(ctx: Context) -> dict:
    """F-14: entry names that must be refused (NFR-PRV-07) next to two good items."""
    name = "zipslip"
    rng = ctx.rng(name)
    out = ctx.out_dir / name
    reset_dir(out)
    when = dt.datetime(2021, 12, 25, 18, 42, 7, tzinfo=UTC)
    good_id = new_id(rng)
    other_id = new_id(rng)
    entries_json = [
        {"Date": export_date(when), "Media Type": "Image", "Location": "Latitude, Longitude: 51.50735, -0.12776", "Download Link": "", "Media Download Url": "N/A"},
        {"Date": export_date(when - dt.timedelta(days=1)), "Media Type": "Image", "Location": NO_LOCATION_FIELD},
    ]
    good_main = f"memories/{filename_date(when)}_{good_id}-main.jpg"
    other_main = f"memories/{filename_date(when - dt.timedelta(days=1))}_{other_id}-main.jpg"
    photo = media.make_photo(good_id, (200, 80, 80), "jpg", export_date(when), SMALL)
    evil = b"not an image; must never be written anywhere\n"
    entries = [
        ("index.html", INDEX_HTML, None, False, None),
        ("json/memories_history.json", json_bytes({"Saved Media": entries_json}), None, False, None),
        (good_main, photo, None, False, None),
        ("../2021-12-25_" + good_id + "-main.jpg", evil, None, True, "parent_traversal"),
        ("memories/../../2021-12-25_" + good_id + "-overlay.png", evil, None, True, "parent_traversal"),
        ("/tmp/2021-12-25_" + good_id + "-main.jpg", evil, None, True, "absolute_path"),
        ("C:\\Users\\victim\\2021-12-25_" + good_id + "-main.jpg", evil, None, True, "drive_letter"),
        ("memories/2021-12-25_" + good_id + "-link-main.jpg", b"../../etc/passwd", 0o120777 << 16, True, "symlink"),
        ("memories/..\\..\\2021-12-25_" + good_id + "-main.jpg", evil, None, True, "parent_traversal"),
        (other_main, media.make_photo(other_id, (80, 80, 200), "jpg", export_date(when), SMALL), None, False, None),
    ]
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", compression=zipfile.ZIP_STORED) as zf:
        for ename, data, attr, _refused, _reason in entries:
            info = zipfile.ZipInfo("placeholder", date_time=(2026, 10, 1, 0, 0, 0))
            info.filename = ename  # bypass ZipInfo separator normalisation: the raw name is the point
            info.compress_type = zipfile.ZIP_STORED
            info.create_system = 3  # Unix, so the mode bits (symlink) are meaningful
            info.external_attr = attr if attr is not None else (0o644 << 16)
            zf.writestr(info, data)
    zip_path = out / part_name()
    zip_path.write_bytes(buf.getvalue())
    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": "F-14",
        "format": "2026b",
        "generator": _generator_block(ctx),
        "parts": [{"file": zip_path.name, "role": "primary", "entries": [e[0] for e in entries]}],
        "json": {"bom": False, "crlf": False, "indent": 2, "top_level_keys_before": [], "top_level_keys_after": []},
        "counts": {
            "json_entries": 2,
            "items": 2,
            "photos": 2,
            "videos": 0,
            "overlays": 0,
            "with_location": 1,
            "with_url": 0,
            "with_tokens": 0,
            "media_files": 2,
            "main_files": 2,
            "main_files_skipped": 0,
            "refused_entries": sum(1 for e in entries if e[3]),
        },
        "entries": [{"name": e[0], "refused": e[3], "reason": e[4]} for e in entries],
        "items": [
            {"json_index": 0, "id": good_id, "date_utc": iso_utc(when), "media_type": "photo", "main": good_main},
            {"json_index": 1, "id": other_id, "date_utc": iso_utc(when - dt.timedelta(days=1)), "media_type": "photo", "main": other_main},
        ],
    }
    write_truth(out / "truth.json", truth)
    ctx.log(f"zipslip: {truth['counts']['refused_entries']} entries to refuse, 2 good items")
    return truth


BUILDERS_M1 = {
    "multipart": build_multipart,
    "missing-part": build_missing_part,
    "truncated": build_truncated,
    "zipslip": build_zipslip,
}
