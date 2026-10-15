//! Index of one ZIP part without extracting anything (FR-04, FR-06,
//! NFR-PRV-07).
//!
//! The normal path reads the central directory (ZIP64 included) and lists
//! every entry with its name, sizes, CRC-32 and archive order. When the
//! central directory cannot be read, which is what a download cut short
//! looks like, the archive is `PART_TRUNCATED`: local file headers are walked
//! from the start instead, every entry whose data lies entirely before the
//! cut stays usable, the entry straddling the cut is reported as partial,
//! and whatever followed it is lost without a name.
//!
//! Entry names are untrusted. `..` components, absolute paths, drive letters,
//! control characters and symlink entries are flagged at index time and such
//! entries are never opened.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use thiserror::Error;
use zip::ZipArchive;
use zip::read::{ZipReadOptions, read_zipfile_from_stream_with_options};

const LOCAL_HEADER_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x03, 0x04];
const DATA_DESCRIPTOR_SIGNATURE: u32 = 0x0807_4b50;
const FLAG_DATA_DESCRIPTOR: u16 = 1 << 3;
const METHOD_DEFLATE: u16 = 8;
const ZIP64_EXTRA_ID: u16 = 0x0001;
const U32_MAX: u64 = u32::MAX as u64;

/// Why an entry name is refused (NFR-PRV-07, "zip slip").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnsafeReason {
    EmptyName,
    /// A `..` path component.
    ParentTraversal,
    /// Starts with `/` or `\`.
    AbsolutePath,
    /// Starts with a drive letter such as `C:`.
    DriveLetter,
    /// Symbolic link entry (Unix mode `S_IFLNK`).
    Symlink,
    /// NUL or another control character in the name.
    ControlCharacter,
}

impl fmt::Display for UnsafeReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UnsafeReason::EmptyName => "empty name",
            UnsafeReason::ParentTraversal => "parent-directory component",
            UnsafeReason::AbsolutePath => "absolute path",
            UnsafeReason::DriveLetter => "drive letter",
            UnsafeReason::Symlink => "symbolic link",
            UnsafeReason::ControlCharacter => "control character in name",
        })
    }
}

/// Classify an entry name. `is_symlink` comes from the entry's Unix mode.
pub fn check_entry_name(name: &str, is_symlink: bool) -> Option<UnsafeReason> {
    if is_symlink {
        return Some(UnsafeReason::Symlink);
    }
    if name.is_empty() {
        return Some(UnsafeReason::EmptyName);
    }
    if name.chars().any(char::is_control) {
        return Some(UnsafeReason::ControlCharacter);
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Some(UnsafeReason::AbsolutePath);
    }
    let b = name.as_bytes();
    if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
        return Some(UnsafeReason::DriveLetter);
    }
    if name.split(['/', '\\']).any(|component| component == "..") {
        return Some(UnsafeReason::ParentTraversal);
    }
    None
}

/// One entry as listed in the archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryInfo {
    /// Position in the archive: central-directory order, or local-header
    /// order when the archive was recovered. This is the "ZIP entry order"
    /// of SRS section 3.6.
    pub index: usize,
    pub name: String,
    pub size: u64,
    pub compressed_size: u64,
    pub crc32: u32,
    pub is_dir: bool,
    pub header_start: u64,
    pub unsafe_reason: Option<UnsafeReason>,
    /// `false` only for the entry cut by a truncation.
    pub complete: bool,
}

impl EntryInfo {
    /// A regular, safe, complete file entry: the only kind ever read.
    pub fn is_usable(&self) -> bool {
        !self.is_dir && self.unsafe_reason.is_none() && self.complete
    }
}

/// Whether the archive could be read normally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Integrity {
    Intact,
    /// `PART_TRUNCATED`: no usable central directory. `readable` entries
    /// were recovered from local headers; `partial` is the entry whose data
    /// is cut; entries after it are lost without a name.
    Truncated {
        readable: usize,
        partial: Option<String>,
        scanned_to: u64,
        detail: String,
    },
}

impl Integrity {
    pub fn is_truncated(&self) -> bool {
        matches!(self, Integrity::Truncated { .. })
    }
}

/// Outcome of reading an entry end to end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrcCheck {
    Ok,
    /// `ZIP_ENTRY_CRC`: the data does not match the recorded CRC-32.
    Mismatch {
        recorded: u32,
        computed: u32,
    },
}

