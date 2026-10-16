//! The ffmpeg sidecar: its encoder listing, the H.264 encoder choice of SRS
//! §4.4, and running one command with a timeout.
//!
//! Arguments are always an array handed to the process; nothing is ever
//! joined into a shell string.

use std::collections::{BTreeSet, HashMap};
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::Error;

/// Keep this much of ffmpeg's stderr for error reports.
const STDERR_TAIL: usize = 16 * 1024;

/// The operating system the encoder order is chosen for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Windows,
    MacOs,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Other
        }
    }
}

/// An H.264 encoder configuration the compositor can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "encoder")]
pub enum VideoEncoder {
    /// `h264_mf`, Windows Media Foundation. `hardware: false` is Microsoft's
    /// software H.264 encoder MFT, present on every standard Windows 10/11
    /// edition (and on N/KN editions once the Media Feature Pack is in);
    /// `hardware: true` asks for a GPU MFT (`-hw_encoding 1`).
    MediaFoundation { hardware: bool },
    /// `h264_videotoolbox` with `-allow_sw 1`, macOS.
    VideoToolbox,
    /// `libopenh264`, the cross-platform quality fallback (Q-04).
    OpenH264,
}

impl VideoEncoder {
    /// The ffmpeg encoder name.
    pub fn name(&self) -> &'static str {
        match self {
            VideoEncoder::MediaFoundation { .. } => "h264_mf",
            VideoEncoder::VideoToolbox => "h264_videotoolbox",
            VideoEncoder::OpenH264 => "libopenh264",
        }
    }

    /// The pixel format the last filter converts to. Media Foundation GPU
    /// MFTs refuse anything but NV12 (format negotiation fails on yuv420p);
    /// the software MFT and VideoToolbox take NV12 natively; OpenH264 takes
    /// yuv420p only.
    pub fn pix_fmt(&self) -> &'static str {
        match self {
            VideoEncoder::MediaFoundation { .. } | VideoEncoder::VideoToolbox => "nv12",
            VideoEncoder::OpenH264 => "yuv420p",
        }
    }

    /// Runs on the CPU without vendor hardware.
    pub fn is_software(&self) -> bool {
        matches!(
            self,
            VideoEncoder::MediaFoundation { hardware: false } | VideoEncoder::OpenH264
        )
    }

    /// Encoder arguments for an average bitrate in kbit/s. High profile
    /// everywhere: Media Foundation defaults to Constrained Baseline and only
    /// takes the profile as a number (`100`), OpenH264 defaults to its own
    /// quality mode and needs `-rc_mode bitrate` to honour `-b:v`.
    pub fn args(&self, bitrate_kbps: u32) -> Vec<String> {
        let rate = format!("{bitrate_kbps}k");
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let mut a = match self {
            VideoEncoder::MediaFoundation { hardware } => s(&[
                "-c:v",
                "h264_mf",
                "-hw_encoding",
                if *hardware { "1" } else { "0" },
                "-profile:v",
                "100",
            ]),
            VideoEncoder::VideoToolbox => s(&[
                "-c:v",
                "h264_videotoolbox",
                "-allow_sw",
                "1",
                "-profile:v",
                "high",
            ]),
            VideoEncoder::OpenH264 => s(&[
                "-c:v",
                "libopenh264",
                "-profile:v",
                "high",
                "-rc_mode",
                "bitrate",
                "-allow_skip_frames",
                "0",
            ]),
        };
        a.extend(s(&["-b:v", &rate]));
        a
    }
}

/// What `ffmpeg -encoders` lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderSet {
    pub video: BTreeSet<String>,
    pub audio: BTreeSet<String>,
}

impl EncoderSet {
    /// Parse the listing. Lines after the `------` separator look like
    /// ` V....D h264_mf              H264 via MediaFoundation (codec h264)`.
    pub fn parse(listing: &str) -> Self {
        let mut set = EncoderSet::default();
        let mut started = !listing.contains("------");
        for line in listing.lines() {
            if !started {
                started = line.trim_start().starts_with("------");
                continue;
            }
            let mut parts = line.split_whitespace();
            let (Some(flags), Some(name)) = (parts.next(), parts.next()) else {
                continue;
            };
            if flags.len() != 6 || name == "=" {
                continue;
            }
            match flags.as_bytes()[0] {
                b'V' => {
                    set.video.insert(name.to_string());
                }
                b'A' => {
                    set.audio.insert(name.to_string());
                }
                _ => {}
            }
        }
        set
    }

    pub fn has_video(&self, name: &str) -> bool {
        self.video.contains(name)
    }

    pub fn has_audio(&self, name: &str) -> bool {
        self.audio.contains(name)
    }
}

