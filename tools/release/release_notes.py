#!/usr/bin/env python3
"""Print the CHANGELOG.md section for one version (the GitHub release body).

    python tools/release/release_notes.py 1.0.0

Takes everything between "## [1.0.0]" and the next "## [" heading. Exits 1
when the entry is missing, which tools/release/check.py also reports.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]


def section(version: str) -> str | None:
    text = (REPO / "CHANGELOG.md").read_text(encoding="utf-8")
    m = re.search(rf"^## \[{re.escape(version)}\][^\n]*\n(.*?)(?=^## \[|\Z)", text, re.M | re.S)
    return m.group(1).strip() + "\n" if m else None


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    body = section(sys.argv[1])
    if body is None:
        print(f"no '## [{sys.argv[1]}]' entry in CHANGELOG.md", file=sys.stderr)
        return 1
    sys.stdout.write(body)
    return 0


if __name__ == "__main__":
    sys.exit(main())
