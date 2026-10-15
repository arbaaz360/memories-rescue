"""Synthetic media: solid-colour photos and transparent overlays with the item
ID drawn on them (Pillow), and short test-pattern videos (ffmpeg, optional).

None of the files carry EXIF or QuickTime dates: like a real export, the
metadata lives only in memories_history.json.
"""

from __future__ import annotations

import io
import subprocess
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

from common import FfmpegInfo

PHOTO_SIZE = (540, 960)  # portrait, like a phone capture


def _font(size: int):
    try:
        return ImageFont.load_default(size=size)
    except TypeError:  # Pillow < 10.1
        return ImageFont.load_default()


def _draw_label(draw: ImageDraw.ImageDraw, lines: list[str], fill, y: int = 24) -> None:
    font = _font(22)
    for line in lines:
        draw.text((24, y), line, fill=fill, font=font)
        y += 30


def make_photo(item_id: str, colour: tuple[int, int, int], fmt: str, date_label: str) -> bytes:
    """A solid-colour image with a contrasting frame and the ID written on it."""
    img = Image.new("RGB", PHOTO_SIZE, colour)
    draw = ImageDraw.Draw(img)
    w, h = PHOTO_SIZE
    frame = tuple(255 - c for c in colour)
    draw.rectangle([8, 8, w - 9, h - 9], outline=frame, width=6)
    _draw_label(draw, ["Memories Rescue fixture", item_id, date_label, "no EXIF on purpose"], frame)
    out = io.BytesIO()
    if fmt == "jpg":
        img.save(out, format="JPEG", quality=85, optimize=True)
    elif fmt == "png":
        img.save(out, format="PNG", optimize=True)
    else:
        raise ValueError(fmt)
    return out.getvalue()


def make_overlay(item_id: str, caption: str) -> bytes:
    """A transparent PNG with a caption bar and a round 'sticker'."""
    w, h = PHOTO_SIZE
    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    # Caption bar across the lower third, semi-transparent.
    draw.rectangle([0, int(h * 0.62), w, int(h * 0.62) + 64], fill=(0, 0, 0, 150))
    draw.text((24, int(h * 0.62) + 18), caption, fill=(255, 255, 255, 255), font=_font(24))
    # Sticker: a filled circle with a ring.
    cx, cy, r = w - 110, 130, 70
    draw.ellipse([cx - r, cy - r, cx + r, cy + r], fill=(255, 196, 0, 230), outline=(255, 255, 255, 255), width=5)
    draw.text((24, 24), f"overlay {item_id[:8]}", fill=(255, 255, 255, 255), font=_font(18))
    out = io.BytesIO()
    img.save(out, format="PNG", optimize=True)
    return out.getvalue()


def make_video(ffmpeg: FfmpegInfo, item_id: str, seconds: int = 2) -> bytes:
    """A 2-second portrait testsrc clip (timestamp burned in by the source
    filter) with a sine tone, encoded with the best available encoder."""
    with tempfile.TemporaryDirectory(prefix="mr-fixture-") as tmp:
        out = Path(tmp) / "clip.mp4"
        cmd = [
            ffmpeg.path, "-y", "-hide_banner", "-loglevel", "error", "-nostdin",
            "-f", "lavfi", "-i", f"testsrc=size=360x640:rate=30:duration={seconds}",
            "-f", "lavfi", "-i", f"sine=frequency=440:sample_rate=44100:duration={seconds}",
            "-c:v", ffmpeg.encoder,
        ]
        if ffmpeg.encoder == "libx264":
            cmd += ["-preset", "veryfast", "-crf", "30"]
        elif ffmpeg.encoder == "mpeg4":
            cmd += ["-q:v", "6"]
        cmd += [
            "-pix_fmt", "yuv420p",
            "-c:a", "aac", "-b:a", "48k",
            "-shortest", "-movflags", "+faststart",
            "-metadata", f"title=fixture {item_id}",
            str(out),
        ]
        subprocess.run(cmd, check=True, capture_output=True, timeout=120)
        return out.read_bytes()
