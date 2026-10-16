#!/usr/bin/env python3
"""Assemble the contents of the public releases repository.

    python tools/release/prepare_releases_repo.py --out ../memories-rescue-releases

Copies into --out:

    README.md                    docs/release/releases-repo/README.md
    LICENSE.md                   docs/release/releases-repo/LICENSE.md
    ffmpeg/COPYING.LGPLv2.1      third_party/ffmpeg/COPYING.LGPLv2.1 (the one copy of the text)
    ffmpeg/SOURCE_OFFER.txt      third_party/ffmpeg/SOURCE_OFFER.txt
    ffmpeg/README.md             third_party/ffmpeg/README.md (how the sidecar is built)

The owner creates the repository `arbaaz360/memories-rescue-releases` (public,
default branch `main`), copies these files in and pushes. Nothing in the
application repository keeps a second copy of the ffmpeg licence or the
source offer; this script is the "copy" step.
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
FILES = {
    "README.md": REPO / "docs" / "release" / "releases-repo" / "README.md",
    "LICENSE.md": REPO / "docs" / "release" / "releases-repo" / "LICENSE.md",
    "ffmpeg/COPYING.LGPLv2.1": REPO / "third_party" / "ffmpeg" / "COPYING.LGPLv2.1",
    "ffmpeg/SOURCE_OFFER.txt": REPO / "third_party" / "ffmpeg" / "SOURCE_OFFER.txt",
    "ffmpeg/README.md": REPO / "third_party" / "ffmpeg" / "README.md",
}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, required=True)
    args = ap.parse_args()
    missing = [str(src.relative_to(REPO)) for src in FILES.values() if not src.is_file()]
    if missing:
        raise SystemExit("missing: " + ", ".join(missing))
    for rel, src in FILES.items():
        dest = args.out / rel
        dest.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dest)
        print(f"  {rel}  <-  {src.relative_to(REPO)}")
    print(f"prepared {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
