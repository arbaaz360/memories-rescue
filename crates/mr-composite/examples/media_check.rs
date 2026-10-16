//! Report what this machine can do: the H.264 encoders ffmpeg lists, which
//! of them pass a smoke encode, the encoder plan the compositor would use,
//! and whether Windows can play HEVC (FR-51).
//!
//!     cargo run -p mr-composite --example media_check -- [path/to/ffmpeg]
//!
//! Without an argument it uses `third_party/ffmpeg/ffmpeg(.exe)` from the
//! repository, then `ffmpeg` on PATH.

use std::path::PathBuf;
use std::time::Instant;

use mr_composite::ffmpeg::{Platform, VideoEncoder, encoder_candidates};
use mr_composite::{Compositor, hevc_playback};

fn main() {
    let ffmpeg = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let bundled = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../third_party/ffmpeg")
                .join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" });
            if bundled.is_file() {
                bundled
            } else {
                PathBuf::from("ffmpeg")
            }
        });
    println!("ffmpeg: {}", ffmpeg.display());
    let c = Compositor::new(&ffmpeg);
    match c.encoders() {
        Ok(set) => {
            let h264: Vec<&String> = set.video.iter().filter(|n| n.contains("264")).collect();
            println!("H.264 encoders listed: {h264:?}");
            let all = [
                VideoEncoder::MediaFoundation { hardware: true },
                VideoEncoder::MediaFoundation { hardware: false },
                VideoEncoder::VideoToolbox,
                VideoEncoder::OpenH264,
            ];
            for enc in all {
                if !set.has_video(enc.name()) {
                    continue;
                }
                let t = Instant::now();
                let ok = c.ffmpeg().encoder_works(enc);
                println!(
                    "  smoke {enc:?}: {} ({} ms)",
                    if ok { "works" } else { "FAILS" },
                    t.elapsed().as_millis()
                );
            }
            println!(
                "§4.4 candidates on {:?}: {:?}",
                Platform::current(),
                encoder_candidates(Platform::current(), set, false)
            );
        }
        Err(e) => println!("encoders: {e} [{}]", e.code()),
    }
    match c.encoder_plan() {
        Ok(p) => println!("encoder plan: primary {:?}, software path {:?}", p.primary, p.software),
        Err(e) => println!("encoder plan: {e} [{}]", e.code()),
    }
    let t = Instant::now();
    let h = hevc_playback();
    println!(
        "HEVC playback: available={} package={:?} decoder_mfts={:?} ({} ms)",
        h.available,
        h.package,
        h.decoder_mfts,
        t.elapsed().as_millis()
    );
}
