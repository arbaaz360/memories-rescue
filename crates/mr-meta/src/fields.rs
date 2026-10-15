//! The capture record every writer takes, and the string forms each container
//! wants (SRS §4.3). One place for the formats so exiftool and the native
//! writers cannot drift apart.

use chrono::{DateTime, FixedOffset, TimeZone, Utc};

/// Seconds between the QuickTime epoch (1904-01-01T00:00:00Z) and the Unix epoch.
pub const QT_EPOCH_OFFSET: i64 = 2_082_844_800;

/// What the repaired file should say about its capture.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Capture {
    /// Capture instant (the export's `Date`, which is UTC).
    pub utc: DateTime<Utc>,
    /// Local UTC offset at the capture place/time (§4.3 "Time offset").
    pub offset: FixedOffset,
    /// Decimal degrees, WGS 84. `None` writes no location.
    pub gps: Option<(f64, f64)>,
}

impl Capture {
    pub fn new(utc: DateTime<Utc>, offset_secs: i32, gps: Option<(f64, f64)>) -> Self {
        let offset = FixedOffset::east_opt(offset_secs).expect("offset within +-24h");
        Self { utc, offset, gps }
    }

    pub fn local(&self) -> DateTime<FixedOffset> {
        self.utc.with_timezone(&self.offset)
    }

    /// EXIF `DateTimeOriginal` / `CreateDate` / `ModifyDate`: local wall time.
    pub fn exif_local(&self) -> String {
        self.local().format("%Y:%m:%d %H:%M:%S").to_string()
    }

    /// EXIF `OffsetTime*`: `+01:00`.
    pub fn exif_offset(&self) -> String {
        let total = self.offset.local_minus_utc();
        let sign = if total < 0 { '-' } else { '+' };
        let total = total.abs();
        format!("{sign}{:02}:{:02}", total / 3600, (total % 3600) / 60)
    }

    /// EXIF `GPSDateStamp` (UTC).
    pub fn gps_date(&self) -> String {
        self.utc.format("%Y:%m:%d").to_string()
    }

    /// EXIF `GPSTimeStamp` (UTC).
    pub fn gps_time(&self) -> String {
        self.utc.format("%H:%M:%S").to_string()
    }

    /// QuickTime `mvhd`/`tkhd`/`mdhd` times as exiftool wants them written
    /// without `-api QuickTimeUTC`: the raw stored value, which the spec says is UTC.
    pub fn qt_utc(&self) -> String {
        self.utc.format("%Y:%m:%d %H:%M:%S").to_string()
    }

    /// QuickTime `mvhd`/`tkhd`/`mdhd` time field: seconds since 1904-01-01 UTC.
    pub fn qt_seconds(&self) -> u64 {
        (self.utc.timestamp() + QT_EPOCH_OFFSET).max(0) as u64
    }

    /// `com.apple.quicktime.creationdate`: ISO 8601 local time with offset,
    /// in the form Apple devices write (`2021-06-15T15:30:00+0100`).
    pub fn iso8601_local(&self) -> String {
        self.local().format("%Y-%m-%dT%H:%M:%S%z").to_string()
    }

    /// Same instant in exiftool's own date syntax (`2021:06:15 15:30:00+01:00`).
    pub fn exiftool_local_with_offset(&self) -> String {
        format!("{}{}", self.exif_local(), self.exif_offset())
    }

    /// `com.apple.quicktime.location.ISO6709` / `©xyz`: `+51.5074-000.1278/`.
    pub fn iso6709(&self) -> Option<String> {
        self.gps
            .map(|(lat, lon)| format!("{lat:+08.4}{lon:+09.4}/"))
    }

    /// ffmpeg `-metadata creation_time=` value.
    pub fn ffmpeg_creation_time(&self) -> String {
        self.utc.format("%Y-%m-%dT%H:%M:%S.000000Z").to_string()
    }

    /// EXIF GPS refs and absolute coordinates: `(lat, "N"|"S", lon, "E"|"W")`.
    pub fn gps_refs(&self) -> Option<(f64, &'static str, f64, &'static str)> {
        self.gps.map(|(lat, lon)| {
            (
                lat.abs(),
                if lat < 0.0 { "S" } else { "N" },
                lon.abs(),
                if lon < 0.0 { "W" } else { "E" },
            )
        })
    }

    /// Convenience for tests and the benchmark: a fixed instant.
    pub fn from_unix(unix_secs: i64, offset_secs: i32, gps: Option<(f64, f64)>) -> Self {
        Self::new(
            Utc.timestamp_opt(unix_secs, 0)
                .single()
                .expect("valid unix time"),
            offset_secs,
            gps,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn london() -> Capture {
        // 2021-06-15T14:30:00Z, BST (+01:00), Trafalgar Square.
        Capture::from_unix(1_623_767_400, 3600, Some((51.5074, -0.1278)))
    }

    #[test]
    fn formats_match_the_srs_examples() {
        let c = london();
        assert_eq!(c.exif_local(), "2021:06:15 15:30:00");
        assert_eq!(c.exif_offset(), "+01:00");
        assert_eq!(c.gps_date(), "2021:06:15");
        assert_eq!(c.gps_time(), "14:30:00");
        assert_eq!(c.qt_utc(), "2021:06:15 14:30:00");
        assert_eq!(c.qt_seconds(), 1_623_767_400 + 2_082_844_800);
        assert_eq!(c.iso8601_local(), "2021-06-15T15:30:00+0100");
        assert_eq!(c.iso6709().as_deref(), Some("+51.5074-000.1278/"));
        assert_eq!(c.ffmpeg_creation_time(), "2021-06-15T14:30:00.000000Z");
        assert_eq!(c.gps_refs(), Some((51.5074, "N", 0.1278, "W")));
    }

    #[test]
    fn negative_offsets_and_southern_hemisphere() {
        let c = Capture::from_unix(
            1_623_767_400,
            -(5 * 3600 + 30 * 60),
            Some((-33.8688, 151.2093)),
        );
        assert_eq!(c.exif_offset(), "-05:30");
        assert_eq!(c.iso6709().as_deref(), Some("-33.8688+151.2093/"));
        assert_eq!(c.gps_refs(), Some((33.8688, "S", 151.2093, "E")));
        assert_eq!(Capture::from_unix(0, 0, None).iso6709(), None);
    }
}
