//! Local UTC offset for a capture instant (SRS FR-19, section 4.3 "Time offset").
//!
//! * With GPS: the time zone comes from an offline boundary dataset bundled
//!   in the binary (`tzf-rs` with `tzf-dist`, derived from
//!   timezone-boundary-builder, ODbL), and the offset for the instant,
//!   including DST, from the tzdb bundled by `chrono-tz`.
//! * Without GPS, or when the point is in no zone (open ocean): the home
//!   zone, which defaults to the system zone and falls back to UTC.
//!
//! Every answer says which method produced it, so the item record can store
//! it (FR-19 "The method used is recorded per item").
//!
//! Licences and the ODbL data notice are listed in `THIRD_PARTY_NOTICES.md`.

use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, FixedOffset, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use thiserror::Error;
use tzf_rs::DefaultFinder;

/// How the offset was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Method {
    /// The point lies in `zone` on the boundary dataset.
    Gps { zone: String },
    /// No GPS: the home zone chosen by the user.
    Home { zone: String },
    /// No GPS and no home zone set: the system zone.
    System { zone: String },
    /// GPS given but no zone covers the point: the home or system zone.
    GpsUnknown { fallback_zone: String },
    /// Nothing worked: UTC.
    Utc,
}

impl Method {
    /// Short tag for the item record: `gps`, `home`, `system`, `gps_unknown`, `utc`.
    pub fn tag(&self) -> &'static str {
        match self {
            Method::Gps { .. } => "gps",
            Method::Home { .. } => "home",
            Method::System { .. } => "system",
            Method::GpsUnknown { .. } => "gps_unknown",
            Method::Utc => "utc",
        }
    }

    /// The zone the offset was taken from, when there is one.
    pub fn zone(&self) -> Option<&str> {
        match self {
            Method::Gps { zone } | Method::Home { zone } | Method::System { zone } => Some(zone),
            Method::GpsUnknown { fallback_zone } => Some(fallback_zone),
            Method::Utc => None,
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.zone() {
            Some(zone) => write!(f, "{} ({zone})", self.tag()),
            None => f.write_str(self.tag()),
        }
    }
}

/// The resolved offset for one capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub offset: FixedOffset,
    pub method: Method,
}

impl Resolved {
    pub fn offset_seconds(&self) -> i32 {
        self.offset.local_minus_utc()
    }

    /// `+05:30` form.
    pub fn offset_string(&self) -> String {
        format_offset(self.offset_seconds())
    }

    pub fn local(&self, utc: DateTime<Utc>) -> DateTime<FixedOffset> {
        utc.with_timezone(&self.offset)
    }
}

/// `+05:30`, `-04:00`.
pub fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let s = seconds.abs();
    format!("{sign}{:02}:{:02}", s / 3600, (s % 3600) / 60)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("unknown time zone {0:?}")]
    UnknownZone(String),
}

/// Where the home zone came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeSource {
    /// Set by the user (FR-48 "home timezone").
    User,
    /// Taken from the operating system.
    System,
    /// Neither available: UTC.
    Utc,
}

/// Resolves offsets. Build one per job: the boundary dataset is decoded once
/// (a few megabytes) and lookups are then microseconds.
pub struct Resolver {
    finder: DefaultFinder,
    home: Option<Tz>,
    home_source: HomeSource,
}

impl fmt::Debug for Resolver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Resolver")
            .field("home", &self.home.map(|z| z.name()))
            .field("home_source", &self.home_source)
            .field("data_version", &self.finder.data_version())
            .finish()
    }
}

impl Resolver {
    /// `home` is an IANA zone name such as `Asia/Kolkata`; `None` means the
    /// system zone (or UTC when the system does not say).
    pub fn new(home: Option<&str>) -> Result<Resolver, Error> {
        let (home, home_source) = match home {
            Some(name) => (Some(parse_zone(name)?), HomeSource::User),
            None => match system_zone().and_then(|n| parse_zone(&n).ok()) {
                Some(tz) => (Some(tz), HomeSource::System),
                None => (None, HomeSource::Utc),
            },
        };
        Ok(Resolver {
            finder: DefaultFinder::new(),
            home,
            home_source,
        })
    }

