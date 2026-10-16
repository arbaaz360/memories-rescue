//! FR-27 on F-08: the §4.5 detector against the groups stated in truth.json
//! (default years and all years), AC2 decoys, AC1 (10 + 10 + 4 s with 1 s
//! gaps gives one 24 s ± 0.1 s file with no audio gap at the joins), per
//! segment overlays, and a rotated stream-copy join.

mod common;

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use mr_composite::{
    ClipDescriptor, Compositor, RejoinOptions, Segment, YearRange, detect_clip_groups, probe,
};
use serde_json::Value;

fn descriptors(dir: &Path, truth: &Value) -> Vec<ClipDescriptor> {
    truth["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let info = probe(&dir.join(i["main"].as_str().unwrap())).unwrap();
            let when: DateTime<Utc> = i["date_utc"].as_str().unwrap().parse().unwrap();
            let loc = i["location"]
                .as_object()
                .map(|l| (l["lat"].as_f64().unwrap(), l["lon"].as_f64().unwrap()));
            ClipDescriptor::from_probe(i["id"].as_str().unwrap(), when, loc, &info).unwrap()
        })
        .collect()
}

fn ids(v: &Value) -> Vec<Vec<String>> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|g| {
            g.as_array()
                .unwrap()
                .iter()
                .map(|s| s.as_str().unwrap().to_string())
                .collect()
        })
        .collect()
}

fn segments(dir: &Path, truth: &Value, group: &Value) -> Vec<Segment> {
    group
        .as_array()
        .unwrap()
        .iter()
        .map(|id| {
            let item = truth["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|i| i["id"] == *id)
                .unwrap();
            Segment {
                path: dir.join(item["main"].as_str().unwrap()),
                overlay: item["overlay"].as_str().map(|o| dir.join(o)),
            }
        })
        .collect()
}

#[test]
fn fr27_detection_matches_the_stated_groups() {
    let Some((dir, truth)) = common::fixture("clips") else {
        return;
    };
    let items = descriptors(&dir, &truth);
    let found: Vec<Vec<String>> = detect_clip_groups(&items, &RejoinOptions::default())
        .into_iter()
        .map(|g| g.item_ids)
        .collect();
    assert_eq!(found, ids(&truth["expected_groups"]["default"]));

    let all = RejoinOptions {
        years: YearRange::All,
        ..RejoinOptions::default()
    };
    let found_all: Vec<Vec<String>> = detect_clip_groups(&items, &all)
        .into_iter()
        .map(|g| g.item_ids)
        .collect();
    assert_eq!(found_all, ids(&truth["expected_groups"]["all_years"]));

    // AC2 and the other decoys are in no group.
    let cases = &truth["cases"];
    let grouped: Vec<&String> = found_all.iter().flatten().collect();
    for key in [
        "ac2_decoy_resolution",
        "decoy_gap",
        "location_far",
        "short_middle_alone",
    ] {
        let id = cases[key].as_str().unwrap().to_string();
        assert!(!grouped.contains(&&id), "{key} must not be joined");
    }
    for id in cases["fps_decoy"].as_array().unwrap() {
        assert!(
            !grouped.iter().any(|g| *g == id.as_str().unwrap()),
            "fps decoy joined"
        );
    }
    let ac1 = detect_clip_groups(&items, &RejoinOptions::default())
        .into_iter()
        .find(|g| g.item_ids == ids(&Value::Array(vec![cases["ac1"].clone()]))[0])
        .unwrap();
    assert!((ac1.total_secs - 24.0).abs() < 0.1, "{}", ac1.total_secs);
}

#[test]
fn fr27_ac1_three_clips_join_into_24_s_without_audio_gaps() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = common::fixture("clips") else {
        return;
    };
    let segs = segments(&dir, &truth, &truth["cases"]["ac1"]);
    let out = common::scratch("clips-ac1").join("joined.mp4");
    let c = Compositor::new(&ffmpeg);
    let outcome = c.rejoin(&segs, &out).unwrap();
    eprintln!("AC1: ffmpeg {}", outcome.args.join(" "));
    assert_eq!(
        outcome.encoder, None,
        "no overlays: the video is stream-copied"
    );
    assert!(matches!(outcome.audio, mr_composite::AudioPlan::Aac { .. }));

    let got = probe(&out).unwrap();
    let v = got.video.as_ref().unwrap();
    eprintln!(
        "AC1: file {:.3} s, video {:.3} s, audio {:.3} s in {:.2} s",
        got.duration_secs,
        v.presentation_end_secs(),
        got.audio.as_ref().unwrap().presentation_end_secs(),
        outcome.elapsed.as_secs_f64()
    );
    assert!(
        (got.duration_secs - 24.0).abs() <= 0.1,
        "{}",
        got.duration_secs
    );
    assert!((v.presentation_end_secs() - 24.0).abs() <= 0.1);
    let a_end = got.audio.as_ref().unwrap().presentation_end_secs();
    assert!(
        (v.presentation_end_secs() - a_end).abs() <= 0.040,
        "A/V ends differ by {:.1} ms",
        (v.presentation_end_secs() - a_end).abs() * 1000.0
    );
    let frames: u32 = segs
        .iter()
        .map(|s| probe(&s.path).unwrap().video.unwrap().frame_count)
        .sum();
    assert_eq!(v.frame_count, frames, "every frame of every clip is there");

    // No audible gap: every 10 ms window within 200 ms of each join keeps at
    // least half the median level of the (constant) tone.
    let pcm = common::pcm_mono(&ffmpeg, &out);
    let rms = common::rms_windows(&pcm, 44100);
    let mut sorted = rms.clone();
    sorted.sort_by(f64::total_cmp);
    let median = sorted[sorted.len() / 2];
    let mut t = 0.0;
    for s in &segs[..segs.len() - 1] {
        t += probe(&s.path).unwrap().duration_secs;
        let lo = ((t - 0.2) * 100.0) as usize;
        let hi = (((t + 0.2) * 100.0) as usize).min(rms.len());
        let worst = rms[lo..hi].iter().cloned().fold(f64::MAX, f64::min);
        eprintln!(
            "join at {t:.3} s: lowest 10 ms window {:.0}% of median",
            worst / median * 100.0
        );
        assert!(worst >= 0.5 * median, "audio gap at the join near {t:.3} s");
    }
}

