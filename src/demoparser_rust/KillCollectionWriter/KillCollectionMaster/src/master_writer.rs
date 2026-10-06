//! Master writer module for the KillCollectionMaster crate
//!
//! This module contains functionality for writing kill collections to a master CSV file
//! that matches the Python implementation structure.

use crate::duckdb::DuckDBWriter;
use crate::validation::CollectionValidator;
use interface::models::collection::KillCollection;
use interface::models::demo_source::DemoSource;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct ManifestInfo {
    pub collection_type: String,
    pub folder: String,
    pub total_demos: usize,
    pub total_collections: usize,
    pub unique_steam_ids: usize,
}

/// Master writer for writing kill collections to a master CSV file with Python-compatible structure
pub struct MasterWriter {
    /// Master CSV file path
    master_path: PathBuf,
    /// Collection type (ACE, TRIPLE, etc.)
    collection_type: String,
    /// Folder name
    folder: String,
}

impl MasterWriter {
    /// Create a new master writer
    pub fn new(master_path: &str, collection_type: &str, folder: &str) -> io::Result<Self> {
        // Create the parent directory if it doesn't exist
        if let Some(parent) = Path::new(master_path).parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        Ok(MasterWriter {
            master_path: PathBuf::from(master_path),
            collection_type: collection_type.to_string(),
            folder: folder.to_string(),
        })
    }

    /// Write the complete master file (DuckDB only)
    pub fn write_master_csv(
        &self,
        collections: &[KillCollection],
        lock: Option<&Arc<Mutex<()>>>,
    ) -> io::Result<()> {
        self.write_master_csv_with_sources(collections, &[], lock)
    }

    /// Write collection rows and source provenance atomically to DuckDB.
    pub fn write_master_csv_with_sources(
        &self,
        collections: &[KillCollection],
        demo_sources: &[DemoSource],
        lock: Option<&Arc<Mutex<()>>>,
    ) -> io::Result<()> {
        self.write_collections_with_assets(collections, demo_sources, &[], lock)
    }

