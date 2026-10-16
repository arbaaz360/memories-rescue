#!/usr/bin/env python3
"""Generate THIRD_PARTY_NOTICES.md (SRS NFR-LEG-03).

    python tools/release/notices.py            rewrite THIRD_PARTY_NOTICES.md
    python tools/release/notices.py --check    exit 1 if the committed file is stale (CI)

Sources, all local and deterministic:

- Rust: `cargo metadata --locked` for name, version, licence expression and
  repository; `cargo tree -p memories-rescue --target all -e normal,build`
  for the set of crates that can end up in the shipped app on any platform
  (dev-dependencies and the CLI-only crates are left out). Licence texts and
  copyright lines come from the LICENSE/COPYING files in each crate's source.
- npm: `package-lock.json` entries that are not dev-only, with licence and
  copyright lines from node_modules.
- Bundled data and sidecars: the time zone boundary data (ODbL, text in
  tools/release/licenses/) and the ffmpeg sidecar (LGPL v2.1+, text from
  third_party/ffmpeg/COPYING.LGPLv2.1, the copy FFmpeg ships).

The output is a resource of the app (`bundle.resources` in tauri.conf.json)
and is opened from the About screen.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
OUT = REPO / "THIRD_PARTY_NOTICES.md"
LICENSES = REPO / "tools" / "release" / "licenses"
# The LGPL text FFmpeg ships (third_party/ffmpeg/build.sh builds LGPL v2.1+): one copy, used everywhere.
LGPL_TEXT = REPO / "third_party" / "ffmpeg" / "COPYING.LGPLv2.1"
APP_PACKAGE = "memories-rescue"

# Crates whose licence expression does not tell the whole story.
NOTES = {
    "libsqlite3-sys": "Bundles SQLite, which is in the public domain (https://sqlite.org/copyright.html).",
    "rusqlite": "Job state database (SRS section 3.8) over the bundled SQLite.",
    "chrono-tz": "Bundles the IANA Time Zone Database (tzdb), which is in the public domain.",
    "tzf-rs": "Point-in-polygon lookup from coordinates to a zone name (FR-19).",
    "tzf-rel": "Carries the compiled time zone boundary data; the data itself is ODbL-1.0, see the data notice below.",
    "tzf-rel-lite": "Carries the compiled time zone boundary data; the data itself is ODbL-1.0, see the data notice below.",
    "little_exif": "EXIF writer for JPEG and PNG (SRS section 4.3).",
    "image": "Photo overlay compositing (FR-23).",
}

LICENSE_FILE_RE = re.compile(r"^(LICEN[CS]E|COPYING|NOTICE)(\b|[-_.])", re.IGNORECASE)
COPYRIGHT_RE = re.compile(r"^\s*(copyright|\(c\)|©)\s", re.IGNORECASE)


def run(cmd: list[str], cwd: Path = REPO) -> str:
    return subprocess.run(cmd, cwd=cwd, check=True, capture_output=True, text=True, encoding="utf-8").stdout


def norm(text: str) -> str:
    return text.replace("\r\n", "\n").replace("\r", "\n").strip("\n") + "\n"


# ---- Rust ---------------------------------------------------------------------


def rust_packages() -> list[dict]:
    meta = json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked"]))
    by_key = {(p["name"], p["version"]): p for p in meta["packages"]}
    workspace = {p["name"] for p in meta["packages"] if p["id"] in meta["workspace_members"]}

    tree = run(["cargo", "tree", "-p", APP_PACKAGE, "--target", "all", "-e", "normal,build", "--prefix", "none", "--format", "{p}"])
    wanted: set[tuple[str, str]] = set()
    for line in tree.splitlines():
        m = re.match(r"^(\S+) v(\S+)", line.strip())
        if m and m.group(1) not in workspace:
            wanted.add((m.group(1), m.group(2)))

    out = []
    for key in sorted(wanted):
        p = by_key.get(key)
        if p is None:
            raise SystemExit(f"{key[0]} {key[1]} is in cargo tree but not in cargo metadata")
        crate_dir = Path(p["manifest_path"]).parent
        files = license_files(crate_dir)
        out.append(
            {
                "name": p["name"],
                "version": p["version"],
                "license": p.get("license") or (f"see {p['license_file']}" if p.get("license_file") else "UNKNOWN"),
                "repository": p.get("repository") or "",
                "files": files,
                "copyrights": copyright_lines(files),
            }
        )
    return out


def license_files(directory: Path) -> list[Path]:
    if not directory.is_dir():
        return []
    files = [f for f in directory.iterdir() if f.is_file() and LICENSE_FILE_RE.match(f.name)]
    return sorted(files, key=lambda f: f.name.lower())


def copyright_lines(files: list[Path]) -> list[str]:
    lines: list[str] = []
    for f in files:
        try:
            text = f.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        for line in text.splitlines():
            if COPYRIGHT_RE.match(line):
                clean = re.sub(r"\s+", " ", line).strip()
                if 8 < len(clean) < 200 and clean not in lines:
                    lines.append(clean)
    return lines


# ---- npm ------------------------------------------------------------------------


def npm_packages() -> list[dict]:
    lock = json.loads((REPO / "package-lock.json").read_text(encoding="utf-8"))
    out = []
    for path, info in sorted(lock.get("packages", {}).items()):
        if not path or info.get("dev") or info.get("extraneous"):
            continue
        name = path.rsplit("node_modules/", 1)[-1]
        pkg_dir = REPO / path
        manifest = {}
        if (pkg_dir / "package.json").is_file():
            manifest = json.loads((pkg_dir / "package.json").read_text(encoding="utf-8"))
        lic = info.get("license") or manifest.get("license") or "UNKNOWN"
        if isinstance(lic, dict):
            lic = lic.get("type", "UNKNOWN")
        repo = manifest.get("repository") or ""
        if isinstance(repo, dict):
            repo = repo.get("url", "")
        repo = re.sub(r"^git\+|\.git$", "", repo).replace("git://", "https://").replace("ssh://git@", "https://")
        files = license_files(pkg_dir)
        out.append(
            {
                "name": name,
                "version": info.get("version") or manifest.get("version", ""),
                "license": lic,
                "repository": repo,
                "files": files,
                "copyrights": copyright_lines(files),
            }
        )
    return out


# ---- licence texts ----------------------------------------------------------------

# SPDX id -> file name fragments that identify its text in a crate's source tree.
TEXT_HINTS = {
    "MIT": ["LICENSE-MIT", "LICENSE.MIT", "MIT-LICENSE", "LICENCE-MIT"],
    "Apache-2.0": ["LICENSE-APACHE", "LICENSE.APACHE", "APACHE-LICENSE"],
    "BSD-3-Clause": ["LICENSE-BSD", "LICENSE.BSD"],
    "BSD-2-Clause": ["LICENSE-BSD", "LICENSE.BSD"],
    "ISC": ["LICENSE-ISC", "LICENSE.ISC"],
    "Zlib": ["LICENSE-ZLIB", "LICENSE.ZLIB"],
    "MPL-2.0": ["LICENSE-MPL", "LICENSE.MPL"],
    "Unicode-3.0": ["LICENSE-UNICODE", "LICENSE"],
    "Unlicense": ["UNLICENSE"],
    "0BSD": ["LICENSE-0BSD"],
    "BSL-1.0": ["LICENSE-BOOST", "LICENSE_1_0"],
    "CC0-1.0": ["LICENSE-CC0", "LICENSE"],
}
TEXT_MARKERS = {
    "MIT": "Permission is hereby granted, free of charge",
    "Apache-2.0": "Apache License",
    "BSD-3-Clause": "Redistributions of source code must retain",
    "BSD-2-Clause": "Redistributions of source code must retain",
    "ISC": "Permission to use, copy, modify, and/or distribute",
    "Zlib": "This software is provided 'as-is'",
    "MPL-2.0": "Mozilla Public License Version 2.0",
    "Unicode-3.0": "UNICODE LICENSE V3",
    "Unlicense": "This is free and unencumbered software",
    "0BSD": "Permission to use, copy, modify, and/or distribute",
    "BSL-1.0": "Boost Software License",
    "CC0-1.0": "CC0 1.0 Universal",
}


def spdx_ids(expression: str) -> list[str]:
    ids = re.findall(r"[A-Za-z0-9.+-]+", expression.replace("/", " OR "))
    return [i for i in ids if i.upper() not in {"OR", "AND", "WITH"} and not i.startswith("see")]


def find_text(spdx: str, packages: list[dict]) -> str | None:
    marker = TEXT_MARKERS.get(spdx)
    hints = TEXT_HINTS.get(spdx, [])
    best: tuple[int, str] | None = None
    for p in packages:
        if spdx not in spdx_ids(p["license"]):
            continue
        for f in p["files"]:
            upper = f.name.upper()
            if hints and not any(h in upper for h in hints):
                continue
            try:
                text = norm(f.read_text(encoding="utf-8", errors="replace"))
            except OSError:
                continue
            if marker and marker not in text:
                continue
            # Prefer the shortest text that carries the marker: it is the
            # canonical licence without extra project-specific preamble.
            score = len(text)
            if best is None or score < best[0]:
                best = (score, text)
    if best:
        return best[1]
    # Texts vendored under tools/release/licenses/ for licences whose crates
    # do not ship the text (MPL-2.0, Unlicense) or that point at a file.
    if spdx in ("LGPL-2.1-or-later", "LGPL-2.1"):
        path = LGPL_TEXT
    else:
        path = LICENSES / f"{spdx}.txt"
    if path.is_file():
        return norm(path.read_text(encoding="utf-8"))
    return None


# ---- rendering ----------------------------------------------------------------------


def table(rows: list[dict]) -> str:
    out = ["| Component | Version | Licence | Source |", "|---|---|---|---|"]
    for r in rows:
        src = f"<{r['repository']}>" if r["repository"] else ""
        note = NOTES.get(r["name"])
        lic = r["license"] + (f" ({note})" if note else "")
        out.append(f"| {r['name']} | {r['version']} | {lic} | {src} |")
    return "\n".join(out)


def render() -> str:
    rust = rust_packages()
    npm = npm_packages()
    everything = rust + npm

    by_id: dict[str, list[str]] = defaultdict(list)
    for p in everything:
        for i in spdx_ids(p["license"]):
            by_id[i].append(f"{p['name']} {p['version']}")

    parts: list[str] = []
    parts.append(
        "# Third-party notices\n\n"
        "Memories Rescue ships the components below. This file is generated by\n"
        "`tools/release/notices.py` from the lock files and the crates' own licence\n"
        "files; do not edit it by hand (CI checks that it is current). Where a\n"
        "component is offered under a choice of licences, NovaByteWorks uses it\n"
        "under the first one listed unless noted.\n"
    )

    parts.append("## Bundled data and separate programs\n")
    parts.append(
        "### ffmpeg (separate program, LGPL v2.1 or later)\n\n"
        "The app starts `ffmpeg` (`ffmpeg.exe` on Windows) as a separate process to add\n"
        "overlays to videos, convert HEVC videos to H.264 and join clips. Releases ship\n"
        "NovaByteWorks' own build of FFmpeg 9.0.2 configured with only the components the\n"
        "app uses, without `--enable-gpl` and without `--enable-nonfree`. FFmpeg is licensed\n"
        "under the GNU Lesser General Public License version 2.1 or later; the full text is\n"
        "below and is installed next to the program together with `SOURCE_OFFER.txt`, which\n"
        "says where the complete corresponding source, the build script and the configure\n"
        "options are published (the `ffmpeg/` folder of each release at\n"
        "<https://github.com/arbaaz360/memories-rescue-releases>) and makes the written\n"
        "offer required by the licence. Nothing from FFmpeg is linked into the app and the\n"
        "user may replace the program with another build. Development builds of the app\n"
        "may instead carry the BtbN FFmpeg build (LGPL v3, see `third_party/README.md`);\n"
        "those builds are not released.\n\n"
        "FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project.\n"
    )
    parts.append(
        "### Time zone boundary data (ODbL-1.0)\n\n"
        "The time zone boundary data bundled in this application (via the `tzf-rs` crate\n"
        "and its `tzf-rel` data package) is made available under the Open Database License\n"
        "(ODbL) v1.0, <https://opendatacommons.org/licenses/odbl/1-0/>, whose text is below.\n"
        "It is derived from timezone-boundary-builder by Evan Siroky\n"
        "(<https://github.com/evansiroky/timezone-boundary-builder>), which is in turn derived\n"
        "from OpenStreetMap data, copyright OpenStreetMap contributors\n"
        "(<https://www.openstreetmap.org/copyright>). The data is used unmodified apart from\n"
        "the precision reduction performed by the tzf project; no derivative database is\n"
        "distributed. Any rights in individual contents of the database are licensed under\n"
        "the Database Contents License, <https://opendatacommons.org/licenses/dbcl/1-0/>.\n"
        "The IANA Time Zone Database used for daylight-saving rules is in the public domain.\n"
    )
    parts.append(
        "### Not bundled\n\n"
        "ExifTool (Perl Artistic License or GPL) is used as a test oracle during development\n"
        "only and is not part of the released application (see\n"
        "`docs/decisions/Q-02-metadata-writer.md`).\n"
    )

    parts.append(f"## Rust crates ({len(rust)})\n\nCrates that can be compiled into the application on any supported platform.\n\n{table(rust)}\n")
    parts.append(f"## npm packages ({len(npm)})\n\nPackages bundled into the window's JavaScript.\n\n{table(npm)}\n")

    parts.append("## Copyright notices\n\nCopyright lines found in the components' licence files, by component.\n")
    for p in everything:
        if p["copyrights"]:
            parts.append(f"**{p['name']} {p['version']}**  ")
            parts.append("\n".join(f"{c}  " for c in p["copyrights"]) + "\n")

    parts.append("## Licence texts\n")
    for spdx in sorted(by_id):
        users = sorted(set(by_id[spdx]))
        parts.append(f"### {spdx}\n\nUsed by {len(users)} component(s): " + ", ".join(users) + ".\n")
        text = find_text(spdx, everything)
        if text is None:
            parts.append(f"_The text of {spdx} is not shipped inside the component sources; see <https://spdx.org/licenses/{spdx}.html>._\n")
        else:
            parts.append("```text\n" + text + "```\n")

    parts.append("### GNU Lesser General Public License v2.1 (ffmpeg)\n\n```text\n" + norm(LGPL_TEXT.read_text(encoding="utf-8")) + "```\n")
    parts.append("### Open Database License v1.0 (time zone boundary data)\n\n```text\n" + norm((LICENSES / "ODbL-1.0.txt").read_text(encoding="utf-8")) + "```\n")

    return "\n".join(parts).rstrip("\n") + "\n"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--check", action="store_true", help="compare with the committed file instead of writing")
    ap.add_argument("--out", type=Path, default=OUT)
    args = ap.parse_args()
    text = render()
    if args.check:
        current = args.out.read_text(encoding="utf-8").replace("\r\n", "\n") if args.out.is_file() else ""
        if current != text:
            a, b = hashlib.sha256(current.encode()).hexdigest()[:12], hashlib.sha256(text.encode()).hexdigest()[:12]
            print(f"{args.out.name} is stale ({a} committed, {b} generated); run python tools/release/notices.py", file=sys.stderr)
            return 1
        print(f"{args.out.name} is current")
        return 0
    args.out.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote {args.out} ({len(text.encode()) // 1024} KiB)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
