//! Building bounded, order-preserving demo batches.
//!
//! The parser processes one demo per parsing thread, but demo memory costs vary a great deal.
//! A single global worst-case size makes every batch as small as the largest source in the run.
//! Instead, auto-sized batches below are contiguous and budget the *sum* of their individual
//! estimates. A giant demo therefore constrains only its own singleton batch.

use std::path::{Path, PathBuf};

/// Fraction of currently-available RAM we are willing to fill with in-flight demos.
/// The rest is left for the OS page cache, DuckDB, and the parse working set.
const MEMORY_HEADROOM: f64 = 0.6;

/// Compressed demos expand when decompressed. CS2 `.gz`/`.zst` demos typically expand
/// around 3-4x, so a compressed file's on-disk size understates its memory cost.
const COMPRESSED_EXPANSION: f64 = 4.0;

/// Parsing allocates on top of the demo bytes (entity state, dataframes, output buffers).
/// Expressed as a multiple of the decompressed demo size.
const TRIM_MEMORY_MULTIPLIER: f64 = 4.0;
const REPLAY_MEMORY_MULTIPLIER: f64 = 16.0;
const REPLAY_MINIMUM_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// A contiguous batch plan plus the decision used to construct it.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchPlan {
    pub batches: Vec<Vec<PathBuf>>,
    pub reason: BatchPlanReason,
}

/// How the batch plan was arrived at, for logging.
#[derive(Debug, Clone, PartialEq)]
pub enum BatchPlanReason {
    /// A positive `max_concurrent_files` is an operator override. It retains the historical
    /// fixed-count behavior (capped at the parsing pool); the operator owns the memory risk.
    Manual {
        batch_size: usize,
        requested: usize,
        clamped: bool,
    },
    /// Auto mode used a summed per-demo memory budget.
    MemoryBudgeted {
        cpu_threads: usize,
        available_bytes: u64,
        budget_bytes: u64,
        oversized_files: usize,
        metadata_failures: usize,
    },
    /// Available memory could not be measured, so auto mode falls back to CPU-sized chunks.
    MemoryUnknown { cpu_threads: usize },
}

