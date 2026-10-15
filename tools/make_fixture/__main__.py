"""Generate synthetic Snapchat Memories export fixtures with ground truth.

    python tools/make_fixture --all
    python tools/make_fixture --fixture small-2026 --zip-order shuffled --seed 7

Writes <out>/<fixture>/<mydata~...zip> and <out>/<fixture>/truth.json.
Requires Python 3.10+ and Pillow; ffmpeg is optional and used for videos.
See tools/README.md.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from common import Context, detect_ffmpeg
from fixtures import BUILDERS

REPO_ROOT = Path(__file__).resolve().parents[2]


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="make_fixture", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--fixture", action="append", choices=sorted(BUILDERS), help="fixture to build (repeatable)")
    parser.add_argument("--all", action="store_true", help="build every fixture")
    parser.add_argument("--out", type=Path, default=REPO_ROOT / "fixtures", help="output directory (default: <repo>/fixtures)")
    parser.add_argument("--seed", type=int, default=1, help="random seed (default 1)")
    parser.add_argument(
        "--zip-order",
        choices=["json", "shuffled"],
        default="json",
        help="order of memories/ entries inside the ZIP: following the JSON array (default) or shuffled, to break the ordinal-matching assumption",
    )
    parser.add_argument("--no-video", action="store_true", help="skip video files even if ffmpeg is present")
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args(argv)

    names = sorted(BUILDERS) if args.all else (args.fixture or [])
    if not names:
        parser.error("choose --all or at least one --fixture")

    ffmpeg = None if args.no_video else detect_ffmpeg()
    ctx = Context(out_dir=args.out, seed=args.seed, zip_order=args.zip_order, ffmpeg=ffmpeg, quiet=args.quiet)
    if not args.quiet:
        if ffmpeg:
            print(f"ffmpeg: {ffmpeg.path} ({ffmpeg.encoder})")
        else:
            print("ffmpeg: not found or disabled; video main files will be skipped and noted in truth.json")
        print(f"output: {args.out}  seed: {args.seed}  zip-order: {args.zip_order}")

    for name in names:
        BUILDERS[name](ctx)
    return 0


if __name__ == "__main__":
    sys.exit(main())
