#!/usr/bin/env node
/**
 * Walks the real app (`npm run tauri dev`, engine wired) on a fixture export,
 * over the WebView2 debugging port (WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=
 * --remote-debugging-port=9223). Native pickers cannot be scripted, so inputs
 * and the destination go in through the dev-only `window.__mrStore`; every
 * other step clicks the real buttons. Screenshots go to
 * docs/ui/screenshots/engine/. Usage:
 *
 *   node scripts/tauri-walk.mjs <fixture dir with ZIPs> <empty output dir>
 */

import { mkdirSync, readdirSync, writeFileSync, existsSync } from "node:fs";
import { join, resolve } from "node:path";

const PORT = Number(process.env.TAURI_CDP_PORT ?? 9223);
const OUT = "docs/ui/screenshots/engine";
const [fixtureDir, outDir] = process.argv.slice(2).map((p) => resolve(p));
if (!fixtureDir || !outDir) throw new Error("usage: tauri-walk.mjs <fixture dir> <output dir>");
const zips = readdirSync(fixtureDir)
  .filter((n) => n.endsWith(".zip"))
  .map((n) => join(fixtureDir, n));
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function target() {
  for (let i = 0; i < 400; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
      const page = list.find((t) => t.type === "page" && t.url.startsWith("http://localhost:1420"));
      if (page) return page;
    } catch {
      /* not up yet */
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
  new Promise((resolveSend) => {
    const n = ++id;
    pending.set(n, resolveSend);
    ws.send(JSON.stringify({ id: n, method, params }));
  });
const evaluate = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) throw new Error(`${expression.slice(0, 80)}: ${r.result.exceptionDetails.exception?.description}`);
  return r.result?.result?.value;
};
const waitFor = async (expr, ms = 60000, what = expr) => {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    if (await evaluate(`!!(${expr})`).catch(() => false)) return;
    await sleep(200);
  }
  throw new Error(`timed out: ${what} on "${await evaluate(`document.querySelector('h1')?.textContent`)}"`);
};
const click = async (text) => {
  const ok = await evaluate(`(() => { const b = [...document.querySelectorAll('button')].find(b => b.textContent.replace(/\\s+/g, ' ').trim() === ${JSON.stringify(text)}); if (!b) return false; b.click(); return true; })()`);
  if (!ok) throw new Error(`no button "${text}" on "${await evaluate(`document.querySelector('h1')?.textContent`)}"`);
};
mkdirSync(OUT, { recursive: true });
const shot = async (name) => {
  const r = await send("Page.captureScreenshot", { format: "jpeg", quality: 85 });
  writeFileSync(join(OUT, `${name}.jpg`), Buffer.from(r.result.data, "base64"));
};
const text = (sel) => evaluate(`[...document.querySelectorAll(${JSON.stringify(sel)})].map(e => e.textContent.replace(/\\s+/g, ' ').trim())`);

const report = {};
await waitFor(`window.__mrBoot && window.__mrStore && document.querySelector('h1')`);
report.boot = await evaluate(`window.__mrBoot`);
report.mockBadge = await evaluate(`document.querySelector('.mock-badge')?.textContent ?? null`);
if (report.boot.backend !== "tauri") throw new Error(`expected the Tauri engine, got ${JSON.stringify(report.boot)}`);

await click("Get started");
await evaluate(`window.__mrStore.addPaths(${JSON.stringify(zips)})`);
await waitFor(`document.querySelectorAll('.file-row').length === ${zips.length}`);
report.inputs = await text(".file-row");
await shot("s1-add-files");

await click("Continue");
await waitFor(`document.querySelector('.parts-list')`, 120000);
report.parts = await text(".part");
report.summaryCallouts = await text(".callout-title");
report.counts = await text(".stat");
await shot("s2-summary");

const missing = await evaluate(`!!document.querySelector('.part-missing')`);
await click(missing ? "Continue anyway" : "Continue");
await waitFor(`document.querySelector('h1')?.textContent.startsWith('Choose how')`);
await evaluate(`window.__mrStore.setOptions({ destination: ${JSON.stringify(outDir)} })`);
await waitFor(`document.querySelector('.meter')`, 30000);
report.space = await evaluate(`document.querySelector('.meter')?.getAttribute('aria-valuetext')`);
await shot("s3-options");

await click("Preview first");
await waitFor(`document.querySelector('table.samples tbody tr')`, 120000);
await sleep(1500);
report.preview = {
  stats: await text("dl.stats .stat"),
  warnings: await text("#pwarn-title ~ .callout .callout-title"),
  thumbs: await evaluate(`[...document.querySelectorAll('.thumb img')].map(i => ({ src: i.src.slice(0, 40), ok: i.complete && i.naturalWidth > 0 }))`),
  firstRow: (await text("table.samples tbody tr"))[0],
};
await shot("s4-preview");

await click("Start");
await waitFor(`document.querySelector('dialog[open]') || document.querySelector('.outcome-groups')`, 300000, "free limit or results");
report.atStop = {
  dialog: await evaluate(`document.querySelector('dialog[open] h2')?.textContent ?? null`),
  progress: await evaluate(`document.querySelector('[role=progressbar]')?.getAttribute('aria-valuetext')`),
};
await shot("s5-free-limit");
if (report.atStop.dialog) await click("Finish for now");
await waitFor(`document.querySelector('.outcome-groups')`, 120000, "results");
await sleep(800);
report.results = {
  title: await evaluate(`document.querySelector('h1').textContent`),
  outcomes: await text(".outcome"),
  reportButton: await evaluate(`!![...document.querySelectorAll('button')].find(b => b.textContent.includes('Open report'))`),
  reportFile: existsSync(join(outDir, "Memories Rescue", "_report", "report.html")),
  outputFiles: existsSync(join(outDir, "Memories Rescue")) ? readdirSync(join(outDir, "Memories Rescue")) : [],
};
await shot("s6-results");

await click("About");
await waitFor(`document.querySelector('h1')?.textContent === 'About Memories Rescue'`);
report.aboutTools = await evaluate(`document.querySelector('.kv dd:last-of-type')?.textContent`);
await click("Back");
await click("Settings");
await waitFor(`document.querySelector('h1')?.textContent === 'Settings'`);
report.settingsDestination = await evaluate(`window.__mrStore.settings.value.destination`);
await click("Back");
report.resumable = await evaluate(`window.__TAURI_INTERNALS__.invoke('list_resumable_jobs')`);
report.console = await evaluate(`window.__errors ?? null`);
console.log(JSON.stringify(report, null, 2));
ws.close();
