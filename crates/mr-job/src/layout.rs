//! Output layout and naming (SRS section 3.7, NFR-I18N-04).
//!
//! ```text
//! <dest>/Memories Rescue/
//!   YYYY/MM/<YYYY-MM-DD>_<HHMMSS>_<shortid>.<ext>
//!   YYYY/MM/<stem>-overlay.png            overlay saved beside (pass-through, save-separate)
//!   YYYY/MM/originals/<stem>-main.<ext>   keep originals, when an item is composited
//!   _report/                              report.html, report.csv, failed_items.json
//!   .memories-rescue/job-id               marker; job state lives in app data
//!   .memories-rescue/tmp/<item>/          per-item temp on the destination volume
//! ```
//!
//! Relative paths are kept with `/` separators and converted at the edge.

use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, FixedOffset};
use unicode_normalization::UnicodeNormalization;

pub const ROOT_DIR: &str = "Memories Rescue";
pub const STATE_DIR: &str = ".memories-rescue";
pub const MARKER_FILE: &str = "job-id";
pub const TMP_DIR: &str = "tmp";
pub const REPORT_DIR: &str = "_report";
pub const ORIGINALS_DIR: &str = "originals";

/// `<dest>/Memories Rescue`, absolute.
pub fn output_root(dest: &Path) -> PathBuf {
    absolute(dest).join(ROOT_DIR)
}

/// `true` when `p` is a symbolic link, a junction or any other reparse
/// point (or cannot be inspected). The engine never writes through one in
/// its own folders: a junction at `.memories-rescue` would send temp
/// cleanup into another folder.
pub fn is_redirected(p: &Path) -> bool {
    match std::fs::symlink_metadata(p) {
        Ok(m) => {
            if m.file_type().is_symlink() {
                return true;
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
                if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return true;
                }
            }
            false
        }
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Hardening 8: refuse an output root, state folder or temp folder that is
/// a link to somewhere else. The message is the plain text for the user.
pub fn check_own_folders(root: &Path) -> Result<(), String> {
    let state = root.join(STATE_DIR);
    for p in [root.to_path_buf(), state.clone(), state.join(TMP_DIR)] {
        if is_redirected(&p) {
            return Err(
                "The output folder contains a link to another place (a junction or a symbolic link), and Memories Rescue does not write through links. Choose another folder."
                    .into(),
            );
        }
    }
    Ok(())
}

pub fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// The job-lookup key of an output root: absolute, NFC (macOS file systems
/// may hand back NFD), and case-folded where the file system is
/// case-insensitive by default.
pub fn root_key(root: &Path) -> String {
    let s: String = absolute(root).to_string_lossy().nfc().collect();
    let s = s.trim_end_matches(['/', '\\']).to_string();
    if cfg!(any(windows, target_os = "macos")) {
        s.to_lowercase()
    } else {
        s
    }
}

/// NFC form of a string (NFR-I18N-04).
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Render the file name template. `{date}` `YYYY-MM-DD`, `{time}` `HHMMSS`,
/// `{shortid}` the first eight characters of the ID, upper-cased, `{id}` the
/// whole ID, `{ext}` the lower-case extension. Characters that are illegal in
/// file names are replaced by `_`.
pub fn render_name(template: &str, local: DateTime<FixedOffset>, id: &str, ext: &str) -> String {
    let short: String = id.chars().take(8).collect::<String>().to_ascii_uppercase();
    let rendered = template
        .replace("{date}", &local.format("%Y-%m-%d").to_string())
        .replace("{time}", &local.format("%H%M%S").to_string())
        .replace("{shortid}", &short)
        .replace("{id}", id)
        .replace("{ext}", &ext.to_ascii_lowercase());
    let mut clean: String = rendered
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    clean = clean.trim_matches([' ', '.']).to_string();
    if clean.is_empty() {
        clean = format!("{short}.{}", ext.to_ascii_lowercase());
    }
    if !clean
        .to_ascii_lowercase()
        .ends_with(&format!(".{}", ext.to_ascii_lowercase()))
    {
        clean = format!("{clean}.{}", ext.to_ascii_lowercase());
    }
    nfc(&clean)
}

/// `YYYY/MM` of the local capture date (FR-30).
pub fn folder_of(local: DateTime<FixedOffset>) -> String {
    local.format("%Y/%m").to_string()
}

/// `name.ext` → `name_2.ext` (section 3.7 collision suffix).
pub fn with_suffix(name: &str, n: usize) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{}_{n}{}", &name[..dot], &name[dot..]),
        _ => format!("{name}_{n}"),
    }
}

/// Split a relative path into folder and file name.
pub fn split_rel(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => ("", rel),
    }
}

fn stem_of(name: &str) -> &str {
    match name.rfind('.') {
        Some(dot) if dot > 0 => &name[..dot],
        _ => name,
    }
}

/// The overlay saved beside an output: `YYYY/MM/<stem>-overlay.png`.
pub fn overlay_beside(rel: &str) -> String {
    let (folder, name) = split_rel(rel);
    join_rel(folder, &format!("{}-overlay.png", stem_of(name)))
}

