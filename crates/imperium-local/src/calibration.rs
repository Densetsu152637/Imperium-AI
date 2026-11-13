//! Memory estimates calibrated against measured use.
//!
//! The planner's estimate ([`crate::planner::Footprint`]) is static: weights, a KV cache
//! computed from the model's shape, and a fixed runtime overhead. Real servers differ: compute
//! buffers grow with batch size, a runtime pack may allocate more or less than another, a driver
//! reserves its own share. A benchmark run reads the model's server process while it loads and
//! answers prompts and reports its peak memory; [`Calibration`] compares that peak with what was
//! planned for the same placement and keeps the ratio, per model and placement, in a file for one
//! hardware fingerprint (the machine, its GPUs and the runtime pack).
//!
//! The planner then moves the footprint's overhead by the ratio ([`factor_permille`]), bounded
//! so a stale or noisy measurement cannot do much harm: the estimate never goes below
//! [`MIN_FACTOR_PERMILLE`] of the static one (a measured saving is taken only in part) nor above
//! [`MAX_FACTOR_PERMILLE`] of it. Nothing measured: no change.
//!
//! [`factor_permille`]: Calibration::factor_permille

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

/// The static estimate: no correction.
pub const NEUTRAL_PERMILLE: u32 = 1000;
/// The lowest factor ever applied: planning at most 5% under the static estimate.
pub const MIN_FACTOR_PERMILLE: u32 = 950;
/// The highest factor ever applied: planning at most 50% over the static estimate.
pub const MAX_FACTOR_PERMILLE: u32 = 1500;
/// Entries kept per hardware fingerprint; the least recently measured go first.
const MAX_ENTRIES: usize = 256;
const VERSION: u32 = 1;

/// Where a model ran, as far as memory estimates are concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Every layer in GPU memory (or in the shared memory of a unified-memory machine).
    Gpu,
    /// Some layers on the GPU, the rest in RAM.
    Partial,
    /// All in RAM.
    Cpu,
}

impl Lane {
    pub fn as_str(self) -> &'static str {
        match self {
            Lane::Gpu => "gpu",
            Lane::Partial => "partial",
            Lane::Cpu => "cpu",
        }
    }
}

/// Keeps a factor within what the planner may apply.
pub fn clamp_factor(permille: u32) -> u32 {
    permille.clamp(MIN_FACTOR_PERMILLE, MAX_FACTOR_PERMILLE)
}

/// What a measurement says about the estimate for one model in one lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Measured peak over planned, in thousandths, smoothed over the measurements so far and
    /// clamped to the bounds.
    pub factor_permille: u32,
    /// The latest planned memory (VRAM plus RAM), in bytes.
    pub planned_bytes: u64,
    /// The latest measured peak, in bytes.
    pub measured_bytes: u64,
    pub samples: u32,
    /// Seconds since the epoch of the latest measurement; orders entries for pruning.
    pub updated_at: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    version: u32,
    fingerprint: String,
    entries: BTreeMap<String, Entry>,
}

/// The measured corrections for one hardware fingerprint.
#[derive(Debug)]
pub struct Calibration {
    /// `None` keeps the entries in memory only.
    path: Option<PathBuf>,
    fingerprint: String,
    entries: Mutex<BTreeMap<String, Entry>>,
}

