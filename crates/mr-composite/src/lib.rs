//! Media for Memories Rescue (SRS §4.4, §4.5, FR-23, FR-24, FR-27, FR-51).
//!
//! * [`probe()`] reads what a photo or video is natively, from its boxes and
//!   segments (no ffprobe sidecar).
//! * [`composite_photo`] puts an overlay onto a photo with the `image` crate (FR-23).
//! * [`Compositor`] holds the ffmpeg-backed operations the job engine calls:
//!   [`Compositor::composite_video`] (FR-24), [`Compositor::transcode_h264`]
//!   (Windows compatibility mode) and [`Compositor::rejoin`] (FR-27), with
//!   the §4.4 encoder choice, timeout and one automatic retry.
//! * [`detect_clip_groups`] applies the §4.5 heuristic.
//! * [`hevc_playback_available`] answers FR-51.
//!
//! The pure planners in [`video`] and [`rejoin`] build every ffmpeg argument
//! array; they need no ffmpeg and are what the CI unit tests cover. The API
//! is documented in `docs/api/mr-composite.md`.

pub mod ffmpeg;
pub mod hevc;
pub mod photo;
pub mod probe;
pub mod rejoin;
pub mod video;

pub use ffmpeg::{EncoderPlan, EncoderSet, Ffmpeg, Platform, VideoEncoder, select_encoder};
pub use hevc::{HevcPlayback, hevc_playback, hevc_playback_available};
pub use photo::{PhotoFormat, PhotoOptions, PhotoOutcome, composite_photo};
pub use probe::{AudioTrack, Container, ImageInfo, MediaInfo, VideoTrack, probe, probe_reader};
pub use rejoin::{
    ClipDescriptor, ClipGroup, NotJoined, RejoinOptions, Segment, YearRange, can_follow,
    detect_clip_groups,
};
pub use video::{AudioPlan, Placement, VideoOptions, Warning, place_overlay};

use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;

use crate::video::{OverlayInput, Plan, composite_plan};

/// Everything that can go wrong. [`Error::code`] gives the SRS §8 code the
/// engine reports.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] io::Error),
    /// The file is not something this crate reads (wrong container, no
    /// `moov`, no video track, segments that cannot be joined).
    #[error("unsupported media: {0}")]
    Unsupported(String),
    /// A photo or overlay could not be decoded (§8 `MEDIA_DECODE`).
    #[error("cannot decode {what}: {message}")]
    MediaDecode { what: String, message: String },
    /// No H.264 encoder works on this machine (§8 `ENCODER_UNAVAILABLE`).
    #[error("no usable H.264 encoder: {0}")]
    EncoderUnavailable(String),
    /// ffmpeg exited non-zero or timed out, after the retry (§8 `FFMPEG_FAIL`).
    #[error("ffmpeg failed: {message}")]
    FfmpegFail {
        message: String,
        timed_out: bool,
        /// Tail of ffmpeg's stderr, for diagnostics.
        stderr: String,
    },
    /// The ffmpeg executable could not be started.
    #[error("ffmpeg not available: {0}")]
    FfmpegMissing(String),
}

impl Error {
    /// The SRS §8 error code for the report.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Io(_) => "UNKNOWN",
            Error::Unsupported(_) | Error::MediaDecode { .. } => "MEDIA_DECODE",
            Error::EncoderUnavailable(_) => "ENCODER_UNAVAILABLE",
            Error::FfmpegFail { .. } | Error::FfmpegMissing(_) => "FFMPEG_FAIL",
        }
    }
}

/// A directory under the system temp directory, removed on drop.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn temp_dir(tag: &str) -> io::Result<TempDir> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("mr-composite-{}-{n}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(TempDir(dir))
}

/// What a video operation did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct VideoOutcome {
    /// The encoder that produced the output; `None` for a stream-copy join.
    pub encoder: Option<VideoEncoder>,
    /// The first attempt failed and the software path succeeded.
    pub retried: bool,
    /// The first attempt's error, when `retried`.
    pub first_failure: Option<String>,
    pub output_size: (u32, u32),
    pub bitrate_kbps: Option<u32>,
    pub audio: AudioPlan,
    pub warnings: Vec<Warning>,
    pub elapsed: Duration,
    /// The ffmpeg arguments that produced the output, for the log.
    pub args: Vec<String>,
}

/// What [`Compositor::composite_any`] did.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "outcome", rename_all = "lowercase")]
pub enum Composite {
    Photo(PhotoOutcome),
    Video(VideoOutcome),
}

impl Composite {
    /// Report-worthy warnings, e.g. `overlay_aspect_mismatch`.
    pub fn warnings(&self) -> &[Warning] {
        match self {
            Composite::Photo(p) => &p.warnings,
            Composite::Video(v) => &v.warnings,
        }
    }
}

