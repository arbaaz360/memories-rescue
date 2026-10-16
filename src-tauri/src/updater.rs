//! Auto-update (SRS §10.2) through tauri-plugin-updater, from Rust only: the
//! window has no updater permission and calls `check_update` and
//! `install_update` like any other command.
//!
//! * The feed is `latest.json` on the releases repository's latest release
//!   (stable) or `latest-beta.json` on its fixed `channel-beta` release
//!   (beta; GitHub's "latest" never points at a prerelease), both in
//!   `src/config/links.json`, signed with the minisign key whose public half
//!   is in `tauri.conf.json`.
//! * A check runs at launch and then every 24 hours while update checks are
//!   on (Settings); a found update is announced with `update_available`.
//!   Network and feed errors are logged and change nothing.
//! * Nothing installs without the user's click, and never while a job runs:
//!   the guard is checked before the download and again before the install.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::engine::Engine;
use crate::ipc::{IpcError, IpcResult, UpdateChannel, UpdateInfo, events};
use crate::links::LINKS;

/// Between scheduled checks.
pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// A check without `force` reuses an answer younger than this.
const REUSE: Duration = Duration::from_secs(60 * 60);

/// The update found by the last check, kept until the user installs it.
#[derive(Default)]
pub struct Updates {
    pending: Mutex<Option<Update>>,
    last: Mutex<Option<(Instant, UpdateChannel, Option<UpdateInfo>)>>,
}

/// The feed of a channel.
pub fn feed_url(channel: UpdateChannel) -> &'static str {
    match channel {
        UpdateChannel::Stable => &LINKS.update_feed_stable,
        UpdateChannel::Beta => &LINKS.update_feed_beta,
    }
}

/// [`feed_url`], or in debug builds `MR_UPDATE_FEED` (a local feed for the
/// walk-through; release builds never read it).
fn endpoint(channel: UpdateChannel) -> String {
    if cfg!(debug_assertions)
        && let Ok(v) = std::env::var("MR_UPDATE_FEED")
        && !v.is_empty()
    {
        return v;
    }
    feed_url(channel).to_string()
}

/// §10.2: never while a job runs.
pub fn install_allowed(job_running: bool) -> IpcResult<()> {
    if job_running {
        Err(IpcError::unknown(
            "Updates never install while a job is running. Finish or pause it first.",
        )
        .with_detail("job running"))
    } else {
        Ok(())
    }
}

fn info_of(u: &Update, channel: UpdateChannel) -> UpdateInfo {
    UpdateInfo {
        version: u.version.clone(),
        current_version: u.current_version.clone(),
        date: u
            .date
            .and_then(|d| chrono::DateTime::from_timestamp(d.unix_timestamp(), 0))
            .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        notes: u.body.clone().filter(|b| !b.trim().is_empty()),
        channel,
    }
}

/// Asks the feed of the chosen channel. Without `force`, `None` when update
/// checks are off, and a recent answer for the same channel is reused.
pub async fn check(app: &AppHandle, force: bool) -> IpcResult<Option<UpdateInfo>> {
    let engine = app.state::<std::sync::Arc<Engine>>().inner().clone();
    let updates = app.state::<Updates>();
    let settings = engine.get_settings();
    if !force && !settings.update_checks {
        return Ok(None);
    }
    let channel = settings.update_channel;
    if !force {
        let last = updates.last.lock().expect("updates").clone();
        if let Some((at, ch, info)) = last
            && ch == channel
            && at.elapsed() < REUSE
        {
            return Ok(info);
        }
    }
    let url = match endpoint(channel).parse() {
        Ok(u) => u,
        Err(e) => {
            tracing::error!("update feed address is not valid: {e}");
            return Ok(None);
        }
    };
    let updater = app
        .updater_builder()
        .endpoints(vec![url])
        .and_then(|b| b.timeout(Duration::from_secs(30)).build());
    let found = match updater {
        Ok(u) => u.check().await,
        Err(e) => {
            tracing::error!("updater could not start: {e}");
            return Ok(None);
        }
    };
    let info = match found {
        Ok(Some(u)) => {
            let info = info_of(&u, channel);
            tracing::info!("update {} available on {channel:?}", info.version);
            *updates.pending.lock().expect("updates") = Some(u);
            Some(info)
        }
        Ok(None) => {
            *updates.pending.lock().expect("updates") = None;
            None
        }
        Err(e) => {
            tracing::warn!("update check failed: {e}");
            return Ok(None);
        }
    };
    *updates.last.lock().expect("updates") = Some((Instant::now(), channel, info.clone()));
    Ok(info)
}

/// Downloads, verifies the signature, installs and restarts. Only after the
/// user agreed in the window, and never while a job runs.
pub async fn install(app: &AppHandle) -> IpcResult<()> {
    let engine = app.state::<std::sync::Arc<Engine>>().inner().clone();
    install_allowed(engine.any_job_running())?;
    let mut update = app
        .state::<Updates>()
        .pending
        .lock()
        .expect("updates")
        .take();
    if update.is_none() {
        check(app, true).await?;
        update = app
            .state::<Updates>()
            .pending
            .lock()
            .expect("updates")
            .take();
    }
    let Some(update) = update else {
        return Err(IpcError::unknown("There is no update to install."));
    };
    let failed = |e: tauri_plugin_updater::Error| {
        tracing::error!("update failed: {e}");
        IpcError::unknown("The update could not be installed.").with_detail(e.to_string())
    };
    tracing::info!("downloading update {}", update.version);
    let bytes = update.download(|_, _| {}, || {}).await.map_err(failed)?;
    // A job may have started during the download.
    install_allowed(engine.any_job_running())?;
    tracing::info!("installing update {}", update.version);
    update.install(bytes).map_err(failed)?;
    app.restart();
}

/// At launch, then every 24 hours: announces an update with `update_available`.
pub fn schedule(app: AppHandle) {
    std::thread::Builder::new()
        .name("update-checks".into())
        .spawn(move || {
            loop {
                if let Ok(Some(info)) = tauri::async_runtime::block_on(check(&app, false)) {
                    let engine = app.state::<std::sync::Arc<Engine>>();
                    engine.emit(events::UPDATE_AVAILABLE, &info);
                }
                std::thread::sleep(INTERVAL);
            }
        })
        .expect("spawn update checks");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_map_to_their_feeds() {
        assert!(feed_url(UpdateChannel::Stable).ends_with("/releases/latest/download/latest.json"));
        // GitHub's /releases/latest/ never resolves to a prerelease, so the
        // beta manifest lives on the fixed `channel-beta` release instead.
        assert!(
            feed_url(UpdateChannel::Beta)
                .ends_with("/releases/download/channel-beta/latest-beta.json")
        );
        assert!(
            feed_url(UpdateChannel::Stable)
                .starts_with("https://github.com/arbaaz360/memories-rescue-releases/")
        );
    }

    #[test]
    fn nothing_installs_while_a_job_runs() {
        let e = install_allowed(true).unwrap_err();
        assert_eq!(e.code, crate::ipc::ErrorCode::Unknown);
        assert_eq!(e.detail.as_deref(), Some("job running"));
        assert!(install_allowed(false).is_ok());
    }
}