impl BatchPlanReason {
    pub fn describe(&self) -> String {
        match self {
            BatchPlanReason::Manual {
                batch_size,
                requested,
                clamped,
            } => {
                if *clamped {
                    format!(
                        "fixed batches of {batch_size} demos (max_concurrent_files requested \
                         {requested}, reduced to the {batch_size} parsing threads available; \
                         manual mode bypasses auto memory sizing)"
                    )
                } else {
                    format!(
                        "fixed batches of {batch_size} demos (pinned by max_concurrent_files; \
                         manual mode bypasses auto memory sizing)"
                    )
                }
            }
            BatchPlanReason::MemoryBudgeted {
                cpu_threads,
                available_bytes,
                budget_bytes,
                oversized_files,
                metadata_failures,
            } => {
                let mut detail = format!(
                    "contiguous batches of at most {cpu_threads} demos and ~{} summed estimate \
                     ({} available × {:.0}% headroom)",
                    human_bytes(*budget_bytes),
                    human_bytes(*available_bytes),
                    MEMORY_HEADROOM * 100.0,
                );
                if *oversized_files > 0 {
                    detail.push_str(&format!("; {oversized_files} oversized source(s) isolated"));
                }
                if *metadata_failures > 0 {
                    detail.push_str(&format!(
                        "; {metadata_failures} source(s) with unreadable metadata isolated"
                    ));
                }
                detail
            }
            BatchPlanReason::MemoryUnknown { cpu_threads } => format!(
                "contiguous batches of at most {cpu_threads} demos (available memory could not \
                 be determined)"
            ),
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if bytes as f64 >= GB {
        format!("{:.1} GB", bytes as f64 / GB)
    } else {
        format!("{:.0} MB", bytes as f64 / MB)
    }
}

fn is_compressed(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("gz") | Some("zst")
    )
}

/// Estimated peak memory for one in-flight demo, from its on-disk size.
pub fn estimate_demo_bytes(path: impl AsRef<Path>) -> Option<u64> {
    estimate_demo_bytes_for_workload(path.as_ref(), false)
}

fn estimate_demo_bytes_for_workload(path: &Path, process_replays: bool) -> Option<u64> {
    let path = path.as_ref();
    let meta = std::fs::metadata(path).ok()?;
    let on_disk = meta.len() as f64;
    let decompressed = if is_compressed(path) {
        on_disk * COMPRESSED_EXPANSION
    } else {
        on_disk
    };
    // Discovery retains entity/dataframe state; replay mode also retains all-player tick
    // data until batch commit. The old 1.5x estimate admitted 16 tournament demos while
    // their actual resident working set exhausted a 64 GiB machine and caused paging.
    let multiplier = if process_replays { REPLAY_MEMORY_MULTIPLIER } else { TRIM_MEMORY_MULTIPLIER };
    let minimum = if process_replays { REPLAY_MINIMUM_BYTES } else { 256 * 1024 * 1024 };
    Some(((decompressed * multiplier) as u64).max(minimum))
}

/// Build contiguous batches without changing discovery order.
///
/// `manual` is the operator override: `None` or `Some(0)` enables auto sizing. In auto
/// mode every batch contains at most `cpu_threads` files and no more than the memory budget
/// in summed estimates. A demo larger than the whole budget is still processed, but alone.
/// A path whose metadata cannot be read is also isolated rather than guessed small.
pub fn build_batch_plan(
    entries: Vec<PathBuf>,
    cpu_threads: usize,
    manual: Option<usize>,
    available_memory: Option<u64>,
) -> BatchPlan {
    build_batch_plan_for_workload(entries, cpu_threads, manual, available_memory, false)
}

pub fn build_batch_plan_for_workload(
    entries: Vec<PathBuf>,
    cpu_threads: usize,
    manual: Option<usize>,
    available_memory: Option<u64>,
    process_replays: bool,
) -> BatchPlan {
    let cpu_threads = cpu_threads.max(1);

    if let Some(requested) = manual.filter(|n| *n > 0) {
        let batch_size = requested.min(cpu_threads).max(1);
        return BatchPlan {
            batches: entries
                .chunks(batch_size)
                .map(|chunk| chunk.to_vec())
                .collect(),
            reason: BatchPlanReason::Manual {
                batch_size,
                requested,
                clamped: batch_size != requested,
            },
        };
    }

    let Some(available_bytes) = available_memory else {
        let fallback_threads = if process_replays { 1 } else { cpu_threads.min(2) };
        return BatchPlan {
            batches: entries
                .chunks(fallback_threads)
                .map(|chunk| chunk.to_vec())
                .collect(),
            reason: BatchPlanReason::MemoryUnknown { cpu_threads: fallback_threads },
        };
    };

    let estimates: Vec<_> = entries.iter().map(|path| estimate_demo_bytes_for_workload(path, process_replays)).collect();
    build_auto_batch_plan(entries, cpu_threads, available_bytes, estimates)
}

fn build_auto_batch_plan<I>(
    entries: Vec<PathBuf>,
    cpu_threads: usize,
    available_bytes: u64,
    estimates: I,
) -> BatchPlan
where
    I: IntoIterator<Item = Option<u64>>,
{
    let budget_bytes = (available_bytes as f64 * MEMORY_HEADROOM) as u64;
    let mut batches = Vec::new();
    let mut current = Vec::with_capacity(cpu_threads);
    let mut current_bytes = 0_u64;
    let mut oversized_files = 0;
    let mut metadata_failures = 0;

    let mut estimates = estimates.into_iter();
    for entry in entries {
        // Keep every discovered entry in the plan even if a future estimator accidentally
        // yields fewer values than paths; treating the missing value as unreadable is safe.
        let estimate = estimates.next().unwrap_or(None);
        let Some(estimate) = estimate else {
            metadata_failures += 1;
            if !current.is_empty() {
                batches.push(std::mem::take(&mut current));
                current_bytes = 0;
            }
            batches.push(vec![entry]);
            continue;
        };

        if estimate > budget_bytes {
            oversized_files += 1;
            if !current.is_empty() {
                batches.push(std::mem::take(&mut current));
                current_bytes = 0;
            }
            batches.push(vec![entry]);
            continue;
        }

        let reaches_file_cap = current.len() == cpu_threads;
        let exceeds_budget = current_bytes.saturating_add(estimate) > budget_bytes;
        if !current.is_empty() && (reaches_file_cap || exceeds_budget) {
            batches.push(std::mem::take(&mut current));
            current_bytes = 0;
        }

        current_bytes = current_bytes.saturating_add(estimate);
        current.push(entry);
    }
    if !current.is_empty() {
        batches.push(current);
    }

    BatchPlan {
        batches,
        reason: BatchPlanReason::MemoryBudgeted {
            cpu_threads,
            available_bytes,
            budget_bytes,
            oversized_files,
            metadata_failures,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;
    const MB: u64 = 1024 * 1024;

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn auto_plan(
        names: &[&str],
        cpu_threads: usize,
        available: u64,
        estimates: &[Option<u64>],
    ) -> BatchPlan {
        build_auto_batch_plan(
            paths(names),
            cpu_threads,
            available,
            estimates.iter().copied(),
        )
    }

    fn batch_names(plan: &BatchPlan) -> Vec<Vec<String>> {
        plan.batches
            .iter()
            .map(|batch| {
                batch
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn auto_batches_are_contiguous_and_budgeted_by_sum() {
        // 1 GiB available gives a 0.6 GiB budget. A global 500 MiB worst case would make
        // every batch singleton, but only the final pair needs to be split here.
        let plan = auto_plan(
            &["small-a", "small-b", "medium", "small-c"],
            4,
            GB,
            &[
                Some(100 * MB),
                Some(100 * MB),
                Some(500 * MB),
                Some(100 * MB),
            ],
        );

        assert_eq!(
            batch_names(&plan),
            vec![vec!["small-a", "small-b"], vec!["medium", "small-c"]]
        );
    }

    #[test]
    fn giant_demo_is_a_singleton_and_does_not_throttle_neighbors() {
        let plan = auto_plan(
            &["small-a", "giant", "small-b", "small-c", "small-d"],
            3,
            GB,
            &[
                Some(100 * MB),
                Some(700 * MB),
                Some(100 * MB),
                Some(100 * MB),
                Some(100 * MB),
            ],
        );

        assert_eq!(
            batch_names(&plan),
            vec![
                vec!["small-a"],
                vec!["giant"],
                vec!["small-b", "small-c", "small-d"],
            ]
        );
        assert!(matches!(
            plan.reason,
            BatchPlanReason::MemoryBudgeted {
                oversized_files: 1,
                ..
            }
        ));
    }

    #[test]
    fn cpu_limit_still_caps_auto_batches() {
        let plan = auto_plan(&["a", "b", "c", "d", "e"], 2, 64 * GB, &[Some(MB); 5]);
        assert_eq!(
            batch_names(&plan),
            vec![vec!["a", "b"], vec!["c", "d"], vec!["e"]]
        );
    }

    #[test]
    fn unreadable_metadata_is_safely_isolated() {
        let plan = auto_plan(
            &["before", "unreadable", "after"],
            4,
            GB,
            &[Some(MB), None, Some(MB)],
        );
        assert_eq!(
            batch_names(&plan),
            vec![vec!["before"], vec!["unreadable"], vec!["after"]]
        );
        assert!(matches!(
            plan.reason,
            BatchPlanReason::MemoryBudgeted {
                metadata_failures: 1,
                ..
            }
        ));
    }

    #[test]
    fn manual_setting_keeps_fixed_count_behavior() {
        let plan = build_batch_plan(paths(&["a", "b", "c", "d", "e"]), 4, Some(3), Some(GB));
        assert_eq!(
            batch_names(&plan),
            vec![vec!["a", "b", "c"], vec!["d", "e"]]
        );
        assert!(matches!(
            plan.reason,
            BatchPlanReason::Manual {
                batch_size: 3,
                clamped: false,
                ..
            }
        ));
    }

    #[test]
    fn manual_setting_is_still_capped_at_parsing_threads() {
        let plan = build_batch_plan(paths(&["a", "b", "c"]), 2, Some(99), Some(GB));
        assert_eq!(batch_names(&plan), vec![vec!["a", "b"], vec!["c"]]);
        assert!(matches!(
            plan.reason,
            BatchPlanReason::Manual {
                requested: 99,
                clamped: true,
                ..
            }
        ));
    }

    #[test]
    fn zero_or_absent_manual_setting_enables_auto() {
        let zero = build_batch_plan(paths(&["missing"]), 2, Some(0), Some(GB));
        let absent = build_batch_plan(paths(&["missing"]), 2, None, Some(GB));
        assert!(matches!(
            zero.reason,
            BatchPlanReason::MemoryBudgeted { .. }
        ));
        assert!(matches!(
            absent.reason,
            BatchPlanReason::MemoryBudgeted { .. }
        ));
    }

    #[test]
    fn unknown_memory_falls_back_to_cpu_sized_chunks() {
        let plan = build_batch_plan(paths(&["a", "b", "c"]), 2, None, None);
        assert_eq!(batch_names(&plan), vec![vec!["a", "b"], vec!["c"]]);
        assert!(matches!(plan.reason, BatchPlanReason::MemoryUnknown { .. }));
    }

    #[test]
    fn compressed_demos_are_estimated_larger_than_their_file_size() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("a.dem");
        std::fs::File::create(&plain).unwrap().set_len(128 * MB).unwrap();
        let packed = dir.path().join("b.dem.gz");
        std::fs::File::create(&packed).unwrap().set_len(128 * MB).unwrap();

        let plain_est = estimate_demo_bytes(&plain).unwrap();
        let packed_est = estimate_demo_bytes(&packed).unwrap();

        assert!(
            packed_est > plain_est,
            "a compressed demo costs more memory than its on-disk size suggests"
        );
    }

    #[test]
    fn replay_workload_does_not_admit_sixteen_small_tournament_demos() {
        let dir = tempfile::tempdir().unwrap();
        let entries: Vec<_> = (0..31).map(|i| {
            let path = dir.path().join(format!("match{i}.dem"));
            std::fs::File::create(&path).unwrap().set_len(300 * MB).unwrap();
            path
        }).collect();
        let plan = build_batch_plan_for_workload(entries, 16, Some(0), Some(36 * GB), true);
        assert!(plan.batches.iter().all(|batch| batch.len() <= 4));
        assert_eq!(plan.batches.iter().map(Vec::len).sum::<usize>(), 31);
        for batch in &plan.batches {
            let estimated: u64 = batch.iter().map(|p| estimate_demo_bytes_for_workload(p, true).unwrap()).sum();
            assert!(estimated <= (36.0 * GB as f64 * MEMORY_HEADROOM) as u64);
        }
    }

    #[test]
    fn replay_minimum_and_unknown_memory_keep_dense_demos_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.dem");
        std::fs::write(&path, [0; 1024]).unwrap();
        assert_eq!(estimate_demo_bytes_for_workload(&path, true), Some(REPLAY_MINIMUM_BYTES));
        let plan = build_batch_plan_for_workload(paths(&["a", "b", "c"]), 16, None, None, true);
        assert!(plan.batches.iter().all(|batch| batch.len() == 1));
    }

    #[test]
    fn missing_demo_has_no_estimate() {
        assert!(estimate_demo_bytes("does-not-exist.dem").is_none());
    }
}
