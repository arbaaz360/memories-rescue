"""Shared helpers for the fixture generator: deterministic randomness, export
field formatting, ZIP assembly and ground-truth writing.

Standard library only; Pillow and ffmpeg are used from media.py.
"""

from __future__ import annotations

import datetime as dt
import io
import json
import os
import random
import shutil
import subprocess
import uuid
import zipfile
from dataclasses import dataclass, field
from pathlib import Path

GENERATOR_NAME = "make_fixture"
GENERATOR_VERSION = "0.1.0"
TRUTH_SCHEMA = 1

# Export part names look like mydata~<epoch ms>.zip; the number is arbitrary.
EXPORT_EPOCH_MS = 1760000000000
# Fixed timestamp for every ZIP entry so archives are byte-stable.
ZIP_ENTRY_TIME = (2026, 10, 1, 0, 0, 0)

UTC = dt.timezone.utc

# (name, lat, lon): spread over hemispheres and the date line.
CITIES = [
    ("London", 51.50735, -0.12776),
    ("New York", 40.71280, -74.00600),
    ("Sydney", -33.86880, 151.20930),
    ("Tokyo", 35.67620, 139.65030),
    ("Sao Paulo", -23.55050, -46.63330),
    ("Cape Town", -33.92490, 18.42410),
    ("Reykjavik", 64.14660, -21.94260),
    ("Mumbai", 19.07600, 72.87770),
    ("Honolulu", 21.30690, -157.85830),
    ("Auckland", -36.84850, 174.76330),
]

NO_LOCATION_FIELD = "Latitude, Longitude: 0.0, 0.0"


@dataclass
class FfmpegInfo:
    path: str
    version: str
    encoder: str


@dataclass
class Context:
    out_dir: Path
    seed: int
    zip_order: str  # "json" | "shuffled"
    ffmpeg: FfmpegInfo | None
    quiet: bool = False
    multipart_profile: str = "2026b"  # or "2026a": Download Link with sid/mid in the multipart family
    notes: list[str] = field(default_factory=list)
    cache: dict = field(default_factory=dict)

    def rng(self, salt: str) -> random.Random:
        """A fresh generator per fixture so fixtures do not perturb each other."""
        return random.Random(f"{self.seed}:{salt}")

    def log(self, msg: str) -> None:
        if not self.quiet:
            print(msg)


# ---------------------------------------------------------------------------
# Values
# ---------------------------------------------------------------------------


def new_id(rng: random.Random, upper: bool = True) -> str:
    value = str(uuid.UUID(int=rng.getrandbits(128), version=4))
    return value.upper() if upper else value


def random_instant(rng: random.Random, start: dt.datetime, end: dt.datetime) -> dt.datetime:
    span = int((end - start).total_seconds())
    return (start + dt.timedelta(seconds=rng.randrange(span))).replace(microsecond=0)


def export_date(d: dt.datetime) -> str:
    """The export's own Date format."""
    return d.astimezone(UTC).strftime("%Y-%m-%d %H:%M:%S UTC")


