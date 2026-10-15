// Frontend placeholder for milestone M1. Plain TypeScript, no UI framework yet.
// All user-facing strings will move to an i18n catalogue before M3 (SRS C-09).

import { invoke } from "@tauri-apps/api/core";

const APP_NAME = "Memories Rescue";

/** Mirrors `SidecarVersions` in src-tauri/src/lib.rs. */
interface SidecarVersions {
  ffmpeg: string;
  exiftool: string;
}

function byId(id: string): HTMLElement | null {
  return document.getElementById(id);
}

async function showSidecarVersions(): Promise<void> {
  const status = byId("sidecar-status");
  try {
    const v = await invoke<SidecarVersions>("sidecar_versions");
    const ffmpeg = byId("ffmpeg-version");
    const exiftool = byId("exiftool-version");
    if (ffmpeg) ffmpeg.textContent = v.ffmpeg;
    if (exiftool) exiftool.textContent = v.exiftool;
    if (status) {
      status.textContent = "Both bundled tools answered.";
      status.dataset.state = "ok";
    }
  } catch (err) {
    if (status) {
      status.textContent = `Bundled tool check failed: ${String(err)}`;
      status.dataset.state = "error";
    }
  }
}

window.addEventListener("DOMContentLoaded", () => {
  document.title = APP_NAME;
  const status = byId("status");
  if (status) {
    status.dataset.ready = "true";
  }
  void showSidecarVersions();
});
