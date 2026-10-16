# mr-composite: media probe, overlays, video compositing, clip rejoin

| | |
|---|---|
| Crate | `crates/mr-composite` |
| SRS | §4.4 video compositing, §4.5 clip rejoin, FR-23, FR-24, FR-27, FR-51, §8 error codes, §11.2 F-07/F-08/F-09 |
| Callers | the job engine (M2 core) through its Compositor seam; the UI only for FR-51 |
| Needs | the ffmpeg sidecar for video; nothing external for photos and the probe |

The crate answers four questions for the engine: what is this file
(`probe`), put the overlay on this photo (`composite_photo`), put the
overlay on this video or make it H.264 (`Compositor`), and which clips
belong together and how to join them (`detect_clip_groups`,
`Compositor::rejoin`). It writes media only. Capture metadata is written
afterwards by the engine with `mr-meta` (§4.3), so every output here is
deliberately bare: no EXIF, no QuickTime tags.

```text
 job engine ──► probe(path) ─────────────────────► MediaInfo        (native, no sidecar)
     │
     ├──────► composite_photo(main, overlay, out) ► PhotoOutcome    (image crate)
     │
     ├──────► Compositor::composite_video / transcode_h264 / rejoin
     │            │  planners build one argument array (video::*, rejoin::*)
     │            │  encoder plan: listing + smoke encode, cached once
     │            └─► ffmpeg sidecar, timeout, one retry on the software path
     │
     ├──────► detect_clip_groups(descriptors, options) ► Vec<ClipGroup>
     └──────► hevc_playback_available()  (FR-51 notice, Windows)
```

## Quick start

```rust
use mr_composite::{
    composite_photo, detect_clip_groups, probe, ClipDescriptor, Compositor,
    PhotoOptions, RejoinOptions, Segment,
};

// One per job; share it by reference across worker threads.
let compositor = Compositor::new(sidecar_ffmpeg_path);
match compositor.encoder_plan() {
    Ok(plan) => log::info!("video encoders: {plan:?}"),
    // ENCODER_UNAVAILABLE: turn video compositing off, keep fixing metadata.
    Err(e) => report.issue(e.code(), e.to_string()),
}

// FR-23
let photo = composite_photo(&main_jpg, &overlay_png, &out, &PhotoOptions::default())?;
// photo.format says JPEG or PNG; name the file accordingly, then write EXIF.

// FR-24
let video = compositor.composite_video(&main_mp4, &overlay_png, &out_mp4)?;
for w in &video.warnings { report.warn(w.code(), w); }   // overlay_aspect_mismatch, ...

// Windows compatibility mode (HEVC source, no overlay)
compositor.transcode_h264(&hevc_mp4, &out_mp4)?;

// FR-27
let descriptors: Vec<ClipDescriptor> = videos
    .iter()
    .filter_map(|v| ClipDescriptor::from_probe(&v.id, v.date_utc, v.location, &probe(&v.path).ok()?))
    .collect();
for group in detect_clip_groups(&descriptors, &RejoinOptions::default()) {
    // dry run: show group.item_ids, group.total_secs; the user may untick (FR-27 AC3)
    let segments: Vec<Segment> = group.item_ids.iter().map(|id| Segment {
        path: extracted_main(id),
        overlay: burn_in.then(|| extracted_overlay(id)).flatten(),
    }).collect();
    compositor.rejoin(&segments, &joined_mp4)?;   // originals are kept by the engine
}
```

### In the engine

The job engine uses this crate through `mr_job::MediaCompositor`
(`crates/mr-job/src/media.rs`), which implements the engine's
`Compositor` seam: `composite` calls `Compositor::composite_any` (photo or
video by content), `compat_transcode` calls `transcode_h264` for HEVC
videos when "Maximise Windows compatibility" is on, and `rejoin` calls
`Compositor::rejoin` for each clip group the dry run found with
`detect_clip_groups` (the dry run probes videos inside the ZIP with
`probe_reader`). Warnings travel back as `CompositedWithWarnings` and reach
the report; `ENCODER_UNAVAILABLE` declines video work instead of failing
items. `docs/api/mr-job.md` ("The compositor seam") has the engine side, and
`crates/mr-job/tests/m3.rs` runs it on F-07, F-08 and F-09.

