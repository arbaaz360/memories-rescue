# Releasing Memories Rescue

How a version tag becomes signed installers and a draft release on the
public releases repository `arbaaz360/memories-rescue-releases`, what
secrets that needs, and how the owner obtains each of them. Companion
decisions: `docs/decisions/signing.md` (why these certificates),
`third_party/ffmpeg/PUBLISHING.md` on the ffmpeg branch (what the LGPL
obligations require per release), SRS §10.

## The one-command release

```bash
# 1. bump the version in package.json, package-lock.json, Cargo.toml [workspace.package]
#    and src-tauri/tauri.conf.json; write the CHANGELOG.md entry "## [1.0.0] - YYYY-MM-DD"
python tools/release/notices.py          # if dependencies changed
python tools/release/check.py --tag v1.0.0
git commit -am "Release 1.0.0" && git tag v1.0.0 && git push origin main v1.0.0
```

Pushing the tag runs `.github/workflows/release.yml`:

| Job | Runner | Does |
|---|---|---|
| Checklist | ubuntu | `tools/release/check.py --tag`: versions agree, changelog entry, notices current, overlay parses; extracts the release notes from CHANGELOG.md; prerelease when the tag has a `-` (`v1.1.0-beta.1`) |
| Windows x64 | windows | stages the pinned minimal ffmpeg, picks eSigner or Trusted Signing from the secrets, `tauri build --config src-tauri/tauri.release.conf.json`: NSIS (per-user) and MSI, app and sidecar signed through `tools/release/sign-windows.ps1`, updater `.sig` files, uploads to the draft release |
| macOS universal | macos | same with `--target universal-apple-darwin`: app and sidecar signed with the Developer ID, hardened runtime, notarized and stapled DMG, updater `.tar.gz` + `.sig` |
| Finish draft release | ubuntu | `tools/release/finish_release.py`: `SHA256SUMS.txt`, the ffmpeg source bundle copied in from the sidecar release, and an "unsigned build" label when any secret was missing. The release stays a draft. |

The owner then opens the draft on the releases repository, reads the notes,
and runs

```bash
python tools/release/publish.py v1.0.0
```

which publishes the draft and rewrites `latest-beta.json` on the fixed
`channel-beta` release. Nothing publishes itself, so installed apps only see
a release the owner has looked at.

### Update channels

| Channel | The app reads | Written by |
|---|---|---|
| stable | `.../releases/latest/download/latest.json` | tauri-action attaches `latest.json` to every release; GitHub's `/releases/latest/` resolves to the newest published non-prerelease |
| beta | `.../releases/download/channel-beta/latest-beta.json` | `publish.py`, on every publish (beta or stable), onto the fixed release `channel-beta`, created as a prerelease so it can never become "latest" |

GitHub's `/releases/latest/` never points at a prerelease, which is why the
beta feed cannot sit on the prerelease itself. A beta user therefore also
moves forward to the next stable release and is never left behind on an old
beta. The two addresses live in `src/config/links.json` (and the stable one
again in `tauri.conf.json`, as Tauri requires).

Dry runs: `workflow_dispatch` with an existing tag builds without pushing a
new tag (re-running a failed release); `allow_dev_ffmpeg` lets the Windows
job use the BtbN development ffmpeg when no minimal build is pinned, and the
release is labelled accordingly. Tag pushes never use the development ffmpeg.

## What is released

