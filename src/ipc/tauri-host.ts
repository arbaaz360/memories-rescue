import { convertFileSrc } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { open } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { Unlisten } from "./backend";
import { detectPlatform, type DragDropEvent, type Host, type PickerText, type Platform } from "./host";
import { mockThumbnailUrl } from "./mock/thumbs";

function asList(picked: string | string[] | null): string[] {
  if (picked === null) return [];
  return Array.isArray(picked) ? picked : [picked];
}

/** Real platform services through the dialog, opener and notification plugins. */
export class TauriHost implements Host {
  readonly kind = "tauri" as const;
  readonly platform: Platform = detectPlatform(navigator.userAgent);

  async pickExportFiles(text: PickerText): Promise<string[]> {
    return asList(
      await open({ title: text.title, multiple: true, directory: false, filters: [{ name: text.filterName, extensions: ["zip"] }] }),
    );
  }

  async pickExportFolders(title: string): Promise<string[]> {
    return asList(await open({ title, multiple: true, directory: true }));
  }

  async pickDestination(title: string, current: string | null): Promise<string | null> {
    const picked = await open({ title, multiple: false, directory: true, defaultPath: current ?? undefined });
    return typeof picked === "string" ? picked : null;
  }

  async onDragDrop(handler: (e: DragDropEvent) => void): Promise<Unlisten> {
    return getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "drop") handler({ type: "drop", paths: p.paths });
      else if (p.type === "enter") handler({ type: "enter", paths: p.paths });
      else if (p.type === "over") handler({ type: "over", paths: [] });
      else handler({ type: "leave", paths: [] });
    });
  }

  async openUrl(url: string): Promise<void> {
    await openUrl(url);
  }

  async notify(title: string, body: string): Promise<void> {
    let granted = await isPermissionGranted();
    if (!granted) granted = (await requestPermission()) === "granted";
    if (granted) sendNotification({ title, body });
  }

  assetUrl(path: string): string {
    return mockThumbnailUrl(path) ?? convertFileSrc(path);
  }
}
