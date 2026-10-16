//! FR-31: pack the output tree into one or more ZIP64 archives.
//!
//! Runs after every item (and clip join) is finished. What is packed is the
//! set of files the job itself recorded writing (items, overlays, originals,
//! joined clips), in path order, under `Memories Rescue/` so that extracting
//! an archive gives back the same tree. The report folder and the job's own
//! state folder are not packed; the report describes the archives instead.
//!
//! * **Streamed, constant memory.** Each file is copied through a 1 MiB
//!   buffer, stored (media is already compressed), with its CRC-32 computed on
//!   the way. Memory grows only with the number of entries in the archive
//!   being written (the central directory, about 150 bytes per entry).
//! * **Volumes.** With a volume size every archive stays at or below it
//!   (the FAT32 preset is [`crate::options::ZIP_VOLUME_FAT32`]): files are
//!   assigned to archives up front, deterministically, with a generous
//!   per-entry header allowance. A single file larger than the volume size
//!   gets an archive of its own and a note in the report.
//! * **Verified.** Every archive is written as `<name>.partial`, then
//!   re-opened with the engine's own ZIP reader (not the writer library):
//!   intact central directory, the expected names and sizes in order, every
//!   recorded CRC equal to the CRC of the source bytes, and every entry read
//!   back to the end with its CRC checked.
//! * **Resumable.** The partition is keyed by a manifest hash (every packed
//!   path, size and modification time, plus the volume size). Finished
//!   archives are recorded in `job.sqlite` with their size and time; an
//!   interrupted pack keeps them and restarts the archive it was writing. The
//!   intent is recorded before the final rename, so a crash between the
//!   rename and the checkpoint is recognised on the next run. When the tree
//!   changes (a retry, an added part), the manifest changes and the pack is
//!   redone; archives it made before are removed only while their size and
//!   time still match the record, and a file of someone else's at an
//!   archive's name makes the name `_2`, `_3`.

use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{Datelike, Local, TimeZone, Timelike};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::control::Control;
use crate::layout;

/// Header bytes allowed per entry on top of twice its name: local header
/// (30) and central directory record (46), ZIP64 extras (20 + 28), the
/// extended timestamp (9 + 9), with room to spare.
pub const ENTRY_OVERHEAD: u64 = 256;
/// End records of one archive: ZIP64 end of central directory (56), its
/// locator (20), the classic end record (22), with room to spare.
pub const VOLUME_OVERHEAD: u64 = 1024;
const COPY_BUFFER: usize = 1 << 20;

/// One archive of a pack, as recorded and as reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZipVolume {
    /// 1-based position in the pack.
    pub seq: usize,
    /// File name in the destination folder, e.g. `Memories Rescue-002.zip`.
    pub name: String,
    pub files: usize,
    /// Sum of the packed files' sizes.
    pub data_bytes: u64,
    /// Size of the archive on disk.
    pub size: u64,
    /// `done` (written and verified), `verified` (verified, final rename
    /// not yet recorded) or `failed`.
    pub state: String,
    pub error_code: Option<String>,
    pub error_detail: Option<String>,
    /// Larger than the volume size because one file alone is.
    pub oversize: bool,
}

/// A file to pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackFile {
    /// Relative to the output root, with `/`.
    pub rel: String,
    /// Name inside the archive: `Memories Rescue/<rel>`.
    pub entry: String,
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime: i64,
}

impl PackFile {
    /// Bytes this file adds to an archive, generously.
    pub fn cost(&self) -> u64 {
        self.size + 2 * self.entry.len() as u64 + ENTRY_OVERHEAD
    }
}

/// The files of `rels` that exist under `root`, sorted, and those missing.
pub fn collect(
    root: &Path,
    rels: impl IntoIterator<Item = String>,
) -> (Vec<PackFile>, Vec<String>) {
    let mut rels: Vec<String> = rels.into_iter().collect();
    rels.sort();
    rels.dedup();
    let mut files = Vec::with_capacity(rels.len());
    let mut missing = Vec::new();
    for rel in rels {
        match fs::metadata(layout::to_fs(root, &rel)) {
            Ok(m) if m.is_file() => {
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                files.push(PackFile {
                    entry: format!("{}/{rel}", layout::ROOT_DIR),
                    rel,
                    size: m.len(),
                    mtime,
                });
            }
            _ => missing.push(rel),
        }
    }
    (files, missing)
}