#[derive(Debug, Error)]
pub enum ZipIndexError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{path}: not a ZIP archive ({detail})")]
    NotZip { path: PathBuf, detail: String },
    #[error("{path}: entry {index} does not exist")]
    NoSuchEntry { path: PathBuf, index: usize },
    #[error("{path}: entry {name:?} is refused ({reason}) and is never read")]
    UnsafeEntry {
        path: PathBuf,
        name: String,
        reason: UnsafeReason,
    },
    #[error("{path}: entry {name:?} is cut by the truncation")]
    PartialEntry { path: PathBuf, name: String },
    #[error("{path}: entry {name:?}: {source}")]
    Zip {
        path: PathBuf,
        name: String,
        #[source]
        source: zip::result::ZipError,
    },
}

/// The index of one ZIP part.
#[derive(Debug)]
pub struct ZipIndex {
    pub path: PathBuf,
    pub file_size: u64,
    pub entries: Vec<EntryInfo>,
    pub integrity: Integrity,
    /// Central directory or an entry used ZIP64 fields.
    pub zip64: bool,
}

impl ZipIndex {
    /// Read the central directory, or recover from local headers if it is
    /// missing or damaged. Never reads entry data.
    pub fn open(path: &Path) -> Result<ZipIndex, ZipIndexError> {
        let io_err = |source| ZipIndexError::Io {
            path: path.to_path_buf(),
            source,
        };
        let file = File::open(path).map_err(io_err)?;
        let file_size = file.metadata().map_err(io_err)?.len();
        match ZipArchive::new(BufReader::new(file)) {
            Ok(archive) => Self::from_central_directory(path, file_size, &archive),
            Err(err) => Self::recover(path, file_size, err.to_string()),
        }
    }

    fn from_central_directory(
        path: &Path,
        file_size: u64,
        archive: &ZipArchive<BufReader<File>>,
    ) -> Result<ZipIndex, ZipIndexError> {
        let mut entries = Vec::with_capacity(archive.len());
        let mut zip64 = archive.len() >= 0xFFFF || archive.central_directory_start() >= U32_MAX;
        for index in 0..archive.len() {
            let entry = archive
                .by_index_data(index)
                .map_err(|source| ZipIndexError::Zip {
                    path: path.to_path_buf(),
                    name: format!("#{index}"),
                    source,
                })?;
            let name = match entry.name() {
                Ok(name) => name.into_owned(),
                Err(_) => String::from_utf8_lossy(entry.name_raw()).into_owned(),
            };
            let size = entry.size();
            let compressed_size = entry.compressed_size();
            if size >= U32_MAX || compressed_size >= U32_MAX || entry.header_start() >= U32_MAX {
                zip64 = true;
            }
            let unsafe_reason = check_entry_name(&name, entry.is_symlink());
            entries.push(EntryInfo {
                index,
                name,
                size,
                compressed_size,
                crc32: entry.crc32(),
                is_dir: entry.is_dir(),
                header_start: entry.header_start(),
                unsafe_reason,
                complete: true,
            });
        }
        Ok(ZipIndex {
            path: path.to_path_buf(),
            file_size,
            entries,
            integrity: Integrity::Intact,
            zip64,
        })
    }

