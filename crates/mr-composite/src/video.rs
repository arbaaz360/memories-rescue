//! Video compositing (FR-24, SRS §4.4) and the Windows compatibility
//! transcode (§4.4 "HEVC sources", FR-51): the pure planning functions that
//! turn a probed video into one ffmpeg argument array.
//!
//! Rotation is applied by this crate, not by ffmpeg's autorotate: the main
//! input is opened with `-noautorotate -display_rotation 0`, the probe's
//! rotation becomes `transpose` filters, and the output therefore carries an
//! identity matrix. `-noautorotate` alone is not enough: ffmpeg 9 copies the
//! input display matrix to the output, and players would rotate the already
//! upright frames a second time.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::ffmpeg::VideoEncoder;
use crate::probe::MediaInfo;

/// Tunables for compositing and transcoding. `Default` is the SRS behaviour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoOptions {
    /// Try a GPU Media Foundation MFT before the software one (Windows).
    /// Off by default: the software MFT is present everywhere and behaves
    /// the same on every machine; see the API doc for the measured speeds.
    pub prefer_hardware: bool,
    /// Bitrate floor in kbit/s.
    pub min_kbps: u32,
    /// Bitrate ceiling in kbit/s.
    pub max_kbps: u32,
    /// Bitrate floor per pixel per frame, so tiny sources do not starve.
    pub min_bits_per_pixel: f64,
    /// H.264 needs more bits than HEVC for the same quality; an HEVC
    /// source's bitrate is multiplied by this.
    pub hevc_to_h264_factor: f64,
    /// Extra bitrate for Microsoft's software H.264 MFT, which needs about
    /// 1.3x the bits of OpenH264 for the same SSIM (measured; see the API
    /// doc). Q-04 (provisionally closed 2026-10-16): the release ships no
    /// OpenH264 and uses the software MFT at 1.3x; 1.0 is §4.4's literal
    /// "matched to the source".
    pub mf_software_bitrate_factor: f64,
    /// Bitrate of the native `aac` encoder when audio cannot be copied.
    pub audio_kbps: u32,
    /// Relative aspect difference above which the overlay is fitted to the
    /// width and centred vertically (§4.4: 2%).
    pub aspect_tolerance: f64,
    /// Timeout: this many seconds of wall time per second of media ...
    pub timeout_per_media_second: f64,
    /// ... plus this.
    pub timeout_base_secs: f64,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            prefer_hardware: false,
            min_kbps: 500,
            max_kbps: 50_000,
            min_bits_per_pixel: 0.04,
            hevc_to_h264_factor: 1.5,
            mf_software_bitrate_factor: 1.3,
            audio_kbps: 160,
            aspect_tolerance: 0.02,
            timeout_per_media_second: 10.0,
            timeout_base_secs: 60.0,
        }
    }
}

impl VideoOptions {
    /// §4.4: duration × 10 + 60 s by default.
    pub fn timeout(&self, media_secs: f64) -> Duration {
        Duration::from_secs_f64(
            (media_secs.max(0.0) * self.timeout_per_media_second + self.timeout_base_secs).max(1.0),
        )
    }
}

/// Something worth a line in the report; the item still succeeded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum Warning {
    /// §4.4: the overlay's aspect differs from the media's by more than the
    /// tolerance; it was fitted to the width and centred vertically.
    OverlayAspectMismatch {
        overlay: (u32, u32),
        target: (u32, u32),
        /// Relative difference of the two aspect ratios.
        difference: f64,
    },
    /// The display size was odd; H.264 4:2:0 needs even sizes, so the video
    /// was scaled by one pixel.
    OddSizeScaled { from: (u32, u32), to: (u32, u32) },
    /// The audio could not be stream-copied and was re-encoded to AAC.
    AudioReencoded { from: String },
}

impl Warning {
    pub fn code(&self) -> &'static str {
        match self {
            Warning::OverlayAspectMismatch { .. } => "overlay_aspect_mismatch",
            Warning::OddSizeScaled { .. } => "odd_size_scaled",
            Warning::AudioReencoded { .. } => "audio_reencoded",
        }
    }

    pub(crate) fn log(&self) {
        log::warn!("{}: {:?}", self.code(), self);
    }
}

