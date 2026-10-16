//! Shared harness for the mr-composite integration tests: fixtures, the
//! ffmpeg used by the compositor and as an oracle, SSIM, frames and audio.
#![allow(dead_code)]

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Once, OnceLock};

use image::{GrayImage, RgbImage};
use serde_json::Value;

pub const M3_FIXTURES: [&str; 3] = ["rotation", "clips", "codecs"];

pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

static GENERATE: Once = Once::new();

/// Generate F-07, F-08 and F-09 once per test binary if any is missing.
pub fn ensure_fixtures() {
    GENERATE.call_once(|| {
        let root = repo_root();
        let missing = M3_FIXTURES
            .iter()
            .any(|n| !root.join("fixtures").join(n).join("truth.json").is_file());
        if !missing {
            return;
        }
        eprintln!("M3 fixtures missing; running tools/make_fixture for rotation, clips, codecs");
        let args = [
            "tools/make_fixture",
            "--fixture",
            "rotation",
            "--fixture",
            "clips",
            "--fixture",
            "codecs",
            "--quiet",
        ];
        for python in ["python", "python3", "py"] {
            match Command::new(python).args(args).current_dir(&root).status() {
                Ok(s) if s.success() => return,
                Ok(s) => panic!("{python} tools/make_fixture failed with {s}"),
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => panic!("could not run {python}: {e}"),
            }
        }
        panic!("no Python interpreter found; run `python tools/make_fixture --all`");
    });
}

/// `fixtures/<name>` and its truth.json. `None` (with a skip message) when
/// the generator had no ffmpeg and wrote a skipped truth.
pub fn fixture(name: &str) -> Option<(PathBuf, Value)> {
    ensure_fixtures();
    let dir = repo_root().join("fixtures").join(name);
    let truth: Value = serde_json::from_slice(
        &std::fs::read(dir.join("truth.json")).expect("truth.json readable"),
    )
    .expect("truth.json parses");
    if let Some(reason) = truth.get("skipped").and_then(Value::as_str) {
        eprintln!("skip: fixture {name} was generated without media ({reason})");
        return None;
    }
    Some((dir, truth))
}

fn runs(path: &Path) -> bool {
    path.is_file()
        && std::fs::metadata(path)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
        && Command::new(path)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

/// The ffmpeg the compositor tests use: `MR_FFMPEG` when set (e.g. the
/// minimal release build, `third_party/ffmpeg/dist/...`), else the sidecar
/// staged in `third_party/ffmpeg`, else one on PATH. `None` on the CI
/// runners, which have none of these; tests then skip.
pub fn ffmpeg() -> Option<PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            if let Some(p) = std::env::var_os("MR_FFMPEG").map(PathBuf::from) {
                assert!(runs(&p), "MR_FFMPEG={} does not run", p.display());
                eprintln!("ffmpeg: {} (MR_FFMPEG)", p.display());
                return Some(p);
            }
            let exe = if cfg!(windows) {
                "ffmpeg.exe"
            } else {
                "ffmpeg"
            };
            let bundled = repo_root().join("third_party").join("ffmpeg").join(exe);
            if runs(&bundled) {
                return Some(bundled);
            }
            let path = std::env::var_os("PATH")?;
            std::env::split_paths(&path)
                .map(|d| d.join(exe))
                .find(|p| runs(p))
        })
        .clone()
}

/// Skip the calling test (return early) when ffmpeg is absent.
#[macro_export]
macro_rules! need_ffmpeg {
    () => {
        match common::ffmpeg() {
            Some(f) => f,
            None => {
                eprintln!("skip: no ffmpeg (python tools/sidecars.py fetch, or ffmpeg on PATH)");
                return;
            }
        }
    };
}