    /// Walk local file headers from the start of the file (FR-04).
    fn recover(path: &Path, file_size: u64, detail: String) -> Result<ZipIndex, ZipIndexError> {
        let io_err = |source| ZipIndexError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut file = BufReader::new(File::open(path).map_err(io_err)?);
        let mut entries: Vec<EntryInfo> = Vec::new();
        let mut partial = None;
        let mut zip64 = false;
        let mut pos: u64 = 0;
        loop {
            file.seek(SeekFrom::Start(pos)).map_err(io_err)?;
            let Some(header) = LocalHeader::read(&mut file).map_err(io_err)? else {
                break;
            };
            zip64 |= header.zip64;
            let data_start = pos + header.len();
            let unsafe_reason = check_entry_name(&header.name, false);
            let is_dir = header.name.ends_with('/');
            let mut info = EntryInfo {
                index: entries.len(),
                name: header.name.clone(),
                size: header.uncompressed_size,
                compressed_size: header.compressed_size,
                crc32: header.crc32,
                is_dir,
                header_start: pos,
                unsafe_reason,
                complete: true,
            };
            let next = if header.data_descriptor {
                match header.method {
                    METHOD_DEFLATE => {
                        file.seek(SeekFrom::Start(data_start)).map_err(io_err)?;
                        match inflate_to_end(&mut file) {
                            Ok(consumed) => {
                                let descriptor_at = data_start + consumed;
                                file.seek(SeekFrom::Start(descriptor_at)).map_err(io_err)?;
                                match DataDescriptor::read(&mut file, consumed).map_err(io_err)? {
                                    Some(d) => {
                                        info.compressed_size = d.compressed_size;
                                        info.size = d.uncompressed_size;
                                        info.crc32 = d.crc32;
                                        descriptor_at + d.len
                                    }
                                    None => u64::MAX,
                                }
                            }
                            Err(_) => u64::MAX,
                        }
                    }
                    // Stored data with a trailing descriptor has no length
                    // anywhere before the data; nothing after it can be found.
                    _ => u64::MAX,
                }
            } else {
                data_start.saturating_add(header.compressed_size)
            };
            if next > file_size {
                info.complete = false;
                partial = Some(info.name.clone());
                entries.push(info);
                break;
            }
            entries.push(info);
            pos = next;
        }
        if entries.is_empty() {
            return Err(ZipIndexError::NotZip {
                path: path.to_path_buf(),
                detail,
            });
        }
        let readable = entries.iter().filter(|e| e.complete).count();
        Ok(ZipIndex {
            path: path.to_path_buf(),
            file_size,
            entries,
            integrity: Integrity::Truncated {
                readable,
                partial,
                scanned_to: pos,
                detail,
            },
            zip64,
        })
    }

    pub fn entry(&self, index: usize) -> Option<&EntryInfo> {
        self.entries.get(index)
    }

    pub fn find(&self, name: &str) -> Option<&EntryInfo> {
        self.entries.iter().find(|e| e.name == name)
    }

    /// Entries that may be read: regular, safe and complete.
    pub fn usable(&self) -> impl Iterator<Item = &EntryInfo> {
        self.entries.iter().filter(|e| e.is_usable())
    }

    /// Refused entries with their reasons.
    pub fn unsafe_entries(&self) -> impl Iterator<Item = (&EntryInfo, UnsafeReason)> {
        self.entries
            .iter()
            .filter_map(|e| e.unsafe_reason.map(|r| (e, r)))
    }

    /// Open the part for reading entries. Parses the central directory once;
    /// keep the reader around when reading many entries.
    pub fn reader(&self) -> Result<EntryReader<'_>, ZipIndexError> {
        let io_err = |source| ZipIndexError::Io {
            path: self.path.clone(),
            source,
        };
        let file = BufReader::new(File::open(&self.path).map_err(io_err)?);
        let source = match self.integrity {
            Integrity::Intact => {
                let archive = ZipArchive::new(file).map_err(|source| ZipIndexError::Zip {
                    path: self.path.clone(),
                    name: String::new(),
                    source,
                })?;
                ReaderSource::Archive(archive)
            }
            Integrity::Truncated { .. } => ReaderSource::Raw(file),
        };
        Ok(EntryReader {
            index: self,
            source,
        })
    }

    /// Run `f` over one entry's decompressed bytes.
    pub fn with_entry<T>(
        &self,
        index: usize,
        f: impl FnOnce(&mut dyn Read) -> io::Result<T>,
    ) -> Result<T, ZipIndexError> {
        self.reader()?.with_entry(index, f)
    }

    /// Read an entry end to end and compare its CRC-32 with the recorded one.
    pub fn verify_crc(&self, index: usize) -> Result<CrcCheck, ZipIndexError> {
        self.reader()?.verify_crc(index)
    }
}

enum ReaderSource {
    Archive(ZipArchive<BufReader<File>>),
    Raw(BufReader<File>),
}

/// A handle for reading entries of one part.
pub struct EntryReader<'a> {
    index: &'a ZipIndex,
    source: ReaderSource,
}

