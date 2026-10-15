//! Field parsing rules for `memories_history.json` entries (SRS §3.5).
//!
//! | Field | Rule |
//! |---|---|
//! | `Date` | `YYYY-MM-DD HH:MM:SS UTC`, tolerating ISO 8601 variants; stored as UTC. If unparseable, the caller falls back to the filename date at 12:00:00 UTC with `date_precision = day` ([`resolve_date`]). |
//! | `Media Type` | `Image`/`Photo` → photo; `Video` → video; anything else → infer from the file extension. |
//! | `Location` | Two decimals via `(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)`; `0.0, 0.0`, empty or out-of-range is **no location**. |
//! | `Download Link` / `Media Download Url` | Empty, `N/A` or missing is "no URL". Full URLs are never logged ([`redact_url`]). |

use std::sync::LazyLock;

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use regex::Regex;

// ---------------------------------------------------------------------------
// Date
// ---------------------------------------------------------------------------

/// How precisely the capture instant is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DatePrecision {
    /// Full timestamp from the JSON `Date` field.
    Second,
    /// Only the calendar day is known; the time is a 12:00:00 UTC placeholder.
    Day,
}

/// Outcome of parsing one `Date` field value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateField {
    /// A full instant, normalised to UTC and truncated to whole seconds.
    Instant(DateTime<Utc>),
    /// A bare calendar date (`2021-12-25`) with no time of day.
    DayOnly(NaiveDate),
    /// Nothing recognisable.
    Unparseable,
}

impl DateField {
    pub fn instant(self) -> Option<DateTime<Utc>> {
        match self {
            DateField::Instant(dt) => Some(dt),
            _ => None,
        }
    }
}

/// The capture date an item will be written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureDate {
    pub utc: DateTime<Utc>,
    pub precision: DatePrecision,
}

/// Parse a `Date` field. The export writes `YYYY-MM-DD HH:MM:SS UTC`; also
/// accepted are RFC 3339 / ISO 8601 forms (`T` separator, `Z`, `±HH:MM` or
/// `±HHMM` offsets, fractional seconds), a trailing `GMT`, and a bare
/// `YYYY-MM-DD HH:MM:SS` or `YYYY-MM-DD HH:MM` which is taken as UTC. Offsets
/// are converted to UTC; fractional seconds are dropped.
pub fn parse_date(raw: &str) -> DateField {
    let s = raw.trim();
    if s.is_empty() {
        return DateField::Unparseable;
    }

    // Canonical export form first.
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S UTC") {
        return DateField::Instant(truncate(ndt.and_utc()));
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return DateField::Instant(truncate(dt.with_timezone(&Utc)));
    }

    // Normalise: a `T` separator becomes a space; trailing zone words mean UTC.
    let mut t = s.to_string();
    if t.len() > 10 && matches!(t.as_bytes()[10], b'T' | b't') {
        t.replace_range(10..11, " ");
    }
    let lower = t.to_ascii_lowercase();
    let explicit_utc = ["utc", "gmt", "z"]
        .iter()
        .find_map(|suffix| lower.strip_suffix(suffix).map(|rest| rest.len()));
    if let Some(cut) = explicit_utc {
        t.truncate(cut);
        let t = t.trim_end();
        for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M"] {
            if let Ok(ndt) = NaiveDateTime::parse_from_str(t, fmt) {
                return DateField::Instant(truncate(ndt.and_utc()));
            }
        }
        return DateField::Unparseable;
    }

    // Numeric offsets, with or without a colon or a separating space.
    for fmt in ["%Y-%m-%d %H:%M:%S%.f%z", "%Y-%m-%d %H:%M%z"] {
        if let Ok(dt) = DateTime::parse_from_str(&t, fmt) {
            return DateField::Instant(truncate(dt.with_timezone(&Utc)));
        }
    }
    // No zone at all: the export's times are UTC, so assume UTC.
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d %H:%M"] {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(&t, fmt) {
            return DateField::Instant(truncate(ndt.and_utc()));
        }
    }
    if let Ok(d) = NaiveDate::parse_from_str(&t, "%Y-%m-%d") {
        return DateField::DayOnly(d);
    }
    DateField::Unparseable
}

