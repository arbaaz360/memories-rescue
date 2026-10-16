//! Settings (FR-48) stored as JSON in the state directory, and the mapping
//! between the contract's options and `mr_job::Options`.

use std::path::Path;

use crate::ipc::{
    IpcError, IpcResult, JobOptions, OverlayMode, Settings, UpdateChannel, YearRange,
};

use super::{Engine, Paths, io_error};

const FILE: &str = "settings.json";

pub fn defaults(paths: &Paths) -> Settings {
    Settings {
        destination: paths.pictures_dir.to_string_lossy().into_owned(),
        home_timezone: system_zone(),
        overlay_mode: OverlayMode::BurnInKeepOriginals,
        keep_originals: true,
        clip_rejoin: true,
        clip_rejoin_years: Some(YearRange {
            from: 2017,
            to: 2021,
        }),
        apply_uncertain_locations: false,
        windows_compatibility: false,
        workers: None,
        output_template: mr_job::options::DEFAULT_TEMPLATE.to_string(),
        zip_output: false,
        zip_volume_bytes: None,
        update_checks: true,
        update_channel: UpdateChannel::Stable,
        telemetry: false,
        language: "system".to_string(),
    }
}

pub fn system_zone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

/// Reads the stored settings; a missing or unreadable file gives the defaults.
pub fn load(paths: &Paths) -> Settings {
    let path = paths.state_dir.join(FILE);
    let stored = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Settings>(&s).ok());
    let mut s = stored.unwrap_or_else(|| defaults(paths));
    if s.destination.trim().is_empty() {
        s.destination = paths.pictures_dir.to_string_lossy().into_owned();
    }
    if s.home_timezone.trim().is_empty() {
        s.home_timezone = system_zone();
    }
    s
}

pub fn save(state_dir: &Path, s: &Settings) -> IpcResult<()> {
    std::fs::create_dir_all(state_dir).map_err(|e| io_error(state_dir, &e))?;
    let path = state_dir.join(FILE);
    let tmp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(s).map_err(|e| {
        IpcError::unknown("Settings could not be saved.").with_detail(e.to_string())
    })?;
    std::fs::write(&tmp, text).map_err(|e| io_error(&tmp, &e))?;
    std::fs::rename(&tmp, &path).map_err(|e| io_error(&path, &e))
}

impl Engine {
    pub fn get_settings(&self) -> Settings {
        self.settings.lock().expect("settings").clone()
    }

    pub fn set_settings(&self, s: Settings) -> IpcResult<Settings> {
        if let Some(problem) = template_problem(&s.output_template) {
            return Err(
                IpcError::unknown("The file name pattern is not valid.").with_detail(problem)
            );
        }
        save(self.state_dir(), &s)?;
        *self.settings.lock().expect("settings") = s.clone();
        Ok(s)
    }

    /// Remembers the last destination (keeps the default flow to five steps).
    pub(crate) fn remember_destination(&self, dest: &str) {
        let mut s = self.settings.lock().expect("settings");
        if s.destination != dest {
            s.destination = dest.to_string();
            let _ = save(&self.paths.state_dir, &s);
        }
    }
}

/// Same rule as the Settings screen: mr-job's tokens only, an id and the extension.
pub fn template_problem(t: &str) -> Option<&'static str> {
    const TOKENS: [&str; 5] = ["{date}", "{time}", "{shortid}", "{id}", "{ext}"];
    if t.trim().is_empty() {
        return Some("empty");
    }
    let mut rest = t.to_string();
    for tok in TOKENS {
        rest = rest.replace(tok, "");
    }
    if rest.contains('{') || rest.contains('}') {
        return Some("token");
    }
    if rest.chars().any(|c| "<>:\"/\\|?*".contains(c)) {
        return Some("chars");
    }
    if !t.contains("{shortid}") && !t.contains("{id}") {
        return Some("id");
    }
    if !t.contains("{ext}") {
        return Some("ext");
    }
    None
}

/// The contract's job options plus the stored template, as mr-job options.
/// `metadata_only` (FR-21) keeps captions as separate files and skips rejoining.
pub fn to_engine(o: &JobOptions, template: &str, unlocked: bool) -> mr_job::Options {
    let mut e = mr_job::Options::new(&o.destination);
    e.overlay_mode = match o.overlay_mode {
        OverlayMode::BurnInKeepOriginals => mr_job::OverlayMode::BurnInKeepOriginals,
        OverlayMode::BurnInOnly => mr_job::OverlayMode::BurnInOnly,
        OverlayMode::SeparateFile => mr_job::OverlayMode::SaveSeparate,
        OverlayMode::Ignore => mr_job::OverlayMode::Ignore,
    };
    e.keep_originals = o.keep_originals;
    e.home_tz = (!o.home_timezone.trim().is_empty()).then(|| o.home_timezone.clone());
    e.workers = o.workers.map(|w| w.max(1) as usize);
    e.output_template = if template.trim().is_empty() {
        mr_job::options::DEFAULT_TEMPLATE.to_string()
    } else {
        template.to_string()
    };
    e.unlocked = unlocked;
    e.apply_uncertain_locations = o.apply_uncertain_locations;
    e.windows_compat = o.windows_compatibility;
    e.clip_rejoin = mr_job::ClipRejoin {
        enabled: o.clip_rejoin,
        year_range: o.clip_rejoin_years.map(|y| (y.from, y.to)),
        skip_groups: o.excluded_clip_groups.clone(),
    };
    e.zip_output = o.zip_output;
    if o.metadata_only {
        if e.overlay_mode.burns_in() {
            e.overlay_mode = mr_job::OverlayMode::SaveSeparate;
        }
        e.keep_originals = false;
        e.clip_rejoin.enabled = false;
    }
    e
}

