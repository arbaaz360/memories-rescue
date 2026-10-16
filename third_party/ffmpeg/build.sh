#!/usr/bin/env bash
# NovaByteWorks' minimal LGPL ffmpeg for Memories Rescue (SRS v0.2 section 10.4).
#
#   ./build.sh fetch              download and verify the pinned FFmpeg source
#   ./build.sh docker-windows     Windows x64 in a pinned Debian container (any host with Docker)
#   ./build.sh windows-x64        Windows x64 with a mingw-w64 cross toolchain on PATH
#   ./build.sh macos-universal    macOS arm64 + x86_64, joined with lipo (on a Mac with Xcode CLT)
#   ./build.sh configure-line     print the configure flags for each target
#
# Output: dist/<target triple>/ffmpeg[.exe] plus BUILDINFO.txt (configure
# line, toolchain versions, sizes, SHA-256). Everything downloaded lands in
# _downloads/, everything built in _build/; both are gitignored.
#
# The build is LGPL v2.1 or later: no --enable-gpl, no --enable-nonfree, and
# no --enable-version3, because no enabled component needs (L)GPL v3.
# ffmpeg runs as a separate executable (a Tauri sidecar), never linked into
# the app, so a static ffmpeg.exe is fine under the LGPL: the user can
# replace the whole executable with one built from the published source.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$HERE"

FFMPEG_VERSION="9.0.2"                     # upstream tag n9.0.2
FFMPEG_TARBALL="ffmpeg-${FFMPEG_VERSION}.tar.xz"
FFMPEG_URL="https://ffmpeg.org/releases/${FFMPEG_TARBALL}"
FFMPEG_SHA256="8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e"
FFMPEG_SIZE=12040788
# FFmpeg release signing key <ffmpeg-devel@ffmpeg.org>
FFMPEG_KEY_FPR="FCF986EA15E6E293A5644F10B4322F04D67658D8"

# Debian 13 (trixie) slim, pinned by digest; packages installed in it are
# recorded in BUILDINFO.txt.
DOCKER_IMAGE="debian@sha256:a29215f6a35e51e22adffa17f89e9d2ef06214e64a2bad10d765c46aea49f11f"
DOCKER_PACKAGES="ca-certificates curl xz-utils make nasm pkg-config gcc libc6-dev gcc-mingw-w64-x86-64-posix g++-mingw-w64-x86-64-posix binutils-mingw-w64-x86-64 libz-mingw-w64-dev"

MACOS_MIN="11.0"

DOWNLOADS="$HERE/_downloads"
BUILD="${MR_FFMPEG_BUILD_DIR:-$HERE/_build}"   # the Docker build uses the container's own disk
DIST="$HERE/dist"

# ---------------------------------------------------------------------------
# Exactly what crates/mr-composite runs, and what its tests run to check it
# (see README.md "What is in the build" for where each one is used).
# ---------------------------------------------------------------------------
# Component names as configure knows them: `-f png_pipe` is image_png_pipe,
# `-f s16le` is pcm_s16le. A name configure does not know is skipped with
# only a warning, so check_components() verifies the result.
DEMUXERS="mov,concat,image2,image_png_pipe"
MUXERS="mp4,mov,image2,pcm_s16le"
DECODERS="h264,hevc,aac,png,mjpeg"
ENCODERS="aac,png,mjpeg,pcm_s16le"
PARSERS="h264,hevc,aac,png,mjpeg"
BSFS="h264_mp4toannexb,hevc_mp4toannexb,aac_adtstoasc,extract_extradata"
# buffer, buffersink, abuffer and abuffersink are always built (libavfilter
# API, not configurable components); they carry every graph in and out.
FILTERS="null,anull,format,aformat,scale,transpose,overlay,aresample,trim,atrim,fps"
PROTOCOLS="file,pipe"

common_flags() {
  printf '%s\n' \
    --disable-everything \
    --disable-autodetect \
    --disable-doc \
    --disable-debug \
    --disable-network \
    --disable-avdevice \
    --disable-ffplay \
    --disable-ffprobe \
    --enable-ffmpeg \
    --enable-avcodec --enable-avformat --enable-avfilter --enable-swscale --enable-swresample \
    --enable-static --disable-shared \
    --enable-zlib \
    "--enable-protocol=${PROTOCOLS}" \
    "--enable-demuxer=${DEMUXERS}" \
    "--enable-muxer=${MUXERS}" \
    "--enable-decoder=${DECODERS}" \
    "--enable-parser=${PARSERS}" \
    "--enable-bsf=${BSFS}" \
    "--enable-filter=${FILTERS}"
}

