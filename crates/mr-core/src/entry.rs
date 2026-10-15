//! One entry of the `Saved Media` array, raw and parsed.
//!
//! Every field is optional and unknown fields are tolerated (FR-10). Field
//! names are matched after trimming and case-folding so that a casing change
//! by Snap does not silently drop a field.

use std::fmt;

use serde_json::{Map, Value};

use crate::fields::{
    self, DateField, Location, LocationParse, MediaTypeHint, UrlTokens, extract_url_tokens,
    redact_url,
};

/// The string fields of one entry as found in the file, before any parsing.
///
/// `Debug` redacts the two URL fields so the struct can be logged safely.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RawEntry {
    /// Zero-based position in the `Saved Media` array.
    pub index: usize,
    pub date: Option<String>,
    pub media_type: Option<String>,
    pub location: Option<String>,
    pub download_link: Option<String>,
    pub media_download_url: Option<String>,
    /// Names of fields this version does not know (values are not kept).
    pub unknown_fields: Vec<String>,
    /// Known fields whose value was neither a string nor `null`.
    pub non_string_fields: Vec<String>,
}

impl RawEntry {
    /// Build from a decoded JSON object. Known keys are matched trimmed and
    /// case-insensitively; `null` counts as missing; any other non-string
    /// value is recorded in `non_string_fields` and treated as missing.
    pub fn from_json_object(index: usize, object: Map<String, Value>) -> RawEntry {
        let mut raw = RawEntry {
            index,
            ..RawEntry::default()
        };
        for (key, value) in object {
            let slot = match key.trim().to_ascii_lowercase().as_str() {
                "date" => &mut raw.date,
                "media type" => &mut raw.media_type,
                "location" => &mut raw.location,
                "download link" => &mut raw.download_link,
                "media download url" => &mut raw.media_download_url,
                _ => {
                    raw.unknown_fields.push(key);
                    continue;
                }
            };
            match value {
                Value::String(s) => *slot = Some(s),
                Value::Null => {}
                _ => raw.non_string_fields.push(key),
            }
        }
        raw
    }
}

impl fmt::Debug for RawEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RawEntry")
            .field("index", &self.index)
            .field("date", &self.date)
            .field("media_type", &self.media_type)
            .field("location", &self.location)
            .field(
                "download_link",
                &self.download_link.as_deref().map(redact_url),
            )
            .field(
                "media_download_url",
                &self.media_download_url.as_deref().map(redact_url),
            )
            .field("unknown_fields", &self.unknown_fields)
            .field("non_string_fields", &self.non_string_fields)
            .finish()
    }
}

/// Something worth reporting about an entry that did not stop parsing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Warning {
    UnknownField(String),
    NonStringField(String),
    DateMissing,
    DateUnparseable,
    DateDayOnly,
    MediaTypeUnknown,
    LocationMissing,
    LocationUnrecognised,
    LocationOutOfRange,
}

/// An entry with every field run through the §3.5 rules.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEntry {
    pub index: usize,
    /// `None` when the `Date` field is absent.
    pub date: Option<DateField>,
    pub media_type: MediaTypeHint,
    pub location: Option<Location>,
    pub download_link: Option<String>,
    pub media_download_url: Option<String>,
    pub warnings: Vec<Warning>,
}

impl ParsedEntry {
    /// `true` when either URL field holds a value: the mark of the legacy format.
    pub fn has_url(&self) -> bool {
        self.download_link.is_some() || self.media_download_url.is_some()
    }

    /// `sid`/`mid` identifiers from the download link, falling back to the
    /// media URL; empty when neither carries them (the 2026 format).
    pub fn url_tokens(&self) -> UrlTokens {
        for url in [&self.download_link, &self.media_download_url]
            .into_iter()
            .flatten()
        {
            let tokens = extract_url_tokens(url);
            if !tokens.is_empty() {
                return tokens;
            }
        }
        UrlTokens::default()
    }
}