/// The ffmpeg-backed operations, bound to one ffmpeg executable.
///
/// Cheap to share by reference across worker threads: the encoder listing
/// and the encoder smoke tests run once and are cached.
#[derive(Debug)]
pub struct Compositor {
    ffmpeg: Ffmpeg,
    options: VideoOptions,
    photo: PhotoOptions,
    plan: OnceLock<Result<EncoderPlan, (bool, String)>>,
}

impl Compositor {
    /// Bind to the ffmpeg sidecar at `ffmpeg`.
    pub fn new(ffmpeg: impl Into<PathBuf>) -> Self {
        Self::with_options(ffmpeg, VideoOptions::default())
    }

    pub fn with_options(ffmpeg: impl Into<PathBuf>, options: VideoOptions) -> Self {
        Self {
            ffmpeg: Ffmpeg::new(ffmpeg),
            options,
            photo: PhotoOptions::default(),
            plan: OnceLock::new(),
        }
    }

    /// Photo settings for [`Compositor::composite_any`].
    pub fn with_photo_options(mut self, photo: PhotoOptions) -> Self {
        self.photo = photo;
        self
    }

    pub fn ffmpeg(&self) -> &Ffmpeg {
        &self.ffmpeg
    }

    pub fn options(&self) -> &VideoOptions {
        &self.options
    }

    /// `ffmpeg -encoders`, probed once.
    pub fn encoders(&self) -> Result<&EncoderSet, Error> {
        self.ffmpeg.encoders()
    }

    /// The §4.4 encoder choice for this machine, smoke-tested once. An error
    /// here is `ENCODER_UNAVAILABLE` (or `FFMPEG_FAIL` when ffmpeg itself
    /// will not start): the engine disables video compositing and still
    /// fixes metadata.
    pub fn encoder_plan(&self) -> Result<EncoderPlan, Error> {
        let cached = self.plan.get_or_init(|| {
            self.ffmpeg
                .encoder_plan(self.options.prefer_hardware)
                .map_err(|e| (matches!(e, Error::EncoderUnavailable(_)), e.to_string()))
        });
        match cached {
            Ok(p) => Ok(*p),
            Err((true, m)) => Err(Error::EncoderUnavailable(m.clone())),
            Err((false, m)) => Err(Error::FfmpegMissing(m.clone())),
        }
    }

    /// One burn-in, photo or video decided by the file's content: the shape
    /// of the job engine's seam (`mr_job::Compositor::composite`). Photos go
    /// through [`composite_photo`] and need no ffmpeg; videos through
    /// [`Compositor::composite_video`].
    pub fn composite_any(
        &self,
        main: &Path,
        overlay: &Path,
        out: &Path,
    ) -> Result<Composite, Error> {
        if probe(main)?.is_video() {
            self.composite_video(main, overlay, out)
                .map(Composite::Video)
        } else {
            composite_photo(main, overlay, out, &self.photo).map(Composite::Photo)
        }
    }

    /// FR-24: burn `overlay` into `main`, writing H.264 to `out`.
    pub fn composite_video(
        &self,
        main: &Path,
        overlay: &Path,
        out: &Path,
    ) -> Result<VideoOutcome, Error> {
        self.encode(main, Some(overlay), out)
    }

    /// Windows compatibility mode (§4.4): re-encode `main` (typically HEVC)
    /// to H.264 with no overlay, rotation baked in.
    pub fn transcode_h264(&self, main: &Path, out: &Path) -> Result<VideoOutcome, Error> {
        self.encode(main, None, out)
    }

    fn encode(
        &self,
        main: &Path,
        overlay: Option<&Path>,
        out: &Path,
    ) -> Result<VideoOutcome, Error> {
        let info = probe(main)?;
        let ov_size = match overlay {
            Some(p) => Some(overlay_size(p)?),
            None => None,
        };
        let encoders = self.encoder_plan()?;
        let ov = overlay
            .zip(ov_size)
            .map(|(path, size)| OverlayInput { path, size });
        let first = composite_plan(main, &info, ov, out, encoders.primary, false, &self.options)?;
        let retry = || composite_plan(main, &info, ov, out, encoders.software, true, &self.options);
        self.run_with_retry(first, retry, out)
    }