# --enable-d3d11va: libavcodec/mfenc.c in 9.0.2 uses ID3D11Texture2D and
# AVD3D11VADeviceContext outside its CONFIG_D3D11VA guard, so h264_mf does
# not compile without it. It enables the D3D11 API support only; no hardware
# decoders (hwaccels stay disabled by --disable-everything).
windows_flags() {
  common_flags
  printf '%s\n' \
    --target-os=mingw32 --arch=x86_64 --enable-cross-compile \
    --cross-prefix=x86_64-w64-mingw32- \
    --pkg-config=pkg-config --pkg-config-flags=--static \
    --extra-ldflags=-static \
    --enable-w32threads \
    --enable-mediafoundation \
    --enable-d3d11va \
    "--enable-encoder=${ENCODERS},h264_mf,hevc_mf"
}

macos_flags() { # $1 = arm64 | x86_64
  common_flags
  printf '%s\n' \
    --enable-cross-compile --target-os=darwin "--arch=$1" \
    "--cc=clang -arch $1" \
    "--extra-cflags=-mmacosx-version-min=${MACOS_MIN}" \
    "--extra-ldflags=-mmacosx-version-min=${MACOS_MIN}" \
    --enable-pthreads \
    --enable-videotoolbox \
    "--enable-encoder=${ENCODERS},h264_videotoolbox,hevc_videotoolbox"
}

# Read one flag per line into the array named $1 (bash 3.2 on macOS has no mapfile).
read_lines() {
  local __name="$1" __line
  eval "$__name=()"
  while IFS= read -r __line; do eval "$__name+=(\"\$__line\")"; done
}

