pub mod round_replay;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use dashmap::DashMap;
use rayon::prelude::*;

use config::AppConfig;
use interface::models::collection::KillCollection;

pub mod batch_processor;
pub mod batch_sizing;
mod catalog_scan;
pub mod collection_buffer;
pub mod collection_converter;
pub mod crash_logger;
pub mod decompression;
pub mod demo_processor;
pub mod fetch_pipeline;
pub use demofetch;
pub mod pipeline_metrics;
pub mod selection;
pub mod tick_by_tick;
pub mod topology;
pub mod trim_pipeline;
pub mod utils;

/// Restrict discovery to one source representation. This avoids a raw `.dem` and its compressed
/// sibling racing to publish the same deterministic `_rN.dem` clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    All,
    Raw,
    Archives,
}

impl InputKind {
    pub(crate) fn accepts(self, path: &std::path::Path) -> bool {
        // Directory discovery already omits generated `_rN.dem` clips that sit beside a raw
        // source or a DemoWriter report. Clip-only folders and explicit clip paths must still
        // parse. `--source-kind raw|archives` keeps excluding them so a mixed source job cannot
        // race a published clip.
        let round_clip = utils::has_round_clip_name(path);
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match self {
            Self::All => true,
            Self::Raw => !round_clip && extension == "dem",
            Self::Archives => !round_clip
                && matches!(extension.as_str(), "gz" | "zst")
                && !utils::has_published_round_clip_sibling(path),
        }
    }
}

use batch_processor::{
    add_batch_collections_to_buffer, recalculate_duckdb_metadata, write_buffered_assets_to_duckdb,
    write_buffered_collections_to_duckdb,
};
use collection_buffer::CollectionBuffer;
use decompression::decompress_batch_async;
use demo_processor::{
    process_single_demo_file_no_masters,
    should_skip_demo_processing,
};
use tick_by_tick::kill_collection_parser::KillCollectionData;
use tokio::task;
use topology::{bind_thread_to_core, get_processor_topology};

/// Event sent during parsing to report progress
#[derive(Debug, Clone)]
pub enum ProgressEvent {
    ReplayProgress {
        filename: String,
        phase: &'static str,
        completed: usize,
        total: usize,
    },
    /// Cumulative wall time / newly completed source demos. Skips and failures are excluded
    /// from the denominator; their overhead remains in elapsed time. Final includes metadata.
    PipelineMetrics {
        completed_demos: usize,
        skipped_demos: usize,
        failed_demos: usize,
        elapsed: f64,
        final_sample: bool,
    },
    PhaseCompleted {
        phase: &'static str,
        elapsed: f64,
    },
    Started {
        total_files: usize,
        total_batches: usize,
    },
    BatchStarted {
        batch_num: usize,
        total_batches: usize,
        file_count: usize,
    },
    BatchCompleted {
        batch_num: usize,
        total_batches: usize,
        demos_count: usize,
        collections_count: usize,
        elapsed: f64,
    },
    DemoProcessed {
        filename: String,
    },
    Error {
        path: PathBuf,
        error: String,
    },
    Finished {
        total_successful: usize,
        total_failed: usize,
        elapsed: f64,
    },

    // New granular progress events
    UnzippedFile {
        filename: String,
    },
    CollectionWritingProgress {
        completed: usize,
        total: usize,
    },
    TickWritingProgress {
        completed: usize,
        total: usize,
    },
}

/// Results from batch processing
pub struct BatchResults {
    pub start_time: Instant,
    pub total_aces: usize,
    pub total_quads: usize,
    pub total_triples: usize,
    pub total_multis: usize,
    pub total_doubles: usize,
    pub errors: Vec<(PathBuf, String)>,
}

struct RunCacheCleanup;
impl Drop for RunCacheCleanup {
    fn drop(&mut self) {
        interface::demo_cache::clear_cache();
    }
}

