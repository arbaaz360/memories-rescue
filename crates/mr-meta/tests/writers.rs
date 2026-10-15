//! Writer tests. The native writers always run (on the tiny media under
//! `tests/data`); the exiftool and ffprobe checks run only where the binaries
//! are present (`third_party/` or `PATH`), and print a skip note otherwise,
//! which is the case on the CI runners.

use std::path::{Path, PathBuf};

use mr_meta::exiftool::{self, ExifTool};
use mr_meta::fields::Capture;
use mr_meta::{ffmpeg_remux, native_jpeg, native_mp4, repo_root};

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mr-meta-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn london() -> Capture {
    Capture::from_unix(1_623_767_400, 3600, Some((51.5074, -0.1278)))
}

#[test]
fn native_mp4_moov_first_shifts_chunk_offsets_and_adds_keys() {
    let src = data("tiny-moov-first.mp4");
    let before = native_mp4::summarize(&src).unwrap();
    assert_eq!(before.top_level, ["ftyp", "moov", "free", "mdat"]);
    let dst = tmp("moov-first.mp4");
    let p = native_mp4::write_mp4(&src, &dst, &london()).unwrap();
    assert!(p.moov_first);
    let after = native_mp4::summarize(&dst).unwrap();
    let delta = p.bytes_out as i64 - p.bytes_in as i64;
    assert!(delta > 0, "moov must grow");
    assert_eq!(p.offsets_shifted, before.chunk_offsets.len());
    for (b, a) in before.chunk_offsets.iter().zip(&after.chunk_offsets) {
        assert_eq!(
            *a as i64,
            *b as i64 + delta,
            "every chunk offset moves by the moov growth"
        );
    }
    assert_eq!(after.mvhd_creation, Some(london().qt_seconds()));
    assert!(
        after
            .tkhd_creations
            .iter()
            .all(|t| *t == london().qt_seconds())
    );
    assert!(
        after
            .mdhd_creations
            .iter()
            .all(|t| *t == london().qt_seconds())
    );
    assert_eq!(
        after.keys,
        vec![
            (
                "com.apple.quicktime.creationdate".to_string(),
                "2021-06-15T15:30:00+0100".to_string()
            ),
            (
                "com.apple.quicktime.location.ISO6709".to_string(),
                "+51.5074-000.1278/".to_string()
            ),
        ]
    );
    assert_eq!(after.xyz.as_deref(), Some("+51.5074-000.1278/"));
    assert_eq!(
        after.top_level,
        ["ftyp", "moov", "free", "mdat"],
        "layout is preserved"
    );
    // mdat bytes are untouched.
    let a = std::fs::read(&src).unwrap();
    let b = std::fs::read(&dst).unwrap();
    assert_eq!(&a[a.len() - 64..], &b[b.len() - 64..]);
}

#[test]
fn native_mp4_moov_last_leaves_offsets_alone() {
    let src = data("tiny-moov-last.mp4");
    let before = native_mp4::summarize(&src).unwrap();
    assert_eq!(before.top_level, ["ftyp", "free", "mdat", "moov"]);
    let dst = tmp("moov-last.mp4");
    let p = native_mp4::write_mp4(&src, &dst, &london()).unwrap();
    assert!(!p.moov_first);
    assert_eq!(p.offsets_shifted, 0);
    let after = native_mp4::summarize(&dst).unwrap();
    assert_eq!(after.chunk_offsets, before.chunk_offsets);
    assert_eq!(after.xyz.as_deref(), Some("+51.5074-000.1278/"));
    assert_eq!(after.keys.len(), 2);
}

#[test]
fn native_mp4_is_idempotent_on_its_own_output() {
    let src = data("tiny-moov-first.mp4");
    let once = tmp("once.mp4");
    let twice = tmp("twice.mp4");
    native_mp4::write_mp4(&src, &once, &london()).unwrap();
    let later = Capture::from_unix(1_623_767_400 + 86_400, -18_000, Some((40.7128, -74.0060)));
    native_mp4::write_mp4(&once, &twice, &later).unwrap();
    let s = native_mp4::summarize(&twice).unwrap();
    assert_eq!(s.keys.len(), 2, "old Keys replaced, not appended");
    assert_eq!(s.keys[1].1, "+40.7128-074.0060/");
    assert_eq!(s.xyz.as_deref(), Some("+40.7128-074.0060/"));
    assert_eq!(
        std::fs::metadata(&once).unwrap().len(),
        std::fs::metadata(&twice).unwrap().len()
    );
}

#[test]
fn native_jpeg_writes_the_field_set() {
    let src = data("tiny.jpg");
    let dst = tmp("tiny-out.jpg");
    let (bytes_in, bytes_out) = native_jpeg::write_jpeg(&src, &dst, &london()).unwrap();
    assert!(bytes_out > bytes_in);
    let out = std::fs::read(&dst).unwrap();
    assert_eq!(&out[..2], &[0xFF, 0xD8], "still a JPEG");
    let md = little_exif::metadata::Metadata::new_from_vec(
        &out,
        little_exif::filetype::FileExtension::JPEG,
    )
    .unwrap();
    let mut seen = Vec::new();
    for tag in &md {
        seen.push(tag.as_u16());
    }
    for (name, hex) in [
        ("DateTimeOriginal", 0x9003u16),
        ("CreateDate", 0x9004),
        ("OffsetTimeOriginal", 0x9011),
        ("OffsetTime", 0x9010),
        ("GPSLatitude", 0x0002),
        ("GPSLongitudeRef", 0x0003),
        ("GPSTimeStamp", 0x0007),
        ("GPSDateStamp", 0x001d),
    ] {
        assert!(
            seen.contains(&hex),
            "{name} ({hex:#06x}) missing; have {seen:#06x?}"
        );
    }
}

