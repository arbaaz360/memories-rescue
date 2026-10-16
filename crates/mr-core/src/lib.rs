//! Memories Rescue core library: the data formats of a Snapchat "Download My
//! Data" Memories export (SRS §3).
//!
//! M0 scope, all pure and I/O-free beyond `std::io::Read`:
//!
//! * [`filename`]: the `memories/` filename patterns (§3.2).
//! * [`fields`]: field parsing rules for `memories_history.json` entries (§3.5).
//! * [`entry`]: one `Saved Media` entry, raw and parsed, with warnings.
//! * [`stream`]: a streaming reader for the `Saved Media` array that never
//!   holds the whole file in memory (FR-10, NFR-PERF-02).
//!
//! Matching JSON entries to files (§3.6), ZIP indexing and metadata writing
//! arrive in M1 and later.

#![forbid(unsafe_code)]

pub mod discover;
pub mod entry;
pub mod fields;
pub mod filename;
pub mod index;
pub mod issue;
pub mod matcher;
pub mod redact;
pub mod stream;
pub mod zipindex;

pub use discover::{Discovery, Export, Part, PartEntry, discover};
pub use entry::{ParsedEntry, RawEntry, Summary, Warning};
pub use fields::{
    CaptureDate, DateField, DatePrecision, Location, LocationParse, MediaType, MediaTypeHint,
    UrlTokens,
};
pub use filename::{FileKind, ParsedName};
pub use index::{
    EntryRef, ExportSummary, JsonLoad, MediaFile, MergedIndex, Profile, build_index, load_json,
    summarize,
};
pub use issue::{Code, Issue};
pub use matcher::{
    Confidence, Item, ItemStatus, LocationDecision, MatchOptions, MatchReport, OrderAgreement,
    OrderVerdict, match_export, order_agreement,
};
pub use stream::{SavedMediaReader, StreamError};
pub use zipindex::{CrcCheck, EntryInfo, Integrity, UnsafeReason, ZipIndex, ZipIndexError};
