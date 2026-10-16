//! The real compositor and the media facts the plan needs, both from
//! `mr-composite` (SRS sections 4.4 and 4.5, FR-23, FR-24, FR-27, FR-51).
//!
//! The shell plugs the compositor in with
//! `job.set_compositor(Arc::new(MediaCompositor::new(sidecar_ffmpeg)))`.
//! Photos need no ffmpeg; videos use the sidecar, and when no H.264 encoder
//! works on the machine (`ENCODER_UNAVAILABLE`, e.g. a Windows N edition
//! without the Media Feature Pack) video burn-ins are declined
//! (`NotSupported`): the item is written with its overlay beside it and
//! marked `composite_pending`, so `Job::requeue_composite_pending` can finish
//! it once the component is installed.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use mr_composite::{MediaInfo, Segment, Warning};
use mr_core::discover::{Discovery, Source};

use crate::compositor::{
    CompositeError, CompositeOutcome, CompositeRequest, CompositeWarning, Compositor,
    RejoinRequest, TranscodeRequest,
};
use crate::plan::SourceRef;

pub use mr_composite::{EncoderPlan, hevc_playback, hevc_playback_available};

/// [`Compositor`] backed by `mr_composite::Compositor`.
#[derive(Debug)]
pub struct MediaCompositor {
    inner: mr_composite::Compositor,
}

impl MediaCompositor {
    /// Bind to the ffmpeg sidecar.
    pub fn new(ffmpeg: impl Into<PathBuf>) -> Self {
        Self {
            inner: mr_composite::Compositor::new(ffmpeg),
        }
    }

    /// With explicit video and photo options.
    pub fn with(inner: mr_composite::Compositor) -> Self {
        Self { inner }
    }

    pub fn inner(&self) -> &mr_composite::Compositor {
        &self.inner
    }

    /// The video encoders this machine can use, or `ENCODER_UNAVAILABLE`.
    /// Cached after the first call; the UI can show it before a run.
    pub fn encoder_plan(&self) -> Result<EncoderPlan, CompositeError> {
        self.inner.encoder_plan().map_err(error)
    }
}

fn error(e: mr_composite::Error) -> CompositeError {
    CompositeError {
        code: e.code().into(),
        detail: e.to_string(),
    }
}

fn warnings(w: &[Warning]) -> CompositeOutcome {
    if w.is_empty() {
        return CompositeOutcome::Composited;
    }
    CompositeOutcome::CompositedWithWarnings(
        w.iter()
            .map(|w| CompositeWarning {
                code: w.code().to_string(),
                detail: match w {
                    Warning::OverlayAspectMismatch {
                        overlay, target, ..
                    } => format!(
                        "overlay {}x{} on {}x{}: fitted to the width and centred",
                        overlay.0, overlay.1, target.0, target.1
                    ),
                    Warning::OddSizeScaled { from, to } => {
                        format!("{}x{} scaled to {}x{}", from.0, from.1, to.0, to.1)
                    }
                    Warning::AudioReencoded { from } => format!("{from} audio re-encoded to AAC"),
                },
            })
            .collect(),
    )
}

/// `ENCODER_UNAVAILABLE` is not an item failure: video burn-in is off on
/// this machine until the component is installed (SRS section 8).
fn declined_or(e: mr_composite::Error) -> Result<CompositeOutcome, CompositeError> {
    match e {
        mr_composite::Error::EncoderUnavailable(_) => Ok(CompositeOutcome::NotSupported),
        other => Err(error(other)),
    }
}

/// `can_burn_in` keeps the trait's `true`: photos never need ffmpeg, and
/// when no H.264 encoder works each video burn-in is declined on its own
/// (`composite_pending`), so the plan's `COMPOSITE_PENDING` warning would be
/// wrong for every photo.
impl Compositor for MediaCompositor {
    fn composite(&self, req: &CompositeRequest<'_>) -> Result<CompositeOutcome, CompositeError> {
        match self.inner.composite_any(req.main, req.overlay, req.out) {
            Ok(c) => Ok(warnings(c.warnings())),
            Err(e) => declined_or(e),
        }
    }

    fn compat_transcode(
        &self,
        req: &TranscodeRequest<'_>,
    ) -> Result<CompositeOutcome, CompositeError> {
        let info = mr_composite::probe(req.main).map_err(error)?;
        if !is_hevc(&info) {
            return Ok(CompositeOutcome::NotSupported);
        }
        match self.inner.transcode_h264(req.main, req.out) {
            Ok(v) => Ok(warnings(&v.warnings)),
            Err(e) => declined_or(e),
        }
    }