Every operation writes to `.<name>.mr-partial(.<ext>)` next to the output
and renames it into place on success, so a crash never leaves a
half-written file under the final name; on failure the partial file is
removed.

## API

### `probe(path) -> Result<MediaInfo, Error>`

Reads the file by content, not by extension. MP4/MOV are read box by box
(only `moov` is loaded, tens of KB to a few MB for a phone clip); JPEG and
PNG by their segments and chunks. No ffprobe: it is not shipped (§10.4), and
ffmpeg's own `-i` listing is used only as a test oracle.

| `MediaInfo` field | Source |
|---|---|
| `container` | `Mp4`, `Mov` (major brand `qt  `), `Jpeg`, `Png` |
| `brand` | `ftyp` major brand |
| `duration_secs` | `mvhd` duration / timescale |
| `bitrate_kbps` | file size × 8 / duration |
| `video.codec` | first `stsd` entry fourcc: `avc1`, `avc3`, `hvc1`, `hev1`, `mp4v`, ... |
| `video.coded_width/height` | visual sample entry |
| `video.rotation` | clockwise display rotation from the `tkhd` matrix, `atan2(b, a)` snapped to 0/90/180/270; the old `rotate` tag value |
| `video.display_size()` | coded size, axes swapped for 90/270 |
| `video.frame_rate` | `timescale / delta` when `stts` is constant (a single short last sample is ignored), else frames / duration |
| `video.variable_frame_rate` | more than one sample duration in `stts` |
| `video.bit_depth`, `profile` | `hvcC` (`bitDepthLumaMinus8`), `avcC` high-profile extension, else the H.264 SPS |
| `video.presented_secs`, `edit_delay_secs`, `edit_skip_secs` | `elst`: sum of non-empty edits, leading empty edit, media start of the first edit |
| `video.config_hash` | FNV-1a of `avcC`/`hvcC`; equal hashes allow a stream-copy join |
| `video.bytes`, `bitrate_kbps` | sum of `stsz` / media duration |
| `audio.codec` | `aac`/`mp3` from the `esds` object type, `ac3`, `eac3`, `alac`, `opus`, `flac`, else the fourcc |
| `audio.channels`, `sample_rate`, `sample_count`, `presented_secs` | sound sample entry (v0, v1, v2), `stsz`, `elst` |
| `image.width/height/orientation` | JPEG `SOFn` and APP1 EXIF IFD0 tag 0x0112; PNG `IHDR` and `eXIf` |

`MediaInfo` is `Serialize`; `cargo run -p mr-composite --example probe -- <file>` prints it.

`probe_reader(reader, len, limit)` reads the same facts from a forward-only
stream, such as an entry being read out of a ZIP: boxes before `moov` are
skipped by reading, reading stops once `moov` is parsed, and more than
`limit` bytes before `moov` (a file with `moov` after a large `mdat`) gives
`Unsupported` instead of reading the whole file. The engine's dry run uses it
so that nothing is extracted to find clip groups or HEVC videos.

### `composite_photo(main, overlay, out, &PhotoOptions) -> Result<PhotoOutcome, Error>`

1. Decode the photo (`image` crate: zune-jpeg, png), apply its EXIF
   orientation (1 to 8) so the pixels are upright.
2. Decode the overlay to RGBA and place it on the upright size with the
   §4.4 rule shared with video (`place_overlay`): stretched when the
   aspects agree within 2%, otherwise fitted to the width and centred
   vertically, with an `overlay_aspect_mismatch` warning. Resampling is
   bilinear (`FilterType::Triangle`).
3. Straight-alpha "over" onto the photo (integer, rounded; PNG sources with
   alpha get full Porter-Duff).
4. JPEG sources are written as JPEG at `jpeg_quality` (default 92); PNG
   sources stay PNG (alpha kept if the source had it). The format is chosen
   by the source, not by `out`'s extension; `PhotoOutcome::format` tells the
   engine which extension to use.

The output has no EXIF at all. Its pixels are upright, so **the metadata
writer must not copy the source's Orientation tag**.