/// Assign files to archives in order: each archive's estimated size stays at
/// or below `limit` unless one file alone exceeds it. Returns index ranges.
#[allow(clippy::single_range_in_vec_init)] // one archive is one range
pub fn partition(files: &[PackFile], limit: Option<u64>) -> Vec<std::ops::Range<usize>> {
    let Some(limit) = limit else {
        return vec![0..files.len()];
    };
    let mut out = Vec::new();
    let mut start = 0;
    let mut used = VOLUME_OVERHEAD;
    for (i, f) in files.iter().enumerate() {
        if i > start && used + f.cost() > limit {
            out.push(start..i);
            start = i;
            used = VOLUME_OVERHEAD;
        }
        used += f.cost();
    }
    if start < files.len() || out.is_empty() {
        out.push(start..files.len());
    }
    out
}

/// Archive names for a pack of `count`: `Memories Rescue.zip` for one,
/// `Memories Rescue-001.zip` ... for more.
pub fn volume_names(count: usize) -> Vec<String> {
    if count <= 1 {
        return vec![format!("{}.zip", layout::ROOT_DIR)];
    }
    let width = count.to_string().len().max(3);
    (1..=count)
        .map(|i| format!("{}-{i:0width$}.zip", layout::ROOT_DIR))
        .collect()
}

/// The pack's identity: every packed path, size and time, and the volume size.
pub fn manifest(files: &[PackFile], limit: Option<u64>) -> String {
    let mut h = Sha256::new();
    h.update(format!("limit={limit:?}\n").as_bytes());
    for f in files {
        h.update(format!("{}\t{}\t{}\n", f.rel, f.size, f.mtime).as_bytes());
    }
    crate::fingerprint::hex(&h.finalize())
}

/// Why writing an archive stopped.
#[derive(Debug)]
pub enum PackError {
    /// The destination is full (`DISK_FULL`).
    DiskFull(String),
    /// The destination went away (`DEST_UNAVAILABLE`).
    DestGone(String),
    /// A packed file could not be read, or another write error (`ZIP_WRITE`).
    Write(String),
}

impl PackError {
    pub fn code(&self) -> &'static str {
        match self {
            PackError::DiskFull(_) => "DISK_FULL",
            PackError::DestGone(_) => "DEST_UNAVAILABLE",
            PackError::Write(_) => "ZIP_WRITE",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            PackError::DiskFull(d) | PackError::DestGone(d) | PackError::Write(d) => d,
        }
    }

    fn from_io(e: &io::Error, dest: &Path, what: &str) -> PackError {
        let detail = format!("{what}: {e}");
        if e.kind() == io::ErrorKind::StorageFull || e.raw_os_error() == Some(112) {
            PackError::DiskFull(detail)
        } else if !dest.exists() {
            PackError::DestGone(detail)
        } else {
            PackError::Write(detail)
        }
    }
}

/// What writing one archive produced.
#[derive(Debug)]
pub enum Written {
    /// The CRC-32 of each file's bytes as read, in order.
    Done(Vec<u32>),
    /// Paused or cancelled between two files; the partial file is removed.
    Stopped,
}

/// DOS date and time (local, the ZIP format's own) for a Unix time; the
/// extended timestamp field carries the exact UTC time next to it.
fn dos_time(mtime: i64) -> zip::DateTime {
    let local = Local
        .timestamp_opt(mtime, 0)
        .single()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).unwrap());
    zip::DateTime::from_date_and_time(
        local.year().clamp(1980, 2107) as u16,
        local.month() as u8,
        local.day() as u8,
        local.hour() as u8,
        local.minute() as u8,
        local.second() as u8,
    )
    .unwrap_or_default()
}