/// Parse a raw entry. Never fails: everything doubtful becomes a [`Warning`].
pub fn parse_entry(raw: &RawEntry) -> ParsedEntry {
    let mut warnings: Vec<Warning> = raw
        .unknown_fields
        .iter()
        .cloned()
        .map(Warning::UnknownField)
        .chain(
            raw.non_string_fields
                .iter()
                .cloned()
                .map(Warning::NonStringField),
        )
        .collect();

    let date = match raw.date.as_deref() {
        None => {
            warnings.push(Warning::DateMissing);
            None
        }
        Some(s) => {
            let parsed = fields::parse_date(s);
            match parsed {
                DateField::Instant(_) => {}
                DateField::DayOnly(_) => warnings.push(Warning::DateDayOnly),
                DateField::Unparseable => warnings.push(Warning::DateUnparseable),
            }
            Some(parsed)
        }
    };

    let media_type = fields::parse_media_type(raw.media_type.as_deref());
    if media_type == MediaTypeHint::Unknown {
        warnings.push(Warning::MediaTypeUnknown);
    }

    let location = match fields::parse_location(raw.location.as_deref()) {
        LocationParse::Found(loc) => Some(loc),
        LocationParse::Empty => {
            warnings.push(Warning::LocationMissing);
            None
        }
        LocationParse::Unrecognised => {
            warnings.push(Warning::LocationUnrecognised);
            None
        }
        // `0.0, 0.0` is how the export says "no GPS": not worth a warning.
        LocationParse::NullIsland => None,
        LocationParse::OutOfRange => {
            warnings.push(Warning::LocationOutOfRange);
            None
        }
    };

    ParsedEntry {
        index: raw.index,
        date,
        media_type,
        location,
        download_link: fields::parse_url(raw.download_link.as_deref()).map(str::to_string),
        media_download_url: fields::parse_url(raw.media_download_url.as_deref())
            .map(str::to_string),
        warnings,
    }
}

/// Running counts over a stream of parsed entries (feeds FR-07's summary).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub entries: usize,
    pub photos: usize,
    pub videos: usize,
    pub unknown_type: usize,
    pub with_instant: usize,
    pub with_location: usize,
    pub with_url: usize,
    /// Entries whose URL carries usable `sid`/`mid` parameters (the `2026a`
    /// exact-matching path of SRS v0.2 section 3.6).
    pub with_tokens: usize,
    pub with_warnings: usize,
}