/// The §4.4 order for a platform, filtered by what the build lists:
/// Media Foundation on Windows (hardware first when `prefer_hardware`, the
/// software MFT always), VideoToolbox on macOS, then OpenH264.
pub fn encoder_candidates(
    platform: Platform,
    listed: &EncoderSet,
    prefer_hardware: bool,
) -> Vec<VideoEncoder> {
    let mut out = Vec::new();
    match platform {
        Platform::Windows if listed.has_video("h264_mf") => {
            if prefer_hardware {
                out.push(VideoEncoder::MediaFoundation { hardware: true });
            }
            out.push(VideoEncoder::MediaFoundation { hardware: false });
        }
        Platform::MacOs if listed.has_video("h264_videotoolbox") => {
            out.push(VideoEncoder::VideoToolbox)
        }
        _ => {}
    }
    if listed.has_video("libopenh264") {
        out.push(VideoEncoder::OpenH264);
    }
    out
}

/// The first candidate, or `ENCODER_UNAVAILABLE`.
pub fn select_encoder(
    platform: Platform,
    listed: &EncoderSet,
    prefer_hardware: bool,
) -> Result<VideoEncoder, Error> {
    encoder_candidates(platform, listed, prefer_hardware)
        .into_iter()
        .next()
        .ok_or_else(|| {
            Error::EncoderUnavailable(format!(
                "ffmpeg lists none of h264_mf, h264_videotoolbox, libopenh264 for {platform:?}"
            ))
        })
}

/// The encoders a job uses: the primary, and the software path for the one
/// automatic retry (§8 `FFMPEG_FAIL`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderPlan {
    pub primary: VideoEncoder,
    /// OpenH264 when it works and is not the primary, else the software MFT
    /// when the primary is a GPU MFT, else the primary itself (the retry
    /// then differs only in its safe settings: audio re-encoded).
    pub software: VideoEncoder,
}

impl EncoderPlan {
    /// Build the plan from the candidates that passed their smoke test, in order.
    pub fn from_working(working: &[VideoEncoder]) -> Option<Self> {
        let primary = *working.first()?;
        let software = working
            .iter()
            .copied()
            .find(|e| *e == VideoEncoder::OpenH264 && *e != primary)
            .or_else(|| {
                working
                    .iter()
                    .copied()
                    .find(|e| e.is_software() && *e != primary)
            })
            .unwrap_or(primary);
        Some(Self { primary, software })
    }
}

/// Result of one ffmpeg run that exited 0.
#[derive(Debug, Clone)]
pub struct RunOutput {
    pub elapsed: Duration,
    /// The tail of stderr (warnings at `-loglevel warning` and above).
    pub stderr: String,
}

/// One ffmpeg executable, with its encoder listing and smoke-test results
/// cached for the life of the value.
#[derive(Debug)]
pub struct Ffmpeg {
    path: PathBuf,
    listing: OnceLock<Result<EncoderSet, String>>,
    smoke: Mutex<HashMap<VideoEncoder, bool>>,
}