/// Write one archive of `files` (under `root`) to `out`. `on_bytes` is told
/// how many bytes were copied after each chunk and returns `false` to stop
/// (pause, cancel, low space); `control` is also checked between files.
pub fn write_volume(
    root: &Path,
    dest: &Path,
    files: &[PackFile],
    out: &Path,
    control: &Control,
    file_delay: Option<std::time::Duration>,
    on_bytes: &mut dyn FnMut(u64) -> bool,
) -> Result<Written, PackError> {
    let file = File::create(out).map_err(|e| PackError::from_io(&e, dest, "create archive"))?;
    let mut zip = zip::ZipWriter::new(BufWriter::with_capacity(COPY_BUFFER, file));
    let mut crcs = Vec::with_capacity(files.len());
    let mut buf = vec![0u8; COPY_BUFFER];
    for f in files {
        if control.is_cancelled() || control.is_paused() {
            drop(zip);
            let _ = fs::remove_file(out);
            return Ok(Written::Stopped);
        }
        if let Some(d) = file_delay {
            std::thread::sleep(d);
        }
        let mut opts = zip::write::FullFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(f.size >= u64::from(u32::MAX))
            .last_modified_time(dos_time(f.mtime))
            .unix_permissions(0o644);
        // Extended timestamp (0x5455): flags = modification time present,
        // then the UTC time, so extraction restores the exact time.
        let mut ut = vec![1u8];
        ut.extend_from_slice(&(f.mtime.clamp(0, i64::from(u32::MAX)) as u32).to_le_bytes());
        opts.add_extra_field(0x5455, ut, false)
            .map_err(|e| PackError::Write(format!("{}: {e}", f.rel)))?;
        zip.start_file(&f.entry, opts)
            .map_err(|e| zip_error(e, dest, &f.rel))?;
        let src_path = layout::long_path(&layout::to_fs(root, &f.rel));
        let mut src =
            File::open(&src_path).map_err(|e| PackError::Write(format!("read {}: {e}", f.rel)))?;
        let mut crc = crc32fast::Hasher::new();
        let mut copied = 0u64;
        loop {
            let n = match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(PackError::Write(format!("read {}: {e}", f.rel))),
            };
            crc.update(&buf[..n]);
            zip.write_all(&buf[..n])
                .map_err(|e| PackError::from_io(&e, dest, "write archive"))?;
            copied += n as u64;
            if !on_bytes(n as u64) {
                drop(src);
                drop(zip);
                let _ = fs::remove_file(out);
                return Ok(Written::Stopped);
            }
        }
        if copied != f.size {
            return Err(PackError::Write(format!(
                "{} changed while it was packed ({} bytes, expected {})",
                f.rel, copied, f.size
            )));
        }
        crcs.push(crc.finalize());
    }
    let mut inner = zip
        .finish()
        .map_err(|e| zip_error(e, dest, "finish archive"))?;
    inner
        .flush()
        .map_err(|e| PackError::from_io(&e, dest, "flush archive"))?;
    let file = inner
        .into_inner()
        .map_err(|e| PackError::from_io(e.error(), dest, "flush archive"))?;
    file.sync_all()
        .map_err(|e| PackError::from_io(&e, dest, "fsync archive"))?;
    Ok(Written::Done(crcs))
}

fn zip_error(e: zip::result::ZipError, dest: &Path, what: &str) -> PackError {
    match e {
        zip::result::ZipError::Io(io) => PackError::from_io(&io, dest, what),
        other => PackError::Write(format!("{what}: {other}")),
    }
}

