#!/usr/bin/env python3
"""Fetch, verify and stage the sidecar binaries (ffmpeg, exiftool), or create
placeholders for CI.

    python tools/sidecars.py fetch          download, check SHA-256, extract, stage
    python tools/sidecars.py placeholders   empty files so tauri-build is satisfied
    python tools/sidecars.py verify         re-hash what is staged against the pins

Layout produced (all gitignored except third_party/README.md):

    third_party/_downloads/            verified archives, kept for re-staging
    third_party/exiftool/              exiftool.exe + exiftool_files/ (Perl runtime)
    third_party/ffmpeg/                ffmpeg.exe, ffprobe.exe, LICENSE.txt
    src-tauri/binaries/                what `bundle.externalBin` and `bundle.resources`
                                       point at: <name>-<target triple>.exe, exiftool_files/

Pins are per target triple. Only x86_64-pc-windows-msvc is pinned today; macOS
sidecars come with NovaByteWorks' own LGPL ffmpeg build (SRS §10.4) and are
not fetched by this script yet. Archives are extracted with Python's zipfile
into their own directories and nothing inside them is executed by this script.
"""

from __future__ import annotations

import argparse
import hashlib
import shutil
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
THIRD_PARTY = REPO / "third_party"
BINARIES = REPO / "src-tauri" / "binaries"

PINS = {
    "x86_64-pc-windows-msvc": {
        "exiftool": {
            "version": "13.59",
            # exiftool.org links its Windows zips to this SourceForge mirror; the
            # direct exiftool.org URL returns 404. Hash from https://exiftool.org/checksums.txt
            "url": "https://sourceforge.net/projects/exiftool/files/exiftool-13.59_64.zip/download",
            "archive": "exiftool-13.59_64.zip",
            "sha256": "44b512b25af500724ba579d0a53c8fc5851628b692dd5e5d94ae4a15c2cba9ec",
            "size": 11_183_675,
        },
        "ffmpeg": {
            "version": "n9.0.2-28-g669f5ab083-20261015",
            # BtbN/FFmpeg-Builds dated release; LGPL v3 static build, no --enable-gpl / --enable-nonfree.
            # Hash from the release's checksums.sha256.
            "url": "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-10-15-13-03/ffmpeg-n9.0.2-28-g669f5ab083-win64-lgpl-9.0.zip",
            "archive": "ffmpeg-n9.0.2-28-g669f5ab083-win64-lgpl-9.0.zip",
            "sha256": "d05ca4eeab509d631928ad26363597d3fb0dfc3ef4163980cfbfef784ca64e41",
            "size": 171_480_504,
        },
    }
}

# SHA-256 of the staged executables, for `verify`.
STAGED_SHA256 = {
    "x86_64-pc-windows-msvc": {
        "exiftool.exe": "68c079c32fdae0d6c7130e9a5fb73f8ac9dabdf9ab8da312da4f6c549d6d3385",
        "ffmpeg.exe": "d388e9a142bc2306dd0922570b9de9b59ae204748b60b6afdfff821ba6413ee9",
        "ffprobe.exe": "15bb93293f959105bf51911c81ce98bf864b6421419212e3db8e1842a83fd3d6",
    }
}


def host_triple() -> str:
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    for line in out.splitlines():
        if line.startswith("host:"):
            return line.split(":", 1)[1].strip()
    raise SystemExit("could not read host triple from rustc -vV")


def exe(triple: str, name: str) -> str:
    return f"{name}.exe" if "windows" in triple else name


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download(url: str, dest: Path, expected_sha: str, expected_size: int) -> None:
    if dest.exists() and sha256_of(dest) == expected_sha:
        print(f"  {dest.name}: already downloaded and verified")
        return
    dest.parent.mkdir(parents=True, exist_ok=True)
    print(f"  downloading {url}")
    req = urllib.request.Request(url, headers={"User-Agent": "memories-rescue-sidecars/1"})
    with urllib.request.urlopen(req) as r, dest.open("wb") as f:
        shutil.copyfileobj(r, f, 1 << 20)
    size = dest.stat().st_size
    if size != expected_size:
        dest.unlink()
        raise SystemExit(f"{dest.name}: size {size} != pinned {expected_size}")
    got = sha256_of(dest)
    if got != expected_sha:
        dest.unlink()
        raise SystemExit(f"{dest.name}: sha256 {got} != pinned {expected_sha}")
    print(f"  {dest.name}: {size} bytes, sha256 ok")


def extract(archive: Path, into: Path) -> None:
    if into.exists():
        shutil.rmtree(into)
    into.mkdir(parents=True)
    with zipfile.ZipFile(archive) as z:
        for info in z.infolist():
            # zip-slip guard
            target = (into / info.filename).resolve()
            if not str(target).startswith(str(into.resolve())):
                raise SystemExit(f"{archive.name}: refusing entry {info.filename!r}")
        z.extractall(into)


