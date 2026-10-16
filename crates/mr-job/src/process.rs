//! Processing one item on a worker thread (section 4.1 "per-item worker
//! pool"): extract to a temp directory on the destination volume, burn in
//! through the [`Compositor`] seam, write metadata (FR-15 to FR-18), read it
//! back (NFR-REL-05), set file times (FR-20), fsync and rename into place
//! atomically (FR-35).
//!
//! Placement is idempotent: a target that already holds the same bytes (a
//! crash between rename and checkpoint) is accepted as written; a target
//! this item wrote before (a retry) is replaced; anything else is someone
//! else's file and the name gets a `_2`, `_3` suffix.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use little_exif::exif_tag::ExifTag;
use little_exif::filetype::FileExtension;
use little_exif::metadata::Metadata;
use mr_core::discover::{Discovery, Source};
use mr_core::zipindex::{EntryReader, ZipIndexError};
use mr_meta::fields::Capture;
use mr_meta::{native_jpeg, native_mp4, native_png};

use crate::compositor::{
    CompositeRequest, Compositor, RejoinRequest, RejoinSegment, TranscodeRequest,
};
use crate::fingerprint::sha256_file;
use crate::layout;
use crate::options::{OverlayMode, TestHooks};
use crate::plan::SourceRef;
use crate::platform;
use crate::state::ItemRow;

/// What a worker needs besides the item.
#[derive(Debug, Clone)]
pub struct WorkSettings {
    pub overlay_mode: OverlayMode,
    pub keep_originals: bool,
    pub windows_compat: bool,
    pub hooks: TestHooks,
    /// The user's destination folder; its disappearance is `DEST_UNAVAILABLE`.
    pub dest: PathBuf,
}

/// The files an item ended up with.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Written {
    pub output_rel: String,
    pub output_sha256: String,
    pub overlay_rel: Option<String>,
    pub originals_main: Option<String>,
    pub originals_overlay: Option<String>,
    pub composite_pending: bool,
    /// Output kept, but the item is reported failed (METADATA_WRITE,
    /// FFMPEG_FAIL fallback): code and detail.
    pub kept_with_error: Option<(String, String)>,
    pub bytes: u64,
    /// Report-worthy notes from the compositor (code, detail), e.g.
    /// `overlay_aspect_mismatch`; the item still succeeded.
    pub warnings: Vec<(String, String)>,
}

/// A clip group to join (FR-27, SRS section 4.5).
#[derive(Debug, Clone)]
pub struct GroupWork {
    pub id: String,
    /// Planned output, relative to the output root.
    pub output_rel: String,
    /// The clips in order; their own outputs are written as items.
    pub members: Vec<ItemRow>,
    /// This group's earlier output (path, hash), replaced when written again.
    pub previous: Option<(String, Option<String>)>,
}

/// What joining a group wrote.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GroupWritten {
    pub output_rel: String,
    pub output_sha256: String,
    pub warnings: Vec<(String, String)>,
}

/// Why an item did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// This item failed; the job carries on (NFR-REL-03).
    Item { code: String, detail: String },
    /// A condition that stops the job until it is cleared
    /// (`DEST_UNAVAILABLE`, `PERMISSION_DENIED`, `DISK_FULL`,
    /// `INPUT_UNAVAILABLE`); the item goes back to the queue.
    Job { code: String, detail: String },
}

fn item(code: &str, detail: impl Into<String>) -> Failure {
    Failure::Item {
        code: code.into(),
        detail: detail.into(),
    }
}

fn job(code: &str, detail: impl Into<String>) -> Failure {
    Failure::Job {
        code: code.into(),
        detail: detail.into(),
    }
}

/// Classify an error while writing to the destination.
fn dest_error(e: io::Error, settings: &WorkSettings, what: &str) -> Failure {
    if !settings.dest.exists() {
        return job("DEST_UNAVAILABLE", "the output drive is not available");
    }
    match e.kind() {
        io::ErrorKind::PermissionDenied => job(
            "PERMISSION_DENIED",
            format!("cannot write to the output folder ({what})"),
        ),
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => {
            job("DISK_FULL", "the output drive is full")
        }
        io::ErrorKind::NotFound if !settings.dest.exists() => {
            job("DEST_UNAVAILABLE", "the output drive is not available")
        }
        _ => item("UNKNOWN", format!("{what}: {e}")),
    }
}

