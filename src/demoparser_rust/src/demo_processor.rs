//! Demo processing utilities for generating collection CSVs and master files
//!
//! Handles the core demo parsing logic, collection CSV generation, and
//! interfacing with master file writers.

use anyhow::{anyhow, Result};
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::collection_buffer::CollectionBuffer;
use crate::collection_converter::convert_to_tick_by_tick_format;
use crate::tick_by_tick::kill_collection_parser::KillCollectionData;
use config::AppConfig;
use interface::core::demo_processor::DemoProcessor;
use interface::core::tick_processor::TickProcessor;
use interface::models::collection::KillCollection;
use kill_collection_master::master_writer::MasterWriter;

/// Helper to convert main AppConfig to Master AppConfig
#[allow(dead_code)]
fn to_master_config(config: &AppConfig) -> kill_collection_master::config::AppConfig {
    kill_collection_master::config::AppConfig {
        paths: kill_collection_master::config::PathsConfig {
            parser_output: config.paths.parser_output.clone(),
        },
        parser: kill_collection_master::config::ParserConfig {
            process_tick_data: config.parser.process_tick_data,
            pad_ticks: config.parser.pad_ticks,
            track_all_players: config.parser.track_all_players,
            aces: config.parser.aces,
            quads: config.parser.quads,
            triples: config.parser.triples,
            multi: config.parser.multi,
            singles: config.parser.singles,
            doubles: config.parser.doubles,
        },
        downloader: kill_collection_master::config::DownloaderConfig {
            batch_size: config.downloader.batch_size,
        },
        batch: kill_collection_master::config::BatchConfig {
            threads: config.batch.threads,
            max_concurrent_files: config.batch.max_concurrent_files,
            max_retries: config.batch.max_retries,
            retry_delay: config.batch.retry_delay,
            show_progress: config.batch.show_progress,
        },
    }
}

/// Extract folder name from demo path
pub fn extract_folder_name_from_path(demo_path: &str) -> String {
    interface::utils::parser_utils::get_folder_from_demo_path(demo_path)
}

/// Check if demo should be skipped (Placeholder for future logic, e.g. DuckDB check)
pub fn should_skip_demo_processing(_demo_path: &PathBuf, config: &AppConfig) -> Result<bool> {
    // If overwrite is false, we might want to check if it exists in DB.
    // For now, we rely on DuckDB UPSERT logic or caller to handle skipping.
    // Returning false means "process it".
    if !config.parser.overwrite {
        // TODO: Implement check against DuckDB if needed for optimization
    }
    Ok(false)
}

/// Process a single demo file without writing to master files (for batch processing)
/// Returns (success, optional_master_collections, optional_tbt_data)
pub fn process_single_demo_file_no_masters(
    demo_path: &PathBuf,
    config: &AppConfig,
    original_file_path: Option<PathBuf>,
) -> Result<(
    bool,
    Option<Vec<KillCollection>>,
    Option<KillCollectionData>,
)> {
    if should_skip_demo_processing(demo_path, config)? {
        let demo_name = demo_path.file_stem().unwrap().to_str().unwrap();
        println!("Skipping already processed demo: {}", demo_name);
        return Ok((true, None, None));
    }

    println!("Processing Demo: {}", demo_path.display());

    // 1. Create DemoProcessor
    let processor = DemoProcessor::new(&demo_path.to_string_lossy())?;

    // 2. Generate Collections using TickProcessor
    let mut tick_processor = TickProcessor::new(
        processor.get_demo_info().clone(),
        processor.get_player_info().to_vec(),
        processor.get_rounds().to_vec(),
        processor.get_game_events().to_vec(),
    );

    tick_processor.process_events();
    tick_processor.create_collections();
    let mut collections = tick_processor.get_collections().to_vec();

    // Calculate total collections for this demo
    let col_total = collections.len() as i32;

    // Determine forced folder name if original path is provided
    let forced_folder_name = Some(config::storage::source_folder(
        &config.paths.parser_output, original_file_path.as_ref().unwrap_or(demo_path)));

    // Update collections with col_total and override folder name if forced
    for col in &mut collections {
        col.col_total = col_total;
        if let Some(ref folder_name) = forced_folder_name {
            col.folder = folder_name.clone();
        }
    }

    // Filter legacy checkpoint prelude collections before consulting stable IDs.
    // Their previous-round kills are not part of the saved round the user chose.
    let mut scope = convert_to_tick_by_tick_format(&collections, &processor, original_file_path.as_ref())?;
    if crate::round_replay::standalone_clip(demo_path, &mut scope)?.is_some() {
        let round = scope.rounds[0].round as i32;
        collections.retain(|c| c.round == round);
        for col in &mut collections {
            col.round_start_tick = scope.rounds[0].start_tick as i32;
            col.round_freeze_end = col.round_freeze_end.max(2);
        }
    }
    crate::round_replay::restore_clip_identity(&config.paths.parser_output, demo_path, &mut collections)?;

    // 3. Convert to In-Memory format for TBT
    let tbt_data =
        convert_to_tick_by_tick_format(&collections, &processor, original_file_path.as_ref())?;

    Ok((true, Some(collections), Some(tbt_data)))
}

