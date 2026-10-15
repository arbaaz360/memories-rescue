# Memories Rescue (working name)

Memories Rescue is a local Windows and macOS desktop app (Tauri 2, Rust core) that repairs your own Snapchat "Download My Data" Memories export: it restores capture dates and GPS into the photos and videos, pairs caption overlays with their media, and writes a clean, date-sorted `YYYY/MM` library that photo apps import on the right day and place. Everything runs on your computer; no Snapchat login, no uploads. The requirements live in the public spec at <https://github.com/arbaaz360/memories-rescue-srs> (v0.2); this repository is the application code, currently at milestone M1 (core: ZIP index, part discovery, merged index, JSON-to-file matching).

Not affiliated with, endorsed by, or sponsored by Snap Inc. "Snapchat", "Memories" and "My Eyes Only" are trademarks of Snap Inc. and are used only to describe compatibility.

## Layout

| Path | What it is |
|---|---|
| `crates/mr-core` | Rust library: filename patterns and field parsers (SRS §3.2, §3.5), streaming `Saved Media` reader, ZIP part index with truncation recovery and zip-slip checks, input discovery and part grouping (FR-01 to FR-05), merged index and FR-07 summary, the §3.6 matcher. |
| `crates/mr-meta` | Rust library: metadata writers (SRS §4.3): exiftool `-stay_open` sidecar driver, native JPEG/MP4 writers, and the Q-02 benchmark (`q02-bench`). |
| `crates/mr-cli` | Command line: `mr-cli index <inputs...> [--json]` prints the summary and the per-item match table; `mr-cli order-check <inputs...>` measures the ZIP-order assumption on a `2026a` export (Q-03). |
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

## Try it on an export

```bash
cargo run -p mr-cli -- index path/to/mydata~1729000000000.zip path/to/mydata~1729000000000-2.zip
```

Add `--json` for machine-readable output. On a current export whose JSON carries `sid`/`mid` in `Download Link`:

```bash
cargo run -p mr-cli -- order-check path/to/mydata~1729000000000.zip
```

prints, per day, whether the ZIP entry order agrees with the JSON order. That measurement decides whether the ordinal matching of SRS §3.6 may be trusted (Q-03); the result belongs in the SRS repository once a real export has been checked.

By NovaByteWorks, <https://novabyteworks.com>