def iso_utc(d: dt.datetime) -> str:
    """RFC 3339 in UTC, the form truth.json uses."""
    return d.astimezone(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def filename_date(d: dt.datetime) -> str:
    return d.astimezone(UTC).strftime("%Y-%m-%d")


def random_location(rng: random.Random) -> tuple[float, float]:
    _, lat, lon = rng.choice(CITIES)
    lat = round(lat + rng.uniform(-0.05, 0.05), 5)
    lon = round(lon + rng.uniform(-0.05, 0.05), 5)
    return lat, lon


def location_field(lat: float, lon: float) -> str:
    return f"Latitude, Longitude: {lat:.5f}, {lon:.5f}"


def media_type_field(kind: str) -> str:
    return {"photo": "Image", "video": "Video"}[kind]


# ---------------------------------------------------------------------------
# Files
# ---------------------------------------------------------------------------


def json_bytes(obj: object, *, bom: bool = False, crlf: bool = False, indent: str | int = 2) -> bytes:
    text = json.dumps(obj, indent=indent, ensure_ascii=True)
    if crlf:
        text = text.replace("\n", "\r\n")
    data = text.encode("utf-8")
    if bom:
        data = b"\xef\xbb\xbf" + data
    return data


INDEX_HTML = b"<!doctype html><html><head><title>My Data</title></head><body><p>Synthetic export fixture. No user data.</p></body></html>\n"
MEMORIES_HTML = b"<!doctype html><html><body><h1>Memories</h1><p>Synthetic export fixture.</p></body></html>\n"


class ZipBuilder:
    """Collects entries, then writes them in a chosen order with fixed timestamps."""

    def __init__(self) -> None:
        self.header: list[tuple[str, bytes]] = []
        self.media: list[tuple[str, bytes]] = []

    def add_header(self, name: str, data: bytes) -> None:
        self.header.append((name, data))

    def add_media(self, name: str, data: bytes) -> None:
        self.media.append((name, data))

    def to_bytes(self, order: str, rng: random.Random) -> tuple[bytes, list[str]]:
        media = list(self.media)
        if order == "shuffled":
            rng.shuffle(media)
        elif order != "json":
            raise ValueError(f"unknown zip order {order!r}")
        entries = self.header + media
        buf = io.BytesIO()
        with zipfile.ZipFile(buf, "w", compression=zipfile.ZIP_DEFLATED) as zf:
            for name, data in entries:
                info = zipfile.ZipInfo(name, date_time=ZIP_ENTRY_TIME)
                info.compress_type = zipfile.ZIP_STORED if _is_compressed_media(name) else zipfile.ZIP_DEFLATED
                info.external_attr = 0o644 << 16
                zf.writestr(info, data)
        return buf.getvalue(), [name for name, _ in entries]

    def write(self, path: Path, order: str, rng: random.Random) -> list[str]:
        data, names = self.to_bytes(order, rng)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return names


def _is_compressed_media(name: str) -> bool:
    return name.lower().endswith((".jpg", ".jpeg", ".mp4", ".mov"))


def write_truth(path: Path, truth: dict) -> None:
    path.write_text(json.dumps(truth, indent=2, ensure_ascii=True) + "\n", encoding="utf-8")


def reset_dir(path: Path) -> None:
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)


def part_name(suffix: str = "") -> str:
    return f"mydata~{EXPORT_EPOCH_MS}{suffix}.zip"


# ---------------------------------------------------------------------------
# ffmpeg
# ---------------------------------------------------------------------------

PREFERRED_ENCODERS = ["libx264", "libopenh264", "mpeg4"]
FFMPEG_ENV = "MR_FIXTURE_FFMPEG"


def ffmpeg_candidates() -> list[str]:
    """``MR_FIXTURE_FFMPEG`` alone when set; otherwise every ffmpeg on PATH,
    in PATH order. The first one is often not the right one: ``cargo test``
    puts ``target/debug`` first on PATH on Windows, and tauri-build copies the
    LGPL sidecar there (no libx264), or an empty placeholder in CI."""
    env = os.environ.get(FFMPEG_ENV)
    if env:
        return [env]
    exe = "ffmpeg.exe" if os.name == "nt" else "ffmpeg"
    seen, out = set(), []
    for d in os.environ.get("PATH", "").split(os.pathsep):
        if not d:
            continue
        p = os.path.join(d, exe)
        key = os.path.normcase(os.path.abspath(p))
        if key not in seen and os.path.isfile(p) and os.path.getsize(p) > 0:
            seen.add(key)
            out.append(p)
    return out


def _probe_ffmpeg(path: str) -> tuple[str, set[str]] | None:
    try:
        version = subprocess.run(
            [path, "-version"], capture_output=True, text=True, check=True, timeout=30
        ).stdout.splitlines()[0]
        encoders = subprocess.run(
            [path, "-hide_banner", "-encoders"], capture_output=True, text=True, check=True, timeout=30
        ).stdout
    except (OSError, subprocess.SubprocessError, IndexError):
        return None
    names = {line.split()[1] for line in encoders.splitlines() if line.startswith(" V") and len(line.split()) > 1}
    return version, names


def detect_ffmpeg() -> FfmpegInfo | None:
    """The best ffmpeg anywhere on PATH: the first with libx264, else the
    first with libopenh264, else the first with mpeg4."""
    found = [(p, r) for p in ffmpeg_candidates() if (r := _probe_ffmpeg(p))]
    for enc in PREFERRED_ENCODERS:
        for path, (version, names) in found:
            if enc in names:
                return FfmpegInfo(path=path, version=version, encoder=enc)
    return None
