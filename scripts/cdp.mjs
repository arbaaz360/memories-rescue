#!/usr/bin/env node
/**
 * One step of a walk through the real app (`npm run tauri dev` started with
 * WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9223), over
 * the WebView2 debugging port. Each call connects, does one thing, prints
 * JSON and exits, so a walk is a sequence of shell commands:
 *
 *   node scripts/cdp.mjs eval  "<expression>"         value of the expression (promises awaited)
 *   node scripts/cdp.mjs click "<button text>"        clicks the button whose text is exactly that
 *   node scripts/cdp.mjs wait  "<expression>" [ms]    until the expression is truthy
 *   node scripts/cdp.mjs text  "<css selector>"       text of every match
 *   node scripts/cdp.mjs shot  <name>                 JPEG to $SHOTS (default docs/ui/screenshots/app-v1)
 *   node scripts/cdp.mjs state                        screen title, dialog, notices, live errors
 *
 * Native pickers cannot be scripted: put inputs in through the dev-only
 * `window.__mrStore` (for example by replacing `__mrStore.host.pickExportFiles`).
 */

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const PORT = Number(process.env.TAURI_CDP_PORT ?? 9223);
const SHOTS = process.env.SHOTS ?? "docs/ui/screenshots/app-v1";
const [cmd, arg, extra] = process.argv.slice(2);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function target() {
  for (let i = 0; i < 300; i++) {
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

const ws = new WebSocket((await target()).webSocketDebuggerUrl);
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
  new Promise((done) => {
    const n = ++id;
    pending.set(n, done);
    ws.send(JSON.stringify({ id: n, method, params }));
  });
const evaluate = async (expression) => {
  const r = await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true });
  if (r.result?.exceptionDetails) {
    throw new Error(r.result.exceptionDetails.exception?.description ?? JSON.stringify(r.result.exceptionDetails));
  }
  return r.result?.result?.value;
};
const title = () => evaluate(`document.querySelector('h1')?.textContent ?? null`);

let out;
switch (cmd) {
  case "eval":
    out = await evaluate(arg);
    break;
  case "click": {
    const ok = await evaluate(
      `(() => { const b = [...document.querySelectorAll('button, [role=button], a')].find(b => b.textContent.replace(/\\s+/g, ' ').trim() === ${JSON.stringify(arg)} && !b.disabled); if (!b) return false; b.click(); return true; })()`,
    );
    if (!ok) throw new Error(`no enabled button "${arg}" on "${await title()}"`);
    out = { clicked: arg };
    break;
  }
  case "wait": {
    const end = Date.now() + Number(extra ?? 60000);
    for (;;) {
      if (await evaluate(`!!(${arg})`).catch(() => false)) break;
      if (Date.now() > end) throw new Error(`timed out: ${arg} on "${await title()}"`);
      await sleep(250);
    }
    out = { ok: true, title: await title() };
    break;
  }
  case "text":
    out = await evaluate(`[...document.querySelectorAll(${JSON.stringify(arg)})].map(e => e.textContent.replace(/\\s+/g, ' ').trim())`);
    break;
  case "shot": {
    mkdirSync(SHOTS, { recursive: true });
    const r = await send("Page.captureScreenshot", { format: "jpeg", quality: 85 });
    const file = join(SHOTS, `${arg}.jpg`);
    writeFileSync(file, Buffer.from(r.result.data, "base64"));
    out = { file };
    break;
  }
  case "state":
    out = await evaluate(`({
      title: document.querySelector('h1')?.textContent ?? null,
      dialog: document.querySelector('dialog[open] h2')?.textContent ?? null,
      callouts: [...document.querySelectorAll('.callout-title')].map(e => e.textContent.trim()),
      buttons: [...document.querySelectorAll('button')].filter(b => b.offsetParent).map(b => b.textContent.replace(/\\s+/g, ' ').trim() + (b.disabled ? ' (disabled)' : '')),
      boot: window.__mrBoot ?? null,
    })`);
    break;
  default:
    throw new Error("usage: cdp.mjs eval|click|wait|text|shot|state ...");
}
console.log(JSON.stringify(out, null, 2));
ws.onclose = () => process.exit(0);
ws.close();
