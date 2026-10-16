# Performance and soak results

Engine v1, measured on 2026-10-16 on one Windows machine (described under
"How it was measured"). Raw numbers: `results-2026-10-16.json`. Rerun with
`tools/perf_bench.py`.

## Against the SRS

| Requirement | Target | Measured here | Verdict |
|---|---|---|---|
| FR-06 AC1 | 50k entries, 5 parts: index in ≤ 60 s, peak ≤ 1 GB (reference hardware) | 0.36 s, 102 MB | Pass |
| H-05 | 200k JSON entries: parse ≤ 30 s, RSS ≤ 500 MB | 0.24 s, 55 MB | Pass |
| NFR-PERF-01 | 100 GB / 50,000 items over ≥ 10 parts, to completion | 50,000 items over 12 parts complete; the data is 0.55 GB, not 100 GB | Items and parts: pass. **100 GB: not tested** |
| NFR-PERF-02 | ≤ 1.5 GB app + sidecars; ≤ 300 MB beyond page cache and index at 50k | 356 MB engine + ffmpeg; engine alone 207 MB | Pass |
| H-01 | completes; report rows = 50k; zero corrupt outputs by decode | completed; 50,000 rows; 0 of 79,882 files bad | Pass (scaled down, see above) |
| H-02 | 10 kills, a sleep/wake, a USB unplug; no duplicates; rework only in-flight items | 10 kills: no duplicates, 40 items redone (10 × 4 workers), 0 finished items redone | Kills: pass. **Sleep/wake and unplug: not run** |
| H-03 | 60% of the needed space: precheck blocks; a forced run pauses at the threshold, no corrupt files | blocked with 0 files written; forced run paused at the 2 GB floor after 61% of items, 0 bad files | Pass |
| NFR-PERF-03 | ≥ 20 photos/s and ≥ 5 videos/s metadata-only, on reference hardware | 32.9 photos/s and 7.8 videos/s with 4 workers; 17.9 and 2.6 with 1 worker | Pass here with the default workers. **Reference hardware: not measured, at risk** |
| NFR-PERF-06 | resume after a crash starts processing within 10 s at 50k | 0.71 to 1.09 s (median 0.86 s) over 10 resumes | Pass |
| FR-31 at scale | (no SRS number) | 79,882 files, 0.91 GB: one verified archive in 44 s; four 256 MB volumes in 34 s | Works; see below |

## What misses, plainly

1. **100 GB was not tested.** The H-01 export here is 0.55 GB in and
   0.93 GB out (small media; the run had to stay under 40 GB). Item count,
   part count, memory and completion are covered; very large files are not.
   Their paths (ZIP64 entries, the `co64` upgrade, `moov` patching of
   multi-GB clips) are covered by unit tests only.
2. **NFR-PERF-03 on the reference laptop is unknown and at risk.** Here it
   passes with the default 4 workers, with a 1.6× margin for both photos and
   videos. It misses with 1 worker. An item's cost is disk latency, not CPU:
   each output is synced to disk before its rename, about 25 ms per 1 MB file
   on this NVMe drive with Defender on. The SRS figure of 287 to 373 photos/s
   (Q-02) measured the metadata writer alone, not the item pipeline. An
   8th-gen i5 on a SATA SSD runs 3 workers by default, so it would likely miss.
3. **H-02 without sleep/wake and without a USB unplug.** Neither can be done
   from this session. Drive removal is covered by the test suite through the
   destination-offline hook (NFR-REL-06).
4. **Not run:** H-04 (slow media), anything on macOS, and NFR-PERF-04/05/07.
   NFR-PERF-04 is the media session's measurement; 05 and 07 are the app's.

## Worth knowing

- **Memory grows with item count, within budget.** Engine peak was 49 MB at
  1,000 items and 207 MB at 50,000, about 3.2 KB per item. The pending items'
  plans are loaded when a run starts, and SQLite's page cache adds to that.
  Projected for 200,000 items: about 700 MB, still under 1.5 GB.
- **The machine stalled, not the engine.** During H-01, all four workers
  stood still at once 44 times for over a second, up to 16 s, about 97 s of
  the first 730 s. The stalled items were trivial (a 3 KB photo), which
  points to Defender scanning or a disk flush. These stalls set the maximum
  per-item times in every run.
- **A fresh 50k job takes 8 to 9 s to its first finished item.** That is the
  dry run plus writing 50,000 rows into the new job. No SRS target covers it;
  the user sees it after pressing Start.

## Details

### FR-06 AC1 and H-05: index and JSON (`mr-cli bench index`, release)

| Export | Discover | Index | JSON | Match | Total | Engine peak |
|---|---|---|---|---|---|---|
| 50,000 entries, 65,143 files (15,143 overlays), 5 parts, 244 MB | 0.11 s | 0.09 s | 0.07 s | 0.08 s | 0.36 s | 102 MB |
| 200,000 entries, 33.6 MB of JSON, no media | 0.00 s | 0.00 s | 0.24 s | 0.02 s | 0.26 s | 55 MB |

The JSON is streamed (`SavedMediaReader`); the 200k-entry file never sits
in memory as a whole.

### NFR-PERF-03: metadata-only throughput (release, FR-21 mode, no compositor)

| Media | Workers | Items | Wall | Items/s | Mean per item |
|---|---|---|---|---|---|
| Photos, 1080x1920 JPEG, about 0.9 MB | 4 (default) | 1,000 | 30.4 s | **32.9** | 120 ms |
| | 1 | 1,000 | 55.8 s | 17.9 | 54 ms |
| Videos, 10 s 1080x1920 H.264, about 4.5 MB, half `moov` first | 4 (default) | 200 | 25.5 s | **7.8** | 251 ms |
| | 1 | 200 | 76.9 s | 2.6 | 373 ms |

