# Q-02: metadata writer, exiftool sidecar or native Rust

| | |
|---|---|
| Status | **Recommended 2026-10-16 (M1 spike); owner Adam decides at the M1 review** |
| Decides | SRS Q-02 and the "or" in C-05; how §4.3 is implemented in M2 |
| Inputs | `crates/mr-meta` (both writers, the `-stay_open` driver, `q02-bench`), `tools/bench_corpus.py`, SRS §4.3, C-05, NFR-PERF-03 |
| Measured on | Windows 11 Pro N, Intel Core Ultra 7 265K (20 logical CPUs), 64 GB, NVMe SSD. Far above the NFR-PERF-03 reference machine (Core i5 8th gen, 8 GB, SSD); the "what it means" section scales the numbers down. |

## Recommendation

**Write metadata natively in Rust: `little_exif` for JPEG (and PNG), the
`mr-meta` `moov` patcher for MP4/MOV. Keep exiftool as the read-back oracle
in the test suite and as a working `-stay_open` sidecar driver in the code,
but do not bundle it in v1.0 unless the M2 check on a real export finds a file
the native writer cannot handle.** The ffmpeg remux route is not a metadata
writer and is dropped for that purpose.

Why, in one paragraph: both candidates write the full §4.3 field set and read
back identically through exiftool and ffprobe, including
`com.apple.quicktime.location.ISO6709` = `+51.5074-000.1278/`. exiftool meets
NFR-PERF-03 only with four worker processes and only with a margin that
shrinks as videos get larger, because its cost grows at 6–25 ms per megabyte
on top of a ~25 ms floor per file; the native writers are bound by disk
bandwidth (3–5 ms per phone-sized file on one thread, 10–30× exiftool) and
have no floor. Bundling exiftool means shipping a 35 MB Perl runtime on
Windows, depending on the system Perl on macOS, carrying a second license
regime, and keeping a process protocol alive for a fallback nobody has yet
needed. The native MP4 code is 350 lines, tested against exiftool's own
output byte layout, and the one real risk it carries, that Apple Photos might
read exiftool's output and not ours, is already an M2 exit criterion ("video
map location verified in Apple Photos") and is mitigated by writing the
*same* atom layout exiftool writes.

## Candidates

### (a) exiftool as a `-stay_open` sidecar

One `exiftool -stay_open True -@ -` process per worker, one command block per
file on stdin, `-execute<n>` ends a block, `{ready<n>}` on stdout says it is
done, `-echo4 {done<n>}` stamps stderr so warnings can be attributed to a
file. Implemented in `crates/mr-meta/src/exiftool.rs`. Writes source to
destination in one pass with `-o`.

### (b) native Rust crates

| Crate | Version | Reads | Writes | Covers the §4.3 set? | Verdict |
|---|---|---|---|---|---|
| `little_exif` | 0.6.23 (2026-01) | EXIF in JPEG, PNG, WebP, TIFF, HEIF, JXL | yes, same formats | JPEG: `DateTimeOriginal`, `CreateDate`, `ModifyDate`, `OffsetTime*`, GPS lat/lon/refs, `GPSDateStamp`, `GPSTimeStamp`, `GPSVersionID`: all present and verified. PNG via eXIf chunk (not benchmarked). | **Used.** The only maintained pure-Rust crate that both reads and writes EXIF. |
| `kamadak-exif` | 0.6.1 | yes | no | | read-only |
| `nom-exif` | 3.8.0 | yes, incl. MP4/MOV | no | | read-only |
| `img-parts` | 0.4.0 | JPEG/PNG/RIFF segments | segment-level only | no TIFF/EXIF encoder | would need a hand-written EXIF encoder |
| `mp4` | 0.14.0 (2023) | yes | re-muxes samples through `Mp4Writer` | no `mdta` Keys, no `©xyz`, rewrites the whole file through a parser | stale, wrong shape |
| `mp4ameta` | 0.13.0 | iTunes `ilst` | iTunes `ilst` (audio metadata) | no `mdta` Keys, no `mvhd` times | wrong atoms |
| `mp4-atom` | 0.16.2 (2026-10) | generic atom codec | generic atom codec | no Keys/`©xyz` helpers; would be a base for a patcher | viable base, not used (hand-rolled walker was smaller) |
| `mp4parse` | 0.17.0 | yes (Mozilla) | no | | read-only |