| `PhotoOptions` | Default |
|---|---|
| `jpeg_quality` | 92 |
| `aspect_tolerance` | 0.02 |

### `Compositor`

`Compositor::new(ffmpeg)` or `Compositor::with_options(ffmpeg, VideoOptions)`.
`Sync`: one per job, shared by reference.

| Method | What it does |
|---|---|
| `encoders()` | `ffmpeg -hide_banner -encoders`, parsed (`EncoderSet`), run once and cached |
| `encoder_plan()` | the §4.4 order filtered by the listing, each candidate smoke-tested once; `EncoderPlan { primary, software }` or `ENCODER_UNAVAILABLE` |
| `composite_any(main, overlay, out)` | the engine's seam: photo or video by content; returns `Composite::Photo(PhotoOutcome)` or `Composite::Video(VideoOutcome)`; photo settings from `with_photo_options` |
| `composite_video(main, overlay, out)` | FR-24 |
| `transcode_h264(main, out)` | the Windows compatibility mode: H.264, rotation baked in, no overlay |
| `rejoin(segments, out)` | FR-27 join of two or more segments in order |

All three return `VideoOutcome { encoder, retried, first_failure,
output_size, bitrate_kbps, audio, warnings, elapsed, args }`. `args` is the
ffmpeg command that produced the output, for the log.

**Encoder choice (§4.4).** Candidates in order: `h264_mf` on Windows
(the software MFT; a GPU MFT first only with `prefer_hardware`),
`h264_videotoolbox` with `-allow_sw 1` on macOS, then `libopenh264`. Being
listed is not enough: `h264_mf` is compiled into every Windows build and
fails at open on an N edition without the Media Feature Pack, so each
candidate encodes a quarter second of a 256×256 picture first. The smoke
input is a PNG through the image2 demuxer, not lavfi, so it also works on
the minimal §10.4 build. No candidate works: `ENCODER_UNAVAILABLE`.

**Retry (§8 `FFMPEG_FAIL`).** A non-zero exit or the timeout triggers one
retry on the software path: OpenH264 if it works and is not the primary,
else the software MFT, else the primary again; the retry always re-encodes
the audio with the native `aac` encoder. A second failure is `FFMPEG_FAIL`
naming both errors; the engine then outputs the original plus the separate
overlay (§8).

**Timeout.** `duration × timeout_per_media_second + timeout_base_secs`,
default `duration × 10 + 60 s`; ffmpeg is killed when it passes.

### `VideoOptions`

