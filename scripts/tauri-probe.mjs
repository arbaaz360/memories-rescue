#!/usr/bin/env node
/**
 * Attaches to a running `npm run tauri dev` window over the WebView2
 * debugging port (start it with WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=
 * --remote-debugging-port=9223) and reports what the window chose at startup:
 * which backend, why, and the sidecar_versions smoke result. Also opens About
 * and captures both screens into docs/ui/screenshots/tauri/.
 */

import { mkdirSync, writeFileSync } from "node:fs";

const PORT = Number(process.env.TAURI_CDP_PORT ?? 9223);
const OUT = "docs/ui/screenshots/tauri";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function target() {
  for (let i = 0; i < 300; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
      const page = list.find((t) => t.type === "page" && t.url.startsWith("http://localhost:1420"));
      if (page) return page;
    } catch {
      /* window not up yet */
    }
    await sleep(1000);
  }
  throw new Error("no WebView2 page on the debugging port");
}

const page = await target();
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r, j) => {
  ws.onopen = r;
  ws.onerror = j;
});
let id = 0;
const pending = new Map();
ws.onmessage = (m) => {
  const msg = JSON.parse(m.data);
  if (msg.id && pending.has(msg.id)) {
    pending.get(msg.id)(msg);
    pending.delete(msg.id);
  }
};
const send = (method, params = {}) =>
  new Promise((resolve) => {
    const n = ++id;
    pending.set(n, resolve);
    ws.send(JSON.stringify({ id: n, method, params }));
  });
const evaluate = async (expression) => (await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true })).result?.result?.value;

for (let i = 0; i < 60; i++) {
  const ready = await evaluate(`!!(window.__mrBoot && window.__mrBoot.sidecars)`);
  if (ready) break;
  await sleep(500);
}
const report = await evaluate(`({
  url: location.href,
  title: document.title,
  h1: document.querySelector('h1')?.textContent,
  boot: window.__mrBoot,
  tauri: '__TAURI_INTERNALS__' in window,
  mockBadge: document.querySelector('.mock-badge')?.textContent ?? null,
  viewport: [innerWidth, innerHeight],
})`);
mkdirSync(OUT, { recursive: true });
const shot = async (name) => {
  const r = await send("Page.captureScreenshot", { format: "jpeg", quality: 85 });
  writeFileSync(`${OUT}/${name}.jpg`, Buffer.from(r.result.data, "base64"));
};
await shot("s0-welcome");
await evaluate(`[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'About').click()`);
await sleep(600);
report.aboutTools = await evaluate(`document.querySelector('.kv dd:last-of-type')?.textContent`);
await shot("about");
await evaluate(`[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Back').click()`);
// One mock round trip inside Tauri: Get started, Add files (real picker would open, so only check the screen change).
await evaluate(`[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Get started').click()`);
await sleep(400);
report.afterGetStarted = await evaluate(`document.querySelector('h1')?.textContent`);
console.log(JSON.stringify(report, null, 2));
ws.close();
