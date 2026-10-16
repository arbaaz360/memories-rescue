//! FR-41: a rotating local log in the app's log folder.
//!
//! * At most [`FILES`] files of [`MAX_FILE`] bytes: 50 MB in all.
//! * Every line goes through [`Redactor`] before it is written, so nothing
//!   that identifies a person, an account or a memory reaches the disk:
//!   - the home folder becomes `~`, in every spelling: either separator, any
//!     case, the `\\?\` long-path form, and the escaped form that JSON and
//!     `{:?}` output use (`C:\\Users\\...`);
//!   - web addresses become `[url]` and query strings `[query]` (the
//!     export's links carry `sid`/`mid` tokens);
//!   - export entry names (`memories/...`, any media file name) become
//!     `[entry]`;
//!   - item keys (`YYYY-MM-DD_<id>`, any case, UUID or not) become `[item]`
//!     and any other UUID `[id]`, except a job id after `job=` or `job_id=`,
//!     which is random and lets support follow a job through the log.
//! * A panic is logged with its location before the default hook runs
//!   ([`install_panic_hook`]), so a crash in the field leaves a trace in the
//!   diagnostics ZIP.
//! * `tracing` events from the app and `log` records from the engine crates
//!   (mr-composite logs through `log`) end up in the same file.
//!
//! Export diagnostics zips these files (`engine/diagnostics.rs`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regex::Regex;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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
    entry: Regex,
    item: Regex,
    media: Regex,
    uuid: Regex,
}

/// Contexts in which a UUID is one of the app's own random job ids.
const JOB_ID_CONTEXTS: [&str; 2] = ["job=", "job_id="];

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
        let home = home.trim_end_matches(['/', '\\']);
        if home.len() > 3 {
            let back = home.replace('/', "\\");
            for h in [
                home.to_string(),
                home.replace('\\', "/"),
                back.clone(),
                // `\\?\C:\Users\...` long-path form.
                format!(r"\\?\{back}"),
            ] {
                // The same path as JSON and `{:?}` write it: every backslash doubled.
                homes.push(h.replace('\\', "\\\\"));
                homes.push(h);
            }
        }
        homes.sort_by_key(|h| std::cmp::Reverse(h.len()));
        homes.dedup();
        Redactor {
            homes,
            url: Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s"'<>)]*"#).expect("url regex"),
            query: Regex::new(r#"\?[^\s"'<>?]*="#).expect("query regex"),
            // Entries inside an export: `memories/...`, `chat_media/...`, `json/...`, `html/...`.
            entry: Regex::new(r#"(?i)\b(?:memories|chat_media|json|html|media)/[^\s"'<>]+"#)
                .expect("entry regex"),
            // mr-job item keys and Snapchat file names: the capture date, `_`, the item id (any case).
            item: Regex::new(r"\b\d{4}-\d{2}-\d{2}_[A-Za-z0-9][A-Za-z0-9_-]*").expect("item regex"),
            // Any other media file name: it is the name of a memory.
            media: Regex::new(
                r#"(?i)[^\s"'<>/\\|:*?\[\]]+\.(?:jpe?g|png|gif|webp|heic|heif|mp4|mov|m4v|avi|3gp)\b"#,
            )
            .expect("media regex"),
            uuid: Regex::new(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
                .expect("uuid regex"),
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
        let out = self.entry.replace_all(&out, "[entry]");
        let out = self.item.replace_all(&out, "[item]");
        let out = self.media.replace_all(&out, "[entry]");
        let mut kept = String::with_capacity(out.len());
        let mut last = 0;
        for m in self.uuid.find_iter(&out) {
            kept.push_str(&out[last..m.start()]);
            let before = &out[..m.start()];
            if JOB_ID_CONTEXTS.iter().any(|c| before.ends_with(c)) {
                kept.push_str(m.as_str());
            } else {
                kept.push_str("[id]");
            }
            last = m.end();
        }
        kept.push_str(&out[last..]);
        kept
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

/// Info and above, except `little_exif`, which reports "No EXIF data found"
/// as an error for every photo that simply has none yet (it is about to be
/// written): noise that would mislead whoever reads a support log.
pub fn filter() -> Targets {
    Targets::new()
        .with_default(LevelFilter::INFO)
        .with_target("little_exif", LevelFilter::OFF)
}

/// Logs a panic (message, location, thread) through `tracing`, so it is
/// redacted and written like every other line, then runs the previous hook.
/// The release profile may abort right after the hook; the event is written
/// synchronously before that.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log_panic(info);
        previous(info);
    }));
}

