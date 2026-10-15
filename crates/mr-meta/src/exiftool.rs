//! exiftool as a long-lived sidecar (SRS C-05): one process per worker,
//! started with `-stay_open True -@ -`, fed one command block per file on
//! stdin, each block ended by `-execute`. The process answers `{ready}` on
//! stdout when a block is done; `-echo4` stamps a marker on stderr after the
//! block so warnings and errors can be attributed to the right file.

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

use crate::fields::Capture;

/// What one `-execute` block produced.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub stdout: String,
    pub stderr: String,
}

impl Reply {
    /// exiftool reports problems on stderr; "Warning:" lines are advisory,
    /// "Error:" lines mean nothing was written.
    pub fn error(&self) -> Option<&str> {
        self.stderr.lines().find(|l| l.starts_with("Error:"))
    }
}

pub struct ExifTool {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    stderr_rx: Receiver<String>,
    seq: u64,
}

impl ExifTool {
    /// Start `exiftool -stay_open True -@ -`.
    pub fn spawn(exe: &Path) -> io::Result<Self> {
        let mut cmd = Command::new(exe);
        cmd.args(["-stay_open", "True", "-@", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = BufReader::new(child.stdout.take().expect("piped stdout"));
        let stderr = BufReader::new(child.stderr.take().expect("piped stderr"));

        // stderr is split into per-command chunks on the `{done<seq>}` marker.
        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
            let mut chunk = String::new();
            for line in stderr.lines() {
                let Ok(line) = line else { break };
                if line.starts_with("{done") {
                    if tx.send(std::mem::take(&mut chunk)).is_err() {
                        break;
                    }
                } else {
                    chunk.push_str(&line);
                    chunk.push('\n');
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            stdout,
            stderr_rx: rx,
            seq: 0,
        })
    }

    /// Run one command block. `args` are exiftool arguments, one per entry,
    /// exactly as they would appear on a command line (values are not quoted).
    pub fn execute<I, S>(&mut self, args: I) -> io::Result<Reply>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.seq += 1;
        let seq = self.seq;
        let mut block = String::new();
        // Paths and values are UTF-8 on the pipe; without this exiftool reads
        // filenames in the system code page on Windows and breaks on "Café".
        block.push_str("-charset\nfilename=UTF8\n");
        for a in args {
            let a = a.as_ref();
            debug_assert!(
                !a.contains('\n'),
                "argfile arguments cannot contain newlines"
            );
            block.push_str(a);
            block.push('\n');
        }
        block.push_str(&format!("-echo4\n{{done{seq}}}\n-execute{seq}\n"));
        self.stdin.write_all(block.as_bytes())?;
        self.stdin.flush()?;

        let ready = format!("{{ready{seq}}}");
        let mut out = String::new();
        let mut line = String::new();
        loop {
            line.clear();
            if self.stdout.read_line(&mut line)? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "exiftool exited",
                ));
            }
            if line.trim_end() == ready {
                break;
            }
            out.push_str(&line);
        }
        let err = self
            .stderr_rx
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "exiftool stderr closed"))?;
        Ok(Reply {
            stdout: out,
            stderr: err,
        })
    }

    /// `exiftool -ver` through the pipe.
    pub fn version(&mut self) -> io::Result<String> {
        Ok(self.execute(["-ver"])?.stdout.trim().to_string())
    }

    /// Write the §4.3 JPEG field set, copying `src` to `dst` in one pass (`-o`).
    pub fn write_jpeg(&mut self, src: &Path, dst: &Path, cap: &Capture) -> io::Result<Reply> {
        let mut args = vec!["-o".to_string(), path_arg(dst)];
        args.extend(jpeg_args(cap));
        args.push(path_arg(src));
        self.execute(&args)
    }

    /// Write the §4.3 MP4/MOV field set, copying `src` to `dst` in one pass (`-o`).
    pub fn write_mp4(&mut self, src: &Path, dst: &Path, cap: &Capture) -> io::Result<Reply> {
        let mut args = vec!["-o".to_string(), path_arg(dst)];
        args.extend(mp4_args(cap));
        args.push(path_arg(src));
        self.execute(&args)
    }

    /// `exiftool -a -G1 -s -j -n <file>` parsed into JSON.
    pub fn read_json(&mut self, file: &Path, extra: &[&str]) -> io::Result<serde_json::Value> {
        let mut args: Vec<String> = ["-a", "-G1", "-s", "-j", "-n"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        args.extend(extra.iter().map(|s| s.to_string()));
        args.push(path_arg(file));
        let reply = self.execute(&args)?;
        let v: serde_json::Value = serde_json::from_str(&reply.stdout).map_err(|e| {
            io::Error::new(io::ErrorKind::InvalidData, format!("{e}: {}", reply.stdout))
        })?;
        Ok(v.as_array()
            .and_then(|a| a.first().cloned())
            .unwrap_or(serde_json::Value::Null))
    }

    /// Ask the process to exit and wait for it.
    pub fn close(mut self) -> io::Result<()> {
        let _ = self.stdin.write_all(b"-stay_open\nFalse\n");
        let _ = self.stdin.flush();
        self.child.wait().map(|_| ())
    }
}

