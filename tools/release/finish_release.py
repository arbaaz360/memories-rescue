#!/usr/bin/env python3
"""Finish the draft release that tauri-action created (release.yml, job `publish`).

    python tools/release/finish_release.py --repo arbaaz360/memories-rescue-releases \
        --tag v1.0.0 --prerelease false --build-info build-info/*.json

Needs the `gh` CLI with GH_TOKEN set to a token that can write releases in
`--repo`. Steps, each idempotent so the job can be re-run:

1. Download every asset of the draft release and write `SHA256SUMS.txt`.
2. Prereleases: the updater manifest is served as `latest-beta.json`; the
   `latest.json` tauri-action uploaded is renamed so the stable channel never
   sees a prerelease.
3. If any build was unsigned or carried the development ffmpeg (from the
   per-target build-info files), upload `BUILD-NOTES.txt` saying so, put the
   same banner at the top of the release notes and "(unsigned build)" in the
   title. Such a release must not be published as is.
4. If the minimal ffmpeg was shipped, copy its source bundle (tarball,
   signature, build script, build records, licence) from the sidecar release
   named in tools/release/sidecars.lock.json into this release, which is the
   LGPL "equivalent access" the SOURCE_OFFER.txt promises.
5. Upload `SHA256SUMS.txt` last, so it covers everything above.
"""

from __future__ import annotations

import argparse
import glob
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
LOCK = REPO / "tools" / "release" / "sidecars.lock.json"


def gh(*args: str, check: bool = True) -> str:
    r = subprocess.run(["gh", *args], capture_output=True, text=True, encoding="utf-8")
    if check and r.returncode != 0:
        raise SystemExit(f"gh {' '.join(args)} failed:\n{r.stderr}")
    return r.stdout


def sha256_of(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", required=True)
    ap.add_argument("--tag", required=True)
    ap.add_argument("--prerelease", default="false")
    ap.add_argument("--build-info", nargs="*", default=[])
    args = ap.parse_args()

    infos = []
    for pattern in args.build_info:
        for f in glob.glob(pattern):
            infos.append(json.loads(Path(f).read_text(encoding="utf-8")))
    unsigned = [i["target"] for i in infos if not i.get("signed", False)]
    dev_ffmpeg = [i["target"] for i in infos if i.get("ffmpeg_kind") != "minimal"]

    release = json.loads(gh("release", "view", args.tag, "--repo", args.repo, "--json", "assets,body,name,isDraft"))
    asset_names = {a["name"] for a in release["assets"]}
    print(f"{args.repo} {args.tag}: {len(asset_names)} assets, draft={release['isDraft']}")

    work = Path(tempfile.mkdtemp(prefix="mr-release-"))

    # 2. Prerelease channel.
    if args.prerelease == "true" and "latest.json" in asset_names:
        gh("release", "download", args.tag, "--repo", args.repo, "--pattern", "latest.json", "--dir", str(work), "--clobber")
        (work / "latest.json").rename(work / "latest-beta.json")
        gh("release", "upload", args.tag, str(work / "latest-beta.json"), "--repo", args.repo, "--clobber")
        gh("release", "delete-asset", args.tag, "latest.json", "--repo", args.repo, "--yes")
        print("  latest.json -> latest-beta.json")

    # 4. ffmpeg source bundle next to the binaries that were built from it.
    lock = json.loads(LOCK.read_text(encoding="utf-8"))
    if infos and not dev_ffmpeg:
        src_dir = work / "ffmpeg-source"
        src_dir.mkdir()
        for name in lock["source_assets"]:
            r = subprocess.run(["gh", "release", "download", lock["release"], "--repo", lock["repo"], "--pattern", name, "--dir", str(src_dir), "--clobber"], capture_output=True, text=True)
            if r.returncode != 0:
                print(f"  warning: {name} missing from {lock['release']}: {r.stderr.strip()}")
                continue
            gh("release", "upload", args.tag, str(src_dir / name), "--repo", args.repo, "--clobber")
            print(f"  uploaded ffmpeg source asset {name}")

    # 3. Labels for builds that must not ship.
    banner_lines = []
    if unsigned:
        banner_lines.append(f"UNSIGNED BUILD: no code-signing secrets were present for {', '.join(unsigned)}. Do not publish; install only for testing.")
    if dev_ffmpeg:
        banner_lines.append(f"DEVELOPMENT FFMPEG: {', '.join(dev_ffmpeg)} carry the BtbN development build, not the minimal LGPL build of SRS 10.4.")
    if banner_lines:
        notes = work / "BUILD-NOTES.txt"
        notes.write_text("\n".join(banner_lines) + "\n", encoding="utf-8")
        gh("release", "upload", args.tag, str(notes), "--repo", args.repo, "--clobber")
        body = release.get("body") or ""
        banner = "> **" + " ".join(banner_lines) + "**\n\n"
        if not body.startswith("> **"):
            gh("release", "edit", args.tag, "--repo", args.repo, "--notes", banner + body)
        title = release.get("name") or args.tag
        if unsigned and "(unsigned build)" not in title:
            gh("release", "edit", args.tag, "--repo", args.repo, "--title", f"{title} (unsigned build)")
        print("  labelled: " + "; ".join(banner_lines))

    # 1 + 5. Checksums over everything now attached.
    release = json.loads(gh("release", "view", args.tag, "--repo", args.repo, "--json", "assets"))
    dl = work / "assets"
    dl.mkdir(exist_ok=True)
    lines = []
    for a in sorted(release["assets"], key=lambda a: a["name"]):
        if a["name"] == "SHA256SUMS.txt":
            continue
        gh("release", "download", args.tag, "--repo", args.repo, "--pattern", a["name"], "--dir", str(dl), "--clobber")
        lines.append(f"{sha256_of(dl / a['name'])}  {a['name']}")
    sums = work / "SHA256SUMS.txt"
    sums.write_text("\n".join(lines) + "\n", encoding="utf-8")
    gh("release", "upload", args.tag, str(sums), "--repo", args.repo, "--clobber")
    print(f"  SHA256SUMS.txt: {len(lines)} entries")
    return 0


if __name__ == "__main__":
    sys.exit(main())