fn log_panic(info: &std::panic::PanicHookInfo<'_>) {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string());
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "unknown".to_string());
    let thread = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    tracing::error!(target: "panic", location = %location, thread = %thread, "panic: {message}");
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
        .with_filter(filter());
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
        assert_eq!(mac.redact("/Users/adam/Movies/a.mp4"), "~/Movies/[entry]");
        assert_eq!(r.redact(r"\\?\C:\Users\Adam\x"), r"~\x");
        assert_eq!(
            r.redact("clip group 2019-07-14_CCB7781A-1F90-4612-BDFC-FDBCDCA2614E done; file 75103A0C-87F2-4B73-ABF7-A82A20C6D024-main.mp4; job da8fa300-7fe6-4f89-9a73-9e8260872ebf"),
            "clip group [item] done; file [entry]; job [id]"
        );
    }

    #[test]
    fn every_spelling_of_the_home_folder_is_redacted() {
        let r = Redactor::new(r"C:\Users\Adam Smith");
        for text in [
            r"C:\Users\Adam Smith\Pictures",
            r"c:\users\adam smith\pictures",
            "C:/Users/Adam Smith/Pictures",
            r"\\?\C:\Users\Adam Smith\Pictures",
            // As serde_json and `{:?}` write it.
            r#"{"destination":"C:\\Users\\Adam Smith\\Pictures"}"#,
            r#"Some("\\\\?\\C:\\Users\\Adam Smith\\AppData")"#,
        ] {
            let out = r.redact(text);
            assert!(!out.to_lowercase().contains("adam"), "{text} -> {out}");
            assert!(out.contains('~'), "{text} -> {out}");
        }
    }

    #[test]
    fn item_keys_entry_names_and_ids_are_masked_but_job_ids_stay() {
        let r = Redactor::new("/home/sam");
        let cases = [
            (
                "item=2019-07-14_ccb7781a-1f90-4612-bdfc-fdbcdca2614e done",
                "item=[item] done",
            ),
            ("item=2017-05-23_AbC123xyz failed", "item=[item] failed"),
            (
                "read memories/2019-07-14_CCB7781A-main.mp4 ok",
                "read [entry] ok",
            ),
            ("entry json/memories_history.json", "entry [entry]"),
            (
                "wrote 2019-07-14_235000_CCB7781A-joined.mp4",
                "wrote [item].mp4",
            ),
            (
                "unrecognised IMG_1234.JPG and Snapchat-1234.mov",
                "unrecognised [entry] and [entry]",
            ),
            ("file 75103a0c-87f2-4b73-abf7-a82a20c6d024", "file [id]"),
            (
                "job=da8fa300-7fe6-4f89-9a73-9e8260872ebf job_id=0f6e1c2a-1b2c-4d5e-8f90-123456789abc",
                "job=da8fa300-7fe6-4f89-9a73-9e8260872ebf job_id=0f6e1c2a-1b2c-4d5e-8f90-123456789abc",
            ),
            (
                "2026-10-16T10:45:11.176309Z INFO report.html written",
                "2026-10-16T10:45:11.176309Z INFO report.html written",
            ),
        ];
        for (text, want) in cases {
            assert_eq!(r.redact(text), want, "{text}");
        }
    }

    #[test]
    fn a_panic_is_logged_with_its_location_and_redacted() {
        let dir = std::env::temp_dir().join(format!("mr-log-panic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let log = SharedLog::new(
            RotatingFile::open(&dir, MAX_FILE, FILES).unwrap(),
            Redactor::new("/home/sam"),
        );
        install_panic_hook();
        let writer = log.clone();
        let joined = std::thread::Builder::new()
            .name("job-test".into())
            .spawn(move || {
                let sub = tracing_subscriber::registry().with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(writer)
                        .with_ansi(false),
                );
                tracing::subscriber::with_default(sub, || {
                    panic!("could not read /home/sam/x.zip");
                });
            })
            .unwrap()
            .join();
        assert!(joined.is_err());
        let text = fs::read_to_string(dir.join(BASE)).unwrap();
        assert!(text.contains("panic: could not read ~/x.zip"), "{text}");
        assert!(text.contains("logging.rs:"), "{text}");
        assert!(text.contains("thread=job-test"), "{text}");
        assert!(!text.contains("/home/sam"), "{text}");
        let _ = fs::remove_dir_all(&dir);
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