impl EntryReader<'_> {
    fn checked(&self, index: usize) -> Result<&EntryInfo, ZipIndexError> {
        let path = &self.index.path;
        let entry = self
            .index
            .entries
            .get(index)
            .ok_or_else(|| ZipIndexError::NoSuchEntry {
                path: path.clone(),
                index,
            })?;
        if let Some(reason) = entry.unsafe_reason {
            return Err(ZipIndexError::UnsafeEntry {
                path: path.clone(),
                name: entry.name.clone(),
                reason,
            });
        }
        if !entry.complete {
            return Err(ZipIndexError::PartialEntry {
                path: path.clone(),
                name: entry.name.clone(),
            });
        }
        Ok(entry)
    }

    /// Run `f` over one entry's decompressed bytes. The archive's own CRC
    /// check applies: reading past the end of a damaged entry fails with an
    /// `InvalidData` error.
    pub fn with_entry<T>(
        &mut self,
        index: usize,
        f: impl FnOnce(&mut dyn Read) -> io::Result<T>,
    ) -> Result<T, ZipIndexError> {
        let entry = self.checked(index)?.clone();
        let path = self.index.path.clone();
        let zip_err = |source| ZipIndexError::Zip {
            path: path.clone(),
            name: entry.name.clone(),
            source,
        };
        let io_err = |source| ZipIndexError::Io {
            path: path.clone(),
            source,
        };
        match &mut self.source {
            ReaderSource::Archive(archive) => {
                let mut file = archive.by_index(index).map_err(zip_err)?;
                f(&mut file).map_err(io_err)
            }
            ReaderSource::Raw(file) => {
                file.seek(SeekFrom::Start(entry.header_start))
                    .map_err(io_err)?;
                let options = ZipReadOptions::default()
                    .override_compressed_size(entry.compressed_size)
                    .override_uncompressed_size(entry.size)
                    .override_crc(entry.crc32);
                let mut zf = read_zipfile_from_stream_with_options(file, options)
                    .map_err(zip_err)?
                    .ok_or_else(|| ZipIndexError::NotZip {
                        path: path.clone(),
                        detail: format!("no local header at {}", entry.header_start),
                    })?;
                f(&mut zf).map_err(io_err)
            }
        }
    }

    /// Read an entry end to end and compare its CRC-32 with the recorded one.
    pub fn verify_crc(&mut self, index: usize) -> Result<CrcCheck, ZipIndexError> {
        let recorded = self.checked(index)?.crc32;
        let result = self.with_entry(index, |r| {
            let mut hasher = crc32fast::Hasher::new();
            let mut buf = [0u8; 64 * 1024];
            loop {
                let n = r.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            Ok(hasher.finalize())
        });
        match result {
            Ok(computed) if computed == recorded => Ok(CrcCheck::Ok),
            Ok(computed) => Ok(CrcCheck::Mismatch { recorded, computed }),
            // The zip crate reports its own CRC failure as InvalidData.
            Err(ZipIndexError::Io { source, .. })
                if source.kind() == io::ErrorKind::InvalidData =>
            {
                Ok(CrcCheck::Mismatch {
                    recorded,
                    computed: 0,
                })
            }
            Err(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------
// Local header recovery
// ---------------------------------------------------------------------------

struct LocalHeader {
    name: String,
    method: u16,
    data_descriptor: bool,
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    name_len: u16,
    extra_len: u16,
    zip64: bool,
}

impl LocalHeader {
    const FIXED: u64 = 30;

    fn len(&self) -> u64 {
        Self::FIXED + self.name_len as u64 + self.extra_len as u64
    }

    /// `Ok(None)` at end of file, at the central directory, or at anything
    /// that is not a local header.
    fn read(r: &mut impl Read) -> io::Result<Option<LocalHeader>> {
        let mut sig = [0u8; 4];
        if !read_fully(r, &mut sig)? || sig != LOCAL_HEADER_SIGNATURE {
            return Ok(None);
        }
        let mut fixed = [0u8; 26];
        if !read_fully(r, &mut fixed)? {
            return Ok(None);
        }
        let flags = u16::from_le_bytes([fixed[2], fixed[3]]);
        let method = u16::from_le_bytes([fixed[4], fixed[5]]);
        let crc32 = u32::from_le_bytes([fixed[10], fixed[11], fixed[12], fixed[13]]);
        let mut compressed_size =
            u32::from_le_bytes([fixed[14], fixed[15], fixed[16], fixed[17]]) as u64;
        let mut uncompressed_size =
            u32::from_le_bytes([fixed[18], fixed[19], fixed[20], fixed[21]]) as u64;
        let name_len = u16::from_le_bytes([fixed[22], fixed[23]]);
        let extra_len = u16::from_le_bytes([fixed[24], fixed[25]]);
        let mut name = vec![0u8; name_len as usize];
        if !read_fully(r, &mut name)? {
            return Ok(None);
        }
        let mut extra = vec![0u8; extra_len as usize];
        if !read_fully(r, &mut extra)? {
            return Ok(None);
        }
        let mut zip64 = false;
        if compressed_size == U32_MAX || uncompressed_size == U32_MAX {
            let mut i = 0;
            while i + 4 <= extra.len() {
                let id = u16::from_le_bytes([extra[i], extra[i + 1]]);
                let len = u16::from_le_bytes([extra[i + 2], extra[i + 3]]) as usize;
                let body = &extra[i + 4..(i + 4 + len).min(extra.len())];
                if id == ZIP64_EXTRA_ID {
                    zip64 = true;
                    let mut off = 0;
                    if uncompressed_size == U32_MAX && body.len() >= off + 8 {
                        uncompressed_size =
                            u64::from_le_bytes(body[off..off + 8].try_into().unwrap());
                        off += 8;
                    }
                    if compressed_size == U32_MAX && body.len() >= off + 8 {
                        compressed_size =
                            u64::from_le_bytes(body[off..off + 8].try_into().unwrap());
                    }
                }
                i += 4 + len;
            }
        }
        Ok(Some(LocalHeader {
            name: String::from_utf8_lossy(&name).into_owned(),
            method,
            data_descriptor: flags & FLAG_DATA_DESCRIPTOR != 0,
            crc32,
            compressed_size,
            uncompressed_size,
            name_len,
            extra_len,
            zip64,
        }))
    }
}

fn read_fully(r: &mut impl Read, buf: &mut [u8]) -> io::Result<bool> {
    match r.read_exact(buf) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e),
    }
}

/// Inflate a raw deflate stream to its end and return how many compressed
/// bytes it occupied. Fails if the stream is cut.
fn inflate_to_end(file: &mut BufReader<File>) -> io::Result<u64> {
    let mut decoder = flate2::read::DeflateDecoder::new(file);
    io::copy(&mut decoder, &mut io::sink())?;
    Ok(decoder.total_in())
}

struct DataDescriptor {
    crc32: u32,
    compressed_size: u64,
    uncompressed_size: u64,
    len: u64,
}

impl DataDescriptor {
    /// Parse a descriptor whose compressed size is known to be `consumed`,
    /// which decides between the 32-bit and ZIP64 (64-bit) layouts.
    fn read(r: &mut impl Read, consumed: u64) -> io::Result<Option<DataDescriptor>> {
        let mut buf = [0u8; 24];
        let mut got = 0;
        while got < buf.len() {
            match r.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        let bytes = &buf[..got];
        let mut off = 0;
        let mut len = 0u64;
        if bytes.len() >= 4
            && u32::from_le_bytes(bytes[0..4].try_into().unwrap()) == DATA_DESCRIPTOR_SIGNATURE
        {
            off = 4;
            len = 4;
        }
        if bytes.len() < off + 12 {
            return Ok(None);
        }
        let crc32 = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap());
        let c32 = u32::from_le_bytes(bytes[off + 4..off + 8].try_into().unwrap()) as u64;
        if c32 == consumed {
            let u32size = u32::from_le_bytes(bytes[off + 8..off + 12].try_into().unwrap()) as u64;
            return Ok(Some(DataDescriptor {
                crc32,
                compressed_size: c32,
                uncompressed_size: u32size,
                len: len + 12,
            }));
        }
        if bytes.len() < off + 20 {
            return Ok(None);
        }
        let c64 = u64::from_le_bytes(bytes[off + 4..off + 12].try_into().unwrap());
        let u64size = u64::from_le_bytes(bytes[off + 12..off + 20].try_into().unwrap());
        if c64 == consumed {
            return Ok(Some(DataDescriptor {
                crc32,
                compressed_size: c64,
                uncompressed_size: u64size,
                len: len + 20,
            }));
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};
    use zip::CompressionMethod;
    use zip::write::{SimpleFileOptions, ZipWriter};

    fn temp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mr-core-zipindex-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn write_zip(entries: &[(&str, &[u8], CompressionMethod)]) -> Vec<u8> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data, method) in entries {
            zw.start_file(
                *name,
                SimpleFileOptions::default().compression_method(*method),
            )
            .unwrap();
            zw.write_all(data).unwrap();
        }
        zw.finish().unwrap().into_inner()
    }

    fn sample_entries() -> Vec<(&'static str, Vec<u8>, CompressionMethod)> {
        vec![
            (
                "index.html",
                b"<html>fixture</html>".to_vec(),
                CompressionMethod::Deflated,
            ),
            (
                "json/memories_history.json",
                br#"{"Saved Media": [{"Date": "2021-12-25 18:42:07 UTC"}]}"#.to_vec(),
                CompressionMethod::Deflated,
            ),
            (
                "memories/2021-12-25_A81B-main.jpg",
                (0..5000u32).map(|i| (i * 7 % 251) as u8).collect(),
                CompressionMethod::Stored,
            ),
            (
                "memories/2021-12-25_A81B-overlay.png",
                b"png bytes ".repeat(300),
                CompressionMethod::Deflated,
            ),
            (
                "memories/2019-07-04_3F2A-main.mp4",
                (0..7000u32).map(|i| (i * 13 % 199) as u8).collect(),
                CompressionMethod::Stored,
            ),
        ]
    }

    fn sample_zip() -> Vec<u8> {
        let entries = sample_entries();
        let refs: Vec<(&str, &[u8], CompressionMethod)> = entries
            .iter()
            .map(|(n, d, m)| (*n, d.as_slice(), *m))
            .collect();
        write_zip(&refs)
    }

    #[test]
    fn lists_central_directory_in_order_without_reading_data() {
        let path = temp_path("intact.zip");
        std::fs::write(&path, sample_zip()).unwrap();
        let index = ZipIndex::open(&path).unwrap();
        assert_eq!(index.integrity, Integrity::Intact);
        assert!(!index.zip64);
        let names: Vec<&str> = index.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "index.html",
                "json/memories_history.json",
                "memories/2021-12-25_A81B-main.jpg",
                "memories/2021-12-25_A81B-overlay.png",
                "memories/2019-07-04_3F2A-main.mp4",
            ]
        );
        for (i, e) in index.entries.iter().enumerate() {
            assert_eq!(e.index, i);
            assert!(e.is_usable());
        }
        let jpg = index.find("memories/2021-12-25_A81B-main.jpg").unwrap();
        assert_eq!(jpg.size, 5000);
        assert_eq!(jpg.compressed_size, 5000, "stored");
        let expected_crc = crc32fast::hash(&sample_entries()[2].1);
        assert_eq!(jpg.crc32, expected_crc);
        assert_eq!(index.usable().count(), 5);
    }

    #[test]
    fn reads_entries_and_verifies_crc() {
        let path = temp_path("read.zip");
        std::fs::write(&path, sample_zip()).unwrap();
        let index = ZipIndex::open(&path).unwrap();
        let mut reader = index.reader().unwrap();
        let text = reader
            .with_entry(1, |r| {
                let mut s = String::new();
                r.read_to_string(&mut s)?;
                Ok(s)
            })
            .unwrap();
        assert!(text.contains("Saved Media"));
        for i in 0..index.entries.len() {
            assert_eq!(reader.verify_crc(i).unwrap(), CrcCheck::Ok, "entry {i}");
        }
        assert!(matches!(
            reader.with_entry(9, |_| Ok(())),
            Err(ZipIndexError::NoSuchEntry { index: 9, .. })
        ));
    }

    #[test]
    fn corrupt_stored_entry_fails_crc_check() {
        let mut bytes = sample_zip();
        // Flip a byte inside the stored jpg data: after its local header
        // (30 + name length) at its header offset.
        let path = temp_path("corrupt.zip");
        std::fs::write(&path, &bytes).unwrap();
        let index = ZipIndex::open(&path).unwrap();
        let jpg = index
            .find("memories/2021-12-25_A81B-main.jpg")
            .unwrap()
            .clone();
        let data_start = jpg.header_start as usize + 30 + jpg.name.len();
        bytes[data_start + 100] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();
        let index = ZipIndex::open(&path).unwrap();
        assert_eq!(index.integrity, Integrity::Intact);
        match index.verify_crc(jpg.index).unwrap() {
            CrcCheck::Mismatch { recorded, .. } => assert_eq!(recorded, jpg.crc32),
            CrcCheck::Ok => panic!("corruption not detected"),
        }
        assert_eq!(
            index.verify_crc(0).unwrap(),
            CrcCheck::Ok,
            "other entries fine"
        );
    }

    #[test]
    fn truncated_archive_is_recovered_from_local_headers() {
        let bytes = sample_zip();
        let path = temp_path("full.zip");
        std::fs::write(&path, &bytes).unwrap();
        let full = ZipIndex::open(&path).unwrap();
        // Cut inside the fourth entry's data: the first three stay readable.
        let fourth = &full.entries[3];
        let cut = (fourth.header_start + 30 + fourth.name.len() as u64 + 10) as usize;
        let path = temp_path("truncated.zip");
        std::fs::write(&path, &bytes[..cut]).unwrap();
        let index = ZipIndex::open(&path).unwrap();
        match &index.integrity {
            Integrity::Truncated {
                readable, partial, ..
            } => {
                assert_eq!(*readable, 3);
                assert_eq!(
                    partial.as_deref(),
                    Some("memories/2021-12-25_A81B-overlay.png")
                );
            }
            Integrity::Intact => panic!("must be truncated"),
        }
        assert_eq!(
            index.entries.len(),
            4,
            "three complete plus the partial one"
        );
        assert!(!index.entries[3].complete);
        let names: Vec<&str> = index.usable().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "index.html",
                "json/memories_history.json",
                "memories/2021-12-25_A81B-main.jpg"
            ]
        );
        // Recovered metadata matches the central directory's.
        for (a, b) in index.entries.iter().take(3).zip(&full.entries) {
            assert_eq!(
                (a.size, a.compressed_size, a.crc32),
                (b.size, b.compressed_size, b.crc32),
                "{}",
                a.name
            );
        }
        // Readable entries can still be read and verified.
        let mut reader = index.reader().unwrap();
        for i in 0..3 {
            assert_eq!(reader.verify_crc(i).unwrap(), CrcCheck::Ok, "entry {i}");
        }
        let text = reader
            .with_entry(1, |r| {
                let mut s = String::new();
                r.read_to_string(&mut s)?;
                Ok(s)
            })
            .unwrap();
        assert!(text.contains("Saved Media"));
        assert!(matches!(
            reader.with_entry(3, |_| Ok(())),
            Err(ZipIndexError::PartialEntry { .. })
        ));
    }

    #[test]
    fn truncated_streaming_archive_with_data_descriptors_is_recovered() {
        // A streaming writer leaves sizes and CRC in trailing descriptors.
        let entries = sample_entries();
        let mut zw = ZipWriter::new_stream(Cursor::new(Vec::new()));
        for (name, data, _) in &entries {
            zw.start_file(
                *name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Deflated),
            )
            .unwrap();
            zw.write_all(data).unwrap();
        }
        let bytes = zw.finish().unwrap().into_inner().into_inner();
        let path = temp_path("stream-full.zip");
        std::fs::write(&path, &bytes).unwrap();
        let full = ZipIndex::open(&path).unwrap();
        assert_eq!(full.integrity, Integrity::Intact);
        assert_eq!(full.entries.len(), 5);

        let cut = full.entries[4].header_start as usize + 60;
        let path = temp_path("stream-cut.zip");
        std::fs::write(&path, &bytes[..cut]).unwrap();
        let index = ZipIndex::open(&path).unwrap();
        assert!(index.integrity.is_truncated());
        let names: Vec<&str> = index.usable().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            full.entries[..4]
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>()
        );
        for (a, b) in index.entries.iter().take(4).zip(&full.entries) {
            assert_eq!((a.size, a.crc32), (b.size, b.crc32), "{}", a.name);
        }
        let mut reader = index.reader().unwrap();
        for i in 0..4 {
            assert_eq!(reader.verify_crc(i).unwrap(), CrcCheck::Ok, "entry {i}");
        }
    }

    #[test]
    fn not_a_zip_is_reported() {
        let path = temp_path("not.zip");
        std::fs::write(&path, b"just some text, not an archive at all").unwrap();
        assert!(matches!(
            ZipIndex::open(&path),
            Err(ZipIndexError::NotZip { .. })
        ));
        let missing = temp_path("missing.zip");
        assert!(matches!(
            ZipIndex::open(&missing),
            Err(ZipIndexError::Io { .. })
        ));
    }

    #[test]
    fn zip_slip_names_and_symlinks_are_refused_but_listed() {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for name in [
            "memories/ok.jpg",
            "../evil.jpg",
            "memories/../../evil2.jpg",
            "/abs/evil3.jpg",
            "C:\\evil4.jpg",
            "memories/dot/./fine.jpg",
        ] {
            zw.start_file(name, opts).unwrap();
            zw.write_all(b"x").unwrap();
        }
        zw.add_symlink("memories/link.jpg", "../../etc/passwd", opts)
            .unwrap();
        let bytes = zw.finish().unwrap().into_inner();
        let path = temp_path("slip.zip");
        std::fs::write(&path, bytes).unwrap();
        let index = ZipIndex::open(&path).unwrap();
        let reasons: Vec<(&str, Option<UnsafeReason>)> = index
            .entries
            .iter()
            .map(|e| (e.name.as_str(), e.unsafe_reason))
            .collect();
        assert_eq!(
            reasons,
            [
                ("memories/ok.jpg", None),
                ("../evil.jpg", Some(UnsafeReason::ParentTraversal)),
                (
                    "memories/../../evil2.jpg",
                    Some(UnsafeReason::ParentTraversal)
                ),
                ("/abs/evil3.jpg", Some(UnsafeReason::AbsolutePath)),
                ("C:\\evil4.jpg", Some(UnsafeReason::DriveLetter)),
                ("memories/dot/./fine.jpg", None),
                ("memories/link.jpg", Some(UnsafeReason::Symlink)),
            ]
        );
        assert_eq!(index.usable().count(), 2);
        assert!(matches!(
            index.with_entry(1, |_| Ok(())),
            Err(ZipIndexError::UnsafeEntry {
                reason: UnsafeReason::ParentTraversal,
                ..
            })
        ));
        assert!(matches!(
            index.with_entry(6, |_| Ok(())),
            Err(ZipIndexError::UnsafeEntry {
                reason: UnsafeReason::Symlink,
                ..
            })
        ));
    }

    #[test]
    fn name_checks() {
        assert_eq!(check_entry_name("memories/a.jpg", false), None);
        assert_eq!(
            check_entry_name("a/../b", false),
            Some(UnsafeReason::ParentTraversal)
        );
        assert_eq!(
            check_entry_name("a\\..\\b", false),
            Some(UnsafeReason::ParentTraversal)
        );
        assert_eq!(
            check_entry_name("..", false),
            Some(UnsafeReason::ParentTraversal)
        );
        assert_eq!(
            check_entry_name("a/..b", false),
            None,
            "..b is a legal name"
        );
        assert_eq!(
            check_entry_name("/etc/x", false),
            Some(UnsafeReason::AbsolutePath)
        );
        assert_eq!(
            check_entry_name("\\\\server\\x", false),
            Some(UnsafeReason::AbsolutePath)
        );
        assert_eq!(
            check_entry_name("d:/x", false),
            Some(UnsafeReason::DriveLetter)
        );
        assert_eq!(check_entry_name("", false), Some(UnsafeReason::EmptyName));
        assert_eq!(
            check_entry_name("a\0b", false),
            Some(UnsafeReason::ControlCharacter)
        );
        assert_eq!(check_entry_name("fine", true), Some(UnsafeReason::Symlink));
    }

    #[test]
    fn zip64_archive_is_indexed() {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .large_file(true);
        zw.start_file("memories/2021-12-25_A81B-main.jpg", opts)
            .unwrap();
        zw.write_all(&[1u8; 1234]).unwrap();
        let bytes = zw.finish().unwrap().into_inner();
        let path = temp_path("zip64.zip");
        std::fs::write(&path, &bytes).unwrap();
        let index = ZipIndex::open(&path).unwrap();
        assert_eq!(index.entries[0].size, 1234);
        assert_eq!(index.verify_crc(0).unwrap(), CrcCheck::Ok);
        // Cut after the entry so the central directory is lost; the ZIP64
        // extra field in the local header must still give the sizes.
        let cut = index.entries[0].header_start as usize
            + 30
            + index.entries[0].name.len()
            + 20 /* zip64 extra */
            + 1234;
        let path = temp_path("zip64-cut.zip");
        std::fs::write(&path, &bytes[..cut.min(bytes.len())]).unwrap();
        let recovered = ZipIndex::open(&path).unwrap();
        assert!(recovered.integrity.is_truncated());
        assert_eq!(recovered.entries[0].size, 1234);
        assert!(recovered.zip64);
        assert_eq!(recovered.verify_crc(0).unwrap(), CrcCheck::Ok);
    }
}
