#!/usr/bin/env python3
"""Publish a checked draft release and move the beta channel forward.

    python tools/release/publish.py v1.0.0            # after reading the draft on GitHub
    python tools/release/publish.py v1.1.0-beta.1

Needs the `gh` CLI signed in to an account that can write releases in
arbaaz360/memories-rescue-releases (or GH_TOKEN set to such a token).

Why this exists: GitHub's `/releases/latest/` never points at a prerelease,
so a beta manifest attached to a prerelease is unreachable through that
path. The app therefore reads two fixed addresses
(src/config/links.json):

  stable  https://github.com/arbaaz360/memories-rescue-releases/releases/latest/download/latest.json
          the `latest.json` on the newest published stable release (GitHub resolves it)
  beta    https://github.com/arbaaz360/memories-rescue-releases/releases/download/channel-beta/latest-beta.json
          one asset on a fixed release tagged `channel-beta`, rewritten by this script

Steps:
1. Refuse unless the release exists, is a draft and has a `latest.json`
   (the owner checks drafts before publishing; nothing publishes itself).
2. Publish it (`gh release edit --draft=false`). Prereleases keep their
   prerelease flag, so they never become "latest".
3. Download its `latest.json` and upload it as `latest-beta.json` to the
   `channel-beta` release, creating that release as a prerelease if it is
   missing (so it can never become "latest" either). This runs for stable
   releases too: beta users move forward to the stable build that follows
   their beta; a user on the beta channel is never left behind.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
from pathlib import Path

RELEASES_REPO = "arbaaz360/memories-rescue-releases"
CHANNEL_BETA = "channel-beta"


def gh(*args: str, check: bool = True) -> subprocess.CompletedProcess:
    r = subprocess.run(["gh", *args], capture_output=True, text=True, encoding="utf-8")
    if check and r.returncode != 0:
        raise SystemExit(f"gh {' '.join(args)} failed:\n{r.stderr.strip()}")
    return r


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("tag")
    ap.add_argument("--repo", default=RELEASES_REPO)
    ap.add_argument("--keep-draft", action="store_true", help="only update channel-beta; do not publish")
    args = ap.parse_args()

    view = gh("release", "view", args.tag, "--repo", args.repo, "--json", "isDraft,isPrerelease,assets,name,url")
    rel = json.loads(view.stdout)
    assets = {a["name"] for a in rel["assets"]}
    print(f"{rel['name']}: draft={rel['isDraft']} prerelease={rel['isPrerelease']} assets={len(assets)}")
    if "latest.json" not in assets:
        raise SystemExit("the release has no latest.json (updater artifacts missing?); not publishing")
    if "BUILD-NOTES.txt" in assets:
        raise SystemExit("the release carries BUILD-NOTES.txt (unsigned or development ffmpeg); not publishing")

    if not args.keep_draft:
        if not rel["isDraft"]:
            print("already published; only the beta channel is updated")
        else:
            gh("release", "edit", args.tag, "--repo", args.repo, "--draft=false")
            print(f"published {rel['url']}")

    work = Path(tempfile.mkdtemp(prefix="mr-publish-"))
    gh("release", "download", args.tag, "--repo", args.repo, "--pattern", "latest.json", "--dir", str(work), "--clobber")
    manifest = work / "latest.json"
    data = json.loads(manifest.read_text(encoding="utf-8"))
    beta = work / "latest-beta.json"
    beta.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")

    exists = gh("release", "view", CHANNEL_BETA, "--repo", args.repo, check=False).returncode == 0
    if not exists:
        gh(
            "release", "create", CHANNEL_BETA, "--repo", args.repo, "--prerelease", "--title", "Beta channel",
            "--notes", "Fixed address for the beta update feed. `latest-beta.json` here always describes the newest "
            "published release, beta or stable. Not a download page: use the newest release instead.",
        )
        print(f"created release {CHANNEL_BETA} (prerelease)")
    gh("release", "upload", CHANNEL_BETA, str(beta), "--repo", args.repo, "--clobber")
    print(f"channel-beta/latest-beta.json now points at {data.get('version', '?')} ({args.tag})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