/// mr-job options back as the contract's job options (for reports).
pub fn from_engine(e: &mr_job::Options) -> JobOptions {
    JobOptions {
        destination: e.dest.to_string_lossy().into_owned(),
        overlay_mode: match e.overlay_mode {
            mr_job::OverlayMode::BurnInKeepOriginals => OverlayMode::BurnInKeepOriginals,
            mr_job::OverlayMode::BurnInOnly => OverlayMode::BurnInOnly,
            mr_job::OverlayMode::SaveSeparate => OverlayMode::SeparateFile,
            mr_job::OverlayMode::Ignore => OverlayMode::Ignore,
        },
        keep_originals: e.keep_originals,
        clip_rejoin: e.clip_rejoin.enabled,
        clip_rejoin_years: e
            .clip_rejoin
            .year_range
            .map(|(from, to)| YearRange { from, to }),
        apply_uncertain_locations: e.apply_uncertain_locations,
        windows_compatibility: e.windows_compat,
        zip_output: e.zip_output,
        zip_volume_bytes: None,
        home_timezone: e.home_tz.clone().unwrap_or_default(),
        metadata_only: false,
        workers: e.workers.map(|w| w as u32),
        excluded_clip_groups: Vec::new(),
    }
}

/// The settings as the default options of a new job.
pub fn options_from_settings(s: &Settings) -> JobOptions {
    JobOptions {
        destination: s.destination.clone(),
        overlay_mode: s.overlay_mode,
        keep_originals: s.keep_originals,
        clip_rejoin: s.clip_rejoin,
        clip_rejoin_years: s.clip_rejoin_years,
        apply_uncertain_locations: s.apply_uncertain_locations,
        windows_compatibility: s.windows_compatibility,
        zip_output: s.zip_output,
        zip_volume_bytes: s.zip_volume_bytes,
        home_timezone: s.home_timezone.clone(),
        metadata_only: false,
        workers: s.workers,
        excluded_clip_groups: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_rule_matches_the_settings_screen() {
        assert_eq!(template_problem("{date}_{time}_{shortid}.{ext}"), None);
        assert_eq!(template_problem(" "), Some("empty"));
        assert_eq!(
            template_problem("{YYYY-MM-DD}_{shortid}.{ext}"),
            Some("token")
        );
        assert_eq!(template_problem("{date}/{shortid}.{ext}"), Some("chars"));
        assert_eq!(template_problem("{date}_{time}.{ext}"), Some("id"));
        assert_eq!(template_problem("{date}_{id}"), Some("ext"));
    }

    #[test]
    fn options_round_trip_and_metadata_only() {
        let s = Settings {
            destination: "D:\\Memories".into(),
            home_timezone: "Europe/London".into(),
            overlay_mode: OverlayMode::SeparateFile,
            keep_originals: true,
            clip_rejoin: true,
            clip_rejoin_years: None,
            apply_uncertain_locations: true,
            windows_compatibility: true,
            workers: Some(3),
            output_template: "{date}_{shortid}.{ext}".into(),
            zip_output: true,
            zip_volume_bytes: None,
            update_checks: true,
            update_channel: UpdateChannel::Beta,
            telemetry: false,
            language: "system".into(),
        };
        let o = options_from_settings(&s);
        let e = to_engine(&o, &s.output_template, true);
        assert_eq!(e.overlay_mode, mr_job::OverlayMode::SaveSeparate);
        assert_eq!(e.home_tz.as_deref(), Some("Europe/London"));
        assert_eq!(e.workers, Some(3));
        assert_eq!(e.output_template, "{date}_{shortid}.{ext}");
        assert!(e.unlocked && e.apply_uncertain_locations && e.windows_compat && e.zip_output);
        assert_eq!(e.clip_rejoin.year_range, None);
        assert_eq!(from_engine(&e), o);

        let meta = JobOptions {
            metadata_only: true,
            overlay_mode: OverlayMode::BurnInKeepOriginals,
            ..o
        };
        let e = to_engine(&meta, "", false);
        assert_eq!(e.overlay_mode, mr_job::OverlayMode::SaveSeparate);
        assert!(!e.keep_originals && !e.clip_rejoin.enabled && !e.unlocked);
        assert_eq!(e.output_template, mr_job::options::DEFAULT_TEMPLATE);
    }
}
