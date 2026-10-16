//! `app_info`, `telemetry_preview` and `export_diagnostics` (FR-41,
//! NFR-PRV-04, NFR-PRV-06). Diagnostics hold app facts, settings without the
//! licence key, and per-job counts and journal events with the home directory
//! replaced by `~`: no media, no JSON content, no file names from the export.

use std::collections::BTreeMap;
use std::io::Write;

use crate::ipc::{AppInfo, IpcError, IpcResult, TelemetryPayload};

use super::{Engine, io_error, now_utc, os_family, redact_home};

pub const SUPPORT_EMAIL: &str = "support@novabyteworks.com";

impl Engine {
    pub fn app_info(&self) -> AppInfo {
        let res = self.paths.resource_dir.clone().unwrap_or_default();
        let notices = res.join("THIRD_PARTY_NOTICES.md");
        AppInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            os_family: os_family().to_string(),
            // The app's own licence is not chosen yet (LICENSE-TODO.md); the notices cover the bundled parts.
            licenses_path: notices.to_string_lossy().into_owned(),
            third_party_notices_path: notices.to_string_lossy().into_owned(),
            ffmpeg_source_url: "https://novabyteworks.com/memories-rescue/ffmpeg-source"
                .to_string(),
            privacy_policy_url: "https://novabyteworks.com/memories-rescue/privacy".to_string(),
            support_email: SUPPORT_EMAIL.to_string(),
            website_url: "https://novabyteworks.com".to_string(),
        }
    }

    /// Exactly what an opted-in upload would contain (NFR-PRV-04).
    pub fn telemetry_preview(&self) -> TelemetryPayload {
        let jobs = mr_job::list_jobs(self.state_dir());
        let processed: usize = jobs
            .iter()
            .map(|j| {
                j.counts.get("done").copied().unwrap_or(0)
                    + j.counts.get("failed").copied().unwrap_or(0)
            })
            .sum();
        TelemetryPayload {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            os_family: os_family().to_string(),
            items_processed_bucket: bucket(processed).to_string(),
            error_code_counts: BTreeMap::new(),
            crash_signatures: Vec::new(),
        }
    }

    pub fn export_diagnostics(&self) -> IpcResult<String> {
        let day = now_utc()[..10].to_string();
        let mut dir = self.paths.desktop_dir.clone();
        if !dir.is_dir() {
            dir = self.paths.state_dir.clone();
        }
        let path = dir.join(format!("memories-rescue-diagnostics-{day}.zip"));
        let file = std::fs::File::create(&path).map_err(|e| io_error(&path, &e))?;
        let mut zip = zip::ZipWriter::new(file);
        let mut add = |name: &str, text: String| add_file(&mut zip, &path, name, &text);
        let info = serde_json::json!({
            "app": self.app_info(),
            "generated_utc": now_utc(),
            "licence_tier": self.license.status().tier,
        });
        add(
            "app.json",
            serde_json::to_string_pretty(&info).unwrap_or_default(),
        )?;
        add(
            "settings.json",
            serde_json::to_string_pretty(&self.get_settings()).unwrap_or_default(),
        )?;
        let jobs = mr_job::list_jobs(self.state_dir());
        let summaries: Vec<serde_json::Value> = jobs
            .iter()
            .map(|j| serde_json::json!({ "id": j.id, "created_utc": j.created_utc, "status": j.status, "profile": j.profile, "parts": j.inputs.len(), "counts": j.counts }))
            .collect();
        add(
            "jobs.json",
            serde_json::to_string_pretty(&summaries).unwrap_or_default(),
        )?;
        for j in jobs.iter().take(5) {
            if let Ok(job) = mr_job::Job::open(self.state_dir(), &j.id) {
                let events: Vec<serde_json::Value> = job
                    .journal()
                    .unwrap_or_default()
                    .iter()
                    .map(|e| serde_json::to_value(e).unwrap_or_default())
                    .collect();
                add(
                    &format!("journal-{}.json", &j.id[..8.min(j.id.len())]),
                    serde_json::to_string_pretty(&events).unwrap_or_default(),
                )?;
            }
        }
        zip.finish().map_err(|e| {
            IpcError::unknown("Diagnostics could not be written.").with_detail(e.to_string())
        })?;
        Ok(path.to_string_lossy().into_owned())
    }
}

fn add_file(
    zip: &mut zip::ZipWriter<std::fs::File>,
    path: &std::path::Path,
    name: &str,
    text: &str,
) -> IpcResult<()> {
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file(name, opts).map_err(|e| {
        IpcError::unknown("Diagnostics could not be written.").with_detail(e.to_string())
    })?;
    zip.write_all(redact_home(text).as_bytes())
        .map_err(|e| io_error(path, &e))
}

fn bucket(n: usize) -> &'static str {
    match n {
        0 => "0",
        1..=99 => "1-99",
        100..=999 => "100-999",
        1000..=9999 => "1k-10k",
        _ => "10k+",
    }
}
