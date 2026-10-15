# Memories Rescue (working name)

Memories Rescue is a local Windows and macOS desktop app (Tauri 2, Rust core) that repairs your own Snapchat "Download My Data" Memories export: it restores capture dates and GPS into the photos and videos, pairs caption overlays with their media, and writes a clean, date-sorted `YYYY/MM` library that photo apps import on the right day and place. Everything runs on your computer; no Snapchat login, no uploads. The requirements live in the public spec at <https://github.com/arbaaz360/memories-rescue-srs>; this repository is the application code and starts at milestone M0 (repository, CI, fixture generator v0).

Not affiliated with, endorsed by, or sponsored by Snap Inc. "Snapchat", "Memories" and "My Eyes Only" are trademarks of Snap Inc. and are used only to describe compatibility.

## Layout

| Path | What it is |
|---|---|
| `crates/mr-core` | Rust library: filename patterns, `memories_history.json` field parsers, streaming `Saved Media` reader (SRS §3). |
| `crates/mr-meta` | Rust library: metadata writers (SRS §4.3): exiftool `-stay_open` sidecar driver, native JPEG/MP4 writers, and the Q-02 benchmark (`q02-bench`). |
| `src-tauri` | Tauri 2 shell: placeholder window, ffmpeg and exiftool sidecars (`bundle.externalBin`), NSIS and MSI bundles. |
| `third_party/` | Sidecar binaries, gitignored; `third_party/README.md` records source, version, SHA-256 and license. `python tools/sidecars.py fetch` stages them. |
| `docs/decisions/` | Decision records: `Q-02-metadata-writer.md`, `signing.md`. |
| `src` | Frontend placeholder: plain TypeScript + Vite, no UI framework yet. |
| `tools/make_fixture` | Python generator for synthetic test exports with `truth.json` ground truth (SRS §11.2). See `tools/README.md`. |
| `fixtures/` | Generated fixtures (gitignored). |

## Build and test

```bash
npm ci
npm run build                      # frontend; must run before cargo, src-tauri embeds dist/
npm run fixtures                   # python tools/make_fixture --all (needs Python 3 + Pillow; ffmpeg optional)
python tools/sidecars.py fetch     # ffmpeg + exiftool into third_party/ and src-tauri/binaries/ (Windows x64 pins)
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`cargo test` generates the fixtures itself if `fixtures/` is missing, by calling the same Python generator.
Without the sidecar binaries, `cargo build` of `src-tauri` needs `python tools/sidecars.py placeholders`
(what CI does); the sidecar tests then skip. `npm run tauri build` produces the NSIS and MSI installers
under `src-tauri/target/release/bundle/` (unsigned until the certificates in `docs/decisions/signing.md` exist).

By NovaByteWorks, <https://novabyteworks.com>
