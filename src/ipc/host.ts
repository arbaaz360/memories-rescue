/**
 * Platform services the screens use directly, outside the engine contract:
 * pickers, drag-and-drop, opening URLs, notifications and thumbnail URLs.
 * `TauriHost` (./tauri-host.ts) uses the dialog, opener and notification
 * plugins; `BrowserHost` stands in for them in a plain browser so the whole
 * flow can be walked with the mock engine.
 */

import type { Unlisten } from "./backend";

export type Platform = "windows" | "macos" | "linux" | "other";

export interface DragDropEvent {
  type: "enter" | "over" | "drop" | "leave";
  paths: string[];
}

export interface PickerText {
  title: string;
  filterName: string;
}

export interface Host {
  readonly kind: "tauri" | "browser";
  readonly platform: Platform;
  pickExportFiles(text: PickerText): Promise<string[]>;
  pickExportFolders(title: string): Promise<string[]>;
  pickDestination(title: string, current: string | null): Promise<string | null>;
  onDragDrop(handler: (e: DragDropEvent) => void): Promise<Unlisten>;
  notify(title: string, body: string): Promise<void>;
}

export function detectPlatform(userAgent: string): Platform {
  if (/Windows/i.test(userAgent)) return "windows";
  if (/Macintosh|Mac OS X/i.test(userAgent)) return "macos";
  if (/Linux/i.test(userAgent)) return "linux";
  return "other";
}

/** Fake locations the browser stand-in hands out, per platform. */
export function samplePaths(platform: Platform) {
  const win = platform === "windows" || platform === "other";
  const downloads = win ? "C:\\Users\\you\\Downloads" : "/Users/you/Downloads";
  const join = (dir: string, name: string) => (win ? `${dir}\\${name}` : `${dir}/${name}`);
  return {
    downloads,
    join,
    exportFiles: [
      join(downloads, "mydata~1729000000000.zip"),
      join(downloads, "mydata~1729000000000-2.zip"),
      join(downloads, "mydata~1729000000000-4.zip"),
      join(downloads, "IMG_2041.jpg"),
    ],
    missingPart: join(downloads, "mydata~1729000000000-3.zip"),
    folder: join(downloads, "mydata~1729000000000"),
    destinations: win ? ["D:\\Memories", "E:\\"] : ["/Users/you/Pictures/Memories", "/Volumes/USB"],
  };
}

/** Plain-browser stand-in. Pickers return sample paths so the mock flow can be walked end to end. */
export class BrowserHost implements Host {
  readonly kind = "browser" as const;
  readonly platform: Platform;
  private filePicks = 0;
  private destinationPicks = 0;

  constructor(platform: Platform = detectPlatform(navigator.userAgent)) {
    this.platform = platform;
  }

  async pickExportFiles(): Promise<string[]> {
    const p = samplePaths(this.platform);
    this.filePicks += 1;
    // First pick: the three parts that arrived plus a stray photo; later picks: the missing part.
    return this.filePicks === 1 ? p.exportFiles : [p.missingPart];
  }

  async pickExportFolders(): Promise<string[]> {
    return [samplePaths(this.platform).folder];
  }

  async pickDestination(): Promise<string | null> {
    const d = samplePaths(this.platform).destinations;
    const pick = d[this.destinationPicks % d.length];
    this.destinationPicks += 1;
    return pick;
  }

  async onDragDrop(handler: (e: DragDropEvent) => void): Promise<Unlisten> {
    const p = samplePaths(this.platform);
    const toPaths = (dt: DataTransfer | null): string[] => {
      if (!dt) return [];
      const out: string[] = [];
      for (const item of Array.from(dt.items)) {
        const entry = typeof item.webkitGetAsEntry === "function" ? item.webkitGetAsEntry() : null;
        const file = item.getAsFile();
        const name = entry?.name ?? file?.name;
        if (name) out.push(p.join(p.downloads, name));
      }
      return out;
    };
    let depth = 0;
    const onEnter = (e: DragEvent) => {
      e.preventDefault();
      depth += 1;
      if (depth === 1) handler({ type: "enter", paths: [] });
    };
    const onOver = (e: DragEvent) => {
      e.preventDefault();
      handler({ type: "over", paths: [] });
    };
    const onLeave = (e: DragEvent) => {
      e.preventDefault();
      depth = Math.max(0, depth - 1);
      if (depth === 0) handler({ type: "leave", paths: [] });
    };
    const onDrop = (e: DragEvent) => {
      e.preventDefault();
      depth = 0;
      handler({ type: "drop", paths: toPaths(e.dataTransfer) });
    };
    window.addEventListener("dragenter", onEnter);
    window.addEventListener("dragover", onOver);
    window.addEventListener("dragleave", onLeave);
    window.addEventListener("drop", onDrop);
    return () => {
      window.removeEventListener("dragenter", onEnter);
      window.removeEventListener("dragover", onOver);
      window.removeEventListener("dragleave", onLeave);
      window.removeEventListener("drop", onDrop);
    };
  }

  async notify(title: string, body: string): Promise<void> {
    if ("Notification" in window && Notification.permission === "granted") {
      new Notification(title, { body });
    } else {
      console.info(`[notification] ${title}: ${body}`);
    }
  }
}
