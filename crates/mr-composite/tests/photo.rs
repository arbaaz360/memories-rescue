//! FR-23 AC1: photo composites match Pillow's reference composite with
//! SSIM >= 0.98 on every F-07 photo, EXIF orientations 3, 6 and 8 and
//! aspect-mismatched overlays included. Needs Python and Pillow only (the
//! F-07 photos are generated without ffmpeg), so it runs on CI too.

mod common;

use mr_composite::{PhotoFormat, PhotoOptions, composite_photo, probe};
use serde_json::Value;

#[test]
fn fr23_ac1_photo_composites_match_the_reference() {
    let Some((dir, truth)) = common::fixture("rotation") else {
        return;
    };
    let out_dir = common::scratch("photo");
    let photos: Vec<&Value> = truth["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["media_type"] == "photo")
        .collect();
    assert_eq!(photos.len(), 7, "F-07 has seven photos");
    let mut seen_orientations = std::collections::BTreeSet::new();
    for item in photos {
        let case = item["case"].as_str().unwrap();
        let main = dir.join(item["main"].as_str().unwrap());
        let overlay = dir.join(item["overlay"].as_str().unwrap());
        let out = out_dir.join(format!("{case}.out"));

        // The probe reads what was stored.
        let info = probe(&main).unwrap();
        let img = info.image.as_ref().unwrap();
        assert_eq!(
            (img.width, img.height),
            common::size_of(&item["stored_size"]),
            "{case}"
        );
        let orientation = item["orientation"].as_u64().unwrap() as u8;
        assert_eq!(img.orientation, orientation, "{case}");
        assert_eq!(
            info.display_size(),
            Some(common::size_of(&item["display_size"])),
            "{case}"
        );
        seen_orientations.insert(orientation);

        let outcome = composite_photo(&main, &overlay, &out, &PhotoOptions::default()).unwrap();
        let expect_format = match item["output_format"].as_str().unwrap() {
            "png" => PhotoFormat::Png,
            _ => PhotoFormat::Jpeg,
        };
        assert_eq!(outcome.format, expect_format, "{case}");
        assert_eq!(
            outcome.size,
            common::size_of(&item["display_size"]),
            "{case}"
        );
        assert_eq!(outcome.orientation, orientation, "{case}");
        let p = &item["placement"];
        assert_eq!(
            (
                outcome.placement.x,
                outcome.placement.y,
                outcome.placement.width,
                outcome.placement.height
            ),
            (
                p["x"].as_i64().unwrap() as i32,
                p["y"].as_i64().unwrap() as i32,
                p["width"].as_u64().unwrap() as u32,
                p["height"].as_u64().unwrap() as u32
            ),
            "{case}"
        );
        let mismatch = p["aspect_mismatch"].as_bool().unwrap();
        assert_eq!(
            outcome
                .warnings
                .iter()
                .any(|w| w.code() == "overlay_aspect_mismatch"),
            mismatch,
            "{case}"
        );

        // The written file: right container, no EXIF orientation left behind.
        let written = probe(&out).unwrap();
        assert_eq!(
            written.container,
            match expect_format {
                PhotoFormat::Png => mr_composite::Container::Png,
                PhotoFormat::Jpeg => mr_composite::Container::Jpeg,
            },
            "{case}"
        );
        assert_eq!(written.image.as_ref().unwrap().orientation, 1, "{case}");

        let got = common::open_rgb(&out);
        let reference = common::open_rgb(&dir.join(item["reference"].as_str().unwrap()));
        common::assert_markers(&got, &item["markers"], case);
        let s = common::ssim(&got, &reference);
        eprintln!("{case}: orientation {orientation}, SSIM {s:.4}");
        assert!(s >= 0.98, "{case}: SSIM {s:.4} < 0.98");
    }
    assert!(
        [3u8, 6, 8].iter().all(|o| seen_orientations.contains(o)),
        "orientations 3, 6 and 8 are covered: {seen_orientations:?}"
    );
}

#[test]
fn jpeg_quality_is_configurable() {
    let Some((dir, truth)) = common::fixture("rotation") else {
        return;
    };
    let item = truth["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["case"] == "p_orientation_1")
        .unwrap();
    let main = dir.join(item["main"].as_str().unwrap());
    let overlay = dir.join(item["overlay"].as_str().unwrap());
    let out_dir = common::scratch("photo-quality");
    let sizes: Vec<u64> = [60u8, 92]
        .iter()
        .map(|q| {
            let out = out_dir.join(format!("q{q}.jpg"));
            let opts = PhotoOptions {
                jpeg_quality: *q,
                ..PhotoOptions::default()
            };
            composite_photo(&main, &overlay, &out, &opts).unwrap();
            std::fs::metadata(&out).unwrap().len()
        })
        .collect();
    assert!(
        sizes[0] < sizes[1],
        "quality 60 is smaller than 92: {sizes:?}"
    );
    // No partial file is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(&out_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains("mr-partial"))
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn undecodable_overlay_is_media_decode() {
    let Some((dir, truth)) = common::fixture("rotation") else {
        return;
    };
    let item = truth["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["media_type"] == "photo")
        .unwrap();
    let main = dir.join(item["main"].as_str().unwrap());
    let out_dir = common::scratch("photo-bad");
    let bad = out_dir.join("bad-overlay.png");
    std::fs::write(&bad, b"\x89PNG\r\n\x1a\nnot really a png").unwrap();
    let err = composite_photo(
        &main,
        &bad,
        &out_dir.join("o.jpg"),
        &PhotoOptions::default(),
    )
    .unwrap_err();
    assert_eq!(err.code(), "MEDIA_DECODE", "{err}");
    assert!(!out_dir.join("o.jpg").exists());
}