fn key(model_id: &str, lane: Lane) -> String {
    format!("{model_id}\u{1f}{}", lane.as_str())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Calibration {
    /// Corrections kept only in memory.
    pub fn in_memory(fingerprint: &str) -> Self {
        Calibration {
            path: None,
            fingerprint: fingerprint.to_string(),
            entries: Mutex::default(),
        }
    }

    /// The corrections saved in `dir` for `fingerprint`. A missing, unreadable or foreign file
    /// starts empty: a calibration is only ever an optimisation, never a reason to fail.
    pub fn open(dir: &Path, fingerprint: &str) -> Self {
        let name: String = fingerprint
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        let path = dir.join(format!("{name}.json"));
        let entries: BTreeMap<String, Entry> = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<File>(&text).ok())
            .filter(|file| file.version == VERSION && file.fingerprint == fingerprint)
            .map(|file| {
                file.entries
                    .into_iter()
                    .map(|(key, mut entry)| {
                        // A hand-edited or corrupted file must not widen the bounds.
                        entry.factor_permille = clamp_factor(entry.factor_permille);
                        (key, entry)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Calibration {
            path: Some(path),
            fingerprint: fingerprint.to_string(),
            entries: Mutex::new(entries),
        }
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The factor to plan `model_id` with, in thousandths: 1000 when nothing was measured. Off
    /// the CPU lane the larger of the GPU and partial entries counts, since the planner does
    /// not know which of the two the model will get.
    pub fn factor_permille(&self, model_id: &str, cpu: bool) -> u32 {
        let entries = lock(&self.entries);
        let lanes: &[Lane] = if cpu {
            &[Lane::Cpu]
        } else {
            &[Lane::Gpu, Lane::Partial]
        };
        lanes
            .iter()
            .filter_map(|lane| entries.get(&key(model_id, *lane)))
            .map(|entry| entry.factor_permille)
            .max()
            .map_or(NEUTRAL_PERMILLE, clamp_factor)
    }

    /// The entry for `model_id` in `lane`, for display and tests.
    pub fn entry(&self, model_id: &str, lane: Lane) -> Option<Entry> {
        lock(&self.entries).get(&key(model_id, lane)).cloned()
    }

    /// Records a measured peak against the memory planned for it, and saves. Ignored when either
    /// is zero. Returns the factor now in force for the model and lane.
    pub fn record(
        &self,
        model_id: &str,
        lane: Lane,
        planned_bytes: u64,
        measured_bytes: u64,
        now: u64,
    ) -> Option<u32> {
        if planned_bytes == 0 || measured_bytes == 0 {
            return None;
        }
        let ratio = u32::try_from(
            u128::from(measured_bytes)
                .saturating_mul(1000)
                .checked_div(u128::from(planned_bytes))?,
        )
        .unwrap_or(u32::MAX);
        let ratio = clamp_factor(ratio);
        let (factor, snapshot) = {
            let mut entries = lock(&self.entries);
            let entry = entries.entry(key(model_id, lane)).or_insert(Entry {
                factor_permille: ratio,
                planned_bytes,
                measured_bytes,
                samples: 0,
                updated_at: now,
            });
            // Smoothed, but a higher reading counts in full: a model that needed more than was
            // planned must not be planned for less next time because one run was quiet.
            entry.factor_permille = if entry.samples == 0 {
                ratio
            } else {
                clamp_factor(((entry.factor_permille + ratio) / 2).max(ratio))
            };
            entry.planned_bytes = planned_bytes;
            entry.measured_bytes = measured_bytes;
            entry.samples = entry.samples.saturating_add(1);
            entry.updated_at = now;
            let factor = entry.factor_permille;
            prune(&mut entries);
            (factor, entries.clone())
        };
        if let Err(error) = self.save(&snapshot) {
            tracing::warn!(%error, "could not save the memory calibration");
        }
        Some(factor)
    }

    fn save(&self, entries: &BTreeMap<String, Entry>) -> io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(&File {
            version: VERSION,
            fingerprint: self.fingerprint.clone(),
            entries: entries.clone(),
        })
        .map_err(io::Error::other)?;
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, text)?;
        fs::rename(&temporary, path)
    }
}

fn prune(entries: &mut BTreeMap<String, Entry>) {
    while entries.len() > MAX_ENTRIES {
        let Some(oldest) = entries
            .iter()
            .min_by_key(|(_, entry)| entry.updated_at)
            .map(|(key, _)| key.clone())
        else {
            return;
        };
        entries.remove(&oldest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn nothing_measured_leaves_the_estimate_alone() {
        let calibration = Calibration::in_memory("abc");
        assert_eq!(calibration.factor_permille("m", false), NEUTRAL_PERMILLE);
        assert_eq!(calibration.factor_permille("m", true), NEUTRAL_PERMILLE);
    }

    #[test]
    fn a_measurement_sets_a_factor_within_the_bounds() {
        let calibration = Calibration::in_memory("abc");
        // 11 GiB measured against 10 GiB planned.
        let factor = calibration.record("m", Lane::Gpu, 10 * GIB, 11 * GIB, 1);
        assert_eq!(factor, Some(1100));
        assert_eq!(calibration.factor_permille("m", false), 1100);
        // The CPU lane has its own entry.
        assert_eq!(calibration.factor_permille("m", true), NEUTRAL_PERMILLE);
        // A wild reading is capped; a tiny one is raised to the floor.
        calibration.record("big", Lane::Gpu, GIB, 40 * GIB, 1);
        assert_eq!(
            calibration.factor_permille("big", false),
            MAX_FACTOR_PERMILLE
        );
        calibration.record("small", Lane::Cpu, 10 * GIB, GIB, 1);
        assert_eq!(
            calibration.factor_permille("small", true),
            MIN_FACTOR_PERMILLE
        );
    }

    #[test]
    fn an_empty_measurement_is_ignored() {
        let calibration = Calibration::in_memory("abc");
        assert_eq!(calibration.record("m", Lane::Gpu, 0, GIB, 1), None);
        assert_eq!(calibration.record("m", Lane::Gpu, GIB, 0, 1), None);
        assert!(calibration.entry("m", Lane::Gpu).is_none());
    }

    #[test]
    fn a_higher_reading_counts_in_full_and_a_lower_one_only_in_part() {
        let calibration = Calibration::in_memory("abc");
        calibration.record("m", Lane::Gpu, 10 * GIB, 11 * GIB, 1);
        assert_eq!(calibration.factor_permille("m", false), 1100);
        // A quiet run: 1.0 against 1.1 averages to 1.05.
        calibration.record("m", Lane::Gpu, 10 * GIB, 10 * GIB, 2);
        assert_eq!(calibration.factor_permille("m", false), 1050);
        // A busy one takes the factor straight to it.
        calibration.record("m", Lane::Gpu, 10 * GIB, 13 * GIB, 3);
        assert_eq!(calibration.factor_permille("m", false), 1300);
        assert_eq!(
            calibration.entry("m", Lane::Gpu).map(|e| e.samples),
            Some(3)
        );
    }

    #[test]
    fn off_the_cpu_lane_the_larger_of_gpu_and_partial_counts() {
        let calibration = Calibration::in_memory("abc");
        calibration.record("m", Lane::Gpu, 10 * GIB, 10 * GIB, 1);
        calibration.record("m", Lane::Partial, 10 * GIB, 12 * GIB, 1);
        assert_eq!(calibration.factor_permille("m", false), 1200);
    }

    #[test]
    fn corrections_survive_a_restart_and_belong_to_one_fingerprint() {
        let dir = tempfile::tempdir().expect("temp dir");
        let calibration = Calibration::open(dir.path(), "fp1");
        calibration.record("m", Lane::Gpu, 10 * GIB, 12 * GIB, 7);
        let reopened = Calibration::open(dir.path(), "fp1");
        assert_eq!(reopened.factor_permille("m", false), 1200);
        // Another machine (another fingerprint) has its own file and starts empty.
        assert_eq!(
            Calibration::open(dir.path(), "fp2").factor_permille("m", false),
            NEUTRAL_PERMILLE
        );
    }

    #[test]
    fn a_foreign_or_corrupt_file_is_ignored_and_a_wide_factor_is_clamped() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("fp1.json"), "not json").expect("write");
        assert_eq!(
            Calibration::open(dir.path(), "fp1").factor_permille("m", false),
            NEUTRAL_PERMILLE
        );
        let entry = |factor_permille| Entry {
            factor_permille,
            planned_bytes: 1,
            measured_bytes: 1,
            samples: 1,
            updated_at: 1,
        };
        let wide = File {
            version: VERSION,
            fingerprint: "fp1".into(),
            entries: BTreeMap::from([
                (key("m", Lane::Gpu), entry(9000)),
                (key("l", Lane::Gpu), entry(10)),
            ]),
        };
        fs::write(
            dir.path().join("fp1.json"),
            serde_json::to_string(&wide).expect("json"),
        )
        .expect("write");
        let calibration = Calibration::open(dir.path(), "fp1");
        assert_eq!(calibration.factor_permille("m", false), MAX_FACTOR_PERMILLE);
        assert_eq!(calibration.factor_permille("l", false), MIN_FACTOR_PERMILLE);
    }

    #[test]
    fn the_oldest_entries_are_dropped_past_the_limit() {
        let calibration = Calibration::in_memory("abc");
        let limit = u64::try_from(MAX_ENTRIES).unwrap_or(0);
        for i in 0..(limit + 5) {
            calibration.record(&format!("m{i}"), Lane::Gpu, GIB, GIB, i);
        }
        assert!(calibration.entry("m0", Lane::Gpu).is_none());
        assert!(
            calibration
                .entry(&format!("m{}", limit + 4), Lane::Gpu)
                .is_some()
        );
    }
}