| Field | Default | Meaning |
|---|---|---|
| `prefer_hardware` | `false` | try a GPU Media Foundation MFT before the software one |
| `min_kbps` / `max_kbps` | 500 / 50 000 | bitrate floor and ceiling |
| `min_bits_per_pixel` | 0.04 | per-pixel floor (2.5 Mbit/s for 1080×1920 at 30 fps) |
| `hevc_to_h264_factor` | 1.5 | an HEVC source's bitrate × this for the H.264 output |
| `mf_software_bitrate_factor` | 1.0 | extra bitrate for the software MFT only (Q-04; 1.3 matches OpenH264's quality) |
| `audio_kbps` | 160 | native `aac` when audio cannot be copied |
| `aspect_tolerance` | 0.02 | §4.4 |
| `timeout_per_media_second` / `timeout_base_secs` | 10 / 60 | §4.4 timeout |

### Clip rejoin

`detect_clip_groups(&[ClipDescriptor], &RejoinOptions) -> Vec<ClipGroup>`
applies every §4.5 rule; `can_follow(prev, next, opts)` returns the first
rule that fails as `NotJoined` (`NotVideo`, `OutsideYears`, `Resolution`,
`Codec`, `FrameRate`, `TooShort`, `Gap`, `Location`) for the dry-run
preview. `ClipDescriptor::from_probe(id, date_utc, location, &MediaInfo)`
fills width, height, codec and frame rate from the probe.

| `RejoinOptions` | Default | Rule |
|---|---|---|
| `years` | `Years { first: 2017, last: 2021 }` (`All` for "all years") | every clip of the group, by UTC year |
| `min_clip_secs` | 9.0 | every clip but the last |
| `max_gap_secs` | 2.0 | next `Date` − (this `Date` + duration) |
| `max_overlap_secs` | 1.0 | negative gaps up to this are accepted (`Date` has whole seconds) |
| `location_tolerance_m` | 100 | "same location"; both `None` also matches |
| `fps_tolerance` | 0.01 | same frame rate |

Same resolution compares displayed sizes, same codec compares fourccs.
A clip's successor is the earliest later clip that may follow it and is not
already taken, so an unrelated clip recorded in between does not split a
group. `ClipGroup { item_ids, start_utc, total_secs, gaps_secs }`.

`Compositor::rejoin(&[Segment { path, overlay }], out)` probes the
segments, refuses segments that differ in codec, coded size or rotation
(`MEDIA_DECODE`), writes an `ffconcat` list to a temp directory, and:

* **no overlays and identical `avcC`/`hvcC`**: video stream copy; the
  concat demuxer keeps the first segment's display matrix, so a rotated
  group stays rotated (tested);
* **any overlay, or differing decoder configurations**: re-encode with the
  encoder plan; rotation baked in; each segment's overlay enabled over
  `[start, end)` of that segment on the joined timeline.

The audio is always re-encoded with the native `aac` encoder (see
deviations), and the input timeline is kept with `-copyts`.

### FR-51: `hevc_playback_available() -> bool`, `hevc_playback() -> HevcPlayback`

Windows decodes HEVC only through a Media Foundation decoder MFT. Two
checks, either is enough:

1. **Package**: a subkey starting with `Microsoft.HEVCVideoExtension` under
   `HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages`
   (the per-user package repository `Get-AppxPackage` reads). Matches the
   paid `Microsoft.HEVCVideoExtension` and the free OEM
   `Microsoft.HEVCVideoExtensions`.
2. **Decoder MFT**: `MFTEnumEx(MFT_CATEGORY_VIDEO_DECODER, SYNC|ASYNC|HARDWARE|LOCALMFT|SORTANDFILTER, {Video, HEVC})`.
   `mfplat.dll` is loaded at run time from System32
   (`LoadLibraryExW(..., LOAD_LIBRARY_SEARCH_SYSTEM32)`), never linked: an
   import-time link would stop the app from starting on Windows N/KN
   without the Media Feature Pack, the case where metadata-only mode must
   still work.

On this machine (Windows 11 Pro N with the Media Feature Pack and the OEM
HEVC Video Extensions 2.4.110) the package check and `MFTEnumEx` agree: one
HEVC decoder MFT, found in 21 ms. HKLM's MFT registry lists no HEVC decoder,
so `MFTEnumEx` does return Store-packaged codecs to a desktop process. The
"not installed" case was not reproduced (uninstalling the owner's codec was
out of scope). macOS reports `true` (system HEVC since 10.13).

### Errors and warnings

| `Error` | `code()` (§8) | Engine behaviour (§8) |
|---|---|---|
| `Unsupported`, `MediaDecode` | `MEDIA_DECODE` | copy the original as-is, flag |
| `EncoderUnavailable` | `ENCODER_UNAVAILABLE` | disable video compositing; metadata still fixed |
| `FfmpegFail { timed_out, stderr }` (after the retry) | `FFMPEG_FAIL` | output original + separate overlay |
| `FfmpegMissing` | `FFMPEG_FAIL` | sidecar missing or not executable |
| `Io` | `UNKNOWN` | |

| `Warning` | `code()` |
|---|---|
| `OverlayAspectMismatch { overlay, target, difference }` | `overlay_aspect_mismatch` (§4.4) |
| `OddSizeScaled { from, to }` | `odd_size_scaled` |
| `AudioReencoded { from }` | `audio_reencoded` |

Warnings are also logged through the `log` crate at `warn`.

### Pure planners (what CI tests without ffmpeg)

`video::composite_plan`, `rejoin::rejoin_plan`, `rejoin::concat_list`,
`video::place_overlay`, `video::target_bitrate_kbps`,
`video::encoder_bitrate_kbps`, `video::rotation_filters`,
`video::even_size`, `ffmpeg::EncoderSet::parse`,
`ffmpeg::encoder_candidates`, `ffmpeg::select_encoder`,
`ffmpeg::EncoderPlan::from_working`. Each returns data; `Plan::args` is a
`Vec<OsString>` handed to `Command::args`, never a shell string, so paths
with spaces, quotes or non-ASCII characters are single arguments.

## The ffmpeg commands

Printed by the tests on F-07/F-08 (paths shortened; on Windows the sidecar
is the pinned BtbN LGPL build).

**FR-24 composite**, portrait `rotate=90`, 360×780 display, 9:16 overlay
(aspect mismatch, fitted to the width and centred, y = 70):

```text
ffmpeg -hide_banner -nostdin -nostats -loglevel error -y
  -noautorotate -display_rotation 0 -i main.mp4 -i overlay.png
  -filter_complex "[0:v]transpose=clock[base];[1:v]scale=360:640[ov];[base][ov]overlay=0:70:eof_action=repeat,format=nv12[v]"
  -map [v] -map 0:a:0 -fps_mode passthrough
  -c:v h264_mf -hw_encoding 0 -profile:v 100 -b:v 1275k
  -c:a copy -map_metadata -1 -map_chapters -1 -movflags +faststart out.mp4
```

**Compatibility transcode**, `rotate=270`, no overlay:

```text
ffmpeg ... -noautorotate -display_rotation 0 -i main.mp4
  -filter_complex "[0:v]transpose=cclock,format=nv12[v]" -map [v] -map 0:a:0 -fps_mode passthrough
  -c:v h264_mf -hw_encoding 0 -profile:v 100 -b:v 1148k -c:a copy
  -map_metadata -1 -map_chapters -1 -movflags +faststart out.mp4
```

**FR-27 AC1 join**, no overlays (video copied):

```text
ffmpeg ... -copyts -f concat -safe 0 -i segments.ffconcat
  -map 0:v:0 -c:v copy -map 0:a:0 -c:a aac -b:a 160k
  -map_metadata -1 -map_chapters -1 -movflags +faststart joined.mp4
```

with `segments.ffconcat`:

```text
ffconcat version 1.0
file 'C:/.../2019-07-14_<id>-main.mp4'
file 'C:/.../2019-07-14_<id>-main.mp4'
file 'C:/.../2019-07-14_<id>-main.mp4'
```

**FR-27 join with overlays** on segments 0 and 2 of four rotated segments:

```text
ffmpeg ... -copyts -noautorotate -display_rotation 0 -f concat -safe 0 -i segments.ffconcat
  -i seg0-overlay.png -i seg2-overlay.png
  -filter_complex "[0:v]transpose=clock[base];
    [1:v]scale=288:512[ov0];[base][ov0]overlay=0:0:eof_action=repeat:enable='gte(t,0.000000)*lt(t,10.000000)'[s0];
    [2:v]scale=288:512[ov2];[s0][ov2]overlay=0:0:eof_action=repeat:enable='gte(t,20.000000)*lt(t,30.000000)'[s2];
    [s2]format=nv12[v]"
  -map [v] -fps_mode passthrough -c:v h264_mf -hw_encoding 0 -profile:v 100 -b:v 500k
  -map 0:a:0 -c:a aac -b:a 160k -map_metadata -1 -map_chapters -1 -movflags +faststart joined.mp4
```

**Encoder listing and smoke test**:

```text
ffmpeg -hide_banner -nostdin -encoders
ffmpeg -hide_banner -nostdin -loglevel error -y -loop 1 -framerate 30 -t 0.25 -i smoke.png
  -vf scale=256:256,format=nv12 -c:v h264_mf -hw_encoding 0 -profile:v 100 -b:v 500k smoke.mp4
```

**Test oracles and helpers** (tests only): `ffmpeg -ss <t> -i out.mp4 -frames:v 1 frame.png`
(the frame a player shows at `t`), `ffmpeg -i out.mp4 -map 0:a:0 -ac 1 -ar 44100 -f s16le pipe:1`
(audio levels at the joins), `ffmpeg -i file` (probe oracle on F-09).

## Measured results

All on the owner's machine (Windows 11 Pro N, Core Ultra 7 265K, NVIDIA
GPU), pinned BtbN LGPL ffmpeg n9.0.2, debug build of the tests.

