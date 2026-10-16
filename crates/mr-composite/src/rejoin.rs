//! Clip rejoin (FR-27, SRS §4.5): which consecutive clips belong together,
//! and the ffmpeg plan that joins them.
//!
//! Joining uses the concat demuxer. The video is stream-copied unless
//! overlays are burned in (then each segment's overlay is enabled over its
//! own time span) or the segments' decoder configurations differ. The audio
//! is always re-encoded with ffmpeg's native `aac` encoder: copied AAC
//! carries each later segment's encoder priming into the middle of the
//! stream, which measured as a ~10 ms drop to under 1% level at every join
//! (an audible click), while the re-encoded join stays above 97%. The
//! input timeline is kept with `-copyts` (see [`rejoin_plan`]).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::ffmpeg::VideoEncoder;
use crate::probe::MediaInfo;
use crate::video::{
    AudioPlan, OverlayInput, Plan, VideoOptions, Warning, aspect_difference, checked_placement,
    encoder_bitrate_kbps, even_size, head_args, rotation_filters, tail_args,
};

/// What the detector needs to know about one item. The engine builds these
/// from the index and [`crate::probe`]; [`ClipDescriptor::from_probe`] helps.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipDescriptor {
    pub item_id: String,
    /// The export's `Date` (UTC): when the clip started.
    pub capture_utc: DateTime<Utc>,
    pub is_video: bool,
    pub duration_secs: f64,
    /// Displayed size (after rotation).
    pub width: u32,
    pub height: u32,
    /// Sample entry fourcc, e.g. `avc1`.
    pub codec: String,
    pub fps: Option<f64>,
    /// Decimal degrees; `None` when the item has no location.
    pub location: Option<(f64, f64)>,
}

impl ClipDescriptor {
    /// A descriptor from a probe; `None` when `info` has no video track.
    pub fn from_probe(
        item_id: impl Into<String>,
        capture_utc: DateTime<Utc>,
        location: Option<(f64, f64)>,
        info: &MediaInfo,
    ) -> Option<Self> {
        let v = info.video.as_ref()?;
        let (width, height) = v.display_size();
        Some(Self {
            item_id: item_id.into(),
            capture_utc,
            is_video: true,
            duration_secs: info.duration_secs,
            width,
            height,
            codec: v.codec.clone(),
            fps: v.frame_rate,
            location,
        })
    }

    fn end_secs(&self) -> f64 {
        self.capture_utc.timestamp_millis() as f64 / 1000.0 + self.duration_secs
    }

    fn start_secs(&self) -> f64 {
        self.capture_utc.timestamp_millis() as f64 / 1000.0
    }
}

/// Which capture years are candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum YearRange {
    All,
    /// Inclusive, by UTC calendar year.
    Years {
        first: i32,
        last: i32,
    },
}

/// §4.5 thresholds. `Default` is the SRS: 2017–2021, ≥ 9.0 s, ≤ 2 s gap.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RejoinOptions {
    pub years: YearRange,
    /// Every clip but the last must be at least this long.
    pub min_clip_secs: f64,
    /// Largest gap between one clip's end (`Date` + duration) and the next `Date`.
    pub max_gap_secs: f64,
    /// Largest overlap accepted: `Date` has whole seconds, so a 10.4 s clip
    /// followed at +10 s looks like a 0.4 s overlap.
    pub max_overlap_secs: f64,
    /// Two locations "match" within this distance (the §3.6 100 m figure).
    pub location_tolerance_m: f64,
    /// Frame rates match within this many frames per second.
    pub fps_tolerance: f64,
}

impl Default for RejoinOptions {
    fn default() -> Self {
        Self {
            years: YearRange::Years {
                first: 2017,
                last: 2021,
            },
            min_clip_secs: 9.0,
            max_gap_secs: 2.0,
            max_overlap_secs: 1.0,
            location_tolerance_m: 100.0,
            fps_tolerance: 0.01,
        }
    }
}

/// Why two clips are not joined; for the dry-run preview and tests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum NotJoined {
    NotVideo,
    OutsideYears,
    Resolution,
    Codec,
    FrameRate,
    /// The earlier clip is shorter than `min_clip_secs` and is not the last.
    TooShort {
        secs: f64,
    },
    Gap {
        secs: f64,
    },
    Location,
}

