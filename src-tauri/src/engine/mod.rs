//! The engine adapter: everything the window's commands need, on top of
//! `mr-job`, `mr-core` and `mr-report`. Independent of Tauri so it can be
//! tested on the fixtures without a window; `commands.rs` is the thin Tauri
//! layer. Events go out through [`EventSink`].
//!
//! Media work goes through mr-job's [`mr_job::Compositor`] seam: the real
//! [`mr_job::MediaCompositor`] on the bundled ffmpeg sidecar when
//! [`Paths::ffmpeg`] names one, else mr-job's pass-through (overlays saved
//! beside the files), which is what the fast tests use.

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
    /// FR-41: the rotating log (`logging.rs`); Export diagnostics reads it.
    pub log_dir: PathBuf,
    /// The bundled ffmpeg sidecar. `None`, or an empty placeholder file,
    /// keeps mr-job's pass-through compositor.
    pub ffmpeg: Option<PathBuf>,
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
    /// The real compositor, when the sidecar is there.
    pub(crate) media: Option<Arc<mr_job::MediaCompositor>>,
    /// What jobs and plans use: `media`, or the pass-through.
    pub(crate) compositor: Arc<dyn mr_job::Compositor>,
}

/// Q-04: Microsoft's software H.264 encoder (`h264_mf`, no GPU first) with
/// 1.3 times the source bitrate, which it needs to match OpenH264's quality.
pub const MF_SOFTWARE_BITRATE_FACTOR: f64 = 1.3;

/// The compositor the app runs, on the given ffmpeg.
pub fn compositor_for(ffmpeg: &Path) -> mr_job::MediaCompositor {
    let video = mr_composite::VideoOptions {
        prefer_hardware: false,
        mf_software_bitrate_factor: MF_SOFTWARE_BITRATE_FACTOR,
        ..mr_composite::VideoOptions::default()
    };
    mr_job::MediaCompositor::with(mr_composite::Compositor::with_options(ffmpeg, video))
}

/// Debug builds only: `MR_TEST_HOOKS` as mr-job's [`mr_job::TestHooks`] JSON
/// (`{"delay_ms":300,"fail_ids":["..."],"force_hevc_unplayable":true}`), so
/// the walk-through of the real app can pause mid-run, fail an item or show
/// the HEVC notice. Release builds never read it (and mr-job ignores hooks).
pub fn test_hooks() -> mr_job::TestHooks {
    if !cfg!(debug_assertions) {
        return mr_job::TestHooks::default();
    }
    std::env::var("MR_TEST_HOOKS")
        .ok()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
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
        let media = paths
            .ffmpeg
            .as_deref()
            .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() > 0))
            .map(|p| Arc::new(compositor_for(p)));
        let compositor: Arc<dyn mr_job::Compositor> = match &media {
            Some(m) => m.clone(),
            None => Arc::new(mr_job::PassThrough),
        };
        tracing::info!(
            compositor = compositor.name(),
            "engine ready (bitrate factor {MF_SOFTWARE_BITRATE_FACTOR})"
        );
        Arc::new(Engine {
            paths,
            sink,
            inputs: Mutex::new(inputs::Registry::default()),
            plans: Mutex::new(HashMap::new()),
            last_analysis: Mutex::new(None),
            jobs: Mutex::new(HashMap::new()),
            settings: Mutex::new(settings),
            license,
            media,
            compositor,
        })
    }

    /// Captions can be burned in (a real compositor is set).
    pub fn burns_in(&self) -> bool {
        self.media.is_some()
    }

    pub fn emit<T: Serialize>(&self, event: &str, payload: &T) {
        match serde_json::to_value(payload) {
            Ok(v) => self.sink.emit(event, v),
            Err(e) => tracing::error!("event {event} not serialisable: {e}"),
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

/// Home directory replaced by `~`, web addresses and query strings removed
/// (FR-41), for anything that may leave the machine: the same rule as the log.
pub fn redact_home(text: &str) -> String {
    crate::logging::redact(text)
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