| Acceptance criterion | Fixture | Result |
|---|---|---|
| FR-23 AC1: SSIM ≥ 0.98 vs reference, orientations 6/8 included | F-07, 7 photos (orientations 1, 3, 6, 8; two aspect mismatches; one PNG) | **0.983 to 0.990** for JPEG at q92, **1.000** for PNG; markers upright in every output |
| FR-24 AC1: portrait (rotate 90/270), 180 and landscape upright, frame SSIM ≥ 0.95 | F-07, 4 videos, 11 sampled frames | **0.985 to 0.993**; markers upright; output rotation 0 |
| FR-24 AC2: A/V drift ≤ 40 ms over 60 s | F-07 60 s landscape | **0.0 ms**; same frame count and audio sample count as the source |
| FR-27 AC1: 10 + 10 + 4 s with 1 s gaps → one 24 s ± 0.1 s file, no audible gap | F-08 group A | **24.000 s** file, video and audio; lowest 10 ms window at the joins **96 to 97%** of the median level |
| FR-27 AC2: unrelated adjacent clips not joined | F-08 decoys (resolution, 5 s gap, 25 vs 30 fps, 1.1 km apart, short middle clip, New Year across 2022) | none joined; 7 groups by default, 9 with all years, exactly as stated in truth.json |
| Per-segment overlays | F-08 group D (rotated, overlays on segments 0 and 2) | sticker present only in its own segments (diff 98 vs 0.5); segments without overlay SSIM 0.99 to their source |
| Probe correctness | F-09 (H.264, HEVC 8-bit `hvc1` rotated, HEVC 10-bit `hev1`, VFR, no audio, 361×641, 25 fps) | all fields equal truth.json and ffmpeg's `-i` listing |
| Codec robustness | F-09, every case | transcodes and composites; odd size scaled to 360×640; VFR kept with every frame |

