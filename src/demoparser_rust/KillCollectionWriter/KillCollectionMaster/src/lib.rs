//! KillCollectionMaster crate for the demoparser_rust project
//!
//! This crate provides functionality for managing master CSV/DuckDB files of kill collections.

pub mod cleanup;
pub mod config;
pub mod duckdb;
pub mod duckdb_inspector;
pub mod master_writer;
pub mod validation;

use std::io;
use std::path::Path;

use interface::models::collection::KillCollection;
use master_writer::MasterWriter;
use validation::CollectionValidator;

/// Write collections directly to a master file (DuckDB) for a specific collection type
///
/// # Arguments
///
/// * `collections` - Collections to write
/// * `master_path` - Path to the master file (historically CSV path used for identifier)
/// * `collection_type` - Type of collection (ACE, TRIPLE, etc.)
/// * `folder` - Folder name to use
///
/// # Returns
///
/// Returns `Ok(())` on success, or an `io::Error` on failure.
pub fn write_collections_to_master(
    collections: &[KillCollection],
    master_path: &str,
    collection_type: &str,
    folder: &str,
) -> io::Result<()> {
    // Create the parent directory if it doesn't exist
    if let Some(parent) = Path::new(master_path).parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Create a master writer
    let master_writer = MasterWriter::new(master_path, collection_type, folder)?;

    // Write the collections to the master (DuckDB only now)
    master_writer.write_master_csv(collections, None)?;

    Ok(())
}

/// Validate collections before processing
///
/// # Arguments
///
/// * `collections` - Collections to validate
///
/// # Returns
///
/// Returns validated collections on success, or an `io::Error` on failure.
pub fn validate_collections(collections: &[KillCollection]) -> io::Result<Vec<KillCollection>> {
    // Create a collection validator
    let validator = CollectionValidator::new();

    // Validate the collections and filter out corrupted ones
    let validated_collections = validator.filter_corrupted_collections(collections);

    Ok(validated_collections)
}

/// Process collections, validate them, and write to master file
///
/// This is a high-level function that combines validation and writing for a specific collection type.
///
/// # Arguments
///
/// * `collections` - Collections to process
/// * `master_path` - Path to the master file
/// * `collection_type` - Type of collection (ACE, TRIPLE, etc.)
/// * `folder` - Folder name to use
///
/// # Returns
///
/// Returns `Ok(())` on success, or an `io::Error` on failure.
pub fn process_and_write_collections(
    collections: &[KillCollection],
    master_path: &str,
    collection_type: &str,
    folder: &str,
) -> io::Result<()> {
    // Validate the collections
    let validated_collections = validate_collections(collections)?;

    // Filter collections by type
    let filtered_collections: Vec<KillCollection> = validated_collections
        .into_iter()
        .filter(|c| c.collection_type == collection_type)
        .collect();

    // Write the collections to the master file
    write_collections_to_master(&filtered_collections, master_path, collection_type, folder)?;

    Ok(())
}

/// Create a master writer for a specific collection type
///
/// # Arguments
///
/// * `master_path` - Path to the master file
/// * `collection_type` - Type of collection (ACE, TRIPLE, etc.)
/// * `folder` - Folder name to use
///
/// # Returns
///
/// Returns a `MasterWriter` on success, or an `io::Error` on failure.
pub fn create_master_writer(
    master_path: &str,
    collection_type: &str,
    folder: &str,
) -> io::Result<MasterWriter> {
    MasterWriter::new(master_path, collection_type, folder)
}
