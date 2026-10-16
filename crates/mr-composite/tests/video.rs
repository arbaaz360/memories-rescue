//! FR-24 on F-07: AC1 (portrait rotate 90/270, 180 and landscape composites
//! upright, overlay aligned, frame-sample SSIM >= 0.95 against the reference
//! ffmpeg filter graph) and AC2 (A/V drift <= 40 ms over 60 s, from the
//! output's track durations). Needs ffmpeg; skips cleanly without it.

mod common;

use mr_composite::{Compositor, VideoEncoder, probe};
use serde_json::Value;

fn videos(truth: &Value) -> Vec<&Value> {
    truth["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["media_type"] == "video")
        .collect()
}

#[test]
fn fr24_ac1_composites_are_upright_and_match_the_reference() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = common::fixture("rotation") else {
        return;
    };
    let vids = videos(&truth);
    if vids.is_empty() {
        eprintln!("skip: F-07 was generated without its videos");
        return;
    }
    let c = Compositor::new(&ffmpeg);
    let out_dir = common::scratch("video");
    let mut rotations = std::collections::BTreeSet::new();
    for item in vids {
        let case = item["case"].as_str().unwrap();
        if case == "v_landscape_60s" {
            continue; // AC2 covers it; AC1 runs on the 3-second clips
        }
        let main = dir.join(item["main"].as_str().unwrap());
        let overlay = dir.join(item["overlay"].as_str().unwrap());
        let out = out_dir.join(format!("{case}.mp4"));

        let src = probe(&main).unwrap();
        let rotation = item["rotation"].as_u64().unwrap() as u16;
        assert_eq!(src.video.as_ref().unwrap().rotation, rotation, "{case}");
        rotations.insert(rotation);

        let outcome = c.composite_video(&main, &overlay, &out).unwrap();
        eprintln!(
            "{case}: {:?} {} kbit/s in {:.2} s{}",
            outcome.encoder.unwrap(),
            outcome.bitrate_kbps.unwrap(),
            outcome.elapsed.as_secs_f64(),
            if outcome.retried { " (retried)" } else { "" }
        );
        assert!(!outcome.retried, "{case}: {:?}", outcome.first_failure);
        eprintln!("  ffmpeg {}", outcome.args.join(" "));
        let display = common::size_of(&item["display_size"]);
        assert_eq!(outcome.output_size, display, "{case}");
        let mismatch = item["placement"]["aspect_mismatch"].as_bool().unwrap();
        assert_eq!(
            outcome
                .warnings
                .iter()
                .any(|w| w.code() == "overlay_aspect_mismatch"),
            mismatch,
            "{case}"
        );

        // The output is H.264, upright in its pixels, with no rotation flag.
        let got = probe(&out).unwrap();
        let v = got.video.as_ref().unwrap();
        assert_eq!(v.codec, "avc1", "{case}");
        assert_eq!(v.rotation, 0, "{case}: rotation flag must be removed");
        assert_eq!((v.coded_width, v.coded_height), display, "{case}");
        assert!(got.audio.is_some(), "{case}: audio kept");
        assert!(
            (got.duration_secs - src.duration_secs).abs() < 0.1,
            "{case}: {} vs {}",
            got.duration_secs,
            src.duration_secs
        );

        for f in item["reference_frames"].as_array().unwrap() {
            let t = f["t"].as_f64().unwrap();
            let png = out_dir.join(format!("{case}_{t:.3}.png"));
            let frame = common::frame_at(&ffmpeg, &out, t, &png);
            let reference = common::open_rgb(&dir.join(f["file"].as_str().unwrap()));
            common::assert_markers(&frame, &item["markers"], case);
            let s = common::ssim(&frame, &reference);
            eprintln!("  t={t:.3}: SSIM {s:.4}");
            assert!(s >= 0.95, "{case} at {t}: SSIM {s:.4} < 0.95");
        }
    }
    assert!(
        [90u16, 180, 270].iter().all(|r| rotations.contains(r)),
        "{rotations:?}"
    );
}

