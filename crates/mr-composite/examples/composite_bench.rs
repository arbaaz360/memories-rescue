//! Time one overlay composite with every H.264 encoder this ffmpeg offers
//! (NFR-PERF-04: at least 1x realtime on the reference machines), and score
//! each output with ffmpeg's `ssim` filter against the uncompressed
//! composite.
//!
//!     cargo run --release -p mr-composite --example composite_bench -- \
//!         <main.mp4> <overlay.png> [path/to/ffmpeg]

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use mr_composite::ffmpeg::{Ffmpeg, VideoEncoder};
use mr_composite::probe;
use mr_composite::video::{OverlayInput, VideoOptions, composite_plan, place_overlay};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let (Some(main), Some(overlay)) = (args.next(), args.next()) else {
        eprintln!("usage: composite_bench <main.mp4> <overlay.png> [ffmpeg]");
        std::process::exit(2);
    };
    let (main, overlay) = (PathBuf::from(main), PathBuf::from(overlay));
    let ffmpeg = args.next().map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../third_party/ffmpeg")
            .join(if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            })
    });
    let ff = Ffmpeg::new(&ffmpeg);
    let info = probe(&main).expect("main probes");
    let ov = probe(&overlay)
        .expect("overlay probes")
        .image
        .expect("overlay is an image");
    let v = info.video.as_ref().expect("main has video");
    println!(
        "{}: {} {}x{} r{} {:.1} s, {} kbit/s",
        main.display(),
        v.codec,
        v.coded_width,
        v.coded_height,
        v.rotation,
        info.duration_secs,
        v.bitrate_kbps.unwrap_or(0)
    );
    let opts = VideoOptions::default();
    let out_dir = std::env::temp_dir().join("mr-composite-bench");
    std::fs::create_dir_all(&out_dir).unwrap();
    let candidates = [
        VideoEncoder::MediaFoundation { hardware: false },
        VideoEncoder::MediaFoundation { hardware: true },
        VideoEncoder::VideoToolbox,
        VideoEncoder::OpenH264,
    ];
    let listed = ff.encoders().expect("ffmpeg runs");
    println!("| encoder | wall s | x realtime | output kbit/s | SSIM (all frames) |");
    println!("|---|---|---|---|---|");
    for enc in candidates {
        if !listed.has_video(enc.name()) || !ff.encoder_works(enc) {
            continue;
        }
        let out = out_dir.join(format!(
            "{}.mp4",
            enc.name().to_owned()
                + if enc == (VideoEncoder::MediaFoundation { hardware: true }) {
                    "_hw"
                } else {
                    ""
                }
        ));
        let plan = composite_plan(
            &main,
            &info,
            Some(OverlayInput {
                path: &overlay,
                size: (ov.width, ov.height),
            }),
            &out,
            enc,
            false,
            &opts,
        )
        .unwrap();
        let r = ff.run(&plan.args, plan.timeout);
        let Ok(r) = r else {
            println!("| {enc:?} | failed: {} | | | |", r.unwrap_err());
            continue;
        };
        let secs = r.elapsed.as_secs_f64();
        let got = probe(&out).unwrap();
        let ssim = score(
            &ffmpeg,
            &out,
            &main,
            &overlay,
            plan.output_size,
            (ov.width, ov.height),
        );
        println!(
            "| {enc:?} | {secs:.2} | {:.1} | {} | {} |",
            info.duration_secs / secs,
            got.video.unwrap().bitrate_kbps.unwrap_or(0),
            ssim.unwrap_or_else(|| "n/a".into())
        );
    }
}

/// Average SSIM of `out` against main + overlay rendered by ffmpeg itself
/// (autorotate on), frame by frame.
fn score(
    ffmpeg: &std::path::Path,
    out: &std::path::Path,
    main: &std::path::Path,
    overlay: &std::path::Path,
    size: (u32, u32),
    ov: (u32, u32),
) -> Option<String> {
    let p = place_overlay(size, ov, 0.02);
    let graph = format!(
        "[1:v]scale={}:{}[base];[2:v]scale={}:{}[ov];[base][ov]overlay={}:{}:eof_action=repeat,format=yuv420p[ref];[0:v]format=yuv420p[out];[out][ref]ssim",
        size.0, size.1, p.width, p.height, p.x, p.y
    );
    let args: Vec<OsString> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-i".into(),
        out.into(),
        "-i".into(),
        main.into(),
        "-i".into(),
        overlay.into(),
        "-filter_complex".into(),
        graph.into(),
        "-f".into(),
        "null".into(),
        "-".into(),
    ];
    let o = Command::new(ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&o.stderr);
    let line = text.lines().rev().find(|l| l.contains("SSIM Y:"))?;
    let y = line.split("Y:").nth(1)?.split_whitespace().next()?;
    let all = line.split("All:").nth(1)?.split_whitespace().next()?;
    Some(format!("Y {y}, all {all}"))
}
