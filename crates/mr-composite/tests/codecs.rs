//! F-09 codec robustness: the native probe against truth.json and against
//! ffmpeg's own `-i` listing (the oracle), then a transcode and a composite
//! of every case: H.264, HEVC 8-bit (hvc1, rotated) and 10-bit (hev1), VFR,
//! no audio, odd size, 25 fps.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};

use mr_composite::video::even_size;
use mr_composite::{Compositor, hevc_playback, probe};
use serde_json::Value;

/// What ffmpeg's `-i` listing says about the first video and audio streams.
#[derive(Debug, Default)]
struct Oracle {
    fourcc: String,
    size: (u32, u32),
    fps: Option<f64>,
    rotation: u16,
    ten_bit: bool,
    has_audio: bool,
}

fn oracle(ffmpeg: &Path, file: &Path) -> Oracle {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(file)
        .stdin(Stdio::null())
        .output()
        .expect("ffmpeg runs");
    let text = String::from_utf8_lossy(&out.stderr);
    let mut o = Oracle::default();
    let mut in_video = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with("Stream #") && l.contains("Video:") {
            in_video = true;
            // "... Video: h264 (High) (avc1 / 0x31637661), yuv420p(progressive), 640x360 [SAR ...], ..."
            if let Some(i) = l.find(" / 0x") {
                o.fourcc = l[..i].rsplit('(').next().unwrap().to_string();
            }
            o.ten_bit = l.contains("10le");
            for tok in l.split([',', ' ']) {
                if let Some((w, h)) = tok.split_once('x')
                    && let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>())
                    && o.size == (0, 0)
                {
                    o.size = (w, h);
                }
            }
            let parts: Vec<&str> = l.split(", ").collect();
            for p in parts {
                if let Some(v) = p.strip_suffix(" fps") {
                    o.fps = v.trim().parse().ok();
                }
            }
        } else if l.starts_with("Stream #") {
            in_video = false;
            if l.contains("Audio:") {
                o.has_audio = true;
            }
        } else if in_video && let Some(i) = l.find("rotation of ") {
            let deg: f64 = l[i + 12..]
                .split_whitespace()
                .next()
                .unwrap()
                .parse()
                .unwrap();
            // ffmpeg prints the counter-clockwise angle.
            o.rotation = ((-deg).round() as i32).rem_euclid(360) as u16;
        }
    }
    o
}

fn items(truth: &Value) -> &Vec<Value> {
    truth["items"].as_array().unwrap()
}

#[test]
fn f09_probe_matches_truth_and_the_ffmpeg_oracle() {
    let Some((dir, truth)) = common::fixture("codecs") else {
        return;
    };
    let ffmpeg = common::ffmpeg();
    assert!(items(&truth).len() >= 5, "F-09 cases present");
    for item in items(&truth) {
        let case = item["case"].as_str().unwrap();
        let e = &item["expected"];
        let path = dir.join(item["main"].as_str().unwrap());
        let info = probe(&path).unwrap();
        let v = info.video.as_ref().unwrap();
        eprintln!(
            "{case}: {} {}x{} r{} {:?} fps vfr={} {:?}-bit audio={:?} {:.3} s {:?} kbit/s",
            v.codec,
            v.coded_width,
            v.coded_height,
            v.rotation,
            v.frame_rate,
            v.variable_frame_rate,
            v.bit_depth,
            info.audio.as_ref().map(|a| a.codec.as_str()),
            info.duration_secs,
            info.bitrate_kbps
        );
        assert_eq!(v.codec, e["codec"].as_str().unwrap(), "{case}");
        assert_eq!(
            (v.coded_width, v.coded_height),
            common::size_of(&e["coded_size"]),
            "{case}"
        );
        assert_eq!(v.rotation as u64, e["rotation"].as_u64().unwrap(), "{case}");
        assert_eq!(
            v.display_size(),
            common::size_of(&e["display_size"]),
            "{case}"
        );
        assert_eq!(v.variable_frame_rate, e["vfr"].as_bool().unwrap(), "{case}");
        if let Some(fps) = e["fps"].as_f64() {
            assert!(
                (v.frame_rate.unwrap() - fps).abs() < 0.01,
                "{case}: {:?}",
                v.frame_rate
            );
        }
        assert_eq!(
            v.bit_depth.map(u64::from),
            e["bit_depth"].as_u64(),
            "{case}"
        );
        assert_eq!(
            info.audio.is_some(),
            e["has_audio"].as_bool().unwrap(),
            "{case}"
        );
        if let Some(a) = &info.audio {
            assert_eq!((a.codec.as_str(), a.sample_rate), ("aac", 44100), "{case}");
        }
        assert!(
            (info.duration_secs - e["duration"].as_f64().unwrap()).abs() < 0.05,
            "{case}"
        );
        assert!(info.bitrate_kbps.unwrap() > 0 && v.bitrate_kbps.unwrap() > 0);

        if let Some(ff) = &ffmpeg {
            let o = oracle(ff, &path);
            assert_eq!(o.fourcc, v.codec, "{case}: oracle {o:?}");
            assert_eq!(
                o.size,
                (v.coded_width, v.coded_height),
                "{case}: oracle {o:?}"
            );
            assert_eq!(o.rotation, v.rotation, "{case}: oracle {o:?}");
            assert_eq!(o.ten_bit, v.bit_depth == Some(10), "{case}: oracle {o:?}");
            assert_eq!(o.has_audio, info.audio.is_some(), "{case}: oracle {o:?}");
            if !v.variable_frame_rate {
                assert!(
                    (o.fps.unwrap() - v.frame_rate.unwrap()).abs() < 0.01,
                    "{case}: oracle {o:?}"
                );
            }
        }
    }
}

