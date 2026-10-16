# third_party/ffmpeg: the minimal LGPL ffmpeg sidecar (SRS §10.4)

NovaByteWorks' own ffmpeg for Memories Rescue: FFmpeg 9.0.2 configured with
`--disable-everything` and only the components the app's media code and its
tests use. It replaces the 134 MB BtbN development build in releases.

| | |
|---|---|
| Upstream | FFmpeg **9.0.2** (tag `n9.0.2`), <https://ffmpeg.org/releases/ffmpeg-9.0.2.tar.xz> |
| Tarball | 12,040,788 bytes, SHA-256 `8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e` |
| Signature | `ffmpeg-9.0.2.tar.xz.asc`, good signature by the FFmpeg release signing key `FCF9 86EA 15E6 E293 A564 4F10 B432 2F04 D676 58D8` (checked 2026-10-16) |
| Patches | none |
| License of the build | **LGPL v2.1 or later**: no `--enable-gpl`, no `--enable-nonfree`, no `--enable-version3` |
| Windows x64 | WINDOWS_SIZE |
| macOS universal | recipe in `build.sh`, **unverified** (no Mac available; CI blocked) |

## Files

| File | Tracked | What |
|---|---|---|
| `build.sh` | yes | download, verify, configure, build; Windows in Docker or with mingw-w64, macOS with Xcode tools |
| `README.md` | yes | this file |
| `COPYING.LGPLv2.1` | yes | the license text to ship next to `ffmpeg(.exe)` (from the tarball) |
| `LICENSE-ffmpeg.md` | yes | FFmpeg's own `LICENSE.md` from the tarball (which parts are LGPL/GPL) |
| `SOURCE_OFFER.txt` | yes | the notice and written offer for the install folder and the About screen |
| `PUBLISHING.md` | yes | what each release must publish in `arbaaz360/memories-rescue-releases` |
| `_downloads/` | no | the tarball and its signature |
| `_build/` | no | build trees (the Docker build uses the container's disk instead) |
| `dist/<triple>/` | no | `ffmpeg(.exe)`, `BUILDINFO.txt`, `licenses/` |
| `ffmpeg.exe`, `ffprobe.exe`, `LICENSE.txt`, `STAGED.json` | no | whatever `tools/sidecars.py` staged (BtbN or this build) |

## Build

```bash
cd third_party/ffmpeg
./build.sh docker-windows      # any host with Docker: pinned Debian 13 + mingw-w64 (what was used)
./build.sh windows-x64         # with x86_64-w64-mingw32-gcc, nasm, mingw zlib on PATH
./build.sh macos-universal     # on a Mac: Xcode command line tools, nasm (brew)
./build.sh configure-line      # print the configure flags
```

Then stage it for the app and the tests:

```bash
python tools/sidecars.py stage-local
```

`docker-windows` runs in `debian@sha256:a29215f6…f11f` (Debian 13.7) with
`gcc-mingw-w64-x86-64-posix` 14.2, `libz-mingw-w64-dev` 1.3.1 and `nasm`
2.16.03; the exact package versions land in `dist/…/BUILDINFO.txt`. The
macOS recipe builds arm64 and x86_64 separately (`--arch`, `clang -arch`,
deployment target 11.0, VideoToolbox) and joins them with `lipo`; it also
leaves Tauri's per-arch names `ffmpeg-aarch64-apple-darwin` and
`ffmpeg-x86_64-apple-darwin` in `dist/`.

## The configure line

Shared by both platforms:

```text
--disable-everything --disable-autodetect --disable-doc --disable-debug
--disable-network --disable-avdevice --disable-ffplay --disable-ffprobe --enable-ffmpeg
--enable-avcodec --enable-avformat --enable-avfilter --enable-swscale --enable-swresample
--enable-static --disable-shared --enable-zlib
--enable-protocol=file,pipe
--enable-demuxer=mov,concat,image2,image_png_pipe
--enable-muxer=mp4,mov,image2,pcm_s16le
--enable-decoder=h264,hevc,aac,png,mjpeg
--enable-parser=h264,hevc,aac,png,mjpeg
--enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc,extract_extradata
--enable-filter=null,anull,format,aformat,scale,transpose,overlay,aresample,trim,atrim,fps
```

Windows x64 adds:

```text
--target-os=mingw32 --arch=x86_64 --enable-cross-compile --cross-prefix=x86_64-w64-mingw32-
--pkg-config=pkg-config --pkg-config-flags=--static --extra-ldflags=-static
--enable-w32threads --enable-mediafoundation --enable-d3d11va
--enable-encoder=aac,png,mjpeg,pcm_s16le,h264_mf,hevc_mf
```

macOS (per arch) adds:

```text
--enable-cross-compile --target-os=darwin --arch=<arm64|x86_64> --cc="clang -arch <arch>"
--extra-cflags=-mmacosx-version-min=11.0 --extra-ldflags=-mmacosx-version-min=11.0
--enable-pthreads --enable-videotoolbox
--enable-encoder=aac,png,mjpeg,pcm_s16le,h264_videotoolbox,hevc_videotoolbox
```

`--disable-autodetect` turns off every library configure would otherwise
pick up from the build machine (so the result does not depend on it), and
with it the thread libraries: `--enable-w32threads` / `--enable-pthreads`
are therefore explicit. Configure names differ from `-f` names in two
places (`image_png_pipe`, `pcm_s16le`) and an unknown name is only a
warning, so `build.sh` checks `config_components.h` after configure and
stops if anything asked for is missing. `--enable-d3d11va` on Windows is
there because `libavcodec/mfenc.c` in 9.0.2 uses D3D11 types outside its
`CONFIG_D3D11VA` guard; it adds the D3D11 API support, no hardware decoders. The `ffmpeg` program itself pulls in a few filters
of its own (`crop`, `hflip`, `vflip`) through configure's `ffmpeg_select`.

No component needs `--enable-version3`: Media Foundation, VideoToolbox and
zlib are not on configure's v3 list, so the build stays LGPL **v2.1+**
(the BtbN build is v3 only because it enables v3-only libraries).

## What is in the build, and why

Found by running every command shape of `crates/mr-composite` and its tests
through the BtbN build at `-v debug` and reading which formats, codecs and
filters it opened, then building and running the whole media test suite
against this binary.

| Component | Used by |
|---|---|
| demuxer `mov` | every video input (`.mp4`, `.mov`) |
| demuxer `image_png_pipe` (`-f png_pipe`) | overlay PNGs and the encoder smoke test's PNG (ffmpeg probes `.png` files as `png_pipe`; `-loop 1` works on it) |
| demuxer `image2` | explicit `-f image2` inputs; kept for image sequences |
| demuxer `concat` | clip rejoin (`-f concat -safe 0 -i list.ffconcat`) |
| muxers `mp4`, `mov` | every output (by extension) |
| muxer `image2` | tests: frame extraction to PNG |
| muxer `pcm_s16le` (`-f s16le`) | tests: audio levels at the joins (`-f s16le pipe:1`) |
| decoders `h264`, `hevc` | sources, including 10-bit HEVC and 4:4:4 H.264 |
| decoder `aac` | audio re-encode (rejoin, safe retry) and tests |
| decoder `png` | overlays |
| decoder `mjpeg`, encoders `png`, `mjpeg` | SRS §10.4 list; `png` also for test frames |
| encoders `h264_mf`, `hevc_mf` (Windows), `h264_videotoolbox`, `hevc_videotoolbox` (macOS) | video output (H.264 by default) |
| encoder `aac` | audio that cannot be stream-copied; every rejoin |
| encoder `pcm_s16le` | tests: audio levels |
| filters `transpose`, `scale`, `overlay`, `format` | the compositing graphs |
| filter `aresample` | inserted by ffmpeg for `-ar`/`-ac` and sample-format changes |
| filters `null`, `anull`, `aformat`, `trim`, `atrim`, `fps` (and the always-built `buffer`, `buffersink`, `abuffer`, `abuffersink`) | the filtergraph plumbing ffmpeg inserts around every graph, `-t`/`-ss` on inputs, output frame-rate handling |
| parsers, bitstream filters | stream copy and concat across segments |
| protocols `file`, `pipe` | paths and `pipe:1` |

No ffprobe (the app reads media natively, `mr_composite::probe`), no
network, no devices, no OpenH264 (Q-04, provisionally closed 2026-10-16: the
software `h264_mf` at 1.3× the source bitrate instead).

## Results

RESULTS

## Notes

- The Windows binary is one static `ffmpeg.exe` (libgcc, winpthreads and
  zlib linked in). That is allowed: the LGPL asks that the user can replace
  the FFmpeg part, and the whole executable is replaceable because the app
  only launches it. Media Foundation is loaded at run time by FFmpeg, so
  `ffmpeg.exe` starts on Windows N without the Media Feature Pack and just
  reports no working H.264 encoder (`ENCODER_UNAVAILABLE`).
- BtbN's build stays the development default (`python tools/sidecars.py fetch`);
  it carries `ffprobe.exe`, which a few `mr-meta` read-back tests use as an
  oracle. `stage-local` replaces only `ffmpeg`.
