# UI verification (feat/ui, 2026-10-16)

What was checked on the UI before the Rust engine exists, how, and the result.
Everything here runs against the mock engine (`src/ipc/mock/`), except the
Tauri smoke test at the end.

## How to repeat it

```bash
npm run dev                      # http://localhost:1420, mock engine in a plain browser
node scripts/ui-verify.mjs       # headless Edge/Chrome walk, writes docs/ui/verification.json and screenshots
npm test                         # Vitest
```

`?mock=licensed,resume,legacy,diskfull` picks mock scenarios and
`?mock_speed=N` the simulated seconds per real second (default 30).

`scripts/ui-verify.mjs` drives the page over the DevTools protocol with real
input events. Per run it walks Welcome, Add files, Check (part 3 missing),
Options (including the FR-28 shortfall on a small drive and its fix), Preview,
Start, the free-limit stop, Activate in the Unlock dialog, Processing with
Pause and Resume, Results, Settings and About, in four modes:

| Mode | Viewport | Media emulation |
|---|---|---|
| light | 1000×700 (the default window) | `prefers-color-scheme: light` |
| dark | 1000×700 | `prefers-color-scheme: dark` |
| zoom200 | 450×300 CSS px at device scale 2, which is a 900×600 window at 200% | light |
| forced | 1000×700 | `forced-colors: active`, dark, `prefers-reduced-motion: reduce` |

## Results

| Check | Requirement | Result |
|---|---|---|
| axe-core 4.14, tags wcag2a, wcag2aa, wcag21a, wcag21aa (includes colour contrast) on every screen, 11 screens × 4 modes | NFR-USE-03, WCAG 2.1 AA | 0 violations in all 44 runs |
| Horizontal overflow at 200% (900×600 window) | NFR-USE-05 | None on any screen. The sample table on Preview scrolls inside its own labelled region, which WCAG 1.4.10 allows for data tables |
| Forced colors | NFR-USE-03 | Controls, progress bar, meter and confidence bars keep visible boundaries in system colours; screenshots in `forced/` |
| Reduced motion | NFR-USE-03 | Spinner and indeterminate bar stop; progress transition 1e-06 s |
| Keyboard only, Welcome to Results and back (Tab, Shift+Tab, Enter, Space; key typed into the Unlock dialog) | NFR-USE-03 | Completed. Every focused element had a visible 3 px outline. The Unlock dialog opens with focus on the key field. Focus stays on the Pause/Resume button across the toggle. A checkbox toggles with Space |
| Screen title focus on screen change | NFR-USE-03 | The `h1` of each screen takes focus before first paint (layout effect), so screen readers announce the new screen |
| Progress announcements | NFR-USE-03 | One polite live region; progress text at most once per 10 s (`AnnounceThrottle`, unit-tested: 13 messages in 2 minutes of 4 events per second); state changes announced at once |
| Minimum window | NFR-USE-05 | `minWidth: 900`, `minHeight: 600` in `src-tauri/tauri.conf.json` |
| RTL readiness | NFR-I18N-02 | CSS uses logical properties only; `<html dir>` follows the UI language |

Details per screen and mode are in `docs/ui/verification.json` (generated).

## In the built-in browser

Walked by hand in the app's browser pane against `npm run dev`: add files
(one non-export ignored with a notice and a live-region message), Check with
part 3 missing, Add missing part on Check (part 3 then found, 4,812 items),
Start, free-limit stop, Later (job finishes partial: 96 done, 3 date only,
1 failed, 4,712 waiting for unlock, shown in separate groups), Unlock to finish
from Results with a wrong key (LICENSE_INVALID copy) and then the mock key
(job resumes at item 101), Settings saved and kept, telemetry off by default
with its payload preview, About. Then, licensed: a run with part 3 missing,
Add missing part from Results, Check in job mode, Rescue these memories, and a
final result of 4,812 accounted for with nothing left "not in your export". No
console errors.

## Inside Tauri before the engine was wired (feat/ui, `npm run tauri dev`, Windows 11)

Read over the WebView2 debugging port (`scripts/tauri-probe.mjs`):

| Check | Result |
|---|---|
| Window loads the UI | Yes, 1000×700, Welcome |
| `sidecar_versions` smoke call | `ffmpeg version n9.0.2-28-g669f5ab083-20261015 …`, `13.59` |
| Engine probe (`app_info`) | `Command app_info not found`, so the mock engine took over; dev badge "Mock engine: Rust engine not wired yet" |
| Allowed: `notification` is_permission_granted, notify; `dialog` open (passes the permission gate) | Answered; a test notification was delivered |
| Refused: opener with another URL, opener `open_path`, dialog save and message, shell execute and spawn | "not allowed" from the ACL |
| Absent: http and fs plugins | "Plugin not found" |

## With the real engine (feat/engine-wiring, `npm run tauri dev`, Windows 11)

`scripts/tauri-walk.mjs` drives the wired app over the WebView2 debugging
port on fixture F-05 (`fixtures/missing-part`: parts 1, 2 and 4 of a 1,000-item
export). Inputs and the destination go in through the dev-only
`window.__mrStore` because native pickers cannot be scripted; everything else
is the real buttons.

| Step | What the window showed |
|---|---|
| Startup | `backend: tauri`, `reason: engine`, no mock badge; About lists ffmpeg n9.0.2 and ExifTool 13.59 |
| Add files | Three ZIPs: Primary, Part 2 (the renamed `mydata (1).zip`, numbered from its content), Part 4 |
| Check | "Part 3 appears to be missing", 250 memories in that part; profile 2026b, 750 by file order |
| Options | Live space gauge on the destination drive |
| Preview | 750 to rescue, 250 not in the export; 11 photo thumbnails loaded through the asset protocol; warnings for part 3, 7 uncertain locations, and captions that cannot be burned in until the compositor is installed |
| Start | Stopped at 100 of 100 with the Unlock dialog (free tier) |
| Finish for now | Results: 93 done, 7 saved without a location, 250 not in your export (part 3), 650 waiting for unlock; `report.html` written; 2017 and 2018 folders in the output |

Screenshots: `docs/ui/screenshots/engine/` (published on the assets branch).
The walk's job, settings and thumbnails were removed from app data afterwards.

The engine adapter also has its own tests (`src-tauri/tests/engine.rs`, on the
fixtures, no window): F-05's 250 missing mains and 77 orphan overlays, a
links-only export blocked, a consistent preview, the free tier stopping at
exactly 100 and finishing partial after "Later", activation against a fake
Gumroad unlocking the remaining 150 without redoing the first 100 (and
re-checks sending `increment_uses_count=false`), adding the missing part
processing only what it brings, settings persisting, and diagnostics holding
no export content. `tests/ipc_samples.rs` round-trips JSON produced by the
TypeScript mock through the Rust contract types.

## Not verified here

- macOS (no Mac on hand): WKWebView rendering, VoiceOver, the real picker and notification permission prompt.
- Narrator or NVDA with a person listening; the structure (landmarks, headings, labels, live region) is what they read.
- Real pickers and drag-and-drop with real files inside Tauri: the plugins are registered and allowed, and the browser stand-in exercised the same code paths, but nothing was dropped onto the Tauri window.
- Compositing (captions burned in) and clip rejoin: not in the engine yet.
- A real Gumroad product: activation was tested against a fake on loopback; the product id is not configured in this build.