/// Where the scaled overlay goes on the target frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    /// Negative when a taller overlay is centred and cropped top and bottom.
    pub y: i32,
    pub aspect_mismatch: bool,
}

/// Relative difference of the two aspect ratios, `|a_o - a_t| / a_t`.
pub fn aspect_difference(target: (u32, u32), overlay: (u32, u32)) -> f64 {
    let at = target.0 as f64 / target.1.max(1) as f64;
    let ao = overlay.0 as f64 / overlay.1.max(1) as f64;
    (ao - at).abs() / at
}

/// §4.4 scale and aspect rule, shared with photos: stretch to the target
/// when the aspects agree within `tolerance`, otherwise fit to the width and
/// centre vertically.
/// An overlay that, fitted to the width, would come out more than this many
/// times taller than the picture is refused: scaling it would need an absurd
/// buffer (a crafted 1x100000 PNG on a 1080-wide photo asks for ~460 GB, and
/// a failed allocation aborts the process). Real overlays are the picture's
/// size or a phone screen's.
pub const MAX_OVERLAY_STRETCH: u64 = 8;

/// [`place_overlay`] for untrusted sizes: `MEDIA_DECODE` for an empty picture
/// or overlay, or a placement more than [`MAX_OVERLAY_STRETCH`] times taller
/// than the picture.
pub fn checked_placement(
    target: (u32, u32),
    overlay: (u32, u32),
    tolerance: f64,
) -> Result<Placement, Error> {
    if target.0 == 0 || target.1 == 0 || overlay.0 == 0 || overlay.1 == 0 {
        return Err(Error::Unsupported(format!(
            "cannot place a {}x{} overlay on a {}x{} picture",
            overlay.0, overlay.1, target.0, target.1
        )));
    }
    let p = place_overlay(target, overlay, tolerance);
    if p.height as u64 > target.1 as u64 * MAX_OVERLAY_STRETCH {
        return Err(Error::Unsupported(format!(
            "a {}x{} overlay would be {} px tall on a {}x{} picture",
            overlay.0, overlay.1, p.height, target.0, target.1
        )));
    }
    Ok(p)
}

pub fn place_overlay(target: (u32, u32), overlay: (u32, u32), tolerance: f64) -> Placement {
    let (w, h) = target;
    // The epsilon keeps an exact 2% difference a match despite float rounding.
    if aspect_difference(target, overlay) <= tolerance + 1e-9 {
        return Placement {
            width: w,
            height: h,
            x: 0,
            y: 0,
            aspect_mismatch: false,
        };
    }
    let oh = ((w as f64 * overlay.1 as f64 / overlay.0.max(1) as f64).round() as u32).max(1);
    Placement {
        width: w,
        height: oh,
        x: 0,
        y: (h as i64 - oh as i64).div_euclid(2) as i32,
        aspect_mismatch: true,
    }
}

/// Round a size down to even numbers (H.264 4:2:0), at least 2×2.
pub fn even_size((w, h): (u32, u32)) -> (u32, u32) {
    ((w & !1).max(2), (h & !1).max(2))
}

/// `transpose` chain that turns coded frames upright for a clockwise display
/// rotation. Only `transpose` is used, which is in the §10.4 filter list.
pub fn rotation_filters(rotation: u16) -> Option<&'static str> {
    match rotation {
        90 => Some("transpose=clock"),
        180 => Some("transpose=clock,transpose=clock"),
        270 => Some("transpose=cclock"),
        _ => None,
    }
}

/// Average bitrate for the H.264 output in kbit/s: the source video
/// track's bitrate (or the file's), raised by `hevc_to_h264_factor` for HEVC
/// sources, at least `min_bits_per_pixel` and `min_kbps`, at most `max_kbps`.
pub fn target_bitrate_kbps(info: &MediaInfo, out: (u32, u32), opts: &VideoOptions) -> u32 {
    let video = info.video.as_ref();
    let audio_kbps = info
        .audio
        .as_ref()
        .and_then(|a| a.bitrate_kbps)
        .unwrap_or(0);
    let source = video
        .and_then(|v| v.bitrate_kbps)
        .or_else(|| info.bitrate_kbps.map(|b| b.saturating_sub(audio_kbps)))
        .unwrap_or(0) as f64;
    let factor = match video.map(|v| v.codec.as_str()) {
        Some("hvc1" | "hev1") => opts.hevc_to_h264_factor,
        _ => 1.0,
    };
    let fps = video.and_then(|v| v.frame_rate).unwrap_or(30.0).max(1.0);
    let per_pixel_floor = out.0 as f64 * out.1 as f64 * fps * opts.min_bits_per_pixel / 1000.0;
    let kbps = (source * factor)
        .max(per_pixel_floor)
        .max(opts.min_kbps as f64);
    (kbps.round() as u32).min(opts.max_kbps)
}