    fn rejoin(&self, req: &RejoinRequest<'_>) -> Result<CompositeOutcome, CompositeError> {
        let segments: Vec<Segment> = req
            .segments
            .iter()
            .map(|s| Segment {
                path: s.main.to_path_buf(),
                overlay: s.overlay.map(Path::to_path_buf),
            })
            .collect();
        match self.inner.rejoin(&segments, req.out) {
            Ok(v) => Ok(warnings(&v.warnings)),
            Err(e) => declined_or(e),
        }
    }

    fn name(&self) -> &str {
        "mr-composite"
    }
}

/// `hvc1` or `hev1`.
pub fn is_hevc(info: &MediaInfo) -> bool {
    info.video
        .as_ref()
        .is_some_and(|v| matches!(v.codec.as_str(), "hvc1" | "hev1"))
}

/// Probe a video entry where it lies, without extracting it: the entry is
/// streamed and reading stops at `moov`, or after `limit` bytes when `moov`
/// comes after the media data. `None` when it cannot be read or parsed.
pub fn probe_source(
    d: &Discovery,
    part_by_path: &HashMap<String, usize>,
    src: &SourceRef,
    limit: u64,
) -> Option<MediaInfo> {
    let &part = part_by_path.get(&src.part_path)?;
    match &d.part(part).source {
        Source::Zip(z) => {
            let index = match z.entry(src.entry_index) {
                Some(e) if e.name == src.entry_name => src.entry_index,
                _ => z.find(&src.entry_name)?.index,
            };
            let mut reader = z.reader().ok()?;
            reader
                .with_entry(index, |r: &mut dyn Read| {
                    mr_composite::probe_reader(r, src.size, limit)
                        .map_err(|e| std::io::Error::other(e.to_string()))
                })
                .ok()
        }
        Source::Folder(folder) => {
            let path = folder
                .root
                .join(src.entry_name.replace('/', std::path::MAIN_SEPARATOR_STR));
            mr_composite::probe(&path).ok()
        }
    }
}

/// Map from a discovered part's path to its id, as the workers use it.
pub fn part_ids(d: &Discovery) -> HashMap<String, usize> {
    d.parts
        .iter()
        .map(|p| (p.path.to_string_lossy().into_owned(), p.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoder_unavailable_declines_and_other_errors_keep_their_code() {
        let declined = declined_or(mr_composite::Error::EncoderUnavailable("N edition".into()));
        assert_eq!(declined, Ok(CompositeOutcome::NotSupported));
        let failed = declined_or(mr_composite::Error::FfmpegFail {
            message: "exit 1".into(),
            timed_out: false,
            stderr: String::new(),
        })
        .unwrap_err();
        assert_eq!(failed.code, "FFMPEG_FAIL");
        let decode = declined_or(mr_composite::Error::Unsupported("x".into())).unwrap_err();
        assert_eq!(decode.code, "MEDIA_DECODE");
    }

    #[test]
    fn warnings_become_report_codes() {
        assert_eq!(warnings(&[]), CompositeOutcome::Composited);
        let o = warnings(&[Warning::OverlayAspectMismatch {
            overlay: (1080, 1920),
            target: (1080, 2340),
            difference: 0.18,
        }]);
        assert!(o.produced());
        assert_eq!(o.warnings()[0].code, "overlay_aspect_mismatch");
        assert!(o.warnings()[0].detail.contains("1080x1920 on 1080x2340"));
    }

    #[test]
    fn photos_composite_without_ffmpeg() {
        // A compositor whose ffmpeg does not exist still burns overlays into photos.
        let png = Path::new(env!("CARGO_MANIFEST_DIR")).join("../mr-meta/tests/data/tiny.png");
        let dir = std::env::temp_dir().join(format!("mr-job-media-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.png");
        let c = MediaCompositor::new(dir.join("no-ffmpeg-here.exe"));
        let req = CompositeRequest {
            kind: crate::compositor::MediaKind::Photo,
            main: &png,
            overlay: &png,
            out: &out,
            ext: "png",
            windows_compat: false,
            item_key: "k",
        };
        assert_eq!(c.composite(&req), Ok(CompositeOutcome::Composited));
        assert!(out.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
