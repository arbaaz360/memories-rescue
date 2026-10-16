//! The Memories Rescue engine (SRS sections 3.7, 3.8, 4.1, 4.2, 5.6 to 5.8).
//!
//! A library that the Tauri layer and `mr-cli` drive; the full API is in
//! `docs/api/mr-job.md`. In short:
//!
//! * [`plan`]: the dry run (FR-29). Writes nothing.
//! * [`Job::create`] / [`Job::open`]: a resumable job whose state lives in a
//!   per-user state directory, keyed by its output folder (FR-33).
//! * [`Job::run`]: processes pending items with a bounded worker pool,
//!   reporting to a [`ProgressSink`] and obeying a [`Control`] (FR-32, FR-34).
//! * [`Job::retry_failed`], [`Job::add_part`], [`Job::unlock`],
//!   [`Job::report`], [`list_jobs`].
//! * [`Compositor`]: the seam where overlay burn-in, the Windows
//!   compatibility transcode and clip rejoin plug in; [`MediaCompositor`] is
//!   the real one (`mr-composite`).

#![deny(unsafe_code)]

mod clips;
pub mod compositor;
pub mod control;
pub mod error;
pub mod fingerprint;
pub mod job;
pub mod layout;
pub mod media;
pub mod options;
pub mod plan;
pub mod platform;
pub mod process;
pub mod space;
pub mod state;

pub use compositor::{
    CompositeError, CompositeOutcome, CompositeRequest, CompositeWarning, Compositor, MediaKind,
    PassThrough, RejoinRequest, RejoinSegment, TranscodeRequest,
};
pub use control::{CollectingSink, Control, ItemOutcome, Progress, ProgressSink, Stage};
pub use error::{JobError, Result};
pub use fingerprint::InputChange;
pub use job::{Job, JobSummary, Outcome, Replanned, RunStats, find_job_for_output, list_jobs};
pub use media::MediaCompositor;
pub use options::{ClipRejoin, FREE_TIER_ITEMS, Options, OverlayMode, TestHooks};
pub use plan::{ExtraKind, ExtraRow, Plan, PlannedItem, Precision, SourceRef, Warning, plan};
pub use space::{SpaceAction, SpaceEstimate, SpaceOption};
pub use state::{ItemRow, JournalEvent};
