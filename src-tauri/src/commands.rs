//! The Tauri commands of `docs/ipc-contract.md`, one per contract command.
//! Heavy work (discovery, planning, job creation, reports, licence checks)
//! runs on the blocking pool so the window never waits on the main thread.
//! Top-level parameters are snake_case here; Tauri maps the window's
//! camelCase keys onto them.

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_shell::ShellExt;

use crate::engine::Engine;
use crate::ipc::*;

type E<'a> = State<'a, Arc<Engine>>;

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> IpcResult<T> + Send + 'static,
) -> IpcResult<T> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| {
        IpcError::unknown("The engine stopped unexpectedly.").with_detail(e.to_string())
    })?
}

#[tauri::command]
pub async fn add_inputs(engine: E<'_>, paths: Vec<String>) -> IpcResult<Vec<InputFile>> {
    let e = engine.inner().clone();
    blocking(move || e.add_inputs(&paths)).await
}

#[tauri::command]
pub async fn remove_input(engine: E<'_>, id: String) -> IpcResult<()> {
    let e = engine.inner().clone();
    blocking(move || {
        e.remove_input(&id);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn analyze(engine: E<'_>, input_ids: Vec<String>) -> IpcResult<ExportSummary> {
    let e = engine.inner().clone();
    blocking(move || e.analyze(&input_ids)).await
}

#[tauri::command]
pub async fn plan(
    engine: E<'_>,
    input_ids: Vec<String>,
    options: JobOptions,
) -> IpcResult<PlanPreview> {
    let e = engine.inner().clone();
    blocking(move || e.plan(&input_ids, &options)).await
}

#[tauri::command]
pub async fn export_preview_report(engine: E<'_>, plan_id: String) -> IpcResult<String> {
    let e = engine.inner().clone();
    blocking(move || e.export_preview_report(&plan_id)).await
}

#[tauri::command]
pub async fn estimate_space(
    engine: E<'_>,
    input_ids: Vec<String>,
    options: JobOptions,
) -> IpcResult<SpaceEstimate> {
    let e = engine.inner().clone();
    blocking(move || e.estimate_space(&input_ids, &options)).await
}

#[tauri::command]
pub async fn volume_info(engine: E<'_>, path: String) -> IpcResult<VolumeInfo> {
    let e = engine.inner().clone();
    blocking(move || e.volume_info(&path)).await
}

#[tauri::command]
pub async fn start(
    engine: E<'_>,
    input_ids: Vec<String>,
    options: JobOptions,
) -> IpcResult<String> {
    let e = engine.inner().clone();
    blocking(move || e.start(&input_ids, &options)).await
}

#[tauri::command]
pub fn pause(engine: E<'_>, job_id: String) -> IpcResult<()> {
    engine.pause(&job_id)
}

#[tauri::command]
pub fn resume(engine: E<'_>, job_id: String) -> IpcResult<()> {
    engine.inner().resume(&job_id)
}

#[tauri::command]
pub fn cancel(engine: E<'_>, job_id: String) -> IpcResult<()> {
    engine.cancel(&job_id)
}

#[tauri::command]
pub async fn retry_failed(
    engine: E<'_>,
    job_id: String,
    options: Option<JobOptions>,
) -> IpcResult<()> {
    let e = engine.inner().clone();
    blocking(move || e.retry_failed(&job_id, options.as_ref())).await
}

#[tauri::command]
pub async fn add_part(engine: E<'_>, job_id: String, path: String) -> IpcResult<InputFile> {
    let e = engine.inner().clone();
    blocking(move || e.add_part(&job_id, &path)).await
}

#[tauri::command]
pub async fn get_report(engine: E<'_>, job_id: String) -> IpcResult<JobReport> {
    let e = engine.inner().clone();
    blocking(move || e.get_report(&job_id)).await
}

#[tauri::command]
pub async fn list_resumable_jobs(engine: E<'_>) -> IpcResult<Vec<ResumableJob>> {
    let e = engine.inner().clone();
    blocking(move || Ok(e.list_resumable_jobs())).await
}

#[tauri::command]
pub fn get_settings(engine: E<'_>) -> IpcResult<Settings> {
    Ok(engine.get_settings())
}

#[tauri::command]
pub fn set_settings(engine: E<'_>, settings: Settings) -> IpcResult<Settings> {
    engine.set_settings(settings)
}

#[tauri::command]
pub fn license_status(engine: E<'_>) -> IpcResult<LicenseStatus> {
    Ok(engine.license.status())
}

#[tauri::command]
pub async fn activate(engine: E<'_>, key: String) -> IpcResult<LicenseStatus> {
    let e = engine.inner().clone();
    blocking(move || e.license.activate(&key)).await
}

#[tauri::command]
pub fn deactivate(engine: E<'_>) -> IpcResult<LicenseStatus> {
    Ok(engine.license.deactivate())
}

/// Opens a file or folder with the system (output folder, report, notices).
/// The window has no open-path permission; this is the only way it opens paths.
#[tauri::command]
pub fn open_path(app: AppHandle, path: String) -> IpcResult<()> {
    let p = PathBuf::from(&path);
    if !p.exists() {
        return Err(IpcError::new(
            ErrorCode::PartMissing,
            "That file or folder doesn't exist (any more).",
        )
        .with_detail(crate::engine::redact_home(&path)));
    }
    app.opener()
        .open_path(path, None::<&str>)
        .map_err(|e| IpcError::unknown("The system could not open it.").with_detail(e.to_string()))
}

#[tauri::command]
pub async fn diagnostics_preview(engine: E<'_>) -> IpcResult<DiagnosticsPreview> {
    let e = engine.inner().clone();
    blocking(move || Ok(e.diagnostics_preview())).await
}

/// §10.2. `force` (the Settings button) checks even when update checks are off.
#[tauri::command]
pub async fn check_update(app: AppHandle, force: bool) -> IpcResult<Option<UpdateInfo>> {
    crate::updater::check(&app, force).await
}

/// §10.2: only after the user agreed, never while a job runs; restarts the app.
#[tauri::command]
pub async fn install_update(app: AppHandle) -> IpcResult<()> {
    crate::updater::install(&app).await
}

#[tauri::command]
pub async fn export_diagnostics(engine: E<'_>) -> IpcResult<String> {
    let e = engine.inner().clone();
    blocking(move || e.export_diagnostics()).await
}

#[tauri::command]
pub fn app_info(engine: E<'_>) -> IpcResult<AppInfo> {
    Ok(engine.app_info())
}

/// Run both bundled sidecars and return what they report. Fails with a
/// readable message if either is missing, cannot start, or exits non-zero,
/// which is what the installed-app smoke test looks for.
#[tauri::command]
pub async fn sidecar_versions(app: AppHandle) -> Result<SidecarVersions, String> {
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