Nothing on crates.io writes QuickTime `mvhd`/`tkhd`/`mdhd` times plus `mdta`
Keys plus `©xyz` without re-muxing, so the spike wrote the patcher:
`crates/mr-meta/src/native_mp4.rs` (about 350 lines with its own reader for
tests). It copies everything outside `moov` byte for byte, patches the three
time pairs in place, replaces `moov/meta` with an `mdta` Keys box holding the
two Apple keys, merges `©xyz` into `udta`, and, when `moov` precedes `mdat`
(the usual "faststart" layout), shifts every `stco`/`co64` chunk offset by the
growth of `moov`. That offset rewrite is the whole difficulty of the native
route and the reason tools like exiftool exist; it is 40 lines here.

### (c) hybrids

Two were considered. **ffmpeg remux** (`-c copy -metadata … -movflags
use_metadata_tags`) was measured because it is "free" once ffmpeg ships: it
writes the times and the two Keys, but it emits a 3GPP `loci` atom instead of
`©xyz`, copies every format tag into Keys, duplicates tags on read-back
(`creation_time` came back as `…Z;…Z`), and moves `moov` to the end unless a
second `+faststart` pass is paid. Not a metadata writer. **Native with
exiftool fallback** is the recommendation's shape minus the bundling: the code
for both exists; whether the fallback ships is the M2 decision above.

## Method

- Corpus (`python tools/bench_corpus.py`, gitignored): `fixture/` = 500 JPEG
  (~23 KB) and 100 MP4 (~23 KB, 2 s test pattern) copied from the F-01
  fixture, as the brief asked; `real/` = 500 JPEG at 1080×1920 (~0.9 MB) and
  100 ten-second 1080×1920 H.264 clips (~4.6 MB), alternating `moov`-first
  and `moov`-last. Byte counts, not pixels, drive a metadata rewrite, so the
  `real` corpus is the one that matters.
- Each writer copies source to destination with the full §4.3 field set;
  one and four workers (threads; one exiftool process per thread). Two full
  passes; both are reported. One process per file was limited to 20 items.
- Read-back with `exiftool -a -G1 -s -j -n` and `ffprobe -show_format
  -show_streams -of json` on the first (and for MP4 the second, `moov`-last)
  file of every run, field by field against the expected strings.
- Binaries: exiftool 13.59 (Windows 64-bit standalone), ffmpeg/ffprobe
  n9.0.2-28-g669f5ab083 BtbN win64 LGPL; see `third_party/README.md`.

## Numbers

### Process start

| Measure | min | median | max |
|---|---|---|---|
| `exiftool -ver`, fresh process | 103 ms | 112 ms | 134 ms |
| `-ver` through the `-stay_open` pipe | 0 ms | 0 ms | 111 ms |
| `ffmpeg -version`, fresh process | 22 ms | 25 ms | 30 ms |
| exiftool, one process per file, first write | | ~285 ms | |

`exiftool -ver` returns before the ExifTool modules are compiled; a real
write in a fresh process costs ~285 ms, of which ~110 ms is process start and
the rest Perl compiling `Image::ExifTool`, `Writer.pl`, `Exif.pm`, `GPS.pm`,
`QuickTime.pm`. The 64-bit package no longer unpacks a runtime at start (see
"Corrections to the SRS").

### Throughput (items per second; two passes)

