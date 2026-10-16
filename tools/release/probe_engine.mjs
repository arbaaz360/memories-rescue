// Asks a running Memories Rescue window whether the Rust engine (not the
// TypeScript mock) is behind it, over WebView2's DevTools protocol.
//
// Launch the installed app with remote debugging enabled, with no other
// instance (and no leftover msedgewebview2.exe of the app) running:
//
//   set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9333
//   "%LOCALAPPDATA%\Memories Rescue\memories-rescue.exe"
//   node tools/release/probe_engine.mjs 9333 [screenshot.png]
//
// It evaluates `window.__mrBoot` (set by the window at boot: backend "tauri"
// with reason "engine", or "mock") and calls the engine's `app_info`
// command through Tauri's internal bridge, which only succeeds when the
// shipped binary carries the engine. Exit code 1 when either says mock.
import { writeFileSync } from "node:fs";

const port = process.argv[2] || "9333";
const outPng = process.argv[3];
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let page = null;
for (let i = 0; i < 60 && !page; i++) {
  try {
    const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    // Only the app's own page: production windows load from tauri://localhost
    // (http://tauri.localhost on Windows). Anything else on the port belongs
    // to some other browser and must not be mistaken for the app.
    page = pages.find((p) => p.type === "page" && /^(https?:\/\/tauri\.localhost|tauri:\/\/)/.test(p.url));
  } catch {}
  if (!page) await sleep(500);
}
if (!page) {
  console.error(`no tauri://localhost page on the debugging port ${port}; is the app running with --remote-debugging-port, and is the port free?`);
  process.exit(1);
}
console.log("page:", page.title, page.url);

const ws = new WebSocket(page.webSocketDebuggerUrl);
let id = 0;
const pending = new Map();
ws.onmessage = (ev) => {
  const m = JSON.parse(ev.data);
  if (m.id && pending.has(m.id)) {
    pending.get(m.id)(m);
    pending.delete(m.id);
  }
};
const send = (method, params = {}) =>
  new Promise((res) => {
    const mid = ++id;
    pending.set(mid, res);
    ws.send(JSON.stringify({ id: mid, method, params }));
  });
await new Promise((r) => (ws.onopen = r));

const evaluate = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) return { error: r.result.exceptionDetails.text || "exception" };
  return r.result?.result?.value;
};

let boot = null;
for (let i = 0; i < 40; i++) {
  boot = await evaluate("window.__mrBoot ?? null");
  if (boot) break;
  await sleep(500);
}
console.log("__mrBoot:", JSON.stringify(boot));

const appInfo = await evaluate(
  "window.__TAURI_INTERNALS__ ? window.__TAURI_INTERNALS__.invoke('app_info', {}).then(v => ({ ok: v }), e => ({ error: String(e) })) : { error: 'no __TAURI_INTERNALS__' }",
);
console.log("app_info via engine:", JSON.stringify(appInfo));

const text = await evaluate("document.body.innerText");
console.log("---- window text ----\n" + text);

if (outPng) {
  const shot = await send("Page.captureScreenshot", { format: "png" });
  writeFileSync(outPng, Buffer.from(shot.result.data, "base64"));
  console.log("screenshot:", outPng);
}
ws.close();

const engine = (boot?.backend === "tauri" || boot === null) && appInfo?.ok && typeof appInfo.ok.version === "string";
console.log(engine ? "RESULT: engine answers" : "RESULT: engine NOT confirmed");
process.exit(engine ? 0 : 1);
