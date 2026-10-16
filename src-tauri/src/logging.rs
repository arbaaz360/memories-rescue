//! FR-41: a rotating local log in the app's log folder.
//!
//! * At most [`FILES`] files of [`MAX_FILE`] bytes: 50 MB in all.
//! * Every line goes through [`Redactor`] before it is written: the home
//!   folder becomes `~`, web addresses become `[url]` and query strings
//!   `[query]`, so nothing that identifies a person or an account reaches the
//!   disk (the export's links carry `sid`/`mid` tokens).
//! * `tracing` events from the app and `log` records from the engine crates
//!   (mr-composite logs through `log`) end up in the same file.
//!
//! Export diagnostics zips these files (`engine/diagnostics.rs`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regex::Regex;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, filter::LevelFilter};

/// Bytes per file before it rotates.
pub const MAX_FILE: u64 = 10 * 1024 * 1024;
/// Files kept, the current one included.
pub const FILES: usize = 5;
pub const BASE: &str = "memories-rescue.log";

/// Removes what must not be logged; built once per process.
pub struct Redactor {
    homes: Vec<String>,
    url: Regex,
    query: Regex,
}

impl Redactor {
    /// For the current user's home folder (USERPROFILE on Windows, HOME elsewhere).
    pub fn for_current_user() -> Redactor {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        Redactor::new(&home)
    }

    pub fn new(home: &str) -> Redactor {
        let mut homes = Vec::new();
        if home.len() > 3 {
            homes.push(home.to_string());
            homes.push(home.replace('\\', "/"));
            homes.push(home.replace('/', "\\"));
            // `\\?\C:\Users\...` long-path form.
            homes.push(format!(r"\\?\{}", home.replace('/', "\\")));
        }
        homes.sort_by_key(|h| std::cmp::Reverse(h.len()));
        homes.dedup();
        Redactor {
            homes,
            url: Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s"'<>)]*"#).expect("url regex"),
            query: Regex::new(r#"\?[^\s"'<>?]*="#).expect("query regex"),
        }
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = self.url.replace_all(text, "[url]").into_owned();
        // A query string left without its URL (`?sid=...&mid=...`): drop up to the next space.
        while let Some(m) = self.query.find(&out) {
            let end = out[m.start()..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .map(|i| m.start() + i)
                .unwrap_or(out.len());
            out.replace_range(m.start()..end, "[query]");
        }
        for home in &self.homes {
            out = replace_ignore_case(&out, home, "~");
        }
        out
    }
}

static REDACTOR: std::sync::LazyLock<Redactor> =
    std::sync::LazyLock::new(Redactor::for_current_user);

/// [`Redactor::redact`] for the current user, built once.
pub fn redact(text: &str) -> String {
    REDACTOR.redact(text)
}

/// Windows paths compare case-insensitively; the home folder may appear in either case.
fn replace_ignore_case(text: &str, needle: &str, with: &str) -> String {
    if needle.is_empty() {
        return text.to_string();
    }
    let lower = text.to_lowercase();
    let lneedle = needle.to_lowercase();
    if lower.len() != text.len() || !lower.contains(&lneedle) {
        return text.replace(needle, with);
    }
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(pos) = lower[i..].find(&lneedle) {
        out.push_str(&text[i..i + pos]);
        out.push_str(with);
        i += pos + lneedle.len();
    }
    out.push_str(&text[i..]);
    out
}

/// A size-capped set of log files: `memories-rescue.log`, `.1`, `.2`, ...
pub struct RotatingFile {
    dir: PathBuf,
    max_file: u64,
    files: usize,
    file: Option<File>,
    size: u64,
}

impl RotatingFile {
    pub fn open(dir: &Path, max_file: u64, files: usize) -> io::Result<RotatingFile> {
        fs::create_dir_all(dir)?;
        let path = dir.join(BASE);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(RotatingFile {
            dir: dir.to_path_buf(),
            max_file,
            files: files.max(1),
            file: Some(file),
            size,
        })
    }

    fn numbered(&self, n: usize) -> PathBuf {
        if n == 0 {
            self.dir.join(BASE)
        } else {
            self.dir.join(format!("{BASE}.{n}"))
        }
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file = None;
        let _ = fs::remove_file(self.numbered(self.files - 1));
        for n in (0..self.files - 1).rev() {
            let from = self.numbered(n);
            if from.exists() {
                fs::rename(&from, self.numbered(n + 1))?;
            }
        }
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.numbered(0))?,
        );
        self.size = 0;
        Ok(())
    }

