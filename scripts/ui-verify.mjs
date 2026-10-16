#!/usr/bin/env node
/**
 * Walks the whole Memories Rescue UI against the mock engine in headless
 * Edge/Chrome over the DevTools protocol, and records what the PR claims:
 *
 * - every screen in light and dark (screenshots in docs/ui/screenshots/)
 * - axe-core WCAG 2.1 A/AA results per screen and scheme (includes contrast)
 * - the same flow at 450x300 CSS px with device scale 2, which is a
 *   900x600 window at 200% zoom: no horizontal overflow allowed
 * - forced-colors (Windows high contrast) and prefers-reduced-motion
 * - a keyboard-only walk from Welcome to Results with focus visibility checks
 *
 * Needs `npm run dev` on http://localhost:1420 (or UI_URL). Writes
 * docs/ui/verification.json. Usage: node scripts/ui-verify.mjs [--no-shots]
 */

import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const BASE = process.env.UI_URL ?? "http://localhost:1420/";
const OUT = process.env.UI_OUT ?? "docs/ui/screenshots";
const PORT = Number(process.env.UI_CDP_PORT ?? 9333);
const SHOTS = !process.argv.includes("--no-shots");
/** `--only=light,keyboard` runs a subset and leaves docs/ui/verification.json alone. */
const ONLY = process.argv.find((a) => a.startsWith("--only="))?.slice(7).split(",") ?? null;
const BROWSERS = [
  process.env.UI_BROWSER,
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
].filter(Boolean);

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

class Cdp {
  constructor(url) {
    this.ws = new WebSocket(url);
    this.id = 0;
    this.pending = new Map();
    this.ws.onmessage = (m) => {
      const msg = JSON.parse(typeof m.data === "string" ? m.data : m.data.toString());
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) reject(new Error(`${msg.error.message} (${msg.error.code})`));
        else resolve(msg.result);
      }
    };
  }
  open() {
    return new Promise((resolve, reject) => {
      this.ws.onopen = resolve;
      this.ws.onerror = reject;
    });
  }
  send(method, params = {}) {
    const id = ++this.id;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }
  close() {
    this.ws.close();
  }
}

