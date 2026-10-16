"""Scale fixtures for the performance and soak runs (docs/perf/README.md).

Not part of --all: build them on purpose, into a scratch folder with room:

    python tools/make_fixture --fixture scale-h01 --out <scratch>

| Fixture | For | Contents |
|---|---|---|
| scale-index | FR-06 AC1 | 50,000 entries over 5 parts, 30% overlays; placeholder media bytes (indexing never reads them) |
| scale-h01 | H-01, H-02, H-03, NFR-PERF-06 | 50,000 items over 12 parts: 40,000 photos, 10,000 videos, 30% overlays; small real media (144x256 JPEG, 2 s 144x256 H.264 clips, faststart and moov-last) |
| scale-h05 | H-05 | 200,000 JSON entries, no media |
| perf-photos | NFR-PERF-03 | 1,000 phone-sized photos (1080x1920 JPEG, about 1 MB) |
| perf-videos | NFR-PERF-03 | 200 phone-sized clips (10 s 1080x1920 H.264, about 4.6 MB, moov first and last alternating) |

Every export is newest-first JSON with ZIP order following it. The scale
exports are ``2026b`` (ordinal matching at scale): all items of a day share
a place, and either all or none of a day's items have GPS, so no day group
is location-uncertain and every item gets an exact time. The perf exports
are ``2026a``. Media blobs are reused across items (a handful of variants),
which keeps generation fast; the engine writes different metadata into each
output, so outputs still differ. Parts are written straight to disk.

truth.json carries the counts only (no per-item list).
"""

from __future__ import annotations

import datetime as dt
import io
import random
import subprocess
import tempfile
import zipfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

from common import (
    CITIES,
    GENERATOR_NAME,
    GENERATOR_VERSION,
    INDEX_HTML,
    MEMORIES_HTML,
    NO_LOCATION_FIELD,
    TRUTH_SCHEMA,
    UTC,
    ZIP_ENTRY_TIME,
    Context,
    export_date,
    filename_date,
    json_bytes,
    location_field,
    media_type_field,
    new_id,
    part_name,
    reset_dir,
    write_truth,
)

SMALL = (144, 256)


def _jpeg(size, colour, label: str, quality: int = 85) -> bytes:
    img = Image.new("RGB", size, colour)
    ImageDraw.Draw(img).text((6, 6), label, fill=(255, 255, 255))
    buf = io.BytesIO()
    img.save(buf, "JPEG", quality=quality)
    return buf.getvalue()


def _phone_jpeg(rng: random.Random) -> bytes:
    small = Image.frombytes("RGB", (270, 480), bytes(rng.getrandbits(8) for _ in range(270 * 480 * 3)))
    img = small.resize((1080, 1920), Image.BICUBIC).filter(ImageFilter.GaussianBlur(1))
    buf = io.BytesIO()
    img.save(buf, "JPEG", quality=90)
    return buf.getvalue()


