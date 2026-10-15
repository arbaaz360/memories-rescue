//! Metadata through an ffmpeg stream-copy remux. Measured for completeness:
//! it writes `mvhd`/`tkhd`/`mdhd` and the `mdta` Keys, but it cannot write
//! `©xyz` (it emits a 3GPP `loci` atom instead), it copies every format tag
//! into Keys, and it moves `moov` to the end unless a second `faststart` pass
//! is paid for. See docs/decisions/Q-02-metadata-writer.md.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::fields::Capture;

pub fn write_mp4(ffmpeg: &Path, src: &Path, dst: &Path, cap: &Capture) -> io::Result<()> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-map_metadata",
            "-1",
            "-bitexact",
        ])
        .arg("-metadata")
        .arg(format!("creation_time={}", cap.ffmpeg_creation_time()))
        .arg("-metadata")
        .arg(format!(
            "com.apple.quicktime.creationdate={}",
            cap.iso8601_local()
        ));
    if let Some(iso) = cap.iso6709() {
        cmd.arg("-metadata")
            .arg(format!("com.apple.quicktime.location.ISO6709={iso}"))
            .arg("-metadata")
            .arg(format!("location={iso}"));
    }
    cmd.args(["-movflags", "use_metadata_tags+faststart"])
        .arg(dst);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }
}

/// `third_party/ffmpeg/{ffmpeg,ffprobe}.exe` in this checkout, else `PATH`.
pub fn locate(repo_root: &Path, name: &str) -> Option<PathBuf> {
    let bundled = repo_root.join("third_party/ffmpeg").join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    if bundled.is_file() {
        return Some(bundled);
    }
    crate::exiftool::which(name)
}

/// `ffprobe -show_format -show_streams -of json` parsed.
pub fn ffprobe_json(ffprobe: &Path, file: &Path) -> io::Result<serde_json::Value> {
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(file)
        .stdin(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
