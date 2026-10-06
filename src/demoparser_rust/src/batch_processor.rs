//! Batch processing utilities for handling in-memory collections
//!
//! Provides bulk insert functionality for master files and DuckDB metadata
//! recalculation for improved performance in batch scenarios.

use anyhow::Result;
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::collection_buffer::CollectionBuffer;
use config::AppConfig;
use interface::models::collection::KillCollection;
use interface::models::demo_source::DemoSource;
use kill_collection_master::duckdb::DuckDBWriter;
use kill_collection_master::master_writer::MasterWriter;

// process_batch_collections_to_masters (CSV) removed.

/// Process a batch of in-memory collections to master files with bulk inserts
/// Returns a list of DuckDB files that were updated
pub fn process_batch_memory_collections_to_masters(
    collections_batch: Vec<Vec<KillCollection>>,
    config: &AppConfig,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    progress_callback: Option<&dyn Fn(usize, usize)>,
) -> Result<HashSet<PathBuf>> {
    if collections_batch.is_empty() {
        if let Some(callback) = progress_callback {
            callback(1, 1);
        }
        return Ok(HashSet::new());
    }

    // Load the kill_collection_master config
    let master_config = kill_collection_master::config::AppConfig::load()
        .map_err(|e| anyhow::anyhow!("Failed to load master config: {}", e))?;

    // Group collections by (collection_type, folder)
    let mut grouped_collections: HashMap<(String, String), Vec<KillCollection>> = HashMap::new();

    for demo_collections in collections_batch {
        for collection in demo_collections {
            let folder = collection.folder.clone();
            let key = (collection.collection_type.clone(), folder);
            grouped_collections
                .entry(key)
                .or_insert_with(Vec::new)
                .push(collection);
        }
    }

    process_grouped_collections(
        grouped_collections,
        config,
        master_file_lock,
        master_config,
        progress_callback,
    )
}

/// Helper function to process grouped collections to masters
fn process_grouped_collections(
    grouped_collections: HashMap<(String, String), Vec<KillCollection>>,
    config: &AppConfig,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    _master_config: kill_collection_master::config::AppConfig,
    progress_callback: Option<&dyn Fn(usize, usize)>,
) -> Result<HashSet<PathBuf>> {
    let mut updated_duckdb_files = HashSet::new();
    let total_groups = grouped_collections.len();
    let mut groups_processed = 0;

    // Process each group with a single bulk write
    for ((collection_type, folder), collections) in grouped_collections {
        let master_path = config::storage::database_path(&config.paths.parser_output, &collection_type, &folder);

        let master_writer =
            MasterWriter::new(&master_path.to_string_lossy(), &collection_type, &folder)?;

        // Get lock for this master file
        let entry = master_file_lock
            .entry(master_path.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())));
        let lock = Arc::clone(entry.value());

        // Bulk write all collections for this type/folder in one go (Actually just DuckDB now)
        println!(
            "  Bulk inserting {} {} collections to {}_{}...",
            collections.len(),
            collection_type,
            collection_type,
            folder
        );
        master_writer.write_master_csv(&collections, Some(&lock))?;

        // Track the updated DuckDB file
        let duckdb_path = config::storage::database_path(&config.paths.parser_output, &collection_type, &folder);
        if duckdb_path.exists() {
            updated_duckdb_files.insert(duckdb_path);
        }

        groups_processed += 1;
        if let Some(callback) = progress_callback {
            callback(groups_processed, total_groups);
        }
    }

    Ok(updated_duckdb_files)
}

