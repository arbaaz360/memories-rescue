//! Tauri shell for Memories Rescue. M1 spike: ffmpeg and exiftool ship as
//! sidecars (`bundle.externalBin` in `tauri.conf.json`) and are reached through
//! the shell plugin. The capability in `capabilities/default.json` lets the
//! webview run exactly those two binaries with fixed arguments; the Rust side
//! below is what the app actually uses. The processing core lives in
//! `crates/mr-core` and `crates/mr-meta`.

use tauri::AppHandle;
use tauri_plugin_shell::ShellExt;

/// What `sidecar_versions` returns to the window.
#[derive(Debug, serde::Serialize)]
pub struct SidecarVersions {
    /// First line of `ffmpeg -version`, e.g. `ffmpeg version n9.0.2-28-g669f5ab083-20261015 ...`.
    pub ffmpeg: String,
    /// Output of `exiftool -ver`, e.g. `13.59`.
    pub exiftool: String,
}

/// Run both bundled sidecars and return what they report. Fails with a
/// readable message if either is missing, cannot start, or exits non-zero,
/// which is what the installed-app smoke test looks for.
#[tauri::command]
async fn sidecar_versions(app: AppHandle) -> Result<SidecarVersions, String> {
    Ok(SidecarVersions {
        ffmpeg: first_line(&app, "ffmpeg", &["-version"]).await?,
        exiftool: first_line(&app, "exiftool", &["-ver"]).await?,
    })
}

async fn first_line(app: &AppHandle, name: &str, args: &[&str]) -> Result<String, String> {
    let output = app
        .shell()
        .sidecar(name)
        .map_err(|e| format!("{name}: {e}"))?
        .args(args)
        .output()
        .await
        .map_err(|e| format!("{name}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "{name} exited with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![sidecar_versions])
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