    /// FR-27: join `segments` (in order) into `out`. Video is stream-copied
    /// unless a segment has an overlay or the segments' decoder
    /// configurations differ; audio is re-encoded to AAC (see [`rejoin`]).
    pub fn rejoin(&self, segments: &[Segment], out: &Path) -> Result<VideoOutcome, Error> {
        if segments.len() < 2 {
            return Err(Error::Unsupported(
                "rejoin needs two or more segments".into(),
            ));
        }
        let infos = segments
            .iter()
            .map(|s| probe(&s.path))
            .collect::<Result<Vec<_>, _>>()?;
        let same_config = rejoin::check_segments(&infos)?;
        let ov_sizes = segments
            .iter()
            .map(|s| s.overlay.as_deref().map(overlay_size).transpose())
            .collect::<Result<Vec<_>, _>>()?;
        let overlays: Vec<Option<OverlayInput<'_>>> = segments
            .iter()
            .zip(&ov_sizes)
            .map(|(s, size)| {
                s.overlay
                    .as_deref()
                    .zip(*size)
                    .map(|(path, size)| OverlayInput { path, size })
            })
            .collect();
        let dir = temp_dir("rejoin")?;
        let list = dir.path().join("segments.ffconcat");
        let paths: Vec<PathBuf> = segments
            .iter()
            .map(|s| std::path::absolute(&s.path))
            .collect::<Result<_, _>>()?;
        let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
        std::fs::write(&list, rejoin::concat_list(&refs))?;

        let copy = same_config && overlays.iter().all(Option::is_none);
        let first_encoder = if copy {
            None
        } else {
            Some(self.encoder_plan()?.primary)
        };
        let first =
            rejoin::rejoin_plan(&list, &infos, &overlays, out, first_encoder, &self.options)?;
        let retry = || {
            // The software path re-encodes even when the first try was a copy.
            let enc = self.encoder_plan()?.software;
            rejoin::rejoin_plan(&list, &infos, &overlays, out, Some(enc), &self.options)
        };
        self.run_with_retry(first, retry, out)
    }

