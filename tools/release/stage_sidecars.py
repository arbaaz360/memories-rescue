#!/usr/bin/env python3
"""Stage the release sidecars for one target (used by .github/workflows/release.yml).

    python tools/release/stage_sidecars.py --target x86_64-pc-windows-msvc [--allow-dev]

Source of truth: tools/release/sidecars.lock.json, which pins, per target
triple, the minimal LGPL ffmpeg built by third_party/ffmpeg/build.sh and
published as a release of the public releases repository
(arbaaz360/memories-rescue-releases, tag `sidecars-ffmpeg-<version>`), with
its SHA-256. The file is downloaded, verified and handed to
`tools/sidecars.py stage-local`, which places it under third_party/ffmpeg/dist
and src-tauri/binaries/ with the name Tauri expects.

When a target has no pin yet (sha256 is null) the script fails, unless
--allow-dev is given and tools/sidecars.py knows a development build for the
target (the BtbN build on Windows): then that one is staged and
`MR_FFMPEG_KIND=dev` is written to $GITHUB_ENV so the release is labelled.
macOS has no development fallback: the minimal build must be published first.

Why fetch instead of build: the minimal ffmpeg takes 15 to 40 minutes to build
per platform and never changes between app releases; building it once,
publishing it with its source bundle, and pinning the hash keeps the release
workflow at a few minutes and makes every app release reproducible against a
known ffmpeg. The pins are the ffmpeg session's to update.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LOCK = REPO / "tools" / "release" / "sidecars.lock.json"
DIST = REPO / "third_party" / "ffmpeg" / "dist"


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def github_env(key: str, value: str) -> None:
    env_file = os.environ.get("GITHUB_ENV")
    if env_file:
        with open(env_file, "a", encoding="utf-8") as f:
            f.write(f"{key}={value}\n")
    print(f"  {key}={value}")


def download(url: str, dest: Path) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    req = urllib.request.Request(url, headers={"User-Agent": "memories-rescue-release/1"})
    with urllib.request.urlopen(req) as r, dest.open("wb") as f:
        shutil.copyfileobj(r, f, 1 << 20)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--target", required=True)
    ap.add_argument("--allow-dev", action="store_true")
    args = ap.parse_args()

    lock = json.loads(LOCK.read_text(encoding="utf-8"))
    entry = lock["targets"].get(args.target)
    sidecars_py = [sys.executable, str(REPO / "tools" / "sidecars.py")]

    if entry and entry.get("sha256"):
        url = f"https://github.com/{lock['repo']}/releases/download/{lock['release']}/{entry['asset']}"
        dest_dir = DIST / args.target
        exe = "ffmpeg.exe" if "windows" in args.target else "ffmpeg"
        dest = dest_dir / exe
        print(f"fetching {url}")
        download(url, dest)
        got = sha256_of(dest)
        if got != entry["sha256"]:
            dest.unlink()
            raise SystemExit(f"{entry['asset']}: sha256 {got} != pinned {entry['sha256']}")
        if not exe.endswith(".exe"):
            dest.chmod(0o755)
        (dest_dir / "BUILDINFO.txt").write_text(
            f"ffmpeg {lock['ffmpeg_version']} minimal LGPL build\nsource: {lock['repo']} release {lock['release']} asset {entry['asset']}\nsha256: {got}\n",
            encoding="utf-8",
        )
        subprocess.run(sidecars_py + ["stage-local", "--target", args.target], check=True)
        github_env("MR_FFMPEG_KIND", "minimal")
        github_env("MR_FFMPEG_VERSION", lock["ffmpeg_version"])
        return 0

    if args.allow_dev and "windows" in args.target:
        print(f"no pinned minimal ffmpeg for {args.target}; staging the development build (tools/sidecars.py fetch)")
        subprocess.run(sidecars_py + ["fetch", "--target", args.target], check=True)
        github_env("MR_FFMPEG_KIND", "dev")
        return 0

    raise SystemExit(
        f"no pinned minimal ffmpeg for {args.target} in {LOCK.relative_to(REPO)}: build it with "
        "third_party/ffmpeg/build.sh, publish it as release "
        f"{lock['release']} of {lock['repo']} and record its sha256 here"
    )


if __name__ == "__main__":
    sys.exit(main())
