use crate::models::{CollectionEntry, CollectionType, TickFilterState};
use anyhow::Result;
use std::path::{Path, PathBuf};

pub mod loader;
pub mod scanner;
pub mod stats;
pub mod tags;

/// Information about a DuckDB file
#[derive(Debug, Clone)]
pub struct DatabaseInfo {
    pub path: PathBuf,
    pub collection_type: String,
    pub folder: String,
}

impl DatabaseInfo {
    /// Parse database info from file path
    /// Expected format: {TYPE}_{folder}.duckdb
    pub fn from_path(path: &Path) -> Option<Self> {
        let filename = path.file_stem()?.to_str()?;
        let parts: Vec<&str> = filename.split('_').collect();

        if parts.len() < 2 {
            return None;
        }

        let collection_type = parts[0].to_string();
        let folder = parts[1..].join("_");

        Some(DatabaseInfo {
            path: path.to_path_buf(),
            collection_type,
            folder,
        })
    }

    /// Check if this database matches the given collection type
    pub fn matches_type(&self, coll_type: CollectionType) -> bool {
        self.collection_type
            .eq_ignore_ascii_case(coll_type.as_str())
    }
}

/// Database manager for DuckDB files
#[derive(Clone)]
pub struct DatabaseManager {
    master_dir: PathBuf,
}

impl DatabaseManager {
    pub fn new(master_dir: PathBuf) -> Self {
        Self { master_dir }
    }

    /// Scan a directory for demo files (.dem, .gz, .zst)
    pub fn scan_directory_for_demos(&self, path: &Path, recursive: bool) -> Result<Vec<PathBuf>> {
        scanner::scan_directory_for_demos(path, recursive)
    }

    /// Scan directory for DuckDB files matching {TYPE}_*.duckdb pattern
    pub fn scan_databases(&self) -> Result<Vec<DatabaseInfo>> {
        scanner::scan_databases(&self.master_dir)
    }

    /// Load collections from specific databases
    pub fn load_collections(
        &self,
        databases: &[DatabaseInfo],
        tick_filter: TickFilterState,
    ) -> Result<Vec<CollectionEntry>> {
        loader::load_collections(databases, tick_filter)
    }

    /// Load detailed fields for a specific collection entry
    pub fn load_details(&self, entry: CollectionEntry) -> Result<CollectionEntry> {
        loader::load_details(entry)
    }

    /// Update tags for selected collections
    pub fn update_tags(&self, collections: &[&CollectionEntry]) -> Result<()> {
        tags::update_tags(collections)
    }

    /// Get all unique tags from databases
    pub fn get_all_tags(&self, databases: &[DatabaseInfo]) -> Result<Vec<String>> {
        tags::get_all_tags(databases)
    }

    /// Count all directory statistics in a single synchronous call
    pub fn count_all_directory_stats(
        &self,
        folders: &[String],
        parser_output: &Path,
    ) -> Result<Vec<(String, usize, usize, usize)>> {
        stats::count_all_directory_stats(&self.master_dir, folders, parser_output)
    }
}