fn truncate(dt: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(dt.timestamp(), 0).unwrap_or(dt)
}

/// Apply the §3.5 fallback: a full instant wins; otherwise the filename date at
/// 12:00:00 UTC with day precision; otherwise a bare JSON date the same way;
/// otherwise nothing is known.
pub fn resolve_date(
    field: Option<DateField>,
    filename_date: Option<NaiveDate>,
) -> Option<CaptureDate> {
    if let Some(DateField::Instant(utc)) = field {
        return Some(CaptureDate {
            utc,
            precision: DatePrecision::Second,
        });
    }
    let day = filename_date.or(match field {
        Some(DateField::DayOnly(d)) => Some(d),
        _ => None,
    })?;
    Some(CaptureDate {
        utc: day.and_time(NOON).and_utc(),
        precision: DatePrecision::Day,
    })
}

const NOON: NaiveTime = match NaiveTime::from_hms_opt(12, 0, 0) {
    Some(t) => t,
    None => unreachable!(),
};

// ---------------------------------------------------------------------------
// Media Type
// ---------------------------------------------------------------------------

/// The resolved kind of an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaType {
    Photo,
    Video,
}

/// What the `Media Type` field said, before the extension fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaTypeHint {
    Photo,
    Video,
    /// Missing, empty or an unrecognised word: infer from the file extension.
    Unknown,
}

/// `Image`/`Photo` → photo, `Video` → video (case-insensitive, trimmed);
/// anything else, including a missing field, is [`MediaTypeHint::Unknown`].
pub fn parse_media_type(raw: Option<&str>) -> MediaTypeHint {
    match raw.map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("image") || s.eq_ignore_ascii_case("photo") => {
            MediaTypeHint::Photo
        }
        Some(s) if s.eq_ignore_ascii_case("video") => MediaTypeHint::Video,
        _ => MediaTypeHint::Unknown,
    }
}

/// Media type implied by a lower- or upper-case extension without the dot.
pub fn media_type_from_ext(ext: &str) -> Option<MediaType> {
    match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" | "png" | "heic" | "webp" => Some(MediaType::Photo),
        "mp4" | "mov" | "m4v" => Some(MediaType::Video),
        _ => None,
    }
}

/// Combine the field hint with the file extension per §3.5.
pub fn resolve_media_type(hint: MediaTypeHint, ext: Option<&str>) -> Option<MediaType> {
    match hint {
        MediaTypeHint::Photo => Some(MediaType::Photo),
        MediaTypeHint::Video => Some(MediaType::Video),
        MediaTypeHint::Unknown => ext.and_then(media_type_from_ext),
    }
}

// ---------------------------------------------------------------------------
// Location
// ---------------------------------------------------------------------------

/// A validated GPS position in decimal degrees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Location {
    pub lat: f64,
    pub lon: f64,
}

/// Outcome of parsing a `Location` field; everything but `Found` is "no
/// location", the variants exist so the report can say why.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LocationParse {
    Found(Location),
    /// Field missing or blank.
    Empty,
    /// No `number, number` pair in the text.
    Unrecognised,
    /// `0.0, 0.0`: the export's way of saying "none".
    NullIsland,
    /// `|lat| > 90` or `|lon| > 180`.
    OutOfRange,
}

impl LocationParse {
    pub fn location(self) -> Option<Location> {
        match self {
            LocationParse::Found(loc) => Some(loc),
            _ => None,
        }
    }
}

static LOCATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(-?[0-9]+(?:\.[0-9]+)?)\s*,\s*(-?[0-9]+(?:\.[0-9]+)?)").expect("valid regex")
});

/// Parse a `Location` field such as `Latitude, Longitude: 51.50735, -0.12776`.
pub fn parse_location(raw: Option<&str>) -> LocationParse {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return LocationParse::Empty;
    };
    let Some(caps) = LOCATION.captures(s) else {
        return LocationParse::Unrecognised;
    };
    let (Ok(lat), Ok(lon)) = (caps[1].parse::<f64>(), caps[2].parse::<f64>()) else {
        // Digits-only text always parses as f64 (possibly as infinity).
        return LocationParse::Unrecognised;
    };
    if lat == 0.0 && lon == 0.0 {
        return LocationParse::NullIsland;
    }
    if !(lat.abs() <= 90.0 && lon.abs() <= 180.0) {
        return LocationParse::OutOfRange;
    }
    LocationParse::Found(Location { lat, lon })
}