def stage(triple: str) -> None:
    pins = PINS.get(triple)
    if not pins:
        raise SystemExit(f"no sidecar pins for {triple}; see the docstring (macOS uses the §10.4 build)")
    downloads = THIRD_PARTY / "_downloads"
    extracts = THIRD_PARTY / "_extract"
    BINARIES.mkdir(parents=True, exist_ok=True)

    print("exiftool")
    p = pins["exiftool"]
    download(p["url"], downloads / p["archive"], p["sha256"], p["size"])
    extract(downloads / p["archive"], extracts / "exiftool")
    src_root = next((extracts / "exiftool").glob("exiftool-*_64"))
    dest = THIRD_PARTY / "exiftool"
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir()
    # The launcher is shipped as "exiftool(-k).exe"; the "-k" in the name means
    # "pause at exit". Renamed, it runs non-interactively. It needs exiftool_files/
    # (perl.exe + libraries) next to it.
    shutil.copy2(src_root / "exiftool(-k).exe", dest / "exiftool.exe")
    shutil.copytree(src_root / "exiftool_files", dest / "exiftool_files")
    shutil.copy2(src_root / "README.txt", dest / "README-upstream.txt")

    print("ffmpeg")
    p = pins["ffmpeg"]
    download(p["url"], downloads / p["archive"], p["sha256"], p["size"])
    extract(downloads / p["archive"], extracts / "ffmpeg")
    src_root = next((extracts / "ffmpeg").glob("ffmpeg-*"))
    dest = THIRD_PARTY / "ffmpeg"
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir()
    for name in ("ffmpeg.exe", "ffprobe.exe"):
        shutil.copy2(src_root / "bin" / name, dest / name)
    shutil.copy2(src_root / "LICENSE.txt", dest / "LICENSE.txt")

    print(f"staging src-tauri/binaries for {triple}")
    for item in BINARIES.iterdir():
        if item.is_dir():
            shutil.rmtree(item)
        else:
            item.unlink()
    shutil.copy2(THIRD_PARTY / "ffmpeg" / "ffmpeg.exe", BINARIES / exe(triple, "ffmpeg-" + triple))
    shutil.copy2(THIRD_PARTY / "exiftool" / "exiftool.exe", BINARIES / exe(triple, "exiftool-" + triple))
    shutil.copytree(THIRD_PARTY / "exiftool" / "exiftool_files", BINARIES / "exiftool_files")
    shutil.rmtree(extracts)
    verify(triple)


def verify(triple: str) -> None:
    expected = STAGED_SHA256.get(triple, {})
    ok = True
    for name, sha in expected.items():
        tool = "exiftool" if name.startswith("exiftool") else "ffmpeg"
        path = THIRD_PARTY / tool / name
        got = sha256_of(path) if path.is_file() else "missing"
        status = "ok" if got == sha else "MISMATCH"
        ok &= status == "ok"
        print(f"  {path.relative_to(REPO)}: {status}")
    for name in ("ffmpeg", "exiftool"):
        path = BINARIES / exe(triple, f"{name}-{triple}")
        present = path.is_file() and path.stat().st_size > 0
        ok &= present
        print(f"  {path.relative_to(REPO)}: {'ok' if present else 'missing or placeholder'}")
    if not ok:
        raise SystemExit("verification failed")


def placeholders(triple: str) -> None:
    """Empty sidecar files and an empty resource directory, so `cargo build`
    of src-tauri succeeds where the real binaries are not wanted (CI). The
    sidecar test treats zero-length files as "skip"."""
    BINARIES.mkdir(parents=True, exist_ok=True)
    for name in ("ffmpeg", "exiftool"):
        path = BINARIES / exe(triple, f"{name}-{triple}")
        if path.is_file() and path.stat().st_size > 0:
            print(f"  {path.relative_to(REPO)}: real binary present, left alone")
            continue
        path.write_bytes(b"")
        print(f"  {path.relative_to(REPO)}: placeholder")
    res = BINARIES / "exiftool_files"
    res.mkdir(exist_ok=True)
    marker = res / "PLACEHOLDER.txt"
    if not any(res.iterdir()):
        marker.write_text("placeholder for CI; run python tools/sidecars.py fetch\n")
        print(f"  {marker.relative_to(REPO)}: placeholder")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("command", choices=["fetch", "placeholders", "verify"])
    ap.add_argument("--target", help="target triple (default: rustc host)")
    args = ap.parse_args()
    triple = args.target or host_triple()
    print(f"target: {triple}")
    {"fetch": stage, "placeholders": placeholders, "verify": verify}[args.command](triple)
    return 0


if __name__ == "__main__":
    sys.exit(main())