/// One group of consecutive clips to join, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipGroup {
    pub item_ids: Vec<String>,
    pub start_utc: DateTime<Utc>,
    /// Sum of the clip durations: the joined file's expected length.
    pub total_secs: f64,
    /// Gap before each clip after the first.
    pub gaps_secs: Vec<f64>,
}

fn in_years(c: &ClipDescriptor, years: YearRange) -> bool {
    match years {
        YearRange::All => true,
        YearRange::Years { first, last } => (first..=last).contains(&c.capture_utc.year()),
    }
}

fn distance_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (la1, lo1) = (a.0.to_radians(), a.1.to_radians());
    let (la2, lo2) = (b.0.to_radians(), b.1.to_radians());
    let h = ((la2 - la1) / 2.0).sin().powi(2)
        + la1.cos() * la2.cos() * ((lo2 - lo1) / 2.0).sin().powi(2);
    2.0 * 6_371_008.8 * h.sqrt().asin()
}

/// Can `next` directly follow `prev` in one group? Every §4.5 rule.
pub fn can_follow(
    prev: &ClipDescriptor,
    next: &ClipDescriptor,
    opts: &RejoinOptions,
) -> Result<(), NotJoined> {
    if !prev.is_video || !next.is_video {
        return Err(NotJoined::NotVideo);
    }
    if !in_years(prev, opts.years) || !in_years(next, opts.years) {
        return Err(NotJoined::OutsideYears);
    }
    if (prev.width, prev.height) != (next.width, next.height) {
        return Err(NotJoined::Resolution);
    }
    if prev.codec != next.codec {
        return Err(NotJoined::Codec);
    }
    match (prev.fps, next.fps) {
        (Some(a), Some(b)) if (a - b).abs() <= opts.fps_tolerance => {}
        (None, None) => {}
        _ => return Err(NotJoined::FrameRate),
    }
    if prev.duration_secs < opts.min_clip_secs {
        return Err(NotJoined::TooShort {
            secs: prev.duration_secs,
        });
    }
    let gap = next.start_secs() - prev.end_secs();
    if gap > opts.max_gap_secs || gap < -opts.max_overlap_secs {
        return Err(NotJoined::Gap { secs: gap });
    }
    match (prev.location, next.location) {
        (None, None) => {}
        (Some(a), Some(b)) if distance_m(a, b) <= opts.location_tolerance_m => {}
        _ => return Err(NotJoined::Location),
    }
    Ok(())
}

/// Groups of two or more clips that §4.5 says belong together, in capture
/// order. Each clip is in at most one group. A clip's successor is the
/// earliest later clip that may follow it and is not already taken, so an
/// unrelated clip recorded in between (a decoy) does not break a group.
pub fn detect_clip_groups(items: &[ClipDescriptor], opts: &RejoinOptions) -> Vec<ClipGroup> {
    let mut clips: Vec<&ClipDescriptor> = items
        .iter()
        .filter(|c| c.is_video && in_years(c, opts.years))
        .collect();
    clips.sort_by(|a, b| {
        a.capture_utc
            .cmp(&b.capture_utc)
            .then_with(|| a.item_id.cmp(&b.item_id))
    });
    let n = clips.len();
    let mut next: Vec<Option<usize>> = vec![None; n];
    let mut has_prev = vec![false; n];
    for i in 0..n {
        let horizon = clips[i].end_secs() + opts.max_gap_secs;
        // Candidates may start up to max_overlap before the end, and the list
        // is sorted by start, so scan forward from the first one that could.
        let lo =
            clips.partition_point(|c| c.start_secs() < clips[i].end_secs() - opts.max_overlap_secs);
        for j in lo.max(i + 1)..n {
            if clips[j].start_secs() > horizon {
                break;
            }
            if !has_prev[j] && can_follow(clips[i], clips[j], opts).is_ok() {
                next[i] = Some(j);
                has_prev[j] = true;
                break;
            }
        }
    }
    let mut groups = Vec::new();
    for start in 0..n {
        if has_prev[start] || next[start].is_none() {
            continue;
        }
        let mut ids = vec![clips[start].item_id.clone()];
        let mut total = clips[start].duration_secs;
        let mut gaps = Vec::new();
        let mut cur = start;
        while let Some(j) = next[cur] {
            gaps.push(clips[j].start_secs() - clips[cur].end_secs());
            ids.push(clips[j].item_id.clone());
            total += clips[j].duration_secs;
            cur = j;
        }
        groups.push(ClipGroup {
            item_ids: ids,
            start_utc: clips[start].capture_utc,
            total_secs: total,
            gaps_secs: gaps,
        });
    }
    groups
}