#[test]
fn fr27_overlays_are_burned_into_their_own_segments() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = common::fixture("clips") else {
        return;
    };
    let segs = segments(&dir, &truth, &truth["cases"]["overlay_group"]);
    let with_overlay: Vec<usize> = truth["cases"]["overlay_segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap() as usize)
        .collect();
    let scratch = common::scratch("clips-overlay");
    let out = scratch.join("joined.mp4");
    let c = Compositor::new(&ffmpeg);
    let outcome = c.rejoin(&segs, &out).unwrap();
    eprintln!("overlays: ffmpeg {}", outcome.args.join(" "));
    assert!(outcome.encoder.is_some(), "overlays force a re-encode");
    let got = probe(&out).unwrap();
    let v = got.video.as_ref().unwrap();
    assert_eq!((v.codec.as_str(), v.rotation), ("avc1", 0));
    assert_eq!(
        (v.coded_width, v.coded_height),
        (288, 512),
        "rotated sources come out upright"
    );
    let durations: Vec<f64> = segs
        .iter()
        .map(|s| probe(&s.path).unwrap().duration_secs)
        .collect();
    let total: f64 = durations.iter().sum();
    assert!(
        (got.duration_secs - total).abs() <= 0.1,
        "{} vs {total}",
        got.duration_secs
    );

    // The sticker sits at (0.5 w, 0.3 h). Where a segment has its overlay the
    // joined frame differs there from the source frame; elsewhere it matches.
    let (sx, sy) = (144, 154);
    let mut start = 0.0;
    for (k, seg) in segs.iter().enumerate() {
        let mid = durations[k] / 2.0;
        let src = common::frame_at(
            &ffmpeg,
            &seg.path,
            mid,
            &scratch.join(format!("src{k}.png")),
        );
        let joined = common::frame_at(
            &ffmpeg,
            &out,
            start + mid,
            &scratch.join(format!("out{k}.png")),
        );
        let a = common::mean_rgb(&src, sx, sy, 24);
        let b = common::mean_rgb(&joined, sx, sy, 24);
        let diff = (0..3).map(|i| (a[i] - b[i]).abs()).sum::<f64>() / 3.0;
        let s = common::ssim(&src, &joined);
        eprintln!(
            "segment {k} at {:.2} s: sticker diff {diff:.1}, frame SSIM {s:.3}",
            start + mid
        );
        if with_overlay.contains(&k) {
            assert!(
                diff > 30.0,
                "segment {k} should carry its overlay (diff {diff:.1})"
            );
        } else {
            assert!(
                diff < 12.0,
                "segment {k} should have no overlay (diff {diff:.1})"
            );
            assert!(s > 0.9, "segment {k} should match its source (SSIM {s:.3})");
        }
        start += durations[k];
    }
}

#[test]
fn fr27_rotated_group_stream_copy_keeps_the_rotation() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = common::fixture("clips") else {
        return;
    };
    let segs = segments(&dir, &truth, &truth["cases"]["rotated_copy_group"]);
    let out = common::scratch("clips-rotated").join("joined.mp4");
    let outcome = Compositor::new(&ffmpeg).rejoin(&segs, &out).unwrap();
    assert_eq!(outcome.encoder, None);
    let got = probe(&out).unwrap();
    let v = got.video.as_ref().unwrap();
    assert_eq!(
        v.rotation, 90,
        "the display matrix of the first clip is kept"
    );
    assert_eq!(v.display_size(), (288, 512));
    assert!(
        (got.duration_secs - 23.0).abs() <= 0.1,
        "{}",
        got.duration_secs
    );
    let frame = common::frame_at(&ffmpeg, &out, 12.0, &out.with_extension("png"));
    assert_eq!(frame.dimensions(), (288, 512), "plays upright");
}

#[test]
fn rejoin_refuses_mismatched_segments() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = common::fixture("clips") else {
        return;
    };
    let mut segs = segments(&dir, &truth, &truth["cases"]["ac1"]);
    let decoy = truth["cases"]["ac2_decoy_resolution"].clone();
    segs.extend(segments(&dir, &truth, &Value::Array(vec![decoy])));
    let out: PathBuf = common::scratch("clips-refuse").join("x.mp4");
    let err = Compositor::new(&ffmpeg).rejoin(&segs, &out).unwrap_err();
    assert_eq!(err.code(), "MEDIA_DECODE", "{err}");
    assert!(!out.exists());
}