// ---------------------------------------------------------------------------
// URL fields
// ---------------------------------------------------------------------------

/// Treat empty, `N/A` or missing as "no URL". Returns the trimmed text otherwise.
pub fn parse_url(raw: Option<&str>) -> Option<&str> {
    let s = raw?.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("n/a") {
        None
    } else {
        Some(s)
    }
}

/// Item identifiers that a legacy `Download Link` carries in its query string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UrlTokens {
    /// `sid=` query value, when it is UUID-shaped.
    pub sid: Option<String>,
    /// `mid=` query value, when it is UUID-shaped.
    pub mid: Option<String>,
}

impl UrlTokens {
    pub fn is_empty(&self) -> bool {
        self.sid.is_none() && self.mid.is_none()
    }
}

/// Pull `sid` and `mid` UUIDs out of a URL's query string (keys compared
/// case-insensitively; values must be UUID-shaped, kept as written).
pub fn extract_url_tokens(url: &str) -> UrlTokens {
    let mut tokens = UrlTokens::default();
    let Some((_, query)) = url.split_once('?') else {
        return tokens;
    };
    let query = query.split('#').next().unwrap_or("");
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        if !is_uuid(value) {
            continue;
        }
        if key.eq_ignore_ascii_case("sid") {
            tokens.sid.get_or_insert_with(|| value.to_string());
        } else if key.eq_ignore_ascii_case("mid") {
            tokens.mid.get_or_insert_with(|| value.to_string());
        }
    }
    tokens
}