impl Ffmpeg {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            listing: OnceLock::new(),
            smoke: Mutex::new(HashMap::new()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `ffmpeg -hide_banner -encoders`, run once and cached.
    pub fn encoders(&self) -> Result<&EncoderSet, Error> {
        let listing = self.listing.get_or_init(|| {
            let mut cmd = Command::new(&self.path);
            cmd.args(["-hide_banner", "-nostdin", "-encoders"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            no_window(&mut cmd);
            match cmd.output() {
                Ok(o) if o.status.success() => {
                    Ok(EncoderSet::parse(&String::from_utf8_lossy(&o.stdout)))
                }
                Ok(o) => Err(format!("ffmpeg -encoders exited with {}", o.status)),
                Err(e) => Err(format!("cannot run {}: {e}", self.path.display())),
            }
        });
        listing
            .as_ref()
            .map_err(|e| Error::FfmpegMissing(e.clone()))
    }

    /// Encode a quarter second of a 256x256 picture with `enc`; cached per
    /// encoder. Listing an encoder proves nothing: `h264_mf` is compiled into
    /// every Windows build but fails to open on an N edition without the
    /// Media Feature Pack, and a GPU MFT can be listed without a GPU.
    ///
    /// The input is a PNG decoded by the image2 demuxer rather than a lavfi
    /// source, so the test also runs on the minimal §10.4 build, which has
    /// no lavfi.
    pub fn encoder_works(&self, enc: VideoEncoder) -> bool {
        if let Some(ok) = self.smoke.lock().expect("smoke cache").get(&enc) {
            return *ok;
        }
        let ok = self.smoke_test(enc).is_ok();
        self.smoke.lock().expect("smoke cache").insert(enc, ok);
        ok
    }

    fn smoke_test(&self, enc: VideoEncoder) -> Result<(), Error> {
        let dir = crate::temp_dir("smoke")?;
        let png = dir.path().join("smoke.png");
        image::RgbImage::from_fn(64, 64, |x, y| {
            image::Rgb([(x * 4) as u8, (y * 4) as u8, 128])
        })
        .save(&png)
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
        let out = dir.path().join("smoke.mp4");
        let mut args: Vec<OsString> = [
            "-hide_banner",
            "-nostdin",
            "-loglevel",
            "error",
            "-y",
            "-loop",
            "1",
            "-framerate",
            "30",
            "-t",
            "0.25",
            "-i",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        args.push(png.into_os_string());
        args.push("-vf".into());
        args.push(format!("scale=256:256,format={}", enc.pix_fmt()).into());
        args.extend(enc.args(500).into_iter().map(OsString::from));
        args.push(out.clone().into_os_string());
        self.run(&args, Duration::from_secs(30))?;
        if std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0) == 0 {
            return Err(Error::FfmpegFail {
                message: format!("{} wrote nothing", enc.name()),
                timed_out: false,
                stderr: String::new(),
            });
        }
        Ok(())
    }

    /// The encoders this machine can use, in §4.4 order, smoke-tested.
    pub fn encoder_plan(&self, prefer_hardware: bool) -> Result<EncoderPlan, Error> {
        let listed = self.encoders()?;
        let candidates = encoder_candidates(Platform::current(), listed, prefer_hardware);
        if candidates.is_empty() {
            return select_encoder(Platform::current(), listed, prefer_hardware).map(|e| {
                EncoderPlan {
                    primary: e,
                    software: e,
                }
            });
        }
        let working: Vec<VideoEncoder> = candidates
            .iter()
            .copied()
            .filter(|e| self.encoder_works(*e))
            .collect();
        EncoderPlan::from_working(&working).ok_or_else(|| {
            let tried: Vec<&str> = candidates.iter().map(|e| e.name()).collect();
            Error::EncoderUnavailable(format!(
                "listed but none could encode: {} (Windows N/KN editions need the Media Feature Pack)",
                tried.join(", ")
            ))
        })
    }

    /// Run ffmpeg with `args`, killing it after `timeout`. Non-zero exit or
    /// the timeout is `FFMPEG_FAIL` with the tail of stderr.
    pub fn run(&self, args: &[OsString], timeout: Duration) -> Result<RunOutput, Error> {
        let started = Instant::now();
        let mut cmd = Command::new(&self.path);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        no_window(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::FfmpegMissing(format!("{}: {e}", self.path.display())))?;
        let mut stderr = child.stderr.take().expect("stderr piped");
        let reader = std::thread::spawn(move || {
            let mut tail: Vec<u8> = Vec::new();
            let mut buf = [0u8; 8192];
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        tail.extend_from_slice(&buf[..n]);
                        if tail.len() > 2 * STDERR_TAIL {
                            tail.drain(..tail.len() - STDERR_TAIL);
                        }
                    }
                }
            }
            let start = tail.len().saturating_sub(STDERR_TAIL);
            String::from_utf8_lossy(&tail[start..]).into_owned()
        });
        let deadline = started + timeout;
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                let _ = child.kill();
                break child.wait()?;
            }
            std::thread::sleep(Duration::from_millis(15));
        };
        let stderr = reader.join().unwrap_or_default();
        let elapsed = started.elapsed();
        if timed_out {
            return Err(Error::FfmpegFail {
                message: format!("timed out after {:.0} s", timeout.as_secs_f64()),
                timed_out: true,
                stderr,
            });
        }
        if !status.success() {
            let last = stderr.lines().rev().find(|l| !l.trim().is_empty());
            return Err(Error::FfmpegFail {
                message: format!("exit {status}: {}", last.unwrap_or("no message")),
                timed_out: false,
                stderr,
            });
        }
        Ok(RunOutput { elapsed, stderr })
    }
}

/// Do not flash a console window for the child on Windows.
pub(crate) fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed `-encoders` listing of the pinned BtbN LGPL Windows build.
    const WINDOWS_LGPL: &str = "Encoders:
 V..... = Video
 A..... = Audio
 S..... = Subtitle
 .F.... = Frame-level multithreading
 ..S... = Slice-level multithreading
 ...X.. = Codec is experimental
 ....B. = Supports draw_horiz_band
 .....D = Supports direct rendering method 1
 ------
 V....D libopenh264          OpenH264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)
 V....D h264_amf             AMD AMF H.264 Encoder (codec h264)
 V....D h264_mf              H264 via MediaFoundation (codec h264)
 V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)
 V..... hevc_mf              HEVC via MediaFoundation (codec hevc)
 V....D png                  PNG (Portable Network Graphics) image
 A....D aac                  AAC (Advanced Audio Coding)
 A....D aac_mf               AAC via MediaFoundation (codec aac)