/// Recalculate metadata for all updated DuckDB files
pub fn recalculate_duckdb_metadata(
    duckdb_files: &HashSet<PathBuf>,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
) -> Result<()> {
    if duckdb_files.is_empty() {
        return Ok(());
    }

    use crate::crash_logger;

    crash_logger::log_info(&format!(
        "Recalculating metadata for {} DuckDB files...",
        duckdb_files.len()
    ));
    println!(
        "Recalculating metadata for {} DuckDB files...",
        duckdb_files.len()
    );

    let mut errors = Vec::new();

    for duckdb_path in duckdb_files {
        let filename = duckdb_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("<invalid filename>");
        let Some((collection_type, folder)) = duckdb_identity(duckdb_path) else {
            let error_msg = format!(
                "Cannot recover collection type and folder from DuckDB filename: {}",
                duckdb_path.display()
            );
            crash_logger::log_error(&error_msg);
            errors.push(error_msg);
            continue;
        };

        crash_logger::log_debug(&format!("Metadata recalc START: {}", filename));

        // Lock the DuckDB file for thread-safe metadata recalculation
        let entry = master_file_lock
            .entry(duckdb_path.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())));
        let lock = Arc::clone(entry.value());
        let _guard = lock.lock().unwrap();

        let writer = DuckDBWriter::new(duckdb_path.to_str().unwrap(), &collection_type, &folder);

        match writer.recalculate_metadata() {
            Ok(_) => {
                println!("  ✓ Metadata updated: {}", filename);
                crash_logger::log_debug(&format!("Metadata recalc SUCCESS: {}", filename));
            }
            Err(e) => {
                let error_msg = format!("Failed to update metadata for {}: {}", filename, e);
                eprintln!("  ✗ {}", error_msg);
                crash_logger::log_error(&error_msg);
                errors.push(error_msg);
            }
        }
    }

    if !errors.is_empty() {
        let summary = format!(
            "Metadata recalculation failed for {} database(s): {}",
            errors.len(),
            errors.join("; ")
        );
        crash_logger::log_error(&summary);
        return Err(anyhow::anyhow!(summary));
    }

    Ok(())
}

/// Recover the database partition from `{TYPE}_{folder}.duckdb`.
///
/// Collection types are fixed tokens without underscores, while source-folder names may contain
/// any number of them. Splitting at the first underscore preserves the complete folder identity.
fn duckdb_identity(path: &Path) -> Option<(String, String)> {
    if path.extension()?.to_str()? != "duckdb" { return None; }
    let (kind, folder) = config::storage::database_identity(path)?;
    (!kind.is_empty() && !folder.is_empty()).then_some((kind, folder))
}

/// Add batch collections to the RAM buffer instead of writing to DuckDB immediately
/// This is the new approach for better performance with tickbytick processing
pub fn add_batch_collections_to_buffer(
    collections_batch: Vec<Vec<KillCollection>>,
    buffer: &CollectionBuffer,
    progress_callback: Option<&dyn Fn(usize, usize)>,
) -> Result<()> {
    if collections_batch.is_empty() {
        if let Some(callback) = progress_callback {
            callback(1, 1);
        }
        return Ok(());
    }

    let total_demos = collections_batch.len();
    let mut processed_demos = 0;

    for demo_collections in collections_batch {
        buffer.add_collections(demo_collections);
        processed_demos += 1;

        if let Some(callback) = progress_callback {
            callback(processed_demos, total_demos);
        }
    }

    let total_collections = buffer.len();
    println!("Added {} collections to RAM buffer", total_collections);

    Ok(())
}

/// Write all buffered collections to DuckDB at batch completion
/// This is called after tickbytick processing is complete
pub fn write_buffered_collections_to_duckdb(
    buffer: &CollectionBuffer,
    grouped_demo_sources: &HashMap<(String, String), Vec<DemoSource>>,
    config: &AppConfig,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    touched_duckdb_files: &Mutex<HashSet<PathBuf>>,
    progress_callback: Option<&dyn Fn(usize, usize)>,
) -> Result<()> {
    if buffer.is_empty() {
        if let Some(callback) = progress_callback {
            callback(1, 1);
        }
        return Ok(());
    }

    // Get completion stats
    let (completed, total) = buffer.get_completion_stats();
    println!(
        "Writing {} collections to DuckDB ({} with tickbytick data, {} without)...",
        total,
        completed,
        total - completed
    );

    // Get grouped collections from buffer
    let grouped_collections = buffer.get_grouped_collections();
    let mut grouped_assets = buffer.get_grouped_assets();

    // Load the kill_collection_master config (for potential future use)
    let _master_config = kill_collection_master::config::AppConfig::load()
        .map_err(|e| anyhow::anyhow!("Failed to load master config: {}", e))?;

    let total_groups = grouped_collections.len();
    let mut groups_processed = 0;

    // Write each group to DuckDB
    for ((collection_type, folder), collections) in grouped_collections {
        let master_path = config::storage::database_path(&config.paths.parser_output, &collection_type, &folder);

        let master_writer =
            MasterWriter::new(&master_path.to_string_lossy(), &collection_type, &folder)?;

        // Get lock for this master file
        let entry = master_file_lock
            .entry(master_path.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())));
        let lock = Arc::clone(entry.value());

        // Write all collections for this type/folder to DuckDB
        println!(
            "  Writing {} {} collections to DuckDB: {}_{}...",
            collections.len(),
            collection_type,
            collection_type,
            folder
        );
        let sources = grouped_demo_sources
            .get(&(collection_type.clone(), folder.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let assets = grouped_assets
            .remove(&(collection_type.clone(), folder.clone()))
            .unwrap_or_default();
        master_writer.write_collections_with_assets(&collections, sources, &assets, Some(&lock))?;

        // The collection transaction above is already durable. Register its database before any
        // later fallible work so finalization still recalculates it if asset registration or a
        // subsequent database group fails.
        let duckdb_path = config::storage::database_path(&config.paths.parser_output, &collection_type, &folder);
        touched_duckdb_files
            .lock()
            .expect("touched DuckDB set poisoned")
            .insert(duckdb_path);

        groups_processed += 1;
        if let Some(callback) = progress_callback {
            callback(groups_processed, total_groups);
        }
    }

    Ok(())
}