/// `8-4-4-4-12` hexadecimal groups, any case.
pub fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, &c)| match i {
        8 | 13 | 18 | 23 => c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

/// A URL safe to log: scheme, host and path only, with query and fragment
/// removed (they carry the signature and expiry of a legacy link).
pub fn redact_url(url: &str) -> String {
    let base = url.split(['?', '#']).next().unwrap_or("");
    if base.len() < url.len() {
        format!("{base}?<redacted>")
    } else {
        base.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, s)
            .unwrap()
            .and_utc()
    }

    #[test]
    fn date_canonical_export_form() {
        assert_eq!(
            parse_date("2021-12-25 18:42:07 UTC"),
            DateField::Instant(utc(2021, 12, 25, 18, 42, 7))
        );
        assert_eq!(
            parse_date("  2021-12-25 18:42:07 UTC\r\n"),
            DateField::Instant(utc(2021, 12, 25, 18, 42, 7))
        );
    }

    #[test]
    fn date_iso_8601_variants() {
        let want = DateField::Instant(utc(2021, 12, 25, 18, 42, 7));
        for s in [
            "2021-12-25T18:42:07Z",
            "2021-12-25T18:42:07.250Z",
            "2021-12-25t18:42:07z",
            "2021-12-25T19:42:07+01:00",
            "2021-12-25T19:42:07+0100",
            "2021-12-25 19:42:07 +01:00",
            "2021-12-25 13:42:07-05:00",
            "2021-12-25T18:42:07+00:00",
            "2021-12-25 18:42:07",
            "2021-12-25T18:42:07",
            "2021-12-25 18:42:07.5",
            "2021-12-25 18:42:07 GMT",
            "2021-12-25 18:42:07UTC",
            "2021-12-25 18:42:07 utc",
        ] {
            assert_eq!(parse_date(s), want, "{s:?}");
        }
        assert_eq!(
            parse_date("2021-12-25 18:42 UTC"),
            DateField::Instant(utc(2021, 12, 25, 18, 42, 0))
        );
        assert_eq!(
            parse_date("2021-12-25 18:42"),
            DateField::Instant(utc(2021, 12, 25, 18, 42, 0))
        );
    }

    #[test]
    fn date_day_only_and_garbage() {
        assert_eq!(
            parse_date("2021-12-25"),
            DateField::DayOnly(NaiveDate::from_ymd_opt(2021, 12, 25).unwrap())
        );
        for s in [
            "",
            "   ",
            "yesterday",
            "25/12/2021 18:42:07",
            "2021-13-45 18:42:07 UTC",
            "2021-12-25 25:00:00 UTC",
            "2021-12-25 18:42:07 PST",
            "1640457727",
        ] {
            assert_eq!(parse_date(s), DateField::Unparseable, "{s:?}");
        }
    }

    #[test]
    fn date_resolution_fallbacks() {
        let fname = NaiveDate::from_ymd_opt(2019, 7, 4).unwrap();
        let instant = utc(2019, 7, 4, 21, 3, 55);
        assert_eq!(
            resolve_date(Some(DateField::Instant(instant)), Some(fname)),
            Some(CaptureDate {
                utc: instant,
                precision: DatePrecision::Second
            })
        );
        let noon = CaptureDate {
            utc: utc(2019, 7, 4, 12, 0, 0),
            precision: DatePrecision::Day,
        };
        assert_eq!(
            resolve_date(Some(DateField::Unparseable), Some(fname)),
            Some(noon)
        );
        assert_eq!(resolve_date(None, Some(fname)), Some(noon));
        // A JSON day-only value is used only when the filename gives nothing.
        let other = NaiveDate::from_ymd_opt(2020, 1, 2).unwrap();
        assert_eq!(
            resolve_date(Some(DateField::DayOnly(other)), Some(fname)),
            Some(noon)
        );
        assert_eq!(
            resolve_date(Some(DateField::DayOnly(other)), None),
            Some(CaptureDate {
                utc: utc(2020, 1, 2, 12, 0, 0),
                precision: DatePrecision::Day
            })
        );
        assert_eq!(resolve_date(Some(DateField::Unparseable), None), None);
        assert_eq!(resolve_date(None, None), None);
    }

    #[test]
    fn media_type_words() {
        assert_eq!(parse_media_type(Some("Image")), MediaTypeHint::Photo);
        assert_eq!(parse_media_type(Some("Photo")), MediaTypeHint::Photo);
        assert_eq!(parse_media_type(Some(" image ")), MediaTypeHint::Photo);
        assert_eq!(parse_media_type(Some("Video")), MediaTypeHint::Video);
        assert_eq!(parse_media_type(Some("VIDEO")), MediaTypeHint::Video);
        assert_eq!(parse_media_type(Some("Live Photo")), MediaTypeHint::Unknown);
        assert_eq!(parse_media_type(Some("")), MediaTypeHint::Unknown);
        assert_eq!(parse_media_type(None), MediaTypeHint::Unknown);
    }

    #[test]
    fn media_type_extension_fallback() {
        assert_eq!(
            resolve_media_type(MediaTypeHint::Unknown, Some("mp4")),
            Some(MediaType::Video)
        );
        assert_eq!(
            resolve_media_type(MediaTypeHint::Unknown, Some("JPEG")),
            Some(MediaType::Photo)
        );
        assert_eq!(
            resolve_media_type(MediaTypeHint::Unknown, Some("gif")),
            None
        );
        assert_eq!(resolve_media_type(MediaTypeHint::Unknown, None), None);
        // The field wins over the extension when it is explicit.
        assert_eq!(
            resolve_media_type(MediaTypeHint::Video, Some("jpg")),
            Some(MediaType::Video)
        );
    }

    #[test]
    fn location_found() {
        let loc = parse_location(Some("Latitude, Longitude: 51.50735, -0.12776"))
            .location()
            .unwrap();
        assert!((loc.lat - 51.50735).abs() < 1e-9);
        assert!((loc.lon + 0.12776).abs() < 1e-9);
        assert!(
            parse_location(Some("40.71280,-74.00600"))
                .location()
                .is_some()
        );
        assert!(
            parse_location(Some("lat/long 2: -33.8688 , 151.2093"))
                .location()
                .is_some()
        );
        assert!(
            parse_location(Some("Latitude, Longitude: 0.0, 5.0"))
                .location()
                .is_some()
        );
        assert!(parse_location(Some("90, 180")).location().is_some());
        assert!(parse_location(Some("-90, -180")).location().is_some());
    }

    #[test]
    fn location_rejections() {
        assert_eq!(parse_location(None), LocationParse::Empty);
        assert_eq!(parse_location(Some("")), LocationParse::Empty);
        assert_eq!(parse_location(Some("   ")), LocationParse::Empty);
        assert_eq!(
            parse_location(Some("Latitude, Longitude: 0.0, 0.0")),
            LocationParse::NullIsland
        );
        assert_eq!(parse_location(Some("0, 0")), LocationParse::NullIsland);
        assert_eq!(
            parse_location(Some("Latitude, Longitude: 91.0, 10.0")),
            LocationParse::OutOfRange
        );
        assert_eq!(
            parse_location(Some("Latitude, Longitude: 10.0, -181.5")),
            LocationParse::OutOfRange
        );
        assert_eq!(
            parse_location(Some("Latitude, Longitude: ")),
            LocationParse::Unrecognised
        );
        assert_eq!(parse_location(Some("unknown")), LocationParse::Unrecognised);
        assert_eq!(parse_location(Some("51.5")), LocationParse::Unrecognised);
        assert_eq!(parse_location(Some("51., 2.")), LocationParse::Unrecognised);
    }

    #[test]
    fn url_none_values() {
        assert_eq!(parse_url(None), None);
        assert_eq!(parse_url(Some("")), None);
        assert_eq!(parse_url(Some("   ")), None);
        assert_eq!(parse_url(Some("N/A")), None);
        assert_eq!(parse_url(Some("n/a")), None);
        assert_eq!(
            parse_url(Some(" https://example.invalid/x?y=1 ")),
            Some("https://example.invalid/x?y=1")
        );
    }

    #[test]
    fn url_tokens() {
        let url = "https://app.example.invalid/dmd/memories?uid=0f1e2d3c-4b5a-4968-8776-655443322110&sid=3F2A9C1E-5B7D-4E1F-9A2B-0C3D4E5F6A7B&mid=a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d&ts=1729000000&sig=deadbeef";
        let t = extract_url_tokens(url);
        assert_eq!(
            t.sid.as_deref(),
            Some("3F2A9C1E-5B7D-4E1F-9A2B-0C3D4E5F6A7B")
        );
        assert_eq!(
            t.mid.as_deref(),
            Some("a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d")
        );
        assert!(extract_url_tokens("https://example.invalid/no/query").is_empty());
        assert!(extract_url_tokens("https://example.invalid/?sid=notauuid&mid=").is_empty());
        let t = extract_url_tokens(
            "https://example.invalid/?SID=3F2A9C1E-5B7D-4E1F-9A2B-0C3D4E5F6A7B#frag",
        );
        assert_eq!(
            t.sid.as_deref(),
            Some("3F2A9C1E-5B7D-4E1F-9A2B-0C3D4E5F6A7B")
        );
        assert!(t.mid.is_none());
    }

    #[test]
    fn url_redaction() {
        assert_eq!(
            redact_url("https://cdn.example.invalid/m/abc.mp4?Expires=1&Signature=xyz"),
            "https://cdn.example.invalid/m/abc.mp4?<redacted>"
        );
        assert_eq!(
            redact_url("https://cdn.example.invalid/m/abc.mp4"),
            "https://cdn.example.invalid/m/abc.mp4"
        );
        assert_eq!(
            redact_url("https://x.invalid/p#sig"),
            "https://x.invalid/p?<redacted>"
        );
    }

    #[test]
    fn uuid_shape() {
        assert!(is_uuid("3F2A9C1E-5B7D-4E1F-9A2B-0C3D4E5F6A7B"));
        assert!(is_uuid("a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"));
        assert!(!is_uuid("3F2A9C1E5B7D4E1F9A2B0C3D4E5F6A7B"));
        assert!(!is_uuid("3F2A9C1E-5B7D-4E1F-9A2B-0C3D4E5F6A7"));
        assert!(!is_uuid("3F2A9C1E-5B7D-4E1F-9A2B-0C3D4E5F6A7G"));
    }
}
