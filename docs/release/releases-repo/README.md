# Memories Rescue: downloads

Installers and update manifests for **Memories Rescue**, the desktop app by
[NovaByteWorks](https://novabyteworks.com) that repairs a Snapchat Memories
export on your own computer: it puts the capture dates and places back into
your photos and videos, adds the caption overlays, and writes a clean,
date-sorted library. Nothing leaves your computer.

The application's source code is not in this repository. This repository
holds the release files, their checksums, the update manifest the app reads,
and the source code of the one open-source program that ships inside the
app (FFmpeg, see below).

## Download

Go to [Releases](../../releases/latest) and pick:

| You use | Download |
|---|---|
| Windows 10 (22H2) or 11, 64-bit | `Memories Rescue_<version>_x64-setup.exe` (installs for your user, no administrator rights) |
| Windows, managed deployment | `Memories Rescue_<version>_x64_en-US.msi` (per machine) |
| macOS 12 or later, Apple silicon or Intel | `Memories Rescue_<version>_universal.dmg` |

Every file is listed in `SHA256SUMS.txt`. Windows installers are
Authenticode-signed by NovaByteWorks; the macOS app is signed with an Apple
Developer ID and notarized by Apple. If an installer is not signed, or its
checksum does not match, do not run it: tell us at
support@novabyteworks.com.

`latest.json` is the update manifest the installed app checks (daily, with
your consent to install). `latest-beta.json` is the beta channel.

## FFmpeg source code (LGPL)

Memories Rescue starts [FFmpeg](https://ffmpeg.org) as a separate program to
add overlays to videos, convert videos to H.264 and join clips. FFmpeg is
licensed under the GNU Lesser General Public License v2.1 or later
([COPYING.LGPLv2.1](ffmpeg/COPYING.LGPLv2.1)). We ship our own build of the
unmodified FFmpeg release with only the parts the app uses.

Each release carries, as assets next to the installers, the complete
corresponding source: the upstream tarball `ffmpeg-<version>.tar.xz` and its
signature, `build.sh` (the script that builds it, including the exact
configure options and the pinned toolchain), and `BUILDINFO-*.txt` (the
compiler and library versions used). You may replace the `ffmpeg` program
inside the application folder with your own build; see
[ffmpeg/SOURCE_OFFER.txt](ffmpeg/SOURCE_OFFER.txt) for the written offer.

Other third-party components and their licences are listed in the
`THIRD_PARTY_NOTICES.md` installed with the app and shown from its About
screen.

## Privacy

The app sends nothing anywhere. The only network calls it makes are the
update check against this repository and, if you buy a licence, the licence
check with Gumroad. See the privacy policy at
<https://novabyteworks.com/memories-rescue/privacy>.

## Trademarks

Not affiliated with, endorsed by, or sponsored by Snap Inc. "Snapchat" and
"Memories" are trademarks of Snap Inc., used only to describe compatibility.
FFmpeg is a trademark of Fabrice Bellard, originator of the FFmpeg project.