| Corpus | Media | Approach | Workers | Items | items/s (pass 1 / pass 2) | ms per item (best) | Size overhead |
|---|---|---|---|---|---|---|---|
| fixture | JPEG | (a) exiftool `-stay_open` | 1 | 500 | 37 / 47 | 21.2 | +484 B |
| fixture | JPEG | (a) exiftool `-stay_open` | 4 | 500 | 277 / 281 | 3.6 | +484 B |
| fixture | JPEG | exiftool, one process per file | 1 | 20 | 3.4 / 3.5 | 284.8 | +484 B |
| fixture | JPEG | (b) native Rust | 1 | 500 | 863 / 711 | 1.2 | +390 B |
| fixture | JPEG | (b) native Rust | 4 | 500 | 2340 / 1048 | 0.4 | +390 B |
| fixture | MP4 | (a) exiftool `-stay_open` | 1 | 100 | 35 / 37 | 27.0 | +279 B |
| fixture | MP4 | (a) exiftool `-stay_open` | 4 | 100 | 151 / 167 | 6.0 | +279 B |
| fixture | MP4 | exiftool, one process per file | 1 | 20 | 3.9 / 3.8 | 254.5 | +277 B |
| fixture | MP4 | (b) native Rust | 1 | 100 | 945 / 857 | 1.1 | +268 B |
| fixture | MP4 | (b) native Rust | 4 | 100 | 2063 / 2007 | 0.5 | +268 B |
| fixture | MP4 | (c) ffmpeg remux | 1 | 100 | 16 / 18 | 55.9 | +251 B |
| fixture | MP4 | (c) ffmpeg remux | 4 | 100 | 59 / 59 | 16.9 | +251 B |
| real | JPEG | (a) exiftool `-stay_open` | 1 | 500 | 29 / 40 | 25.0 | +484 B |
| real | JPEG | (a) exiftool `-stay_open` | 4 | 500 | 208 / 187 | 4.8 | +484 B |
| real | JPEG | exiftool, one process per file | 1 | 20 | 3.3 / 3.5 | 285.6 | +484 B |
| real | JPEG | (b) native Rust | 1 | 500 | 287 / 373 | 2.7 | +390 B |
| real | JPEG | (b) native Rust | 4 | 500 | 662 / 91 | 1.5 | +390 B |
| real | MP4 | (a) exiftool `-stay_open` | 1 | 100 | 7.1 / 19 | 52.2 | +279 B |
| real | MP4 | (a) exiftool `-stay_open` | 4 | 100 | 46 / 46 | 21.6 | +279 B |
| real | MP4 | exiftool, one process per file | 1 | 20 | 2.5 / 3.6 | 277.2 | +277 B |
| real | MP4 | (b) native Rust | 1 | 100 | 204 / 132 | 4.9 | +268 B |
| real | MP4 | (b) native Rust | 4 | 100 | 76 / 5.5 | 13.2 | +268 B |
| real | MP4 | (c) ffmpeg remux | 1 | 100 | 4.8 / 4.8 | 207.3 | +319 B |
| real | MP4 | (c) ffmpeg remux | 4 | 100 | 2.6 / 25 | 39.5 | +319 B |

Reading the table:

- **exiftool's cost is a floor plus a per-megabyte term.** Fixture files
  (23 KB) cost 21–27 ms each; a 0.9 MB JPEG adds ~4–8 ms, a 4.6 MB MP4 adds
  25–115 ms (the two passes disagree on the MP4 slope; both are far above the
  native writer). Four processes scale almost linearly because the work is
  CPU in Perl, not I/O.
- **The native writers are disk-bound.** 2.7 ms per 0.9 MB JPEG and 4.9 ms
  per 4.6 MB MP4 on one thread is ~1 GB/s of copy. The four-thread cells on
  the `real` corpus are erratic (662 vs 91, 76 vs 5.5): four threads writing
  450–460 MB in a second or two hit the write-back cache on this machine, and
  the measurement becomes a measurement of the SSD and Defender, not of the
  writer. One thread already exceeds NFR-PERF-03 by an order of magnitude, so
  metadata-only mode does not need parallel writers; parallelism belongs to
  compositing.
- **Size overhead** is a few hundred bytes per file for every approach
  (exiftool pads the EXIF block a little more than `little_exif`).
- **One process per file** is 3–4 items/s whatever the file: C-05's
  conclusion stands.

