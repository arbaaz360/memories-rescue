# Code signing and notarization (C-08, §10.1): what to buy, what to flip, what cannot be tested here

Status: **decided on paper, not executed.** The M1 spike built and installed
an unsigned Windows build on the owner's Windows 11 machine. There is no Mac
on hand and no certificate of either kind yet. Certificates are on the
critical path (SRS §13, RISK-07); order both now.

## What to buy

| Platform | Buy | Cost and lead time | Why this one |
|---|---|---|---|
| macOS | **Apple Developer Program** membership (individual or organisation, NovaByteWorks). It grants the *Developer ID Application* certificate used to sign apps distributed outside the App Store, and access to the notary service. | US$99/year. Individual enrolment is usually same-day; organisation enrolment needs a D-U-N-S number and can take 1–2 weeks. | Gatekeeper on macOS 12+ refuses unsigned, un-notarized apps downloaded from the web with no override short of Terminal. There is no cheaper path. |
| Windows | An **OV code-signing certificate** from a CA that issues it on **cloud-held keys with a signing service**, so the key never touches the build machine: SSL.com eSigner, DigiCert KeyLocker, Sectigo via a cloud HSM partner, or GlobalSign. Alternative: an OV certificate issued to a **FIPS 140-2 Level 2 USB token**, which is cheaper but ties signing to whoever holds the token. **Azure Trusted Signing** (Microsoft, ~US$10/month, identity validation included) is the cheapest route and integrates with `signtool` through a dlib; it needs the organisation to have 3 years of verifiable history for the full-trust profile, which NovaByteWorks may not pass yet. | OV certificates: roughly US$200–400/year plus the cloud signing subscription (eSigner ~US$20/month). Identity validation: 1–3 weeks for a company, longer if registration documents are not in order. | Since June 2023 CA/Browser Forum rules require code-signing keys in hardware (HSM or token); a plain `.pfx` is no longer issued. OV is enough to sign; it does not clear SmartScreen reputation on day one (EV used to, and no longer does either), reputation accrues with downloads. |

**Recommendation:** Apple Developer Program now; for Windows, try Azure
Trusted Signing first (cheapest, no token logistics) and fall back to SSL.com
eSigner if the identity validation fails. Both support signing from a CI
runner with secrets, which is how the release pipeline (§10.5) should sign.

## Tauri configuration to flip

### Windows (`src-tauri/tauri.conf.json`, `bundle.windows`)

Option A, `signtool` with a certificate in the local store (token or
KeyLocker/eSigner client, both expose the key through CNG):

```json
"windows": {
  "certificateThumbprint": "<SHA-1 thumbprint of the OV certificate>",
  "digestAlgorithm": "sha256",
  "timestampUrl": "http://timestamp.digicert.com",
  "tsp": false
}
```

Option B (preferred for CI and for Azure Trusted Signing), a custom sign
command; Tauri runs it for the main exe, each sidecar and the installers:

```json
"windows": {
  "signCommand": {
    "cmd": "signtool",
    "args": ["sign", "/v", "/debug", "/fd", "SHA256", "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256",
             "/dlib", "Azure.CodeSigning.Dlib.dll", "/dmdf", "trusted-signing.json", "%1"]
  }
}
```

Secrets stay out of the config: Trusted Signing authenticates through
`DefaultAzureCredential` (environment variables on the runner); eSigner uses
`CodeSignTool` with username, password and TOTP secret from the environment.

Checks after a build: `signtool verify /pa /v <file>` on
`MemoriesRescue_<ver>_x64-setup.exe`, `MemoriesRescue_<ver>_x64.msi`,
`memories-rescue.exe`, `ffmpeg.exe`, `exiftool.exe`.

### macOS (`src-tauri/tauri.conf.json`, `bundle.macOS`)

```json
"macOS": {
  "signingIdentity": "Developer ID Application: NovaByteWorks (TEAMID)",
  "hardenedRuntime": true,
  "entitlements": "entitlements.plist",
  "minimumSystemVersion": "12.0"
}
```

and in the environment of the build:

```text
APPLE_CERTIFICATE / APPLE_CERTIFICATE_PASSWORD   base64 .p12 and its password (CI import)
APPLE_SIGNING_IDENTITY                           same string as signingIdentity
APPLE_ID, APPLE_PASSWORD (app-specific), APPLE_TEAM_ID
   or APPLE_API_ISSUER, APPLE_API_KEY, APPLE_API_KEY_PATH   (App Store Connect API key, better for CI)
```

With these set, `tauri build` signs the app, **signs each `externalBin`
sidecar and each `resources` executable with the same identity**, submits the
`.dmg` to the notary service, waits, and staples the ticket. No separate step is
needed for the sidecars as long as they are listed in `externalBin` or
`resources`; a binary copied in by any other route would be left unsigned and
would fail notarization.

`entitlements.plist` for a hardened-runtime app that runs child processes
and uses WKWebView:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>com.apple.security.cs.allow-jit</key><true/>
  <key>com.apple.security.cs.allow-unsigned-executable-memory</key><false/>
  <key>com.apple.security.cs.disable-library-validation</key><false/>
  <key>com.apple.security.files.user-selected.read-write</key><true/>
</dict></plist>
```

`allow-jit` is for WKWebView's JavaScript engine. Nothing else is needed:
the app is not sandboxed (§10.3, not App Store), so it may read and write any
path the user picks, and child processes inherit the hardened runtime.

## macOS steps that cannot be tested on this machine

1. Enrol, create the Developer ID Application certificate in Xcode or on
   developer.apple.com, export it as `.p12`.
2. Produce universal sidecars: `lipo -create ffmpeg-aarch64 ffmpeg-x86_64 -output ffmpeg-universal-apple-darwin`
   and name them for the `universal-apple-darwin` target (the triple names
   in `src-tauri/binaries/`). exiftool on macOS is the Perl script plus its
   `lib/` directory; ship it as a resource and run it with `/usr/bin/perl`
   (present on macOS 12–15), or ship a small Perl. Decide in M2 together
   with Q-02's choice.
3. **Sidecars must be signed with hardened runtime and a timestamp**
   (`codesign --force --options runtime --timestamp --sign "<identity>" ffmpeg`).
   Tauri does this; verify with `codesign -dvv --entitlements - <path>` on
   each binary inside `Memories Rescue.app/Contents/MacOS/`.
4. Notarize: `tauri build` does it when the Apple env vars are set; by hand
   it is `xcrun notarytool submit <dmg> --keychain-profile <p> --wait` then
   `xcrun stapler staple <dmg>`.
5. Verify on a clean Mac: download the `.dmg` through a browser (so it gets
   the quarantine attribute), `spctl -a -vv -t install "Memories Rescue.app"`
   must say `accepted, source=Notarized Developer ID`; launch; run a job so
   both sidecars start under Gatekeeper.
6. Known notarization rejections to pre-empt: an unsigned binary anywhere in
   the bundle (check `exiftool_files`-style directories, every `.dylib`),
   a sidecar without the hardened runtime flag, a missing secure timestamp,
   and `get-task-allow` entitlements left over from a debug build.

## Windows steps still to do once the certificate exists

1. Sign and time-stamp the three executables and both installers; the NSIS
   installer embeds the already-signed `memories-rescue.exe`, so signing
   order is handled by Tauri.
2. Submit the first signed installer to Microsoft through the SmartScreen
   report page to shorten the reputation period; expect the warning to
   persist for the first few hundred downloads regardless.
3. Smoke-install on clean Windows 10 22H2 and Windows 11 VMs (§10.5):
   per-user NSIS install without UAC prompt, WebView2 bootstrapper runs on a
   machine without the runtime, both version strings show in the window.

## What was verified in this spike (unsigned)

The NSIS and MSI bundles build with `externalBin` and `resources` set, the
installed app runs both sidecars and shows their versions, and the
installer is per-user for NSIS. The WiX template in Tauri 2.12 is
per-machine (`InstallScope="perMachine"`); a per-user MSI needs a custom
`main.wxs` (see `docs/decisions/Q-02-metadata-writer.md` for the spike's
notes on this and the PR description for the status).
