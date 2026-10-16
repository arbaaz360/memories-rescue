//! Read-only media probe without ffprobe (SRS §4.4, RISK-03).
//!
//! MP4/MOV files are read box by box: `ftyp` for the brand, `mvhd` for the
//! movie duration, and per track `tkhd` (display matrix), `elst` (edit
//! offsets), `mdhd` (media timescale and duration), `hdlr` (video or audio),
//! `stsd` (codec fourcc, coded size, `avcC`/`hvcC`/`esds`), `stts` (frame
//! timing) and `stsz` (bytes per track). Only `moov` is read into memory,
//! which is tens of KB to a few MB for a phone clip. JPEG and PNG photos get
//! their pixel size and EXIF orientation from the JPEG `SOF`/`APP1` segments
//! and the PNG `IHDR`/`eXIf` chunks.
//!
//! Nothing here executes a sidecar: the installer ships ffmpeg but not
//! ffprobe (SRS §10.4), and ffmpeg's own `-i` listing is only a test oracle.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Error;

/// Container as far as the compositor cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    /// ISO BMFF with an MP4-family brand (`isom`, `mp42`, `avc1`, `iso5`, ...).
    Mp4,
    /// ISO BMFF with the QuickTime brand `qt  `.
    Mov,
    Jpeg,
    Png,
}

/// Everything the engine and the planners need to know about one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaInfo {
    pub container: Container,
    /// `ftyp` major brand, trimmed (`isom`, `mp42`, `qt`), when present.
    pub brand: Option<String>,
    pub file_size: u64,
    /// Movie duration in seconds (`mvhd`); 0 for photos.
    pub duration_secs: f64,
    /// Whole-file bitrate from size and duration, kbit/s; `None` for photos.
    pub bitrate_kbps: Option<u32>,
    pub video: Option<VideoTrack>,
    pub audio: Option<AudioTrack>,
    /// Set for photos only.
    pub image: Option<ImageInfo>,
}

impl MediaInfo {
    pub fn is_video(&self) -> bool {
        self.video.is_some()
    }

    /// Displayed size: a video's coded size after its display rotation, or a
    /// photo's pixel size after its EXIF orientation.
    pub fn display_size(&self) -> Option<(u32, u32)> {
        if let Some(v) = &self.video {
            return Some(v.display_size());
        }
        self.image.as_ref().map(ImageInfo::display_size)
    }
}

/// The first video track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoTrack {
    /// Sample entry fourcc: `avc1`, `hvc1`, `hev1`, `mp4v`, ...
    pub codec: String,
    /// Stored (unrotated) size from the sample entry.
    pub coded_width: u32,
    pub coded_height: u32,
    /// Clockwise display rotation from the `tkhd` matrix: 0, 90, 180 or 270.
    /// This is the value the old ffmpeg `rotate` tag carried.
    pub rotation: u16,
    /// Media duration in seconds (`mdhd`).
    pub duration_secs: f64,
    pub timescale: u32,
    pub frame_count: u32,
    /// Nominal frame rate: `timescale / delta` when every sample has the same
    /// duration, otherwise the average `frames / duration`.
    pub frame_rate: Option<f64>,
    /// More than one distinct sample duration in `stts` (a final short sample
    /// is ignored).
    pub variable_frame_rate: bool,
    /// Luma bit depth from `hvcC`, the `avcC` extension or the H.264 SPS.
    pub bit_depth: Option<u8>,
    /// `profile_idc` (H.264) or `general_profile_idc` (HEVC).
    pub profile: Option<u8>,
    /// Sum of `stsz` sample sizes.
    pub bytes: u64,
    /// Track bitrate from `bytes` and `duration_secs`, kbit/s.
    pub bitrate_kbps: Option<u32>,
    /// Seconds of empty edit before the first frame (`elst` with media_time -1).
    pub edit_delay_secs: f64,
    /// Seconds of media skipped at the start by the first non-empty edit.
    pub edit_skip_secs: f64,
    /// Presented length: the sum of the non-empty edits, or the media
    /// duration when the track has no edit list.
    pub presented_secs: f64,
    /// FNV-1a of the decoder configuration record (`avcC`/`hvcC`), to tell
    /// whether clips can be joined by stream copy.
    pub config_hash: Option<u64>,
}