/// `YYYY/MM/originals/<stem>-main.<ext>`.
pub fn original_main(rel: &str, ext: &str) -> String {
    let (folder, name) = split_rel(rel);
    join_rel(
        &join_rel(folder, ORIGINALS_DIR),
        &format!("{}-main.{}", stem_of(name), ext.to_ascii_lowercase()),
    )
}

/// `YYYY/MM/originals/<stem>-overlay.png`.
pub fn original_overlay(rel: &str) -> String {
    let (folder, name) = split_rel(rel);
    join_rel(
        &join_rel(folder, ORIGINALS_DIR),
        &format!("{}-overlay.png", stem_of(name)),
    )
}

pub fn join_rel(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// A relative output path on disk under `root`, with the Windows long-path
/// prefix when the result would exceed the classic 260-character limit.
pub fn to_fs(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(part);
    }
    long_path(&p)
}

/// Add `\\?\` (or `\\?\UNC\`) on Windows for absolute paths of 240 characters
/// or more; return other paths unchanged. A no-op elsewhere.
pub fn long_path(p: &Path) -> PathBuf {
    if !cfg!(windows) {
        return p.to_path_buf();
    }
    let s = p.to_string_lossy();
    if s.len() < 240 || s.starts_with(r"\\?\") {
        return p.to_path_buf();
    }
    // `\\?\` turns off normalisation, so the path must be clean first.
    let mut clean = PathBuf::new();
    for c in absolute(p).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other => clean.push(other.as_os_str()),
        }
    }
    let s = clean.to_string_lossy().replace('/', "\\");
    if let Some(unc) = s.strip_prefix(r"\\") {
        PathBuf::from(format!(r"\\?\UNC\{unc}"))
    } else {
        PathBuf::from(format!(r"\\?\{s}"))
    }
}

/// The item key (section 3.8 `item.id`): filename date and upper-cased ID.
pub fn item_key(date: chrono::NaiveDate, id: &str) -> String {
    format!("{}_{}", date.format("%Y-%m-%d"), id.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32, off: i32) -> DateTime<FixedOffset> {
        FixedOffset::east_opt(off)
            .unwrap()
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .unwrap()
    }

    #[test]
    fn default_template_matches_section_3_7() {
        let l = local(2019, 7, 4, 21, 3, 55, 0);
        let name = render_name(
            crate::options::DEFAULT_TEMPLATE,
            l,
            "3f2a9c1e-5b7d-4e1f-9a2b-0c3d4e5f6a7b",
            "MP4",
        );
        assert_eq!(name, "2019-07-04_210355_3F2A9C1E.mp4");
        assert_eq!(folder_of(l), "2019/07");
        assert_eq!(with_suffix(&name, 2), "2019-07-04_210355_3F2A9C1E_2.mp4");
    }

    #[test]
    fn hostile_templates_are_sanitised() {
        let l = local(2021, 12, 25, 18, 42, 7, 3600);
        assert_eq!(
            render_name("../{date}/x:y*{shortid}", l, "A81B", "jpg"),
            "_2021-12-25_x_y_A81B.jpg"
        );
        assert_eq!(render_name("", l, "a81b", "jpg"), "A81B.jpg");
        assert_eq!(render_name("{shortid}.{ext}", l, "A81B", "PNG"), "A81B.png");
    }

    #[test]
    fn derived_paths() {
        let rel = "2021/12/2021-12-25_184207_A81B.jpg";
        assert_eq!(
            overlay_beside(rel),
            "2021/12/2021-12-25_184207_A81B-overlay.png"
        );
        assert_eq!(
            original_main(rel, "jpg"),
            "2021/12/originals/2021-12-25_184207_A81B-main.jpg"
        );
        assert_eq!(
            original_overlay(rel),
            "2021/12/originals/2021-12-25_184207_A81B-overlay.png"
        );
        assert_eq!(split_rel(rel), ("2021/12", "2021-12-25_184207_A81B.jpg"));
    }

    #[test]
    fn long_paths_get_the_prefix_on_windows_only() {
        let deep = std::env::temp_dir().join("a".repeat(250)).join("b.jpg");
        let out = long_path(&deep);
        if cfg!(windows) {
            assert!(
                out.to_string_lossy().starts_with(r"\\?\"),
                "{}",
                out.display()
            );
        } else {
            assert_eq!(out, deep);
        }
        let short = std::env::temp_dir().join("b.jpg");
        assert_eq!(long_path(&short), short);
    }

    #[test]
    fn nfc_and_root_keys() {
        let nfd = "Cafe\u{301}";
        assert_eq!(nfc(nfd), "Caf\u{e9}");
        let a = root_key(Path::new("/tmp/Caf\u{e9}/Memories Rescue"));
        let b = root_key(Path::new("/tmp/Cafe\u{301}/Memories Rescue/"));
        assert_eq!(a, b);
        assert_eq!(
            item_key(
                chrono::NaiveDate::from_ymd_opt(2021, 12, 25).unwrap(),
                "a81b"
            ),
            "2021-12-25_A81B"
        );
    }
}