/// A fresh scratch directory for one test.
pub fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mr-composite-test-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Run ffmpeg as an oracle or helper; panics with stderr on failure.
pub fn ffmpeg_run(ffmpeg: &Path, args: &[&std::ffi::OsStr]) -> Vec<u8> {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("ffmpeg runs");
    assert!(
        out.status.success(),
        "ffmpeg {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// The frame shown at `t` seconds, as RGB (ffmpeg seeks accurately and
/// applies any display matrix, as a player would).
pub fn frame_at(ffmpeg: &Path, video: &Path, t: f64, png: &Path) -> RgbImage {
    let ts = format!("{t:.3}");
    ffmpeg_run(
        ffmpeg,
        &[
            "-ss".as_ref(),
            ts.as_ref(),
            "-i".as_ref(),
            video.as_os_str(),
            "-frames:v".as_ref(),
            "1".as_ref(),
            png.as_os_str(),
        ],
    );
    image::open(png).expect("frame decodes").into_rgb8()
}

/// Decoded audio as mono 16-bit samples at 44.1 kHz.
pub fn pcm_mono(ffmpeg: &Path, video: &Path) -> Vec<i16> {
    let raw = ffmpeg_run(
        ffmpeg,
        &[
            "-i".as_ref(),
            video.as_os_str(),
            "-map".as_ref(),
            "0:a:0".as_ref(),
            "-ac".as_ref(),
            "1".as_ref(),
            "-ar".as_ref(),
            "44100".as_ref(),
            "-f".as_ref(),
            "s16le".as_ref(),
            "pipe:1".as_ref(),
        ],
    );
    let (samples, _) = raw.as_chunks::<2>();
    samples.iter().map(|b| i16::from_le_bytes(*b)).collect()
}

/// RMS of consecutive 10 ms windows.
pub fn rms_windows(pcm: &[i16], rate: usize) -> Vec<f64> {
    let w = rate / 100;
    pcm.chunks_exact(w)
        .map(|c| (c.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / w as f64).sqrt())
        .collect()
}

/// BT.601 luma.
pub fn luma(img: &RgbImage) -> GrayImage {
    GrayImage::from_fn(img.width(), img.height(), |x, y| {
        let p = img.get_pixel(x, y).0;
        let v = 0.299 * p[0] as f64 + 0.587 * p[1] as f64 + 0.114 * p[2] as f64;
        image::Luma([v.round().clamp(0.0, 255.0) as u8])
    })
}

/// SSIM of two equally sized images on luma, the way ffmpeg's `ssim`
/// filter computes it: 8x8 windows every 4 pixels, uniform weights,
/// C1 = (0.01 * 255)^2, C2 = (0.03 * 255)^2, averaged over all windows.
pub fn ssim(a: &RgbImage, b: &RgbImage) -> f64 {
    assert_eq!(a.dimensions(), b.dimensions(), "SSIM needs equal sizes");
    let (ya, yb) = (luma(a), luma(b));
    let (w, h) = ya.dimensions();
    const C1: f64 = (0.01 * 255.0) * (0.01 * 255.0);
    const C2: f64 = (0.03 * 255.0) * (0.03 * 255.0);
    let mut total = 0.0;
    let mut n = 0usize;
    let mut y0 = 0;
    while y0 + 8 <= h {
        let mut x0 = 0;
        while x0 + 8 <= w {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for y in y0..y0 + 8 {
                for x in x0..x0 + 8 {
                    let pa = ya.get_pixel(x, y).0[0] as f64;
                    let pb = yb.get_pixel(x, y).0[0] as f64;
                    sa += pa;
                    sb += pb;
                    saa += pa * pa;
                    sbb += pb * pb;
                    sab += pa * pb;
                }
            }
            let k = 64.0;
            let (ma, mb) = (sa / k, sb / k);
            let va = saa / k - ma * ma;
            let vb = sbb / k - mb * mb;
            let cov = sab / k - ma * mb;
            total += ((2.0 * ma * mb + C1) * (2.0 * cov + C2))
                / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            n += 1;
            x0 += 4;
        }
        y0 += 4;
    }
    total / n as f64
}

/// Mean colour of a `size`-pixel square centred at `(cx, cy)`.
pub fn mean_rgb(img: &RgbImage, cx: u32, cy: u32, size: u32) -> [f64; 3] {
    let half = size / 2;
    let mut acc = [0.0; 3];
    let mut n = 0.0;
    for y in cy.saturating_sub(half)..(cy + half).min(img.height()) {
        for x in cx.saturating_sub(half)..(cx + half).min(img.width()) {
            let p = img.get_pixel(x, y).0;
            for c in 0..3 {
                acc[c] += p[c] as f64;
            }
            n += 1.0;
        }
    }
    acc.map(|v| v / n)
}

/// The red marker is in the top-left corner and the blue one in the
/// bottom-right: the frame is upright and not mirrored.
pub fn assert_markers(img: &RgbImage, markers: &Value, what: &str) {
    let m = markers["size"].as_u64().unwrap() as u32;
    let at = |k: &str| {
        let p = &markers[k];
        (p[0].as_u64().unwrap() as u32, p[1].as_u64().unwrap() as u32)
    };
    let (rx, ry) = at("red");
    let (bx, by) = at("blue");
    let red = mean_rgb(img, rx, ry, m / 2);
    let blue = mean_rgb(img, bx, by, m / 2);
    assert!(
        red[0] > 180.0 && red[1] < 80.0 && red[2] < 80.0,
        "{what}: top-left is {red:?}, expected the red marker"
    );
    assert!(
        blue[2] > 180.0 && blue[0] < 80.0 && blue[1] < 80.0,
        "{what}: bottom-right is {blue:?}, expected the blue marker"
    );
}

/// Decode by content, not by extension.
pub fn open_rgb(path: &Path) -> RgbImage {
    image::ImageReader::open(path)
        .expect("image opens")
        .with_guessed_format()
        .expect("format sniffed")
        .decode()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .into_rgb8()
}

pub fn size_of(v: &Value) -> (u32, u32) {
    (v[0].as_u64().unwrap() as u32, v[1].as_u64().unwrap() as u32)
}