| Artifact | From | Notes |
|---|---|---|
| `Memories Rescue_<ver>_x64-setup.exe` | NSIS | per-user install (no UAC), WebView2 bootstrapper downloaded if missing; the consumer download |
| `Memories Rescue_<ver>_x64_en-US.msi` | WiX | per-machine (Tauri's template); for managed deployment |
| `Memories Rescue_<ver>_x64-setup.exe.sig`, `.msi.sig` | updater | signatures with the updater key |
| `Memories Rescue_<ver>_universal.dmg` | macOS | notarized and stapled |
| `Memories Rescue_universal.app.tar.gz` + `.sig` | updater | what the macOS updater downloads |
| `latest.json` | tauri-action | updater manifest, both platforms, NSIS preferred on Windows; copied to `channel-beta/latest-beta.json` by `publish.py` |
| `SHA256SUMS.txt` | finish step | every asset above |
| `ffmpeg-9.0.2.tar.xz`, `.asc`, `build.sh`, `BUILDINFO-*.txt`, `COPYING.LGPLv2.1` | sidecar release | the LGPL complete corresponding source, next to the binaries that were built from it |
| `BUILD-NOTES.txt` | finish step | only when unsigned or development ffmpeg; such a release is not to be published |

Inside the installers: `memories-rescue.exe` / `Memories Rescue.app`,
`ffmpeg` (the only sidecar; exiftool is not shipped, see Q-02),
`COPYING.LGPLv2.1`, `SOURCE_OFFER.txt` and `THIRD_PARTY_NOTICES.md` as
resources. On Windows the resources sit in the install folder; on macOS in
`Contents/Resources`. The About screen opens `THIRD_PARTY_NOTICES.md` from
the resource directory (`app_info.third_party_notices_path`).

## Secrets

Repository secrets of `arbaaz360/memories-rescue` (Settings → Secrets and
variables → Actions). Every one is optional except `RELEASES_TOKEN`; the
workflow builds without the others and labels the result unsigned.

| Secret | What | Where it comes from |
|---|---|---|
| `RELEASES_TOKEN` | fine-grained personal access token, **Contents: read and write** on `arbaaz360/memories-rescue-releases` only, 1-year expiry | GitHub → Settings → Developer settings → Fine-grained tokens. Used by tauri-action to create the draft release and upload assets, and by the finish step |
| `TAURI_SIGNING_PRIVATE_KEY` | contents of `C:\Users\ASUS\.tauri\memories-rescue-updater.key` (the whole file, one line) | generated by the UI session with `npm run tauri signer generate`; the public key is in `tauri.conf.json` `plugins.updater.pubkey` |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | the key's password | Connections\Secrets.md |
| `APPLE_CERTIFICATE` | the Developer ID Application certificate and private key as a base64-encoded `.p12` | see "Apple" below |
| `APPLE_CERTIFICATE_PASSWORD` | password chosen when exporting the `.p12` | same |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: <name> (<TEAMID>)` exactly as `security find-identity -v -p codesigning` prints it | same |
| `APPLE_TEAM_ID` | the 10-character team id | developer.apple.com → Membership details |
| `APPLE_API_ISSUER` | App Store Connect API key issuer id (UUID) | App Store Connect → Users and Access → Integrations → App Store Connect API |
| `APPLE_API_KEY` | the key id (10 characters) | same |
| `APPLE_API_KEY_CONTENT` | contents of the downloaded `AuthKey_<KEYID>.p8` | same (downloadable once) |
| `ESIGNER_USERNAME`, `ESIGNER_PASSWORD` | SSL.com account login | see "Windows, SSL.com eSigner" |
| `ESIGNER_CREDENTIAL_ID` | the eSigner credential id of the code-signing certificate | SSL.com dashboard → the certificate → eSigner details |
| `ESIGNER_TOTP_SECRET` | the eSigner TOTP secret (automation) | same page, "Automation" |
| `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` | an Entra ID app registration with the **Trusted Signing Certificate Profile Signer** role on the account | see "Windows, Azure Trusted Signing" |
| `TRUSTED_SIGNING_ENDPOINT` | e.g. `https://weu.codesigning.azure.net` | Trusted Signing account overview |
| `TRUSTED_SIGNING_ACCOUNT` | the Trusted Signing account name | same |
| `TRUSTED_SIGNING_PROFILE` | the certificate profile name | same |

Windows picks eSigner when `ESIGNER_USERNAME` is set, else Trusted Signing
when `TRUSTED_SIGNING_ENDPOINT` is set, else unsigned. macOS signs when
`APPLE_CERTIFICATE` is set and notarizes when the API key secrets are set
(Tauri also accepts `APPLE_ID` + `APPLE_PASSWORD` app-specific password +
`APPLE_TEAM_ID` instead of the API key; the API key is preferred for CI).

Nothing in the repository holds a secret: `sign-windows.ps1` and the
workflow pass them to the signing tools from the environment only.

## Owner's steps

### Apple (Developer ID, notarization)

1. Enrol in the Apple Developer Program as NovaByteWorks (organisation
   enrolment needs a D-U-N-S number; individual enrolment is same-day).
   US$99/year.
2. On any Mac signed in with that Apple ID: Xcode → Settings → Accounts →
   Manage Certificates → **+ → Developer ID Application**. Or on
   developer.apple.com → Certificates → + → Developer ID Application, with a
   CSR from Keychain Access.
3. Keychain Access → My Certificates → right-click the Developer ID
   Application certificate → Export → `.p12` with a password. Then
   `base64 -i cert.p12 | pbcopy` → `APPLE_CERTIFICATE`; the password →
   `APPLE_CERTIFICATE_PASSWORD`; `security find-identity -v -p codesigning`
   → `APPLE_SIGNING_IDENTITY`.
4. App Store Connect → Users and Access → Integrations → App Store Connect
   API → Team Keys → + with the **Developer** role. Note the Issuer ID and
   Key ID, download the `.p8` once → `APPLE_API_ISSUER`, `APPLE_API_KEY`,
   `APPLE_API_KEY_CONTENT`.
5. `APPLE_TEAM_ID` from Membership details.
6. First signed run: check the Finish job's `check.py --bundles` output
   (`spctl -a -vv -t exec` must say `source=Notarized Developer ID`,
   `stapler validate` must succeed), then install the DMG on a Mac that has
   never seen the app, downloaded through a browser so it carries the
   quarantine flag.

### Windows, SSL.com eSigner (primary)

1. Buy an **OV code signing certificate** from SSL.com with eSigner cloud
   signing (the key is generated and held in their cloud HSM; nothing to
   ship). Organisation validation: company registration, address and phone
   verification; 1 to 3 weeks. Prices change; expect a few hundred US$ per
   year plus the eSigner subscription.
2. After issuance, in the SSL.com dashboard open the certificate → eSigner:
   note the **credential id**; enable 2FA and under Automation create the
   **TOTP secret**. Secrets: `ESIGNER_USERNAME`, `ESIGNER_PASSWORD`,
   `ESIGNER_CREDENTIAL_ID`, `ESIGNER_TOTP_SECRET`.
3. The workflow downloads SSL.com's CodeSignTool on the runner; the sign
   script calls it once per binary (`CodeSignTool sign -input_file_path
   <file> -override=true`). eSigner bills per signature on some plans: a
   release signs `memories-rescue.exe`, `ffmpeg.exe`, the NSIS uninstaller,
   the setup exe and the MSI, so five signatures per release.
4. SSL.com's sandbox (`https://www.ssl.com/sandbox`) issues test
   credentials that work with the same tool; good for a dry run before the
   real certificate arrives.

### Windows, Azure Trusted Signing (alternative)

Microsoft's service, about US$10 per month, identity validation included,
but at the time of writing open to organisations in the US and Canada (or
with three years of verifiable history elsewhere). If NovaByteWorks passes
validation it is the cheaper path.

1. Azure portal → create a **Trusted Signing account**, then an **Identity
   validation** (organisation) and a **Certificate profile** of type Public
   Trust.
2. Entra ID → App registrations → new registration → Certificates & secrets
   → new client secret. On the Trusted Signing account → Access control →
   add role **Trusted Signing Certificate Profile Signer** to that app.
3. Secrets: `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`,
   `TRUSTED_SIGNING_ENDPOINT`, `TRUSTED_SIGNING_ACCOUNT`,
   `TRUSTED_SIGNING_PROFILE`. The workflow installs the
   `Microsoft.Trusted.Signing.Client` package and signs with `signtool
   /dlib`.

### Updater key

Already generated by the UI session: private key at
`C:\Users\ASUS\.tauri\memories-rescue-updater.key`, password in
Connections\Secrets.md, public key in `tauri.conf.json`. Put the file
contents into `TAURI_SIGNING_PRIVATE_KEY` and the password into
`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. **Losing this key means no installed
app can ever accept an update again**; keep a copy outside this machine.

### Releases repository

1. Create the public repository `arbaaz360/memories-rescue-releases`
   (default branch `main`; the workflow uses `main` as the commitish for
   the tags it creates there), then fill it:

   ```bash
   python tools/release/prepare_releases_repo.py --out ../memories-rescue-releases
   cd ../memories-rescue-releases
   git init -b main && git add . && git commit -m "Downloads repository"
   git remote add origin https://github.com/arbaaz360/memories-rescue-releases.git && git push -u origin main
   ```

   The script copies the README and LICENSE from `docs/release/releases-repo/`
   and the ffmpeg licence text, source offer and build notes from
   `third_party/ffmpeg/`, so this repository keeps one copy of each.
2. Create the fine-grained token above (`RELEASES_TOKEN`).
3. **Sidecar release** (once per ffmpeg version). The Windows binary is
   built: `third_party/ffmpeg/dist/x86_64-pc-windows-msvc/ffmpeg.exe`,
   9,453,568 bytes, SHA-256
   `f17252176a50b04338cd8b41a8a4fe03b06d72e8aa6f18ca5054c392ba966977`,
   `ffmpeg version 9.0.2`; that exact file is what
   `tools/release/sidecars.lock.json` pins. Upload it under the asset name
   the lock file gives, together with the source bundle:

   ```bash
   cd third_party/ffmpeg
   cp dist/x86_64-pc-windows-msvc/ffmpeg.exe ffmpeg-x86_64-pc-windows-msvc.exe
   cp dist/x86_64-pc-windows-msvc/BUILDINFO.txt BUILDINFO-windows-x64.txt
   gh release create sidecars-ffmpeg-9.0.2 --repo arbaaz360/memories-rescue-releases --title "ffmpeg 9.0.2 sidecar (minimal LGPL build)" --notes "NovaByteWorks minimal LGPL v2.1+ build of FFmpeg 9.0.2 for Memories Rescue, with its complete corresponding source. See README.md and ffmpeg/SOURCE_OFFER.txt." ffmpeg-x86_64-pc-windows-msvc.exe BUILDINFO-windows-x64.txt _downloads/ffmpeg-9.0.2.tar.xz _downloads/ffmpeg-9.0.2.tar.xz.asc build.sh COPYING.LGPLv2.1
   ```

   `release.yml` verifies the download against the pinned SHA-256, so the
   file must be byte-identical. When the macOS binaries exist
   (`ffmpeg-universal-apple-darwin`, the two per-arch files,
   `BUILDINFO-macos.txt`), add them to the same release with
   `gh release upload` and fill the `universal-apple-darwin` pin.

## Checks and where they run

| Check | Where | Command |
|---|---|---|
| Versions agree, changelog entry, notices current, overlay parses | CI on every PR, release Checklist job | `python tools/release/check.py [--tag vX.Y.Z]` |
| Notices regenerated | locally after dependency changes | `python tools/release/notices.py` |
| Workflow syntax | CI `workflow lint` job | actionlint |
| Signatures and notarization | release build jobs after `tauri build` | `python tools/release/check.py --bundles --bundle-dir <dir>`; expected outputs in the script's help |
| Installed app answers with the engine, not the mock | manual, both platforms | see "Installed-app smoke test" below |

## Installed-app smoke test (Windows)

Run from PowerShell, not Git Bash: NSIS switches such as `/S` must reach the
installer unchanged, and `Start-Process -Wait` is what makes the silent
installer and uninstaller block until they are done.

```powershell
$setup = Get-Item "target
eleaseundle
sis\Memories Rescue_*_x64-setup.exe"
Start-Process -FilePath $setup.FullName -ArgumentList "/S" -Wait          # per-user install, no UAC
$inst = Join-Path $env:LOCALAPPDATA "Memories Rescue"
Get-ChildItem $inst                                                      # memories-rescue.exe, ffmpeg.exe, COPYING.LGPLv2.1, SOURCE_OFFER.txt, THIRD_PARTY_NOTICES.md
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS = "--remote-debugging-port=9517" # any free port; no other instance running
Start-Process -FilePath (Join-Path $inst "memories-rescue.exe")
node tools
elease\probe_engine.mjs 9517 welcome.png                   # "RESULT: engine answers"
powershell -File tools
eleaseerify_window.ps1 -OutPng about.png -Press About -Expect "Version,NovaByteWorks,Third-party notices" -Reject "Mock engine"
Stop-Process -Name memories-rescue -Force
Start-Process -FilePath (Join-Path $inst "uninstall.exe") -ArgumentList "/S" -Wait
Test-Path $inst                                                          # False
```

## Known gaps


- MSI is per-machine (Tauri's WiX template); the NSIS build is the per-user
  consumer download. A per-user MSI needs a custom `main.wxs`.
- No Mac was available while this was written: the macOS job, the
  universal sidecar naming and the notarization steps follow Tauri's
  documented behaviour and are unverified until the first run.
- GitHub Actions on this repository is blocked on billing; nothing above has
  run on a runner. Both workflows pass actionlint and every script ran locally.