";

    const MAC_MINIMAL: &str = " ------
 V....D h264_videotoolbox    VideoToolbox H.264 Encoder (codec h264)
 V....D hevc_videotoolbox    VideoToolbox H.265 Encoder (codec hevc)
 A....D aac                  AAC (Advanced Audio Coding)
";

    const NO_H264: &str = " ------
 V....D png                  PNG (Portable Network Graphics) image
 V....D mjpeg                MJPEG (Motion JPEG)
 A....D aac                  AAC (Advanced Audio Coding)
";

    #[test]
    fn parses_listing() {
        let s = EncoderSet::parse(WINDOWS_LGPL);
        assert!(s.has_video("h264_mf"));
        assert!(s.has_video("libopenh264"));
        assert!(s.has_audio("aac"));
        assert!(!s.has_video("aac"));
        assert!(!s.has_video("="), "legend lines are not encoders");
        assert_eq!(s.video.len(), 6);
    }

    #[test]
    fn windows_order_is_mf_then_openh264() {
        let s = EncoderSet::parse(WINDOWS_LGPL);
        assert_eq!(
            encoder_candidates(Platform::Windows, &s, false),
            vec![
                VideoEncoder::MediaFoundation { hardware: false },
                VideoEncoder::OpenH264
            ]
        );
        assert_eq!(
            encoder_candidates(Platform::Windows, &s, true),
            vec![
                VideoEncoder::MediaFoundation { hardware: true },
                VideoEncoder::MediaFoundation { hardware: false },
                VideoEncoder::OpenH264
            ]
        );
        assert_eq!(
            select_encoder(Platform::Windows, &s, false).unwrap(),
            VideoEncoder::MediaFoundation { hardware: false }
        );
    }

    #[test]
    fn mac_order_is_videotoolbox() {
        let s = EncoderSet::parse(MAC_MINIMAL);
        assert_eq!(
            encoder_candidates(Platform::MacOs, &s, false),
            vec![VideoEncoder::VideoToolbox]
        );
        // The Windows build's listing on a Mac: only OpenH264 is usable.
        let w = EncoderSet::parse(WINDOWS_LGPL);
        assert_eq!(
            select_encoder(Platform::MacOs, &w, false).unwrap(),
            VideoEncoder::OpenH264
        );
    }

    #[test]
    fn no_h264_encoder_is_encoder_unavailable() {
        let s = EncoderSet::parse(NO_H264);
        let err = select_encoder(Platform::Windows, &s, false).unwrap_err();
        assert_eq!(err.code(), "ENCODER_UNAVAILABLE");
        let err = select_encoder(Platform::MacOs, &s, false).unwrap_err();
        assert_eq!(err.code(), "ENCODER_UNAVAILABLE");
    }

    #[test]
    fn software_path_prefers_openh264() {
        let mf_sw = VideoEncoder::MediaFoundation { hardware: false };
        let mf_hw = VideoEncoder::MediaFoundation { hardware: true };
        let p = EncoderPlan::from_working(&[mf_sw, VideoEncoder::OpenH264]).unwrap();
        assert_eq!((p.primary, p.software), (mf_sw, VideoEncoder::OpenH264));
        let p = EncoderPlan::from_working(&[mf_hw, mf_sw]).unwrap();
        assert_eq!((p.primary, p.software), (mf_hw, mf_sw));
        let p = EncoderPlan::from_working(&[mf_sw]).unwrap();
        assert_eq!((p.primary, p.software), (mf_sw, mf_sw));
        let p = EncoderPlan::from_working(&[VideoEncoder::OpenH264]).unwrap();
        assert_eq!(p.software, VideoEncoder::OpenH264);
        assert!(EncoderPlan::from_working(&[]).is_none());
    }

    #[test]
    fn encoder_args_are_arrays() {
        let a = VideoEncoder::MediaFoundation { hardware: false }.args(4200);
        assert_eq!(
            a,
            [
                "-c:v",
                "h264_mf",
                "-hw_encoding",
                "0",
                "-profile:v",
                "100",
                "-b:v",
                "4200k"
            ]
        );
        let o = VideoEncoder::OpenH264.args(800);
        assert!(o.windows(2).any(|w| w == ["-rc_mode", "bitrate"]));
        assert_eq!(o.last().unwrap(), "800k");
        let v = VideoEncoder::VideoToolbox.args(800);
        assert!(v.windows(2).any(|w| w == ["-allow_sw", "1"]));
    }
}
