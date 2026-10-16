//! The Memories Rescue engine (SRS sections 3.7, 3.8, 4.2, 5.6 to 5.8).
//!
//! A library the Tauri layer and `mr-cli` drive. See `docs/api/mr-job.md`.

#![deny(unsafe_code)]

pub mod compositor;
pub mod control;
pub mod fingerprint;
pub mod layout;
pub mod options;
pub mod platform;
pub mod space;
