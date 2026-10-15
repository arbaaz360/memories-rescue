//! Metadata writers for Memories Rescue (SRS §4.3) and the M1 Q-02 spike.
//!
//! * [`fields`]: the capture record and every string form the containers want.
//! * [`exiftool`]: exiftool as a long-lived `-stay_open` sidecar (C-05).
//! * [`native_jpeg`]: pure-Rust EXIF writing on `little_exif`.
//! * [`native_mp4`]: pure-Rust `moov` patcher for times, `mdta` Keys and `©xyz`.
//! * [`ffmpeg_remux`]: ffmpeg stream-copy remux with `-metadata`, measured for comparison.
//!
//! The decision between them is recorded in `docs/decisions/Q-02-metadata-writer.md`;
//! `src/bin/q02_bench.rs` produces its numbers.

pub mod exiftool;
pub mod ffmpeg_remux;
pub mod fields;
pub mod native_jpeg;
pub mod native_mp4;

use std::path::{Path, PathBuf};

/// The repository root (two levels above this crate's manifest). Tests and the
/// benchmark use it to find `third_party/` and `fixtures/`.
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/mr-meta sits two levels below the repo root")
        .to_path_buf()
}
