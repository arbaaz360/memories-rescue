# third_party/: sidecar binaries

Everything in this directory except this file is gitignored. `python
tools/sidecars.py fetch` downloads the archives below, checks their SHA-256
against the pins in the script, extracts them here and stages the sidecars
into `src-tauri/binaries/` under the names Tauri expects
(`<name>-<target triple>.exe`). CI runs `python tools/sidecars.py placeholders`
instead and the sidecar tests skip.

**These are development and M1-spike binaries.** The release build ships
NovaByteWorks' own LGPL ffmpeg build per SRS §10.4 (minimal components, build
script and configure flags published under `third_party/ffmpeg/`, license text
and source offer in the app). The BtbN build is a full-featured static build
(134 MB per executable, dozens of bundled libraries whose notices would all have
to ship); it is pinned here only so the spike, the benchmarks and the installer
smoke test run on a real LGPL binary today.

## exiftool 13.59 (Windows 64-bit standalone)

| | |
|---|---|
| Source | <https://exiftool.org/> (Windows zip linked to the SourceForge mirror; the direct exiftool.org URL returns 404) |
| Archive | `exiftool-13.59_64.zip`, 11,183,675 bytes, from `https://sourceforge.net/projects/exiftool/files/exiftool-13.59_64.zip/download` |
| SHA-256 (archive) | `44b512b25af500724ba579d0a53c8fc5851628b692dd5e5d94ae4a15c2cba9ec` (matches <https://exiftool.org/checksums.txt>) |
| SHA-256 (staged `exiftool.exe`) | `68c079c32fdae0d6c7130e9a5fb73f8ac9dabdf9ab8da312da4f6c549d6d3385` |
| License | Same terms as Perl: Artistic License or GPL, at the user's choice (`exiftool_files/LICENSE`). Bundled Strawberry Perl runtime notices in `exiftool_files/Licenses_Strawberry_Perl.zip`. |
| Layout | `exiftool.exe` (58 KB launcher, shipped as `exiftool(-k).exe` and renamed) plus `exiftool_files/` (perl.exe, perl532.dll, the ExifTool library; 35 MB, 626 files). The launcher must sit next to `exiftool_files/`. |

The 64-bit package no longer self-extracts a Perl runtime at every start: the
runtime is unpacked once, in `exiftool_files/`. Process start is ~120 ms; the
first write in a fresh process costs ~350 ms because the ExifTool modules are
compiled on load. See `docs/decisions/Q-02-metadata-writer.md`.

In the installed app, Tauri places the sidecar as `exiftool.exe` in the
install directory and `bundle.resources` places `exiftool_files/` beside it
(`src-tauri/tauri.conf.json`).

## ffmpeg n9.0.2-28-g669f5ab083 (BtbN win64 LGPL static)

| | |
|---|---|
| Source | <https://github.com/BtbN/FFmpeg-Builds>, release `autobuild-2026-10-15-13-03` (2026-10-15) |
| Archive | `ffmpeg-n9.0.2-28-g669f5ab083-win64-lgpl-9.0.zip`, 171,480,504 bytes |
| SHA-256 (archive) | `d05ca4eeab509d631928ad26363597d3fb0dfc3ef4163980cfbfef784ca64e41` (matches the release's `checksums.sha256`) |
| SHA-256 (staged) | `ffmpeg.exe` `d388e9a142bc2306dd0922570b9de9b59ae204748b60b6afdfff821ba6413ee9`; `ffprobe.exe` `15bb93293f959105bf51911c81ce98bf864b6421419212e3db8e1842a83fd3d6` |
| Version string | `ffmpeg version n9.0.2-28-g669f5ab083-20261015`, FFmpeg release/9.0 branch at commit `669f5ab083` |
| License | LGPL v3 (`--enable-version3`; configured without `--enable-gpl` and without `--enable-nonfree`; `LICENSE.txt` from the archive). libx264/libx265 are disabled. |
| Encoders of interest | `h264_mf`, `hevc_mf`, `aac_mf` (Media Foundation), `libopenh264`, `h264_amf`, `h264_nvenc`, `h264_qsv`, native `aac` |
| Staged | `ffmpeg.exe`, `ffprobe.exe`, `LICENSE.txt` (`ffplay.exe` is not copied) |

Note on `--enable-version3`: it makes the build LGPL **v3** rather than v2.1,
which is still a free-software LGPL build and still forbids nothing the SRS
needs; the release build can choose v2.1 by dropping the flag.

## Refreshing

Change the pins in `tools/sidecars.py` (URL, size, SHA-256 from the publisher's
checksum file), run `python tools/sidecars.py fetch`, update the tables above,
and re-run `cargo test -p mr-meta` and `cargo test -p memories-rescue`, which
exercise the staged binaries when present.