/// One segment to join: the main file and, if it is burned in, its overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub path: PathBuf,
    pub overlay: Option<PathBuf>,
}

/// An `ffconcat` list. Paths use forward slashes and single quotes, with a
/// quote written as `'\''`, which is how the concat demuxer reads them.
pub fn concat_list(paths: &[&Path]) -> String {
    let mut s = String::from("ffconcat version 1.0\n");
    for p in paths {
        let text = p
            .to_string_lossy()
            .replace('\\', "/")
            .replace('\'', "'\\''");
        s.push_str(&format!("file '{text}'\n"));
    }
    s
}

/// Check that the segments can be joined at all: same display size and
/// rotation, same codec. Returns whether a stream copy is possible
/// (identical decoder configurations).
pub fn check_segments(infos: &[MediaInfo]) -> Result<bool, Error> {
    let first = infos
        .first()
        .and_then(|i| i.video.as_ref())
        .ok_or_else(|| Error::Unsupported("rejoin needs at least one video segment".into()))?;
    let mut same_config = true;
    for (k, info) in infos.iter().enumerate().skip(1) {
        let v = info
            .video
            .as_ref()
            .ok_or_else(|| Error::Unsupported(format!("segment {k} has no video track")))?;
        if v.codec != first.codec
            || (v.coded_width, v.coded_height) != (first.coded_width, first.coded_height)
            || v.rotation != first.rotation
        {
            return Err(Error::Unsupported(format!(
                "segment {k} differs from segment 0 ({} {}x{} r{} vs {} {}x{} r{})",
                v.codec,
                v.coded_width,
                v.coded_height,
                v.rotation,
                first.codec,
                first.coded_width,
                first.coded_height,
                first.rotation
            )));
        }
        same_config &= v.config_hash.is_some() && v.config_hash == first.config_hash;
    }
    Ok(same_config)
}