# Fail unless every requested component is enabled in the configured tree
# ($1): configure ignores unknown names with a warning only.
check_components() { # $1 = configured build dir, $2 = platform encoders (comma list)
  local h="$1/config_components.h" missing=0 pair kind var name macro
  local PLATFORM_ENCODERS="$2"
  for pair in demuxer:DEMUXERS muxer:MUXERS decoder:DECODERS encoder:ENCODERS \
              encoder:PLATFORM_ENCODERS parser:PARSERS bsf:BSFS filter:FILTERS protocol:PROTOCOLS; do
    kind="${pair%%:*}"
    var="${pair##*:}"
    for name in ${!var//,/ }; do
      macro="CONFIG_$(printf '%s_%s' "$name" "$kind" | tr '[:lower:]' '[:upper:]')"
      if ! grep -q "^#define ${macro} 1" "$h"; then
        echo "missing component: $name ($kind)" >&2
        missing=1
      fi
    done
  done
  if [[ $missing != 0 ]]; then
    echo "configure did not enable everything asked for" >&2
    exit 1
  fi
  echo "all requested components are enabled"
}

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

fetch() {
  mkdir -p "$DOWNLOADS"
  local tb="$DOWNLOADS/$FFMPEG_TARBALL"
  if [[ ! -f "$tb" || "$(sha256 "$tb")" != "$FFMPEG_SHA256" ]]; then
    echo "downloading $FFMPEG_URL"
    curl -sSfL "$FFMPEG_URL" -o "$tb.part"
    mv "$tb.part" "$tb"
  fi
  local got; got="$(sha256 "$tb")"
  if [[ "$got" != "$FFMPEG_SHA256" ]]; then
    echo "SHA-256 mismatch for $FFMPEG_TARBALL: $got" >&2
    exit 1
  fi
  echo "$FFMPEG_TARBALL: sha256 ok"
  # The signature is checked when gpg is available; the pinned SHA-256 is
  # the requirement, the signature is how the pin was established.
  if command -v gpg >/dev/null; then
    curl -sSfL "$FFMPEG_URL.asc" -o "$DOWNLOADS/$FFMPEG_TARBALL.asc"
    curl -sSfL https://ffmpeg.org/ffmpeg-devel.asc -o "$DOWNLOADS/ffmpeg-devel.asc"
    local gh; gh="$(mktemp -d)"
    GNUPGHOME="$gh" gpg --batch --quiet --import "$DOWNLOADS/ffmpeg-devel.asc"
    if GNUPGHOME="$gh" gpg --batch --status-fd 1 --verify "$DOWNLOADS/$FFMPEG_TARBALL.asc" "$tb" 2>/dev/null \
        | grep -q "VALIDSIG $FFMPEG_KEY_FPR"; then
      echo "$FFMPEG_TARBALL: signature by $FFMPEG_KEY_FPR ok"
    else
      echo "signature check failed for $FFMPEG_TARBALL" >&2
      exit 1
    fi
    rm -rf "$gh"
  fi
}

unpack() {
  local src="$BUILD/ffmpeg-${FFMPEG_VERSION}"
  if [[ ! -f "$src/configure" ]]; then
    mkdir -p "$BUILD"
    tar -xJf "$DOWNLOADS/$FFMPEG_TARBALL" -C "$BUILD"
  fi
  echo "$src"
}

record() { # $1 = target dir, $2 = binary, $3.. = configure flags
  local dir="$1" bin="$2"; shift 2
  {
    echo "FFmpeg ${FFMPEG_VERSION} (${FFMPEG_TARBALL}, sha256 ${FFMPEG_SHA256})"
    echo "built $(date -u +%Y-%m-%dT%H:%M:%SZ) by third_party/ffmpeg/build.sh"
    echo
    echo "configure:"
    printf '  %q\n' "$@"
    echo
    echo "binary: $(basename "$bin")  $(wc -c <"$bin" | tr -d ' ') bytes  sha256 $(sha256 "$bin")"
    echo
    echo "toolchain:"
    "${TOOLCHAIN_INFO[@]}" 2>&1 | sed 's/^/  /'
  } >"$dir/BUILDINFO.txt"
  cat "$dir/BUILDINFO.txt"
}

windows_x64() {
  fetch
  local src; src="$(unpack)"
  local out="$BUILD/win64" dist="$DIST/x86_64-pc-windows-msvc"
  mkdir -p "$out" "$dist"
  local flags; read_lines flags < <(windows_flags)
  (cd "$out" && "$src/configure" "${flags[@]}")
  check_components "$out" "h264_mf,hevc_mf"
  (cd "$out" && make -j"$(nproc)")
  x86_64-w64-mingw32-strip -o "$dist/ffmpeg.exe" "$out/ffmpeg.exe"
  # Notices to ship next to ffmpeg.exe: FFmpeg's, and those of the static
  # runtime pieces (zlib; mingw-w64's winpthreads and CRT).
  mkdir -p "$dist/licenses"
  cp "$src/COPYING.LGPLv2.1" "$dist/licenses/COPYING.LGPLv2.1"
  cp "$src/LICENSE.md" "$dist/licenses/FFmpeg-LICENSE.md"
  local f
  for f in libz-mingw-w64-dev:zlib mingw-w64-x86-64-dev:mingw-w64-runtime; do
    if [[ -f "/usr/share/doc/${f%%:*}/copyright" ]]; then
      cp "/usr/share/doc/${f%%:*}/copyright" "$dist/licenses/${f##*:}.txt"
    fi
  done
  TOOLCHAIN_INFO=(bash -c 'x86_64-w64-mingw32-gcc --version | head -1; nasm -v; echo "imports:"; x86_64-w64-mingw32-objdump -p '"$dist"'/ffmpeg.exe | grep "DLL Name" | sort -u; if command -v dpkg-query >/dev/null; then dpkg-query -W gcc-mingw-w64-x86-64-posix libz-mingw-w64-dev nasm binutils-mingw-w64-x86-64; fi')
  record "$dist" "$dist/ffmpeg.exe" "${flags[@]}"
}

docker_windows() {
  # Run windows-x64 inside the pinned container; the source tree is this folder.
  local mount="$HERE"
  if command -v cygpath >/dev/null; then mount="$(cygpath -w "$HERE")"; fi
  MSYS_NO_PATHCONV=1 docker run --rm -v "${mount}:/work" -w /work \
    -e MR_FFMPEG_BUILD_DIR=/tmp/build "$DOCKER_IMAGE" bash -c "
    set -e
    apt-get update -qq
    DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends $DOCKER_PACKAGES >/dev/null
    bash ./build.sh windows-x64
  "
}

macos_universal() {
  # UNVERIFIED: written without a Mac to run it on. Needs Xcode command line
  # tools (clang, lipo) and nasm for the x86_64 slice (brew install nasm).
  fetch
  local src; src="$(unpack)"
  local dist="$DIST/universal-apple-darwin"
  mkdir -p "$dist"
  local arch
  for arch in arm64 x86_64; do
    local out="$BUILD/macos-$arch"
    mkdir -p "$out"
    local flags; read_lines flags < <(macos_flags "$arch")
    (cd "$out" && "$src/configure" "${flags[@]}")
    check_components "$out" "h264_videotoolbox,hevc_videotoolbox"
    (cd "$out" && make -j"$(sysctl -n hw.ncpu)")
    cp "$out/ffmpeg" "$dist/ffmpeg-$arch"
    strip "$dist/ffmpeg-$arch"
  done
  lipo -create -output "$dist/ffmpeg" "$dist/ffmpeg-arm64" "$dist/ffmpeg-x86_64"
  lipo -info "$dist/ffmpeg"
  # Tauri's per-arch sidecar names, for builds that are not universal.
  cp "$dist/ffmpeg-arm64" "$DIST/ffmpeg-aarch64-apple-darwin"
  cp "$dist/ffmpeg-x86_64" "$DIST/ffmpeg-x86_64-apple-darwin"
  TOOLCHAIN_INFO=(bash -c 'clang --version | head -1; xcrun --show-sdk-version; nasm -v || true; otool -L '"$dist"'/ffmpeg-arm64')
  read_lines flags < <(macos_flags arm64)
  record "$dist" "$dist/ffmpeg" "${flags[@]}"
}

case "${1:-}" in
  fetch) fetch ;;
  windows-x64) windows_x64 ;;
  docker-windows) docker_windows ;;
  macos-universal) macos_universal ;;
  configure-line)
    echo "# Windows x64"; windows_flags
    echo; echo "# macOS (per arch; arm64 shown)"; macos_flags arm64 ;;
  *)
    sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'
    exit 2 ;;
esac
