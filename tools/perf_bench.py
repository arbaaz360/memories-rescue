#!/usr/bin/env python3
"""Performance and soak runs behind docs/perf/README.md.

    python tools/perf_bench.py --work <scratch> [--only fr06,h05,perf03,h01,h02,h03] [--ffmpeg PATH]

What it runs (all on synthetic data from tools/make_fixture's scale builders,
generated into <work>/fx when missing):

  fr06    FR-06 AC1: `mr-cli bench index` over 50,000 entries in 5 parts
  h05     H-05: `mr-cli bench index` over 200,000 JSON entries
  perf03  NFR-PERF-03: metadata-only runs over phone-sized photos and clips,
          4 workers and 1 worker
  h01     H-01 scaled: 50,000 items in 12 parts, with the compositor when
          --ffmpeg is given; then `mr-cli bench verify` and a decode of every
          output (Pillow for photos, ffmpeg for videos)
  h02     H-02: the same job killed 10 times at random points and resumed;
          NFR-PERF-06 from each resume's time to its first finished item
  h03     H-03: debug build (the free-space hook is debug-only): the
          precheck at 60% of the needed space, then a forced run on a drive
          that fills as the job writes

Memory: the harness polls the mr-cli process tree (the engine and any ffmpeg
it starts) every 100 ms with psutil and keeps the largest sum of resident
memory; the engine also reports its own peak working set (Windows) or
ru_maxrss (macOS) through MR_BENCH_JSON.

Needs Python with psutil and Pillow, `cargo build -p mr-cli` and
`cargo build --release -p mr-cli`, and an ffmpeg on PATH for decoding.
Writes <work>/results.json and prints Markdown tables. Delete <work>
afterwards; the full set needs about 8 GB.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import glob
import json
import os
import random
import shutil
import subprocess
import sys
import time
from pathlib import Path

import psutil

REPO = Path(__file__).resolve().parents[1]
EXE = ".exe" if os.name == "nt" else ""


def cli(release: bool) -> str:
    return str(REPO / "target" / ("release" if release else "debug") / f"mr-cli{EXE}")


def zips(fx: Path, name: str) -> list[str]:
    return sorted(glob.glob(str(fx / name / "*.zip")))


def truth(fx: Path, name: str) -> dict:
    return json.loads((fx / name / "truth.json").read_text(encoding="utf-8"))


def ensure_fixtures(fx: Path, names: list[str]) -> None:
    missing = [n for n in names if not (fx / n / "truth.json").is_file()]
    if missing:
        args = [sys.executable, str(REPO / "tools" / "make_fixture"), "--out", str(fx)]
        for n in missing:
            args += ["--fixture", n]
        subprocess.run(args, check=True)


class Measured:
    def __init__(self) -> None:
        self.wall = 0.0
        self.tree_peak = 0
        self.engine_peak = 0
        self.returncode: int | None = None
        self.killed = False
        self.stdout = ""


def run_measured(cmd: list[str], env: dict, kill_after: float | None = None, cwd: Path | None = None) -> Measured:
    m = Measured()
    t0 = time.perf_counter()
    proc = psutil.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, cwd=cwd)
    out_chunks: list[bytes] = []

    def drain():
        for line in proc.stdout:
            out_chunks.append(line)

    reader = concurrent.futures.ThreadPoolExecutor(max_workers=1).submit(drain)
    while proc.poll() is None:
        try:
            total = proc.memory_info().rss
            peak = getattr(proc.memory_info(), "peak_wset", 0)
            m.engine_peak = max(m.engine_peak, peak)
            for c in proc.children(recursive=True):
                try:
                    total += c.memory_info().rss
                except psutil.Error:
                    pass
            m.tree_peak = max(m.tree_peak, total)
        except psutil.Error:
            pass
        if kill_after is not None and time.perf_counter() - t0 >= kill_after:
            for c in proc.children(recursive=True):
                try:
                    c.kill()
                except psutil.Error:
                    pass
            proc.kill()
            m.killed = True
            break
        time.sleep(0.1)
    proc.wait()
    reader.result(timeout=30)
    m.wall = time.perf_counter() - t0
    m.returncode = proc.returncode
    m.stdout = b"".join(out_chunks).decode("utf-8", "replace")
    return m


def bench_lines(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    return [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]


def last_command(path: Path) -> dict:
    lines = [l for l in bench_lines(path) if "command" in l]
    return lines[-1] if lines else {}


def mb(b: int | None) -> str:
    return "-" if not b else f"{b / 1e6:.0f} MB"


def job_id_of(stdout: str) -> str | None:
    for line in stdout.splitlines():
        if line.startswith("job "):
            return line.split()[1]
    return None


def decode_outputs(root: Path, ffmpeg: str | None) -> dict:
    """Decode every photo with Pillow and every video with ffmpeg."""
    from PIL import Image

    photos, videos = [], []
    for dirpath, dirnames, filenames in os.walk(root):
        rel = os.path.relpath(dirpath, root)
        if rel.startswith(".memories-rescue") or rel.startswith("_report"):
            dirnames[:] = []
            continue
        for f in filenames:
            p = os.path.join(dirpath, f)
            low = f.lower()
            if low.endswith((".jpg", ".jpeg", ".png")):
                photos.append(p)
            elif low.endswith((".mp4", ".mov")):
                videos.append(p)
    bad_photos = []
    for p in photos:
        try:
            with Image.open(p) as im:
                im.load()
        except Exception as e:  # noqa: BLE001
            bad_photos.append(f"{p}: {e}")
    bad_videos = []
    if ffmpeg:
        def dec(p):
            r = subprocess.run([ffmpeg, "-v", "error", "-nostdin", "-i", p, "-f", "null", "-"], capture_output=True, text=True)
            return p if (r.returncode != 0 or r.stderr.strip()) else None

        with concurrent.futures.ThreadPoolExecutor(max_workers=os.cpu_count() or 4) as ex:
            bad_videos = [p for p in ex.map(dec, videos) if p]
    return {
        "photos": len(photos),
        "photos_bad": len(bad_photos),
        "videos": len(videos),
        "videos_decoded": len(videos) if ffmpeg else 0,
        "videos_bad": len(bad_videos),
        "bad_sample": (bad_photos + bad_videos)[:5],
    }


def verify(release: bool, job: str, state: Path, env: dict) -> dict:
    r = subprocess.run([cli(release), "bench", "verify", job, "--state-dir", str(state)], capture_output=True, text=True, env=env)
    out = json.loads(r.stdout) if r.stdout.strip().startswith("{") else {"error": r.stderr}
    out["ok"] = r.returncode == 0
    return out


def fresh(path: Path) -> Path:
    if path.exists():
        shutil.rmtree(path)
    path.mkdir(parents=True)
    return path


# ---------------------------------------------------------------------------


def do_index(work: Path, fx: Path, name: str, env: dict) -> dict:
    m = run_measured([cli(True), "bench", "index", *zips(fx, name)], env)
    data = json.loads(m.stdout)
    data["tree_peak_bytes"] = m.tree_peak
    data["wall_s"] = m.wall
    return data


def do_perf03(work: Path, fx: Path, env: dict) -> list[dict]:
    rows = []
    for name in ("perf-photos", "perf-videos"):
        for workers in (None, 1):
            base = fresh(work / "runs" / f"{name}-{workers or 'default'}")
            bench = base / "bench.jsonl"
            e = dict(env, MR_BENCH_JSON=str(bench))
            e.pop("MR_FFMPEG", None)
            cmd = [cli(True), "run", *zips(fx, name), "--dest", str(base / "dest"), "--state-dir", str(base / "state"),
                   "--unlocked", "--home-tz", "Europe/London", "--overlay-mode", "save-separate", "--quiet"]
            if workers:
                cmd += ["--workers", str(workers)]
            m = run_measured(cmd, e)
            line = last_command(bench)
            n = (line.get("outcome") or {}).get("stats", {}).get("processed_this_run", 0)
            first = line.get("first_item_s") or 0.0
            elapsed = line.get("elapsed_s") or m.wall
            rows.append({
                "fixture": name,
                "workers": workers or "default (4)",
                "items": n,
                "elapsed_s": elapsed,
                "first_item_s": first,
                "items_per_s_overall": n / elapsed if elapsed else 0,
                "items_per_s_processing": (n - 1) / (elapsed - first) if elapsed > first and n > 1 else 0,
                "engine_peak_bytes": line.get("peak_rss_bytes"),
                "tree_peak_bytes": m.tree_peak,
                "input_bytes": truth(fx, name)["counts"]["input_bytes"],
            })
            shutil.rmtree(base / "dest", ignore_errors=True)
    return rows


def h01_cmd(fx: Path, base: Path) -> list[str]:
    return [cli(True), "run", *zips(fx, "scale-h01"), "--dest", str(base / "dest"), "--state-dir", str(base / "state"),
            "--unlocked", "--home-tz", "Europe/London"]


def do_h01(work: Path, fx: Path, env: dict, ffmpeg: str | None) -> dict:
    base = fresh(work / "runs" / "h01")
    bench = base / "bench.jsonl"
    e = dict(env, MR_BENCH_JSON=str(bench))
    m = run_measured(h01_cmd(fx, base) + ["--quiet"], e)
    line = last_command(bench)
    job = job_id_of(m.stdout)
    v = verify(True, job, base / "state", e)
    t0 = time.perf_counter()
    dec = decode_outputs(base / "dest" / "Memories Rescue", ffmpeg)
    dec["seconds"] = time.perf_counter() - t0
    out = {
        "job": job,
        "compositor": "media (ffmpeg)" if e.get("MR_FFMPEG") else "pass-through",
        "wall_s": m.wall,
        "outcome": line.get("outcome"),
        "engine_peak_bytes": line.get("peak_rss_bytes"),
        "tree_peak_bytes": m.tree_peak,
        "verify": v,
        "decode": dec,
        "output_bytes": sum(f.stat().st_size for f in (base / "dest").rglob("*") if f.is_file()),
    }
    return out


def do_h02(work: Path, fx: Path, env: dict, ffmpeg: str | None, h01_wall: float, kills: int = 10) -> dict:
    base = fresh(work / "runs" / "h02")
    bench = base / "bench.jsonl"
    e = dict(env, MR_BENCH_JSON=str(bench))
    rng = random.Random(int(time.time()))
    job = None
    rounds = []
    tree_peak = 0
    t_all = time.perf_counter()
    for k in range(kills + 1):
        before = len(bench_lines(bench))
        cmd = h01_cmd(fx, base) + ["--quiet"] if job is None else [cli(True), "resume", job, "--state-dir", str(base / "state"), "--quiet"]
        kill_at = rng.uniform(0.05, 0.11) * h01_wall + (3.0 if k else 0.0) if k < kills else None
        m = run_measured(cmd, e, kill_after=kill_at)
        job = job or job_id_of(m.stdout)
        if job is None:
            # Killed before the job id was printed: look it up.
            r = subprocess.run([cli(True), "jobs", "--state-dir", str(base / "state"), "--json"], capture_output=True, text=True)
            try:
                job = json.loads(r.stdout)[0]["id"]
            except Exception:  # noqa: BLE001
                job = None
        new = bench_lines(bench)[before:]
        first = next((l["first_item_s"] for l in new if l.get("event") == "first_item"), None)
        rounds.append({"round": k, "killed": m.killed, "wall_s": m.wall, "first_item_s": first, "kill_at_s": kill_at})
        tree_peak = max(tree_peak, m.tree_peak)
        if not m.killed and m.returncode == 0:
            if k < kills:
                # Finished before the kill point; the remaining kills have nothing to interrupt.
                pass
            break
    # Make sure it is finished.
    subprocess.run([cli(True), "resume", job, "--state-dir", str(base / "state"), "--quiet"], env=e, capture_output=True)
    v = verify(True, job, base / "state", e)
    dec = decode_outputs(base / "dest" / "Memories Rescue", ffmpeg)
    resumes = [r["first_item_s"] for r in rounds[1:] if r["first_item_s"] is not None]
    return {
        "job": job,
        "kills": sum(1 for r in rounds if r["killed"]),
        "rounds": rounds,
        "total_wall_s": time.perf_counter() - t_all,
        "tree_peak_bytes": tree_peak,
        "resume_first_item_s": {"min": min(resumes), "median": sorted(resumes)[len(resumes) // 2], "max": max(resumes)} if resumes else None,
        "verify": v,
        "decode": dec,
    }


def do_h03(work: Path, fx: Path, env: dict) -> dict:
    base = fresh(work / "runs" / "h03")
    inputs = zips(fx, "scale-h01")
    plan = subprocess.run([cli(False), "plan", *inputs, "--dest", str(base / "dest-plan"), "--unlocked", "--home-tz", "Europe/London", "--json"],
                          capture_output=True, text=True, env=env)
    p = json.loads(plan.stdout)
    required = p["space"]["required_bytes"]
    e = dict(env)
    e.pop("MR_FFMPEG", None)
    # 1. Precheck at 60% of the needed space.
    fake = int(required * 0.6)
    pre = subprocess.run([cli(False), "run", *inputs, "--dest", str(base / "dest1"), "--state-dir", str(base / "state1"),
                          "--unlocked", "--home-tz", "Europe/London", "--test-fake-free-bytes", str(fake)],
                         capture_output=True, text=True, env=e)
    written_1 = sum(1 for f in (base / "dest1").rglob("*") if f.is_file() and ".memories-rescue" not in str(f))
    # 2. Forced run on a drive that fills as the job writes: 60% of the need
    #    above the 2 GB floor, so it stops at the floor part-way.
    floor = 2 * 1024 ** 3
    fake2 = floor + int(required * 0.6)
    t0 = time.perf_counter()
    forced = subprocess.run([cli(False), "run", *inputs, "--dest", str(base / "dest2"), "--state-dir", str(base / "state2"),
                             "--unlocked", "--home-tz", "Europe/London", "--ignore-space-check",
                             "--test-fake-free-bytes", str(fake2), "--json", "--quiet"],
                            capture_output=True, text=True, env=e)
    seconds = time.perf_counter() - t0
    jid = job_id_of(forced.stdout)
    try:
        body = forced.stdout[forced.stdout.index("{"):]
        outcome = json.loads(body)
    except ValueError:
        outcome = {"raw": forced.stdout[-500:]}
    v = verify(False, jid, base / "state2", e) if jid else {}
    return {
        "required_bytes": required,
        "precheck": {"fake_free_bytes": fake, "exit": pre.returncode, "stdout_tail": pre.stdout.strip().splitlines()[-3:], "files_written": written_1},
        "forced": {"fake_free_bytes": fake2, "exit": forced.returncode, "outcome": outcome.get("outcome"), "counts": outcome.get("counts"),
                   "detail": outcome.get("detail"), "seconds": seconds},
        "verify": v,
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--only", default="fr06,h05,perf03,h01,h02,h03")
    ap.add_argument("--ffmpeg", help="ffmpeg for the compositor (MR_FFMPEG); without it H-01 runs with the pass-through")
    ap.add_argument("--decoder", default=shutil.which("ffmpeg"), help="ffmpeg used to decode the outputs")
    args = ap.parse_args()
    only = set(args.only.split(","))
    fx = args.work / "fx"
    ensure_fixtures(fx, [n for k, n in [("fr06", "scale-index"), ("h05", "scale-h05"), ("perf03", "perf-photos"), ("perf03", "perf-videos"),
                                        ("h01", "scale-h01"), ("h02", "scale-h01"), ("h03", "scale-h01")] if k in only])
    env = dict(os.environ)
    if args.ffmpeg:
        env["MR_FFMPEG"] = args.ffmpeg
    results_path = args.work / "results.json"
    results = json.loads(results_path.read_text()) if results_path.is_file() else {}
    machine = {
        "cpu": os.environ.get("PROCESSOR_IDENTIFIER", ""),
        "logical_cpus": psutil.cpu_count(),
        "physical_cpus": psutil.cpu_count(logical=False),
        "ram_bytes": psutil.virtual_memory().total,
        "os": sys.platform,
    }
    results["machine"] = machine
    if "fr06" in only:
        results["fr06"] = do_index(args.work, fx, "scale-index", env)
        print("fr06", json.dumps(results["fr06"]))
    if "h05" in only:
        results["h05"] = do_index(args.work, fx, "scale-h05", env)
        print("h05", json.dumps(results["h05"]))
    if "perf03" in only:
        results["perf03"] = do_perf03(args.work, fx, env)
        print("perf03", json.dumps(results["perf03"]))
    if "h01" in only:
        results["h01"] = do_h01(args.work, fx, env, args.decoder)
        print("h01", json.dumps(results["h01"])[:2000])
    results_path.write_text(json.dumps(results, indent=2))
    if "h02" in only:
        wall = (results.get("h01") or {}).get("wall_s", 300.0)
        results["h02"] = do_h02(args.work, fx, env, args.decoder, wall)
        print("h02", json.dumps(results["h02"])[:2000])
    results_path.write_text(json.dumps(results, indent=2))
    if "h03" in only:
        results["h03"] = do_h03(args.work, fx, env)
        print("h03", json.dumps(results["h03"])[:2000])
    results_path.write_text(json.dumps(results, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