/// Plan the join. `encoder: None` asks for a video stream copy, which is
/// refused when any segment has an overlay. `overlays[k]` belongs to
/// segment `k` and is enabled from that segment's start to the next one's.
#[allow(clippy::too_many_arguments)]
pub fn rejoin_plan(
    list: &Path,
    infos: &[MediaInfo],
    overlays: &[Option<OverlayInput<'_>>],
    out: &Path,
    encoder: Option<VideoEncoder>,
    opts: &VideoOptions,
) -> Result<Plan, Error> {
    let first = infos
        .first()
        .and_then(|i| i.video.as_ref())
        .ok_or_else(|| Error::Unsupported("rejoin needs at least one video segment".into()))?;
    let total: f64 = infos.iter().map(|i| i.duration_secs).sum();
    let display = first.display_size();
    let has_audio = infos.iter().all(|i| i.audio.is_some());
    let audio = if has_audio {
        AudioPlan::Aac {
            kbps: opts.audio_kbps,
        }
    } else {
        AudioPlan::None
    };
    let mut warnings = Vec::new();
    let mut args = head_args();
    // Keep the sources' own timeline. Without -copyts ffmpeg shifts the
    // concat input so its earliest packet, the first segment's AAC priming at
    // -23 ms, lands on 0; the video then starts 23 ms late, one frame after
    // re-encoding, and every segment's overlay window is a frame off.
    args.push("-copyts".into());

    let (filter, output_size, bitrate_kbps) = match encoder {
        None => {
            if overlays.iter().any(Option::is_some) {
                return Err(Error::Unsupported(
                    "overlays cannot be burned in with a stream copy".into(),
                ));
            }
            // The concat demuxer gives the stream the first file's display
            // matrix; nothing to do for rotation.
            args.extend(["-f", "concat", "-safe", "0", "-i"].map(OsString::from));
            args.push(list.as_os_str().to_owned());
            args.extend(["-map", "0:v:0", "-c:v", "copy"].map(OsString::from));
            (String::new(), display, None)
        }
        Some(enc) => {
            let size = even_size(display);
            if size != display {
                warnings.push(Warning::OddSizeScaled {
                    from: display,
                    to: size,
                });
            }
            let mut base: Vec<String> = Vec::new();
            if let Some(r) = rotation_filters(first.rotation) {
                base.push(r.to_string());
            }
            if size != display {
                base.push(format!("scale={}:{}", size.0, size.1));
            }
            args.extend(
                [
                    "-noautorotate",
                    "-display_rotation",
                    "0",
                    "-f",
                    "concat",
                    "-safe",
                    "0",
                    "-i",
                ]
                .map(OsString::from),
            );
            args.push(list.as_os_str().to_owned());
            let mut graph = Vec::new();
            let mut label = "[0:v]".to_string();
            if !base.is_empty() {
                graph.push(format!("[0:v]{}[base]", base.join(",")));
                label = "[base]".into();
            }
            let mut input = 1;
            let mut t0 = 0.0;
            for (k, info) in infos.iter().enumerate() {
                let t1 = t0 + info.duration_secs;
                if let Some(Some(ov)) = overlays.get(k) {
                    args.push("-i".into());
                    args.push(ov.path.as_os_str().to_owned());
                    let p = checked_placement(size, ov.size, opts.aspect_tolerance)?;
                    if p.aspect_mismatch {
                        warnings.push(Warning::OverlayAspectMismatch {
                            overlay: ov.size,
                            target: size,
                            difference: aspect_difference(size, ov.size),
                        });
                    }
                    graph.push(format!("[{input}:v]scale={}:{}[ov{k}]", p.width, p.height));
                    let out_label = format!("[s{k}]");
                    // Half-open span so the frame at a boundary gets one overlay.
                    graph.push(format!(
                        "{label}[ov{k}]overlay={}:{}:eof_action=repeat:enable='gte(t,{t0:.6})*lt(t,{t1:.6})'{out_label}",
                        p.x, p.y
                    ));
                    label = out_label;
                    input += 1;
                }
                t0 = t1;
            }
            graph.push(format!("{label}format={}[v]", enc.pix_fmt()));
            let filter = graph.join(";");
            args.push("-filter_complex".into());
            args.push(filter.clone().into());
            args.extend(["-map", "[v]", "-fps_mode", "passthrough"].map(OsString::from));
            let kbps = encoder_bitrate_kbps(&infos[0], size, enc, opts);
            args.extend(enc.args(kbps).into_iter().map(OsString::from));
            (filter, size, Some(kbps))
        }
    };
    match &audio {
        AudioPlan::Aac { kbps } => {
            args.extend(["-map", "0:a:0", "-c:a", "aac", "-b:a"].map(OsString::from));
            args.push(format!("{kbps}k").into());
        }
        _ => args.push("-an".into()),
    }
    args.extend(tail_args(out));
    Ok(Plan {
        args,
        timeout: opts.timeout(total),
        filter,
        output_size,
        bitrate_kbps: bitrate_kbps.unwrap_or(0),
        audio,
        encoder,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn clip(id: &str, t: (i32, u32, u32, u32, u32, u32), dur: f64) -> ClipDescriptor {
        ClipDescriptor {
            item_id: id.into(),
            capture_utc: Utc.with_ymd_and_hms(t.0, t.1, t.2, t.3, t.4, t.5).unwrap(),
            is_video: true,
            duration_secs: dur,
            width: 1080,
            height: 1920,
            codec: "avc1".into(),
            fps: Some(30.0),
            location: None,
        }
    }

    fn ids(groups: &[ClipGroup]) -> Vec<Vec<&str>> {
        groups
            .iter()
            .map(|g| g.item_ids.iter().map(String::as_str).collect())
            .collect()
    }

    #[test]
    fn three_clips_with_one_second_gaps_join() {
        let items = vec![
            clip("a", (2019, 6, 1, 12, 0, 0), 10.0),
            clip("b", (2019, 6, 1, 12, 0, 11), 10.0),
            clip("c", (2019, 6, 1, 12, 0, 22), 4.0),
        ];
        let g = detect_clip_groups(&items, &RejoinOptions::default());
        assert_eq!(ids(&g), vec![vec!["a", "b", "c"]]);
        assert!((g[0].total_secs - 24.0).abs() < 1e-9);
        assert_eq!(g[0].gaps_secs, vec![1.0, 1.0]);
    }

    #[test]
    fn decoys_are_not_joined() {
        let mut other_res = clip("d", (2019, 6, 1, 12, 0, 33), 10.0);
        other_res.width = 720;
        other_res.height = 1280;
        let items = vec![
            clip("a", (2019, 6, 1, 12, 0, 0), 10.0),
            clip("b", (2019, 6, 1, 12, 0, 11), 10.0),
            clip("c", (2019, 6, 1, 12, 0, 22), 10.0),
            other_res,                                  // 1 s after c, other size
            clip("far", (2019, 6, 1, 12, 0, 50), 10.0), // 18 s after c
        ];
        let g = detect_clip_groups(&items, &RejoinOptions::default());
        assert_eq!(ids(&g), vec![vec!["a", "b", "c"]]);
        let o = RejoinOptions::default();
        assert_eq!(
            can_follow(&items[2], &items[3], &o),
            Err(NotJoined::Resolution)
        );
        assert!(matches!(
            can_follow(&items[3], &items[4], &o),
            Err(NotJoined::Resolution)
        ));
    }

    #[test]
    fn interleaved_decoy_does_not_break_a_group() {
        let mut decoy = clip("x", (2019, 6, 1, 12, 0, 10), 10.0);
        decoy.codec = "hvc1".into();
        let items = vec![
            clip("a", (2019, 6, 1, 12, 0, 0), 10.0),
            decoy,
            clip("b", (2019, 6, 1, 12, 0, 11), 10.0),
        ];
        assert_eq!(
            ids(&detect_clip_groups(&items, &RejoinOptions::default())),
            vec![vec!["a", "b"]]
        );
    }

    #[test]
    fn short_clip_ends_a_group() {
        let items = vec![
            clip("a", (2020, 1, 1, 0, 0, 0), 10.0),
            clip("b", (2020, 1, 1, 0, 0, 11), 6.0),
            clip("c", (2020, 1, 1, 0, 0, 18), 10.0),
        ];
        let g = detect_clip_groups(&items, &RejoinOptions::default());
        assert_eq!(ids(&g), vec![vec!["a", "b"]]);
        assert_eq!(
            can_follow(&items[1], &items[2], &RejoinOptions::default()),
            Err(NotJoined::TooShort { secs: 6.0 })
        );
    }

    #[test]
    fn year_range_and_all_years() {
        let items = vec![
            clip("a", (2023, 3, 1, 9, 0, 0), 10.0),
            clip("b", (2023, 3, 1, 9, 0, 11), 10.0),
        ];
        assert!(detect_clip_groups(&items, &RejoinOptions::default()).is_empty());
        let all = RejoinOptions {
            years: YearRange::All,
            ..RejoinOptions::default()
        };
        assert_eq!(ids(&detect_clip_groups(&items, &all)), vec![vec!["a", "b"]]);
        // Both clips must be in range: across New Year 2022 nothing joins.
        let edge = vec![
            clip("y", (2021, 12, 31, 23, 59, 49), 10.0),
            clip("z", (2022, 1, 1, 0, 0, 0), 10.0),
        ];
        assert!(detect_clip_groups(&edge, &RejoinOptions::default()).is_empty());
        assert_eq!(ids(&detect_clip_groups(&edge, &all)), vec![vec!["y", "z"]]);
    }

    #[test]
    fn gap_overlap_fps_and_location() {
        let o = RejoinOptions::default();
        let a = clip("a", (2018, 5, 5, 5, 5, 0), 10.0);
        let at = |s: u32| clip("n", (2018, 5, 5, 5, 5, s), 10.0);
        assert!(can_follow(&a, &at(12), &o).is_ok(), "2 s gap is allowed");
        assert_eq!(
            can_follow(&a, &at(13), &o),
            Err(NotJoined::Gap { secs: 3.0 })
        );
        assert!(
            can_follow(&a, &at(9), &o).is_ok(),
            "1 s overlap from whole-second dates"
        );
        assert!(can_follow(&a, &at(8), &o).is_err());
        let mut f = at(11);
        f.fps = Some(25.0);
        assert_eq!(can_follow(&a, &f, &o), Err(NotJoined::FrameRate));
        let mut here = a.clone();
        here.location = Some((51.5074, -0.1278));
        let mut near = at(11);
        near.location = Some((51.5078, -0.1280)); // ~47 m
        assert!(can_follow(&here, &near, &o).is_ok());
        near.location = Some((51.5174, -0.1278)); // ~1.1 km
        assert_eq!(can_follow(&here, &near, &o), Err(NotJoined::Location));
        assert_eq!(can_follow(&here, &at(11), &o), Err(NotJoined::Location));
        let mut photo = at(11);
        photo.is_video = false;
        assert_eq!(can_follow(&a, &photo, &o), Err(NotJoined::NotVideo));
    }

    #[test]
    fn six_segment_group_and_order_independence() {
        let mut items: Vec<ClipDescriptor> = (0..6)
            .map(|k| {
                clip(
                    &format!("s{k}"),
                    (2017, 8, 8, 8, 0, k * 11),
                    if k == 5 { 3.0 } else { 10.0 },
                )
            })
            .collect();
        items.reverse();
        let g = detect_clip_groups(&items, &RejoinOptions::default());
        assert_eq!(ids(&g), vec![vec!["s0", "s1", "s2", "s3", "s4", "s5"]]);
        assert!((g[0].total_secs - 53.0).abs() < 1e-9);
    }

    #[test]
    fn concat_list_quotes_paths() {
        let l = concat_list(&[Path::new(r"C:\a b\it's.mp4"), Path::new("/x/y.mp4")]);
        assert_eq!(
            l,
            "ffconcat version 1.0\nfile 'C:/a b/it'\\''s.mp4'\nfile '/x/y.mp4'\n"
        );
    }

    #[test]
    fn stream_copy_plan() {
        let info = crate::video::tests::video_info("avc1", (1080, 1920), 0, 5000, Some("aac"));
        let infos = vec![info.clone(), info.clone(), info];
        let p = rejoin_plan(
            Path::new("list.ffconcat"),
            &infos,
            &[None, None, None],
            Path::new("joined.mp4"),
            None,
            &VideoOptions::default(),
        )
        .unwrap();
        let a = p.args_lossy();
        let i = a.iter().position(|x| x == "concat").unwrap();
        assert_eq!(&a[i - 1..i + 4], ["-f", "concat", "-safe", "0", "-i"]);
        assert!(a.windows(2).any(|w| w == ["-c:v", "copy"]));
        assert!(a.windows(2).any(|w| w == ["-c:a", "aac"]));
        assert!(!a.iter().any(|x| x == "-filter_complex"));
        assert!(a.contains(&"-copyts".to_string()));
        assert_eq!(p.encoder, None);
        assert_eq!(
            p.timeout,
            std::time::Duration::from_secs_f64(37.5 * 10.0 + 60.0)
        );
    }

    #[test]
    fn overlay_plan_enables_each_overlay_on_its_span() {
        let info = crate::video::tests::video_info("avc1", (1920, 1080), 90, 5000, Some("aac"));
        let infos = vec![info.clone(), info.clone(), info];
        let ov0 = PathBuf::from("o0.png");
        let ov2 = PathBuf::from("o2.png");
        let overlays = [
            Some(OverlayInput {
                path: &ov0,
                size: (1080, 1920),
            }),
            None,
            Some(OverlayInput {
                path: &ov2,
                size: (720, 1280),
            }),
        ];
        let p = rejoin_plan(
            Path::new("list.ffconcat"),
            &infos,
            &overlays,
            Path::new("joined.mp4"),
            Some(VideoEncoder::MediaFoundation { hardware: false }),
            &VideoOptions::default(),
        )
        .unwrap();
        assert_eq!(
            p.filter,
            "[0:v]transpose=clock[base];\
             [1:v]scale=1080:1920[ov0];\
             [base][ov0]overlay=0:0:eof_action=repeat:enable='gte(t,0.000000)*lt(t,12.500000)'[s0];\
             [2:v]scale=1080:1920[ov2];\
             [s0][ov2]overlay=0:0:eof_action=repeat:enable='gte(t,25.000000)*lt(t,37.500000)'[s2];\
             [s2]format=nv12[v]"
        );
        let a = p.args_lossy();
        assert_eq!(a.iter().filter(|x| *x == "-i").count(), 3);
        assert!(
            a.windows(3)
                .any(|w| w == ["-noautorotate", "-display_rotation", "0"])
        );
        assert!(
            rejoin_plan(
                Path::new("l"),
                &infos,
                &overlays,
                Path::new("o.mp4"),
                None,
                &VideoOptions::default()
            )
            .is_err()
        );
    }
}