async function launch() {
  const exe = BROWSERS.find((b) => existsSync(b));
  if (!exe) throw new Error("No Chromium browser found; set UI_BROWSER");
  const dir = mkdtempSync(join(tmpdir(), "mr-ui-"));
  const proc = spawn(
    exe,
    ["--headless=new", `--remote-debugging-port=${PORT}`, `--user-data-dir=${dir}`, "--no-first-run", "--no-default-browser-check", "--disable-extensions", "--hide-scrollbars", "--force-color-profile=srgb", "about:blank"],
    { stdio: "ignore" },
  );
  for (let i = 0; i < 100; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${PORT}/json/list`)).json();
      const page = list.find((t) => t.type === "page");
      if (page) {
        const cdp = new Cdp(page.webSocketDebuggerUrl);
        await cdp.open();
        return { cdp, proc, dir };
      }
    } catch {
      /* not up yet */
    }
    await sleep(150);
  }
  proc.kill();
  throw new Error("browser did not start");
}

function page(cdp) {
  const evaluate = async (expression) => {
    const r = await cdp.send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
    if (r.exceptionDetails) throw new Error(`${expression.slice(0, 80)}: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`);
    return r.result.value;
  };
  const waitFor = async (expression, timeout = 15000, what = expression) => {
    const end = Date.now() + timeout;
    while (Date.now() < end) {
      try {
        if (await evaluate(`!!(${expression})`)) return;
      } catch {
        /* page navigating */
      }
      await sleep(100);
    }
    throw new Error(`timed out waiting for ${what}`);
  };
  const h1 = () => evaluate(`document.querySelector('h1')?.textContent ?? ''`);
  const click = async (text, scope = "document") => {
    const ok = await evaluate(`(() => {
      const t = ${JSON.stringify(text)};
      const root = ${scope};
      const els = [...root.querySelectorAll('button, summary, label, a')].filter(e => e.offsetParent !== null || e.closest('dialog[open]'));
      const el = els.find(e => e.textContent.replace(/\\s+/g, ' ').trim() === t) ?? els.find(e => e.textContent.replace(/\\s+/g, ' ').trim().startsWith(t));
      if (!el) return false;
      el.scrollIntoView({ block: 'center' });
      el.click();
      return true;
    })()`);
    if (!ok) throw new Error(`no clickable "${text}" on "${await h1()}"`);
  };
  const key = async (k, shift = false) => {
    const map = {
      Tab: { code: "Tab", vk: 9 },
      Enter: { code: "Enter", vk: 13, text: "\r" },
      " ": { code: "Space", vk: 32, text: " " },
      Escape: { code: "Escape", vk: 27 },
    };
    const d = map[k];
    const base = { key: k, code: d.code, windowsVirtualKeyCode: d.vk, nativeVirtualKeyCode: d.vk, modifiers: shift ? 8 : 0 };
    await cdp.send("Input.dispatchKeyEvent", { type: d.text ? "keyDown" : "rawKeyDown", ...base, text: d.text });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...base });
  };
  return { evaluate, waitFor, h1, click, key };
}

async function emulate(cdp, { width, height, dsf = 1, scheme = "light", forced = "none", motion = "no-preference" }) {
  await cdp.send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor: dsf, mobile: false });
  await cdp.send("Emulation.setEmulatedMedia", {
    features: [
      { name: "prefers-color-scheme", value: scheme },
      { name: "forced-colors", value: forced },
      { name: "prefers-reduced-motion", value: motion },
    ],
  });
}

/**
 * Full-page captures grow the viewport to the content height first, so the
 * sticky action bar sits at the bottom as it does in the window, then restore it.
 */
async function shot(cdp, file, { full = true } = {}, view = null) {
  if (!SHOTS) return;
  const params = { format: "jpeg", quality: 82 };
  if (full && view) {
    const m = await cdp.send("Page.getLayoutMetrics");
    const h = Math.min(4200, Math.max(view.height, Math.ceil(m.cssContentSize.height)));
    await cdp.send("Emulation.setDeviceMetricsOverride", { width: view.width, height: h, deviceScaleFactor: view.dsf ?? 1, mobile: false });
    await sleep(200);
  }
  const { data } = await cdp.send("Page.captureScreenshot", params);
  if (full && view) {
    await cdp.send("Emulation.setDeviceMetricsOverride", { width: view.width, height: view.height, deviceScaleFactor: view.dsf ?? 1, mobile: false });
  }
  mkdirSync(join(OUT, file.split("/").slice(0, -1).join("/")), { recursive: true });
  writeFileSync(join(OUT, `${file}.jpg`), Buffer.from(data, "base64"));
}

async function axe(p) {
  return p.evaluate(`window.__runAxe().then(r => r.violations.map(v => ({ id: v.id, impact: v.impact, nodes: v.nodes.length, sample: v.nodes.slice(0, 3).map(n => n.target.join(' ') + ' :: ' + (n.failureSummary || '').split('\\n').slice(1, 2).join(' ')) })))`);
}

async function overflow(p) {
  return p.evaluate(`(() => {
    const doc = document.documentElement;
    const over = doc.scrollWidth - doc.clientWidth;
    if (over <= 1) return null;
    const culprits = [...document.querySelectorAll('main *')].filter(e => {
      const r = e.getBoundingClientRect();
      return r.right > doc.clientWidth + 1 && !e.closest('.table-wrap');
    }).slice(0, 5).map(e => e.tagName.toLowerCase() + '.' + [...e.classList].join('.'));
    return { over, culprits };
  })()`);
}

/** One full pass through the flow. `record(name)` runs after each screen settles. */
async function walk(cdp, mode, record) {
  const p = page(cdp);
  await cdp.send("Page.navigate", { url: `${BASE}?mock=resume&mock_speed=60` });
  await p.waitFor(`document.querySelector('h1.screen-title') && window.__mrBoot`);
  await record("s0-welcome");

  await p.click("Get started");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Add your export files'`);
  await p.click("Add files…");
  await p.waitFor(`document.querySelectorAll('.file-row').length === 4`);
  await record("s1-add-files");

  await p.click("Continue");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Check your export' && document.querySelector('.parts-list')`);
  await record("s2-summary");

  await p.click("Continue anyway");
  await p.waitFor(`document.querySelector('h1')?.textContent.startsWith('Choose how') && document.querySelector('.meter')`);
  await record("s3-options");

  // FR-28 shortfall: the browser stand-in hands out D:\\Memories, then E:\\ (10.9 GB free).
  await p.click("Choose folder…");
  await p.waitFor(`document.querySelector('.dest-row .path')?.textContent.includes('D:')`);
  await p.click("Choose folder…");
  await p.waitFor(`document.querySelector('.meter-short')`);
  await record("s3-options-short");
  await p.click("Don't keep originals");
  await p.waitFor(`!document.querySelector('.meter-short') && document.querySelector('.meter')`);
  await p.click("Choose folder…");
  await p.waitFor(`document.querySelector('.dest-row .path')?.textContent.includes('D:')`);
  await p.click("Add them to the picture, and keep the originals");
  await sleep(300);

  await p.click("Preview first");
  await p.waitFor(`document.querySelector('table.samples tbody tr') && document.querySelector('.stack-bar')`, 15000);
  await record("s4-preview");

  await p.click("Start");
  await p.waitFor(`document.querySelector('dialog[open]')`, 20000, "free-limit dialog");
  await record("unlock-free-limit", { full: false });

  await p.evaluate(`(() => { const i = document.querySelector('dialog[open] input'); i.value = 'MOCK-1234-5678-9ABC'; i.dispatchEvent(new Event('input', { bubbles: true })); })()`);
  await p.click("Activate", "document.querySelector('dialog[open]')");
  await p.waitFor(`!document.querySelector('dialog[open]') && document.querySelector('h1')?.textContent === 'Rescuing your memories'`, 10000);
  await p.waitFor(`document.querySelectorAll('.recent-item').length > 5 && !document.querySelector('.progress-indeterminate')`, 10000);
  await sleep(2500);
  await record("s5-processing");

  await p.click("Pause");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Paused'`, 5000);
  await p.click("Resume");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Rescuing your memories'`, 5000);

  await p.waitFor(`document.querySelector('.outcome-groups')`, 90000, "results");
  await record("s6-results");

  await p.click("Settings");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Settings'`);
  await p.evaluate(`document.querySelector('details.payload').open = true`);
  await record("settings");
  await p.click("About");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'About Memories Rescue'`);
  await record("about");
  return mode;
}

/** Keyboard only: Tab / Shift+Tab / Enter / Space, from Welcome to Results. */
async function keyboardWalk(cdp) {
  const p = page(cdp);
  const log = [];
  const noVisibleFocus = new Set();
  await emulate(cdp, { width: 1000, height: 700 });
  await cdp.send("Page.navigate", { url: `${BASE}?mock_speed=80` });
  await p.waitFor(`document.querySelector('h1.screen-title') && window.__mrBoot`);

  const focusInfo = () =>
    p.evaluate(`(() => {
      const e = document.activeElement;
      if (!e || e === document.body) return { text: '', tag: 'body', visible: false };
      const cs = getComputedStyle(e);
      const visible = cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) >= 2;
      const text = (e.getAttribute('aria-label') || (e.labels && e.labels[0] && e.labels[0].textContent) || e.textContent || e.value || '').replace(/\\s+/g, ' ').trim();
      return { text, tag: e.tagName.toLowerCase(), visible, inDialog: !!e.closest('dialog[open]') };
    })()`);

  const tabTo = async (label, max = 90) => {
    for (let i = 0; i < max; i++) {
      await p.key("Tab");
      const f = await focusInfo();
      if (f.tag !== "body" && f.tag !== "h1" && !f.visible) noVisibleFocus.add(`${f.tag}: ${f.text.slice(0, 40)}`);
      if (f.text === label || f.text.startsWith(label)) return i + 1;
    }
    throw new Error(`keyboard: could not reach "${label}" on "${await p.h1()}"`);
  };
  const step = async (label, keyName = "Enter") => {
    const presses = await tabTo(label);
    await p.key(keyName);
    log.push({ screen: await p.h1(), action: label, tabs: presses });
  };
  const h1Focused = () => p.evaluate(`document.activeElement?.tagName === 'H1'`);

  const h1Checks = [];
  await step("Get started");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Add your export files'`);
  h1Checks.push(await h1Focused());
  await step("Add files…");
  await p.waitFor(`document.querySelectorAll('.file-row').length === 4`);
  await step("Continue");
  await p.waitFor(`document.querySelector('.parts-list')`);
  h1Checks.push(await h1Focused());
  await step("Continue anyway");
  await p.waitFor(`document.querySelector('.meter')`);
  h1Checks.push(await h1Focused());
  // A checkbox toggled with Space, then back.
  await step("Apply uncertain locations", " ");
  const toggled = await p.evaluate(`[...document.querySelectorAll('.check')].find(c => c.textContent.includes('Apply uncertain'))?.querySelector('input').checked`);
  await p.key(" ");
  await step("Preview first");
  await p.waitFor(`document.querySelector('table.samples tbody tr')`, 15000);
  h1Checks.push(await h1Focused());
  await step("Start");
  await p.waitFor(`document.querySelector('dialog[open]')`, 20000);
  const dialogFocus = await focusInfo();
  // Escape is "Later" for the free-limit dialog; instead type the key and press Enter in the field.
  await cdp.send("Input.insertText", { text: "MOCK-1234-5678-9ABC" });
  await p.key("Enter");
  await p.waitFor(`!document.querySelector('dialog[open]') && document.querySelector('h1')?.textContent === 'Rescuing your memories'`, 10000);
  await step("Pause");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Paused'`, 5000);
  const afterPause = await focusInfo();
  await step("Resume", " ");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Rescuing your memories'`, 5000);
  await p.waitFor(`document.querySelector('.outcome-groups')`, 90000, "results");
  h1Checks.push(await h1Focused());
  await step("Retry");
  await p.waitFor(`document.querySelector('.outcome-groups') && !document.querySelector('.group-failed')`, 30000, "retry done");
  await step("New job");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Add your export files'`);
  // Shift+Tab reaches the top navigation; Settings opens with Enter, Back returns.
  for (let i = 0; i < 12; i++) {
    await p.key("Tab", true);
    if ((await focusInfo()).text === "Settings") break;
  }
  await p.key("Enter");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Settings'`);
  await step("Back");
  await p.waitFor(`document.querySelector('h1')?.textContent === 'Add your export files'`);

  return {
    log,
    h1FocusedOnScreenChange: h1Checks,
    checkboxToggledBySpace: toggled === true,
    dialogInitialFocus: dialogFocus,
    focusAfterPause: afterPause,
    elementsWithoutVisibleFocus: [...noVisibleFocus],
  };
}