#[test]
fn f09_every_codec_transcodes_and_composites() {
    let ffmpeg = need_ffmpeg!();
    let Some((dir, truth)) = common::fixture("codecs") else {
        return;
    };
    let c = Compositor::new(&ffmpeg);
    let out_dir = common::scratch("codecs");
    for item in items(&truth) {
        let case = item["case"].as_str().unwrap();
        let e = &item["expected"];
        let main = dir.join(item["main"].as_str().unwrap());
        let src = probe(&main).unwrap();
        let display = common::size_of(&e["display_size"]);

        // Windows compatibility mode: H.264, upright, even size.
        let out = out_dir.join(format!("{case}-h264.mp4"));
        let outcome = c.transcode_h264(&main, &out).unwrap();
        let got = probe(&out).unwrap();
        let v = got.video.as_ref().unwrap();
        eprintln!(
            "{case}: transcode {:?} in {:.2} s, {}x{}, {:?}",
            outcome.encoder.unwrap(),
            outcome.elapsed.as_secs_f64(),
            v.coded_width,
            v.coded_height,
            outcome
                .warnings
                .iter()
                .map(|w| w.code())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            (v.codec.as_str(), v.rotation, v.bit_depth),
            ("avc1", 0, Some(8)),
            "{case}"
        );
        assert_eq!(
            (v.coded_width, v.coded_height),
            even_size(display),
            "{case}"
        );
        assert_eq!(
            outcome
                .warnings
                .iter()
                .any(|w| w.code() == "odd_size_scaled"),
            even_size(display) != display,
            "{case}"
        );
        assert_eq!(got.audio.is_some(), src.audio.is_some(), "{case}");
        assert!(
            (got.duration_secs - src.duration_secs).abs() < 0.1,
            "{case}"
        );
        let sv = src.video.as_ref().unwrap();
        assert_eq!(v.frame_count, sv.frame_count, "{case}: every frame kept");
        assert_eq!(
            v.variable_frame_rate, sv.variable_frame_rate,
            "{case}: timing kept"
        );

        // Composite with an overlay of the display size.
        let ov = out_dir.join(format!("{case}-overlay.png"));
        image::RgbaImage::from_fn(display.0, display.1, |x, y| {
            if (y as f64) < display.1 as f64 * 0.2 && x % 16 < 8 {
                image::Rgba([255, 255, 255, 200])
            } else {
                image::Rgba([0, 0, 0, 0])
            }
        })
        .save(&ov)
        .unwrap();
        let out = out_dir.join(format!("{case}-composite.mp4"));
        c.composite_video(&main, &ov, &out).unwrap();
        let got = probe(&out).unwrap();
        assert_eq!(got.video.as_ref().unwrap().codec, "avc1", "{case}");
        assert_eq!(got.audio.is_some(), src.audio.is_some(), "{case}");
    }
}

#[test]
fn hevc_playback_check_is_consistent() {
    let t = std::time::Instant::now();
    let h = hevc_playback();
    eprintln!("{h:?} in {} ms", t.elapsed().as_millis());
    assert_eq!(
        h.available,
        h.package.is_some() || h.decoder_mfts.unwrap_or(0) > 0 || !cfg!(windows)
    );
    if let Some(p) = &h.package {
        assert!(p.starts_with("Microsoft.HEVCVideoExtension"), "{p}");
    }
    assert!(
        t.elapsed().as_secs() < 5,
        "cheap enough to run at job start"
    );
}