/// [`target_bitrate_kbps`] for a given encoder: the software MFT gets
/// `mf_software_bitrate_factor` on top, still capped at `max_kbps`.
pub fn encoder_bitrate_kbps(
    info: &MediaInfo,
    out: (u32, u32),
    encoder: VideoEncoder,
    opts: &VideoOptions,
) -> u32 {
    let base = target_bitrate_kbps(info, out, opts);
    if encoder == (VideoEncoder::MediaFoundation { hardware: false }) {
        ((base as f64 * opts.mf_software_bitrate_factor).round() as u32).min(opts.max_kbps)
    } else {
        base
    }
}

/// How the audio is carried into the output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AudioPlan {
    None,
    Copy,
    Aac { kbps: u32 },
}

impl AudioPlan {
    fn args(&self) -> Vec<String> {
        match self {
            AudioPlan::None => vec!["-an".into()],
            AudioPlan::Copy => vec!["-c:a".into(), "copy".into()],
            AudioPlan::Aac { kbps } => vec![
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                format!("{kbps}k"),
            ],
        }
    }
}

/// An overlay PNG and its pixel size.
#[derive(Debug, Clone, Copy)]
pub struct OverlayInput<'a> {
    pub path: &'a Path,
    pub size: (u32, u32),
}

/// One ffmpeg invocation, ready to run.
#[derive(Debug, Clone)]
pub struct Plan {
    pub args: Vec<OsString>,
    pub timeout: Duration,
    /// The `-filter_complex` graph, also present in `args`.
    pub filter: String,
    pub output_size: (u32, u32),
    pub bitrate_kbps: u32,
    pub audio: AudioPlan,
    /// `None` for a stream copy (clip rejoin without overlays).
    pub encoder: Option<VideoEncoder>,
    pub warnings: Vec<Warning>,
}

impl Plan {
    /// The arguments as text, for logs and the report.
    pub fn args_lossy(&self) -> Vec<String> {
        self.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }
}

/// The common head: quiet, no stdin, overwrite.
pub(crate) fn head_args() -> Vec<OsString> {
    [
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-loglevel",
        "error",
        "-y",
    ]
    .iter()
    .map(OsString::from)
    .collect()
}