async function main() {
  const { cdp, proc, dir } = await launch();
  const results = { url: BASE, when: new Date().toISOString(), modes: {}, keyboard: null };
  try {
    await cdp.send("Page.enable");
    await cdp.send("Runtime.enable");
    const modes = [
      { name: "light", width: 1000, height: 700, scheme: "light", shots: true },
      { name: "dark", width: 1000, height: 700, scheme: "dark", shots: true },
      { name: "zoom200", width: 450, height: 300, dsf: 2, scheme: "light", shots: true, zoom: true },
      { name: "forced", width: 1000, height: 700, scheme: "dark", forced: "active", motion: "reduce", shots: true },
    ];
    for (const mode of modes.filter((m) => !ONLY || ONLY.includes(m.name))) {
      await emulate(cdp, mode);
      const screens = {};
      const p = page(cdp);
      await walk(cdp, mode.name, async (name, opts) => {
        await sleep(250);
        const entry = { h1: await p.h1() };
        entry.axe = await axe(p);
        if (mode.zoom) entry.overflow = await overflow(p);
        if (mode.motion === "reduce") {
          entry.motion = await p.evaluate(`(() => { const s = document.querySelector('.spinner, .progress-fill'); return s ? getComputedStyle(s).animationName + ' / ' + getComputedStyle(s).transitionDuration : 'n/a'; })()`);
        }
        if (mode.shots) await shot(cdp, `${mode.name}/${name}`, opts, mode);
        screens[name] = entry;
        process.stdout.write(`${mode.name}/${name}: axe ${entry.axe.length}${entry.overflow ? ` overflow ${JSON.stringify(entry.overflow)}` : ""}\n`);
      });
      results.modes[mode.name] = screens;
    }
    if (!ONLY || ONLY.includes("keyboard")) results.keyboard = await keyboardWalk(cdp);
    process.stdout.write(`keyboard: ${JSON.stringify(results.keyboard)}\n`);
  } finally {
    mkdirSync("docs/ui", { recursive: true });
    const out = ONLY ? join(tmpdir(), "mr-ui-verification-partial.json") : "docs/ui/verification.json";
    writeFileSync(out, `${JSON.stringify(results, null, 2)}\n`);
    cdp.close();
    proc.kill();
    await sleep(500);
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {
      /* Edge may still hold the profile for a moment */
    }
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});
