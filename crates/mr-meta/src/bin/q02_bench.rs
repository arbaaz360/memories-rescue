//! Q-02 benchmark: exiftool `-stay_open` sidecar versus native Rust writers
//! versus an ffmpeg remux, on the corpus from `tools/bench_corpus.py`.
//!
//! ```text
//! cargo run --release -p mr-meta --bin q02-bench -- [--corpus fixtures/bench] [--out <dir>]
//!     [--workers 1,4] [--json <file>] [--limit N]
//! ```
//!
//! Prints Markdown tables and writes the raw numbers as JSON. Binaries are
//! taken from `third_party/` (or `PATH`); approaches whose binary is missing
//! are skipped and said so.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use mr_meta::exiftool::{self, ExifTool};
use mr_meta::fields::Capture;
use mr_meta::{ffmpeg_remux, native_jpeg, native_mp4};
use serde_json::{Value, json};

const BASE_UNIX: i64 = 1_623_767_400; // 2021-06-15T14:30:00Z
const OFFSET_SECS: i32 = 3600; // BST
const BASE_GPS: (f64, f64) = (51.5074, -0.1278);

fn capture_for(i: usize) -> Capture {
    let (lat, lon) = BASE_GPS;
    let k = i as f64 * 1e-4;
    Capture::from_unix(BASE_UNIX + i as i64, OFFSET_SECS, Some((lat + k, lon - k)))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Media {
    Jpeg,
    Mp4,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Approach {
    ExifToolStayOpen,
    ExifToolPerProcess,
    Native,
    FfmpegRemux,
}

impl Approach {
    fn label(self) -> &'static str {
        match self {
            Approach::ExifToolStayOpen => "exiftool -stay_open (one process per worker)",
            Approach::ExifToolPerProcess => "exiftool, one process per file",
            Approach::Native => "native Rust (little_exif / moov patcher)",
            Approach::FfmpegRemux => "ffmpeg remux -c copy -metadata",
        }
    }
}

struct Tools {
    exiftool: Option<PathBuf>,
    ffmpeg: Option<PathBuf>,
    ffprobe: Option<PathBuf>,
}

struct RunResult {
    corpus: String,
    media: Media,
    approach: Approach,
    workers: usize,
    items: usize,
    errors: usize,
    elapsed: Duration,
    bytes_in: u64,
    bytes_out: u64,
}

impl RunResult {
    fn items_per_s(&self) -> f64 {
        self.items as f64 / self.elapsed.as_secs_f64()
    }
    fn ms_per_item(&self) -> f64 {
        self.elapsed.as_secs_f64() * 1000.0 / self.items as f64
    }
    fn overhead_bytes(&self) -> f64 {
        (self.bytes_out as f64 - self.bytes_in as f64) / self.items as f64
    }
}

fn main() {
    let mut corpus = mr_meta::repo_root().join("fixtures/bench");
    let mut out_dir = std::env::temp_dir().join("mr-meta-q02-bench");
    let mut workers = vec![1usize, 4];
    let mut json_path: Option<PathBuf> = None;
    let mut limit: Option<usize> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--corpus" => corpus = PathBuf::from(args.next().expect("--corpus DIR")),
            "--out" => out_dir = PathBuf::from(args.next().expect("--out DIR")),
            "--workers" => {
                workers = args
                    .next()
                    .expect("--workers 1,4")
                    .split(',')
                    .map(|w| w.parse().expect("worker count"))
                    .collect()
            }
            "--json" => json_path = Some(PathBuf::from(args.next().expect("--json FILE"))),
            "--limit" => limit = Some(args.next().expect("--limit N").parse().expect("limit")),
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }

    let root = mr_meta::repo_root();
    let tools = Tools {
        exiftool: exiftool::locate(&root),
        ffmpeg: ffmpeg_remux::locate(&root, "ffmpeg"),
        ffprobe: ffmpeg_remux::locate(&root, "ffprobe"),
    };
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    println!("# Q-02 benchmark\n");
    println!(
        "- host: {} {}, {cpus} logical CPUs",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    println!("- corpus: `{}`", corpus.display());
    println!("- output: `{}`", out_dir.display());
    for (name, p) in [
        ("exiftool", &tools.exiftool),
        ("ffmpeg", &tools.ffmpeg),
        ("ffprobe", &tools.ffprobe),
    ] {
        match p {
            Some(p) => println!("- {name}: `{}` ({})", p.display(), version_line(p, name)),
            None => println!("- {name}: not found (its approaches are skipped)"),
        }
    }
    println!();

    let mut report = json!({ "host": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH, "cpus": cpus } });

    // 1. Process start costs.
    println!("## Process start\n");
    println!("| Measure | min | median | max |");
    println!("|---|---|---|---|");
    let mut starts = serde_json::Map::new();
    if let Some(ex) = &tools.exiftool {
        let t = time_n(10, || run_quiet(ex, &["-ver"]));
        println!(
            "| `exiftool -ver` (10 runs) | {} | {} | {} |",
            ms(t.0),
            ms(t.1),
            ms(t.2)
        );
        starts.insert(
            "exiftool_ver_ms".into(),
            json!([
                t.0.as_secs_f64() * 1e3,
                t.1.as_secs_f64() * 1e3,
                t.2.as_secs_f64() * 1e3
            ]),
        );
        let mut et = ExifTool::spawn(ex).expect("spawn exiftool");
        let t = time_n(10, || {
            et.version().expect("-ver");
        });
        println!(
            "| `-ver` through `-stay_open` pipe (10 runs) | {} | {} | {} |",
            ms(t.0),
            ms(t.1),
            ms(t.2)
        );
        starts.insert(
            "exiftool_stay_open_ver_ms".into(),
            json!([
                t.0.as_secs_f64() * 1e3,
                t.1.as_secs_f64() * 1e3,
                t.2.as_secs_f64() * 1e3
            ]),
        );
    }
    if let Some(ff) = &tools.ffmpeg {
        let t = time_n(10, || run_quiet(ff, &["-version"]));
        println!(
            "| `ffmpeg -version` (10 runs) | {} | {} | {} |",
            ms(t.0),
            ms(t.1),
            ms(t.2)
        );
        starts.insert(
            "ffmpeg_version_ms".into(),
            json!([
                t.0.as_secs_f64() * 1e3,
                t.1.as_secs_f64() * 1e3,
                t.2.as_secs_f64() * 1e3
            ]),
        );
    }
    report["process_start"] = Value::Object(starts);
    println!();

    // 2. Throughput matrix.
    let mut results: Vec<RunResult> = Vec::new();
    let mut verifications: Vec<Value> = Vec::new();
    for corpus_name in ["fixture", "real"] {
        for media in [Media::Jpeg, Media::Mp4] {
            let sub = match media {
                Media::Jpeg => "jpeg",
                Media::Mp4 => "mp4",
            };
            let dir = corpus.join(corpus_name).join(sub);
            let Ok(rd) = fs::read_dir(&dir) else {
                println!("_skipping `{}`: not generated_\n", dir.display());
                continue;
            };
            let mut files: Vec<PathBuf> = rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.is_file())
                .collect();
            files.sort();
            if let Some(n) = limit {
                files.truncate(n);
            }
            if files.is_empty() {
                continue;
            }
            let approaches: Vec<(Approach, Vec<usize>)> = match media {
                Media::Jpeg => vec![
                    (Approach::ExifToolStayOpen, workers.clone()),
                    (Approach::ExifToolPerProcess, vec![1]),
                    (Approach::Native, workers.clone()),
                ],
                Media::Mp4 => vec![
                    (Approach::ExifToolStayOpen, workers.clone()),
                    (Approach::ExifToolPerProcess, vec![1]),
                    (Approach::Native, workers.clone()),
                    (Approach::FfmpegRemux, workers.clone()),
                ],
            };
            for (approach, worker_counts) in approaches {
                let needs = match approach {
                    Approach::ExifToolStayOpen | Approach::ExifToolPerProcess => {
                        tools.exiftool.is_some()
                    }
                    Approach::Native => true,
                    Approach::FfmpegRemux => tools.ffmpeg.is_some(),
                };
                if !needs {
                    continue;
                }
                // One process per file is slow; 20 files are enough to show the rate.
                let subset: Vec<PathBuf> = if approach == Approach::ExifToolPerProcess {
                    files.iter().take(20).cloned().collect()
                } else {
                    files.clone()
                };
                for &w in &worker_counts {
                    let run_out = out_dir.join(format!("{corpus_name}-{sub}-{:?}-w{w}", approach));
                    let _ = fs::remove_dir_all(&run_out);
                    fs::create_dir_all(&run_out).expect("create out dir");
                    let r =
                        run_matrix_cell(&tools, approach, media, &subset, &run_out, w, corpus_name);
                    eprintln!(
                        "{corpus_name:8} {sub:4} {:<44} w={w}  {:7.1} items/s  {:6.1} ms/item  errors={}",
                        approach.label(),
                        r.items_per_s(),
                        r.ms_per_item(),
                        r.errors
                    );
                    results.push(r);
                    // Verify the first file of the 1-worker run of each approach.
                    if w == worker_counts[0] {
                        let first_out = run_out.join(subset[0].file_name().unwrap());
                        let v = verify(&tools, media, &first_out, &capture_for(0));
                        verifications.push(json!({
                            "corpus": corpus_name, "media": sub, "approach": approach.label(),
                            "file": first_out.display().to_string(), "checks": v
                        }));
                        if media == Media::Mp4 && subset.len() > 1 {
                            // Second file: the moov-last variant in the real corpus.
                            let second_out = run_out.join(subset[1].file_name().unwrap());
                            let v = verify(&tools, media, &second_out, &capture_for(1));
                            verifications.push(json!({
                                "corpus": corpus_name, "media": sub, "approach": approach.label(),
                                "file": second_out.display().to_string(), "checks": v
                            }));
                        }
                    }
                    if w != 1 || approach != Approach::Native {
                        // Keep disk use bounded: only the single-worker native outputs
                        // and the verified files above are needed afterwards.
                        let _ = fs::remove_dir_all(&run_out);
                    }
                }
            }
        }
    }

    println!("## Throughput\n");
    println!(
        "| Corpus | Media | Approach | Workers | Items | items/s | ms/item | Size overhead (bytes/item) | Errors |"
    );
    println!("|---|---|---|---|---|---|---|---|---|");
    for r in &results {
        println!(
            "| {} | {:?} | {} | {} | {} | {:.1} | {:.1} | {:+.0} | {} |",
            r.corpus,
            r.media,
            r.approach.label(),
            r.workers,
            r.items,
            r.items_per_s(),
            r.ms_per_item(),
            r.overhead_bytes(),
            r.errors
        );
    }
    println!();

    println!("## Read-back verification (first file of each run)\n");
    println!("| Corpus | Media | Approach | File | Result |");
    println!("|---|---|---|---|---|");
    for v in &verifications {
        let checks = v["checks"].as_array().cloned().unwrap_or_default();
        let failed: Vec<String> = checks
            .iter()
            .filter(|c| c["ok"] == Value::Bool(false))
            .map(|c| format!("{}: got {}", c["field"].as_str().unwrap_or("?"), c["got"]))
            .collect();
        let name = Path::new(v["file"].as_str().unwrap_or(""))
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        println!(
            "| {} | {} | {} | {} | {} |",
            v["corpus"].as_str().unwrap_or(""),
            v["media"].as_str().unwrap_or(""),
            v["approach"].as_str().unwrap_or(""),
            name,
            if failed.is_empty() {
                format!("PASS ({} fields)", checks.len())
            } else {
                format!("FAIL: {}", failed.join("; "))
            }
        );
    }

    report["throughput"] = Value::Array(
        results
            .iter()
            .map(|r| {
                json!({
                    "corpus": r.corpus, "media": format!("{:?}", r.media), "approach": r.approach.label(),
                    "workers": r.workers, "items": r.items, "errors": r.errors,
                    "seconds": r.elapsed.as_secs_f64(), "items_per_s": r.items_per_s(),
                    "ms_per_item": r.ms_per_item(), "bytes_in": r.bytes_in, "bytes_out": r.bytes_out,
                    "overhead_bytes_per_item": r.overhead_bytes()
                })
            })
            .collect(),
    );
    report["verification"] = Value::Array(verifications);
    if let Some(p) = json_path {
        fs::write(&p, serde_json::to_string_pretty(&report).unwrap()).expect("write json");
        eprintln!("wrote {}", p.display());
    }
}