    pub fn write_collections_with_assets(
        &self,
        collections: &[KillCollection],
        demo_sources: &[DemoSource],
        assets: &[interface::models::tick_asset::TickAsset],
        lock: Option<&Arc<Mutex<()>>>,
    ) -> io::Result<()> {
        // We default to DuckDB only now.

        // Create validator
        let validator = CollectionValidator::new();

        // Validate and sanitize new collections
        let mut validated_collections = Vec::new();
        let mut validation_errors = Vec::new();

        for (index, collection) in collections.iter().enumerate() {
            let mut collection_copy = collection.clone();

            // First check if collection is obviously corrupted
            if validator.is_collection_corrupted(&collection_copy) {
                eprintln!(
                    "Warning: Skipping corrupted NEW collection at index {}: {:?}",
                    index, collection_copy.killer_name
                );
                continue;
            }

            // Try to sanitize and validate
            match validator.validate_and_sanitize_collection(&mut collection_copy) {
                Ok(()) => validated_collections.push(collection_copy),
                Err(e) => {
                    validation_errors.push(format!("Collection {}: {}", index, e));
                    eprintln!(
                        "Warning: Skipping invalid NEW collection at index {}: {}",
                        index, e
                    );
                }
            }
        }

        // Print validation errors if any
        if !validation_errors.is_empty() {
            eprintln!("Validation errors found in new collections:");
            for error in &validation_errors {
                eprintln!("  - {}", error);
            }
        }

        if validated_collections.len() != collections.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Refusing incomplete catalog commit: {} of {} collections passed validation",
                    validated_collections.len(),
                    collections.len()
                ),
            ));
        }

        // Write DuckDB file
        // Acquire lock before writing DuckDB to prevent race conditions
        let _lock_guard = lock.map(|l| l.lock().unwrap());
        // Only pass NEW validated collections - DuckDB will recalculate metadata
        // Propagate rather than warn. DuckDB is the system of record; swallowing a write
        // failure here reported success to the caller, which then marked collections
        // complete for data that was never persisted.
        self.write_duckdb_file(&validated_collections, demo_sources, assets)?;

        Ok(())
    }

    /// Path of the DuckDB file this writer maintains.
    ///
    /// Uppercase collection type to match the expected format
    /// (ACE_test3.duckdb, not ace_test3.duckdb).
    fn duckdb_path(&self) -> PathBuf {
        if self.master_path.extension().is_some_and(|extension| extension == "duckdb") {
            return self.master_path.clone();
        }
        self.master_path.with_file_name(format!(
            "{}_{}.duckdb",
            self.collection_type.to_uppercase(),
            self.folder
        ))
    }

    /// Record tick-data assets for this type/folder.
    ///
    /// Separate from `write_master_csv` because assets describe files on disk rather than
    /// collection contents, and a collection can have zero, one or two of them.
    pub fn write_tick_assets(
        &self,
        assets: &[interface::models::tick_asset::TickAsset],
        lock: Option<&Arc<Mutex<()>>>,
    ) -> io::Result<()> {
        if assets.is_empty() {
            return Ok(());
        }

        let _lock_guard = lock.map(|l| l.lock().unwrap());
        let conn = duckdb::Connection::open(self.duckdb_path()).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("DuckDB open error: {}", e))
        })?;
        crate::duckdb::schema::initialize_tables(&conn).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("DuckDB schema error: {}", e))
        })?;
        crate::duckdb::tick_assets::record_tick_assets(&conn, assets).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("DuckDB tick_assets write error: {}", e),
            )
        })?;

        Ok(())
    }

    /// Eagerly migrate a catalogue partition and atomically backfill source provenance and
    /// already-verified asset rows without touching `kill_collections`.
    pub fn repair_catalog_assets(
        &self,
        demo_sources: &[DemoSource],
        assets: &[interface::models::tick_asset::TickAsset],
        lock: Option<&Arc<Mutex<()>>>,
    ) -> io::Result<()> {
        self.repair_catalog_assets_with_timeline(demo_sources, assets, &[], lock)
    }

    /// Commit a replay refresh and its clip-local tick columns together. User
    /// annotations and all unrelated collection metadata remain untouched.
    pub fn repair_catalog_assets_with_timeline(
        &self,
        demo_sources: &[DemoSource],
        assets: &[interface::models::tick_asset::TickAsset],
        timelines: &[KillCollection],
        lock: Option<&Arc<Mutex<()>>>,
    ) -> io::Result<()> {
        let _lock_guard = lock.map(|l| l.lock().unwrap());
        let conn = duckdb::Connection::open(self.duckdb_path()).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("DuckDB open error: {}", e))
        })?;
        crate::duckdb::schema::initialize_tables(&conn).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("DuckDB schema error: {}", e))
        })?;
        conn.execute("BEGIN TRANSACTION", []).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("DuckDB begin error: {}", e))
        })?;

        let result = (|| -> duckdb::Result<()> {
            for collection in timelines {
                let ticks = collection.kill_ticks.iter().map(i32::to_string).collect::<Vec<_>>().join(";");
                let name = collection.demo_name.trim_end_matches(".gz").trim_end_matches(".zst").trim_end_matches(".dem");
                let changed = conn.execute("UPDATE kill_collections SET start_kill_tick=?, end_kill_tick=?, round_start_tick=?, round_end_tick=?, round_freeze_end=?, kill_ticks=?, demo_relative_path=? WHERE demo_name=? AND collection_num=?", duckdb::params![collection.start_kill_tick, collection.end_kill_tick, collection.round_start_tick, collection.round_end_tick, collection.round_freeze_end, ticks, collection.demo_path, name, collection.collection_num])?;
                if changed != 1 { return Err(duckdb::Error::QueryReturnedNoRows); }
            }
            crate::duckdb::demo_sources::upsert_demo_sources(&conn, demo_sources)?;
            crate::duckdb::tick_assets::repair_tick_assets_in_transaction(&conn, assets)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = conn.execute("ROLLBACK", []);
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("DuckDB catalogue repair error: {}", error),
            ));
        }
        if let Err(error) = conn.execute("COMMIT", []) {
            let _ = conn.execute("ROLLBACK", []);
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("DuckDB commit error: {}", error),
            ));
        }
        Ok(())
    }

    /// Write DuckDB file by appending only new collections
    /// This is much more efficient than rewriting all collections
    /// Note: Metadata recalculation is skipped here and should be done at batch end
    fn write_duckdb_file(
        &self,
        new_collections: &[KillCollection],
        demo_sources: &[DemoSource],
        assets: &[interface::models::tick_asset::TickAsset],
    ) -> io::Result<()> {
        let duckdb_path = self.duckdb_path();

        // Create DuckDB writer
        let duckdb_writer = DuckDBWriter::new(
            duckdb_path.to_str().unwrap(),
            &self.collection_type,
            &self.folder,
        );

        // Append new collections only - skip metadata for fast writes
        // Metadata will be recalculated at the end of batch processing
        duckdb_writer
            .append_collections_with_assets(new_collections, demo_sources, assets, true)
            .map_err(|e| {
                io::Error::new(io::ErrorKind::Other, format!("DuckDB write error: {}", e))
            })?;

        Ok(())
    }

    /// Process a kill collection CSV file (Legacy support removed or redirected)
    /// Now assumed memory-only flow. This function might be unused.
    pub fn process_collection_csv(
        &mut self,
        _csv_path: &str,
        _lock: Option<&Arc<Mutex<()>>>,
    ) -> io::Result<usize> {
        // Deprecated/Removed functionality
        Ok(0)
    }

    /// Get the master path
    pub fn get_master_path(&self) -> &Path {
        &self.master_path
    }
}

/// Process a collection CSV file and update all relevant master files (Deprecated)
pub fn process_kill_collection_csv(
    _csv_path: &str,
    _folder: &str,
    _lock: &Arc<Mutex<()>>,
) -> io::Result<()> {
    // Deprecated implementation stub - code removed
    Ok(())
}

/// Process a collection CSV file and update all relevant master files with custom output directory (Deprecated)
pub fn process_kill_collection_csv_with_output_dir(
    _csv_path: &str,
    _output_dir: &str,
    _folder: &str,
    _lock: &Arc<Mutex<()>>,
) -> io::Result<Vec<PathBuf>> {
    // Deprecated implementation stub
    Ok(Vec::new())
}
