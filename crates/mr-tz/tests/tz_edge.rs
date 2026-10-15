//! F-11 `tz-edge`: the resolver against offsets and local times computed
//! independently with Python's zoneinfo (tools/make_fixture/fixtures_m2.py).

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};
use mr_tz::{Method, Resolver};
use serde::Deserialize;

#[derive(Deserialize)]
struct Truth {
    home_zone: String,
    items: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    case: String,
    date_utc: String,
    location: Option<Loc>,
    expected: Expected,
}

#[derive(Deserialize)]
struct Loc {
    lat: f64,
    lon: f64,
}

#[derive(Deserialize)]
struct Expected {
    method: String,
    #[serde(default)]
    zone: Option<String>,
    #[serde(default)]
    offset_seconds: Option<i32>,
    #[serde(default)]
    local: Option<String>,
    #[serde(default)]
    local_folder: Option<String>,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn truth() -> Truth {
    let root = repo_root();
    let path = root.join("fixtures/tz-edge/truth.json");
    if !path.is_file() {
        for python in ["python", "python3", "py"] {
            match Command::new(python)
                .args(["tools/make_fixture", "--fixture", "tz-edge", "--quiet"])
                .current_dir(&root)
                .status()
            {
                Ok(s) if s.success() => break,
                Ok(s) => panic!("make_fixture failed: {s}"),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => panic!("{e}"),
            }
        }
    }
    serde_json::from_str(&std::fs::read_to_string(&path).expect("truth.json")).expect("truth shape")
}

#[test]
fn f11_every_case_matches_zoneinfo() {
    let truth = truth();
    let resolver = Resolver::new(Some(&truth.home_zone)).unwrap();
    assert_eq!(resolver.home_zone(), Some(truth.home_zone.as_str()));
    let mut checked = 0;
    for item in &truth.items {
        let utc: DateTime<Utc> = DateTime::parse_from_rfc3339(&item.date_utc)
            .unwrap()
            .with_timezone(&Utc);
        let gps = item.location.as_ref().map(|l| (l.lat, l.lon));
        let r = resolver.resolve(utc, gps);
        match item.expected.method.as_str() {
            "gps" => {
                assert!(
                    matches!(r.method, Method::Gps { .. }),
                    "{}: {:?}",
                    item.case,
                    r.method
                );
                assert_eq!(
                    r.method.zone(),
                    item.expected.zone.as_deref(),
                    "{}: zone",
                    item.case
                );
            }
            "home" => assert_eq!(
                r.method,
                Method::Home {
                    zone: truth.home_zone.clone()
                },
                "{}",
                item.case
            ),
            "gps_unknown_or_home" => {
                match &r.method {
                    Method::Gps { zone } => {
                        assert!(zone.starts_with("Etc/"), "{}: {zone}", item.case)
                    }
                    Method::GpsUnknown { .. } => {}
                    other => panic!("{}: {other:?}", item.case),
                }
                continue;
            }
            other => panic!("{}: unknown expected method {other}", item.case),
        }
        assert_eq!(
            Some(r.offset_seconds()),
            item.expected.offset_seconds,
            "{}: offset",
            item.case
        );
        let local = r.local(utc);
        assert_eq!(
            Some(local.format("%Y-%m-%dT%H:%M:%S").to_string()),
            item.expected.local,
            "{}: local time",
            item.case
        );
        assert_eq!(
            Some(local.format("%Y/%m").to_string()),
            item.expected.local_folder,
            "{}: YYYY/MM folder of the local date",
            item.case
        );
        checked += 1;
    }
    assert!(checked >= 20, "{checked} cases checked");
    println!(
        "tz-edge: {checked} cases agree with zoneinfo; boundary data {}",
        resolver.boundary_data_version()
    );
}
