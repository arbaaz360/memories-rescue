//! The compositing seam (FR-23, FR-24, FR-27, FR-51). The engine asks a
//! [`Compositor`] to burn an overlay into a photo or video, to make an HEVC
//! video H.264 for Windows compatibility, and to join a group of clips; the
//! media crate provides the real one ([`crate::media::MediaCompositor`]).
//! The default [`PassThrough`] declines everything, and the engine then
//! copies the main file and saves the overlay beside it, flagging the item
//! `composite_pending` so a later run with a real compositor can finish it.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Photo,
    Video,
}

/// One burn-in request. All paths are inside the item's temp directory on
/// the destination volume.
#[derive(Debug, Clone, Copy)]
pub struct CompositeRequest<'a> {
    pub kind: MediaKind,
    /// The main file as extracted (no metadata written yet).
    pub main: &'a Path,
    /// The transparent overlay PNG.
    pub overlay: &'a Path,
    /// Where to write the composite. The engine writes metadata onto it
    /// afterwards, so the compositor need not preserve any.
    pub out: &'a Path,
    /// Lower-case extension of the main file (`jpg`, `png`, `mp4`, `mov`).
    pub ext: &'a str,
    /// "Maximise Windows compatibility": H.264 output (FR-51).
    pub windows_compat: bool,
    pub item_key: &'a str,
}

/// Windows compatibility mode for a video without a burn-in (FR-51, SRS
/// section 4.4 "HEVC sources"): write an H.264 copy to `out` if `main` needs
/// one. Paths are inside the item's temp directory.
#[derive(Debug, Clone, Copy)]
pub struct TranscodeRequest<'a> {
    pub main: &'a Path,
    pub out: &'a Path,
    pub ext: &'a str,
    pub item_key: &'a str,
}

/// One clip of a group to join, in order.
#[derive(Debug, Clone, Copy)]
pub struct RejoinSegment<'a> {
    pub main: &'a Path,
    /// The segment's overlay, when the overlay mode burns in.
    pub overlay: Option<&'a Path>,
}

/// Join a group of clips into one video (FR-27, SRS section 4.5).
#[derive(Debug, Clone, Copy)]
pub struct RejoinRequest<'a> {
    pub group_id: &'a str,
    pub segments: &'a [RejoinSegment<'a>],
    pub out: &'a Path,
}

/// Something the report should mention about an item that still succeeded,
/// with a lower-case code such as `overlay_aspect_mismatch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompositeWarning {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompositeOutcome {
    /// `out` holds the result.
    Composited,
    /// `out` holds the result, and the report should carry these warnings.
    CompositedWithWarnings(Vec<CompositeWarning>),
    /// This compositor cannot or need not do it; the engine keeps the input
    /// (for a burn-in: original + overlay beside it).
    NotSupported,
}

impl CompositeOutcome {
    /// `Composited` or `CompositedWithWarnings`.
    pub fn produced(&self) -> bool {
        !matches!(self, CompositeOutcome::NotSupported)
    }

    pub fn warnings(&self) -> &[CompositeWarning] {
        match self {
            CompositeOutcome::CompositedWithWarnings(w) => w,
            _ => &[],
        }
    }
}

/// A compositing failure, with an SRS section 8 code such as
/// `FFMPEG_FAIL`, `MEDIA_DECODE` or `ENCODER_UNAVAILABLE`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code}: {detail}")]
pub struct CompositeError {
    pub code: String,
    pub detail: String,
}

/// Burns overlays in. Called from worker threads, so it must be `Send + Sync`.
pub trait Compositor: Send + Sync {
    fn composite(&self, req: &CompositeRequest<'_>) -> Result<CompositeOutcome, CompositeError>;

    /// FR-51: make `req.main` H.264 when it needs it. `NotSupported` keeps
    /// the main file as it is (not HEVC, or no media support).
    fn compat_transcode(
        &self,
        _req: &TranscodeRequest<'_>,
    ) -> Result<CompositeOutcome, CompositeError> {
        Ok(CompositeOutcome::NotSupported)
    }

    /// FR-27: join the segments into `req.out`. `NotSupported` skips the
    /// group; the clips themselves are always written as separate items.
    fn rejoin(&self, _req: &RejoinRequest<'_>) -> Result<CompositeOutcome, CompositeError> {
        Ok(CompositeOutcome::NotSupported)
    }

    /// For logs and the report.
    fn name(&self) -> &str {
        "compositor"
    }

    /// Whether this compositor can burn overlays in at all. Only the
    /// pass-through says no; the plan uses it to decide whether to warn that
    /// overlays will be saved beside the files (`COMPOSITE_PENDING`).
    fn can_burn_in(&self) -> bool {
        true
    }
}

/// The default: never composites.
#[derive(Debug, Default, Clone, Copy)]
pub struct PassThrough;

impl Compositor for PassThrough {
    fn composite(&self, _req: &CompositeRequest<'_>) -> Result<CompositeOutcome, CompositeError> {
        Ok(CompositeOutcome::NotSupported)
    }

    fn name(&self) -> &str {
        "pass-through"
    }

    fn can_burn_in(&self) -> bool {
        false
    }
}
