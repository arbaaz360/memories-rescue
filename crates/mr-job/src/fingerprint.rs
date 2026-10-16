//! Input fingerprints (section 3.8 `part`, FR-33 AC2): path, size, mtime and
//! SHA-256 of the first MiB. A folder part fingerprints its listing.

use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const HEAD_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartFingerprint {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub sha256_head: String,
}

/// What changed about one input part since the job was created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputChange {
    /// The part is gone or unreadable.
    Missing { path: String },
    /// Size, modification time or leading bytes differ.
    Changed {
        path: String,
        was: PartFingerprint,
        now: PartFingerprint,
    },
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn mtime_of(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Fingerprint a ZIP part or an extracted folder.
pub fn fingerprint(path: &Path) -> io::Result<PartFingerprint> {
    let meta = fs::metadata(path)?;
    let key = path.to_string_lossy().into_owned();
    if meta.is_dir() {
        let mut listing: Vec<(String, u64, i64)> = Vec::new();
        for sub in ["", "json", "memories"] {
            let dir = if sub.is_empty() {
                path.to_path_buf()
            } else {
                path.join(sub)
            };
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let Ok(m) = e.metadata() else { continue };
                if m.is_file() {
                    listing.push((
                        format!("{sub}/{}", e.file_name().to_string_lossy()),
                        m.len(),
                        mtime_of(&m),
                    ));
                }
            }
        }
        listing.sort();
        let mut hasher = Sha256::new();
        let mut size = 0;
        let mut mtime = 0;
        for (name, len, mt) in &listing {
            hasher.update(format!("{name}\t{len}\t{mt}\n").as_bytes());
            size += len;
            mtime = mtime.max(*mt);
        }
        return Ok(PartFingerprint {
            path: key,
            size,
            mtime,
            sha256_head: hex(&hasher.finalize()),
        });
    }
    let mut file = fs::File::open(path)?;
    let mut buf = Vec::with_capacity(HEAD_BYTES as usize);
    (&mut file).take(HEAD_BYTES).read_to_end(&mut buf)?;
    Ok(PartFingerprint {
        path: key,
        size: meta.len(),
        mtime: mtime_of(&meta),
        sha256_head: hex(&Sha256::digest(&buf)),
    })
}

/// The job-level fingerprint (section 3.8 `job.input_fingerprint`).
pub fn combined(parts: &[PartFingerprint]) -> String {
    let mut sorted: Vec<&PartFingerprint> = parts.iter().collect();
    sorted.sort_by(|a, b| {
        (&a.path, a.size, a.mtime, &a.sha256_head).cmp(&(&b.path, b.size, b.mtime, &b.sha256_head))
    });
    let mut hasher = Sha256::new();
    for p in sorted {
        hasher
            .update(format!("{}\t{}\t{}\t{}\n", p.path, p.size, p.mtime, p.sha256_head).as_bytes());
    }
    hex(&hasher.finalize())
}

/// Compare stored fingerprints with the parts as they are now.
pub fn changes(stored: &[PartFingerprint]) -> Vec<InputChange> {
    stored
        .iter()
        .filter_map(|was| match fingerprint(Path::new(&was.path)) {
            Err(_) => Some(InputChange::Missing {
                path: was.path.clone(),
            }),
            Ok(now) if &now != was => Some(InputChange::Changed {
                path: was.path.clone(),
                was: was.clone(),
                now,
            }),
            Ok(_) => None,
        })
        .collect()
}

/// SHA-256 of a whole file, hex.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_and_folder_fingerprints_detect_changes() {
        let dir = std::env::temp_dir().join(format!("mr-job-fp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("memories")).unwrap();
        let zip = dir.join("part.zip");
        fs::write(&zip, b"PK fake part").unwrap();
        let a = fingerprint(&zip).unwrap();
        assert_eq!(a.size, 12);
        assert_eq!(a.sha256_head.len(), 64);
        assert!(changes(std::slice::from_ref(&a)).is_empty());
        fs::write(&zip, b"PK fake part, longer now").unwrap();
        assert!(matches!(
            changes(std::slice::from_ref(&a))[0],
            InputChange::Changed { .. }
        ));
        fs::remove_file(&zip).unwrap();
        assert!(matches!(
            changes(std::slice::from_ref(&a))[0],
            InputChange::Missing { .. }
        ));

        fs::write(dir.join("memories/2021-12-25_A-main.jpg"), b"x").unwrap();
        let f1 = fingerprint(&dir).unwrap();
        fs::write(dir.join("memories/2021-12-25_B-main.jpg"), b"yy").unwrap();
        let f2 = fingerprint(&dir).unwrap();
        assert_ne!(f1.sha256_head, f2.sha256_head);
        assert_eq!(f2.size, 3);
        assert_ne!(
            combined(std::slice::from_ref(&f1)),
            combined(std::slice::from_ref(&f2))
        );
        assert_eq!(
            combined(&[f1.clone(), f2.clone()]),
            combined(&[f2, f1]),
            "order-free"
        );
    }
}
