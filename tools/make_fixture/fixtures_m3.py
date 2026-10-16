"""M3 fixtures (SRS v0.2 section 11.2): F-07 rotation, F-08 clips, F-09 codecs.

These are media fixtures for crates/mr-composite. Each one is written three
ways in <out>/<name>/:

    mydata~<epoch>.zip   a 2026b export (memories_history.json + memories/),
                         so the job engine can run end to end on it
    files/               the same media files, extracted, for the media tests
    ref/                 reference composites (F-07 only)
    truth.json           what was generated, item by item

ffmpeg with libx264 is required (libx265 for the HEVC items of F-09). The
generator uses ffmpeg from the MR_FIXTURE_FFMPEG environment variable, else
the one on PATH. Without it, or with --no-video, each fixture still writes a
truth.json with "skipped" set and no media, and the media tests skip; this is
what happens on the CI runners.

Reference composites are produced independently of the Rust code:
- photos with Pillow: ImageOps.exif_transpose, the overlay resized with
  BILINEAR to the placement computed here, Image.alpha_composite;
- video frames with ffmpeg's own autorotate and a reference filter graph
  "[1:v]scale=W:H[ov];[0:v][ov]overlay=X:Y", one PNG per sample time.

Every scene carries two markers in display orientation: a red square in the
top-left corner and a blue one in the bottom-right corner, so a test can tell
an upright frame from a rotated or mirrored one without any reference.
"""

from __future__ import annotations

import concurrent.futures
import datetime as dt
import os
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont, ImageOps

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
    new_id,
    part_name,
    reset_dir,
    write_truth,
)

FFMPEG_ENV = "MR_FIXTURE_FFMPEG"
ASPECT_TOLERANCE = 0.02  # SRS section 4.4


# ---------------------------------------------------------------------------
# Tools
# ---------------------------------------------------------------------------


@dataclass
class Tools:
    ffmpeg: str
    version: str
    x265: bool


def _candidates(ctx: Context) -> list[str]:
    """MR_FIXTURE_FFMPEG alone when set; otherwise every ffmpeg on PATH, in
    order. The first one found is often not the right one: `cargo test` puts
    target/debug first on PATH on Windows, and tauri-build copies the LGPL
    sidecar (no libx264) there."""
    env = os.environ.get(FFMPEG_ENV)
    if env:
        return [env]
    if ctx.ffmpeg is None:  # --no-video, or no ffmpeg on PATH at all
        return []
    exe = "ffmpeg.exe" if os.name == "nt" else "ffmpeg"
    found = [ctx.ffmpeg.path]
    for d in os.environ.get("PATH", "").split(os.pathsep):
        p = os.path.join(d, exe)
        if d and os.path.isfile(p):
            found.append(p)
    seen, out = set(), []
    for p in found:
        key = os.path.normcase(os.path.abspath(p))
        if key not in seen:
            seen.add(key)
            out.append(p)
    return out


def _tools(ctx: Context) -> tuple[Tools | None, str]:
    """The first ffmpeg with libx264, or (None, reason)."""
    reason = "ffmpeg_missing"
    for path in _candidates(ctx):
        try:
            version = subprocess.run([path, "-version"], capture_output=True, text=True, check=True, timeout=30).stdout.splitlines()[0]
            listing = subprocess.run([path, "-hide_banner", "-encoders"], capture_output=True, text=True, check=True, timeout=30).stdout
        except (OSError, subprocess.SubprocessError, IndexError):
            continue
        names = {line.split()[1] for line in listing.splitlines() if line.startswith(" V") and len(line.split()) > 1}
        if "libx264" in names:
            return Tools(ffmpeg=path, version=version, x265="libx265" in names), ""
        reason = "ffmpeg_without_libx264"
    return None, reason


def _ff(tools: Tools, args: list[str]) -> None:
    cmd = [tools.ffmpeg, "-y", "-hide_banner", "-loglevel", "error", "-nostdin", *args]
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=900)
    if r.returncode != 0:
        raise RuntimeError(f"ffmpeg failed ({r.returncode}): {' '.join(cmd)}\n{r.stderr}")