fn run_matrix_cell(
    tools: &Tools,
    approach: Approach,
    media: Media,
    files: &[PathBuf],
    out: &Path,
    workers: usize,
    corpus: &str,
) -> RunResult {
    let chunk = files.len().div_ceil(workers.max(1));
    let started = Instant::now();
    let totals: Vec<(usize, usize, u64, u64)> = std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk.max(1))
            .enumerate()
            .map(|(ci, part)| {
                let base_index = ci * chunk;
                s.spawn(move || worker(tools, approach, media, part, out, base_index))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("worker panicked"))
            .collect()
    });
    let elapsed = started.elapsed();
    let (items, errors, bytes_in, bytes_out) = totals.iter().fold((0, 0, 0, 0), |a, t| {
        (a.0 + t.0, a.1 + t.1, a.2 + t.2, a.3 + t.3)
    });
    RunResult {
        corpus: corpus.to_string(),
        media,
        approach,
        workers,
        items,
        errors,
        elapsed,
        bytes_in,
        bytes_out,
    }
}

/// Returns (items, errors, bytes_in, bytes_out).
fn worker(
    tools: &Tools,
    approach: Approach,
    media: Media,
    files: &[PathBuf],
    out: &Path,
    base_index: usize,
) -> (usize, usize, u64, u64) {
    let mut items = 0;
    let mut errors = 0;
    let mut bytes_in = 0u64;
    let mut bytes_out = 0u64;
    let mut et = match approach {
        Approach::ExifToolStayOpen => {
            Some(ExifTool::spawn(tools.exiftool.as_ref().unwrap()).expect("spawn exiftool"))
        }
        _ => None,
    };
    for (i, src) in files.iter().enumerate() {
        let cap = capture_for(base_index + i);
        let dst = out.join(src.file_name().unwrap());
        let ok = match (approach, media) {
            (Approach::ExifToolStayOpen, Media::Jpeg) => {
                reply_ok(et.as_mut().unwrap().write_jpeg(src, &dst, &cap))
            }
            (Approach::ExifToolStayOpen, Media::Mp4) => {
                reply_ok(et.as_mut().unwrap().write_mp4(src, &dst, &cap))
            }
            (Approach::ExifToolPerProcess, _) => {
                let ex = tools.exiftool.as_ref().unwrap();
                let mut args = vec![
                    "-charset".to_string(),
                    "filename=UTF8".to_string(),
                    "-o".to_string(),
                    dst.display().to_string(),
                ];
                args.extend(match media {
                    Media::Jpeg => exiftool::jpeg_args(&cap),
                    Media::Mp4 => exiftool::mp4_args(&cap),
                });
                args.push(src.display().to_string());
                Command::new(ex)
                    .args(&args)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
            }
            (Approach::Native, Media::Jpeg) => native_jpeg::write_jpeg(src, &dst, &cap)
                .map_err(|e| eprintln!("native jpeg {}: {e}", src.display()))
                .is_ok(),
            (Approach::Native, Media::Mp4) => native_mp4::write_mp4(src, &dst, &cap)
                .map_err(|e| eprintln!("native mp4 {}: {e}", src.display()))
                .is_ok(),
            (Approach::FfmpegRemux, Media::Mp4) => {
                ffmpeg_remux::write_mp4(tools.ffmpeg.as_ref().unwrap(), src, &dst, &cap)
                    .map_err(|e| eprintln!("ffmpeg {}: {e}", src.display()))
                    .is_ok()
            }
            (Approach::FfmpegRemux, Media::Jpeg) => {
                unreachable!("ffmpeg is not a JPEG metadata writer")
            }
        };
        items += 1;
        if ok {
            bytes_in += fs::metadata(src).map(|m| m.len()).unwrap_or(0);
            bytes_out += fs::metadata(&dst).map(|m| m.len()).unwrap_or(0);
        } else {
            errors += 1;
        }
    }
    if let Some(et) = et {
        let _ = et.close();
    }
    (items, errors, bytes_in, bytes_out)
}