    /// Run `first`; on `FFMPEG_FAIL` run the plan `retry` builds, once.
    /// Output goes to a partial file next to `out` and is renamed on success.
    fn run_with_retry(
        &self,
        first: Plan,
        retry: impl FnOnce() -> Result<Plan, Error>,
        out: &Path,
    ) -> Result<VideoOutcome, Error> {
        match self.run_plan(&first, out) {
            Ok(elapsed) => Ok(outcome(&first, elapsed, None)),
            Err(e1 @ Error::FfmpegFail { .. }) => {
                log::warn!("FFMPEG_FAIL, retrying on the software path: {e1}");
                let second = retry()?;
                match self.run_plan(&second, out) {
                    Ok(elapsed) => Ok(outcome(&second, elapsed, Some(e1.to_string()))),
                    Err(Error::FfmpegFail {
                        message,
                        timed_out,
                        stderr,
                    }) => Err(Error::FfmpegFail {
                        message: format!("{e1}; retry failed too: {message}"),
                        timed_out,
                        stderr,
                    }),
                    Err(e2) => Err(e2),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Run one plan into a partial file, then rename it to `out`.
    fn run_plan(&self, plan: &Plan, out: &Path) -> Result<Duration, Error> {
        let partial = partial_video_path(out);
        let mut args = plan.args.clone();
        // The planners put `out` last; swap it for the partial path, which
        // keeps the extension so ffmpeg still picks the muxer from it.
        if let Some(last) = args.last_mut() {
            *last = partial.clone().into_os_string();
        }
        for w in &plan.warnings {
            w.log();
        }
        let result = self.ffmpeg.run(&args, plan.timeout).and_then(|r| {
            std::fs::rename(&partial, out)?;
            Ok(r.elapsed)
        });
        if result.is_err() {
            let _ = std::fs::remove_file(&partial);
        }
        result
    }
}

fn outcome(plan: &Plan, elapsed: Duration, first_failure: Option<String>) -> VideoOutcome {
    VideoOutcome {
        encoder: plan.encoder,
        retried: first_failure.is_some(),
        first_failure,
        output_size: plan.output_size,
        bitrate_kbps: plan.encoder.map(|_| plan.bitrate_kbps),
        audio: plan.audio.clone(),
        warnings: plan.warnings.clone(),
        elapsed,
        args: plan.args_lossy(),
    }
}

fn overlay_size(path: &Path) -> Result<(u32, u32), Error> {
    probe(path)?
        .image
        .map(|i| (i.width, i.height))
        .ok_or_else(|| Error::Unsupported(format!("{}: overlay is not a PNG/JPEG", path.display())))
}

/// `<dir>/.<stem>.mr-partial.<ext>`: same extension, so ffmpeg chooses the
/// same muxer.
fn partial_video_path(out: &Path) -> PathBuf {
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".into());
    let ext = out
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "mp4".into());
    out.with_file_name(format!(".{stem}.mr-partial.{ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compositor_is_shareable_across_worker_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Compositor>();
        assert_send_sync::<VideoOutcome>();
    }

    #[test]
    fn error_codes() {
        assert_eq!(
            Error::EncoderUnavailable("x".into()).code(),
            "ENCODER_UNAVAILABLE"
        );
        assert_eq!(
            Error::FfmpegFail {
                message: "m".into(),
                timed_out: true,
                stderr: String::new()
            }
            .code(),
            "FFMPEG_FAIL"
        );
        assert_eq!(Error::Unsupported("x".into()).code(), "MEDIA_DECODE");
    }

    #[test]
    fn partial_paths_keep_the_extension() {
        assert_eq!(
            partial_video_path(Path::new("out/2021/06/clip.mp4")),
            Path::new("out/2021/06/.clip.mr-partial.mp4")
        );
        assert_eq!(
            partial_video_path(Path::new("a.mov")),
            Path::new(".a.mr-partial.mov")
        );
    }

    /// The staged sidecar, else ffmpeg on PATH; `None` on CI.
    fn test_ffmpeg() -> Option<PathBuf> {
        let exe = if cfg!(windows) {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        };
        let bundled = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../third_party/ffmpeg")
            .join(exe);
        let works = |p: &Path| {
            std::process::Command::new(p)
                .arg("-version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        };
        if bundled.is_file()
            && std::fs::metadata(&bundled).is_ok_and(|m| m.len() > 0)
            && works(&bundled)
        {
            return Some(bundled);
        }
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(exe))
            .find(|p| p.is_file() && works(p))
    }

    fn lavfi_plan(out: &Path, filter: &str) -> Plan {
        let mut args = video::head_args();
        for a in [
            "-f",
            "lavfi",
            "-i",
            "color=c=gray:s=64x64:r=10:d=0.3",
            "-vf",
            filter,
            "-c:v",
            "mpeg4",
        ] {
            args.push(a.into());
        }
        args.push(out.as_os_str().to_owned());
        Plan {
            args,
            timeout: Duration::from_secs(30),
            filter: filter.into(),
            output_size: (64, 64),
            bitrate_kbps: 0,
            audio: AudioPlan::None,
            encoder: Some(VideoEncoder::OpenH264),
            warnings: vec![],
        }
    }

    #[test]
    fn one_retry_on_the_software_path() {
        let Some(ff) = test_ffmpeg() else {
            eprintln!("skip: no ffmpeg");
            return;
        };
        let c = Compositor::new(ff);
        let dir = temp_dir("retry").unwrap();
        let out = dir.path().join("o.mp4");
        let bad = lavfi_plan(&out, "no_such_filter_anywhere");
        let good = lavfi_plan(&out, "format=yuv420p");
        let o = c.run_with_retry(bad, || Ok(good), &out).unwrap();
        assert!(o.retried);
        assert!(
            o.first_failure.as_deref().unwrap().contains("exit"),
            "{:?}",
            o.first_failure
        );
        assert!(out.is_file());
        assert!(!partial_video_path(&out).exists());

        // Both attempts fail: FFMPEG_FAIL naming both, nothing left behind.
        let out2 = dir.path().join("o2.mp4");
        let e = c
            .run_with_retry(
                lavfi_plan(&out2, "nope1"),
                || Ok(lavfi_plan(&out2, "nope2")),
                &out2,
            )
            .unwrap_err();
        assert_eq!(e.code(), "FFMPEG_FAIL");
        assert!(e.to_string().contains("retry failed too"), "{e}");
        assert!(!out2.exists() && !partial_video_path(&out2).exists());
    }

    #[test]
    fn timeout_kills_ffmpeg() {
        let Some(ff) = test_ffmpeg() else {
            eprintln!("skip: no ffmpeg");
            return;
        };
        let f = Ffmpeg::new(ff);
        let args: Vec<std::ffi::OsString> = [
            "-hide_banner",
            "-nostdin",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=64x64:r=10:d=60",
            "-f",
            "null",
            "-",
        ]
        .iter()
        .map(Into::into)
        .collect();
        let t = std::time::Instant::now();
        let e = f.run(&args, Duration::from_millis(500)).unwrap_err();
        assert!(
            matches!(
                e,
                Error::FfmpegFail {
                    timed_out: true,
                    ..
                }
            ),
            "{e}"
        );
        assert!(t.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn missing_ffmpeg_is_reported_not_panicked() {
        let c = Compositor::new("definitely-not-ffmpeg-here.exe");
        let e = c.encoder_plan().unwrap_err();
        assert_eq!(e.code(), "FFMPEG_FAIL");
    }
}
