//! PNG photos get the same field set as JPEG, in an `eXIf` chunk (SRS 4.3).

use std::path::{Path, PathBuf};

use little_exif::exif_tag::ExifTag;
use mr_meta::fields::Capture;
use mr_meta::native_png;

fn data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mr-meta-png-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn native_png_writes_the_field_set_in_an_exif_chunk() {
    let london = Capture::from_unix(1_623_767_400, 3600, Some((51.5074, -0.1278)));
    let dst = tmp("tiny-out.png");
    let (bytes_in, bytes_out) = native_png::write_png(&data("tiny.png"), &dst, &london).unwrap();
    assert!(bytes_out > bytes_in);
    let out = std::fs::read(&dst).unwrap();
    assert_eq!(&out[..8], b"\x89PNG\r\n\x1a\n", "still a PNG");
    assert!(out.windows(4).any(|w| w == b"eXIf"), "eXIf chunk present");
    let md = native_png::read_png(&dst).unwrap();
    let mut original = None;
    let mut offset = None;
    let mut gps = false;
    for tag in &md {
        match tag {
            ExifTag::DateTimeOriginal(s) => original = Some(s.trim_end_matches('\0').to_string()),
            ExifTag::OffsetTimeOriginal(s) => offset = Some(s.trim_end_matches('\0').to_string()),
            ExifTag::GPSLatitude(_) => gps = true,
            _ => {}
        }
    }
    assert_eq!(original.as_deref(), Some("2021:06:15 15:30:00"));
    assert_eq!(offset.as_deref(), Some("+01:00"));
    assert!(gps);
    assert!(native_png::write_png(&data("tiny.jpg"), &tmp("not.png"), &london).is_err());
}