fn reply_ok(r: std::io::Result<exiftool::Reply>) -> bool {
    match r {
        Ok(reply) => match reply.error() {
            Some(e) => {
                eprintln!("exiftool: {e}");
                false
            }
            None => true,
        },
        Err(e) => {
            eprintln!("exiftool pipe: {e}");
            false
        }
    }
}

/// Field-by-field read-back with `exiftool -a -G1 -s -j -n` and `ffprobe -show_format`.
fn verify(tools: &Tools, media: Media, file: &Path, cap: &Capture) -> Vec<Value> {
    let mut checks = Vec::new();
    let mut check = |field: &str, got: Value, want: Value| {
        let ok = got == want;
        checks.push(json!({ "field": field, "want": want, "got": got, "ok": ok }));
    };
    if !file.is_file() {
        check("output exists", json!(false), json!(true));
        return checks;
    }
    if let Some(ex) = &tools.exiftool {
        let mut et = ExifTool::spawn(ex).expect("spawn exiftool");
        let j = et.read_json(file, &[]).unwrap_or(Value::Null);
        let s = |k: &str| j.get(k).cloned().unwrap_or(Value::Null);
        match media {
            Media::Jpeg => {
                check(
                    "ExifIFD:DateTimeOriginal",
                    s("ExifIFD:DateTimeOriginal"),
                    json!(cap.exif_local()),
                );
                check(
                    "ExifIFD:CreateDate",
                    s("ExifIFD:CreateDate"),
                    json!(cap.exif_local()),
                );
                check(
                    "ExifIFD:OffsetTimeOriginal",
                    s("ExifIFD:OffsetTimeOriginal"),
                    json!(cap.exif_offset()),
                );
                check(
                    "ExifIFD:OffsetTime",
                    s("ExifIFD:OffsetTime"),
                    json!(cap.exif_offset()),
                );
                let (lat, lat_ref, lon, lon_ref) = cap.gps_refs().unwrap();
                check(
                    "GPS:GPSLatitudeRef",
                    s("GPS:GPSLatitudeRef"),
                    json!(lat_ref),
                );
                check(
                    "GPS:GPSLongitudeRef",
                    s("GPS:GPSLongitudeRef"),
                    json!(lon_ref),
                );
                check(
                    "GPS:GPSLatitude (4 dp)",
                    round4(&s("GPS:GPSLatitude")),
                    json!(format!("{lat:.4}")),
                );
                check(
                    "GPS:GPSLongitude (4 dp)",
                    round4(&s("GPS:GPSLongitude")),
                    json!(format!("{lon:.4}")),
                );
                check(
                    "GPS:GPSDateStamp",
                    s("GPS:GPSDateStamp"),
                    json!(cap.gps_date()),
                );
                check(
                    "GPS:GPSTimeStamp",
                    s("GPS:GPSTimeStamp"),
                    json!(cap.gps_time()),
                );
            }
            Media::Mp4 => {
                check(
                    "QuickTime:CreateDate",
                    s("QuickTime:CreateDate"),
                    json!(cap.qt_utc()),
                );
                check(
                    "QuickTime:ModifyDate",
                    s("QuickTime:ModifyDate"),
                    json!(cap.qt_utc()),
                );
                check(
                    "Track1:TrackCreateDate",
                    s("Track1:TrackCreateDate"),
                    json!(cap.qt_utc()),
                );
                check(
                    "Track1:MediaCreateDate",
                    s("Track1:MediaCreateDate"),
                    json!(cap.qt_utc()),
                );
                check(
                    "Keys:CreationDate",
                    s("Keys:CreationDate"),
                    json!(cap.exiftool_local_with_offset()),
                );
                let (lat, lon) = cap.gps.unwrap();
                check(
                    "Keys:GPSCoordinates",
                    s("Keys:GPSCoordinates"),
                    json!(format!("{lat:.4} {lon:.4}")),
                );
                check(
                    "UserData:GPSCoordinates (©xyz)",
                    s("UserData:GPSCoordinates"),
                    json!(format!("{lat:.4} {lon:.4}")),
                );
            }
        }
    }
    if media == Media::Mp4
        && let Some(fp) = &tools.ffprobe
    {
        let j = ffprobe_json_or_null(fp, file);
        let tags = j.pointer("/format/tags").cloned().unwrap_or(Value::Null);
        let t = |k: &str| tags.get(k).cloned().unwrap_or(Value::Null);
        check(
            "ffprobe format.tags.creation_time",
            t("creation_time"),
            json!(cap.ffmpeg_creation_time()),
        );
        check(
            "ffprobe com.apple.quicktime.creationdate",
            t("com.apple.quicktime.creationdate"),
            json!(cap.iso8601_local()),
        );
        check(
            "ffprobe com.apple.quicktime.location.ISO6709",
            t("com.apple.quicktime.location.ISO6709"),
            json!(cap.iso6709().unwrap()),
        );
        check(
            "ffprobe location (©xyz)",
            t("location"),
            json!(cap.iso6709().unwrap()),
        );
        let stream_times: Vec<Value> = j
            .pointer("/streams")
            .and_then(|s| s.as_array())
            .map(|a| {
                a.iter()
                    .map(|s| {
                        s.pointer("/tags/creation_time")
                            .cloned()
                            .unwrap_or(Value::Null)
                    })
                    .collect()
            })
            .unwrap_or_default();
        check(
            "ffprobe stream creation_time (all streams)",
            json!(
                stream_times
                    .iter()
                    .all(|v| *v == json!(cap.ffmpeg_creation_time()))
            ),
            json!(true),
        );
        if let Ok(sum) = native_mp4::summarize(file) {
            check(
                "top-level atom order kept moov first if it was",
                json!(sum.top_level.join(" ")),
                json!(sum.top_level.join(" ")),
            );
        }
    }
    checks
}

fn ffprobe_json_or_null(fp: &Path, file: &Path) -> Value {
    ffmpeg_remux::ffprobe_json(fp, file).unwrap_or(Value::Null)
}

fn round4(v: &Value) -> Value {
    match v.as_f64() {
        Some(f) => json!(format!("{f:.4}")),
        None => v.clone(),
    }
}

fn run_quiet(exe: &Path, args: &[&str]) {
    let _ = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn version_line(exe: &Path, name: &str) -> String {
    let arg = if name == "exiftool" {
        "-ver"
    } else {
        "-version"
    };
    Command::new(exe)
        .arg(arg)
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .unwrap_or_default()
}

fn time_n(n: usize, mut f: impl FnMut()) -> (Duration, Duration, Duration) {
    let mut v: Vec<Duration> = (0..n)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    v.sort();
    (v[0], v[n / 2], v[n - 1])
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}
