//! Tauri shell for Memories Rescue.
//!
//! The window talks to the engine through the commands and events of
//! `docs/ipc-contract.md`: `commands.rs` is the thin Tauri layer, `engine/`
//! the adapter onto `mr-job` (testable without a window), `ipc.rs` the
//! contract's types. ffmpeg and exiftool ship as sidecars and run from Rust
//! only. The window uses the dialog, opener and notification plugins exactly
//! as `capabilities/default.json` allows.

pub mod commands;
pub mod engine;
pub mod ipc;

use std::sync::Arc;

use tauri::{Emitter, Manager};

use engine::{Engine, EventSink, Paths, license};

/// Engine events out to the window.
struct WindowSink(tauri::AppHandle);

impl EventSink for WindowSink {
    fn emit(&self, event: &str, payload: serde_json::Value) {
        let _ = self.0.emit(event, payload);
    }
}

fn paths(app: &tauri::AppHandle) -> Paths {
    let p = app.path();
    let home = p.home_dir().unwrap_or_default();
    Paths {
        state_dir: p
            .app_data_dir()
            .unwrap_or_else(|_| home.join(".memories-rescue")),
        cache_dir: p
            .app_cache_dir()
            .unwrap_or_else(|_| std::env::temp_dir().join("memories-rescue")),
        pictures_dir: p.picture_dir().unwrap_or_else(|_| home.clone()),
        desktop_dir: p
            .desktop_dir()
            .or_else(|_| p.download_dir())
            .unwrap_or(home),
        resource_dir: p.resource_dir().ok(),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let engine = Engine::new(
                paths(&handle),
                Arc::new(WindowSink(handle.clone())),
                Arc::new(license::Keychain::new(&app.config().identifier)),
                license::GumroadConfig::from_build(),
            );
            app.manage(engine.clone());
            // §9.3 rule 3: at most every 30 days, never blocking anything.
            std::thread::spawn(move || {
                if engine.license.recheck_if_due() {
                    engine.emit(
                        ipc::events::WARNING,
                        &ipc::WarningEvent {
                            job_id: None,
                            code: ipc::ErrorCode::LicenseInvalid,
                            message: "Gumroad no longer recognises this licence key. The free version returns after 14 days.".into(),
                            count: None,
                        },
                    );
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::add_inputs,
            commands::remove_input,
            commands::analyze,
            commands::plan,
            commands::export_preview_report,
            commands::estimate_space,
            commands::volume_info,
            commands::start,
            commands::pause,
            commands::resume,
            commands::cancel,
            commands::retry_failed,
            commands::add_part,
            commands::get_report,
            commands::list_resumable_jobs,
            commands::get_settings,
            commands::set_settings,
            commands::telemetry_preview,
            commands::license_status,
            commands::activate,
            commands::deactivate,
            commands::open_path,
            commands::export_diagnostics,
            commands::app_info,
            commands::sidecar_versions,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The file tauri-build copies next to the app: `binaries/<name>-<target triple>[.exe]`.
    fn sidecar_file(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("binaries")
            .join(format!(
                "{name}-{}{}",
                env!("TARGET_TRIPLE"),
                std::env::consts::EXE_SUFFIX
            ))
    }

    /// Runs the real sidecar files when they are present. On the CI runners,
    /// which have neither tool, `tools/sidecars.py placeholders` leaves empty
    /// files so tauri-build is satisfied, and this test skips with a note.
    #[test]
    fn bundled_sidecars_report_their_versions() {
        for (name, arg, expected_prefix) in [
            ("ffmpeg", "-version", "ffmpeg version "),
            ("exiftool", "-ver", "1"),
        ] {
            let path = sidecar_file(name);
            let present = std::fs::metadata(&path)
                .map(|m| m.len() > 0)
                .unwrap_or(false);
            if !present {
                eprintln!(
                    "skip: {} is absent or a placeholder (python tools/sidecars.py fetch)",
                    path.display()
                );
                continue;
            }
            let out = Command::new(&path)
                .arg(arg)
                .output()
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            assert!(
                out.status.success(),
                "{name} {arg} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            let first = String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            assert!(
                first.starts_with(expected_prefix),
                "{name}: unexpected first line {first:?}"
            );
        }
    }
}
