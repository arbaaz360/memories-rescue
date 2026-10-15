#!/usr/bin/env python3
"""Build the Q-02 benchmark corpus (SRS M1 metadata-writer decision).

Two corpora, both synthetic:

  <out>/fixture/jpeg/NNN.jpg   copies of the F-01 fixture photos (small, ~20 KB)
  <out>/fixture/mp4/NNN.mp4    copies of the F-01 fixture clips (2 s test pattern, ~23 KB)
  <out>/real/jpeg/NNN.jpg      1080x1920 smooth-noise JPEGs (~1 MB, phone-photo sized)
  <out>/real/mp4/NNN.mp4       1080x1920 10 s H.264 clips, ~4-5 MB, faststart (moov
                               before mdat) on even numbers, moov at the end on odd

The "fixture" corpus is what the SRS brief names; the "real" corpus has the byte
counts of an actual export, which is what dominates a metadata rewrite.

Usage: python tools/bench_corpus.py [--out fixtures/bench] [--jpegs 500] [--videos 100]
Needs Pillow; needs an ffmpeg with an H.264 encoder on PATH for real/mp4.
"""

from __future__ import annotations

import argparse
import io
import random
import shutil
import subprocess
import sys
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]


def extract_fixture_media(zip_path: Path) -> tuple[list[bytes], list[bytes]]:
    jpegs: list[bytes] = []
    mp4s: list[bytes] = []
    with zipfile.ZipFile(zip_path) as z:
        for info in z.infolist():
            name = info.filename.lower()
            if not name.startswith("memories/") or "-overlay" in name:
                continue
            if name.endswith(".jpg"):
                jpegs.append(z.read(info))
            elif name.endswith(".mp4"):
                mp4s.append(z.read(info))
    return jpegs, mp4s


def write_copies(blobs: list[bytes], out_dir: Path, count: int, ext: str) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    for i in range(count):
        (out_dir / f"{i:04d}{ext}").write_bytes(blobs[i % len(blobs)])


def make_real_jpegs(out_dir: Path, count: int, variants: int = 4, seed: int = 1) -> None:
    from PIL import Image, ImageFilter

    rng = random.Random(seed)
    blobs: list[bytes] = []
    for _ in range(variants):
        small = Image.frombytes("RGB", (270, 480), bytes(rng.getrandbits(8) for _ in range(270 * 480 * 3)))
        img = small.resize((1080, 1920), Image.BICUBIC).filter(ImageFilter.GaussianBlur(1))
        buf = io.BytesIO()
        img.save(buf, "JPEG", quality=90)
        blobs.append(buf.getvalue())
    write_copies(blobs, out_dir, count, ".jpg")


def make_real_mp4s(out_dir: Path, count: int, ffmpeg: str) -> None:
    out_dir.mkdir(parents=True, exist_ok=True)
    tmp = out_dir.parent / "_tmp"
    tmp.mkdir(exist_ok=True)
    variants: list[bytes] = []
    for faststart in (True, False):
        target = tmp / ("faststart.mp4" if faststart else "moov_last.mp4")
        cmd = [
            ffmpeg, "-hide_banner", "-loglevel", "error", "-y", "-nostdin",
            "-f", "lavfi", "-i", "testsrc2=size=1080x1920:rate=30",
            "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000",
            "-t", "10", "-c:v", "libx264", "-preset", "veryfast", "-b:v", "3500k",
            "-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "96k",
        ]
        if faststart:
            cmd += ["-movflags", "+faststart"]
        cmd.append(str(target))
        subprocess.run(cmd, check=True)
        variants.append(target.read_bytes())
    write_copies(variants, out_dir, count, ".mp4")
    shutil.rmtree(tmp)


def atom_order(path: Path) -> str:
    """Top-level box order, e.g. 'ftyp moov free mdat'."""
    names = []
    with path.open("rb") as f:
        pos = 0
        size = path.stat().st_size
        while pos + 8 <= size:
            f.seek(pos)
            hdr = f.read(16)
            box_size = int.from_bytes(hdr[:4], "big")
            name = hdr[4:8].decode("latin-1")
            if box_size == 1:
                box_size = int.from_bytes(hdr[8:16], "big")
            elif box_size == 0:
                box_size = size - pos
            names.append(name)
            if box_size < 8:
                break
            pos += box_size
    return " ".join(names)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=REPO / "fixtures" / "bench")
    ap.add_argument("--fixture-zip", type=Path, default=REPO / "fixtures" / "small-2026b" / "mydata~1760000000000.zip")
    ap.add_argument("--jpegs", type=int, default=500)
    ap.add_argument("--videos", type=int, default=100)
    ap.add_argument("--ffmpeg", default=shutil.which("ffmpeg"))
    args = ap.parse_args()

    if not args.fixture_zip.exists():
        print(f"fixture zip missing: {args.fixture_zip} (run: python tools/make_fixture --all)", file=sys.stderr)
        return 2
    if args.out.exists():
        shutil.rmtree(args.out)

    jpegs, mp4s = extract_fixture_media(args.fixture_zip)
    print(f"fixture media: {len(jpegs)} jpeg, {len(mp4s)} mp4")
    write_copies(jpegs, args.out / "fixture" / "jpeg", args.jpegs, ".jpg")
    if mp4s:
        write_copies(mp4s, args.out / "fixture" / "mp4", args.videos, ".mp4")
        print("fixture mp4 atom order:", atom_order(args.out / "fixture" / "mp4" / "0000.mp4"))
    else:
        print("fixture zip has no videos (generated without ffmpeg); fixture/mp4 skipped")

    make_real_jpegs(args.out / "real" / "jpeg", args.jpegs)
    if args.ffmpeg:
        make_real_mp4s(args.out / "real" / "mp4", args.videos, args.ffmpeg)
        print("real mp4 atom order (even):", atom_order(args.out / "real" / "mp4" / "0000.mp4"))
        print("real mp4 atom order (odd): ", atom_order(args.out / "real" / "mp4" / "0001.mp4"))
    else:
        print("no ffmpeg on PATH; real/mp4 skipped")

    for sub in ("fixture/jpeg", "fixture/mp4", "real/jpeg", "real/mp4"):
        d = args.out / sub
        if d.exists():
            files = list(d.iterdir())
            total = sum(f.stat().st_size for f in files)
            print(f"{sub:13} {len(files):4} files  {total / 1e6:8.1f} MB  avg {total / len(files) / 1e3:7.1f} KB")
    return 0


if __name__ == "__main__":
    sys.exit(main())