/// Run the parser on a set of input paths
pub async fn run_parsing<F>(
    input_paths: Vec<PathBuf>,
    config: AppConfig,
    progress_callback: F,
    mut collection_filter: Option<HashMap<PathBuf, Vec<u32>>>,
    asset_refresh_only: bool,
    input_kind: InputKind,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> Result<(
    BatchResults,
    Vec<tick_by_tick::collection_processor::ProcessResult>,
)>
where
    F: Fn(ProgressEvent) + Send + Sync + 'static,
{
    let run_started = Instant::now();
    let _cache_cleanup = RunCacheCleanup;
    let progress_callback = Arc::new(progress_callback);

    let thread_count = config.get_optimal_threads();

    // Detect topology
    let topology = get_processor_topology();
    let use_hybrid = !topology.p_cores.is_empty();

    let (parsing_pool, decompression_pool) = if use_hybrid {
        let mut p_core_logical_masks = topology.p_cores.clone();

        // If manual thread count is set in config (e.g. 12 parsing threads),
        // respect it by limiting usages of P-core logical threads to that count.
        // If config.batch.threads is None (auto), we use all P-core logical threads.
        if let Some(manual_limit) = config.batch.threads {
            if manual_limit > 0 && manual_limit < p_core_logical_masks.len() {
                p_core_logical_masks.truncate(manual_limit);
            }
        }

        let p_core_masks_clone = p_core_logical_masks.clone();
        let parsing_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(p_core_logical_masks.len())
            .start_handler(move |idx| {
                if let Some(mask) = p_core_masks_clone.get(idx) {
                    bind_thread_to_core(*mask);
                }
            })
            .build()
            .unwrap();

        let e_core_logical_masks = topology.e_cores.clone();
        let decompression_pool = if !e_core_logical_masks.is_empty() {
            let e_core_masks_clone = e_core_logical_masks.clone();
            rayon::ThreadPoolBuilder::new()
                .num_threads(e_core_logical_masks.len())
                .start_handler(move |idx| {
                    if let Some(mask) = e_core_masks_clone.get(idx) {
                        bind_thread_to_core(*mask);
                    }
                })
                .build()
                .unwrap()
        } else {
            // Fallback if hybrid but no E-cores found (use generic pool)
            rayon::ThreadPoolBuilder::new()
                .num_threads(thread_count.max(1))
                .build()
                .unwrap()
        };

        (parsing_pool, decompression_pool)
    } else {
        // Non-hybrid (Uniform) Topology:
        // Reserve 1 thread (approx 1 logical core) for decompression to prevent starvation.
        // The rest are used for parsing.
        let parsing_threads = if thread_count > 1 {
            thread_count - 1
        } else {
            1
        };
        let decompression_threads = 1;

        let parsing_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(parsing_threads)
            .build()
            .unwrap();

        let decompression_pool = rayon::ThreadPoolBuilder::new()
            .num_threads(decompression_threads)
            .build()
            .unwrap();

        (parsing_pool, decompression_pool)
    };

    let parsing_pool = Arc::new(parsing_pool);
    let decompression_pool = Arc::new(decompression_pool);
    // Trimming is deliberately isolated from parsing: this bounded pool is created once per run
    // and then reused by every batch, so it cannot nest into or oversubscribe the parsing pool.
    let trim_pool = Arc::new(
        rayon::ThreadPoolBuilder::new()
            .num_threads(trim_pipeline::DEFAULT_TRIM_CONCURRENCY)
            .thread_name(|index| format!("demoparser-trim-{index}"))
            .build()?,
    );

    // Find all demo files
    let entries = utils::find_demo_files_with_catalog(&input_paths, &config.paths.parser_output)?
        .into_iter()
        .filter(|path| input_kind.accepts(path))
        .collect::<Vec<_>>();

    if entries.is_empty() {
        progress_callback(ProgressEvent::PipelineMetrics {
            completed_demos: 0,
            skipped_demos: 0,
            failed_demos: 0,
            elapsed: run_started.elapsed().as_secs_f64(),
            final_sample: true,
        });
        progress_callback(ProgressEvent::Finished {
            total_successful: 0,
            total_failed: 0,
            elapsed: run_started.elapsed().as_secs_f64(),
        });
        return Ok((
            BatchResults {
                start_time: run_started,
                total_aces: 0,
                total_quads: 0,
                total_triples: 0,
                total_multis: 0,
                total_doubles: 0,
                errors: Vec::new(),
            },
            Vec::new(),
        ));
    }

    // Track processing statistics
    let mut total_successful = 0;
    let mut total_failed = 0;
    let mut total_skipped = 0;

    let mut entries = entries;

    // Filter entries based on DuckDB check
    // Only filter if no explicit collection filter is provided.
    // If explicit filter is provided, we assume the caller knows what they want (e.g. reprocessing for tick data).
    if collection_filter.is_none() {
        entries = filter_processed_demos(entries, &config, &mut total_skipped)?;
    }
    entries = crate::round_replay::prefer_saved_rounds(entries, &config, &mut collection_filter);
    progress_callback(ProgressEvent::PhaseCompleted {
        phase: "setup_and_scan",
        elapsed: run_started.elapsed().as_secs_f64(),
    });

    if entries.is_empty() {
        progress_callback(ProgressEvent::PipelineMetrics {
            completed_demos: 0,
            skipped_demos: total_skipped,
            failed_demos: 0,
            elapsed: run_started.elapsed().as_secs_f64(),
            final_sample: true,
        });
        progress_callback(ProgressEvent::Finished {
            total_successful,
            total_failed,
            elapsed: run_started.elapsed().as_secs_f64(),
        });
        return Ok((
            BatchResults {
                start_time: run_started,
                total_aces: 0,
                total_quads: 0,
                total_triples: 0,
                total_multis: 0,
                total_doubles: 0,
                errors: Vec::new(),
            },
            Vec::new(),
        ));
    }

    // Process demos in batches
    let (batch_results, tbt_results) = process_demo_batches(
        entries,
        &config,
        &mut total_successful,
        &mut total_failed,
        &mut total_skipped,
        run_started,
        progress_callback.clone(),
        parsing_pool.clone(),
        decompression_pool.clone(),
        trim_pool,
        collection_filter,
        asset_refresh_only,
        cancel_flag,
    )
    .await?;

    // The finished event should be calculated based on start time
    let elapsed = batch_results.start_time.elapsed().as_secs_f64();

    progress_callback(ProgressEvent::PipelineMetrics {
        completed_demos: total_successful,
        skipped_demos: total_skipped,
        failed_demos: total_failed,
        elapsed,
        final_sample: true,
    });

    progress_callback(ProgressEvent::Finished {
        total_successful,
        total_failed,
        elapsed,
    });

    Ok((batch_results, tbt_results))
}

/// Check if a demo exists in DuckDB and is complete
fn demo_database_names(demo_path: &std::path::Path) -> Vec<String> {
    let file_name = demo_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string();
    let base = file_name
        .trim_end_matches(".zst")
        .trim_end_matches(".gz")
        .trim_end_matches(".dem")
        .to_string();
    let mut names = vec![
        file_name,
        format!("{base}.dem.gz"),
        format!("{base}.dem.zst"),
        format!("{base}.dem"),
        base,
    ];
    names.dedup();
    names
}

fn normalized_demo_identity(value: &str) -> String {
    let name = std::path::Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(value);
    name.trim_end_matches(".zst")
        .trim_end_matches(".gz")
        .trim_end_matches(".dem")
        .to_ascii_lowercase()
}

/// Prove that an asset-only refresh still addresses the same catalogue rows. Collection numbers
/// are parser-assigned; if parser evolution changed them, silently attaching a new S2R to the old
/// number would be worse than doing no repair at all.
fn validate_asset_refresh_targets(
    collections: &[Vec<KillCollection>],
    source_paths: &[(PathBuf, PathBuf)],
    filter: &HashMap<PathBuf, Vec<u32>>,
    output_root: &std::path::Path,
) -> Result<()> {
    let flattened = collections.iter().flatten().collect::<Vec<_>>();
    for (materialized, original) in source_paths {
        let wanted = selection::collections_for(filter, &original.to_string_lossy())
            .or_else(|| selection::collections_for(filter, &materialized.to_string_lossy()));
        let Some(wanted) = wanted else {
            continue;
        };
        let identity = normalized_demo_identity(&original.to_string_lossy());
        let produced = flattened
            .iter()
            .copied()
            .filter(|collection| normalized_demo_identity(&collection.demo_name) == identity || std::path::Path::new(&collection.demo_path) == materialized)
            .collect::<Vec<_>>();
        let produced_nums = produced
            .iter()
            .map(|collection| collection.collection_num as u32)
            .collect::<HashSet<_>>();
        let wanted_nums = wanted.iter().copied().collect::<HashSet<_>>();
        if produced_nums != wanted_nums {
            let mut missing = wanted_nums
                .difference(&produced_nums)
                .copied()
                .collect::<Vec<_>>();
            let mut unexpected = produced_nums
                .difference(&wanted_nums)
                .copied()
                .collect::<Vec<_>>();
            missing.sort_unstable();
            unexpected.sort_unstable();
            anyhow::bail!(
                "asset refresh selection drift for {}: missing {:?}, unexpected {:?}",
                original.display(),
                missing,
                unexpected
            );
        }

        for collection in produced {
            let database = config::storage::database_path(output_root, &collection.collection_type, &collection.folder);
            let connection = duckdb::Connection::open(&database).map_err(|error| {
                anyhow::anyhow!(
                    "could not open asset refresh target {}: {}",
                    database.display(),
                    error
                )
            })?;
            let mut statement = connection.prepare(
                "SELECT demo_name, type, round FROM kill_collections WHERE collection_num = ?",
            )?;
            let rows =
                statement.query_map(duckdb::params![collection.collection_num as i64], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                })?;
            let mut matched = false;
            for row in rows {
                let (demo_name, collection_type, round) = row?;
                if normalized_demo_identity(&demo_name) == normalized_demo_identity(&collection.demo_name)
                    && collection_type.as_deref() == Some(&collection.collection_type)
                    && round == i64::from(collection.round)
                {
                    matched = true;
                    break;
                }
            }
            if !matched {
                anyhow::bail!(
                    "asset refresh target drift: {} collection {} {} round {} no longer matches DuckDB",
                    original.display(),
                    collection.collection_num,
                    collection.collection_type,
                    collection.round
                );
            }
        }
    }
    Ok(())
}