/// The common tail: no stale tags (the engine writes metadata afterwards,
/// §4.3), `moov` first, the output path.
pub(crate) fn tail_args(out: &Path) -> Vec<OsString> {
    let mut v: Vec<OsString> = [
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
        "-movflags",
        "+faststart",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    v.push(out.as_os_str().to_owned());
    v
}

pub(crate) fn audio_plan(
    info: &MediaInfo,
    safe: bool,
    opts: &VideoOptions,
) -> (AudioPlan, Option<Warning>) {
    match &info.audio {
        None => (AudioPlan::None, None),
        Some(a) if a.mp4_copyable() && !safe => (AudioPlan::Copy, None),
        Some(a) => (
            AudioPlan::Aac {
                kbps: opts.audio_kbps,
            },
            (!a.mp4_copyable()).then(|| Warning::AudioReencoded {
                from: a.codec.clone(),
            }),
        ),
    }
}

/// Plan an overlay composite (FR-24) or, with `overlay: None`, an H.264
/// transcode (the compatibility mode). `safe` is the retry: audio is
/// re-encoded instead of copied.
pub fn composite_plan(
    main: &Path,
    info: &MediaInfo,
    overlay: Option<OverlayInput<'_>>,
    out: &Path,
    encoder: VideoEncoder,
    safe: bool,
    opts: &VideoOptions,
) -> Result<Plan, Error> {
    let video = info
        .video
        .as_ref()
        .ok_or_else(|| Error::Unsupported(format!("{}: no video track", main.display())))?;
    let display = video.display_size();
    let output_size = even_size(display);
    let mut warnings = Vec::new();
    if output_size != display {
        warnings.push(Warning::OddSizeScaled {
            from: display,
            to: output_size,
        });
    }

    let mut base: Vec<String> = Vec::new();
    if let Some(r) = rotation_filters(video.rotation) {
        base.push(r.to_string());
    }
    if output_size != display {
        base.push(format!("scale={}:{}", output_size.0, output_size.1));
    }
    let pix = encoder.pix_fmt();
    let filter = match overlay {
        Some(ov) => {
            let p = checked_placement(output_size, ov.size, opts.aspect_tolerance)?;
            if p.aspect_mismatch {
                warnings.push(Warning::OverlayAspectMismatch {
                    overlay: ov.size,
                    target: output_size,
                    difference: aspect_difference(output_size, ov.size),
                });
            }
            let (prefix, base_label) = if base.is_empty() {
                (String::new(), "[0:v]")
            } else {
                (format!("[0:v]{}[base];", base.join(",")), "[base]")
            };
            format!(
                "{prefix}[1:v]scale={}:{}[ov];{base_label}[ov]overlay={}:{}:eof_action=repeat,format={pix}[v]",
                p.width, p.height, p.x, p.y
            )
        }
        None => {
            base.push(format!("format={pix}"));
            format!("[0:v]{}[v]", base.join(","))
        }
    };

    let (audio, audio_warning) = audio_plan(info, safe, opts);
    warnings.extend(audio_warning);
    let bitrate_kbps = encoder_bitrate_kbps(info, output_size, encoder, opts);

    let mut args = head_args();
    args.extend(["-noautorotate", "-display_rotation", "0", "-i"].map(OsString::from));
    args.push(main.as_os_str().to_owned());
    if let Some(ov) = overlay {
        args.push("-i".into());
        args.push(ov.path.as_os_str().to_owned());
    }
    args.push("-filter_complex".into());
    args.push(filter.clone().into());
    args.extend(["-map", "[v]"].map(OsString::from));
    if audio != AudioPlan::None {
        args.extend(["-map", "0:a:0"].map(OsString::from));
    }
    // Keep the source timestamps: variable-frame-rate clips stay variable and
    // the audio stays where it was (FR-24 AC2).
    args.extend(["-fps_mode", "passthrough"].map(OsString::from));
    args.extend(encoder.args(bitrate_kbps).into_iter().map(OsString::from));
    args.extend(audio.args().into_iter().map(OsString::from));
    args.extend(tail_args(out));

    Ok(Plan {
        args,
        timeout: opts.timeout(info.duration_secs),
        filter,
        output_size,
        bitrate_kbps,
        audio,
        encoder: Some(encoder),
        warnings,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::probe::{AudioTrack, Container, VideoTrack};
    use std::path::PathBuf;

    pub(crate) fn video_info(
        codec: &str,
        coded: (u32, u32),
        rotation: u16,
        kbps: u32,
        audio: Option<&str>,
    ) -> MediaInfo {
        MediaInfo {
            container: Container::Mp4,
            brand: Some("isom".into()),
            file_size: 1_000_000,
            duration_secs: 12.5,
            bitrate_kbps: Some(kbps + 128),
            video: Some(VideoTrack {
                codec: codec.into(),
                coded_width: coded.0,
                coded_height: coded.1,
                rotation,
                duration_secs: 12.5,
                timescale: 15360,
                frame_count: 375,
                frame_rate: Some(30.0),
                variable_frame_rate: false,
                bit_depth: Some(8),
                profile: Some(100),
                bytes: 0,
                bitrate_kbps: Some(kbps),
                edit_delay_secs: 0.0,
                edit_skip_secs: 0.0,
                presented_secs: 12.5,
                config_hash: Some(1),
            }),
            audio: audio.map(|c| AudioTrack {
                codec: c.into(),
                channels: 2,
                sample_rate: 44100,
                duration_secs: 12.5,
                timescale: 44100,
                sample_count: 539,
                bytes: 0,
                bitrate_kbps: Some(128),
                edit_delay_secs: 0.0,
                edit_skip_secs: 0.0,
                presented_secs: 12.5,
            }),
            image: None,
        }
    }

    fn strs(p: &Plan) -> Vec<String> {
        p.args_lossy()
    }

    fn after<'a>(args: &'a [String], flag: &str) -> &'a str {
        let i = args
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("{flag} in {args:?}"));
        &args[i + 1]
    }

    const MF: VideoEncoder = VideoEncoder::MediaFoundation { hardware: false };

    #[test]
    fn portrait_rotate_90_with_matching_overlay() {
        let info = video_info("avc1", (1920, 1080), 90, 6000, Some("aac"));
        let main = PathBuf::from("in dir/main.mp4");
        let ov = PathBuf::from("in dir/overlay.png");
        let out = PathBuf::from("out/composite.mp4");
        let plan = composite_plan(
            &main,
            &info,
            Some(OverlayInput {
                path: &ov,
                size: (1080, 1920),
            }),
            &out,
            MF,
            false,
            &VideoOptions::default(),
        )
        .unwrap();
        assert_eq!(plan.output_size, (1080, 1920));
        assert_eq!(
            plan.filter,
            "[0:v]transpose=clock[base];[1:v]scale=1080:1920[ov];[base][ov]overlay=0:0:eof_action=repeat,format=nv12[v]"
        );
        let a = strs(&plan);
        // Paths are single array elements, spaces and all.
        assert!(a.contains(&"in dir/main.mp4".to_string()));
        assert_eq!(a.last().unwrap(), "out/composite.mp4");
        // The rotation is ours: ffmpeg must neither rotate nor copy the matrix.
        let i = a.iter().position(|x| x == "-noautorotate").unwrap();
        assert_eq!(
            &a[i..i + 5],
            [
                "-noautorotate",
                "-display_rotation",
                "0",
                "-i",
                "in dir/main.mp4"
            ]
        );
        assert_eq!(after(&a, "-c:a"), "copy");
        assert_eq!(after(&a, "-c:v"), "h264_mf");
        // 6000 kbit/s source x 1.3 for the software MFT (Q-04).
        assert_eq!(after(&a, "-b:v"), "7800k");
        assert_eq!(after(&a, "-fps_mode"), "passthrough");
        assert_eq!(after(&a, "-movflags"), "+faststart");
        assert!(a.windows(2).any(|w| w == ["-map", "0:a:0"]));
        assert!(plan.warnings.is_empty());
        assert_eq!(plan.timeout, Duration::from_secs_f64(12.5 * 10.0 + 60.0));
    }

    #[test]
    fn rotations_270_and_180() {
        for (rot, filt) in [
            (270, "transpose=cclock"),
            (180, "transpose=clock,transpose=clock"),
        ] {
            let info = video_info("avc1", (1280, 720), rot, 3000, None);
            let p = composite_plan(
                Path::new("m.mp4"),
                &info,
                None,
                Path::new("o.mp4"),
                VideoEncoder::OpenH264,
                false,
                &VideoOptions::default(),
            )
            .unwrap();
            assert_eq!(p.filter, format!("[0:v]{filt},format=yuv420p[v]"));
            let expect = if rot == 180 { (1280, 720) } else { (720, 1280) };
            assert_eq!(p.output_size, expect);
            assert_eq!(p.audio, AudioPlan::None);
            assert!(strs(&p).contains(&"-an".to_string()));
            assert!(!strs(&p).iter().any(|x| x == "0:a:0"));
        }
    }

    #[test]
    fn landscape_without_rotation_has_no_base_chain() {
        let info = video_info("avc1", (1280, 720), 0, 3000, Some("aac"));
        let ov = PathBuf::from("ov.png");
        let p = composite_plan(
            Path::new("m.mp4"),
            &info,
            Some(OverlayInput {
                path: &ov,
                size: (1280, 720),
            }),
            Path::new("o.mp4"),
            MF,
            false,
            &VideoOptions::default(),
        )
        .unwrap();
        assert_eq!(
            p.filter,
            "[1:v]scale=1280:720[ov];[0:v][ov]overlay=0:0:eof_action=repeat,format=nv12[v]"
        );
    }

    #[test]
    fn aspect_mismatch_fits_width_and_centres() {
        // 1080x2340 display (19.5:9) under a 9:16 overlay.
        let info = video_info("avc1", (2340, 1080), 90, 8000, Some("aac"));
        let ov = PathBuf::from("ov.png");
        let p = composite_plan(
            Path::new("m.mp4"),
            &info,
            Some(OverlayInput {
                path: &ov,
                size: (1080, 1920),
            }),
            Path::new("o.mp4"),
            MF,
            false,
            &VideoOptions::default(),
        )
        .unwrap();
        assert!(
            p.filter.contains("[1:v]scale=1080:1920[ov]"),
            "{}",
            p.filter
        );
        assert!(p.filter.contains("overlay=0:210:"), "{}", p.filter);
        assert_eq!(p.warnings.len(), 1);
        assert_eq!(p.warnings[0].code(), "overlay_aspect_mismatch");
    }

    #[test]
    fn placement_rule() {
        // Within 2%: stretched.
        let p = place_overlay((1080, 1920), (720, 1290), 0.02);
        assert!(!p.aspect_mismatch);
        assert_eq!((p.width, p.height, p.x, p.y), (1080, 1920, 0, 0));
        // Taller overlay on a wide frame: fitted to width, cropped equally.
        let p = place_overlay((1280, 720), (1080, 1920), 0.02);
        assert!(p.aspect_mismatch);
        assert_eq!((p.width, p.height), (1280, 2276));
        assert_eq!(p.y, (720 - 2276) / 2);
        // Exactly at the boundary is still a match.
        assert!(!place_overlay((1000, 1000), (1020, 1000), 0.02).aspect_mismatch);
        assert!(place_overlay((1000, 1000), (1021, 1000), 0.02).aspect_mismatch);
    }

    #[test]
    fn absurd_overlays_are_refused_not_allocated() {
        // A 1x100000 overlay fitted to a 1080-wide frame would be 108 million
        // px tall: refused before anything is scaled.
        let e = checked_placement((1080, 1920), (1, 100_000), 0.02).unwrap_err();
        assert_eq!(e.code(), "MEDIA_DECODE");
        for (t, o) in [
            ((0, 10), (1, 1)),
            ((10, 0), (1, 1)),
            ((10, 10), (0, 1)),
            ((10, 10), (1, 0)),
        ] {
            assert_eq!(
                checked_placement(t, o, 0.02).unwrap_err().code(),
                "MEDIA_DECODE"
            );
        }
        // Up to MAX_OVERLAY_STRETCH times the frame height is allowed.
        assert_eq!(
            checked_placement((100, 100), (1, 8), 0.02).unwrap().height,
            800
        );
        assert!(checked_placement((100, 100), (1, 9), 0.02).is_err());
        // Real mismatches (a 9:16 overlay on a 16:9 frame) still pass.
        let p = checked_placement((1280, 720), (1080, 1920), 0.02).unwrap();
        assert_eq!((p.width, p.height), (1280, 2276));
        // A very wide overlay fits to width and is short: fine.
        assert_eq!(
            checked_placement((1080, 1920), (100_000, 1), 0.02)
                .unwrap()
                .height,
            1
        );

        let info = video_info("avc1", (1920, 1080), 0, 900, Some("aac"));
        let ov = PathBuf::from("ov.png");
        let e = composite_plan(
            Path::new("m.mp4"),
            &info,
            Some(OverlayInput {
                path: &ov,
                size: (1, 100_000),
            }),
            Path::new("o.mp4"),
            MF,
            false,
            &VideoOptions::default(),
        )
        .unwrap_err();
        assert_eq!(e.code(), "MEDIA_DECODE");
    }

    #[test]
    fn odd_sizes_scale_to_even() {
        let info = video_info("avc1", (641, 359), 0, 900, Some("aac"));
        let p = composite_plan(
            Path::new("m.mp4"),
            &info,
            None,
            Path::new("o.mp4"),
            MF,
            false,
            &VideoOptions::default(),
        )
        .unwrap();
        assert_eq!(p.output_size, (640, 358));
        assert_eq!(p.filter, "[0:v]scale=640:358,format=nv12[v]");
        assert_eq!(p.warnings[0].code(), "odd_size_scaled");
    }

    #[test]
    fn safe_retry_reencodes_audio() {
        let info = video_info("avc1", (1280, 720), 0, 3000, Some("aac"));
        let p = composite_plan(
            Path::new("m.mp4"),
            &info,
            None,
            Path::new("o.mp4"),
            VideoEncoder::OpenH264,
            true,
            &VideoOptions::default(),
        )
        .unwrap();
        assert_eq!(p.audio, AudioPlan::Aac { kbps: 160 });
        let a = strs(&p);
        assert_eq!(after(&a, "-c:a"), "aac");
        assert_eq!(after(&a, "-b:a"), "160k");
        assert!(p.warnings.is_empty(), "a safe retry is not a warning");
    }

    #[test]
    fn uncopyable_audio_is_reencoded_with_a_warning() {
        let info = video_info("avc1", (1280, 720), 0, 3000, Some("samr"));
        let p = composite_plan(
            Path::new("m.mp4"),
            &info,
            None,
            Path::new("o.mp4"),
            MF,
            false,
            &VideoOptions::default(),
        )
        .unwrap();
        assert_eq!(p.audio, AudioPlan::Aac { kbps: 160 });
        assert_eq!(p.warnings[0].code(), "audio_reencoded");
    }

    #[test]
    fn bitrate_matches_source_with_floors() {
        let o = VideoOptions::default();
        let h264 = video_info("avc1", (1920, 1080), 90, 6000, None);
        assert_eq!(target_bitrate_kbps(&h264, (1080, 1920), &o), 6000);
        let hevc = video_info("hvc1", (1920, 1080), 90, 4000, None);
        assert_eq!(target_bitrate_kbps(&hevc, (1080, 1920), &o), 6000);
        // Starved source: the per-pixel floor wins (1080*1920*30*0.04/1000).
        let starved = video_info("avc1", (1920, 1080), 0, 300, None);
        assert_eq!(target_bitrate_kbps(&starved, (1920, 1080), &o), 2488);
        // Tiny source: the absolute floor wins.
        let tiny = video_info("avc1", (320, 180), 0, 100, None);
        assert_eq!(target_bitrate_kbps(&tiny, (320, 180), &o), 500);
        let huge = video_info("avc1", (3840, 2160), 0, 90_000, None);
        assert_eq!(target_bitrate_kbps(&huge, (3840, 2160), &o), 50_000);
    }

    #[test]
    fn software_mft_factor_applies_to_it_alone() {
        let info = video_info("avc1", (1920, 1080), 90, 6000, None);
        let o = VideoOptions::default();
        assert_eq!(o.mf_software_bitrate_factor, 1.3, "Q-04 default");
        assert_eq!(encoder_bitrate_kbps(&info, (1080, 1920), MF, &o), 7800);
        let hw = VideoEncoder::MediaFoundation { hardware: true };
        assert_eq!(encoder_bitrate_kbps(&info, (1080, 1920), hw, &o), 6000);
        assert_eq!(
            encoder_bitrate_kbps(&info, (1080, 1920), VideoEncoder::OpenH264, &o),
            6000
        );
        let literal = VideoOptions {
            mf_software_bitrate_factor: 1.0,
            ..VideoOptions::default()
        };
        assert_eq!(
            encoder_bitrate_kbps(&info, (1080, 1920), MF, &literal),
            6000,
            "1.0 keeps the source bitrate"
        );
    }

    #[test]
    fn transcode_plan_has_one_input() {
        let info = video_info("hvc1", (1920, 1080), 90, 4000, Some("aac"));
        let p = composite_plan(
            Path::new("hevc.mp4"),
            &info,
            None,
            Path::new("h264.mp4"),
            MF,
            false,
            &VideoOptions::default(),
        )
        .unwrap();
        let a = strs(&p);
        assert_eq!(a.iter().filter(|x| *x == "-i").count(), 1);
        assert_eq!(p.filter, "[0:v]transpose=clock,format=nv12[v]");
        // 4000 kbit/s HEVC x 1.5 for H.264, x 1.3 for the software MFT.
        assert_eq!(p.bitrate_kbps, 7800);
    }
}
