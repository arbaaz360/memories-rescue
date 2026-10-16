//! `app_info`, `diagnostics_preview` and `export_diagnostics` (FR-41,
//! NFR-PRV-06). There is no telemetry in v1.0 (Q-12): nothing here is sent
//! anywhere. Export diagnostics saves a ZIP on the user's computer, for them
//! to attach to an email to support, and the window shows its contents first.
//!
//! The ZIP holds the rotating logs (already redacted on the way to disk:
//! home folder as `~`, no URLs or query strings), app facts, the settings
//! (the licence key is never in them), per-job summaries and journals. No
//! media, no JSON content, no file names from the export: an item's key holds
//! its capture date and the export's file ID, so journals number the items
//! instead (`item-1`, `clip-1`, in the order they first appear) and keep only
//! each warning's code.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;

use crate::ipc::{AppInfo, DiagnosticsFile, DiagnosticsPreview, IpcError, IpcResult};
use crate::links::LINKS;

use super::{Engine, file_name, io_error, now_utc, os_family, redact_home};

/// Journals of at most this many recent jobs.
const JOURNALS: usize = 5;

enum Content {
    Text(String),
    File(PathBuf),
}

struct Entry {
    name: String,
    kind: &'static str,
    content: Content,
}

impl Entry {
    fn bytes(&self) -> u64 {
        match &self.content {
            Content::Text(t) => t.len() as u64,
            Content::File(p) => std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
        }
    }
}

/// Values are redacted before they are serialised: once serde_json has
/// escaped a Windows path (`C:\\Users\\...`), text-level redaction would
/// have to know every escaped spelling.
fn text(name: impl Into<String>, kind: &'static str, value: &impl serde::Serialize) -> Entry {
    let mut value = serde_json::to_value(value).unwrap_or_default();
    redact_json(&mut value);
    let json = serde_json::to_string_pretty(&value).unwrap_or_default();
    Entry {
        name: name.into(),
        kind,
        content: Content::Text(json),
    }
}

/// Every string value through the log's redactor. Object keys are the app's
/// own field names; a job's `id` stays, because it is random and names its
/// journal file.
pub fn redact_json(value: &mut serde_json::Value) {
    use serde_json::Value;
    match value {
        Value::String(s) => *s = redact_home(s),
        Value::Array(items) => items.iter_mut().for_each(redact_json),
        Value::Object(fields) => {
            for (key, v) in fields.iter_mut() {
                if key != "id" {
                    redact_json(v);
                }
            }
        }
        _ => {}
    }
}

/// A job's journal with item keys replaced by numbers and warning details dropped.
pub fn numbered_journal(events: &[mr_job::JournalEvent]) -> Vec<serde_json::Value> {
    let mut names: HashMap<&str, String> = HashMap::new();
    let (mut items, mut clips) = (0u32, 0u32);
    let mut out = Vec::with_capacity(events.len());
    for e in events {
        let name = names
            .entry(e.item.as_str())
            .or_insert_with(|| {
                if e.item.starts_with("clip:") {
                    clips += 1;
                    format!("clip-{clips}")
                } else {
                    items += 1;
                    format!("item-{items}")
                }
            })
            .clone();
        let event = e
            .event
            .split_once('|')
            .map_or(e.event.as_str(), |(code, _)| code);
        out.push(serde_json::json!({
            "seq": e.seq,
            "item": name,
            "event": event,
            "at_utc": e.at_utc,
        }));
    }
    out
}

