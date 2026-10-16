// Runs one real job through the installed app's engine over WebView2 remote
// debugging: the same IPC commands the screens call, invoked from the page.
// This proves the shipped binary, its engine and the bundled ffmpeg together.
//
//   set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9517
//   "%LOCALAPPDATA%\Memories Rescue\memories-rescue.exe"
//   node tools/release/drive_job.mjs 9517 <export.zip> <destination folder>
//
// Prints app_info, sidecar_versions, the analysis summary, the plan totals
// and the final report counts. Exit code 1 if the job does not end in
// `done` or `partial`, or if no item was composited.
import { existsSync } from "node:fs";

const [port = "9517", input, destination] = process.argv.slice(2);
if (!input || !destination) {
  console.error("usage: node drive_job.mjs <port> <export.zip> <destination folder>");
  process.exit(2);
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

let page = null;
for (let i = 0; i < 60 && !page; i++) {
  try {
    const pages = await (await fetch(`http://127.0.0.1:${port}/json`)).json();
    page = pages.find((p) => p.type === "page" && /^(https?:\/\/tauri\.localhost|tauri:\/\/)/.test(p.url));
  } catch {}
  if (!page) await sleep(500);
}
if (!page) {
  console.error("no tauri://localhost page on the debugging port");
  process.exit(1);
}
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

// invoke(cmd, args) runs in the page through Tauri's bridge; errors come back as {error}.
const invoke = async (cmd, args = {}) => {
  const expr = `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(cmd)}, ${JSON.stringify(args)}).then(v => ({ ok: v }), e => ({ error: typeof e === 'string' ? e : JSON.stringify(e) }))`;
  const r = await send("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true });
  const v = r.result?.result?.value;
  if (!v) throw new Error(`${cmd}: no result (${JSON.stringify(r.result?.exceptionDetails?.text)})`);
  if (v.error) throw new Error(`${cmd}: ${v.error}`);
  return v.ok;
};

const info = await invoke("app_info");
console.log("app_info:", JSON.stringify(info));
const sidecars = await invoke("sidecar_versions");
console.log("sidecar_versions:", JSON.stringify(sidecars));

const inputs = await invoke("add_inputs", { paths: [input] });
console.log("add_inputs:", inputs.map((f) => `${f.name} (${f.kind}, ${f.size_bytes} B)`).join(", "));
const inputIds = inputs.map((f) => f.id);

const summary = await invoke("analyze", { inputIds });
console.log("analyze:", JSON.stringify({ profile: summary.profile, items: summary.items_total ?? summary.totals, warnings: summary.warnings?.length }));

const options = {
  destination,
  overlay_mode: "burn_in_keep_originals",
  keep_originals: true,
  clip_rejoin: false,
  clip_rejoin_years: null,
  apply_uncertain_locations: false,
  windows_compatibility: false,
  zip_output: false,
  zip_volume_bytes: null,
  home_timezone: "Europe/London",
  metadata_only: false,
  workers: null,
  excluded_clip_groups: [],
};
const plan = await invoke("plan", { inputIds, options });
console.log("plan:", JSON.stringify({ totals: plan.totals, issues: plan.issues?.map((i) => i.code) }));

const jobId = await invoke("start", { inputIds, options });
console.log("start: job", jobId);

let report = null;
for (let i = 0; i < 600; i++) {
  await sleep(1000);
  report = await invoke("get_report", { jobId });
  if (["done", "partial", "failed", "cancelled", "free_limit_reached"].includes(report.state)) break;
  if (i % 10 === 9) console.log(`  ${report.state}: ${JSON.stringify(report.counts)}`);
}
console.log("report:", JSON.stringify({ state: report.state, output_path: report.output_path, items_total: report.items_total, counts: report.counts, issues: report.issues?.map((i) => i.code), failures: report.failures?.length }));
ws.close();

const finished = report.state === "done" || report.state === "partial";
const outputExists = existsSync(report.output_path);
console.log(finished && outputExists ? "RESULT: job finished" : "RESULT: job did not finish cleanly");
process.exit(finished && outputExists ? 0 : 1);