impl Summary {
    pub fn record(&mut self, entry: &ParsedEntry) {
        self.entries += 1;
        match entry.media_type {
            MediaTypeHint::Photo => self.photos += 1,
            MediaTypeHint::Video => self.videos += 1,
            MediaTypeHint::Unknown => self.unknown_type += 1,
        }
        if matches!(entry.date, Some(DateField::Instant(_))) {
            self.with_instant += 1;
        }
        if entry.location.is_some() {
            self.with_location += 1;
        }
        if entry.has_url() {
            self.with_url += 1;
            if !entry.url_tokens().is_empty() {
                self.with_tokens += 1;
            }
        }
        if !entry.warnings.is_empty() {
            self.with_warnings += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn raw(v: Value) -> RawEntry {
        match v {
            Value::Object(m) => RawEntry::from_json_object(0, m),
            _ => panic!("object expected"),
        }
    }

    #[test]
    fn clean_2026_entry_has_no_warnings() {
        let e = parse_entry(&raw(json!({
            "Date": "2021-12-25 18:42:07 UTC",
            "Media Type": "Image",
            "Location": "Latitude, Longitude: 51.50735, -0.12776",
            "Download Link": "",
            "Media Download Url": "N/A"
        })));
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
        assert!(e.date.unwrap().instant().is_some());
        assert_eq!(e.media_type, MediaTypeHint::Photo);
        assert!(e.location.is_some());
        assert!(!e.has_url());
        assert!(e.url_tokens().is_empty());
    }

    #[test]
    fn legacy_entry_exposes_urls_and_tokens() {
        let e = parse_entry(&raw(json!({
            "Date": "2018-05-12 09:15:30 UTC",
            "Media Type": "Video",
            "Location": "Latitude, Longitude: 40.71280, -74.00600",
            "Download Link": "https://app.example.invalid/dmd/memories?uid=u&sid=3F2A9C1E-5B7D-4E1F-9A2B-0C3D4E5F6A7B&mid=A1B2C3D4-E5F6-4A7B-8C9D-0E1F2A3B4C5D&sig=x",
            "Media Download Url": "https://cdn.example.invalid/m/x.mp4?Expires=1"
        })));
        assert!(e.has_url());
        let t = e.url_tokens();
        assert_eq!(
            t.sid.as_deref(),
            Some("3F2A9C1E-5B7D-4E1F-9A2B-0C3D4E5F6A7B")
        );
        assert_eq!(
            t.mid.as_deref(),
            Some("A1B2C3D4-E5F6-4A7B-8C9D-0E1F2A3B4C5D")
        );
    }

    #[test]
    fn unknown_fields_missing_location_and_odd_values_only_warn() {
        let e = parse_entry(&raw(json!({
            "date": "2021-12-25T18:42:07Z",
            " Media Type ": "Clip",
            "Favourite": true,
            "Extra": {"nested": [1, 2, {"x": null}]},
            "Download Link": 42,
            "Media Download Url": null
        })));
        assert!(
            e.date.unwrap().instant().is_some(),
            "case-folded key still parsed"
        );
        assert_eq!(e.media_type, MediaTypeHint::Unknown);
        assert!(e.location.is_none());
        assert!(!e.has_url());
        let w = &e.warnings;
        assert!(w.contains(&Warning::UnknownField("Favourite".into())));
        assert!(w.contains(&Warning::UnknownField("Extra".into())));
        assert!(w.contains(&Warning::NonStringField("Download Link".into())));
        assert!(w.contains(&Warning::MediaTypeUnknown));
        assert!(w.contains(&Warning::LocationMissing));
        assert!(!w.contains(&Warning::DateMissing));
    }

    #[test]
    fn date_warnings() {
        let e = parse_entry(&raw(json!({"Date": "2021-12-25"})));
        assert!(e.warnings.contains(&Warning::DateDayOnly));
        let e = parse_entry(&raw(json!({"Date": "soon"})));
        assert!(e.warnings.contains(&Warning::DateUnparseable));
        let e = parse_entry(&raw(json!({})));
        assert!(e.warnings.contains(&Warning::DateMissing));
    }

    #[test]
    fn location_warnings() {
        for (text, want) in [
            ("Latitude, Longitude: 95, 0", Warning::LocationOutOfRange),
            ("somewhere", Warning::LocationUnrecognised),
            ("", Warning::LocationMissing),
        ] {
            let e = parse_entry(&raw(json!({"Location": text})));
            assert!(e.location.is_none());
            assert!(e.warnings.contains(&want), "{text:?}: {:?}", e.warnings);
        }
    }

    #[test]
    fn null_island_is_no_location_but_not_a_warning() {
        let e = parse_entry(&raw(json!({
            "Date": "2021-12-25 18:42:07 UTC",
            "Media Type": "Image",
            "Location": "Latitude, Longitude: 0.0, 0.0"
        })));
        assert!(e.location.is_none());
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
    }

    #[test]
    fn debug_output_never_contains_query_strings() {
        let r = raw(json!({
            "Download Link": "https://app.example.invalid/dmd?sid=SECRET&sig=SECRET2",
            "Media Download Url": "https://cdn.example.invalid/m.mp4?Signature=SECRET3"
        }));
        let text = format!("{r:?}");
        assert!(!text.contains("SECRET"), "{text}");
        assert!(text.contains("https://app.example.invalid/dmd?<redacted>"));
    }

    #[test]
    fn summary_counts() {
        let mut s = Summary::default();
        s.record(&parse_entry(&raw(json!({
            "Date": "2021-12-25 18:42:07 UTC", "Media Type": "Image",
            "Location": "Latitude, Longitude: 1, 2"
        }))));
        s.record(&parse_entry(&raw(json!({
            "Date": "2021-12-25", "Media Type": "Video",
            "Download Link": "https://x.invalid/?a=b"
        }))));
        s.record(&parse_entry(&raw(json!({}))));
        assert_eq!(
            s,
            Summary {
                entries: 3,
                photos: 1,
                videos: 1,
                unknown_type: 1,
                with_instant: 1,
                with_location: 1,
                with_url: 1,
                with_tokens: 0,
                with_warnings: 2,
            }
        );
    }
}