pub fn system_time(cap: &Capture) -> SystemTime {
    let secs = cap.utc.timestamp();
    if secs >= 0 {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    } else {
        UNIX_EPOCH - Duration::from_secs(secs.unsigned_abs())
    }
}

/// Write the metadata for `ext` from `src` to `dst`.
pub fn write_metadata(ext: &str, src: &Path, dst: &Path, cap: &Capture) -> Result<(), String> {
    match ext {
        "jpg" | "jpeg" => native_jpeg::write_jpeg(src, dst, cap)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        "png" => native_png::write_png(src, dst, cap)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        "mp4" | "mov" | "m4v" => native_mp4::write_mp4(src, dst, cap)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        other => Err(format!("no metadata writer for .{other} files")),
    }
}

fn exif_strings(md: &Metadata) -> HashMap<&'static str, String> {
    let mut out = HashMap::new();
    for tag in md {
        let (name, value) = match tag {
            ExifTag::DateTimeOriginal(s) => ("DateTimeOriginal", s),
            ExifTag::CreateDate(s) => ("CreateDate", s),
            ExifTag::OffsetTimeOriginal(s) => ("OffsetTimeOriginal", s),
            ExifTag::OffsetTime(s) => ("OffsetTime", s),
            ExifTag::GPSLatitudeRef(s) => ("GPSLatitudeRef", s),
            ExifTag::GPSLongitudeRef(s) => ("GPSLongitudeRef", s),
            ExifTag::GPSDateStamp(s) => ("GPSDateStamp", s),
            _ => continue,
        };
        out.insert(name, value.trim_end_matches('\0').to_string());
    }
    out
}

/// Read the metadata back and compare with what was meant (NFR-REL-05).
pub fn verify(ext: &str, path: &Path, cap: &Capture) -> Result<(), String> {
    match ext {
        "jpg" | "jpeg" | "png" => {
            let bytes = fs::read(path).map_err(|e| e.to_string())?;
            let md = if ext == "png" {
                if bytes.len() < 8 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
                    return Err("output is not a PNG".into());
                }
                native_png::read_png(path).map_err(|e| e.to_string())?
            } else {
                if bytes.len() < 4 || bytes[..2] != [0xFF, 0xD8] {
                    return Err("output is not a JPEG".into());
                }
                let tail = &bytes[bytes.len().saturating_sub(4096)..];
                if !tail.windows(2).any(|w| w == [0xFF, 0xD9]) {
                    return Err("JPEG has no end-of-image marker".into());
                }
                Metadata::new_from_vec(&bytes, FileExtension::JPEG).map_err(|e| e.to_string())?
            };
            let tags = exif_strings(&md);
            let want_local = cap.exif_local();
            let want_offset = cap.exif_offset();
            for (tag, want) in [
                ("DateTimeOriginal", &want_local),
                ("CreateDate", &want_local),
                ("OffsetTimeOriginal", &want_offset),
                ("OffsetTime", &want_offset),
            ] {
                match tags.get(tag) {
                    Some(v) if v == want => {}
                    other => return Err(format!("{tag} reads {other:?}, expected {want}")),
                }
            }
            match (cap.gps_refs(), tags.get("GPSLatitudeRef")) {
                (Some((_, lat_ref, _, lon_ref)), Some(r)) if r == lat_ref => {
                    if tags.get("GPSLongitudeRef").map(String::as_str) != Some(lon_ref) {
                        return Err("GPS longitude reference differs".into());
                    }
                }
                (None, None) => {}
                (want, got) => {
                    return Err(format!(
                        "GPS reads {got:?}, expected {:?}",
                        want.map(|w| w.1)
                    ));
                }
            }
            Ok(())
        }
        "mp4" | "mov" | "m4v" => {
            let s = native_mp4::summarize(path).map_err(|e| e.to_string())?;
            if s.mvhd_creation != Some(cap.qt_seconds()) {
                return Err(format!(
                    "mvhd creation {:?}, expected {}",
                    s.mvhd_creation,
                    cap.qt_seconds()
                ));
            }
            if s.tkhd_creations.iter().any(|t| *t != cap.qt_seconds()) {
                return Err("a tkhd creation time differs".into());
            }
            let key = |k: &str| s.keys.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
            if key("com.apple.quicktime.creationdate") != Some(cap.iso8601_local()) {
                return Err("com.apple.quicktime.creationdate differs".into());
            }
            if key("com.apple.quicktime.location.ISO6709") != cap.iso6709() {
                return Err("com.apple.quicktime.location.ISO6709 differs".into());
            }
            if s.xyz != cap.iso6709() {
                return Err("the xyz location atom differs".into());
            }
            Ok(())
        }
        other => Err(format!("cannot verify .{other} files")),
    }
}

