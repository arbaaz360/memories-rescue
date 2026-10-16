//! The engine adapter: everything the window's commands need, on top of
//! `mr-job`, `mr-core` and `mr-report`. Independent of Tauri so it can be
//! tested on the fixtures without a window; `commands.rs` is the thin Tauri
//! layer. Events go out through [`EventSink`].

pub mod analysis;
pub mod diagnostics;
pub mod inputs;
pub mod jobs;
pub mod license;
pub mod settings;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::ipc::{ErrorCode, IpcError};

/// Where events go: the Tauri app handle in the app, a collector in tests.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: &str, payload: serde_json::Value);
}

/// Directories the engine works in, resolved by the caller.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Per-user app data: jobs, settings, licence cache (SRS 3.7).
    pub state_dir: PathBuf,
    /// Per-user cache: preview thumbnails.
    pub cache_dir: PathBuf,
    /// First default destination.
    pub pictures_dir: PathBuf,
    /// Where diagnostics ZIPs go.
    pub desktop_dir: PathBuf,
    /// Bundled resources (licence texts), when known.
    pub resource_dir: Option<PathBuf>,
}

pub struct Engine {
    pub paths: Paths,
    sink: Arc<dyn EventSink>,
    pub(crate) inputs: Mutex<inputs::Registry>,
    pub(crate) plans: Mutex<HashMap<String, analysis::CachedPlan>>,
    pub(crate) last_analysis: Mutex<Option<analysis::CachedPlan>>,
    pub(crate) jobs: Mutex<HashMap<String, Arc<jobs::JobHandle>>>,
    pub(crate) settings: Mutex<crate::ipc::Settings>,
    pub license: license::Licenser,
}

impl Engine {
    pub fn new(
        paths: Paths,
        sink: Arc<dyn EventSink>,
        secrets: Arc<dyn license::SecretStore>,
        gumroad: license::GumroadConfig,
    ) -> Arc<Engine> {
        let _ = std::fs::create_dir_all(&paths.state_dir);
        let settings = settings::load(&paths);
        let license = license::Licenser::new(paths.state_dir.clone(), secrets, gumroad);
        Arc::new(Engine {
            paths,
            sink,
            inputs: Mutex::new(inputs::Registry::default()),
            plans: Mutex::new(HashMap::new()),
            last_analysis: Mutex::new(None),
            jobs: Mutex::new(HashMap::new()),
            settings: Mutex::new(settings),
            license,
        })
    }

    pub fn emit<T: Serialize>(&self, event: &str, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(v) => self.sink.emit(event, v),
            Err(e) => eprintln!("event {event} not serialisable: {e}"),
        }
    }

    /// The state directory's `jobs` live here; mr-job adds `jobs/<id>`.
    pub fn state_dir(&self) -> &Path {
        &self.paths.state_dir
    }
}

/// Maps mr-job errors onto the contract.
pub fn job_error(e: mr_job::JobError) -> IpcError {
    use mr_job::JobError as E;
    match e {
        E::NoExport(why) => IpcError::new(
            ErrorCode::InputNotExport,
            "None of these files is a Snapchat export.",
        )
        .with_detail(why),
        E::MultipleExports(n) => IpcError::new(
            ErrorCode::InputNotExport,
            format!("These files hold {n} different exports. Start one job per export."),
        )
        .with_detail(format!("{n} exports")),
        E::TimeZone(name) => IpcError::unknown("Unknown home time zone.").with_detail(name),
        E::NoSuchJob(id) => IpcError::unknown("That job no longer exists.").with_detail(id),
        E::OutputInUse { job_id } => {
            IpcError::unknown("This folder already holds a Memories Rescue job.")
                .with_detail(job_id)
        }
        E::Busy { job_id } => IpcError::unknown("This job is already running.").with_detail(job_id),
        E::Io { path, source } => io_error(&path, &source),
        E::Db(e) => IpcError::unknown("The job's saved progress could not be read.")
            .with_detail(e.to_string()),
        E::State(s) => {
            IpcError::unknown("The job's saved progress could not be read.").with_detail(s)
        }
    }
}

pub fn io_error(path: &Path, e: &std::io::Error) -> IpcError {
    use std::io::ErrorKind as K;
    let detail = format!("{}: {e}", redact_home(&path.to_string_lossy()));
    match e.kind() {
        K::PermissionDenied | K::ReadOnlyFilesystem => IpcError::new(
            ErrorCode::PermissionDenied,
            "Memories Rescue can't write to this folder.",
        )
        .with_detail(detail),
        K::StorageFull | K::QuotaExceeded => {
            IpcError::new(ErrorCode::DiskFull, "Not enough space on the destination.")
                .with_detail(detail)
        }
        K::NotFound => IpcError::new(
            ErrorCode::PartMissing,
            "A file or folder could not be found.",
        )
        .with_detail(detail),
        _ => IpcError::unknown("A file could not be read or written.").with_detail(detail),
    }
}

/// Home directory replaced by `~` (FR-41), for anything that may leave the machine.
pub fn redact_home(text: &str) -> String {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    if home.len() > 3 {
        text.replace(&home, "~")
    } else {
        text.to_string()
    }
}

pub fn now_utc() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

pub fn os_family() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

/// A sink that keeps everything, for tests.
#[derive(Default)]
pub struct CollectingSink {
    pub events: Mutex<Vec<(String, serde_json::Value)>>,
}

impl EventSink for CollectingSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        self.events
            .lock()
            .expect("events")
            .push((event.to_string(), payload));
    }
}

impl CollectingSink {
    pub fn of(&self, event: &str) -> Vec<serde_json::Value> {
        self.events
            .lock()
            .expect("events")
            .iter()
            .filter(|(e, _)| e == event)
            .map(|(_, v)| v.clone())
            .collect()
    }
}