/// Persist replay assets and source provenance for an existing catalogue selection without
/// rewriting any `kill_collections` rows or recalculating collection metadata.
pub fn write_buffered_assets_to_duckdb(
    buffer: &CollectionBuffer,
    grouped_demo_sources: &HashMap<(String, String), Vec<DemoSource>>,
    config: &AppConfig,
    master_file_lock: &Arc<DashMap<PathBuf, Arc<Mutex<()>>>>,
    progress_callback: Option<&dyn Fn(usize, usize)>,
) -> Result<()> {
    let grouped_collections = buffer.get_grouped_collections();
    let mut grouped_assets = buffer.get_grouped_assets();
    let mut groups: HashSet<(String, String)> = grouped_assets.keys().cloned().collect();
    groups.extend(grouped_demo_sources.keys().cloned());
    if groups.is_empty() {
        if let Some(callback) = progress_callback {
            callback(1, 1);
        }
        return Ok(());
    }

    let total_groups = groups.len();
    for (index, (collection_type, folder)) in groups.into_iter().enumerate() {
        let master_path = config::storage::database_path(&config.paths.parser_output, &collection_type, &folder);
        let master_writer =
            MasterWriter::new(&master_path.to_string_lossy(), &collection_type, &folder)?;
        let entry = master_file_lock
            .entry(master_path)
            .or_insert_with(|| Arc::new(Mutex::new(())));
        let lock = Arc::clone(entry.value());
        let sources = grouped_demo_sources
            .get(&(collection_type.clone(), folder.clone()))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let assets = grouped_assets
            .remove(&(collection_type.clone(), folder.clone()))
            .unwrap_or_default();
        println!(
            "  Repairing {}_{}: {} provenance rows, {} replay assets",
            collection_type,
            folder,
            sources.len(),
            assets.len()
        );
        let timelines: Vec<_> = grouped_collections.get(&(collection_type.clone(), folder.clone())).into_iter().flatten()
            .filter(|c| assets.iter().any(|a| a.format == interface::models::tick_asset::AssetFormat::Dem && a.format_version == crate::round_replay::LOCAL_DEM_VERSION && a.collection_num == c.collection_num && crate::normalized_demo_identity(&a.demo_name) == crate::normalized_demo_identity(&c.demo_name))).cloned().collect();
        master_writer.repair_catalog_assets_with_timeline(sources, &assets, &timelines, Some(&lock))?;
        if let Some(callback) = progress_callback {
            callback(index + 1, total_groups);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duckdb_identity_preserves_underscores_in_folder() {
        let path = Path::new(r"C:\output\KillCollectionMaster\ACE_2024_01.duckdb");
        assert_eq!(duckdb_identity(path), Some(("ACE".into(), "2024_01".into())));
    }

    #[test]
    fn duckdb_identity_rejects_malformed_names() {
        assert_eq!(duckdb_identity(Path::new("ACE.duckdb")), None);
        assert_eq!(duckdb_identity(Path::new("_folder.duckdb")), None);
        assert_eq!(duckdb_identity(Path::new("ACE_.duckdb")), None);
        assert_eq!(duckdb_identity(Path::new("ACE_folder.csv")), None);
    }

    #[test]
    fn metadata_recalculation_propagates_database_errors() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("ACE_folder_name.duckdb");
        std::fs::write(&database, b"not a DuckDB database").unwrap();

        let databases = HashSet::from([database]);
        let locks = Arc::new(DashMap::new());

        assert!(recalculate_duckdb_metadata(&databases, &locks).is_err());
    }
}