/// Reconcile a whole scan with one catalog connection per folder and collection type.
fn filter_processed_demos(
    entries: Vec<PathBuf>,
    config: &AppConfig,
    total_skipped: &mut usize,
) -> Result<Vec<PathBuf>> {
    if config.parser.overwrite {
        return Ok(entries);
    }
    let completed = catalog_scan::completed_sources(&entries, config);
    *total_skipped += completed.len();
    Ok(entries
        .into_iter()
        .filter(|path| !completed.contains(path))
        .collect())
}

/// Process all demo batches
async fn process_demo_batches<F>(
    entries: Vec<PathBuf>,
    config: &AppConfig,
    total_successful: &mut usize,
    total_failed: &mut usize,
    total_skipped: &mut usize,
    run_started: Instant,
    progress_callback: Arc<F>,
    parsing_pool: Arc<rayon::ThreadPool>,
    decompression_pool: Arc<rayon::ThreadPool>,
    trim_pool: Arc<rayon::ThreadPool>,
    collection_filter: Option<HashMap<PathBuf, Vec<u32>>>,
    asset_refresh_only: bool,
    cancel_flag: Option<Arc<AtomicBool>>,
) -> Result<(
    BatchResults,
    Vec<tick_by_tick::collection_processor::ProcessResult>,
)>
where
    F: Fn(ProgressEvent) + Send + Sync + 'static,
{
    // Auto mode builds contiguous batches from individual estimates. This prevents a rare
    // giant demo from throttling the whole run, while still keeping every auto batch inside
    // the summed memory budget and parsing-pool limit. `max_concurrent_files = 0` (or absent)
    // auto-detects; a positive value preserves the fixed-count manual override.
    let cpu_threads = parsing_pool.current_num_threads().max(1);
    let total_files = entries.len();
    let batch_plan = batch_sizing::build_batch_plan_for_workload(
        entries,
        cpu_threads,
        Some(config.batch.max_concurrent_files),
        topology::available_memory_bytes(),
        config.parser.process_tick_data,
    );

    // Initialize before recording the durable batching decision below.
    if let Err(e) = crash_logger::CrashLogger::initialize() {
        eprintln!("Warning: Failed to initialize crash logger: {}", e);
    }

    println!("Batching: {}", batch_plan.reason.describe());
    crash_logger::log_info(&format!("Batching: {}", batch_plan.reason.describe()));
    let collection_filter = Arc::new(collection_filter);

    let batches = batch_plan.batches;
    let total_batches = batches.len();

    crash_logger::log_info(&format!(
        "Starting batch processing: {} files in {} batches",
        total_files, total_batches
    ));

    progress_callback(ProgressEvent::Started {
        total_files,
        total_batches,
    });

    let start_time = run_started;
    let mut batch_results = BatchResults {
        start_time,
        total_aces: 0,
        total_quads: 0,
        total_triples: 0,
        total_multis: 0,
        total_doubles: 0,
        errors: Vec::new(),
    };
    let mut tbt_results = Vec::new();
    let master_file_lock = Arc::new(DashMap::new());

    let prefetch_depth = config.batch.prefetch_depth;

    // Initialize decompression queue based on prefetch_depth
    let mut pending_decompressions: VecDeque<
        tokio::task::JoinHandle<Result<decompression::BatchDecompressedFiles>>,
    > = VecDeque::new();

    if prefetch_depth > 0 {
        // Pre-decompress initial batches up to prefetch_depth
        let initial_prefetch = prefetch_depth.min(total_batches);

        for i in 0..initial_prefetch {
            let batch_to_decompress = batches[i].clone();
            let config_clone = config.clone();
            let pool_clone = decompression_pool.clone();
            // Note: Prefetching doesn't support granular progress reporting as expected
            // because it runs ahead. We send None for now or need a way to queue events.
            // For now, only synchronous decompression will show granular unzip progress.
            let _handle = tokio::task::spawn(async move {
                decompress_batch_async(
                    &batch_to_decompress,
                    &config_clone,
                    should_skip_demo_processing,
                    pool_clone,
                    None,
                )
                .await
            });
            pending_decompressions.push_back(_handle);
        }
    }

    // Databases this import wrote to, recalculated once after the loop rather than once per
    // batch. Recalculation reads whole tables, so paying it per batch made every batch more
    // expensive as the tables grew. Accumulating here rather than returning it per batch is
    // what lets the finalization pass still run when a batch fails or the import is cancelled.
    let touched_duckdb_files: Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));

    // A batch failure is recorded and reported after finalization, so an import that dies
    // partway still leaves the databases it already wrote with correct totals.
    let mut fatal_batch_error: Option<anyhow::Error> = None;

    for (batch_idx, batch) in batches.iter().enumerate() {
        // Check for cancellation
        if let Some(flag) = &cancel_flag {
            if flag.load(Ordering::Relaxed) {
                // Abort any pending decompressions
                for handle in pending_decompressions {
                    handle.abort();
                }

                // We should clean up any files that were pre-decompressed but not processed?
                // The cleanup_decompressed_files happens per batch inside the loop.
                // But pre-fetched batches might have created files.
                // Since we abort the tasks, they might be half-written or fully written.
                // We don't have easy access to the exact paths here without waiting for results.
                // But generally, the OS temp dir cleans up, or we rely on logic not to fail hard on existing files.
                // Source deletion is owned by the verified trim transaction, not cancellation.

                break;
            }
        }

        let batch_start = Instant::now();
        progress_callback(ProgressEvent::BatchStarted {
            batch_num: batch_idx + 1,
            total_batches,
            file_count: batch.len(),
        });

        // Reset granular progress bars for new batch
        // Set total=0 to indicate inactive state (label will show "Collection"/"Tick" instead of "Writing to Database")
        progress_callback(ProgressEvent::CollectionWritingProgress {
            completed: 0,
            total: 0,
        });
        progress_callback(ProgressEvent::TickWritingProgress {
            completed: 0,
            total: 0,
        });

        // Helper closure for unzip progress
        let unzip_cb = {
            let cb = progress_callback.clone();
            let cb_box: Box<dyn Fn(String) + Send + Sync> = Box::new(move |filename| {
                cb(ProgressEvent::UnzippedFile { filename });
            });
            Arc::new(cb_box)
        };

        // Wait for decompression
        let decompression_started = Instant::now();
        let batch_decompressed = if prefetch_depth > 0 && !pending_decompressions.is_empty() {
            let handle = pending_decompressions.pop_front().unwrap();
            match handle.await {
                Ok(Ok(result)) => {
                    // Result came from prefetch. Report "Unzipped" events now since prefetch suppressed them.
                    for (_decompressed, source) in &result.decompressed_files {
                        let filename = source
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("unknown")
                            .to_string();
                        // Call the callback directly
                        progress_callback(ProgressEvent::UnzippedFile { filename });
                    }
                    result
                }
                Ok(Err(e)) => {
                    eprintln!("Error in background decompression: {}", e);
                    decompress_batch_async(
                        batch,
                        config,
                        should_skip_demo_processing,
                        decompression_pool.clone(),
                        Some(unzip_cb.clone()),
                    )
                    .await?
                }
                Err(e) => {
                    eprintln!("Task join error: {}", e);
                    decompress_batch_async(
                        batch,
                        config,
                        should_skip_demo_processing,
                        decompression_pool.clone(),
                        Some(unzip_cb.clone()),
                    )
                    .await?
                }
            }
        } else {
            decompress_batch_async(
                batch,
                config,
                should_skip_demo_processing,
                decompression_pool.clone(),
                Some(unzip_cb.clone()),
            )
            .await?
        };

        progress_callback(ProgressEvent::PhaseCompleted {
            phase: "decompression_wait",
            elapsed: decompression_started.elapsed().as_secs_f64(),
        });

        // Prefetch next batch
        if prefetch_depth > 0 {
            let next_batch_idx = batch_idx + prefetch_depth;
            if next_batch_idx < total_batches {
                let next_batch = batches[next_batch_idx].clone();
                let config_clone = config.clone();
                let pool_clone = decompression_pool.clone();
                let handle = tokio::task::spawn(async move {
                    decompress_batch_async(
                        &next_batch,
                        &config_clone,
                        should_skip_demo_processing,
                        pool_clone,
                        None,
                    )
                    .await
                });
                pending_decompressions.push_back(handle);
            }
        }

        // Process batch - count failures separately from skips
        let skipped_count = batch.len()
            - batch_decompressed.demo_files.len()
            - batch_decompressed.failed_files.len();
        *total_skipped += skipped_count;

        // Add decompression failures to batch results
        for (failed_path, error_msg) in &batch_decompressed.failed_files {
            batch_results
                .errors
                .push((failed_path.clone(), error_msg.clone()));
        }
        *total_failed += batch_decompressed.failed_files.len();

        // Log decompression failures
        if !batch_decompressed.failed_files.is_empty() {
            crash_logger::log_warning(&format!(
                "Batch {}: {} files failed decompression",
                batch_idx + 1,
                batch_decompressed.failed_files.len()
            ));
            for (path, error) in &batch_decompressed.failed_files {
                crash_logger::log_error(&format!("  - {}: {}", path.display(), error));
            }
        }

        let batch_demo_files = batch_decompressed.demo_files;
        let batch_decompressed_files = batch_decompressed.decompressed_files;

        // Spawn blocking task for CPU intensive work
        let config_clone = config.clone();
        let master_file_lock_clone = master_file_lock.clone();
        let progress_callback_clone = progress_callback.clone();
        let parsing_pool_clone = parsing_pool.clone();
        let trim_pool_clone = trim_pool.clone();
        let collection_filter_clone = collection_filter.clone();
        let touched_duckdb_clone = touched_duckdb_files.clone();

        let batch_outcome =
            task::spawn_blocking(move || {
                crash_logger::log_info(&format!(
                    "Entering spawn_blocking task for batch {}",
                    batch_idx + 1
                ));

                // Wrap entire block in error handling
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    crash_logger::log_info("Installing thread pool...");

                    parsing_pool_clone.install(move || {
                        crash_logger::log_info("Thread pool installed, logging batch start...");
                        crash_logger::log_batch_start(
                            batch_idx + 1,
                            total_batches,
                            batch_demo_files.len(),
                        );
                        crash_logger::log_info(&format!(
                            "Batch start logged, processing {} demos...",
                            batch_demo_files.len()
                        ));

                        // Process demos in parallel
                        let discovery_started = Instant::now();
                        crash_logger::log_info("Starting parallel iteration...");
                        let batch_process_results: Vec<_> = batch_demo_files
                            .par_iter()
                            .map(|demo_path| {
                                let original_path =
                                    batch_decompressed_files.get(demo_path).cloned();
                                let source_path = original_path
                                    .clone()
                                    .unwrap_or_else(|| demo_path.clone());

                                let result = process_single_demo_file_no_masters(
                                    demo_path,
                                    &config_clone,
                                    original_path,
                                );

                                let filename = demo_path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("unknown")
                                    .to_string();

                                progress_callback_clone(ProgressEvent::DemoProcessed { filename });

                                (demo_path.clone(), source_path, result)
                            })
                            .collect();

                        progress_callback_clone(ProgressEvent::PhaseCompleted {
                            phase: "discovery", elapsed: discovery_started.elapsed().as_secs_f64(),
                        });

                        // Collect results and In-Memory data
                        let mut batch_collections = Vec::new();
                        let mut batch_tbt_data_list = Vec::new();

                        // Temporary batch results for this task
                        let mut local_batch_results = BatchResults {
                            start_time: Instant::now(), // unused
                            total_aces: 0,
                            total_quads: 0,
                            total_triples: 0,
                            total_multis: 0,
                            total_doubles: 0,
                            errors: Vec::new(),
                        };

                        let source_paths = batch_process_results.iter()
                            .map(|(materialized, original, _)| (materialized.clone(), original.clone()))
                            .collect::<Vec<_>>();
                        let (batch_successful, batch_failed) = process_batch_results(
                            batch_process_results,
                            &mut batch_collections,
                            &mut batch_tbt_data_list,
                            &mut local_batch_results,
                            &config_clone,
                        )?;

                        // Narrow to the selection before anything is written. The filter used to
                        // be consulted only during tick processing, so a selected re-parse still
                        // rewrote every collection in each chosen demo — doing far more work than
                        // asked and overwriting rows the caller did not name.
                        if let Some(filter) = collection_filter_clone.as_ref() {
                            for demo_collections in batch_collections.iter_mut() {
                                demo_collections.retain(|collection| {
                                    crate::selection::collections_for(filter, &collection.demo_path)
                                        .is_some_and(|wanted| {
                                            wanted.contains(&(collection.collection_num as u32))
                                        })
                                });
                            }

                            batch_collections.retain(|demo| !demo.is_empty());
                        }

                        // Catalog and replay filters are intentionally separate.  DuckDB keeps
                        // the collection rows the user wants to browse; the later tick lane uses
                        // Replay* filters to decide which of those merit an S2R payload.
                        for demo_collections in batch_collections.iter_mut() {
                            demo_collections.retain(|collection| {
                                config_clone.is_catalog_collection_type_enabled(
                                    &collection.collection_type,
                                )
                            });
                        }
                        batch_collections.retain(|demo| !demo.is_empty());

                        // Preserve source provenance at parse time, but materialize the plan only
                        // after selection and catalog filters. This prevents rejected collections
                        // from producing clips or source rows.
                        if asset_refresh_only {
                            let filter = collection_filter_clone
                                .as_ref()
                                .as_ref()
                                .expect("asset refresh requires a collection filter");
                            validate_asset_refresh_targets(
                                &batch_collections,
                                &source_paths,
                                filter,
                                &config_clone.paths.parser_output,
                            )?;
                        }
                        let mut trim_plans = crate::trim_pipeline::build_trim_plans(
                            &batch_collections,
                            &source_paths,
                        )?;
                        // Provenance follows the complete catalog, not the narrower trim scope.
                        // Build it before removing rounds that should not emit physical clips.
                        let source_only_plans: Vec<_> = trim_plans.iter().filter(|p| !crate::round_replay::is_registered_clip(&config_clone.paths.parser_output, &p.original_path)).cloned().collect();
                        let grouped_demo_sources = crate::trim_pipeline::grouped_demo_sources(&source_only_plans)?;
                        let existing_clip_assets = crate::round_replay::existing_assets(&config_clone.paths.parser_output, &mut batch_collections, &mut batch_tbt_data_list, config_clone.parser.skip_buy_time)?;
                        trim_plans.retain(|plan| !existing_clip_assets.values().flatten().any(|a| std::path::Path::new(&a.path).canonicalize().ok() == plan.original_path.canonicalize().ok()));
                        crate::trim_pipeline::retain_trim_enabled_rounds(
                            &mut trim_plans,
                            |collection_type| {
                                config_clone.is_trim_collection_type_enabled(collection_type)
                                    || (config_clone.parser.process_tick_data && config_clone.is_collection_type_enabled(collection_type))
                            },
                        );

                        if !config_clone.parser.trim_collection_rounds {
                            trim_plans.clear();
                        }
                        let trim_started = Instant::now();
                        let local_tbt_results = crate::trim_pipeline::with_verified_trims(
                            &trim_plans,
                            trim_pool_clone.as_ref(),
                            crate::trim_pipeline::source_cleanup_enabled(
                                config_clone.parser.delete_source_after_trim,
                                config_clone.parser.process_tick_data,
                                collection_filter_clone.is_some(),
                            ),
                            config_clone.parser.skip_buy_time,
                            |mut trim_assets| {
                                let batch_tbt_data_list = crate::round_replay::prepare(
                                    &mut batch_collections, batch_tbt_data_list, &mut trim_assets,
                                    config_clone.parser.skip_buy_time)?;
                                for (key, assets) in existing_clip_assets { trim_assets.entry(key).or_default().extend(assets); }
                                progress_callback_clone(ProgressEvent::PhaseCompleted {
                                    phase: "trim", elapsed: trim_started.elapsed().as_secs_f64(),
                                });
                                let replay_started = Instant::now();

                                // 1. Create collection buffer and add all collections to RAM
                                let collection_buffer = CollectionBuffer::new();
                                if !batch_collections.is_empty() {
                                    // Prepare callback for collection writing (tracks buffer adds)
                                    let cb_clone = progress_callback_clone.clone();
                                    let col_cb = move |c: usize, t: usize| {
                                        cb_clone(ProgressEvent::CollectionWritingProgress {
                                            completed: c,
                                            total: t,
                                        });
                                    };

                                    add_batch_collections_to_buffer(
                                        batch_collections,
                                        &collection_buffer,
                                        Some(&col_cb),
                                    )?;
                                }

                                // 2. Process Tick-By-Tick In-Memory (updates buffer)
                                let mut local_tbt_results = Vec::new();
                                if config_clone.parser.process_tick_data && !batch_tbt_data_list.is_empty()
                                {
                                    let total_ticks = batch_tbt_data_list.len();
                                    let ticks_completed = AtomicUsize::new(0);

                                    let tbt_batch_results: Result<Vec<_>> = batch_tbt_data_list
                                        .par_iter()
                                        .map(|tbt_data| {
                                            // Get collection filter for this demo
                                            let demo_path = &tbt_data.demo_info.demo_path;
                                            let clip_local = trim_assets.values().flatten().any(|a| a.path == *demo_path);
                                            // A selection that cannot be resolved to this demo must skip
                                            // it, never fall through. Downstream, `None` means "no filter"
                                            // — so passing the unresolved case on would write every tick
                                            // asset in a demo the caller either did not select or could
                                            // not be identified unambiguously, which is the opposite of
                                            // what was asked.
                                            let collection_nums = match collection_filter_clone.as_ref() {
                                                None => None,
                                                Some(_) if clip_local => Some(tbt_data.collections.iter().map(|c| c.collection_num).collect()),
                                                Some(filter) => {
                                                    match crate::selection::collections_for(filter, demo_path) {
                                                        Some(nums) => Some(nums),
                                                        None => {
                                                            // Skipping silently would exit zero having
                                                            // written nothing, which reads as "your
                                                            // selection produced no collections" rather
                                                            // than "this demo could not be identified".
                                                            progress_callback_clone(ProgressEvent::Error {
                                                                path: std::path::PathBuf::from(demo_path),
                                                                error: "selected demo could not be matched                                                                 unambiguously; nothing was written                                                                 for it"
                                                                    .to_string(),
                                                            });

                                                            return Err(anyhow::anyhow!("selected demo could not be matched: {}", demo_path));
                                                        }
                                                    }
                                                }
                                            };

                                            // Pass buffer to tickbytick processor for RAM updates
                                            let result = crate::demo_processor::process_tick_by_tick_with_progress(
                                                tbt_data,
                                                &config_clone,
                                                &master_file_lock_clone,
                                                collection_nums.as_deref(),
                                                Some(&collection_buffer),
                                                Some(progress_callback_clone.clone()),
                                            ).map_err(|error| anyhow::anyhow!("Replay processing failed for {}: {:#}", demo_path, error));

                                            // Report progress
                                            let completed =
                                                ticks_completed.fetch_add(1, Ordering::Relaxed) + 1;
                                            progress_callback_clone(ProgressEvent::TickWritingProgress {
                                                completed,
                                                total: total_ticks,
                                            });

                                            result
                                        })
                                        .collect();

                                    for results in tbt_batch_results? {
                                        local_tbt_results.extend(results);
                                    }
                                }

                                progress_callback_clone(ProgressEvent::PhaseCompleted {
                                    phase: "replay", elapsed: replay_started.elapsed().as_secs_f64(),
                                });
                                let database_started = Instant::now();
                                collection_buffer.merge_assets(trim_assets.into_values().flatten());
                                if config_clone.parser.process_tick_data {
                                    collection_buffer.validate_required_replays(|kind|
                                        config_clone.is_collection_type_enabled(kind))?;
                                }

                                // 3. Write buffered collections to DuckDB (after tickbytick updates)
                                if !collection_buffer.is_empty() {
                                    crash_logger::log_info("Starting DuckDB write phase...");

                                    let cb_clone = progress_callback_clone.clone();
                                    let write_cb = move |c: usize, t: usize| {
                                        cb_clone(ProgressEvent::CollectionWritingProgress {
                                            completed: c,
                                            total: t,
                                        });
                                    };

                                    if asset_refresh_only {
                                        write_buffered_assets_to_duckdb(
                                            &collection_buffer,
                                            &grouped_demo_sources,
                                            &config_clone,
                                            &master_file_lock_clone,
                                            Some(&write_cb),
                                        )?;
                                    } else {
                                        write_buffered_collections_to_duckdb(
                                            &collection_buffer,
                                            &grouped_demo_sources,
                                            &config_clone,
                                            &master_file_lock_clone,
                                            touched_duckdb_clone.as_ref(),
                                            Some(&write_cb),
                                        )?;
                                    }

                                    crash_logger::log_info("DuckDB write phase completed");
                                }

                                progress_callback_clone(ProgressEvent::PhaseCompleted {
                                    phase: "database", elapsed: database_started.elapsed().as_secs_f64(),
                                });
                                Ok(local_tbt_results)
                        })?;

                        // Cleanup decompressed files
                        cleanup_decompressed_files(&batch_decompressed_files)?;

                        // Log batch completion
                        let total_collections = local_batch_results.total_aces
                            + local_batch_results.total_quads
                            + local_batch_results.total_triples
                            + local_batch_results.total_multis
                            + local_batch_results.total_doubles;
                        crash_logger::log_batch_end(
                            batch_idx + 1,
                            total_collections,
                            batch_start.elapsed().as_secs_f64(),
                        );

                        Ok::<
                            (
                                BatchResults,
                                Vec<tick_by_tick::collection_processor::ProcessResult>,
                                usize,
                                usize,
                            ),
                            anyhow::Error,
                        >((
                            local_batch_results,
                            local_tbt_results,
                            batch_successful,
                            batch_failed,
                        ))
                    })
                }));

                // Handle panic
                match result {
                    Ok(inner_result) => inner_result,
                    Err(panic_info) => {
                        let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                            format!("PANIC: {}", s)
                        } else if let Some(s) = panic_info.downcast_ref::<String>() {
                            format!("PANIC: {}", s)
                        } else {
                            "PANIC: Unknown panic occurred".to_string()
                        };

                        crash_logger::log_error(&panic_msg);
                        eprintln!("{}", panic_msg);

                        // Return error result
                        Err(anyhow::anyhow!("Batch processing panicked: {}", panic_msg))
                    }
                }
            })
            .await;

        // A join failure (the blocking task died outright) and a batch error are both fatal,
        // but neither may leave the loop by `?`: the databases written so far still need their
        // totals recalculated, and that now happens after this loop.
        let (local_batch_results, local_tbt_vec, batch_successful, batch_failed) =
            match batch_outcome {
                Ok(Ok(values)) => values,
                Ok(Err(e)) => {
                    fatal_batch_error = Some(e);
                    break;
                }
                Err(join_error) => {
                    fatal_batch_error = Some(anyhow::anyhow!(
                        "Batch {} did not complete: {}",
                        batch_idx + 1,
                        join_error
                    ));
                    break;
                }
            };

        // Merge results
        batch_results.total_aces += local_batch_results.total_aces;
        batch_results.total_quads += local_batch_results.total_quads;
        batch_results.total_triples += local_batch_results.total_triples;
        batch_results.total_multis += local_batch_results.total_multis;
        batch_results.total_doubles += local_batch_results.total_doubles;
        batch_results.errors.extend(local_batch_results.errors);
        tbt_results.extend(local_tbt_vec);

        *total_successful += batch_successful;
        *total_failed += batch_failed;

        // Record timing
        let batch_elapsed = batch_start.elapsed().as_secs_f64();

        progress_callback(ProgressEvent::BatchCompleted {
            batch_num: batch_idx + 1,
            total_batches,
            demos_count: batch_successful + batch_failed,
            collections_count: local_batch_results.total_aces
                + local_batch_results.total_quads
                + local_batch_results.total_triples
                + local_batch_results.total_multis
                + local_batch_results.total_doubles,
            elapsed: batch_elapsed,
        });
        progress_callback(ProgressEvent::PipelineMetrics {
            completed_demos: *total_successful,
            skipped_demos: *total_skipped,
            failed_demos: *total_failed,
            elapsed: run_started.elapsed().as_secs_f64(),
            final_sample: false,
        });

        // Clear cache after each batch to free memory
        if config.paths.ram_unzip {
            let (count, size) = interface::demo_cache::cache_stats();
            if count > 0 {
                println!(
                    "Clearing demo cache: {} entries, {:.2} MB",
                    count,
                    size as f64 / (1024.0 * 1024.0)
                );
            }
        }
        interface::demo_cache::clear_cache();
    }

    // Finalization. Reached on every exit from the loop above — completion, cancellation, or a
    // failed batch — because a database left with correct rows and stale totals reports the
    // wrong collection counts to every reader.
    let metadata_started = Instant::now();
    let touched = std::mem::take(
        &mut *touched_duckdb_files
            .lock()
            .expect("touched DuckDB set poisoned"),
    );
    let mut metadata_finalization_error = None;
    if !touched.is_empty() {
        crash_logger::log_info(&format!(
            "Recalculating metadata for {} database(s) touched by this import...",
            touched.len()
        ));
        if let Err(e) = recalculate_duckdb_metadata(&touched, &master_file_lock) {
            let message = format!("Metadata recalculation failed: {}", e);
            crash_logger::log_error(&message);
            eprintln!("{}", message);
            metadata_finalization_error = Some(anyhow::anyhow!(message));
        } else {
            crash_logger::log_info("Metadata recalculation completed");
        }
    }

    progress_callback(ProgressEvent::PhaseCompleted {
        phase: "metadata",
        elapsed: metadata_started.elapsed().as_secs_f64(),
    });
    match (fatal_batch_error, metadata_finalization_error) {
        (Some(batch_error), Some(metadata_error)) => {
            return Err(anyhow::anyhow!(
                "{}; metadata finalization also failed: {}",
                batch_error,
                metadata_error
            ));
        }
        (Some(batch_error), None) => return Err(batch_error),
        (None, Some(metadata_error)) => return Err(metadata_error),
        (None, None) => {}
    }

    Ok((batch_results, tbt_results))
}