def _generator_block(ctx: Context, tools: Tools | None) -> dict:
    return {
        "name": GENERATOR_NAME,
        "version": GENERATOR_VERSION,
        "seed": ctx.seed,
        "zip_order": ctx.zip_order,
        "json_order": "newest_first",
        "ffmpeg": (
            {"available": True, "path_env": FFMPEG_ENV, "version": tools.version, "libx265": tools.x265}
            if tools
            else {"available": False}
        ),
    }


def _write_skipped(ctx: Context, name: str, srs_id: str, reason: str) -> dict:
    out = ctx.out_dir / name
    reset_dir(out)
    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": srs_id,
        "format": "2026b",
        "skipped": reason,
        "generator": _generator_block(ctx, None),
        "items": [],
    }
    write_truth(out / "truth.json", truth)
    ctx.log(f"{name}: skipped ({reason}); media tests will skip")
    return truth


# ---------------------------------------------------------------------------
# Scenes, overlays, placement
# ---------------------------------------------------------------------------


def _marker(w: int, h: int) -> int:
    return max(8, min(w, h) // 6)


def _markers(w: int, h: int) -> dict:
    m = _marker(w, h)
    return {"size": m, "red": [m // 2, m // 2], "blue": [w - m + m // 2, h - m + m // 2]}


def _scene_filter(w: int, h: int, fps: int, seconds: float) -> str:
    m = _marker(w, h)
    return (
        f"testsrc2=size={w}x{h}:rate={fps}:duration={seconds},"
        f"drawbox=x=0:y=0:w={m}:h={m}:color=red:t=fill,"
        f"drawbox=x={w - m}:y={h - m}:w={m}:h={m}:color=blue:t=fill"
    )


def _unrotate(rotation: int) -> str | None:
    """The filter that turns an upright scene into the coded frames a
    camera stores for a clockwise display rotation."""
    return {90: "transpose=cclock", 270: "transpose=clock", 180: "transpose=clock,transpose=clock"}.get(rotation)


def _font(size: int):
    try:
        return ImageFont.load_default(size=size)
    except TypeError:  # Pillow < 10.1
        return ImageFont.load_default()


def _photo_scene(w: int, h: int, label: str) -> Image.Image:
    """Bands of colour, diagonal lines, a label and the two corner markers."""
    img = Image.new("RGB", (w, h))
    d = ImageDraw.Draw(img)
    bands = 24
    for i in range(bands):
        y0 = i * h // bands
        y1 = (i + 1) * h // bands
        d.rectangle([0, y0, w, y1], fill=(40 + (i * 9) % 180, 90 + (i * 23) % 140, 160 - (i * 5) % 120))
    for k in range(-h, w, 48):
        d.line([(k, 0), (k + h, h)], fill=(230, 230, 230), width=3)
    d.text((w // 4, h // 2), label, fill=(255, 255, 255), font=_font(max(14, w // 20)))
    m = _marker(w, h)
    d.rectangle([0, 0, m - 1, m - 1], fill=(255, 0, 0))
    d.rectangle([w - m, h - m, w - 1, h - 1], fill=(0, 0, 255))
    return img


def _overlay(w: int, h: int, label: str, sticker: tuple[float, float] = (0.5, 0.3)) -> Image.Image:
    """Transparent PNG: a caption bar and a round sticker, corners left clear."""
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    top = int(h * 0.55)
    d.rectangle([0, top, w, top + max(8, int(h * 0.07))], fill=(0, 0, 0, 150))
    d.text((w // 10, top + max(2, int(h * 0.015))), label, fill=(255, 255, 255, 255), font=_font(max(12, w // 18)))
    cx, cy, r = int(w * sticker[0]), int(h * sticker[1]), max(6, int(min(w, h) * 0.12))
    d.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(255, 196, 0, 230), outline=(255, 255, 255, 255), width=max(2, r // 12))
    return img


def _place(target: tuple[int, int], ov: tuple[int, int]) -> dict:
    """SRS section 4.4: stretch within 2%, else fit the width and centre vertically."""
    at = target[0] / target[1]
    ao = ov[0] / ov[1]
    if abs(ao - at) / at <= ASPECT_TOLERANCE + 1e-9:
        return {"x": 0, "y": 0, "width": target[0], "height": target[1], "aspect_mismatch": False}
    height = max(1, int(target[0] * ov[1] / ov[0] + 0.5))
    return {"x": 0, "y": (target[1] - height) // 2, "width": target[0], "height": height, "aspect_mismatch": True}


def _save_png(img: Image.Image, path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    img.save(path, "PNG", optimize=True)


def _save_jpeg(img: Image.Image, path: Path, orientation: int | None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    kwargs = {"quality": 95}
    if orientation:
        exif = Image.Exif()
        exif[0x0112] = orientation
        kwargs["exif"] = exif.tobytes()
    img.save(path, "JPEG", **kwargs)


# Stored = inverse of the display transform, so that exif_transpose(stored) == upright.
_STORE_FOR_ORIENTATION = {1: None, 3: Image.Transpose.ROTATE_180, 6: Image.Transpose.ROTATE_90, 8: Image.Transpose.ROTATE_270}


# ---------------------------------------------------------------------------
# Video
# ---------------------------------------------------------------------------


@dataclass
class VideoSpec:
    out: Path
    scene: tuple[int, int]  # upright size
    seconds: float
    fps: int = 30
    rotation: int = 0
    codec: str = "h264"  # h264 | hevc
    pix_fmt: str = "yuv420p"
    tag: str | None = None
    audio_hz: int | None = 440
    vfr: bool = False
    force_size: tuple[int, int] | None = None  # coded size override (odd sizes)
    crf: int = 18


def _make_video(tools: Tools, s: VideoSpec) -> None:
    s.out.parent.mkdir(parents=True, exist_ok=True)
    tmp = s.out.with_name(s.out.stem + ".tmp.mp4")
    vf = []
    if s.vfr:
        # 30 fps for the first second, 15 fps after: two frame durations in stts.
        vf.append("select='if(lt(n,30),1,not(mod(n,2)))'")
    unrot = _unrotate(s.rotation)
    if unrot:
        vf.append(unrot)
    if s.force_size:
        vf.append(f"scale={s.force_size[0]}:{s.force_size[1]},setsar=1")
    vf.append(f"format={s.pix_fmt}")
    args = ["-f", "lavfi", "-i", _scene_filter(s.scene[0], s.scene[1], s.fps, s.seconds)]
    if s.audio_hz:
        args += ["-f", "lavfi", "-i", f"sine=frequency={s.audio_hz}:sample_rate=44100:duration={s.seconds}"]
    args += ["-vf", ",".join(vf)]
    if s.vfr:
        args += ["-fps_mode", "vfr"]
    if s.codec == "h264":
        args += ["-c:v", "libx264", "-preset", "veryfast", "-crf", str(s.crf)]
    elif s.codec == "hevc":
        args += ["-c:v", "libx265", "-preset", "fast", "-crf", str(s.crf + 4), "-x265-params", "log-level=error"]
    else:
        raise ValueError(s.codec)
    if s.tag:
        args += ["-tag:v", s.tag]
    args += ["-c:a", "aac", "-b:a", "96k"] if s.audio_hz else ["-an"]
    target = tmp if s.rotation else s.out
    args += ["-movflags", "+faststart", str(target)]
    _ff(tools, args)
    if s.rotation:
        # -display_rotation is counter-clockwise; the clockwise "rotate=90" of
        # a phone is -90 here (ffmpeg -i then prints "rotation of -90.00 degrees").
        _ff(tools, ["-display_rotation", str(-s.rotation), "-i", str(tmp), "-c", "copy", "-movflags", "+faststart", str(s.out)])
        tmp.unlink()


def _run_parallel(jobs: list) -> None:
    workers = max(1, min(8, (os.cpu_count() or 2)))
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        for f in [pool.submit(job) for job in jobs]:
            f.result()


# ---------------------------------------------------------------------------
# Export assembly
# ---------------------------------------------------------------------------


@dataclass
class Media:
    id: str
    when: dt.datetime
    kind: str  # photo | video
    ext: str
    main: Path  # file under files/
    overlay: Path | None = None
    location: tuple[float, float] | None = None
    extra: dict = field(default_factory=dict)


def _media_names(m: Media) -> tuple[str, str | None]:
    base = f"{filename_date(m.when)}_{m.id}"
    return f"{base}-main.{m.ext}", (f"{base}-overlay.png" if m.overlay else None)


def _assemble(ctx: Context, name: str, srs_id: str, tools: Tools | None, media: list[Media], rng, extra: dict) -> dict:
    out = ctx.out_dir / name
    ordered = sorted(media, key=lambda m: (m.when, m.id), reverse=True)
    zipb = ZipBuilder()
    entries = []
    items = []
    for i, m in enumerate(ordered):
        main_name, ov_name = _media_names(m)
        entries.append(
            {
                "Date": export_date(m.when),
                "Media Type": "Image" if m.kind == "photo" else "Video",
                "Location": location_field(*m.location) if m.location else NO_LOCATION_FIELD,
                "Download Link": "",
                "Media Download Url": "N/A",
            }
        )
        zipb.add_media(f"memories/{main_name}", m.main.read_bytes())
        if ov_name:
            zipb.add_media(f"memories/{ov_name}", m.overlay.read_bytes())
        items.append(
            {
                "json_index": i,
                "id": m.id,
                "date_utc": iso_utc(m.when),
                "media_type": m.kind,
                "ext": m.ext,
                "main": f"files/{m.main.name}",
                "overlay": f"files/{m.overlay.name}" if m.overlay else None,
                "zip_main": f"memories/{main_name}",
                "zip_overlay": f"memories/{ov_name}" if ov_name else None,
                "location": {"lat": m.location[0], "lon": m.location[1]} if m.location else None,
                **m.extra,
            }
        )
    zipb.add_header("index.html", INDEX_HTML)
    zipb.add_header("html/memories_history.html", MEMORIES_HTML)
    zipb.add_header("json/memories_history.json", json_bytes({"Saved Media": entries}))
    zip_path = out / part_name()
    order = zipb.write(zip_path, ctx.zip_order, rng)
    photos = sum(1 for m in media if m.kind == "photo")
    truth = {
        "schema": TRUTH_SCHEMA,
        "fixture": name,
        "srs_id": srs_id,
        "format": "2026b",
        "generator": _generator_block(ctx, tools),
        "parts": [{"file": zip_path.name, "role": "primary", "entries": order}],
        "json": {"bom": False, "crlf": False, "indent": 2, "top_level_keys_before": [], "top_level_keys_after": []},
        "counts": {
            "json_entries": len(items),
            "items": len(items),
            "photos": photos,
            "videos": len(items) - photos,
            "overlays": sum(1 for m in media if m.overlay),
            "with_location": sum(1 for m in media if m.location),
            "with_url": 0,
            "with_tokens": 0,
            "media_files": len(items) + sum(1 for m in media if m.overlay),
            "main_files": len(items),
            "main_files_skipped": 0,
        },
        "items": items,
        **extra,
    }
    write_truth(out / "truth.json", truth)
    return truth


def _named(files: Path, m_id: str, when: dt.datetime, ext: str, overlay: bool) -> tuple[Path, Path | None]:
    base = f"{filename_date(when)}_{m_id}"
    return files / f"{base}-main.{ext}", (files / f"{base}-overlay.png" if overlay else None)


# ---------------------------------------------------------------------------
# F-07 rotation
# ---------------------------------------------------------------------------

# (case, upright scene, rotation, overlay size, seconds, sample times)
F07_VIDEOS = [
    ("v_rotate_90", (360, 640), 90, (720, 1280), 3, [0.517, 1.517, 2.517]),
    ("v_rotate_270", (360, 640), 270, (360, 640), 3, [0.517, 1.517, 2.517]),
    ("v_rotate_180", (640, 360), 180, (1280, 720), 3, [0.517, 2.517]),
    ("v_landscape_60s", (640, 360), 0, (640, 360), 60, [0.517, 30.017, 59.017]),
    ("v_rotate_90_aspect_mismatch", (360, 780), 90, (1080, 1920), 3, [0.517, 1.517, 2.517]),
]

# (case, upright size, orientation, overlay size, format)
F07_PHOTOS = [
    ("p_orientation_6", (540, 960), 6, (1080, 1920), "jpg"),
    ("p_orientation_8", (540, 960), 8, (540, 960), "jpg"),
    ("p_orientation_3", (540, 960), 3, (540, 960), "jpg"),
    ("p_orientation_1", (540, 960), 1, (1080, 1920), "jpg"),
    ("p_png", (540, 960), 1, (540, 960), "png"),
    ("p_aspect_mismatch", (540, 1170), 1, (1080, 1920), "jpg"),
    ("p_orientation_6_aspect_mismatch", (540, 1170), 6, (1080, 1920), "jpg"),
]


def build_rotation(ctx: Context) -> dict:
    """F-07. The photos need only Pillow, so they are generated even without
    ffmpeg (FR-23 AC1 then runs on the CI runners too); the videos need ffmpeg
    and are left out, with "videos_skipped" set, when it is missing."""
    name, srs_id = "rotation", "F-07"
    tools, reason = _tools(ctx)
    rng = ctx.rng(name)
    out = ctx.out_dir / name
    reset_dir(out)
    files, ref = out / "files", out / "ref"
    files.mkdir()
    ref.mkdir()
    base = dt.datetime(2020, 8, 1, 12, 0, 0, tzinfo=UTC)
    media: list[Media] = []
    jobs = []
    videos = []
    for k, (case, scene, rot, ov_size, secs, times) in enumerate(F07_VIDEOS if tools else []):
        m_id = new_id(rng)
        when = base + dt.timedelta(hours=k)
        main, ov = _named(files, m_id, when, "mp4", True)
        _save_png(_overlay(*ov_size, f"F-07 {case}"), ov)
        jobs.append(lambda s=VideoSpec(main, scene, secs, rotation=rot, crf=18 if secs < 10 else 23): _make_video(tools, s))
        coded = scene if rot in (0, 180) else (scene[1], scene[0])
        placement = _place(scene, ov_size)
        videos.append((case, main, ov, placement, times))
        media.append(
            Media(
                m_id, when, "video", "mp4", main, ov,
                extra={
                    "case": case,
                    "rotation": rot,
                    "coded_size": list(coded),
                    "display_size": list(scene),
                    "overlay_size": list(ov_size),
                    "placement": placement,
                    "duration": secs,
                    "has_audio": True,
                    "markers": _markers(*scene),
                },
            )
        )
    _run_parallel(jobs)

    # Reference frames: ffmpeg's own autorotate plus the reference graph.
    ref_jobs = []
    for case, main, ov, p, times in videos:
        frames = []
        for t in times:
            png = ref / f"{case}_t{t:.3f}.png"
            frames.append({"t": t, "file": f"ref/{png.name}"})
            graph = f"[1:v]scale={p['width']}:{p['height']}[ov];[0:v][ov]overlay={p['x']}:{p['y']}:eof_action=repeat"
            ref_jobs.append(lambda t=t, main=main, ov=ov, graph=graph, png=png: _ff(tools, ["-ss", f"{t:.3f}", "-i", str(main), "-i", str(ov), "-filter_complex", graph, "-frames:v", "1", str(png)]))
        next(m for m in media if m.extra.get("case") == case).extra["reference_frames"] = frames
    _run_parallel(ref_jobs)

    for k, (case, size, orientation, ov_size, fmt) in enumerate(F07_PHOTOS):
        m_id = new_id(rng)
        when = base + dt.timedelta(days=1, hours=k)
        main, ov = _named(files, m_id, when, fmt, True)
        upright = _photo_scene(*size, f"F-07 {case}")
        op = _STORE_FOR_ORIENTATION[orientation]
        stored = upright.transpose(op) if op else upright
        if fmt == "jpg":
            _save_jpeg(stored, main, orientation if orientation != 1 else None)
        else:
            _save_png(stored, main)
        overlay = _overlay(*ov_size, f"F-07 {case}", sticker=(0.6, 0.25))
        _save_png(overlay, ov)
        # Reference: decode what was written, turn it upright with Pillow's
        # own EXIF handling, composite with Pillow.
        with Image.open(main) as im:
            up = ImageOps.exif_transpose(im).convert("RGBA")
        if up.size != size:
            raise RuntimeError(f"{case}: exif_transpose gave {up.size}, expected {size}")
        p = _place(size, ov_size)
        layer = Image.new("RGBA", size, (0, 0, 0, 0))
        layer.paste(overlay.resize((p["width"], p["height"]), Image.Resampling.BILINEAR), (p["x"], p["y"]))
        reference = Image.alpha_composite(up, layer).convert("RGB")
        ref_png = ref / f"{case}.png"
        _save_png(reference, ref_png)
        media.append(
            Media(
                m_id, when, "photo", fmt, main, ov,
                extra={
                    "case": case,
                    "orientation": orientation,
                    "stored_size": list(stored.size),
                    "display_size": list(size),
                    "overlay_size": list(ov_size),
                    "placement": p,
                    "output_format": "png" if fmt == "png" else "jpeg",
                    "reference": f"ref/{ref_png.name}",
                    "markers": _markers(*size),
                },
            )
        )
    extra = {"aspect_tolerance": ASPECT_TOLERANCE, "videos_skipped": None if tools else reason}
    truth = _assemble(ctx, name, srs_id, tools, media, rng, extra)
    videos_note = f"{len(F07_VIDEOS)} videos" if tools else f"videos skipped ({reason})"
    ctx.log(f"{name}: {videos_note}, {len(F07_PHOTOS)} photos, references in ref/")
    return truth


# ---------------------------------------------------------------------------
# F-08 clips
# ---------------------------------------------------------------------------

CLIP_SCENE = (288, 512)
LONDON = (51.50735, -0.12776)
LONDON_1KM = (51.51735, -0.12776)  # 1.1 km north
LONDON_1KM_NEAR = (51.51750, -0.12790)  # ~20 m from LONDON_1KM
TOKYO = (35.67620, 139.65030)
SYDNEY = (-33.86880, 151.20930)


@dataclass
class ClipSpec:
    group: str
    index: int
    start: dt.datetime
    seconds: float
    scene: tuple[int, int] = CLIP_SCENE
    fps: int = 30
    rotation: int = 0
    location: tuple[float, float] | None = None
    overlay: bool = False
    audio_hz: int = 440


def _chain(group: str, start: dt.datetime, durations: list[float], gap: float = 1.0, **kw) -> list[ClipSpec]:
    """Clips that follow each other with `gap` seconds between end and next Date."""
    out = []
    t = start
    for i, d in enumerate(durations):
        out.append(ClipSpec(group, i, t, d, **kw))
        t = t + dt.timedelta(seconds=d + gap)
    return out


def _f08_specs() -> list[ClipSpec]:
    d = lambda *a: dt.datetime(*a, tzinfo=UTC)  # noqa: E731
    clips: list[ClipSpec] = []
    a = _chain("A_ac1", d(2019, 7, 14, 18, 20, 0), [10, 10, 4], audio_hz=440)
    clips += a
    # AC2: one second after A's last clip, same codec and rate, other size.
    clips.append(ClipSpec("decoy_resolution", 0, d(2019, 7, 14, 18, 20, 27), 10, scene=(360, 640), audio_hz=440))
    b = _chain("B_2017", d(2017, 3, 2, 9, 0, 0), [10, 10], location=LONDON, audio_hz=330)
    clips += b
    clips.append(ClipSpec("decoy_gap", 0, d(2017, 3, 2, 9, 0, 26), 10, location=LONDON, audio_hz=330))  # 5 s after B
    clips += _chain("C_six", d(2021, 12, 31, 20, 0, 0), [10, 10, 10, 10, 10, 3], audio_hz=550)
    clips += _chain("D_overlays", d(2018, 5, 20, 14, 0, 0), [10, 10, 10, 6], rotation=90, location=TOKYO, audio_hz=660)
    for c in clips:
        if c.group == "D_overlays" and c.index in (0, 2):
            c.overlay = True
    clips += _chain("E_2023", d(2023, 8, 1, 10, 0, 0), [10, 10, 5], audio_hz=495)
    clips += _chain("F_short_middle", d(2020, 2, 2, 12, 0, 0), [10, 6, 10], audio_hz=520)
    g = _chain("G_fps", d(2020, 4, 4, 16, 0, 0), [10, 10], audio_hz=600)
    g[1].fps = 25
    clips += g
    h = _chain("H_location", d(2019, 9, 9, 9, 9, 0), [10, 10, 10], audio_hz=700)
    h[0].location, h[1].location, h[2].location = LONDON, LONDON_1KM, LONDON_1KM_NEAR
    clips += h
    clips += _chain("I_rotated_copy", d(2019, 1, 6, 15, 0, 0), [10, 10, 3], rotation=90, location=SYDNEY, audio_hz=740)
    clips += _chain("K_new_year", d(2021, 12, 31, 23, 59, 49), [10, 10], gap=1.0, audio_hz=800)
    return clips


def build_clips(ctx: Context) -> dict:
    name, srs_id = "clips", "F-08"
    tools, reason = _tools(ctx)
    if not tools:
        return _write_skipped(ctx, name, srs_id, reason)
    rng = ctx.rng(name)
    out = ctx.out_dir / name
    reset_dir(out)
    files = out / "files"
    files.mkdir()
    specs = _f08_specs()
    media: list[Media] = []
    ids: dict[tuple[str, int], str] = {}
    jobs = []
    for c in specs:
        m_id = new_id(rng)
        ids[(c.group, c.index)] = m_id
        main, ov = _named(files, m_id, c.start, "mp4", c.overlay)
        if ov:
            # 2x the display size, sticker placed per segment so a test can see which one is on.
            _save_png(_overlay(c.scene[0] * 2, c.scene[1] * 2, f"F-08 {c.group} {c.index}", sticker=(0.5, 0.3)), ov)
        spec = VideoSpec(main, c.scene, c.seconds, fps=c.fps, rotation=c.rotation, audio_hz=c.audio_hz, crf=23)
        jobs.append(lambda s=spec: _make_video(tools, s))
        coded = c.scene if c.rotation in (0, 180) else (c.scene[1], c.scene[0])
        media.append(
            Media(
                m_id, c.start, "video", "mp4", main, ov, c.location,
                extra={
                    "group": c.group,
                    "index": c.index,
                    "duration": c.seconds,
                    "display_size": list(c.scene),
                    "coded_size": list(coded),
                    "rotation": c.rotation,
                    "fps": c.fps,
                    "codec": "avc1",
                    "audio_hz": c.audio_hz,
                },
            )
        )
    _run_parallel(jobs)

    def grp(g: str, n: int) -> list[str]:
        return [ids[(g, i)] for i in range(n)]

    default_groups = [
        grp("B_2017", 2),
        grp("D_overlays", 4),
        grp("I_rotated_copy", 3),
        grp("A_ac1", 3),
        [ids[("H_location", 1)], ids[("H_location", 2)]],
        [ids[("F_short_middle", 0)], ids[("F_short_middle", 1)]],
        grp("C_six", 6),
    ]
    all_years_groups = default_groups + [grp("K_new_year", 2), grp("E_2023", 3)]
    extra = {
        "rejoin_defaults": {"years": [2017, 2021], "min_clip_secs": 9.0, "max_gap_secs": 2.0},
        # Expected groups, stated by construction, in capture order.
        "expected_groups": {"default": default_groups, "all_years": all_years_groups},
        "cases": {
            "ac1": grp("A_ac1", 3),
            "ac1_expected_secs": 24.0,
            "ac2_decoy_resolution": ids[("decoy_resolution", 0)],
            "decoy_gap": ids[("decoy_gap", 0)],
            "fps_decoy": grp("G_fps", 2),
            "location_far": ids[("H_location", 0)],
            "short_middle_alone": ids[("F_short_middle", 2)],
            "overlay_group": grp("D_overlays", 4),
            "overlay_segments": [0, 2],
            "rotated_copy_group": grp("I_rotated_copy", 3),
            "year_2023": grp("E_2023", 3),
            "new_year": grp("K_new_year", 2),
        },
    }
    truth = _assemble(ctx, name, srs_id, tools, media, rng, extra)
    ctx.log(f"{name}: {len(specs)} clips, {len(default_groups)} groups by default, {len(all_years_groups)} with all years")
    return truth


# ---------------------------------------------------------------------------
# F-09 codecs
# ---------------------------------------------------------------------------

# (case, VideoSpec kwargs, expected probe fields, needs x265)
def _f09_cases() -> list[tuple[str, dict, dict, bool]]:
    return [
        ("h264_8bit", {"scene": (640, 360), "seconds": 3}, {"codec": "avc1", "coded_size": [640, 360], "rotation": 0, "fps": 30.0, "vfr": False, "bit_depth": 8, "has_audio": True}, False),
        ("hevc_8bit_rotate_90", {"scene": (360, 640), "seconds": 3, "codec": "hevc", "tag": "hvc1", "rotation": 90}, {"codec": "hvc1", "coded_size": [640, 360], "rotation": 90, "fps": 30.0, "vfr": False, "bit_depth": 8, "has_audio": True}, True),
        ("hevc_10bit", {"scene": (640, 360), "seconds": 3, "codec": "hevc", "pix_fmt": "yuv420p10le"}, {"codec": "hev1", "coded_size": [640, 360], "rotation": 0, "fps": 30.0, "vfr": False, "bit_depth": 10, "has_audio": True}, True),
        ("h264_vfr", {"scene": (360, 640), "seconds": 3, "vfr": True}, {"codec": "avc1", "coded_size": [360, 640], "rotation": 0, "fps": None, "vfr": True, "bit_depth": 8, "has_audio": True}, False),
        ("h264_no_audio", {"scene": (360, 640), "seconds": 3, "audio_hz": None}, {"codec": "avc1", "coded_size": [360, 640], "rotation": 0, "fps": 30.0, "vfr": False, "bit_depth": 8, "has_audio": False}, False),
        ("h264_odd_size", {"scene": (360, 640), "seconds": 3, "pix_fmt": "yuv444p", "force_size": (361, 641)}, {"codec": "avc1", "coded_size": [361, 641], "rotation": 0, "fps": 30.0, "vfr": False, "bit_depth": 8, "has_audio": True}, False),
        ("h264_25fps", {"scene": (640, 360), "seconds": 3, "fps": 25}, {"codec": "avc1", "coded_size": [640, 360], "rotation": 0, "fps": 25.0, "vfr": False, "bit_depth": 8, "has_audio": True}, False),
    ]


def build_codecs(ctx: Context) -> dict:
    name, srs_id = "codecs", "F-09"
    tools, reason = _tools(ctx)
    if not tools:
        return _write_skipped(ctx, name, srs_id, reason)
    rng = ctx.rng(name)
    out = ctx.out_dir / name
    reset_dir(out)
    files = out / "files"
    files.mkdir()
    base = dt.datetime(2022, 3, 3, 8, 0, 0, tzinfo=UTC)
    media: list[Media] = []
    skipped = []
    jobs = []
    for k, (case, kw, expected, needs_x265) in enumerate(_f09_cases()):
        if needs_x265 and not tools.x265:
            skipped.append({"case": case, "reason": "ffmpeg_without_libx265"})
            continue
        m_id = new_id(rng)
        when = base + dt.timedelta(hours=k)
        main, _ = _named(files, m_id, when, "mp4", False)
        spec = VideoSpec(main, **kw)
        jobs.append(lambda s=spec: _make_video(tools, s))
        display = list(spec.force_size) if spec.force_size else list(spec.scene)
        expected = {**expected, "display_size": display, "duration": spec.seconds}
        media.append(Media(m_id, when, "video", "mp4", main, None, extra={"case": case, "expected": expected}))
    _run_parallel(jobs)
    truth = _assemble(ctx, name, srs_id, tools, media, rng, {"skipped_cases": skipped})
    ctx.log(f"{name}: {len(media)} codec cases" + (f", {len(skipped)} skipped (no libx265)" if skipped else ""))
    return truth


BUILDERS_M3 = {
    "rotation": build_rotation,
    "clips": build_clips,
    "codecs": build_codecs,
}


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit("run through: python tools/make_fixture --fixture rotation|clips|codecs")