### Read-back verification

All 10 JPEG fields and all 13 MP4 fields (7 through exiftool, 6 through
ffprobe) matched for exiftool and for the native writers on both corpora and
both `moov` layouts, with two exceptions that are findings, not failures:

1. exiftool writes the ISO 6709 string **with whatever precision it is
   given.** The benchmark passed `-0.12789999999999999` (a float printed in
   full) and got `+51.5075-000.12789999999999999/` back. The caller must round
   to four decimals; `mr_meta::exiftool::mp4_args` now does, and the native
   writer always did (`Capture::iso6709`). The verified string for the base
   case is exactly `+51.5074-000.1278/` from both writers.
2. ffmpeg remux: no `©xyz` (it writes `loci`), and `creation_time` read back
   doubled, as described under (c).

## What it means for NFR-PERF-03 (≥ 20 photos/s, ≥ 5 videos/s on a Core i5 8th gen, 8 GB, SSD)

Scaling: a Core i5-8250U/8265U is roughly 2.7× slower per thread than this
machine and has four cores, so four-worker numbers divide by about 2.7 and
single-thread numbers by about 2.7 as well; its SATA SSD moves ~500 MB/s
against this NVMe's ~3 GB/s, so disk-bound numbers divide by up to 6.

| Approach | Reference-machine estimate | Meets NFR-PERF-03? |
|---|---|---|
| exiftool `-stay_open`, 4 workers, 0.9 MB photos | 187–208 ÷ 2.7 ≈ **70–75 photos/s** | yes |
| exiftool `-stay_open`, 4 workers, 4.6 MB videos | 46 ÷ 2.7 ≈ **17 videos/s** | yes |
| exiftool `-stay_open`, 4 workers, 15 MB videos (clips at higher bitrates) | (27 ms + 15 × 6–25 ms) per item per worker → 2.4–7 items/s per worker here → ≈ **3.5–10 videos/s** on the reference machine | borderline |
| exiftool, one process per file | 3.5 ÷ 2.7 ≈ 1.3 items/s | no (C-05 is right) |
| native, 1 thread, photos | 287–373 ÷ 2.7 (CPU) or ÷ 6 (disk) ≈ **50–140 photos/s** | yes, 2.5–7× margin |
| native, 1 thread, 4.6 MB videos | 132–204 ÷ 6 ≈ **20–35 videos/s** | yes, 4–7× margin |
| native, 1 thread, 15 MB videos | disk-bound: 500 MB/s ÷ 30 MB read+write ≈ **15 videos/s** | yes |

Both routes can pass on paper. exiftool passes with four worker processes
and loses its margin as clips get larger; the native route passes on one
thread with margin to spare and leaves the CPU to compositing, which is the
part of the pipeline that is actually slow (NFR-PERF-04). Memory: four
exiftool processes are ~60 MB RSS each on Windows (Perl), inside NFR-PERF-02
either way.

The reference machine is not on hand. M2 should run `q02-bench` once on an
i5-class laptop before the metadata-only mode is called done; the estimates
above are the acceptance thresholds for that run.

## The exact exiftool lines (kept for the test oracle and the fallback)

Argfile, one argument per line, as `mr_meta::exiftool::{jpeg_args, mp4_args}`
emit them. Every block is prefixed with `-charset` / `filename=UTF8`
(without it exiftool reads paths in the system code page on Windows and
fails on `Café ☕ 写真.jpg`), and ended with `-echo4` / `{done<n>}` /
`-execute<n>`.

JPEG:

```text
-o
<destination>.jpg
-EXIF:DateTimeOriginal=2021:06:15 15:30:00
-EXIF:CreateDate=2021:06:15 15:30:00
-EXIF:ModifyDate=2021:06:15 15:30:00
-EXIF:OffsetTimeOriginal=+01:00
-EXIF:OffsetTimeDigitized=+01:00
-EXIF:OffsetTime=+01:00
-GPS:GPSVersionID=2.3.0.0
-GPS:GPSLatitude=51.5074
-GPS:GPSLatitudeRef=N
-GPS:GPSLongitude=0.1278
-GPS:GPSLongitudeRef=W
-GPS:GPSDateStamp=2021:06:15
-GPS:GPSTimeStamp=14:30:00
<source>.jpg
```