fn process_batch_results(
    batch_process_results: Vec<(
        PathBuf,
        PathBuf,
        Result<(
            bool,
            Option<Vec<KillCollection>>,
            Option<KillCollectionData>,
        )>,
    )>,
    batch_collections: &mut Vec<Vec<KillCollection>>,
    batch_tbt_data_list: &mut Vec<KillCollectionData>,
    batch_results: &mut BatchResults,
    _config: &AppConfig,
) -> Result<(usize, usize)> {
    let mut batch_successful = 0;
    let mut batch_failed = 0;

    for (demo_path, _original_path, result) in batch_process_results {
        match result {
            Ok((demo_success, collections_opt, tbt_data_opt)) => {
                if demo_success {
                    batch_successful += 1;

                    if let Some(collections) = collections_opt {
                        for col in &collections {
                            match col.collection_type.as_str() {
                                "ACE" => batch_results.total_aces += 1,
                                "QUAD" => batch_results.total_quads += 1,
                                "TRIPLE" => batch_results.total_triples += 1,
                                "MULTI" => batch_results.total_multis += 1,
                                "DOUBLE" => batch_results.total_doubles += 1,
                                _ => {}
                            }
                        }
                        batch_collections.push(collections);
                    }

                    if let Some(tbt_data) = tbt_data_opt {
                        batch_tbt_data_list.push(tbt_data);
                    }
                } else {
                    batch_failed += 1;
                    batch_results
                        .errors
                        .push((demo_path.clone(), "Unknown processing error".to_string()));
                }
            }
            Err(e) => {
                batch_failed += 1;
                let error_msg = format_error_message(&e);
                batch_results.errors.push((demo_path.clone(), error_msg));
            }
        }
    }

    Ok((batch_successful, batch_failed))
}