impl VideoTrack {
    /// Coded size with the display rotation applied.
    pub fn display_size(&self) -> (u32, u32) {
        if self.rotation == 90 || self.rotation == 270 {
            (self.coded_height, self.coded_width)
        } else {
            (self.coded_width, self.coded_height)
        }
    }

    /// Presentation end of the track: the empty-edit delay plus the
    /// presented length. Used by the A/V sync check.
    pub fn presentation_end_secs(&self) -> f64 {
        self.edit_delay_secs + self.presented_secs
    }
}

/// The first audio track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioTrack {
    /// `aac`, `mp3`, `ac3`, `alac`, or the raw fourcc when `esds` says nothing.
    pub codec: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub duration_secs: f64,
    pub timescale: u32,
    pub sample_count: u32,
    pub bytes: u64,
    pub bitrate_kbps: Option<u32>,
    pub edit_delay_secs: f64,
    pub edit_skip_secs: f64,
    pub presented_secs: f64,
}

impl AudioTrack {
    pub fn presentation_end_secs(&self) -> f64 {
        self.edit_delay_secs + self.presented_secs
    }

    /// Codecs the MP4 muxer takes by stream copy; anything else is re-encoded
    /// with the native `aac` encoder (§4.4).
    pub fn mp4_copyable(&self) -> bool {
        matches!(
            self.codec.as_str(),
            "aac" | "mp3" | "ac3" | "eac3" | "alac" | "opus" | "flac"
        )
    }
}

/// A photo's pixel size and EXIF orientation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// EXIF orientation 1 to 8; 1 when absent.
    pub orientation: u8,
}

impl ImageInfo {
    /// Size after the orientation is applied: 5 to 8 swap the axes.
    pub fn display_size(&self) -> (u32, u32) {
        if self.orientation >= 5 {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }
}

/// Probe one file by its contents (the extension is not consulted).
pub fn probe(path: &Path) -> Result<MediaInfo, Error> {
    let mut f = BufReader::with_capacity(1 << 16, File::open(path)?);
    let len = f.get_ref().metadata()?.len();
    let mut head = [0u8; 12];
    let n = f.read(&mut head)?;
    f.seek(SeekFrom::Start(0))?;
    if n >= 2 && head[..2] == [0xFF, 0xD8] {
        return probe_jpeg(&mut f, len);
    }
    if n >= 8 && head[..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
        return probe_png(&mut f, len);
    }
    if n >= 8 && is_box_type(&head[4..8]) {
        return probe_mp4(&mut f, len);
    }
    Err(Error::Unsupported(format!(
        "{}: not a JPEG, PNG or ISO BMFF file",
        path.display()
    )))
}

fn is_box_type(t: &[u8]) -> bool {
    matches!(
        t,
        b"ftyp" | b"moov" | b"mdat" | b"free" | b"skip" | b"wide" | b"moof" | b"styp"
    )
}

// ---------------------------------------------------------------------------
// ISO BMFF
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct TopBox {
    typ: [u8; 4],
    start: u64,
    header_len: u64,
    size: u64,
}

fn top_level<R: Read + Seek>(r: &mut R, len: u64) -> Result<Vec<TopBox>, Error> {
    let mut boxes = Vec::new();
    let mut pos = 0u64;
    let mut hdr = [0u8; 16];
    while pos + 8 <= len {
        r.seek(SeekFrom::Start(pos))?;
        let n = r.read(&mut hdr)?;
        if n < 8 {
            break;
        }
        let size32 = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let typ = [hdr[4], hdr[5], hdr[6], hdr[7]];
        let (size, header_len) = match size32 {
            0 => (len - pos, 8),
            1 if n >= 16 => (
                u64::from_be_bytes([
                    hdr[8], hdr[9], hdr[10], hdr[11], hdr[12], hdr[13], hdr[14], hdr[15],
                ]),
                16,
            ),
            1 => break,
            s => (s, 8),
        };
        if size < header_len || pos + size > len {
            break;
        }
        boxes.push(TopBox {
            typ,
            start: pos,
            header_len,
            size,
        });
        pos += size;
    }
    Ok(boxes)
}

/// A child box inside a buffer: its type and payload range.
struct Child {
    typ: [u8; 4],
    payload: std::ops::Range<usize>,
}

/// Children of a container payload. Tolerant: stops at the first malformed box.
fn children(buf: &[u8]) -> Vec<Child> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 8 <= buf.len() {
        let size32 = be32(buf, pos).unwrap_or(0) as usize;
        let typ = [buf[pos + 4], buf[pos + 5], buf[pos + 6], buf[pos + 7]];
        let (size, hdr) = match size32 {
            0 => (buf.len() - pos, 8),
            1 => match be64(buf, pos + 8) {
                Some(s) => (s as usize, 16),
                None => break,
            },
            s => (s, 8),
        };
        if size < hdr || pos + size > buf.len() {
            break;
        }
        out.push(Child {
            typ,
            payload: pos + hdr..pos + size,
        });
        pos += size;
    }
    out
}