def _overlay(size) -> bytes:
    w, h = size
    img = Image.new("RGBA", size, (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    d.rectangle([0, int(h * 0.62), w, int(h * 0.62) + h // 10], fill=(0, 0, 0, 150))
    d.ellipse([w - w // 3, h // 20, w - w // 20, h // 20 + w // 4], fill=(255, 196, 0, 230))
    buf = io.BytesIO()
    img.save(buf, "PNG", optimize=True)
    return buf.getvalue()


def _clip(ctx: Context, size, seconds: int, bitrate: str | None, faststart: bool) -> bytes:
    if not ctx.ffmpeg:
        raise SystemExit("scale fixtures need ffmpeg with an H.264 encoder (see tools/README.md)")
    w, h = size
    with tempfile.TemporaryDirectory(prefix="mr-scale-") as tmp:
        out = Path(tmp) / "clip.mp4"
        cmd = [
            ctx.ffmpeg.path, "-y", "-hide_banner", "-loglevel", "error", "-nostdin",
            "-f", "lavfi", "-i", f"testsrc2=size={w}x{h}:rate=30:duration={seconds}",
            "-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=48000:duration={seconds}",
            "-c:v", ctx.ffmpeg.encoder,
        ]
        if ctx.ffmpeg.encoder == "libx264":
            cmd += ["-preset", "veryfast"] + (["-b:v", bitrate] if bitrate else ["-crf", "30"])
        cmd += ["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "64k", "-shortest"]
        if faststart:
            cmd += ["-movflags", "+faststart"]
        cmd.append(str(out))
        subprocess.run(cmd, check=True, capture_output=True, timeout=600)
        return out.read_bytes()


def _write_part(path: Path, entries) -> int:
    """Write (name, bytes) pairs straight to a ZIP; returns the entry count."""
    n = 0
    with zipfile.ZipFile(path, "w", allowZip64=True) as zf:
        for name, data in entries:
            info = zipfile.ZipInfo(name, date_time=ZIP_ENTRY_TIME)
            stored = name.lower().endswith((".jpg", ".jpeg", ".mp4", ".mov"))
            info.compress_type = zipfile.ZIP_STORED if stored else zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            zf.writestr(info, data)
            n += 1
    return n


def _items(rng: random.Random, photos: int, videos: int, overlay_every: float, start: dt.datetime, end: dt.datetime, gps_days: float):
    """Drafts newest first. A day's items share a place (a few metres
    apart) and either all or none have GPS."""
    n = photos + videos
    kinds = ["photo"] * photos + ["video"] * videos
    rng.shuffle(kinds)
    span = int((end - start).total_seconds())
    drafts = []
    for i in range(n):
        when = (start + dt.timedelta(seconds=rng.randrange(span))).replace(microsecond=0)
        drafts.append({"id": new_id(rng), "when": when, "kind": kinds[i], "overlay": rng.random() < overlay_every})
    drafts.sort(key=lambda d: d["when"], reverse=True)
    day_place: dict[str, tuple[float, float] | None] = {}
    for d in drafts:
        day = filename_date(d["when"])
        if day not in day_place:
            if rng.random() < gps_days:
                _, lat, lon = CITIES[rng.randrange(len(CITIES))]
                day_place[day] = (round(lat + rng.uniform(-0.05, 0.05), 5), round(lon + rng.uniform(-0.05, 0.05), 5))
            else:
                day_place[day] = None
        place = day_place[day]
        d["loc"] = None if place is None else (round(place[0] + rng.uniform(-0.0001, 0.0001), 5), round(place[1] + rng.uniform(-0.0001, 0.0001), 5))
    return drafts


def _build(ctx: Context, name: str, srs_id: str, *, photos: int, videos: int, parts: int, overlays: float, media: str, profile: str, json_only: bool = False) -> dict:
    rng = ctx.rng(name)
    out = ctx.out_dir / name
    reset_dir(out)
    drafts = _items(rng, photos, videos, overlays, dt.datetime(2016, 1, 1, tzinfo=UTC), dt.datetime(2026, 6, 30, tzinfo=UTC), 0.7)

    # Media variants.
    if media == "placeholder":
        photo_blobs = [rng.randbytes(2048 + rng.randrange(2048)) for _ in range(8)]
        video_blobs = [rng.randbytes(8192 + rng.randrange(4096)) for _ in range(4)]
        overlay_blobs = [rng.randbytes(512) for _ in range(4)]
    elif media == "small":
        photo_blobs = [_jpeg(SMALL, (rng.randrange(40, 220), rng.randrange(40, 220), rng.randrange(40, 220)), f"v{i}") for i in range(16)]
        video_blobs = [_clip(ctx, SMALL, 2, None, fs) for fs in (True, False)] if videos else []
        overlay_blobs = [_overlay(SMALL)]
    elif media == "phone":
        photo_blobs = [_phone_jpeg(rng) for _ in range(6)] if photos else []
        video_blobs = [_clip(ctx, (1080, 1920), 10, "3500k", fs) for fs in (True, False)] if videos else []
        overlay_blobs = [_overlay((1080, 1920))]
    else:
        raise ValueError(media)

    uid = new_id(rng, upper=False)
    entries = []
    for d in drafts:
        e = {
            "Date": export_date(d["when"]),
            "Media Type": media_type_field(d["kind"]),
            "Location": location_field(*d["loc"]) if d["loc"] else NO_LOCATION_FIELD,
        }
        if profile == "2026a":
            e["Download Link"] = f"https://app.example.invalid/dmd/memories?uid={uid}&sid={d['id']}&mid={d['id']}&ts=1746057600&sig=0"
        else:
            e["Download Link"] = ""
        entries.append(e)
    json_data = json_bytes({"Saved Media": entries})

    # Consecutive chunks of the newest-first list, one per part.
    per = -(-len(drafts) // parts) if drafts else 0
    part_files = []
    media_files = 0
    overlay_count = 0
    for p in range(parts):
        chunk = drafts[p * per:(p + 1) * per]
        file_name = part_name() if p == 0 else part_name(f"-{p + 1}")

        def gen(chunk=chunk, first=(p == 0)):
            if first:
                yield "index.html", INDEX_HTML
                yield "html/memories_history.html", MEMORIES_HTML
                yield "json/memories_history.json", json_data
            if json_only:
                return
            for i, d in enumerate(chunk):
                day = filename_date(d["when"])
                if d["kind"] == "video":
                    yield f"memories/{day}_{d['id']}-main.mp4", video_blobs[i % len(video_blobs)]
                else:
                    yield f"memories/{day}_{d['id']}-main.jpg", photo_blobs[i % len(photo_blobs)]
                if d["overlay"]:
                    yield f"memories/{day}_{d['id']}-overlay.png", overlay_blobs[i % len(overlay_blobs)]

        n = _write_part(out / file_name, gen())
        part_files.append({"file": file_name, "role": "primary" if p == 0 else "media", "number": p + 1, "entries_count": n})
        if not json_only:
            media_files += sum(1 + int(d["overlay"]) for d in chunk)
            overlay_count += sum(int(d["overlay"]) for d in chunk)

    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": srs_id,
        "format": profile,
        "generator": {
            "name": GENERATOR_NAME,
            "version": GENERATOR_VERSION,
            "seed": ctx.seed,
            "zip_order": "json",
            "json_order": "newest_first",
            "media": media,
            "ffmpeg": {"path": ctx.ffmpeg.path, "encoder": ctx.ffmpeg.encoder} if ctx.ffmpeg else None,
        },
        "parts": part_files,
        "counts": {
            "json_entries": len(entries),
            "items": 0 if json_only else len(drafts),
            "photos": 0 if json_only else sum(1 for d in drafts if d["kind"] == "photo"),
            "videos": 0 if json_only else sum(1 for d in drafts if d["kind"] == "video"),
            "overlays": overlay_count,
            "media_files": media_files,
            "main_files": 0 if json_only else len(drafts),
            "with_location": sum(1 for d in drafts if d["loc"]),
            "input_bytes": sum((out / p["file"]).stat().st_size for p in part_files),
        },
    }
    write_truth(out / "truth.json", truth)
    ctx.log(f"{name}: {len(part_files)} parts, {len(entries)} entries, {truth['counts']['input_bytes'] / 1e6:.0f} MB")
    return truth


def build_scale_index(ctx: Context) -> dict:
    return _build(ctx, "scale-index", "FR-06", photos=40_000, videos=10_000, parts=5, overlays=0.3, media="placeholder", profile="2026b")


def build_scale_h01(ctx: Context) -> dict:
    return _build(ctx, "scale-h01", "H-01", photos=40_000, videos=10_000, parts=12, overlays=0.3, media="small", profile="2026b")


def build_scale_h05(ctx: Context) -> dict:
    return _build(ctx, "scale-h05", "H-05", photos=160_000, videos=40_000, parts=1, overlays=0.0, media="placeholder", profile="2026b", json_only=True)


def build_perf_photos(ctx: Context) -> dict:
    return _build(ctx, "perf-photos", "NFR-PERF-03", photos=1_000, videos=0, parts=1, overlays=0.0, media="phone", profile="2026a")


def build_perf_videos(ctx: Context) -> dict:
    return _build(ctx, "perf-videos", "NFR-PERF-03", photos=0, videos=200, parts=1, overlays=0.0, media="phone", profile="2026a")


SCALE_BUILDERS = {
    "scale-index": build_scale_index,
    "scale-h01": build_scale_h01,
    "scale-h05": build_scale_h05,
    "perf-photos": build_perf_photos,
    "perf-videos": build_perf_videos,
}
