//! The compositing seam (FR-23, FR-24). The engine asks a [`Compositor`] to
//! burn an overlay into a photo or video; the media crate provides the real
//! one. The default [`PassThrough`] declines, and the engine then copies the
//! main file and saves the overlay beside it, flagging the item
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeOutcome {
    /// `out` holds the composite.
    Composited,
    /// This compositor cannot do it (yet); fall back to original + overlay.
    NotSupported,
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

    /// For logs and the report.
    fn name(&self) -> &str {
        "compositor"
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
}