fn child<'a>(buf: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    children(buf)
        .into_iter()
        .find(|c| &c.typ == typ)
        .map(|c| &buf[c.payload])
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    b.get(at..at + 2).map(|s| u16::from_be_bytes([s[0], s[1]]))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8)
        .map(|s| u64::from_be_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

fn fourcc(t: &[u8; 4]) -> String {
    String::from_utf8_lossy(t).trim().to_string()
}

fn probe_mp4<R: Read + Seek>(r: &mut R, len: u64) -> Result<MediaInfo, Error> {
    let boxes = top_level(r, len)?;
    let mut brand = None;
    let mut container = Container::Mp4;
    if let Some(ftyp) = boxes.iter().find(|b| &b.typ == b"ftyp") {
        r.seek(SeekFrom::Start(ftyp.start + ftyp.header_len))?;
        let mut major = [0u8; 4];
        if (ftyp.size - ftyp.header_len) >= 4 {
            r.read_exact(&mut major)?;
            if &major == b"qt  " {
                container = Container::Mov;
            }
            brand = Some(fourcc(&major));
        }
    }
    let moov = boxes
        .iter()
        .find(|b| &b.typ == b"moov")
        .ok_or_else(|| Error::Unsupported("no moov box (not a finished MP4/MOV)".into()))?;
    let payload_len = (moov.size - moov.header_len) as usize;
    if payload_len > 256 << 20 {
        return Err(Error::Unsupported("moov larger than 256 MiB".into()));
    }
    r.seek(SeekFrom::Start(moov.start + moov.header_len))?;
    let mut payload = vec![0u8; payload_len];
    r.read_exact(&mut payload)?;

    let (movie_timescale, movie_duration) = child(&payload, b"mvhd")
        .and_then(parse_mvhd)
        .unwrap_or((1000, 0));
    let duration_secs = if movie_timescale > 0 {
        movie_duration as f64 / movie_timescale as f64
    } else {
        0.0
    };

    let mut video = None;
    let mut audio = None;
    for trak in children(&payload).into_iter().filter(|c| &c.typ == b"trak") {
        let t = &payload[trak.payload];
        match parse_trak(t, movie_timescale) {
            Some(Track::Video(v)) if video.is_none() => video = Some(v),
            Some(Track::Audio(a)) if audio.is_none() => audio = Some(a),
            _ => {}
        }
    }
    let bitrate_kbps = (duration_secs > 0.0).then(|| kbps(len, duration_secs));
    Ok(MediaInfo {
        container,
        brand,
        file_size: len,
        duration_secs,
        bitrate_kbps,
        video,
        audio,
        image: None,
    })
}

fn kbps(bytes: u64, secs: f64) -> u32 {
    (bytes as f64 * 8.0 / secs / 1000.0).round() as u32
}

/// `(timescale, duration)` from `mvhd`.
fn parse_mvhd(p: &[u8]) -> Option<(u32, u64)> {
    match p.first()? {
        0 => Some((be32(p, 12)?, be32(p, 16)? as u64)),
        1 => Some((be32(p, 20)?, be64(p, 24)?)),
        _ => None,
    }
}

/// `(timescale, duration)` from `mdhd`; the same layout as `mvhd` for these fields.
fn parse_mdhd(p: &[u8]) -> Option<(u32, u64)> {
    parse_mvhd(p)
}

enum Track {
    Video(VideoTrack),
    Audio(AudioTrack),
}

/// Clockwise display rotation from the `tkhd` matrix `[a b u c d v x y w]`
/// (16.16 fixed point except u, v, w). `atan2(b, a)` in degrees, normalised to
/// `[0, 360)` and snapped to a multiple of 90, matches what ffmpeg's mov
/// demuxer exported as the `rotate` tag (its `av_display_rotation_get` returns
/// the counter-clockwise angle, which the demuxer negated).
fn parse_tkhd_rotation(p: &[u8]) -> Option<u16> {
    let matrix_at = match p.first()? {
        0 => 40,
        1 => 52,
        _ => return None,
    };
    let a = be32(p, matrix_at)? as i32 as f64 / 65536.0;
    let b = be32(p, matrix_at + 4)? as i32 as f64 / 65536.0;
    if a == 0.0 && b == 0.0 {
        return Some(0);
    }
    let deg = b.atan2(a).to_degrees();
    let deg = ((deg % 360.0) + 360.0) % 360.0;
    let snapped = ((deg / 90.0).round() as u16 % 4) * 90;
    Some(snapped)
}

/// Edit list summary, in seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Edits {
    /// Leading empty edit.
    delay: f64,
    /// Media time where the first non-empty edit starts.
    skip: f64,
    /// Sum of the non-empty edit durations; `None` when an edit has duration
    /// 0 (meaning "to the end of the media").
    presented: Option<f64>,
}

