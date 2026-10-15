//! Property tests (SRS §11.1: filename regexes and location parsing).

use chrono::NaiveDate;
use mr_core::fields::{LocationParse, parse_date, parse_location};
use mr_core::filename::{FileKind, parse_filename};
use proptest::prelude::*;

fn valid_date() -> impl Strategy<Value = NaiveDate> {
    (1990i32..=2100, 1u32..=12, 1u32..=28)
        .prop_map(|(y, m, d)| NaiveDate::from_ymd_opt(y, m, d).expect("day <= 28 always valid"))
}

/// IDs from the §3.2 character class that do not end in a suffix word, so the
/// suffix-less shape stays unambiguous.
fn plain_id() -> impl Strategy<Value = String> {
    "[A-Za-z0-9-]{1,48}".prop_filter("id must not end in a suffix word", |id| {
        let lower = id.to_ascii_lowercase();
        !lower.ends_with("-main") && !lower.ends_with("-overlay")
    })
}

fn mixed_case(s: &str, mask: u64) -> String {
    s.chars()
        .enumerate()
        .map(|(i, c)| {
            if (mask >> (i % 64)) & 1 == 1 {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect()
}

proptest! {
    #[test]
    fn main_names_round_trip(date in valid_date(), id in plain_id(),
                             ext in prop::sample::select(vec!["jpg", "jpeg", "png", "mp4", "mov"]),
                             mask in any::<u64>()) {
        let name = format!("{}_{}-{}.{}", date.format("%Y-%m-%d"), id,
                           mixed_case("main", mask), mixed_case(ext, mask >> 8));
        let p = parse_filename(&name).expect("main name parses");
        prop_assert_eq!(p.date, date);
        prop_assert_eq!(&p.id, &id);
        prop_assert_eq!(p.kind, FileKind::Main);
        prop_assert_eq!(&p.ext, ext);
    }

    #[test]
    fn overlay_names_round_trip_and_pair_with_their_main(date in valid_date(), id in plain_id(),
                                                         mask in any::<u64>()) {
        let overlay = format!("{}_{}-overlay.{}", date.format("%Y-%m-%d"), id, mixed_case("png", mask));
        let o = parse_filename(&overlay).expect("overlay parses");
        prop_assert_eq!(o.kind, FileKind::Overlay);
        prop_assert_eq!(&o.ext, "png");
        let main = format!("{}_{}-main.jpg", date.format("%Y-%m-%d"), mixed_case(&id, mask));
        let m = parse_filename(&main).expect("main parses");
        prop_assert_eq!(m.pair_key(), o.pair_key());
    }

    #[test]
    fn suffixless_names_round_trip(date in valid_date(), id in plain_id(),
                                   ext in prop::sample::select(vec!["jpg", "mp4"])) {
        let name = format!("{}_{}.{}", date.format("%Y-%m-%d"), id, ext);
        let p = parse_filename(&name).expect("suffix-less name parses");
        prop_assert_eq!(p.date, date);
        prop_assert_eq!(&p.id, &id);
        prop_assert_eq!(p.kind, FileKind::MainNoSuffix);
        prop_assert_eq!(&p.ext, ext);
    }

    #[test]
    fn suffixless_png_and_mov_are_not_recognised(date in valid_date(), id in plain_id(),
                                                 ext in prop::sample::select(vec!["png", "mov", "jpeg", "gif"])) {
        let name = format!("{}_{}.{}", date.format("%Y-%m-%d"), id, ext);
        prop_assert!(parse_filename(&name).is_none());
    }

    #[test]
    fn arbitrary_text_never_panics_and_results_are_consistent(name in "\\PC{0,80}") {
        if let Some(p) = parse_filename(&name) {
            prop_assert!(name.starts_with(&p.date.format("%Y-%m-%d").to_string()));
            prop_assert!(!p.id.is_empty());
            prop_assert!(p.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
            prop_assert_eq!(p.ext.clone(), p.ext.to_ascii_lowercase());
            if p.kind == FileKind::Overlay { prop_assert_eq!(&p.ext, "png"); }
        }
    }

    #[test]
    fn in_range_locations_parse_back(lat in -90.0f64..=90.0, lon in -180.0f64..=180.0,
                                     prefix in prop::sample::select(vec!["Latitude, Longitude: ", "", "GPS: "]),
                                     spaces in 0usize..3) {
        prop_assume!(!(lat == 0.0 && lon == 0.0));
        let gap = " ".repeat(spaces);
        let text = format!("{prefix}{lat:.6},{gap}{lon:.6}");
        match parse_location(Some(&text)) {
            LocationParse::Found(loc) => {
                prop_assert!((loc.lat - lat).abs() < 1e-6, "{text}");
                prop_assert!((loc.lon - lon).abs() < 1e-6, "{text}");
            }
            // Rounding to 6 places can land exactly on 0.0, 0.0.
            LocationParse::NullIsland => prop_assert!(lat.abs() < 5e-7 && lon.abs() < 5e-7),
            other => prop_assert!(false, "{text} -> {other:?}"),
        }
    }

    #[test]
    fn out_of_range_latitude_is_rejected(lat in prop_oneof![90.000001f64..1e6, -1e6..-90.000001f64],
                                         lon in -180.0f64..=180.0) {
        let text = format!("Latitude, Longitude: {lat}, {lon}");
        prop_assert_eq!(parse_location(Some(&text)), LocationParse::OutOfRange);
    }

    #[test]
    fn out_of_range_longitude_is_rejected(lat in -90.0f64..=90.0,
                                          lon in prop_oneof![180.000001f64..1e6, -1e6..-180.000001f64]) {
        let text = format!("Latitude, Longitude: {lat}, {lon}");
        prop_assert_eq!(parse_location(Some(&text)), LocationParse::OutOfRange);
    }

    #[test]
    fn location_parser_never_panics_and_never_returns_junk(text in "\\PC{0,60}") {
        if let LocationParse::Found(loc) = parse_location(Some(&text)) {
            prop_assert!(loc.lat.abs() <= 90.0 && loc.lon.abs() <= 180.0);
            prop_assert!(!(loc.lat == 0.0 && loc.lon == 0.0));
            prop_assert!(loc.lat.is_finite() && loc.lon.is_finite());
        }
    }

    #[test]
    fn date_parser_never_panics(text in "\\PC{0,40}") {
        let _ = parse_date(&text);
    }

    #[test]
    fn canonical_dates_round_trip(y in 2010i32..=2030, mo in 1u32..=12, d in 1u32..=28,
                                  h in 0u32..24, mi in 0u32..60, s in 0u32..60) {
        let want = NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, s).unwrap().and_utc();
        let canonical = want.format("%Y-%m-%d %H:%M:%S UTC").to_string();
        prop_assert_eq!(parse_date(&canonical).instant(), Some(want));
        let iso = want.to_rfc3339();
        prop_assert_eq!(parse_date(&iso).instant(), Some(want));
    }
}
