import { defineConfig } from "vite";
import preact from "@preact/preset-vite";
import process from "node:process";
import { readFileSync } from "node:fs";

const host = process.env.TAURI_DEV_HOST;
const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf-8")) as { version: string };

// https://vite.dev/config/
export default defineConfig(() => ({
  plugins: [preact()],
  define: {
    __APP_VERSION__: JSON.stringify(pkg.version),
  },
  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    // WebView2 (Chromium) on Windows and WKWebView from macOS 12 (Safari 15).
    target: ["chrome105", "safari15"],
  },
}));
