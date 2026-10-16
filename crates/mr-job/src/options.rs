//! Job options (SRS FR-48 and the per-job settings of FR-25, FR-26, FR-27,
//! FR-31, FR-44, section 3.6 step 4).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What to do with overlays (FR-26).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayMode {
    /// Burn the overlay in and keep the untouched main file and overlay under
    /// `originals/` (the default).
    #[default]
    BurnInKeepOriginals,
    /// Burn the overlay in; keep nothing else.
    BurnInOnly,
    /// Do not burn in; save the overlay PNG next to the output.
    SaveSeparate,
    /// Drop overlays.
    Ignore,
}

impl OverlayMode {
    pub fn burns_in(self) -> bool {
        matches!(
            self,
            OverlayMode::BurnInKeepOriginals | OverlayMode::BurnInOnly
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OverlayMode::BurnInKeepOriginals => "burn_in_keep_originals",
            OverlayMode::BurnInOnly => "burn_in_only",
            OverlayMode::SaveSeparate => "save_separate",
            OverlayMode::Ignore => "ignore",
        }
    }
}

/// Clip rejoin settings (FR-27). M2 records them; detection and joining
/// arrive with the media crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipRejoin {
    pub enabled: bool,
    /// Inclusive year range of candidate clips; `None` means all years.
    pub year_range: Option<(i32, i32)>,
}

impl Default for ClipRejoin {
    fn default() -> Self {
        ClipRejoin {
            enabled: false,
            year_range: Some((2017, 2021)),
        }
    }
}

/// Hooks for the test harness (SRS section 11.1 "fault injection"). Honoured
/// in debug builds only; release builds ignore them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TestHooks {
    /// Item keys or file IDs that fail with `TEST_INJECTED` on every attempt
    /// while the hook is set (FR-39 AC1: inject, then remove the fault).
    pub fail_ids: Vec<String>,
    /// Extra milliseconds spent per item, to make interruption tests meaningful.
    pub delay_ms: u64,
    /// Pretend the destination volume has this many free bytes (FR-28).
    pub fake_free_bytes: Option<u64>,
    /// Re-check free space every N items instead of 500 (FR-28 AC2).
    pub recheck_every: Option<usize>,
    /// After N completed items, report the destination as missing for
    /// `dest_offline_secs` seconds (DEST_UNAVAILABLE, NFR-REL-06).
    pub dest_offline_after: Option<usize>,
    pub dest_offline_secs: u64,
}

impl TestHooks {
    pub fn enabled(&self) -> bool {
        cfg!(debug_assertions)
    }

    pub fn fails(&self, key: &str, file_id: &str) -> bool {
        self.enabled()
            && self
                .fail_ids
                .iter()
                .any(|f| f.eq_ignore_ascii_case(key) || f.eq_ignore_ascii_case(file_id))
    }

    pub fn delay(&self) -> Option<std::time::Duration> {
        (self.enabled() && self.delay_ms > 0)
            .then(|| std::time::Duration::from_millis(self.delay_ms))
    }
}

/// Everything a job needs to know besides its inputs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Options {
    /// Destination folder chosen by the user. Output goes to
    /// `<dest>/Memories Rescue/` (section 3.7).
    pub dest: PathBuf,
    pub overlay_mode: OverlayMode,
    /// Also save the untouched main file (with metadata fixed) and overlay
    /// under `originals/` for composited items (FR-25). Default on.
    pub keep_originals: bool,
    /// IANA zone used when an item has no GPS (FR-19). `None`: system zone.
    pub home_tz: Option<String>,
    /// Worker threads; `None` picks `min(cores - 1, 4)` (section 4.2).
    pub workers: Option<usize>,
    /// Output file name template (section 3.7). Placeholders: `{date}`,
    /// `{time}`, `{shortid}`, `{id}`, `{ext}`.
    pub output_template: String,
    /// Licensed. When `false` the first 100 items in chronological order are
    /// processed and the rest are `skipped_free_limit` (FR-44).
    pub unlocked: bool,
    /// "Apply uncertain locations" (section 3.6 step 4). Default off.
    pub apply_uncertain_locations: bool,
    /// "Maximise Windows compatibility": H.264 output when compositing (FR-51).
    pub windows_compat: bool,
    pub clip_rejoin: ClipRejoin,
    /// ZIP output (FR-31). Placeholder: recorded, not implemented in M2.
    pub zip_output: bool,
    /// Skip the disk-space precheck (FR-28) when the user chose to continue.
    pub ignore_space_check: bool,
    #[serde(default)]
    pub test_hooks: TestHooks,
}

impl Options {
    pub fn new(dest: impl Into<PathBuf>) -> Options {
        Options {
            dest: dest.into(),
            overlay_mode: OverlayMode::default(),
            keep_originals: true,
            home_tz: None,
            workers: None,
            output_template: DEFAULT_TEMPLATE.to_string(),
            unlocked: false,
            apply_uncertain_locations: false,
            windows_compat: false,
            clip_rejoin: ClipRejoin::default(),
            zip_output: false,
            ignore_space_check: false,
            test_hooks: TestHooks::default(),
        }
    }

    /// Photo workers per section 4.2: `min(physical cores - 1, 4)`, at least 1.
    pub fn worker_count(&self) -> usize {
        self.workers.unwrap_or_else(default_workers).max(1)
    }
}

pub const DEFAULT_TEMPLATE: &str = "{date}_{time}_{shortid}.{ext}";

/// Items processed without a licence (FR-44).
pub const FREE_TIER_ITEMS: usize = 100;

pub fn default_workers() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2);
    cores.saturating_sub(1).clamp(1, 4)
}
