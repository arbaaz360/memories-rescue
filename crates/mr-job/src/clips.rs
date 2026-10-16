//! Media facts the dry run needs (FR-27 clip groups, FR-51 HEVC notice),
//! read from the export without extracting anything: each video entry is
//! streamed only until its `moov` box has been parsed.

use mr_composite::{ClipDescriptor, RejoinOptions, YearRange, detect_clip_groups};
use mr_core::discover::Discovery;

use crate::layout;
use crate::media::{is_hevc, part_ids, probe_source};
use crate::options::Options;
use crate::plan::{ClipGroup, PlannedItem, Precision, Warning};

/// Longest single clip worth considering (Snapchat recorded up to 60 s).
const MAX_CLIP_SECS: i64 = 61;
/// Bytes a probe may read before `moov` when the file keeps it at the end.
const CLIP_PROBE_LIMIT: u64 = 256 << 20;
/// For the HEVC sample a faststart file is enough; `moov` at the end is skipped.
const SAMPLE_PROBE_LIMIT: u64 = 8 << 20;
const HEVC_SAMPLE: usize = 16;

fn rejoin_options(options: &Options) -> RejoinOptions {
    RejoinOptions {
        years: match options.clip_rejoin.year_range {
            Some((first, last)) => YearRange::Years { first, last },
            None => YearRange::All,
        },
        ..RejoinOptions::default()
    }
}

/// `2019/07/2019-07-14_182000_ABCD.mp4` -> `2019/07/2019-07-14_182000_ABCD-joined.mp4`.
pub fn joined_name(first_output_rel: &str) -> String {
    let (folder, name) = layout::split_rel(first_output_rel);
    let joined = match name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}-joined.{ext}"),
        None => format!("{name}-joined"),
    };
    layout::join_rel(folder, &joined)
}

/// FR-27, section 4.5: the clip groups of this export, in capture order.
/// Only videos with an exact time, inside the year range, with another
/// video starting within a minute are probed.
pub fn detect(items: &[PlannedItem], d: &Discovery, options: &Options) -> Vec<ClipGroup> {
    if !options.clip_rejoin.enabled {
        return Vec::new();
    }
    let opts = rejoin_options(options);
    let in_years = |i: &PlannedItem| match opts.years {
        YearRange::All => true,
        YearRange::Years { first, last } => {
            (first..=last).contains(&chrono::Datelike::year(&i.capture_utc))
        }
    };
    let mut videos: Vec<&PlannedItem> = items
        .iter()
        .filter(|i| {
            i.is_video()
                && i.precision == Precision::Second
                && matches!(i.ext.as_str(), "mp4" | "mov" | "m4v")
                && in_years(i)
        })
        .collect();
    videos.sort_by_key(|i| (i.capture_utc, i.key.clone()));
    let near = |a: &PlannedItem, b: &PlannedItem| {
        (b.capture_utc - a.capture_utc).num_seconds().abs() <= MAX_CLIP_SECS + 2
    };
    let candidates: Vec<&PlannedItem> = videos
        .iter()
        .enumerate()
        .filter(|(k, v)| {
            (*k > 0 && near(videos[k - 1], v)) || videos.get(k + 1).is_some_and(|n| near(v, n))
        })
        .map(|(_, v)| *v)
        .collect();
    if candidates.len() < 2 {
        return Vec::new();
    }
    let parts = part_ids(d);
    let descriptors: Vec<ClipDescriptor> = candidates
        .iter()
        .filter_map(|i| {
            let info = probe_source(d, &parts, &i.main, CLIP_PROBE_LIMIT)?;
            ClipDescriptor::from_probe(&i.key, i.capture_utc, i.gps, &info)
        })
        .collect();
    detect_clip_groups(&descriptors, &opts)
        .into_iter()
        .map(|g| {
            let first = items
                .iter()
                .find(|i| i.key == g.item_ids[0])
                .expect("group members are planned items");
            ClipGroup {
                id: g.item_ids[0].clone(),
                enabled: !options.clip_rejoin.skip_groups.contains(&g.item_ids[0]),
                start_utc: Some(g.start_utc),
                total_secs: g.total_secs,
                output_rel: joined_name(&first.output_rel),
                items: g.item_ids,
            }
        })
        .collect()
}

/// FR-51: a notice when the export has HEVC videos and this computer cannot
/// play HEVC, unless the compatibility mode is already on. Checks the
/// machine first (cheap; always playable on macOS), then a sample of up to
/// 16 videos spread over the export.
pub fn hevc_notice(items: &[PlannedItem], d: &Discovery, options: &Options) -> Option<Warning> {
    if options.windows_compat {
        return None;
    }
    let unplayable =
        options.test_hooks.hevc_unplayable() || !mr_composite::hevc_playback_available();
    if !unplayable {
        return None;
    }
    let videos: Vec<&PlannedItem> = items.iter().filter(|i| i.is_video()).collect();
    if videos.is_empty() {
        return None;
    }
    let step = videos.len().div_ceil(HEVC_SAMPLE).max(1);
    let parts = part_ids(d);
    let sample: Vec<&PlannedItem> = videos.iter().step_by(step).copied().collect();
    let hevc = sample
        .iter()
        .filter(|i| {
            probe_source(d, &parts, &i.main, SAMPLE_PROBE_LIMIT).is_some_and(|m| is_hevc(&m))
        })
        .count();
    (hevc > 0).then(|| Warning {
        code: "HEVC_PLAYBACK".into(),
        detail: format!(
            "{hevc} of {} sampled videos are HEVC, which this computer cannot play without Microsoft's HEVC Video Extensions. Turn on \"Maximise Windows compatibility\" to save them as H.264.",
            sample.len()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_names() {
        assert_eq!(
            joined_name("2019/07/2019-07-14_182000_ABCD1234.mp4"),
            "2019/07/2019-07-14_182000_ABCD1234-joined.mp4"
        );
        assert_eq!(joined_name("a.mov"), "a-joined.mov");
    }
}
