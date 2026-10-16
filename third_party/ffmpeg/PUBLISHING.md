# Publishing the ffmpeg sidecar with a release (LGPL v2.1+)

What every Memories Rescue release that ships the minimal ffmpeg must do, so
the LGPL obligations are met. Proposed public repository:
`arbaaz360/memories-rescue-releases` (not created yet).

## In the installer and app

| What | Where | Why |
|---|---|---|
| `COPYING.LGPLv2.1` (this folder) | next to `ffmpeg(.exe)` in the install folder | LGPL 2.1 §6: a copy of the license with the work |
| `SOURCE_OFFER.txt` (this folder) | next to `ffmpeg(.exe)`, and its text on the About screen | where to get the source; written offer as a fallback (§6(c)) |
| `dist/<triple>/licenses/` from the build (zlib, mingw-w64 runtime notices; Windows only) | next to `ffmpeg.exe` | the Windows binary links zlib and winpthreads statically; their licenses ask for the notice in copies |
| ffmpeg as a separate executable | install folder, launched by the app | §6: the user can replace it; nothing of FFmpeg is linked into the app |

The app must not stop the user from replacing `ffmpeg(.exe)` (no checksum
lock on it). A replaced ffmpeg that lacks an encoder simply leads to
`ENCODER_UNAVAILABLE`, which the app already handles.

## In `arbaaz360/memories-rescue-releases`, per release

```text
<release tag>/ffmpeg/
  ffmpeg-9.0.2.tar.xz          the exact upstream source tarball (unmodified)
  ffmpeg-9.0.2.tar.xz.asc      FFmpeg's signature for it
  build.sh                     this folder's build script, as used for the release
  BUILDINFO-windows-x64.txt    from dist/x86_64-pc-windows-msvc/ (configure line, toolchain, SHA-256)
  BUILDINFO-macos.txt          from dist/universal-apple-darwin/
  COPYING.LGPLv2.1
  README.md                    this folder's README
```

LGPL 2.1 §0 defines the complete source as "all the source code for all
modules it contains, plus any associated interface definition files, plus
the scripts used to control compilation and installation". The tarball is
the source (no patches are applied), `build.sh` and the configure line in
`BUILDINFO` are the scripts. The pinned Docker image digest in `build.sh` and
the package versions in `BUILDINFO` let anyone rebuild the Windows binary.

Publishing these next to the release downloads is "equivalent access to copy
the above specified materials from the same place" (§6(d)); the written
offer in `SOURCE_OFFER.txt` covers anyone who got the installer elsewhere.

## Before each release

1. `./build.sh docker-windows` (and `macos-universal` on a Mac) from a clean checkout.
2. `python tools/sidecars.py stage-local`, then the media tests (see README).
3. Copy the files above into the releases repository under the release tag.
4. Check that the About screen's source link points at that tag.
5. If the FFmpeg version changes, update `SOURCE_OFFER.txt` (version and SHA-256).