fn parse_elst(p: &[u8], movie_timescale: u32, media_timescale: u32) -> Option<Edits> {
    let version = *p.first()?;
    let count = be32(p, 4)? as usize;
    let mut pos = 8;
    let mut edits = Edits {
        presented: Some(0.0),
        ..Edits::default()
    };
    let mut seen_media = false;
    for i in 0..count.min(64) {
        let (seg_dur, media_time) = match version {
            0 => (be32(p, pos)? as u64, be32(p, pos + 4)? as i32 as i64),
            1 => (be64(p, pos)?, be64(p, pos + 8)? as i64),
            _ => return None,
        };
        pos += if version == 0 { 12 } else { 20 };
        let secs = if movie_timescale > 0 {
            seg_dur as f64 / movie_timescale as f64
        } else {
            0.0
        };
        if media_time < 0 {
            if i == 0 {
                edits.delay = secs;
            }
            continue;
        }
        if !seen_media {
            seen_media = true;
            if media_timescale > 0 {
                edits.skip = media_time as f64 / media_timescale as f64;
            }
        }
        edits.presented = match (edits.presented, seg_dur) {
            (_, 0) | (None, _) => None,
            (Some(sum), _) => Some(sum + secs),
        };
    }
    if !seen_media {
        edits.presented = Some(0.0);
    }
    Some(edits)
}

