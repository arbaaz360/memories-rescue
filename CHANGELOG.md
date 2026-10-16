# Changelog

All notable changes to Memories Rescue. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the version is the
one in `package.json`, `Cargo.toml` and `src-tauri/tauri.conf.json`, and a
release tag is `v<version>`. `tools/release/check.py` requires an entry for
the version being released.

## [Unreleased]

### Security

- A damaged or crafted MP4/MOV in an export no longer crashes or hangs the
  run: the media probe checks every box size and offset, bounds its walks and
  its memory, and an absurd overlay aspect is refused before resizing. Such
  a file now gets a skipped probe, or is copied as-is and flagged.

## [0.1.0] - 2026-10-16

First internal build (milestones M0 to M3 of the SRS).

### Added

- Reads Snapchat "Download My Data" exports: ZIP parts, extracted folders,
  folders of ZIPs; multi-part merge, truncated-part recovery, zip-slip
  refusal.
- Matches `memories_history.json` entries to media files (exact `sid`/`mid`,
  positional within a day, honest day-precision fallback) with the 100 m
  location rule.
- Writes capture dates and GPS into JPEG, PNG and MP4/MOV natively; local
  time offsets from bundled time zone boundaries; file times; `YYYY/MM`
  library with atomic placement; resumable jobs; dry run; report.
- Photo and video caption overlays, HEVC to H.264 compatibility mode and clip
  rejoin through a separate LGPL ffmpeg.
- All screens of the SRS flow, free tier of 100 items, Gumroad activation.