impl Engine {
    pub fn app_info(&self) -> AppInfo {
        let res = self.paths.resource_dir.clone().unwrap_or_default();
        let notices = PathBuf::from(super::plain_path(
            &res.join("THIRD_PARTY_NOTICES.md").to_string_lossy(),
        ));
        AppInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            os_family: os_family().to_string(),
            // The app's own licence is not chosen yet (LICENSE-TODO.md); the notices cover the bundled parts.
            licenses_path: notices.to_string_lossy().into_owned(),
            third_party_notices_path: notices.to_string_lossy().into_owned(),
            ffmpeg_source_url: LINKS.ffmpeg_source.clone(),
            privacy_policy_url: LINKS.privacy.clone(),
            support_email: LINKS.support_email.clone(),
            website_url: LINKS.website.clone(),
            terms_url: LINKS.terms.clone(),
            faq_url: LINKS.faq.clone(),
        }
    }

    fn diagnostics_destination(&self) -> PathBuf {
        let day = now_utc()[..10].to_string();
        let mut dir = self.paths.desktop_dir.clone();
        if !dir.is_dir() {
            dir = self.paths.state_dir.clone();
        }
        dir.join(format!("memories-rescue-diagnostics-{day}.zip"))
    }

    fn diagnostics_entries(&self) -> Vec<Entry> {
        let mut out: Vec<Entry> = crate::logging::RotatingFile::files_in(&self.paths.log_dir)
            .into_iter()
            .map(|p| Entry {
                name: format!("logs/{}", file_name(&p)),
                kind: "logs",
                content: Content::File(p),
            })
            .collect();
        let info = serde_json::json!({
            "app": self.app_info(),
            "generated_utc": now_utc(),
            "licence_tier": self.license.status().tier,
            "compositor": self.compositor.name(),
            "encoder": self.media.as_ref().map(|m| match m.encoder_plan() {
                Ok(p) => format!("{p:?}"),
                Err(e) => e.code,
            }),
        });
        out.push(text("app.json", "app", &info));
        out.push(text("settings.json", "settings", &self.get_settings()));
        let jobs = mr_job::list_jobs(self.state_dir());
        let summaries: Vec<serde_json::Value> = jobs
            .iter()
            .map(|j| {
                serde_json::json!({
                    "id": j.id,
                    "created_utc": j.created_utc,
                    "status": j.status,
                    "profile": j.profile,
                    "parts": j.inputs.len(),
                    "counts": j.counts,
                })
            })
            .collect();
        out.push(text("jobs.json", "jobs", &summaries));
        for j in jobs.iter().take(JOURNALS) {
            if let Ok(job) = mr_job::Job::open(self.state_dir(), &j.id) {
                let events = numbered_journal(&job.journal().unwrap_or_default());
                out.push(text(
                    format!("journal-{}.json", &j.id[..8.min(j.id.len())]),
                    "journal",
                    &events,
                ));
            }
        }
        out
    }

    /// What [`Engine::export_diagnostics`] would save, and where (shown first).
    pub fn diagnostics_preview(&self) -> DiagnosticsPreview {
        DiagnosticsPreview {
            destination: self
                .diagnostics_destination()
                .to_string_lossy()
                .into_owned(),
            files: self
                .diagnostics_entries()
                .iter()
                .map(|e| DiagnosticsFile {
                    name: e.name.clone(),
                    bytes: e.bytes(),
                    kind: e.kind.to_string(),
                })
                .collect(),
        }
    }

    pub fn export_diagnostics(&self) -> IpcResult<String> {
        let path = self.diagnostics_destination();
        let tmp = path.with_extension("zip.tmp");
        let file = std::fs::File::create(&tmp).map_err(|e| io_error(&tmp, &e))?;
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let failed = |e: &dyn std::fmt::Display| {
            IpcError::unknown("Diagnostics could not be written.").with_detail(e.to_string())
        };
        for entry in self.diagnostics_entries() {
            zip.start_file(entry.name.as_str(), opts)
                .map_err(|e| failed(&e))?;
            match &entry.content {
                Content::Text(t) => zip
                    .write_all(t.as_bytes())
                    .map_err(|e| io_error(&tmp, &e))?,
                Content::File(p) => {
                    // The log may be rotating while it is copied: copy what is
                    // there, line by line through today's redactor, so lines an
                    // older version wrote are held to the current rules too.
                    if let Ok(f) = std::fs::File::open(p) {
                        let mut reader = std::io::BufReader::new(f);
                        let mut line = Vec::new();
                        while std::io::BufRead::read_until(&mut reader, b'\n', &mut line)
                            .map_err(|e| io_error(p, &e))?
                            > 0
                        {
                            let clean = redact_home(&String::from_utf8_lossy(&line));
                            zip.write_all(clean.as_bytes())
                                .map_err(|e| io_error(&tmp, &e))?;
                            line.clear();
                        }
                    }
                }
            }
        }
        zip.finish().map_err(|e| failed(&e))?;
        std::fs::rename(&tmp, &path).map_err(|e| io_error(&path, &e))?;
        self.issue_path(&path);
        tracing::info!("diagnostics saved");
        Ok(path.to_string_lossy().into_owned())
    }
}