MP4/MOV (times are UTC and written raw, without `-api QuickTimeUTC`, which
is what the container stores; `Keys:CreationDate` is local time with offset):

```text
-o
<destination>.mp4
-QuickTime:CreateDate=2021:06:15 14:30:00
-QuickTime:ModifyDate=2021:06:15 14:30:00
-QuickTime:TrackCreateDate=2021:06:15 14:30:00
-QuickTime:TrackModifyDate=2021:06:15 14:30:00
-QuickTime:MediaCreateDate=2021:06:15 14:30:00
-QuickTime:MediaModifyDate=2021:06:15 14:30:00
-Keys:CreationDate=2021:06:15 15:30:00+01:00
-Keys:GPSCoordinates=51.5074 -0.1278
-UserData:GPSCoordinates=51.5074 -0.1278
<source>.mp4
```

Read back (`exiftool -a -G1 -s -j -n`): `QuickTime:CreateDate`
`2021:06:15 14:30:00`, `Track1:TrackCreateDate`, `Track1:MediaCreateDate`
(and Track2 for audio) the same, `Keys:CreationDate`
`2021:06:15 15:30:00+01:00`, `Keys:GPSCoordinates` `51.5074 -0.1278`,
`UserData:GPSCoordinates` `51.5074 -0.1278`. ffprobe `format.tags`:
`creation_time` `2021-06-15T14:30:00.000000Z`,
`com.apple.quicktime.creationdate` `2021-06-15T15:30:00+0100`,
`com.apple.quicktime.location.ISO6709` `+51.5074-000.1278/`, `location`
(the `©xyz` atom) `+51.5074-000.1278/`; every stream's `creation_time` the
same instant. `-QuickTime:CreateDate` writes `mvhd`, `TrackCreateDate`
`tkhd`, `MediaCreateDate` `mdhd`.

What exiftool writes on disk, which the native writer mirrors: `moov/meta`
as a QuickTime-style plain box (no version/flags) containing a 32-byte
`hdlr` (`mdta`, empty name), `keys` and `ilst` with 1-based index item
types and UTF-8 `data` boxes; `©xyz` in `moov/udta` as 16-bit length, 16-bit
language 0, string. `moov` stays where it was.

## Native writer: what is there and what M2 must add

Done and tested: JPEG field set (with or without pre-existing EXIF; Snapchat
exports have none); MP4 times in `mvhd`/`tkhd`/`mdhd` for 32- and 64-bit
versions; `mdta` Keys; `©xyz` merged into an existing `udta`; `stco`/`co64`
shift for `moov`-first files; byte-identical `mdat`; idempotent on its own
output; read-back through exiftool and ffprobe identical to exiftool's.

To add in M2 (each small, listed so nobody discovers them late):

- `stco` → `co64` upgrade when a shifted offset would pass 4 GiB (today an
  error); exports with multi-gigabyte clips would need it.
- Merge an existing `moov/meta` with `mdta` keys instead of replacing it
  (other Apple keys would be dropped today). Snapchat-produced MP4s have not
  shown any; the real export check (Q-03) should confirm.
- Reuse an adjacent `free` atom so a `moov`-first file need not shift at all
  (optimisation, not correctness).
- PNG through `little_exif` (eXIf chunk) and a check in Apple Photos that it
  reads the chunk; HEIC only if the real export contains any.
- File modification and creation times (§4.3 last row), a separate concern.
- Streaming the `moov` copy for very large `moov` atoms (today `moov` is read
  into memory; it is tens of KB to a few MB for a phone clip).

## Risks and the fallback

