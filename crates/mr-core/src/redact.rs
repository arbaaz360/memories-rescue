//! Redaction for log events (FR-41): the home directory becomes `~`, and
//! URLs and query strings never reach a log line.
//!
//! Every path or free-text value the engine puts into a `tracing` event goes
//! through [`path`] or [`text`] first:
//!
//! ```
//! use std::path::Path;
//! let dest = Path::new("/tmp/out");
//! tracing::info!(dest = %mr_core::redact::path(dest), "job created");
//! tracing::warn!(detail = %mr_core::redact::text("see https://x.invalid/a?sig=1"), "item failed");
//! ```
//!
//! The wrappers redact lazily, in `Display`, so an event below the active
//! level costs nothing. The subscriber, the file and its rotation belong to
//! the app (`src-tauri`); the engine only emits.
//!
//! Event conventions: target is the crate (`mr_core::…`, `mr_job::…`); fields
//! are `job`, `item` (the `YYYY-MM-DD_ID` key), `code` (an SRS section 8 code),
//! `path`/`dest`, `detail`, and counts. No GPS coordinates, captions, JSON
//! content or media bytes are ever logged; item keys and archive entry names
//! are (the log stays on the device and leaves it only in the diagnostics ZIP
//! the user chooses to send).

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, OnceLock};

use regex::Regex;

static HOME: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The current user's home directory, read once: `USERPROFILE` on Windows,
/// `HOME` elsewhere.
pub fn home() -> Option<&'static Path> {
    HOME.get_or_init(|| {
        let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.as_os_str().len() > 1)
    })
    .as_deref()
}

/// A path, displayed with the home directory as `~` and any URL removed.
pub fn path(p: &Path) -> RedactedPath<'_> {
    RedactedPath(p)
}

/// Free text (an error detail, an ffmpeg message), displayed with URLs and
/// query strings removed and the home directory as `~`.
pub fn text(s: &str) -> RedactedText<'_> {
    RedactedText(s)
}

#[derive(Debug, Clone, Copy)]
pub struct RedactedPath<'a>(pub &'a Path);

impl fmt::Display for RedactedPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&path_with_home(self.0, home()))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RedactedText<'a>(pub &'a str);

impl fmt::Display for RedactedText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&text_with_home(self.0, home()))
    }
}

/// [`path`] with an explicit home directory (for tests and callers that
/// know it).
pub fn path_with_home(p: &Path, home: Option<&Path>) -> String {
    text_with_home(&p.to_string_lossy(), home)
}

/// [`text`] with an explicit home directory.
pub fn text_with_home(s: &str, home: Option<&Path>) -> String {
    let s = URL.replace_all(s, "<link removed>");
    let s = QUERY.replace_all(&s, "?<query removed>");
    let s = s.replace(r"\\?\UNC\", r"\\").replace(r"\\?\", "");
    match home {
        Some(h) => replace_home(&s, &h.to_string_lossy()),
        None => s,
    }
}

static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s"'<>]*"#).expect("regex"));
/// A query string left over without its URL: `?name=value…`.
static QUERY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\?[A-Za-z0-9_.\-]+=[^\s"'<>]*"#).expect("regex"));

/// Replace every occurrence of `home` that ends at a path boundary with `~`.
/// Separators match either way round, and on Windows the comparison ignores
/// case (`C:\Users\Adam` and `c:/users/adam` are the same folder).
fn replace_home(s: &str, home: &str) -> String {
    let home = home.trim_end_matches(['/', '\\']);
    if home.is_empty() {
        return s.to_string();
    }
    let norm = |c: char| -> char {
        let c = if c == '\\' { '/' } else { c };
        if cfg!(windows) {
            c.to_ascii_lowercase()
        } else {
            c
        }
    };
    let needle: Vec<char> = home.chars().map(norm).collect();
    let hay: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < hay.len() {
        let end = i + needle.len();
        if end <= hay.len()
            && hay[i..end]
                .iter()
                .map(|&c| norm(c))
                .eq(needle.iter().copied())
            && hay.get(end).is_none_or(|&c| {
                c == '/'
                    || c == '\\'
                    || c.is_whitespace()
                    || c == '"'
                    || c == '\''
                    || c == ','
                    || c == ')'
            })
        {
            out.push('~');
            i = end;
        } else {
            out.push(hay[i]);
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_becomes_tilde_at_path_boundaries_only() {
        let home = Path::new(if cfg!(windows) {
            r"C:\Users\adam"
        } else {
            "/home/adam"
        });
        let inside = home.join("Pictures").join("x.jpg");
        let got = path_with_home(&inside, Some(home));
        assert!(got.starts_with('~'), "{got}");
        assert!(!got.contains("adam"), "{got}");
        assert_eq!(path_with_home(home, Some(home)), "~");
        // A sibling whose name starts with the home folder's name stays.
        let sibling = PathBuf::from(format!("{}2", home.display()));
        assert!(path_with_home(&sibling, Some(home)).contains("adam2"));
        assert_eq!(path_with_home(Path::new("D:/x/y"), Some(home)), "D:/x/y");
    }

    #[cfg(windows)]
    #[test]
    fn windows_home_ignores_case_separators_and_long_path_prefix() {
        let home = Path::new(r"C:\Users\Adam");
        for p in [
            r"\\?\C:\Users\Adam\Pictures\Memories Rescue",
            r"c:/users/adam/Pictures/Memories Rescue",
            r"C:\USERS\ADAM\Pictures\Memories Rescue",
        ] {
            let got = path_with_home(Path::new(p), Some(home));
            assert!(got.starts_with("~"), "{p} -> {got}");
            assert!(got.ends_with("Memories Rescue"), "{got}");
            assert!(!got.to_lowercase().contains("adam"), "{got}");
        }
        assert_eq!(
            path_with_home(Path::new(r"\\?\UNC\nas\share\x"), Some(home)),
            r"\\nas\share\x"
        );
    }

    #[test]
    fn urls_and_query_strings_are_removed() {
        let s = text_with_home(
            "fetch https://app.example.invalid/dmd/memories?uid=1&sid=2&sig=3 failed; then cdn.example.invalid/a.mp4?Expires=1&Signature=2 too",
            None,
        );
        assert!(!s.contains("example.invalid/dmd"), "{s}");
        assert!(
            !s.contains("sid=") && !s.contains("sig=") && !s.contains("Expires="),
            "{s}"
        );
        assert!(
            s.contains("<link removed>") && s.contains("?<query removed>"),
            "{s}"
        );
        assert_eq!(
            text_with_home("2021/12/a.jpg stays", None),
            "2021/12/a.jpg stays"
        );
        assert_eq!(
            text_with_home("what? yes", None),
            "what? yes",
            "a bare ? is not a query"
        );
    }

    #[test]
    fn text_redacts_home_inside_messages() {
        let home = Path::new(if cfg!(windows) {
            r"C:\Users\adam"
        } else {
            "/home/adam"
        });
        let msg = format!(
            "cannot write {}: denied",
            home.join("out").join("a.jpg").display()
        );
        let got = text_with_home(&msg, Some(home));
        assert!(got.starts_with("cannot write ~"), "{got}");
        assert!(got.ends_with(": denied"), "{got}");
    }

    #[test]
    fn display_wrappers_use_the_process_home() {
        let Some(h) = home() else { return };
        let p = h.join("Pictures");
        assert!(path(&p).to_string().starts_with('~'));
        assert!(
            text(&format!("at {}", p.display()))
                .to_string()
                .starts_with("at ~")
        );
    }
}