Engine peak about 49 MB in every run. Where a 1 MB photo's 54 ms goes on one
worker: the entry is extracted to a temp file, the metadata writer reads it
and writes the tagged copy, the copy is read back and checked (NFR-REL-05),
its times are set and it is synced to disk, then hashed and renamed into
place, and the job database records it. A Python probe on the same drive
puts write + hash + rename at 8.8 ms per 1 MB file, and the sync at about
25 ms more. With 4 workers the mean per item rises to 120 ms: the syncs do
not overlap well, and the drive, not the CPU, sets the pace.

### H-01: 50,000 items, 12 parts, real compositor (release)

| Measure | Value |
|---|---|
| Export | 40,000 photos, 10,000 videos, 14,941 overlays (30%); 0.55 GB; `2026b`, every item matched to the second |
| Compositor | the pinned LGPL ffmpeg (software `h264_mf`); 14,941 burn-ins, about 3,000 of them videos |
| Wall time | 1,073 s (17.9 min), 47 items/s; first item after 7.9 s |
| Mean per item (sample at 33k items) | photo 35 ms; photo with burn-in 96 ms; video 49 ms; video with burn-in 380 ms |
| Memory | engine 207 MB; engine + ffmpeg 356 MB |
| Output | 79,882 files, 0.93 GB: exactly the recorded set; 0 hash mismatches; 0 metadata read-back failures |
| Report | 50,000 rows |
| Decode | 66,973 photos and overlays (Pillow), 12,909 videos (ffmpeg): 0 bad |

### H-02: the same job, 10 random kills (release)

| Measure | Value |
|---|---|
| Kills | 10, each 37 to 74 s into a run, then resumed; an 11th run finished |
| Total wall time | 772 s |
| Item starts | 50,040 for 50,000 items: 40 in-flight items redone (10 kills × 4 workers); 0 finished items started again |
| Output | 79,882 files, exactly the recorded set; 0 hash or metadata failures; temp folder empty |
| Decode | 66,973 photos and overlays, 12,909 videos: 0 bad |
| Memory | engine + ffmpeg 302 MB |
| NFR-PERF-06 | time from `mr-cli resume` to the first finished item: 0.71 / 0.86 / 1.09 s (min / median / max) |

A first attempt with a wider kill spacing got 6 kills in before the job
finished. It passed the same way: 24 items redone (6 × 4), resumes 0.69 to
1.52 s.

### H-03: low disk (debug build, the free-space hook)

The dry run needs 962 MB for this job. A fake drive shrinks by what the
estimate counts for each finished item.

| Step | Result |
|---|---|
| Precheck with 60% of the need free (577 MB) | blocked before any item (`InsufficientSpace`); 0 files written; options offered: change destination, turn off "keep originals" (saves 244 MB) |
| Forced run (`--ignore-space-check`) with 2 GiB + 60% of the need free | paused with `DISK_FULL_IMMINENT` ("Only 2.1 GB left") after 30,503 of 50,000 items (61%) |
| Output at the pause | 39,553 files, exactly the recorded set; 0 hash or metadata failures; temp folder empty; 33,544 photos and overlays and 6,009 videos decode |

### FR-31 at scale: packing the H-02 output (release)

| Volume size | Archives | Wall (resume, pack, verify) | Engine peak | Independent check |
|---|---|---|---|---|
| FAT32 preset | `Memories Rescue.zip`, 913 MB, 79,882 entries | 43.6 s | 181 MB | Python `testzip`: 0 bad |
| 256M | four archives, 265 / 265 / 265 / 119 MB, each under 256 MiB; the earlier archive removed | 33.9 s | 183 MB | 0 bad |

## How it was measured

- **Machine.** Intel Core Ultra 7 265K (20 cores), 64 GB RAM, Windows 11,
  NVMe SSD (ADATA LEGEND 900 PRO) for inputs, outputs and job state, with
  Windows Defender real-time protection on. This is much faster than the
  SRS reference hardware (Intel Core i5 8th gen, 8 GB, SATA SSD; Apple M1
  8 GB). No Mac was available.
- **Build.** `cargo build --release -p mr-cli` (LTO, one codegen unit,
  `panic = "abort"`). H-03 uses the debug build, because the free-space hook
  works only in debug builds.
- **Data.** Synthetic exports from `tools/make_fixture` (the scale builders
  in `fixtures_scale.py`, described in `tools/README.md`).
- **Memory.** *Engine*: the process's own peak working set, from
  `mr_job::platform::peak_rss_bytes()` through `MR_BENCH_JSON`. *Engine +
  ffmpeg*: the largest sum of resident memory over the `mr-cli` process and
  its children, polled every 100 ms with psutil. Polling can miss a short
  spike; the engine figure cannot.
- **Correctness.** `mr-cli bench verify` checks four things. Every finished
  item's file hashes to its record and reads its capture metadata back. The
  output folder holds exactly the recorded files. The journal shows no
  finished item started again. The temp folder is empty. Then every output
  is decoded: Pillow `load()` for photos and overlays, `ffmpeg -v error -f
  null` for videos.
- **Throughput while busy.** H-01 shared the machine with a few cargo builds
  and tests, so its wall time is an upper bound. The other runs had the
  machine to themselves.

Reproduce:

```bash
cargo build --release -p mr-cli
cargo build -p mr-cli
python tools/perf_bench.py --work <scratch> --ffmpeg <sidecar ffmpeg>
```

The scratch folder needs about 8 GB; delete it afterwards.