| Risk | Where it bites | Mitigation |
|---|---|---|
| Apple Photos (or Google Photos, Windows Photos) ignores the native output but honours exiftool's | M2 exit criterion "video map location verified in Apple Photos" | The native writer emits the same atom layout and strings as exiftool, verified byte-level in `tests/writers.rs`; if Photos still differs, the exiftool driver is in the tree and `externalBin` already carries it in this spike. |
| An input the patcher refuses (truncated `moov`, 64-bit offsets, odd brands) | Long tail of a 50 k-item export | Refusals are typed errors that surface in the report (FR-40) with the file name; such files can be retried through exiftool in a point release or by the user. |
| `little_exif` bug or abandonment | JPEG/PNG path | Crate is active (2026-01), small, pure Rust; the EXIF writer surface we use is ~12 tags, replaceable by ~300 lines if needed. |
| macOS Perl goes away | exiftool fallback only | Does not affect the native path; one more reason not to bundle. |

## Corrections to the SRS

- **C-05 / Q-02:** "the standalone Windows `exiftool.exe` unpacks its Perl
  runtime at every start, which costs far more than the write itself" was true
  of the old single-file PAR package. The current 64-bit package is a 58 KB
  launcher beside a pre-unpacked `exiftool_files/` Perl (35 MB, 626 files):
  process start is ~110 ms and the first write ~285 ms because Perl compiles
  the ExifTool modules; nothing is unpacked. The conclusion (one process per
  file cannot meet NFR-PERF-03, `-stay_open` is mandatory if exiftool is
  used) is still right; the number behind it is 3.5 items/s. The larger
  limit, missing from the SRS, is exiftool's per-megabyte cost on video.
  C-05 lists `mvhd`/`tkhd` but not `mdhd`; §4.3 has all three; align C-05.
- **§4.3:** add `OffsetTimeDigitized` (pairs with `CreateDate`) and
  `ModifyDate` (pairs with `OffsetTime`) to the JPEG row, otherwise
  `OffsetTime` dangles. State that the ISO 6709 string is formatted to four
  decimals (`%+08.4f%+09.4f/`) by the app, whichever writer is used. "Written
  as a lossless remux" should read "written by rewriting `moov` only; no
  demux/remux", and note that an ffmpeg remux is **not** a substitute (no
  `©xyz`, Keys pollution, `moov` moved to the end). Record the `moov/meta`
  layout (QuickTime-style, no version/flags, as exiftool and Apple write it).
- **§4.4 / Q-04 (C-04 context):** on this Windows 11 **Pro N** machine
  `h264_mf` and `aac_mf` work with the LGPL build (the Media Feature Pack is
  present; `mfh264enc.dll` in System32), `hevc_mf` fails (no HEVC encoder MFT
  without the HEVC Video Extensions; irrelevant while H.264 is the default
  output). The BtbN LGPL build also carries `libopenh264` built from BSD
  source, so an LGPL build *can* include OpenH264 without the Cisco binary,
  but then without Cisco's patent licence, which covers only their binary.
  Q-04 should weigh that explicitly.
- **C-04 / §10.4:** the BtbN LGPL build is LGPL **v3** (`--enable-version3`);
  §10.4 should say which LGPL version the NovaByteWorks build targets (v2.1
  needs the flag dropped and loses a few v3-only components). A full-featured
  static LGPL build is 134 MB per executable (ffmpeg and ffprobe each), so
  §10.4's minimal component list is what keeps the installer small, and
  ffprobe is not needed at all if exiftool or the native reader does read-back.
- **§10.1:** "per-user install by default" holds for the NSIS installer
  (`installMode: currentUser`). Tauri's WiX template is per-machine
  (`InstallScope="perMachine"`, `Program Files`); a per-user MSI needs a
  custom `main.wxs`. Recommend NSIS as the consumer download and MSI, per
  machine, as the IT/managed-deployment artifact, and word §10.1 that way.

## How to reproduce

```bash
python tools/make_fixture --all
python tools/bench_corpus.py
python tools/sidecars.py fetch
cargo run --release -p mr-meta --bin q02-bench -- --workers 1,4 --json q02.json
cargo test -p mr-meta        # read-back tests; skip exiftool/ffprobe parts when absent
```