The SSIM threshold discriminates: on the same F-07 frame, a composite
without the overlay scores 0.937, one with the overlay 40 px off 0.887, and
a mirrored one 0.484, against 0.985+ for the real output.

### Encoders on a phone-sized clip

10 s, 1080×1920 display (coded 1920×1080 with `rotate=90`), 30 fps,
overlay 1080×1920; `cargo run --release -p mr-composite --example composite_bench`;
SSIM is ffmpeg's `ssim` filter, all frames, against the uncompressed
composite. Two passes; the range is shown.

| Source | Encoder | × realtime | Output kbit/s | SSIM Y |
|---|---|---|---|---|
| H.264 5.7 Mbit/s | `h264_mf` software (default) | 6.3 to 13 | 5 874 | 0.9856 |
| | `h264_mf` hardware (NVIDIA MFT) | 4.3 to 4.5 | 5 893 | 0.9950 |
| | `libopenh264` | 4.5 to 5.6 | 5 660 | 0.9912 |
| HEVC 10-bit 5.0 Mbit/s | `h264_mf` software | 8.4 to 11 | 7 809 (×1.5) | 0.9914 |
| | `h264_mf` hardware | 4.6 to 4.7 | 7 802 | 0.9960 |
| | `libopenh264` | 4.9 to 6.2 | 7 516 | 0.9939 |

Software MFT at a higher bitrate, same clip:

| Bitrate | SSIM Y | File |
|---|---|---|
| 1.0× (5 677k) | 0.9856 | 7.5 MB |
| 1.3× (7 400k) | 0.9904 | 9.8 MB |
| 1.5× (8 500k) | 0.9928 | 11.1 MB |
| 2.0× (11 400k) | 0.9956 | 14.2 MB |

What this means: every encoder is several times realtime here; scaled by
the Q-02 doc's ~2.7× for the i5-8th-gen reference laptop, the software MFT
lands around 2–5× and OpenH264 around 2×, so NFR-PERF-04 (≥ 1×) holds even
without a GPU. The software MFT is the fastest and the weakest per bit;
OpenH264 matches the software MFT at about 1.3× its bitrate. The NVIDIA MFT
is the best per bit but pays ~0.8 s of GPU start-up per file.