/// A worker: its own readers per part (section 4.2), shared discovery.
pub struct Worker<'a> {
    d: &'a Discovery,
    part_by_path: &'a HashMap<String, usize>,
    readers: HashMap<usize, EntryReader<'a>>,
    root: PathBuf,
    compositor: &'a dyn Compositor,
    settings: &'a WorkSettings,
}

/// A file this item wrote earlier: its path and, when recorded, its hash.
/// Only a file that still has the recorded bytes counts as ours; one the
/// user has edited since is theirs and is never overwritten or removed.
#[derive(Debug, Clone)]
struct Owned {
    rel: String,
    sha: Option<String>,
}

impl Owned {
    fn matches(&self, rel: &str, existing_sha: &str) -> bool {
        self.rel.eq_ignore_ascii_case(rel) && self.sha.as_deref().is_none_or(|s| s == existing_sha)
    }
}

enum Placement {
    /// Nothing there: write.
    Write,
    /// The same bytes are already there.
    AlreadyThere,
    /// Our earlier output: overwrite.
    Replace,
}

impl<'a> Worker<'a> {
    pub fn new(
        d: &'a Discovery,
        part_by_path: &'a HashMap<String, usize>,
        root: PathBuf,
        compositor: &'a dyn Compositor,
        settings: &'a WorkSettings,
    ) -> Worker<'a> {
        Worker {
            d,
            part_by_path,
            readers: HashMap::new(),
            root,
            compositor,
            settings,
        }
    }

    fn tmp_dir(&self, key: &str) -> PathBuf {
        layout::long_path(
            &self
                .root
                .join(layout::STATE_DIR)
                .join(layout::TMP_DIR)
                .join(key),
        )
    }

    /// Stream one entry out of its part into `dst`.
    fn extract(&mut self, src: &SourceRef, dst: &Path) -> Result<(), Failure> {
        let Some(&part_idx) = self.part_by_path.get(&src.part_path) else {
            return Err(job(
                "INPUT_UNAVAILABLE",
                format!("{} is no longer available", src.part_label),
            ));
        };
        let part = self.d.part(part_idx);
        let mut write_error: Option<io::Error> = None;
        let copy = |r: &mut dyn Read, write_error: &mut Option<io::Error>| -> io::Result<u64> {
            let file = match File::create(dst) {
                Ok(f) => f,
                Err(e) => {
                    *write_error = Some(e);
                    return Err(io::Error::other("write failed"));
                }
            };
            let mut out = BufWriter::with_capacity(256 * 1024, file);
            let mut buf = vec![0u8; 256 * 1024];
            let mut total = 0u64;
            loop {
                let n = r.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                if let Err(e) = out.write_all(&buf[..n]) {
                    *write_error = Some(e);
                    return Err(io::Error::other("write failed"));
                }
                total += n as u64;
            }
            if let Err(e) = out.flush() {
                *write_error = Some(e);
                return Err(io::Error::other("write failed"));
            }
            Ok(total)
        };
        let result: Result<u64, io::Error> = match &part.source {
            Source::Zip(z) => {
                let index = match z.entry(src.entry_index) {
                    Some(e) if e.name == src.entry_name => src.entry_index,
                    _ => match z.find(&src.entry_name) {
                        Some(e) => e.index,
                        None => {
                            return Err(item(
                                "MEDIA_DECODE",
                                format!("{} is not in {} any more", src.entry_name, src.part_label),
                            ));
                        }
                    },
                };
                let reader = match self.readers.entry(part_idx) {
                    std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
                    std::collections::hash_map::Entry::Vacant(v) => {
                        v.insert(z.reader().map_err(|e| {
                            job(
                                "INPUT_UNAVAILABLE",
                                format!("{} cannot be opened: {e}", src.part_label),
                            )
                        })?)
                    }
                };
                match reader.with_entry(index, |r| copy(r, &mut write_error)) {
                    Ok(n) => Ok(n),
                    Err(ZipIndexError::Io { source, .. }) => Err(source),
                    Err(e) => Err(io::Error::other(e.to_string())),
                }
            }
            Source::Folder(folder) => {
                let path = folder
                    .root
                    .join(src.entry_name.replace('/', std::path::MAIN_SEPARATOR_STR));
                match File::open(&path) {
                    Ok(mut f) => copy(&mut f, &mut write_error),
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        return Err(job(
                            "INPUT_UNAVAILABLE",
                            format!("{} is no longer in {}", src.entry_name, src.part_label),
                        ));
                    }
                    Err(e) => Err(e),
                }
            }
        };
        if let Some(e) = write_error {
            return Err(dest_error(e, self.settings, "temp file"));
        }
        match result {
            Ok(_) => Ok(()),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof
                ) =>
            {
                Err(item(
                    "ZIP_ENTRY_CRC",
                    format!("{} in {} is damaged", src.entry_name, src.part_label),
                ))
            }
            Err(e) => Err(item(
                "ZIP_ENTRY_CRC",
                format!(
                    "{} in {} cannot be read: {e}",
                    src.entry_name, src.part_label
                ),
            )),
        }
    }

    fn decide(&self, rel: &str, sha: &str, own: &[Owned]) -> Result<Placement, Failure> {
        let target = layout::to_fs(&self.root, rel);
        if !target.exists() {
            return Ok(Placement::Write);
        }
        let existing =
            sha256_file(&target).map_err(|e| dest_error(e, self.settings, "existing output"))?;
        if existing == sha {
            Ok(Placement::AlreadyThere)
        } else if own.iter().any(|o| o.matches(rel, &existing)) {
            Ok(Placement::Replace)
        } else {
            Err(item("NAME_TAKEN", rel.to_string()))
        }
    }

    /// The first name from `rel`, `rel_2`, ... that this file may take.
    fn choose(&self, rel: &str, sha: &str, own: &[Owned]) -> Result<(String, Placement), Failure> {
        let (folder, name) = layout::split_rel(rel);
        for n in 1..10_000 {
            let candidate = if n == 1 {
                rel.to_string()
            } else {
                layout::join_rel(folder, &layout::with_suffix(name, n))
            };
            match self.decide(&candidate, sha, own) {
                Ok(p) => return Ok((candidate, p)),
                Err(Failure::Item { code, .. }) if code == "NAME_TAKEN" => continue,
                Err(e) => return Err(e),
            }
        }
        Err(item("UNKNOWN", format!("no free name for {rel}")))
    }

    fn place(&self, tmp: &Path, rel: &str, placement: Placement) -> Result<(), Failure> {
        let target = layout::to_fs(&self.root, rel);
        match placement {
            Placement::AlreadyThere => {
                let _ = fs::remove_file(tmp);
                Ok(())
            }
            Placement::Write | Placement::Replace => {
                let parent = target.parent().expect("output has a parent");
                fs::create_dir_all(parent).map_err(|e| dest_error(e, self.settings, "folder"))?;
                fs::rename(tmp, &target).map_err(|e| dest_error(e, self.settings, "rename"))?;
                let _ = platform::sync_dir(parent);
                Ok(())
            }
        }
    }

    /// Finish a temp file: times, fsync.
    fn seal(&self, path: &Path, cap: &Capture) -> Result<(), Failure> {
        platform::set_file_times(path, system_time(cap))
            .map_err(|e| dest_error(e, self.settings, "file times"))?;
        File::options()
            .read(true)
            .write(true)
            .open(path)
            .and_then(|f| f.sync_all())
            .map_err(|e| dest_error(e, self.settings, "fsync"))
    }

    fn copy_file(&self, from: &Path, to: &Path) -> Result<(), Failure> {
        fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| dest_error(e, self.settings, "copy"))
    }

    /// Process one item. `attempt` is the attempt number just recorded.
    pub fn process(&mut self, row: &ItemRow) -> Result<Written, Failure> {
        let p = &row.plan;
        let s = self.settings;
        if !s.dest.exists() {
            return Err(job("DEST_UNAVAILABLE", "the output drive is not available"));
        }
        let tmp = self.tmp_dir(&p.key);
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| dest_error(e, s, "temp folder"))?;
        let result = self.process_in(row, &tmp);
        let _ = fs::remove_dir_all(&tmp);
        result
    }

    fn process_in(&mut self, row: &ItemRow, tmp: &Path) -> Result<Written, Failure> {
        let p = &row.plan;
        let s = self.settings;
        if let Some(d) = s.hooks.delay() {
            std::thread::sleep(d);
        }
        if s.hooks.fails(&p.key, &p.file_id) {
            return Err(item("TEST_INJECTED", "failure injected by a test hook"));
        }
        let cap = p.capture();
        let ext = p.ext.as_str();
        let main_raw = tmp.join(format!("main.{ext}"));
        self.extract(&p.main, &main_raw)?;
        let overlay_raw = match (&p.overlay, s.overlay_mode) {
            (Some(o), mode) if mode != OverlayMode::Ignore => {
                let path = tmp.join("overlay.png");
                self.extract(o, &path)?;
                Some(path)
            }
            _ => None,
        };

        // Burn-in through the seam (FR-23, FR-24).
        let mut written = Written {
            bytes: p.main.size + p.overlay.as_ref().map(|o| o.size).unwrap_or(0),
            ..Written::default()
        };
        let mut base = main_raw.clone();
        let mut composited = false;
        if let (Some(ov), true) = (&overlay_raw, s.overlay_mode.burns_in()) {
            let out = tmp.join(format!("composite.{ext}"));
            let req = CompositeRequest {
                kind: p.kind,
                main: &main_raw,
                overlay: ov,
                out: &out,
                ext,
                windows_compat: s.windows_compat,
                item_key: &p.key,
            };
            match self.compositor.composite(&req) {
                Ok(o) if o.produced() && out.is_file() => {
                    written.warnings.extend(
                        o.warnings()
                            .iter()
                            .map(|w| (w.code.clone(), w.detail.clone())),
                    );
                    base = out;
                    composited = true;
                }
                Ok(_) => written.composite_pending = true,
                Err(e) => written.kept_with_error = Some((e.code, e.detail)),
            }
        }

        // Windows compatibility (FR-51, section 4.4 "HEVC sources"): a video
        // that was not composited (a composite is H.264 already) is made
        // H.264 when the compositor says it needs it.
        if s.windows_compat && p.is_video() && !composited {
            let out = tmp.join(format!("compat.{ext}"));
            let req = TranscodeRequest {
                main: &base,
                out: &out,
                ext,
                item_key: &p.key,
            };
            match self.compositor.compat_transcode(&req) {
                Ok(o) if o.produced() && out.is_file() => {
                    written.warnings.push((
                        "h264_compat".into(),
                        "converted to H.264 for Windows compatibility".into(),
                    ));
                    written.warnings.extend(
                        o.warnings()
                            .iter()
                            .map(|w| (w.code.clone(), w.detail.clone())),
                    );
                    base = out;
                }
                Ok(_) => {}
                Err(e) => {
                    if written.kept_with_error.is_none() {
                        written.kept_with_error = Some((e.code, e.detail));
                    }
                }
            }
        }

        // Metadata (FR-15 to FR-18) and read-back (NFR-REL-05).
        let tagged = tmp.join(format!("out.{ext}"));
        match write_metadata(ext, &base, &tagged, &cap).and_then(|()| verify(ext, &tagged, &cap)) {
            Ok(()) => {}
            Err(why) => {
                let _ = fs::remove_file(&tagged);
                self.copy_file(&base, &tagged)?;
                if written.kept_with_error.is_none() {
                    written.kept_with_error = Some(("METADATA_WRITE".into(), why));
                }
            }
        }
        self.seal(&tagged, &cap)?;
        let sha = sha256_file(&tagged).map_err(|e| dest_error(e, s, "hash"))?;

        let own: Vec<Owned> = row
            .recorded_outputs()
            .into_iter()
            .map(|rel| Owned {
                sha: (row.output_path.as_deref() == Some(rel.as_str()))
                    .then(|| row.output_sha256.clone())
                    .flatten(),
                rel,
            })
            .collect();
        let (main_rel, main_place) = self.choose(&p.output_rel, &sha, &own)?;

        // Sidecars: the overlay beside the file, or originals/ after a burn-in.
        let mut sidecars: Vec<(PathBuf, String, Placement)> = Vec::new();
        if let Some(ov) = &overlay_raw {
            let beside = s.overlay_mode == OverlayMode::SaveSeparate
                || (s.overlay_mode.burns_in() && !composited);
            if beside {
                let f = tmp.join("overlay-out.png");
                self.copy_file(ov, &f)?;
                self.seal(&f, &cap)?;
                let osha = sha256_file(&f).map_err(|e| dest_error(e, s, "hash"))?;
                let (rel, place) = self.choose(&layout::overlay_beside(&main_rel), &osha, &own)?;
                written.overlay_rel = Some(rel.clone());
                sidecars.push((f, rel, place));
            }
            if composited && s.keep_originals && s.overlay_mode == OverlayMode::BurnInKeepOriginals
            {
                let f = tmp.join(format!("original.{ext}"));
                if write_metadata(ext, &main_raw, &f, &cap).is_err() {
                    let _ = fs::remove_file(&f);
                    self.copy_file(&main_raw, &f)?;
                }
                self.seal(&f, &cap)?;
                let msha = sha256_file(&f).map_err(|e| dest_error(e, s, "hash"))?;
                let (rel, place) =
                    self.choose(&layout::original_main(&main_rel, ext), &msha, &own)?;
                written.originals_main = Some(rel.clone());
                sidecars.push((f, rel, place));
                let g = tmp.join("original-overlay.png");
                self.copy_file(ov, &g)?;
                self.seal(&g, &cap)?;
                let gsha = sha256_file(&g).map_err(|e| dest_error(e, s, "hash"))?;
                let (rel, place) =
                    self.choose(&layout::original_overlay(&main_rel), &gsha, &own)?;
                written.originals_overlay = Some(rel.clone());
                sidecars.push((g, rel, place));
            }
        }

        // Sidecars first, the main output last.
        for (f, rel, place) in sidecars {
            self.place(&f, &rel, place)?;
        }
        self.place(&tagged, &main_rel, main_place)?;

        // An earlier write under other names (the plan changed): remove it.
        let keep: Vec<String> = [
            Some(main_rel.clone()),
            written.overlay_rel.clone(),
            written.originals_main.clone(),
            written.originals_overlay.clone(),
        ]
        .into_iter()
        .flatten()
        .map(|r| r.to_lowercase())
        .collect();
        for old in &row.previous_outputs {
            if keep.contains(&old.to_lowercase()) {
                continue;
            }
            let path = layout::to_fs(&self.root, old);
            let ours = match own.iter().find(|o| o.rel.eq_ignore_ascii_case(old)) {
                Some(o) => match &o.sha {
                    Some(recorded) => sha256_file(&path).is_ok_and(|now| &now == recorded),
                    None => true,
                },
                None => false,
            };
            if ours {
                let _ = fs::remove_file(path);
            }
        }

        written.output_rel = main_rel;
        written.output_sha256 = sha;
        Ok(written)
    }

    /// Join one clip group (FR-27): extract every clip (and its overlay when
    /// the mode burns in), let the compositor join them, write the first
    /// clip's capture metadata onto the result (section 4.5), and place it
    /// atomically. `Ok(None)` when the compositor declines. The clips' own
    /// outputs are not touched.
    pub fn process_group(&mut self, g: &GroupWork) -> Result<Option<GroupWritten>, Failure> {
        let s = self.settings;
        if !s.dest.exists() {
            return Err(job("DEST_UNAVAILABLE", "the output drive is not available"));
        }
        let tmp = self.tmp_dir(&format!("group-{}", g.id));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| dest_error(e, s, "temp folder"))?;
        let result = self.process_group_in(g, &tmp);
        let _ = fs::remove_dir_all(&tmp);
        result
    }

    fn process_group_in(
        &mut self,
        g: &GroupWork,
        tmp: &Path,
    ) -> Result<Option<GroupWritten>, Failure> {
        let s = self.settings;
        let first = g
            .members
            .first()
            .ok_or_else(|| item("UNKNOWN", "empty clip group"))?;
        let ext = first.plan.ext.as_str();
        let mut paths: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
        for (k, m) in g.members.iter().enumerate() {
            let main = tmp.join(format!("seg{k}.{}", m.plan.ext));
            self.extract(&m.plan.main, &main)?;
            let overlay = match (&m.plan.overlay, s.overlay_mode.burns_in()) {
                (Some(o), true) => {
                    let path = tmp.join(format!("seg{k}-overlay.png"));
                    self.extract(o, &path)?;
                    Some(path)
                }
                _ => None,
            };
            paths.push((main, overlay));
        }
        let segments: Vec<RejoinSegment<'_>> = paths
            .iter()
            .map(|(main, overlay)| RejoinSegment {
                main,
                overlay: overlay.as_deref(),
            })
            .collect();
        let joined = tmp.join(format!("joined.{ext}"));
        let req = RejoinRequest {
            group_id: &g.id,
            segments: &segments,
            out: &joined,
        };
        let mut warnings: Vec<(String, String)> = Vec::new();
        match self.compositor.rejoin(&req) {
            Ok(o) if o.produced() && joined.is_file() => warnings.extend(
                o.warnings()
                    .iter()
                    .map(|w| (w.code.clone(), w.detail.clone())),
            ),
            Ok(_) => return Ok(None),
            Err(e) => return Err(item(&e.code, e.detail)),
        }

        let cap = first.plan.capture();
        let tagged = tmp.join(format!("joined-out.{ext}"));
        if let Err(why) =
            write_metadata(ext, &joined, &tagged, &cap).and_then(|()| verify(ext, &tagged, &cap))
        {
            let _ = fs::remove_file(&tagged);
            self.copy_file(&joined, &tagged)?;
            warnings.push(("METADATA_WRITE".into(), why));
        }
        self.seal(&tagged, &cap)?;
        let sha = sha256_file(&tagged).map_err(|e| dest_error(e, s, "hash"))?;
        let own: Vec<Owned> = g
            .previous
            .iter()
            .map(|(rel, sha)| Owned {
                rel: rel.clone(),
                sha: sha.clone(),
            })
            .collect();
        let (rel, place) = self.choose(&g.output_rel, &sha, &own)?;
        self.place(&tagged, &rel, place)?;
        if let Some((old, old_sha)) = &g.previous
            && !old.eq_ignore_ascii_case(&rel)
        {
            let path = layout::to_fs(&self.root, old);
            let ours = match old_sha {
                Some(recorded) => sha256_file(&path).is_ok_and(|now| &now == recorded),
                None => true,
            };
            if ours {
                let _ = fs::remove_file(path);
            }
        }
        Ok(Some(GroupWritten {
            output_rel: rel,
            output_sha256: sha,
            warnings,
        }))
    }
}

/// Is `path` a finished output for `row`? Used when rebuilding state from
/// the output folder (`JOB_STATE_CORRUPT`).
pub fn adopt_check(root: &Path, row: &ItemRow) -> Option<String> {
    let target = layout::to_fs(root, &row.plan.output_rel);
    if !target.is_file() {
        return None;
    }
    let cap = row.plan.capture();
    if verify(&row.plan.ext, &target, &cap).is_err() {
        return None;
    }
    sha256_file(&target).ok()
}