/// Re-open `path` with the engine's own reader and check it holds exactly
/// `files`, in order, with these CRCs, every entry readable to the end.
/// Returns a description of the first problem.
pub fn verify_volume(path: &Path, files: &[PackFile], crcs: &[u32]) -> Result<(), String> {
    let index = mr_core::zipindex::ZipIndex::open(path).map_err(|e| e.to_string())?;
    if index.integrity.is_truncated() {
        return Err("the central directory could not be read back".into());
    }
    if index.entries.len() != files.len() {
        return Err(format!(
            "{} entries read back, {} written",
            index.entries.len(),
            files.len()
        ));
    }
    let mut reader = index.reader().map_err(|e| e.to_string())?;
    for (i, (e, f)) in index.entries.iter().zip(files).enumerate() {
        if e.name != f.entry || e.size != f.size || e.crc32 != crcs[i] {
            return Err(format!(
                "entry {i} reads back as {:?} ({} bytes, CRC {:08x}), written as {:?} ({} bytes, CRC {:08x})",
                e.name, e.size, e.crc32, f.entry, f.size, crcs[i]
            ));
        }
        match reader.verify_crc(i).map_err(|e| e.to_string())? {
            mr_core::zipindex::CrcCheck::Ok => {}
            mr_core::zipindex::CrcCheck::Mismatch { recorded, computed } => {
                return Err(format!(
                    "{}: CRC {computed:08x} read back, {recorded:08x} recorded",
                    f.entry
                ));
            }
        }
    }
    Ok(())
}

/// Size and modification time (Unix seconds) of a file, for the ownership
/// record of an archive.
pub fn stamp(path: &Path) -> Option<(u64, i64)> {
    let m = fs::metadata(path).ok()?;
    let t = m
        .modified()
        .ok()?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some((m.len(), t))
}

/// Where an archive is written before it is verified.
pub fn partial_of(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".partial");
    PathBuf::from(s)
}

/// `<dest>/<name>`, or `<name stem>_2.zip`, `_3` ... while `taken` says the
/// name holds someone else's file.
pub fn free_name(dest: &Path, name: &str, taken: impl Fn(&Path) -> bool) -> (String, PathBuf) {
    let stem = name.strip_suffix(".zip").unwrap_or(name);
    for n in 1..10_000 {
        let candidate = if n == 1 {
            name.to_string()
        } else {
            format!("{stem}_{n}.zip")
        };
        let path = dest.join(&candidate);
        if !taken(&path) {
            return (candidate, path);
        }
    }
    (name.to_string(), dest.join(name))
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    fn pf(rel: &str, size: u64) -> PackFile {
        PackFile {
            rel: rel.into(),
            entry: format!("{}/{rel}", layout::ROOT_DIR),
            size,
            mtime: 1_640_000_000,
        }
    }

    #[test]
    fn partition_keeps_every_volume_under_the_limit() {
        let files: Vec<PackFile> = (0..100)
            .map(|i| pf(&format!("2021/12/{i:03}.jpg"), 1_000_000))
            .collect();
        let limit = 10_000_000;
        let parts = partition(&files, Some(limit));
        assert_eq!(parts.iter().map(|r| r.len()).sum::<usize>(), 100);
        assert!(parts.len() >= 10);
        for r in &parts {
            let est: u64 =
                VOLUME_OVERHEAD + files[r.clone()].iter().map(PackFile::cost).sum::<u64>();
            assert!(est <= limit, "{r:?}: {est}");
        }
        assert_eq!(partition(&files, None), vec![0..100]);
        assert_eq!(partition(&[], Some(limit)), vec![0..0]);
    }

    #[test]
    fn an_oversize_file_gets_a_volume_of_its_own() {
        let files = vec![pf("a.mp4", 10), pf("b.mp4", 5_000), pf("c.mp4", 10)];
        let parts = partition(&files, Some(2_000));
        assert_eq!(parts, vec![0..1, 1..2, 2..3]);
    }

    #[test]
    fn names_and_manifest() {
        assert_eq!(volume_names(1), vec!["Memories Rescue.zip"]);
        assert_eq!(volume_names(3)[2], "Memories Rescue-003.zip");
        assert_eq!(volume_names(1200)[0], "Memories Rescue-0001.zip");
        let a = vec![pf("a.jpg", 1)];
        let mut b = a.clone();
        b[0].mtime += 1;
        assert_ne!(
            manifest(&a, None),
            manifest(&b, None),
            "a touched file changes the pack"
        );
        assert_ne!(
            manifest(&a, None),
            manifest(&a, Some(1 << 30)),
            "so does the volume size"
        );
        assert_eq!(manifest(&a, None), manifest(&a.clone(), None));
    }
}