#[test]
fn fr24_ac2_av_drift_within_40_ms_over_60_s() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = common::fixture("rotation") else {
        return;
    };
    let Some(item) = videos(&truth)
        .into_iter()
        .find(|i| i["case"] == "v_landscape_60s")
    else {
        eprintln!("skip: F-07 was generated without its videos");
        return;
    };
    let main = dir.join(item["main"].as_str().unwrap());
    let overlay = dir.join(item["overlay"].as_str().unwrap());
    let out = common::scratch("video-ac2").join("landscape_60s.mp4");
    let c = Compositor::new(&ffmpeg);
    let outcome = c.composite_video(&main, &overlay, &out).unwrap();

    let src = probe(&main).unwrap();
    let got = probe(&out).unwrap();
    let (sv, sa) = (src.video.unwrap(), src.audio.unwrap());
    let (gv, ga) = (got.video.unwrap(), got.audio.unwrap());
    let src_offset = sv.presentation_end_secs() - sa.presentation_end_secs();
    let out_offset = gv.presentation_end_secs() - ga.presentation_end_secs();
    let drift = (out_offset - src_offset).abs();
    eprintln!(
        "60 s landscape via {:?} in {:.2} s: video {:.4} s, audio {:.4} s (source {:.4} / {:.4}); drift {:.1} ms",
        outcome.encoder.unwrap(),
        outcome.elapsed.as_secs_f64(),
        gv.presentation_end_secs(),
        ga.presentation_end_secs(),
        sv.presentation_end_secs(),
        sa.presentation_end_secs(),
        drift * 1000.0
    );
    assert!(src.duration_secs > 59.9, "the AC2 clip is 60 s long");
    assert!(drift <= 0.040, "A/V drift {:.1} ms > 40 ms", drift * 1000.0);
    assert!((gv.presentation_end_secs() - sv.presentation_end_secs()).abs() <= 0.040);
    assert!((ga.presentation_end_secs() - sa.presentation_end_secs()).abs() <= 0.040);
    assert_eq!(
        gv.frame_count, sv.frame_count,
        "no frames dropped or duplicated"
    );
    // Audio was stream-copied: same sample count.
    assert_eq!(ga.sample_count, sa.sample_count);
    assert_eq!(outcome.audio, mr_composite::AudioPlan::Copy);
}

#[test]
fn encoder_plan_on_this_machine() {
    let ffmpeg = need_ffmpeg!();
    let c = Compositor::new(&ffmpeg);
    let plan = c.encoder_plan().expect("an H.264 encoder works here");
    eprintln!("encoder plan: {plan:?}");
    if cfg!(windows) && c.encoders().unwrap().has_video("h264_mf") {
        assert_eq!(
            plan.primary,
            VideoEncoder::MediaFoundation { hardware: false }
        );
    }
    if cfg!(target_os = "macos") && c.encoders().unwrap().has_video("h264_videotoolbox") {
        assert_eq!(plan.primary, VideoEncoder::VideoToolbox);
    }
    // Cached: a second call returns the same plan without re-running ffmpeg.
    assert_eq!(c.encoder_plan().unwrap(), plan);
}

#[test]
fn windows_compatibility_transcode_of_a_rotated_clip() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = common::fixture("rotation") else {
        return;
    };
    let Some(item) = videos(&truth)
        .into_iter()
        .find(|i| i["case"] == "v_rotate_270")
    else {
        eprintln!("skip: F-07 was generated without its videos");
        return;
    };
    let main = dir.join(item["main"].as_str().unwrap());
    let out = common::scratch("video-transcode").join("t.mp4");
    let c = Compositor::new(&ffmpeg);
    let outcome = c.transcode_h264(&main, &out).unwrap();
    eprintln!("transcode: ffmpeg {}", outcome.args.join(" "));
    let got = probe(&out).unwrap();
    let v = got.video.unwrap();
    assert_eq!((v.codec.as_str(), v.rotation), ("avc1", 0));
    assert_eq!(outcome.output_size, common::size_of(&item["display_size"]));
    let frame = common::frame_at(&ffmpeg, &out, 1.517, &out.with_extension("png"));
    common::assert_markers(&frame, &item["markers"], "transcode v_rotate_270");
}
