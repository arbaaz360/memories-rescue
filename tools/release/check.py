#!/usr/bin/env python3
"""Release checklist (SRS section 10.5) as a script.

    python tools/release/check.py                      pre-build checks (CI runs this)
    python tools/release/check.py --tag v1.0.0         also require the tag to match the version
    python tools/release/check.py --bundles             post-build: list the installers, verify signatures

Pre-build checks (always):
  1. One version everywhere: package.json, package-lock.json, Cargo.toml
     [workspace.package], src-tauri/tauri.conf.json; equal to --tag minus the
     leading "v" when given (a prerelease suffix such as -beta.1 is allowed).
  2. CHANGELOG.md has a "## [<version>]" entry (Keep a Changelog layout).
  3. THIRD_PARTY_NOTICES.md is current (tools/release/notices.py --check);
     skipped with a note when node_modules is absent.
  4. The release config overlay src-tauri/tauri.release.conf.json parses and
     keeps the bundle identifier and product name of the base config.

Post-build checks (--bundles, after `tauri build`):
  5. Every installer and updater artifact under the bundle directory is
     listed with its size.
  6. Signatures. Windows: `signtool verify /pa /v <file>` on the installers,
     the app executable and each sidecar; expected output ends in
     "Successfully verified" and "Number of errors: 0". macOS:
     `codesign --verify --deep --strict --verbose=2 <app>` ("valid on disk",
     "satisfies its Designated Requirement"), `spctl -a -vv -t exec <app>`
     ("accepted", "source=Notarized Developer ID") and
     `xcrun stapler validate <dmg>` ("The validate action worked!").
     With --unsigned, or when the build reports no signing secrets, these
     are reported as skipped instead of failed.

Exit code 1 when any check fails.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import re
import shutil
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
RESULTS: list[tuple[str, str, str]] = []  # (status, check, detail)


def ok(check: str, detail: str = "") -> None:
    RESULTS.append(("ok", check, detail))


def fail(check: str, detail: str = "") -> None:
    RESULTS.append(("FAIL", check, detail))


def skip(check: str, detail: str = "") -> None:
    RESULTS.append(("skip", check, detail))


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def cargo_workspace_version() -> str | None:
    text = (REPO / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r"\[workspace\.package\](.*?)(\n\[|\Z)", text, re.S)
    if not m:
        return None
    v = re.search(r'^version\s*=\s*"([^"]+)"', m.group(1), re.M)
    return v.group(1) if v else None


def check_versions(tag: str | None) -> str | None:
    versions = {
        "package.json": read_json(REPO / "package.json").get("version"),
        "package-lock.json": read_json(REPO / "package-lock.json").get("version"),
        "Cargo.toml [workspace.package]": cargo_workspace_version(),
        "src-tauri/tauri.conf.json": read_json(REPO / "src-tauri" / "tauri.conf.json").get("version"),
    }
    distinct = {v for v in versions.values() if v is not None}
    detail = ", ".join(f"{k}={v}" for k, v in versions.items())
    if len(distinct) != 1:
        fail("one version everywhere", detail)
        return None
    version = distinct.pop()
    if tag is not None:
        expected = tag[1:] if tag.startswith("v") else tag
        if expected != version:
            fail("tag matches version", f"tag {tag} vs version {version}")
            return version
        ok("tag matches version", f"{tag} == {version}")
    ok("one version everywhere", version)
    return version


def check_changelog(version: str | None) -> None:
    path = REPO / "CHANGELOG.md"
    if not path.is_file():
        fail("CHANGELOG.md present", "missing")
        return
    if version is None:
        skip("changelog entry", "no single version")
        return
    text = path.read_text(encoding="utf-8")
    if re.search(rf"^## \[{re.escape(version)}\]", text, re.M):
        ok("changelog entry", f"## [{version}]")
    else:
        fail("changelog entry", f"no '## [{version}]' heading in CHANGELOG.md")


def check_notices() -> None:
    if not (REPO / "node_modules").is_dir():
        skip("notices current", "node_modules absent; run npm ci first")
        return
    r = subprocess.run([sys.executable, str(REPO / "tools" / "release" / "notices.py"), "--check"], capture_output=True, text=True)
    if r.returncode == 0:
        ok("notices current")
    else:
        fail("notices current", (r.stderr or r.stdout).strip().splitlines()[-1] if (r.stderr or r.stdout).strip() else "stale")


def check_overlay() -> None:
    base = read_json(REPO / "src-tauri" / "tauri.conf.json")
    overlay_path = REPO / "src-tauri" / "tauri.release.conf.json"
    if not overlay_path.is_file():
        fail("release overlay parses", "src-tauri/tauri.release.conf.json missing")
        return
    try:
        overlay = read_json(overlay_path)
    except ValueError as e:
        fail("release overlay parses", str(e))
        return
    for key in ("identifier", "productName"):
        if key in overlay and overlay[key] != base.get(key):
            fail("release overlay keeps identity", f"{key} differs from tauri.conf.json")
            return
    ok("release overlay parses", "identifier and productName unchanged")


# ---- post-build ------------------------------------------------------------------

INSTALLER_GLOBS = ["nsis/*.exe", "msi/*.msi", "dmg/*.dmg", "macos/*.app", "nsis/*.sig", "msi/*.sig", "macos/*.tar.gz", "macos/*.tar.gz.sig"]


def artifacts(bundle_dir: Path) -> list[Path]:
    found: list[Path] = []
    for g in INSTALLER_GLOBS:
        found.extend(sorted(bundle_dir.glob(g)))
    return found


def size_of(path: Path) -> int:
    if path.is_dir():
        return sum(f.stat().st_size for f in path.rglob("*") if f.is_file())
    return path.stat().st_size


def run_capture(cmd: list[str]) -> tuple[int, str]:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
    except FileNotFoundError:
        return 127, f"{cmd[0]} not found"
    return r.returncode, (r.stdout + r.stderr)


def check_bundles(bundle_dir: Path, unsigned: bool, sidecar_names: list[str]) -> None:
    if not bundle_dir.is_dir():
        fail("bundle directory", f"{bundle_dir} does not exist")
        return
    found = artifacts(bundle_dir)
    if not found:
        fail("installers present", f"nothing under {bundle_dir}")
        return
    rows = [f"{p.relative_to(bundle_dir)}: {size_of(p) / (1 << 20):.1f} MiB" for p in found]
    ok("installers present", "; ".join(rows))

    release_dir = bundle_dir.parent
    system = platform.system()
    if unsigned:
        skip("signatures", "unsigned build (no signing secrets)")
        return
    if system == "Windows":
        signtool = shutil.which("signtool") or find_signtool()
        if not signtool:
            fail("signtool available", "signtool.exe not found (Windows SDK)")
            return
        targets = [p for p in found if p.suffix.lower() in {".exe", ".msi"}]
        targets += [release_dir / "memories-rescue.exe"] + [release_dir / f"{n}.exe" for n in sidecar_names]
        for t in targets:
            if not t.is_file():
                fail("signature", f"{t.name}: file missing")
                continue
            code, out = run_capture([signtool, "verify", "/pa", "/v", str(t)])
            if code == 0 and "Successfully verified" in out:
                ok("signature", f"{t.name}: Successfully verified")
            else:
                fail("signature", f"{t.name}: {out.strip().splitlines()[-1] if out.strip() else code}")
    elif system == "Darwin":
        apps = [p for p in found if p.suffix == ".app"]
        dmgs = [p for p in found if p.suffix == ".dmg"]
        for app in apps:
            code, out = run_capture(["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app)])
            (ok if code == 0 else fail)("codesign verify", f"{app.name}: {out.strip().splitlines()[-1] if out.strip() else code}")
            code, out = run_capture(["spctl", "-a", "-vv", "-t", "exec", str(app)])
            (ok if code == 0 and "Notarized Developer ID" in out else fail)("spctl", f"{app.name}: {out.strip()}")
            for n in sidecar_names:
                side = app / "Contents" / "MacOS" / n
                code, out = run_capture(["codesign", "-dvv", str(side)])
                (ok if code == 0 and "Developer ID Application" in out else fail)("sidecar signature", f"{n}: {'Developer ID' if code == 0 else out.strip()}")
        for dmg in dmgs:
            code, out = run_capture(["xcrun", "stapler", "validate", str(dmg)])
            (ok if code == 0 else fail)("stapler validate", f"{dmg.name}: {out.strip().splitlines()[-1] if out.strip() else code}")
    else:
        skip("signatures", f"no signature check on {system}")


def find_signtool() -> str | None:
    kits = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Windows Kits" / "10" / "bin"
    if not kits.is_dir():
        return None
    candidates = sorted(kits.glob("10.*/x64/signtool.exe"))
    return str(candidates[-1]) if candidates else None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--tag", help="release tag, e.g. v1.0.0")
    ap.add_argument("--bundles", action="store_true", help="post-build checks on the bundle directory")
    ap.add_argument("--bundle-dir", type=Path, default=REPO / "target" / "release" / "bundle")
    ap.add_argument("--unsigned", action="store_true", help="the build had no signing secrets; report signature checks as skipped")
    ap.add_argument("--sidecars", default="ffmpeg", help="comma-separated sidecar names to verify (default: ffmpeg)")
    args = ap.parse_args()

    version = check_versions(args.tag)
    check_changelog(version)
    check_notices()
    check_overlay()
    if args.bundles:
        check_bundles(args.bundle_dir, args.unsigned or os.environ.get("MR_RELEASE_SIGNED") == "false", [s for s in args.sidecars.split(",") if s])

    width = max(len(c) for _, c, _ in RESULTS)
    for status, check, detail in RESULTS:
        print(f"{status:>4}  {check:<{width}}  {detail}")
    failed = [r for r in RESULTS if r[0] == "FAIL"]
    print(f"\n{len(RESULTS) - len(failed)} passed or skipped, {len(failed)} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