    pub fn home_zone(&self) -> Option<&str> {
        self.home.map(|z| z.name())
    }

    pub fn home_source(&self) -> HomeSource {
        self.home_source
    }

    /// Version of the bundled boundary dataset.
    pub fn boundary_data_version(&self) -> &str {
        self.finder.data_version()
    }

    /// The IANA zone a point lies in, if the dataset covers it.
    pub fn zone_at(&self, lat: f64, lon: f64) -> Option<Tz> {
        if !(lat.abs() <= 90.0 && lon.abs() <= 180.0) {
            return None;
        }
        let name = self.finder.get_tz_name(lon, lat);
        if name.is_empty() {
            return None;
        }
        parse_zone(name).ok()
    }

    /// The local offset of `utc` at `gps`, or at home.
    pub fn resolve(&self, utc: DateTime<Utc>, gps: Option<(f64, f64)>) -> Resolved {
        if let Some((lat, lon)) = gps {
            if let Some(tz) = self.zone_at(lat, lon) {
                return Resolved {
                    offset: offset_in(tz, utc),
                    method: Method::Gps {
                        zone: tz.name().to_string(),
                    },
                };
            }
            return match self.home {
                Some(tz) => Resolved {
                    offset: offset_in(tz, utc),
                    method: Method::GpsUnknown {
                        fallback_zone: tz.name().to_string(),
                    },
                },
                None => Resolved {
                    offset: FixedOffset::east_opt(0).expect("zero offset"),
                    method: Method::Utc,
                },
            };
        }
        match self.home {
            Some(tz) => Resolved {
                offset: offset_in(tz, utc),
                method: match self.home_source {
                    HomeSource::User => Method::Home {
                        zone: tz.name().to_string(),
                    },
                    HomeSource::System | HomeSource::Utc => Method::System {
                        zone: tz.name().to_string(),
                    },
                },
            },
            None => Resolved {
                offset: FixedOffset::east_opt(0).expect("zero offset"),
                method: Method::Utc,
            },
        }
    }
}

/// Offset of `tz` at the instant `utc`, DST included.
pub fn offset_in(tz: Tz, utc: DateTime<Utc>) -> FixedOffset {
    tz.offset_from_utc_datetime(&utc.naive_utc()).fix()
}

/// Parse an IANA zone name against the bundled tzdb.
pub fn parse_zone(name: &str) -> Result<Tz, Error> {
    Tz::from_str(name.trim()).map_err(|_| Error::UnknownZone(name.to_string()))
}

/// The operating system's zone as an IANA name, if it can be determined.
pub fn system_zone() -> Option<String> {
    iana_time_zone::get_timezone().ok()
}