impl Drop for ExifTool {
    fn drop(&mut self) {
        let _ = self.stdin.write_all(b"-stay_open\nFalse\n");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

fn path_arg(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

/// The exact argfile lines for a JPEG (SRS §4.3 row 1, plus the offsets that
/// pair with `CreateDate` and `ModifyDate`).
pub fn jpeg_args(cap: &Capture) -> Vec<String> {
    let local = cap.exif_local();
    let off = cap.exif_offset();
    let mut v = vec![
        format!("-EXIF:DateTimeOriginal={local}"),
        format!("-EXIF:CreateDate={local}"),
        format!("-EXIF:ModifyDate={local}"),
        format!("-EXIF:OffsetTimeOriginal={off}"),
        format!("-EXIF:OffsetTimeDigitized={off}"),
        format!("-EXIF:OffsetTime={off}"),
    ];
    if let Some((lat, lat_ref, lon, lon_ref)) = cap.gps_refs() {
        v.extend([
            "-GPS:GPSVersionID=2.3.0.0".to_string(),
            format!("-GPS:GPSLatitude={lat}"),
            format!("-GPS:GPSLatitudeRef={lat_ref}"),
            format!("-GPS:GPSLongitude={lon}"),
            format!("-GPS:GPSLongitudeRef={lon_ref}"),
            format!("-GPS:GPSDateStamp={}", cap.gps_date()),
            format!("-GPS:GPSTimeStamp={}", cap.gps_time()),
        ]);
    }
    v
}

/// The exact argfile lines for an MP4/MOV (SRS §4.3 row 3). Times are given
/// as UTC and written raw (no `-api QuickTimeUTC`), which is what the
/// container stores. `Keys:GPSCoordinates` becomes
/// `com.apple.quicktime.location.ISO6709` = `+51.5074-000.1278/`;
/// `UserData:GPSCoordinates` is the `©xyz` atom with the same string.
pub fn mp4_args(cap: &Capture) -> Vec<String> {
    let utc = cap.qt_utc();
    let mut v = vec![
        format!("-QuickTime:CreateDate={utc}"),
        format!("-QuickTime:ModifyDate={utc}"),
        format!("-QuickTime:TrackCreateDate={utc}"),
        format!("-QuickTime:TrackModifyDate={utc}"),
        format!("-QuickTime:MediaCreateDate={utc}"),
        format!("-QuickTime:MediaModifyDate={utc}"),
        format!("-Keys:CreationDate={}", cap.exiftool_local_with_offset()),
    ];
    if let Some((lat, lon)) = cap.gps {
        // exiftool keeps whatever precision it is given: a float such as
        // -0.12789999999999999 would land verbatim in the ISO 6709 string.
        // Four decimals (11 m) is what Apple devices and exiftool's own
        // formatter use, and what `Capture::iso6709` writes natively.
        v.push(format!("-Keys:GPSCoordinates={lat:.4} {lon:.4}"));
        v.push(format!("-UserData:GPSCoordinates={lat:.4} {lon:.4}"));
    }
    v
}

/// Where the standalone binary lives for this checkout: `third_party/exiftool/exiftool.exe`
/// (Windows) or `exiftool` on `PATH` elsewhere. `None` when it is not there,
/// which is the case on CI runners; callers skip.
pub fn locate(repo_root: &Path) -> Option<PathBuf> {
    let bundled = repo_root
        .join("third_party/exiftool")
        .join(if cfg!(windows) {
            "exiftool.exe"
        } else {
            "exiftool"
        });
    if bundled.is_file() {
        return Some(bundled);
    }
    which("exiftool", "-ver")
}

/// First `name` on `PATH` that actually runs `name <probe_arg>` successfully.
/// A bare file-name match is not enough: the GitHub Windows runner carries an
/// `exiftool.exe` that is not a Win32 executable (error 193), so a candidate
/// is executed once before it is trusted.
pub(crate) fn which(name: &str, probe_arg: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|d| d.join(&exe))
        .filter(|p| p.is_file())
        .find(|p| {
            Command::new(p)
                .arg(probe_arg)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp4_args_round_coordinates_to_four_decimals() {
        let cap = Capture::from_unix(
            1_623_767_400,
            3600,
            Some((51.5074, -0.127_899_999_999_999_99)),
        );
        let args = mp4_args(&cap);
        assert!(
            args.contains(&"-Keys:GPSCoordinates=51.5074 -0.1279".to_string()),
            "{args:?}"
        );
        assert!(args.contains(&"-Keys:CreationDate=2021:06:15 15:30:00+01:00".to_string()));
        assert!(args.contains(&"-QuickTime:MediaCreateDate=2021:06:15 14:30:00".to_string()));
    }

    #[test]
    fn jpeg_args_cover_the_srs_field_set() {
        let args = jpeg_args(&Capture::from_unix(
            1_623_767_400,
            3600,
            Some((51.5074, -0.1278)),
        ));
        for needle in [
            "-EXIF:DateTimeOriginal=2021:06:15 15:30:00",
            "-EXIF:OffsetTimeOriginal=+01:00",
            "-GPS:GPSLongitudeRef=W",
            "-GPS:GPSTimeStamp=14:30:00",
            "-GPS:GPSDateStamp=2021:06:15",
        ] {
            assert!(
                args.iter().any(|a| a == needle),
                "missing {needle}: {args:?}"
            );
        }
    }
}
