//! Disk-space precheck (FR-28).
//!
//! Required space = final output + originals (if kept) + peak temp
//! (largest item x workers x 2) + optional ZIP. The run is blocked when free
//! space is below required x 1.1, and paused while running when free space
//! drops under 2 GB (`DISK_FULL_IMMINENT`).

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Free space below which a running job pauses (FR-28 AC2).
pub const MIN_FREE_WHILE_RUNNING: u64 = 2 * 1024 * 1024 * 1024;
/// Safety factor on the estimate (FR-28 AC1).
pub const SAFETY_FACTOR: f64 = 1.1;
/// Metadata a writer adds to one file, generously.
pub const METADATA_OVERHEAD: u64 = 4 * 1024;
/// ZIP header bytes per packed file, generously (a long name twice plus
/// `zipout::ENTRY_OVERHEAD`).
pub const ZIP_ENTRY_ALLOWANCE: u64 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceAction {
    ChangeDestination,
    DisableKeepOriginals,
    DisableZip,
}

/// One way out of a shortfall, with the bytes it would save.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceOption {
    pub action: SpaceAction,
    pub saves_bytes: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInputs {
    /// Sum of the files that will be written as outputs (mains, overlays saved).
    pub final_bytes: u64,
    /// Sum of what `originals/` would hold.
    pub originals_bytes: u64,
    /// Largest single item (main + overlay).
    pub largest_item: u64,
    pub workers: usize,
    pub zip_output: bool,
    /// Files the ZIP will hold from this run (for the header allowance).
    #[serde(default)]
    pub zip_entries: u64,
    /// Outputs written by earlier runs that the ZIP must hold too (FR-31
    /// packs the whole job, not only this run's items).
    #[serde(default)]
    pub zip_carry_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpaceEstimate {
    pub final_bytes: u64,
    pub originals_bytes: u64,
    pub peak_temp_bytes: u64,
    pub zip_bytes: u64,
    pub required_bytes: u64,
    /// `required x 1.1`.
    pub threshold_bytes: u64,
    /// `None` when the destination volume could not be queried.
    pub free_bytes: Option<u64>,
    pub sufficient: bool,
    /// How many bytes are missing to reach the threshold.
    pub shortfall_bytes: u64,
    pub options: Vec<SpaceOption>,
}

impl SpaceEstimate {
    pub fn compute(inputs: &SpaceInputs, free_bytes: Option<u64>) -> SpaceEstimate {
        let peak = inputs
            .largest_item
            .saturating_mul(inputs.workers.max(1) as u64)
            .saturating_mul(2);
        let zip = if inputs.zip_output {
            inputs.final_bytes
                + inputs.originals_bytes
                + inputs.zip_carry_bytes
                + inputs.zip_entries * ZIP_ENTRY_ALLOWANCE
        } else {
            0
        };
        let required = inputs.final_bytes + inputs.originals_bytes + peak + zip;
        // required x 1.1, rounded up, in integers (no float drift at the boundary).
        let threshold = required + required.div_ceil(10);
        let (sufficient, shortfall) = match free_bytes {
            Some(free) if free < threshold => (false, threshold - free),
            _ => (true, 0),
        };
        let mut options = Vec::new();
        if !sufficient {
            options.push(SpaceOption {
                action: SpaceAction::ChangeDestination,
                saves_bytes: 0,
            });
            if inputs.originals_bytes > 0 {
                let saved = inputs.originals_bytes * if inputs.zip_output { 2 } else { 1 };
                options.push(SpaceOption {
                    action: SpaceAction::DisableKeepOriginals,
                    saves_bytes: saved + saved.div_ceil(10),
                });
            }
            if zip > 0 {
                options.push(SpaceOption {
                    action: SpaceAction::DisableZip,
                    saves_bytes: zip + zip.div_ceil(10),
                });
            }
        }
        SpaceEstimate {
            final_bytes: inputs.final_bytes,
            originals_bytes: inputs.originals_bytes,
            peak_temp_bytes: peak,
            zip_bytes: zip,
            required_bytes: required,
            threshold_bytes: threshold,
            free_bytes,
            sufficient,
            shortfall_bytes: shortfall,
            options,
        }
    }
}

/// Free bytes available to this user on the volume holding `path` (or its
/// nearest existing ancestor).
pub fn available_space(path: &Path) -> Option<u64> {
    let mut p = Some(path);
    while let Some(candidate) = p {
        if candidate.exists() {
            return fs4::available_space(candidate).ok();
        }
        p = candidate.parent();
    }
    None
}

/// Bytes in GB with one decimal, for messages; small amounts in MB or KB.
pub fn gb(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).ceil())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zip_counts_earlier_outputs_and_headers() {
        let inputs = SpaceInputs {
            final_bytes: 10_000,
            originals_bytes: 2_000,
            largest_item: 500,
            workers: 1,
            zip_output: true,
            zip_entries: 3,
            zip_carry_bytes: 50_000,
        };
        let e = SpaceEstimate::compute(&inputs, None);
        assert_eq!(e.zip_bytes, 12_000 + 50_000 + 3 * ZIP_ENTRY_ALLOWANCE);
        let off = SpaceEstimate::compute(
            &SpaceInputs {
                zip_output: false,
                ..inputs
            },
            None,
        );
        assert_eq!(off.zip_bytes, 0, "no ZIP, no carry");
    }

    #[test]
    fn formula_and_shortfall_options() {
        let inputs = SpaceInputs {
            final_bytes: 10_000,
            originals_bytes: 2_000,
            largest_item: 500,
            workers: 4,
            zip_output: true,
            ..SpaceInputs::default()
        };
        let enough = SpaceEstimate::compute(&inputs, Some(1_000_000));
        assert_eq!(enough.peak_temp_bytes, 4_000);
        assert_eq!(enough.zip_bytes, 12_000);
        assert_eq!(enough.required_bytes, 28_000);
        assert_eq!(enough.threshold_bytes, 30_800);
        assert!(enough.sufficient && enough.options.is_empty());

        let short = SpaceEstimate::compute(&inputs, Some(25_000));
        assert!(!short.sufficient);
        assert_eq!(short.shortfall_bytes, 5_800);
        let actions: Vec<SpaceAction> = short.options.iter().map(|o| o.action).collect();
        assert_eq!(
            actions,
            [
                SpaceAction::ChangeDestination,
                SpaceAction::DisableKeepOriginals,
                SpaceAction::DisableZip
            ]
        );
        // Exactly at the threshold is enough; one byte less is not.
        assert!(SpaceEstimate::compute(&inputs, Some(30_800)).sufficient);
        assert!(!SpaceEstimate::compute(&inputs, Some(30_799)).sufficient);
        assert!(
            SpaceEstimate::compute(&inputs, None).sufficient,
            "unknown free space never blocks"
        );
    }

    #[test]
    fn free_space_of_a_missing_path_uses_its_ancestor() {
        let p = std::env::temp_dir()
            .join("mr-job-space-does-not-exist")
            .join("deeper");
        assert!(available_space(&p).is_some_and(|b| b > 0));
    }
}