## Deviations from §4.4 and §4.5

| SRS says | Implemented | Why |
|---|---|---|
| §4.4 "auto-rotate the main video first" | `-noautorotate -display_rotation 0`, then `transpose` from the probe's rotation | Deterministic and testable. `-noautorotate` alone is wrong: ffmpeg 9 copies the input display matrix to the output and players rotate the upright frames again (measured). Needs an ffmpeg with the `-display_rotation` input option; the §10.4 build must keep it. |
| §4.4 "remove the rotation flag" | output has an identity matrix | by the above; tested on 90/180/270 |
| §4.4 "scale the overlay to the display dimensions" | display size rounded down to even, with `odd_size_scaled` | H.264 4:2:0 needs even sizes |
| §4.4 "target bitrate matched to the source" | source video bitrate; ×1.5 for HEVC sources; floors 500 kbit/s and 0.04 bit/pixel; cap 50 Mbit/s; optional software-MFT factor | H.264 needs more bits than HEVC; tiny sources starve otherwise |
| §4.4 encoder order | software `h264_mf` primary, GPU MFT only with `prefer_hardware` | present on every standard edition, same output everywhere, fastest here |
| §4.4 audio "stream-copied, else native aac" | as written for compositing; the retry always re-encodes | the retry is the "safe settings" path |
| FR-23 "resized to the photo's displayed dimensions" | the §4.4 2% fit-width-and-centre rule also applies to photos | stretching a 9:16 sticker onto a 19.5:9 photo distorts it |
| §4.5 "concat demuxer with stream copy" | video copied; **audio always re-encoded** (native `aac`) | copied AAC leaves each later segment's 1024 priming samples mid-stream: a ~10 ms drop to under 1% level at every join (audible click); re-encoded joins stay above 96% |
| §4.5 (implicit) | `-copyts` on every join | otherwise ffmpeg shifts the concat input by the first segment's AAC priming (23 ms) and the video lands one frame late, moving every overlay window by a frame |
| §4.5 gap ≤ 2 s | gap in [−1 s, 2 s] | `Date` has whole seconds |
| §4.5 "share a location" | within 100 m (configurable) | the §3.6 figure; exact equality would miss GPS jitter |
| §4.5 (no limit) | no maximum group size | the SRS gives none; Snapchat's 10 s segments ran to 6 |
| §4.5 "re-encodes only if overlays are burned in" | also when the segments' `avcC`/`hvcC` differ | concat copy uses the first segment's parameter sets |
| — | segments must share codec, coded size and rotation | refused as `MEDIA_DECODE` otherwise |
| — | non-square pixel aspect ratios are not corrected | phones write 1:1; not seen in any fixture |

## Fixtures (`tools/make_fixture/fixtures_m3.py`)

`python tools/make_fixture --fixture rotation --fixture clips --fixture codecs`
(also part of `--all`). Each writes `fixtures/<name>/` with a `2026b`
export (`mydata~1760000000000.zip`, so the engine can run end to end),
`files/` (the same media extracted), `ref/` (F-07 references) and
`truth.json`. Generation takes about 4 seconds here.

ffmpeg with libx264 is required (libx265 for the two HEVC cases). The
generator uses `MR_FIXTURE_FFMPEG` when set, else the first ffmpeg on
`PATH` that has libx264: `cargo test` puts `target/debug` first on Windows,
and tauri-build copies the LGPL sidecar (no libx264) there. Without one,
`clips` and `codecs` write a truth.json with `"skipped"` and no media, and
`rotation` writes its photos only (`"videos_skipped"`); the tests skip what
is missing. That is what happens on the CI runners.

Every scene has a red square in the top-left corner and a blue one in the
bottom-right corner, in display orientation, so a test can tell an upright
frame from a rotated or mirrored one without any reference.

