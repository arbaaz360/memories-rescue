//! Error and warning codes shared by the pipeline stages (SRS section 8).

use std::fmt;

/// Codes from the SRS error table that the M1 stages can raise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Code {
    /// Not a ZIP or folder with `memories/` or `json/`.
    InputNotExport,
    /// A gap in the part sequence, or no primary part.
    PartMissing,
    /// Central directory unreadable; entries recovered from local headers.
    PartTruncated,
    /// An entry's data does not match its CRC-32.
    ZipEntryCrc,
    /// An entry name refused for safety (NFR-PRV-07).
    EntryRefused,
    /// No `json/memories_history.json`.
    JsonMissing,
    /// `memories_history.json` is malformed.
    JsonParse,
    /// Legacy profile: links instead of media.
    LegacyLinksOnly,
    /// Exact time unknown for some items.
    MatchPartial,
    /// Ordinal match whose group locations disagree.
    LocationUncertain,
}

impl Code {
    /// The identifier used in reports and logs, as spelled in the SRS.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::InputNotExport => "INPUT_NOT_EXPORT",
            Code::PartMissing => "PART_MISSING",
            Code::PartTruncated => "PART_TRUNCATED",
            Code::ZipEntryCrc => "ZIP_ENTRY_CRC",
            Code::EntryRefused => "ENTRY_REFUSED",
            Code::JsonMissing => "JSON_MISSING",
            Code::JsonParse => "JSON_PARSE",
            Code::LegacyLinksOnly => "LEGACY_LINKS_ONLY",
            Code::MatchPartial => "MATCH_PARTIAL",
            Code::LocationUncertain => "LOCATION_UNCERTAIN",
        }
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One reported condition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub code: Code,
    /// Plain-language detail; never contains a full URL.
    pub detail: String,
    /// The part this concerns, when it concerns one.
    pub part: Option<usize>,
}

impl Issue {
    pub fn new(code: Code, detail: impl Into<String>) -> Issue {
        Issue {
            code,
            detail: detail.into(),
            part: None,
        }
    }

    pub fn for_part(code: Code, part: usize, detail: impl Into<String>) -> Issue {
        Issue {
            code,
            detail: detail.into(),
            part: Some(part),
        }
    }
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.part {
            Some(part) => write!(f, "{} (part #{}): {}", self.code, part, self.detail),
            None => write!(f, "{}: {}", self.code, self.detail),
        }
    }
}