fn parse_trak(t: &[u8], movie_timescale: u32) -> Option<Track> {
    let mdia = child(t, b"mdia")?;
    let handler =
        child(mdia, b"hdlr").and_then(|h| h.get(8..12).map(|s| [s[0], s[1], s[2], s[3]]))?;
    let (timescale, media_duration) = child(mdia, b"mdhd").and_then(parse_mdhd)?;
    let duration_secs = if timescale > 0 {
        media_duration as f64 / timescale as f64
    } else {
        0.0
    };
    let edits = child(t, b"edts")
        .and_then(|e| child(e, b"elst"))
        .and_then(|e| parse_elst(e, movie_timescale, timescale));
    let edit_delay_secs = edits.map(|e| e.delay).unwrap_or(0.0);
    let edit_skip_secs = edits.map(|e| e.skip).unwrap_or(0.0);
    let presented_secs = edits
        .and_then(|e| e.presented)
        .unwrap_or((duration_secs - edit_skip_secs).max(0.0));
    let stbl = child(mdia, b"minf").and_then(|m| child(m, b"stbl"))?;
    let stsd = child(stbl, b"stsd")?;
    let entries = children(stsd.get(8..)?);
    let entry = entries.first()?;
    let codec_fourcc = entry.typ;
    let entry_payload = &stsd[8..][entry.payload.clone()];
    let (bytes, sample_count) = child(stbl, b"stsz").and_then(parse_stsz).unwrap_or((0, 0));
    let bitrate_kbps = (duration_secs > 0.0 && bytes > 0).then(|| kbps(bytes, duration_secs));

    match &handler {
        b"vide" => {
            let coded_width = be16(entry_payload, 24)? as u32;
            let coded_height = be16(entry_payload, 26)? as u32;
            let rotation = child(t, b"tkhd").and_then(parse_tkhd_rotation).unwrap_or(0);
            let (frame_count, frame_rate, variable_frame_rate) = child(stbl, b"stts")
                .map(|s| parse_stts(s, timescale))
                .unwrap_or((sample_count, None, false));
            let config = entry_payload.get(78..).unwrap_or(&[]);
            let record = match &codec_fourcc {
                b"avc1" | b"avc3" => child(config, b"avcC"),
                b"hvc1" | b"hev1" => child(config, b"hvcC"),
                _ => None,
            };
            let (bit_depth, profile) = match (&codec_fourcc, record) {
                (b"avc1" | b"avc3", Some(r)) => parse_avcc(r),
                (b"hvc1" | b"hev1", Some(r)) => parse_hvcc(r),
                _ => (None, None),
            };
            let config_hash = record.map(fnv1a);
            Some(Track::Video(VideoTrack {
                codec: fourcc(&codec_fourcc),
                coded_width,
                coded_height,
                rotation,
                duration_secs,
                timescale,
                frame_count,
                frame_rate,
                variable_frame_rate,
                bit_depth,
                profile,
                bytes,
                bitrate_kbps,
                edit_delay_secs,
                edit_skip_secs,
                presented_secs,
                config_hash,
            }))
        }
        b"soun" => {
            let version = be16(entry_payload, 8).unwrap_or(0);
            let channels = be16(entry_payload, 16).unwrap_or(0);
            let sample_rate = if version == 2 {
                // QuickTime version 2 sound description: 64-bit float rate at 32.
                be64(entry_payload, 32)
                    .map(|bits| f64::from_bits(bits).round() as u32)
                    .unwrap_or(0)
            } else {
                be32(entry_payload, 24).unwrap_or(0) >> 16
            };
            let config_at = match version {
                1 => 44,
                2 => 64,
                _ => 28,
            };
            let config = entry_payload.get(config_at..).unwrap_or(&[]);
            let codec = match &codec_fourcc {
                b"mp4a" => child(config, b"esds")
                    .and_then(esds_object_type)
                    .map(|oti| match oti {
                        0x40 | 0x66 | 0x67 | 0x68 => "aac".to_string(),
                        0x69 | 0x6B => "mp3".to_string(),
                        0xA9 => "dts".to_string(),
                        other => format!("mp4a/0x{other:02x}"),
                    })
                    .unwrap_or_else(|| "aac".to_string()),
                b"ac-3" => "ac3".to_string(),
                b"ec-3" => "eac3".to_string(),
                b"alac" => "alac".to_string(),
                b"Opus" => "opus".to_string(),
                b"fLaC" => "flac".to_string(),
                b".mp3" => "mp3".to_string(),
                other => fourcc(other),
            };
            Some(Track::Audio(AudioTrack {
                codec,
                channels,
                sample_rate,
                duration_secs,
                timescale,
                sample_count,
                bytes,
                bitrate_kbps,
                edit_delay_secs,
                edit_skip_secs,
                presented_secs,
            }))
        }
        _ => None,
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// `(total bytes, sample count)` from `stsz`.
fn parse_stsz(p: &[u8]) -> Option<(u64, u32)> {
    let sample_size = be32(p, 4)?;
    let count = be32(p, 8)?;
    if sample_size != 0 {
        return Some((sample_size as u64 * count as u64, count));
    }
    let mut total = 0u64;
    for i in 0..count as usize {
        total += be32(p, 12 + i * 4)? as u64;
    }
    Some((total, count))
}

/// `(frame count, nominal frame rate, variable)` from `stts`.
fn parse_stts(p: &[u8], timescale: u32) -> (u32, Option<f64>, bool) {
    let count = be32(p, 4).unwrap_or(0) as usize;
    let mut entries: Vec<(u32, u32)> = Vec::with_capacity(count.min(1 << 16));
    for i in 0..count {
        match (be32(p, 8 + i * 8), be32(p, 12 + i * 8)) {
            (Some(n), Some(d)) => entries.push((n, d)),
            _ => break,
        }
    }
    let frames: u64 = entries.iter().map(|(n, _)| *n as u64).sum();
    let ticks: u64 = entries.iter().map(|(n, d)| *n as u64 * *d as u64).sum();
    if frames == 0 || ticks == 0 || timescale == 0 {
        return (frames as u32, None, false);
    }
    // A single trailing sample with its own duration is how muxers close a
    // constant-rate stream; it does not make the stream variable.
    let body: Vec<&(u32, u32)> = match entries.as_slice() {
        [head @ .., (1, _)] if !head.is_empty() => head.iter().collect(),
        all => all.iter().collect(),
    };
    let first = body[0].1;
    let constant = body.iter().all(|(_, d)| *d == first) && first > 0;
    let rate = if constant {
        timescale as f64 / first as f64
    } else {
        frames as f64 * timescale as f64 / ticks as f64
    };
    let rate = (rate * 1000.0).round() / 1000.0;
    (frames as u32, Some(rate), !constant)
}

/// `(bit depth, profile)` from an `avcC` record: the optional extension
/// first, then the SPS, then the profile alone.
fn parse_avcc(p: &[u8]) -> (Option<u8>, Option<u8>) {
    let profile = p.get(1).copied();
    let Some(profile_idc) = profile else {
        return (None, None);
    };
    let high = matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    );
    // Walk the parameter sets to find where the extension would start.
    let mut pos = 6usize;
    let sps_count = p.get(5).map(|b| b & 0x1F).unwrap_or(0) as usize;
    let mut first_sps: Option<&[u8]> = None;
    for _ in 0..sps_count {
        let Some(len) = be16(p, pos) else {
            return (None, profile);
        };
        let nal = p.get(pos + 2..pos + 2 + len as usize);
        if first_sps.is_none() {
            first_sps = nal;
        }
        pos += 2 + len as usize;
    }
    let pps_count = p.get(pos).copied().unwrap_or(0) as usize;
    pos += 1;
    for _ in 0..pps_count {
        let Some(len) = be16(p, pos) else {
            return (None, profile);
        };
        pos += 2 + len as usize;
    }
    if !high {
        return (Some(8), profile);
    }
    if let Some(b) = p.get(pos + 1) {
        return (Some((b & 0x07) + 8), profile);
    }
    let depth = first_sps.and_then(sps_bit_depth);
    (depth, profile)
}

/// Luma bit depth from an H.264 SPS NAL unit (with its 1-byte header).
fn sps_bit_depth(nal: &[u8]) -> Option<u8> {
    let rbsp = unescape_rbsp(nal.get(1..)?);
    let mut r = BitReader::new(&rbsp);
    let profile_idc = r.bits(8)? as u8;
    r.bits(8)?; // constraint flags
    r.bits(8)?; // level_idc
    r.ue()?; // seq_parameter_set_id
    if !matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        return Some(8);
    }
    let chroma_format_idc = r.ue()?;
    if chroma_format_idc == 3 {
        r.bits(1)?;
    }
    let bit_depth_luma_minus8 = r.ue()?;
    Some((bit_depth_luma_minus8 as u8).saturating_add(8))
}