    pub fn write_line(&mut self, line: &[u8]) -> io::Result<()> {
        if self.size > 0 && self.size + line.len() as u64 > self.max_file {
            self.rotate()?;
        }
        let f = match &mut self.file {
            Some(f) => f,
            None => return Ok(()),
        };
        let take = line.len().min(self.max_file as usize);
        f.write_all(&line[..take])?;
        self.size += take as u64;
        Ok(())
    }

    /// The log files that exist, newest first.
    pub fn files_in(dir: &Path) -> Vec<PathBuf> {
        (0..FILES + 1)
            .map(|n| {
                if n == 0 {
                    dir.join(BASE)
                } else {
                    dir.join(format!("{BASE}.{n}"))
                }
            })
            .filter(|p| p.is_file())
            .collect()
    }
}

/// The writer handed to `tracing`: buffers one event, redacts it, appends it.
#[derive(Clone)]
pub struct SharedLog {
    file: Arc<Mutex<RotatingFile>>,
    redactor: Arc<Redactor>,
}

impl SharedLog {
    pub fn new(file: RotatingFile, redactor: Redactor) -> SharedLog {
        SharedLog {
            file: Arc::new(Mutex::new(file)),
            redactor: Arc::new(redactor),
        }
    }
}

pub struct EventWriter {
    log: SharedLog,
    buf: Vec<u8>,
}

impl Write for EventWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&self.buf);
        let clean = self.log.redactor.redact(&text);
        self.buf.clear();
        match self.log.file.lock() {
            Ok(mut f) => f.write_line(clean.as_bytes()),
            Err(_) => Ok(()),
        }
    }
}

impl Drop for EventWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl<'a> MakeWriter<'a> for SharedLog {
    type Writer = EventWriter;

    fn make_writer(&'a self) -> EventWriter {
        EventWriter {
            log: self.clone(),
            buf: Vec::with_capacity(256),
        }
    }
}

/// Starts logging to `dir`. Returns false if the folder cannot be written
/// (the app then runs without a log rather than not at all).
pub fn init(dir: &Path) -> bool {
    let Ok(file) = RotatingFile::open(dir, MAX_FILE, FILES) else {
        return false;
    };
    let writer = SharedLog::new(file, Redactor::for_current_user());
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false)
        .with_target(true)
        .with_filter(LevelFilter::INFO);
    tracing_subscriber::registry()
        .with(layer)
        .try_init()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_hides_home_urls_and_query_strings() {
        let r = Redactor::new(r"C:\Users\Adam");
        let line = r"opened C:\Users\Adam\Downloads\mydata~1.zip and c:\users\adam\Pictures; GET https://cf-st.sc-cdn.net/x?sid=ABC&mid=DEF done; also /?mid=123&x=1 here";
        let out = r.redact(line);
        assert!(!out.to_lowercase().contains("adam"), "{out}");
        assert!(out.contains(r"~\Downloads\mydata~1.zip"), "{out}");
        assert!(
            !out.contains("sc-cdn") && !out.contains("sid=") && !out.contains("mid="),
            "{out}"
        );
        assert!(out.contains("[url]") && out.contains("[query]"), "{out}");
        let mac = Redactor::new("/Users/adam");
        assert_eq!(mac.redact("/Users/adam/Movies/a.mp4"), "~/Movies/a.mp4");
        assert_eq!(r.redact(r"\\?\C:\Users\Adam\x"), r"~\x");
    }

    #[test]
    fn rotation_caps_the_total_size() {
        let dir = std::env::temp_dir().join(format!("mr-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut f = RotatingFile::open(&dir, 1000, 3).unwrap();
        let line = [b'x'; 99];
        for _ in 0..200 {
            f.write_line(&line).unwrap();
            f.write_line(b"\n").unwrap();
        }
        drop(f);
        let files: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(files.len(), 3, "{files:?}");
        let total: u64 = files.iter().map(|p| fs::metadata(p).unwrap().len()).sum();
        assert!(total <= 3000, "{total}");
        assert!(dir.join(format!("{BASE}.2")).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn events_are_redacted_on_the_way_to_disk() {
        let dir = std::env::temp_dir().join(format!("mr-log-ev-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let log = SharedLog::new(
            RotatingFile::open(&dir, MAX_FILE, FILES).unwrap(),
            Redactor::new("/home/sam"),
        );
        {
            let mut w = log.make_writer();
            w.write_all(b"read /home/sam/x.zip from https://example.com/a?b=c\n")
                .unwrap();
        }
        let text = fs::read_to_string(dir.join(BASE)).unwrap();
        assert_eq!(text, "read ~/x.zip from [url]\n");
        let _ = fs::remove_dir_all(&dir);
    }
}