/// Process a single demo file
#[allow(dead_code)]
pub fn process_single_demo_file(
    demo_path: &PathBuf,
    config: &AppConfig,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
) -> Result<(bool, Option<String>)> {
    // 1. Process demo to get in-memory data
    // Pass None for forced_folder_name in single file mode (assumes no unzip_dir complexity or handled inside)
    let (success, collections_opt, tbt_data_opt) =
        process_single_demo_file_no_masters(demo_path, config, None)?;

    if !success || collections_opt.is_none() || tbt_data_opt.is_none() {
        return Ok((success, None));
    }

    let collections = collections_opt.unwrap();
    let tbt_data = tbt_data_opt.unwrap();
    let folder_name = config::storage::source_folder(&config.paths.parser_output, demo_path);

    // 2. Write to Master Files (DuckDB only preferred)
    let mut collections_by_type: std::collections::HashMap<String, Vec<KillCollection>> =
        std::collections::HashMap::new();
    for col in &collections {
        collections_by_type
            .entry(col.collection_type.clone())
            .or_default()
            .push(col.clone());
    }

    for (col_type, cols) in collections_by_type {
        let master_path = config::storage::database_path(&config.paths.parser_output, &col_type, &folder_name);

        let file_lock = master_file_lock
            .entry(master_path.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())));
        let _guard = file_lock.lock().unwrap();

        let master_writer =
            MasterWriter::new(master_path.to_str().unwrap(), &col_type, &folder_name)?;

        // Propagate: a failed master write means these collections were never persisted,
        // so reporting the demo as processed would be a lie.
        master_writer
            .write_master_csv(&cols, Some(file_lock.value()))
            .map_err(|e| {
                anyhow!(
                    "Failed to write master file for type {} ({}): {}",
                    col_type,
                    master_path.display(),
                    e
                )
            })?;
    }

    // 3. Tick-By-Tick Moving (if enabled)
    if config.parser.process_tick_data {
        match process_tick_by_tick_in_memory(&tbt_data, config, master_file_lock, None) {
            Ok(_) => Ok((true, None)),
            Err(e) => Ok((true, Some(format!("Tick-by-tick failed: {}", e)))),
        }
    } else {
        Ok((true, None))
    }
}

/// Process tick-by-tick data using in-memory KillCollectionData (legacy without buffer)
pub fn process_tick_by_tick_in_memory(
    collection_data: &KillCollectionData,
    config: &AppConfig,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    collection_nums: Option<&[u32]>,
) -> Result<Vec<crate::tick_by_tick::collection_processor::ProcessResult>> {
    process_tick_by_tick_with_buffer(
        collection_data,
        config,
        master_file_lock,
        collection_nums,
        None,
    )
}

/// Process tick-by-tick data with optional buffer for RAM-based updates
pub fn process_tick_by_tick_with_buffer(
    collection_data: &KillCollectionData,
    config: &AppConfig,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    collection_nums: Option<&[u32]>,
    buffer: Option<&CollectionBuffer>,
) -> Result<Vec<crate::tick_by_tick::collection_processor::ProcessResult>> {
    process_tick_by_tick_with_progress(collection_data, config, master_file_lock, collection_nums, buffer, None)
}

pub fn process_tick_by_tick_with_progress(
    collection_data: &KillCollectionData,
    config: &AppConfig,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    collection_nums: Option<&[u32]>,
    buffer: Option<&CollectionBuffer>,
    progress: Option<Arc<dyn Fn(crate::ProgressEvent) + Send + Sync>>,
) -> Result<Vec<crate::tick_by_tick::collection_processor::ProcessResult>> {
    use crate::tick_by_tick::cli::CliArgs;
    use crate::tick_by_tick::collection_processor::CollectionProcessor;
    use crate::tick_by_tick::config_manager as tbt_config_manager;

    let cli_args = CliArgs {
        track_all_players: Some(config.parser.track_all_players),
        padding: Some(config.parser.pad_ticks.to_string()),
        collection_nums: collection_nums.map(|nums| nums.to_vec()),
        collection_types: crate::tick_by_tick::cli::CollectionTypeFilter {
            ace: Some(config.parser.aces),
            quad: Some(config.parser.quads),
            triple: Some(config.parser.triples),
            multi: Some(config.parser.multi),
            double: Some(config.parser.doubles),
            single: Some(config.parser.singles),
            all_types: false,
        },
        output_dir: Some(config.paths.parser_output.clone()),
    };

    let mut tbt_config = tbt_config_manager::load_config(None)?;
    tbt_config.overwrite = config.parser.overwrite;
    tbt_config.skip_buy_time = config.parser.skip_buy_time;

    let final_config = tbt_config_manager::merge_cli_overrides(tbt_config, &cli_args);

    let processor =
        CollectionProcessor::new(final_config, cli_args.clone(), master_file_lock.clone())
            .with_progress(progress);

    let results = processor.process_collections_with_buffer(collection_data, buffer)?;

    Ok(results)
}