/// Needs exiftool (and ffprobe for the container view). Skips cleanly without them.
#[test]
fn exiftool_stay_open_round_trip() {
    let root = repo_root();
    let Some(ex) = exiftool::locate(&root) else {
        eprintln!("skip: exiftool not found (third_party/ or PATH)");
        return;
    };
    let mut et = ExifTool::spawn(&ex).unwrap();
    let ver = et.version().unwrap();
    assert!(ver.starts_with("1"), "exiftool version {ver:?}");

    let cap = london();
    let jpg = tmp("et.jpg");
    let _ = std::fs::remove_file(&jpg);
    let r = et.write_jpeg(&data("tiny.jpg"), &jpg, &cap).unwrap();
    assert!(r.error().is_none(), "{}", r.stderr);
    let j = et.read_json(&jpg, &[]).unwrap();
    assert_eq!(j["ExifIFD:DateTimeOriginal"], "2021:06:15 15:30:00");
    assert_eq!(j["ExifIFD:OffsetTimeOriginal"], "+01:00");
    assert_eq!(j["GPS:GPSLongitudeRef"], "W");
    assert_eq!(j["GPS:GPSTimeStamp"], "14:30:00");

    let mp4 = tmp("et.mp4");
    let _ = std::fs::remove_file(&mp4);
    let r = et
        .write_mp4(&data("tiny-moov-first.mp4"), &mp4, &cap)
        .unwrap();
    assert!(r.error().is_none(), "{}", r.stderr);
    let j = et.read_json(&mp4, &[]).unwrap();
    assert_eq!(j["QuickTime:CreateDate"], "2021:06:15 14:30:00");
    assert_eq!(j["Track1:MediaCreateDate"], "2021:06:15 14:30:00");
    assert_eq!(j["Keys:CreationDate"], "2021:06:15 15:30:00+01:00");
    assert_eq!(j["Keys:GPSCoordinates"], "51.5074 -0.1278");
    assert_eq!(j["UserData:GPSCoordinates"], "51.5074 -0.1278");
    let s = native_mp4::summarize(&mp4).unwrap();
    assert_eq!(
        s.xyz.as_deref(),
        Some("+51.5074-000.1278/"),
        "ISO 6709 string in ©xyz"
    );
    assert_eq!(
        s.keys[1],
        (
            "com.apple.quicktime.location.ISO6709".to_string(),
            "+51.5074-000.1278/".to_string()
        )
    );

    // The native writer's output must read back identically through exiftool.
    let nat = tmp("native-for-exiftool.mp4");
    native_mp4::write_mp4(&data("tiny-moov-first.mp4"), &nat, &cap).unwrap();
    let j = et.read_json(&nat, &[]).unwrap();
    assert_eq!(j["QuickTime:CreateDate"], "2021:06:15 14:30:00");
    assert_eq!(j["Keys:CreationDate"], "2021:06:15 15:30:00+01:00");
    assert_eq!(j["Keys:GPSCoordinates"], "51.5074 -0.1278");
    assert_eq!(j["UserData:GPSCoordinates"], "51.5074 -0.1278");

    let natjpg = tmp("native-for-exiftool.jpg");
    native_jpeg::write_jpeg(&data("tiny.jpg"), &natjpg, &cap).unwrap();
    let j = et.read_json(&natjpg, &[]).unwrap();
    assert_eq!(j["ExifIFD:DateTimeOriginal"], "2021:06:15 15:30:00");
    assert_eq!(j["ExifIFD:OffsetTime"], "+01:00");
    assert_eq!(j["GPS:GPSLatitudeRef"], "N");
    assert_eq!(j["GPS:GPSDateStamp"], "2021:06:15");
    assert_eq!(j["GPS:GPSTimeStamp"], "14:30:00");
    let lat = j["GPS:GPSLatitude"].as_f64().unwrap();
    assert!((lat - 51.5074).abs() < 1e-6, "{lat}");

    if let Some(fp) = ffmpeg_remux::locate(&root, "ffprobe") {
        for f in [&mp4, &nat] {
            let j = ffmpeg_remux::ffprobe_json(&fp, f).unwrap();
            assert_eq!(
                j["format"]["tags"]["com.apple.quicktime.location.ISO6709"],
                "+51.5074-000.1278/"
            );
            assert_eq!(
                j["format"]["tags"]["com.apple.quicktime.creationdate"],
                "2021-06-15T15:30:00+0100"
            );
            assert_eq!(
                j["format"]["tags"]["creation_time"],
                "2021-06-15T14:30:00.000000Z"
            );
            assert_eq!(j["format"]["tags"]["location"], "+51.5074-000.1278/");
        }
    } else {
        eprintln!("skip: ffprobe not found, container view not checked");
    }
    et.close().unwrap();
}

#[test]
fn exiftool_handles_non_ascii_paths() {
    let Some(ex) = exiftool::locate(&repo_root()) else {
        eprintln!("skip: exiftool not found");
        return;
    };
    let mut et = ExifTool::spawn(&ex).unwrap();
    let dst = tmp("Café ☕ 写真.jpg");
    let _ = std::fs::remove_file(&dst);
    let r = et.write_jpeg(&data("tiny.jpg"), &dst, &london()).unwrap();
    assert!(r.error().is_none(), "{}", r.stderr);
    assert!(dst.is_file());
    let j = et.read_json(&dst, &[]).unwrap();
    assert_eq!(j["ExifIFD:DateTimeOriginal"], "2021:06:15 15:30:00");
}