fn unescape_rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

struct BitReader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.buf.get(self.pos / 8)?;
        let bit = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(bit as u32)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }

    /// Unsigned Exp-Golomb.
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.bits(zeros)?;
        Some((1u32 << zeros) - 1 + rest)
    }
}

/// `(bit depth, profile)` from an `hvcC` record.
fn parse_hvcc(p: &[u8]) -> (Option<u8>, Option<u8>) {
    let profile = p.get(1).map(|b| b & 0x1F);
    let depth = p.get(17).map(|b| (b & 0x07) + 8);
    (depth, profile)
}

/// The `objectTypeIndication` of the `DecoderConfigDescriptor` inside `esds`.
fn esds_object_type(p: &[u8]) -> Option<u8> {
    let mut pos = 4; // version/flags
    let mut guard = 0;
    while pos < p.len() && guard < 16 {
        guard += 1;
        let tag = *p.get(pos)?;
        pos += 1;
        let mut size = 0usize;
        for _ in 0..4 {
            let b = *p.get(pos)?;
            pos += 1;
            size = (size << 7) | (b & 0x7F) as usize;
            if b & 0x80 == 0 {
                break;
            }
        }
        match tag {
            0x03 => {
                // ES_Descriptor: ES_ID(2) flags(1) [dependsOn(2)] [url] [OCR(2)] then children.
                let flags = *p.get(pos + 2)?;
                pos += 3;
                if flags & 0x80 != 0 {
                    pos += 2;
                }
                if flags & 0x40 != 0 {
                    let url_len = *p.get(pos)? as usize;
                    pos += 1 + url_len;
                }
                if flags & 0x20 != 0 {
                    pos += 2;
                }
            }
            0x04 => return p.get(pos).copied(),
            _ => pos += size,
        }
    }
    None
}