| Fixture | Contents | truth.json item fields |
|---|---|---|
| F-07 `rotation` | 5 videos: 360×640 with `rotate=90` and `rotate=270`, 640×360 with 180, a 60 s 640×360 landscape (AC2), 360×780 with `rotate=90` under a 9:16 overlay (mismatch). 7 photos 540×960 or 540×1170: EXIF orientation 1, 3, 6, 8, a PNG, two aspect mismatches (one with orientation 6). Overlays at 1× or 2× the display size. | `case`, `rotation` or `orientation`, `coded_size`/`stored_size`, `display_size`, `overlay_size`, `placement {x, y, width, height, aspect_mismatch}`, `duration`, `has_audio`, `markers {size, red, blue}`, `reference` (photos) or `reference_frames [{t, file}]` (videos), `output_format` |
| F-08 `clips` | A `2026a` export (exact matching, so the engine keeps exact times on the day whose clips are 1.1 km apart and §4.5 alone decides). 33 clips at 288×512 (30 fps, sine audio): group A 10+10+4 s (AC1, 2019), B 2 clips (2017, London), C 6 clips ending in 3 s (2021-12-31), D 4 rotated clips with overlays on 0 and 2 (Tokyo), I 3 rotated clips for a copy join (Sydney), E 3 clips in 2023, F 10/6/10 s (short middle clip), G 30 then 25 fps, H three clips 1.1 km then 20 m apart, K across New Year 2022; decoys: 360×640 one second after A, a clip 5 s after B. | `group`, `index`, `duration`, `display_size`, `coded_size`, `rotation`, `fps`, `codec`, `audio_hz`; top level `expected_groups {default, all_years}` stated by construction, `cases {ac1, ac2_decoy_resolution, decoy_gap, fps_decoy, location_far, short_middle_alone, overlay_group, overlay_segments, rotated_copy_group, year_2023, new_year}` |
| F-09 `codecs` | 3 s each: H.264 640×360, HEVC 8-bit `hvc1` with `rotate=90`, HEVC 10-bit `hev1`, H.264 VFR (30 then 15 fps), H.264 without audio, H.264 4:4:4 at 361×641, H.264 25 fps | `case`, `expected {codec, coded_size, display_size, rotation, fps, vfr, bit_depth, has_audio, duration}`; top level `skipped_cases` |

References: photos with Pillow (`ImageOps.exif_transpose`, overlay resized
`BILINEAR` to the placement computed in Python, `Image.alpha_composite`);
video frames with ffmpeg's own autorotate and
`[1:v]scale=W:H[ov];[0:v][ov]overlay=X:Y` at each sample time. Neither path
shares code with the crate.

## Tests and where they run

| Test | Covers | CI (no ffmpeg) |
|---|---|---|
| 44 unit tests (`cargo test -p mr-composite --lib`) | probe parsers (matrix, `stts`, `elst`, EXIF, SPS, `hvcC`, `esds`), placement, bitrate, every argument array, encoder selection with fake `-encoders` listings (Windows LGPL, macOS, none), the software path, concat quoting, the §4.5 rules, photo orientation and blending, error codes; retry and timeout against a real ffmpeg | run; the two ffmpeg ones skip |
| `tests/photo.rs` | FR-23 AC1, quality option, `MEDIA_DECODE` | **run** (F-07 photos need only Pillow) |
| `tests/video.rs` | FR-24 AC1, AC2, encoder plan, compatibility transcode | skip |
| `tests/clips.rs` | FR-27 detection, AC1, AC2, per-segment overlays, rotated copy join, refusal | skip |
| `tests/codecs.rs` | F-09 probe vs truth and oracle, transcode and composite of every case, FR-51 check | probe and composite skip; the HEVC check runs |

Locally the tests use `third_party/ffmpeg/ffmpeg(.exe)` when staged
(`python tools/sidecars.py fetch`), else an ffmpeg on `PATH`.

## Examples

```bash
cargo run -p mr-composite --example probe -- path/to/file.mp4
```

```bash
cargo run -p mr-composite --example media_check
```

```bash
cargo run --release -p mr-composite --example composite_bench -- main.mp4 overlay.png
```

`media_check` prints the listed H.264 encoders, which pass the smoke
encode, the encoder plan and the FR-51 answer; run it on an N edition
without the Media Feature Pack, and on a Mac with the §10.4 build, to close
the two cases this machine cannot show.