/// Every zone name the bundled tzdb knows, for a settings picker.
pub fn zone_names() -> impl Iterator<Item = &'static str> {
    chrono_tz::TZ_VARIANTS.iter().map(|z| z.name())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;

    static RESOLVER: LazyLock<Resolver> =
        LazyLock::new(|| Resolver::new(Some("Asia/Kolkata")).unwrap());

    fn utc(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn fr19_ac1_new_york_london_and_kolkata_home() {
        let t = utc("2020-07-01T12:00:00Z");
        let ny = RESOLVER.resolve(t, Some((40.7128, -74.0060)));
        assert_eq!(ny.offset_string(), "-04:00");
        assert_eq!(
            ny.method,
            Method::Gps {
                zone: "America/New_York".into()
            }
        );
        let london = RESOLVER.resolve(t, Some((51.5074, -0.1278)));
        assert_eq!(london.offset_string(), "+01:00");
        assert_eq!(london.method.zone(), Some("Europe/London"));
        let home = RESOLVER.resolve(t, None);
        assert_eq!(home.offset_string(), "+05:30");
        assert_eq!(
            home.method,
            Method::Home {
                zone: "Asia/Kolkata".into()
            }
        );
        assert_eq!(home.method.tag(), "home");
    }

    #[test]
    fn dst_transitions_both_directions() {
        let ny = Some((40.7128, -74.0060));
        assert_eq!(
            RESOLVER
                .resolve(utc("2021-03-14T06:59:59Z"), ny)
                .offset_string(),
            "-05:00"
        );
        assert_eq!(
            RESOLVER
                .resolve(utc("2021-03-14T07:00:00Z"), ny)
                .offset_string(),
            "-04:00"
        );
        assert_eq!(
            RESOLVER
                .resolve(utc("2021-11-07T05:59:59Z"), ny)
                .offset_string(),
            "-04:00"
        );
        assert_eq!(
            RESOLVER
                .resolve(utc("2021-11-07T06:00:00Z"), ny)
                .offset_string(),
            "-05:00"
        );
        let london = Some((51.5074, -0.1278));
        assert_eq!(
            RESOLVER
                .resolve(utc("2021-03-28T00:59:59Z"), london)
                .offset_string(),
            "+00:00"
        );
        assert_eq!(
            RESOLVER
                .resolve(utc("2021-03-28T01:00:00Z"), london)
                .offset_string(),
            "+01:00"
        );
    }

    #[test]
    fn odd_offsets_and_the_date_line() {
        let r = RESOLVER.resolve(utc("2021-06-01T12:00:00Z"), Some((27.7172, 85.3240)));
        assert_eq!(
            (r.offset_string(), r.method.zone()),
            ("+05:45".into(), Some("Asia/Kathmandu"))
        );
        let r = RESOLVER.resolve(utc("2021-01-15T12:00:00Z"), Some((-43.95, -176.55)));
        assert_eq!(r.offset_string(), "+13:45", "Chatham in summer");
        let r = RESOLVER.resolve(utc("2021-01-01T11:00:00Z"), Some((1.8721, -157.4278)));
        assert_eq!(r.offset_string(), "+14:00");
        assert_eq!(
            r.local(utc("2021-01-01T11:00:00Z"))
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            "2021-01-02 01:00"
        );
        let r = RESOLVER.resolve(utc("2021-01-01T10:30:00Z"), Some((-14.2756, -170.7020)));
        assert_eq!(r.offset_string(), "-11:00");
        assert_eq!(
            r.local(utc("2021-01-01T10:30:00Z"))
                .format("%Y-%m-%d %H:%M")
                .to_string(),
            "2020-12-31 23:30"
        );
    }

    #[test]
    fn ocean_falls_back_to_home_and_out_of_range_is_ignored() {
        let r = RESOLVER.resolve(utc("2021-06-01T12:00:00Z"), Some((0.0001, -30.0)));
        match &r.method {
            Method::Gps { zone } => {
                assert!(zone.starts_with("Etc/"), "ocean zones are Etc/GMT: {zone}")
            }
            Method::GpsUnknown { fallback_zone } => assert_eq!(fallback_zone, "Asia/Kolkata"),
            other => panic!("{other:?}"),
        }
        let r = RESOLVER.resolve(utc("2021-06-01T12:00:00Z"), Some((95.0, 10.0)));
        assert_eq!(
            r.method,
            Method::GpsUnknown {
                fallback_zone: "Asia/Kolkata".into()
            }
        );
    }

    #[test]
    fn home_zone_validation_and_system_fallback() {
        assert!(matches!(
            Resolver::new(Some("Mars/Olympus")),
            Err(Error::UnknownZone(_))
        ));
        let r = Resolver::new(None).unwrap();
        assert!(matches!(
            r.home_source(),
            HomeSource::System | HomeSource::Utc
        ));
        let resolved = r.resolve(utc("2021-06-01T12:00:00Z"), None);
        assert!(matches!(
            resolved.method,
            Method::System { .. } | Method::Utc
        ));
        assert!(zone_names().any(|z| z == "Asia/Kolkata"));
        assert_eq!(format_offset(-(5 * 3600 + 30 * 60)), "-05:30");
    }
}