// ---------------------------------------------------------------------------
// JPEG and PNG
// ---------------------------------------------------------------------------

fn probe_jpeg<R: Read>(r: &mut R, len: u64) -> Result<MediaInfo, Error> {
    let mut data = Vec::new();
    r.take(4 << 20).read_to_end(&mut data)?;
    let mut pos = 2usize;
    let mut size = None;
    let mut orientation = 1u8;
    while pos + 4 <= data.len() {
        if data[pos] != 0xFF {
            pos += 1;
            continue;
        }
        let marker = data[pos + 1];
        if marker == 0xFF {
            pos += 1;
            continue;
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            pos += 2;
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        let seg_len = be16(&data, pos + 2).unwrap_or(0) as usize;
        if seg_len < 2 {
            break;
        }
        let seg = data.get(pos + 4..pos + 2 + seg_len).unwrap_or(&[]);
        match marker {
            0xE1 if seg.starts_with(b"Exif\0\0") => {
                if let Some(o) = tiff_orientation(&seg[6..]) {
                    orientation = o;
                }
            }
            0xC0 | 0xC1 | 0xC2 | 0xC3 | 0xC5 | 0xC6 | 0xC7 | 0xC9 | 0xCA | 0xCB | 0xCD | 0xCE
            | 0xCF => {
                if let (Some(h), Some(w)) = (be16(seg, 1), be16(seg, 3)) {
                    size = Some((w as u32, h as u32));
                    break;
                }
            }
            _ => {}
        }
        pos += 2 + seg_len;
    }
    let (width, height) =
        size.ok_or_else(|| Error::Unsupported("JPEG without a frame header".into()))?;
    Ok(photo_info(Container::Jpeg, len, width, height, orientation))
}

fn probe_png<R: Read>(r: &mut R, len: u64) -> Result<MediaInfo, Error> {
    let mut data = Vec::new();
    r.take(4 << 20).read_to_end(&mut data)?;
    let mut pos = 8usize;
    let mut size = None;
    let mut orientation = 1u8;
    while pos + 8 <= data.len() {
        let chunk_len = be32(&data, pos).unwrap_or(0) as usize;
        let typ = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + chunk_len).unwrap_or(&[]);
        match typ {
            b"IHDR" => {
                if let (Some(w), Some(h)) = (be32(body, 0), be32(body, 4)) {
                    size = Some((w, h));
                }
            }
            b"eXIf" => {
                if let Some(o) = tiff_orientation(body) {
                    orientation = o;
                }
            }
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        pos += 12 + chunk_len;
    }
    let (width, height) = size.ok_or_else(|| Error::Unsupported("PNG without IHDR".into()))?;
    Ok(photo_info(Container::Png, len, width, height, orientation))
}

fn photo_info(
    container: Container,
    len: u64,
    width: u32,
    height: u32,
    orientation: u8,
) -> MediaInfo {
    MediaInfo {
        container,
        brand: None,
        file_size: len,
        duration_secs: 0.0,
        bitrate_kbps: None,
        video: None,
        audio: None,
        image: Some(ImageInfo {
            width,
            height,
            orientation,
        }),
    }
}

/// EXIF orientation (tag 0x0112) from a TIFF blob (IFD0 only).
pub(crate) fn tiff_orientation(t: &[u8]) -> Option<u8> {
    let le = match t.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let rd16 = |at: usize| -> Option<u16> {
        let s = t.get(at..at + 2)?;
        Some(if le {
            u16::from_le_bytes([s[0], s[1]])
        } else {
            u16::from_be_bytes([s[0], s[1]])
        })
    };
    let rd32 = |at: usize| -> Option<u32> {
        let s = t.get(at..at + 4)?;
        Some(if le {
            u32::from_le_bytes([s[0], s[1], s[2], s[3]])
        } else {
            u32::from_be_bytes([s[0], s[1], s[2], s[3]])
        })
    };
    if rd16(2)? != 42 {
        return None;
    }
    let ifd = rd32(4)? as usize;
    let count = rd16(ifd)? as usize;
    for i in 0..count.min(512) {
        let e = ifd + 2 + i * 12;
        if rd16(e)? == 0x0112 {
            let v = rd16(e + 8)? as u8;
            return (1..=8).contains(&v).then_some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tkhd_with_matrix(a: i32, b: i32, c: i32, d: i32) -> Vec<u8> {
        let mut p = vec![0u8; 84];
        for (i, v) in [a, b, 0, c, d, 0, 0, 0, 0x4000_0000].iter().enumerate() {
            p[40 + i * 4..44 + i * 4].copy_from_slice(&(*v as u32).to_be_bytes());
        }
        p
    }

    #[test]
    fn rotation_from_matrix() {
        let one = 0x1_0000;
        assert_eq!(
            parse_tkhd_rotation(&tkhd_with_matrix(one, 0, 0, one)),
            Some(0)
        );
        assert_eq!(
            parse_tkhd_rotation(&tkhd_with_matrix(0, one, -one, 0)),
            Some(90)
        );
        assert_eq!(
            parse_tkhd_rotation(&tkhd_with_matrix(-one, 0, 0, -one)),
            Some(180)
        );
        assert_eq!(
            parse_tkhd_rotation(&tkhd_with_matrix(0, -one, one, 0)),
            Some(270)
        );
        assert_eq!(parse_tkhd_rotation(&tkhd_with_matrix(0, 0, 0, 0)), Some(0));
    }

    #[test]
    fn stts_constant_and_variable() {
        let mut p = vec![0u8; 8];
        p[4..8].copy_from_slice(&2u32.to_be_bytes());
        p.extend_from_slice(&299u32.to_be_bytes());
        p.extend_from_slice(&512u32.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
        p.extend_from_slice(&256u32.to_be_bytes());
        assert_eq!(parse_stts(&p, 15360), (300, Some(30.0), false));

        let mut v = vec![0u8; 8];
        v[4..8].copy_from_slice(&2u32.to_be_bytes());
        v.extend_from_slice(&150u32.to_be_bytes());
        v.extend_from_slice(&666u32.to_be_bytes());
        v.extend_from_slice(&150u32.to_be_bytes());
        v.extend_from_slice(&358u32.to_be_bytes());
        let (n, rate, vfr) = parse_stts(&v, 15360);
        assert_eq!(n, 300);
        assert!(vfr);
        assert!((rate.unwrap() - 30.0).abs() < 0.2, "{rate:?}");
    }

    #[test]
    fn orientation_from_tiff() {
        let mut t = b"MM\0\x2a\0\0\0\x08\0\x01\x01\x12\0\x03\0\0\0\x01\0\x06\0\0\0\0\0\0".to_vec();
        assert_eq!(tiff_orientation(&t), Some(6));
        t[0] = b'X';
        assert_eq!(tiff_orientation(&t), None);
        let le = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x08\0\0\0\0\0\0\0".to_vec();
        assert_eq!(tiff_orientation(&le), Some(8));
    }

    #[test]
    fn sps_bit_depth_high10() {
        // profile_idc 110, constraints 0, level 31, sps_id 0 (ue "1"),
        // chroma_format_idc 1 (ue "010"), bit_depth_luma_minus8 2 (ue "011").
        // bits: 1 010 011 ... -> 1010 0110 -> 0xA6
        let nal = [0x67, 110, 0, 31, 0xA6, 0x00];
        assert_eq!(sps_bit_depth(&nal), Some(10));
        let base = [0x67, 66, 0, 31, 0x80];
        assert_eq!(sps_bit_depth(&base), Some(8));
    }

    #[test]
    fn hvcc_depth() {
        let mut p = vec![0u8; 23];
        p[1] = 0x01; // main profile
        p[17] = 0xF8 | 2; // bitDepthLumaMinus8 = 2
        assert_eq!(parse_hvcc(&p), (Some(10), Some(1)));
    }

    #[test]
    fn esds_aac() {
        // version/flags, ES_Descriptor(03) len, ES_ID, flags 0, DecoderConfig(04) len, OTI 0x40
        let p = [0, 0, 0, 0, 0x03, 0x19, 0, 1, 0, 0x04, 0x11, 0x40, 0x15];
        assert_eq!(esds_object_type(&p), Some(0x40));
    }
}
