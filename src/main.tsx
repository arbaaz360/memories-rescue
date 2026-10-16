import { render } from "preact";
import "./styles/tokens.css";
import "./styles/app.css";
import { App } from "./app";
import { detectTauri, probeSidecars, selectBackend } from "./ipc/backend";
import { BrowserHost, detectPlatform, type Host } from "./ipc/host";
import { MockBackend, mockConfigFromUrl } from "./ipc/mock/engine";
import { TauriBackend } from "./ipc/tauri";
import { AppStore } from "./state/store";

declare global {
  interface Window {
    /** Dev builds only: what the window chose at startup, for the verification scripts. */
    __mrBoot?: { backend: string; reason: string; detail: string | null; sidecars: unknown };
    /** Dev builds only: run axe-core on the current page. */
    __runAxe?: () => Promise<unknown>;
    /** Dev builds only: the store, so verification scripts can add real paths without a picker. */
    __mrStore?: AppStore;
  }
}

async function boot(): Promise<void> {
  const inTauri = detectTauri();
  const platform = detectPlatform(navigator.userAgent);
  const selection = await selectBackend({
    isTauri: () => inTauri,
    createTauri: () => new TauriBackend(),
    createMock: () => new MockBackend(mockConfigFromUrl(window.location.search, platform)),
  });

  let host: Host;
  if (inTauri) {
    const { TauriHost } = await import("./ipc/tauri-host");
    host = new TauriHost();
  } else {
    host = new BrowserHost(platform);
  }

  const store = new AppStore(selection.backend, host, { reason: selection.reason, detail: selection.detail });
  await store.init();

  const root = document.getElementById("app");
  if (!root) throw new Error("#app missing");
  render(<App store={store} />, root);

  // The smoke call: the shell answers sidecar_versions even before the engine is wired.
  void probeSidecars(inTauri, () => new TauriBackend()).then((probe) => {
    store.sidecars.value = probe;
    if (import.meta.env.DEV) {
      window.__mrBoot = { backend: selection.backend.kind, reason: selection.reason, detail: selection.detail, sidecars: probe };
      console.info("[boot]", window.__mrBoot);
    }
  });

  if (import.meta.env.DEV) {
    window.__mrBoot = { backend: selection.backend.kind, reason: selection.reason, detail: selection.detail, sidecars: null };
    window.__mrStore = store;
    window.__runAxe = async () => {
      const axe = (await import("axe-core")).default;
      return axe.run(document, { runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"] } });
    };
  }
}

void boot().catch((e: unknown) => {
  // Last resort: nothing else can render. Plain text, no strings to translate yet.
  console.error(e);
  const root = document.getElementById("app");
  if (root) root.textContent = String(e);
});
