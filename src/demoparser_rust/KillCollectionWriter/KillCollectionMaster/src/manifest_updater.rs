//! Manifest updater module for the KillCollectionMaster crate
//!
//! This module contains functionality for updating the manifest file with information about the master CSV file.

use std::io::{self, Write, Read};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::{self, Value};

/// Manifest information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestInfo {
    /// Master path
    pub master_path: String,
    /// Number of collections
    pub num_collections: usize,
    /// Last updated timestamp
    pub last_updated: String,
    /// Collection types
    pub collection_types: HashMap<String, usize>,
    /// Maps
    pub maps: HashMap<String, usize>,
    /// Weapons
    pub weapons: HashMap<String, usize>,
}

/// Manifest updater for updating the manifest file
pub struct ManifestUpdater {
    /// Master path
    master_path: String,
    /// Manifest path
    manifest_path: String,
    /// Manifest info
    manifest_info: ManifestInfo,
}

impl ManifestUpdater {
    /// Create a new manifest updater
    pub fn new(master_path: &str, manifest_path: &str) -> io::Result<Self> {
        // Create the parent directory if it doesn't exist
        if let Some(parent) = Path::new(manifest_path).parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }

        // Check if the manifest file exists
        let manifest_exists = Path::new(manifest_path).exists();

        // Create or load the manifest info
        let manifest_info = if manifest_exists {
            // Load the manifest info from the file
            let mut file = File::open(manifest_path)?;
            let mut contents = String::new();
            file.read_to_string(&mut contents)?;

            // Parse the manifest info
            serde_json::from_str(&contents)?
        } else {
            // Create a new manifest info
            ManifestInfo {
                master_path: master_path.to_string(),
                num_collections: 0,
                last_updated: chrono::Local::now().to_rfc3339(),
                collection_types: HashMap::new(),
                maps: HashMap::new(),
                weapons: HashMap::new(),
            }
        };

        Ok(ManifestUpdater {
            master_path: master_path.to_string(),
            manifest_path: manifest_path.to_string(),
            manifest_info,
        })
    }

    /// Update the manifest
    pub fn update_manifest(&mut self) -> io::Result<()> {
        // Check if the master file exists
        if !Path::new(&self.master_path).exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Master file not found: {}", self.master_path),
            ));
        }

        // Read the master file
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(b',')
            .from_path(&self.master_path)?;

        // Count the collections and gather statistics
        let mut num_collections = 0;
        let mut collection_types = HashMap::new();
        let mut maps = HashMap::new();
        let mut weapons = HashMap::new();

        for result in reader.records() {
            let record = result?;

            // Increment the collection count
            num_collections += 1;

            // Update collection types
            if let Some(collection_type) = record.get(0) {
                *collection_types.entry(collection_type.to_string()).or_insert(0) += 1;
            }

            // Update maps
            if let Some(map_name) = record.get(3) {
                *maps.entry(map_name.to_string()).or_insert(0) += 1;
            }

            // Update weapons
            if let Some(weapons_str) = record.get(21) {
                for weapon in weapons_str.split(';') {
                    *weapons.entry(weapon.to_string()).or_insert(0) += 1;
                }
            }
        }

        // Update the manifest info
        self.manifest_info.master_path = self.master_path.clone();
        self.manifest_info.num_collections = num_collections;
        self.manifest_info.last_updated = chrono::Local::now().to_rfc3339();
        self.manifest_info.collection_types = collection_types;
        self.manifest_info.maps = maps;
        self.manifest_info.weapons = weapons;

        // Write the manifest info to the file
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.manifest_path)?;

        let json = serde_json::to_string_pretty(&self.manifest_info)?;
        file.write_all(json.as_bytes())?;

        Ok(())
    }

    /// Close the updater
    pub fn close(&mut self) -> io::Result<()> {
        // Nothing to do
        Ok(())
    }

    /// Get the manifest info
    pub fn get_manifest_info(&self) -> &ManifestInfo {
        &self.manifest_info
    }

    /// Get the master path
    pub fn get_master_path(&self) -> &str {
        &self.master_path
    }

    /// Get the manifest path
    pub fn get_manifest_path(&self) -> &str {
        &self.manifest_path
    }
}