fn format_error_message(e: &anyhow::Error) -> String {
    let e_str = e.to_string();
    if e_str.contains("os error 2") {
        "Demo file not found or moved during processing".to_string()
    } else if e_str.contains("Access is denied") {
        "Permission denied - file may be in use by another process".to_string()
    } else if e_str.contains("Collection CSV was not generated") {
        "Failed to generate collection data from demo file".to_string()
    } else if e_str.contains("corrupted") {
        "Demo file appears to be corrupted or invalid format".to_string()
    } else {
        format!("Demo processing error: {}", e)
    }
}

#[cfg(test)]
mod input_kind_tests {
    use super::InputKind;
    use std::path::Path;

    #[test]
    fn all_accepts_round_clips_and_raw_sources() {
        assert!(InputKind::All.accepts(Path::new("match.dem")));
        assert!(InputKind::All.accepts(Path::new("match_r10.dem")));
        assert!(InputKind::All.accepts(Path::new("match.dem.zst")));
    }

    #[test]
    fn raw_and_archives_still_exclude_round_clips() {
        assert!(!InputKind::Raw.accepts(Path::new("match_r10.dem")));
        assert!(InputKind::Raw.accepts(Path::new("match.dem")));
        assert!(!InputKind::Archives.accepts(Path::new("match_r10.dem")));
        assert!(InputKind::Archives.accepts(Path::new("match.dem.zst")));
    }
}

fn cleanup_decompressed_files(decompressed_files: &HashMap<PathBuf, PathBuf>) -> Result<()> {
    for (decompressed_path, _) in decompressed_files {
        if decompressed_path.exists() {
            if let Err(e) = fs::remove_file(decompressed_path) {
                eprintln!(
                    "Warning: Failed to clean up {}: {}",
                    decompressed_path.display(),
                    e
                );
            }
        }
    }
    Ok(())
}
