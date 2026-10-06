//! Validation module for the KillCollectionMaster crate
//!
//! This module provides comprehensive validation for kill collection data
//! to prevent corruption and ensure data integrity.

use interface::models::collection::KillCollection;
use std::collections::HashSet;

/// Validation errors that can occur during collection validation
#[derive(Debug, Clone)]
pub enum ValidationError {
    /// Required field is missing or empty
    MissingRequiredField(String),
    /// Field contains invalid characters or format
    InvalidFieldFormat(String, String),
    /// Array fields have mismatched lengths
    ArrayLengthMismatch(String),
    /// Numeric field is out of valid range
    InvalidRange(String, String),
    /// Duplicate entry detected
    DuplicateEntry(String),
    /// Corrupted or malformed data
    CorruptedData(String),
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::MissingRequiredField(field) => {
                write!(f, "Missing required field: {}", field)
            }
            ValidationError::InvalidFieldFormat(field, reason) => {
                write!(f, "Invalid format for field '{}': {}", field, reason)
            }
            ValidationError::ArrayLengthMismatch(field) => {
                write!(f, "Array length mismatch for field: {}", field)
            }
            ValidationError::InvalidRange(field, reason) => {
                write!(f, "Invalid range for field '{}': {}", field, reason)
            }
            ValidationError::DuplicateEntry(key) => {
                write!(f, "Duplicate entry detected: {}", key)
            }
            ValidationError::CorruptedData(reason) => {
                write!(f, "Corrupted data: {}", reason)
            }
        }
    }
}

impl std::error::Error for ValidationError {}

/// Comprehensive validator for kill collections
pub struct CollectionValidator {
    /// Set of known valid collection types
    valid_collection_types: HashSet<String>,
    /// Set of known valid weapon names
    valid_weapons: HashSet<String>,
    /// Set of known valid map names
    valid_maps: HashSet<String>,
}

impl CollectionValidator {
    /// Create a new collection validator
    pub fn new() -> Self {
        let mut valid_collection_types = HashSet::new();
        valid_collection_types.insert("ACE".to_string());
        valid_collection_types.insert("QUAD".to_string());
        valid_collection_types.insert("TRIPLE".to_string());
        valid_collection_types.insert("MULTI".to_string());
        valid_collection_types.insert("DOUBLE".to_string());
        valid_collection_types.insert("SINGLE".to_string());

        let mut valid_weapons = HashSet::new();
        // Add common CS2 weapons
        valid_weapons.insert("ak47".to_string());
        valid_weapons.insert("awp".to_string());
        valid_weapons.insert("m4a1".to_string());
        valid_weapons.insert("m4a1_silencer".to_string());
        valid_weapons.insert("glock".to_string());
        valid_weapons.insert("usp_silencer".to_string());
        valid_weapons.insert("deagle".to_string());
        valid_weapons.insert("mp9".to_string());
        valid_weapons.insert("tec9".to_string());
        valid_weapons.insert("galilar".to_string());
        valid_weapons.insert("mac10".to_string());
        valid_weapons.insert("nova".to_string());
        valid_weapons.insert("xm1014".to_string());
        valid_weapons.insert("mag7".to_string());
        valid_weapons.insert("sawedoff".to_string());
        valid_weapons.insert("famas".to_string());
        valid_weapons.insert("sg556".to_string());
        valid_weapons.insert("aug".to_string());
        valid_weapons.insert("scar20".to_string());
        valid_weapons.insert("g3sg1".to_string());
        valid_weapons.insert("p90".to_string());
        valid_weapons.insert("bizon".to_string());
        valid_weapons.insert("ump45".to_string());
        valid_weapons.insert("mp7".to_string());
        valid_weapons.insert("mp5sd".to_string());
        valid_weapons.insert("p250".to_string());
        valid_weapons.insert("fiveseven".to_string());
        valid_weapons.insert("cz75a".to_string());
        valid_weapons.insert("revolver".to_string());
        valid_weapons.insert("dualberettas".to_string());

        let mut valid_maps = HashSet::new();
        // Add common CS2 maps
        valid_maps.insert("de_ancient".to_string());
        valid_maps.insert("de_anubis".to_string());
        valid_maps.insert("de_dust2".to_string());
        valid_maps.insert("de_mirage".to_string());
        valid_maps.insert("de_nuke".to_string());
        valid_maps.insert("de_vertigo".to_string());
        valid_maps.insert("de_inferno".to_string());
        valid_maps.insert("de_overpass".to_string());
        valid_maps.insert("de_cache".to_string());
        valid_maps.insert("de_train".to_string());

        CollectionValidator {
            valid_collection_types,
            valid_weapons,
            valid_maps,
        }
    }

    /// Add a valid weapon to the validator
    pub fn add_valid_weapon(&mut self, weapon: &str) {
        self.valid_weapons.insert(weapon.to_string());
    }

    /// Add a valid map to the validator
    pub fn add_valid_map(&mut self, map: &str) {
        self.valid_maps.insert(map.to_string());
    }

    /// Validate a single kill collection
    pub fn validate_collection(&self, collection: &KillCollection) -> Result<(), ValidationError> {
        // Validate required fields
        self.validate_required_fields(collection)?;

        // Validate field formats
        self.validate_field_formats(collection)?;

        // Validate array field consistency
        self.validate_array_consistency(collection)?;

        // Validate ranges
        self.validate_ranges(collection)?;

        Ok(())
    }

    /// Validate required fields are present and not empty
    fn validate_required_fields(&self, collection: &KillCollection) -> Result<(), ValidationError> {
        if collection.collection_type.is_empty() {
            return Err(ValidationError::MissingRequiredField(
                "collection_type".to_string(),
            ));
        }

        if collection.killer_name.is_empty() {
            return Err(ValidationError::MissingRequiredField(
                "killer_name".to_string(),
            ));
        }

        if collection.killer_steamid.is_empty() {
            return Err(ValidationError::MissingRequiredField(
                "killer_steamid".to_string(),
            ));
        }

        if collection.demo_name.is_empty() {
            return Err(ValidationError::MissingRequiredField(
                "demo_name".to_string(),
            ));
        }

        if collection.map_name.is_empty() {
            return Err(ValidationError::MissingRequiredField(
                "map_name".to_string(),
            ));
        }

        if collection.weapons.is_empty() {
            return Err(ValidationError::MissingRequiredField("weapons".to_string()));
        }

        if collection.kill_ticks.is_empty() {
            return Err(ValidationError::MissingRequiredField(
                "kill_ticks".to_string(),
            ));
        }

        Ok(())
    }

    /// Validate field formats
    fn validate_field_formats(&self, collection: &KillCollection) -> Result<(), ValidationError> {
        // Validate collection type
        if !self
            .valid_collection_types
            .contains(&collection.collection_type)
        {
            return Err(ValidationError::InvalidFieldFormat(
                "collection_type".to_string(),
                format!("Unknown collection type: {}", collection.collection_type),
            ));
        }

        // Validate SteamID format (should be numeric and 17 digits)
        if !collection.killer_steamid.chars().all(|c| c.is_numeric()) {
            return Err(ValidationError::InvalidFieldFormat(
                "killer_steamid".to_string(),
                "SteamID must be numeric".to_string(),
            ));
        }

        if collection.killer_steamid.len() != 17 {
            return Err(ValidationError::InvalidFieldFormat(
                "killer_steamid".to_string(),
                "SteamID must be 17 digits".to_string(),
            ));
        }

        // Validate killer team format
        if !collection.killer_team.is_empty() {
            match collection.killer_team.as_str() {
                "T" | "CT" | "2" | "3" => {} // Valid team values
                _ => {
                    return Err(ValidationError::InvalidFieldFormat(
                        "killer_team".to_string(),
                        format!("Invalid team value: {}", collection.killer_team),
                    ))
                }
            }
        }

        // Validate victim team format
        if !collection.victim_team.is_empty() {
            match collection.victim_team.as_str() {
                "T" | "CT" | "2" | "3" => {} // Valid team values
                _ => {
                    return Err(ValidationError::InvalidFieldFormat(
                        "victim_team".to_string(),
                        format!("Invalid team value: {}", collection.victim_team),
                    ))
                }
            }
        }

        // Validate weapons
        for weapon in &collection.weapons {
            if weapon.is_empty() {
                return Err(ValidationError::InvalidFieldFormat(
                    "weapons".to_string(),
                    "Empty weapon name found".to_string(),
                ));
            }

            // Check for obvious corruption patterns
            if weapon.contains("[") || weapon.contains("]") || weapon.contains(",") {
                return Err(ValidationError::CorruptedData(format!(
                    "Weapon name contains invalid characters: {}",
                    weapon
                )));
            }
        }

        // Validate map name
        if collection.map_name.contains("[")
            || collection.map_name.contains("]")
            || collection.map_name.contains(",")
        {
            return Err(ValidationError::CorruptedData(format!(
                "Map name contains invalid characters: {}",
                collection.map_name
            )));
        }

        // Validate killer name
        if collection.killer_name.contains("[")
            || collection.killer_name.contains("]")
            || collection.killer_name.contains(",")
        {
            return Err(ValidationError::CorruptedData(format!(
                "Killer name contains invalid characters: {}",
                collection.killer_name
            )));
        }

        Ok(())
    }

    /// Validate array field consistency
    fn validate_array_consistency(
        &self,
        collection: &KillCollection,
    ) -> Result<(), ValidationError> {
        let weapon_count = collection.weapons.len();

        // All arrays should have the same length
        if !collection.weapons_id.is_empty() && collection.weapons_id.len() != weapon_count {
            return Err(ValidationError::ArrayLengthMismatch(
                "weapons_id".to_string(),
            ));
        }

        if collection.kill_ticks.len() != weapon_count {
            return Err(ValidationError::ArrayLengthMismatch(
                "kill_ticks".to_string(),
            ));
        }

        if collection.victim_indices.len() != weapon_count {
            return Err(ValidationError::ArrayLengthMismatch(
                "victim_indices".to_string(),
            ));
        }

        Ok(())
    }

    /// Validate numeric ranges
    fn validate_ranges(&self, collection: &KillCollection) -> Result<(), ValidationError> {
        // Validate tick values
        if collection.start_kill_tick > collection.end_kill_tick {
            return Err(ValidationError::InvalidRange(
                "kill_ticks".to_string(),
                "Start kill tick cannot be greater than end kill tick".to_string(),
            ));
        }

        if collection.round_start_tick > collection.round_end_tick {
            return Err(ValidationError::InvalidRange(
                "round_ticks".to_string(),
                "Round start tick cannot be greater than round end tick".to_string(),
            ));
        }

        // Validate that kill ticks are within reasonable bounds
        for &tick in &collection.kill_ticks {
            if tick < 0 {
                return Err(ValidationError::InvalidRange(
                    "kill_ticks".to_string(),
                    "Kill tick cannot be negative".to_string(),
                ));
            }

            if tick > 10000000 {
                // Reasonable upper bound for ticks
                return Err(ValidationError::InvalidRange(
                    "kill_ticks".to_string(),
                    "Kill tick value is unreasonably large".to_string(),
                ));
            }
        }

        // Validate round number
        if collection.round < 1 {
            return Err(ValidationError::InvalidRange(
                "round".to_string(),
                "Round number must be at least 1".to_string(),
            ));
        }

        if collection.round > 100 {
            // Reasonable upper bound
            return Err(ValidationError::InvalidRange(
                "round".to_string(),
                "Round number is unreasonably large".to_string(),
            ));
        }

        // Validate collection number
        if collection.collection_num < 1 {
            return Err(ValidationError::InvalidRange(
                "collection_num".to_string(),
                "Collection number must be at least 1".to_string(),
            ));
        }

        // Validate killer and victim indices
        if collection.killer_index < 0 || collection.killer_index > 64 {
            return Err(ValidationError::InvalidRange(
                "killer_index".to_string(),
                "Killer index must be between 0 and 64".to_string(),
            ));
        }

        for &victim_index in &collection.victim_indices {
            if victim_index < 0 || victim_index > 64 {
                return Err(ValidationError::InvalidRange(
                    "victim_indices".to_string(),
                    "Victim index must be between 0 and 64".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Validate multiple collections for duplicates
    pub fn validate_collections_for_duplicates(
        &self,
        collections: &[KillCollection],
    ) -> Result<(), ValidationError> {
        let mut seen_keys = HashSet::new();

        for collection in collections {
            let key = self.generate_collection_key(collection);
            if seen_keys.contains(&key) {
                return Err(ValidationError::DuplicateEntry(key));
            }
            seen_keys.insert(key);
        }

        Ok(())
    }

    /// Generate a unique key for a collection to detect duplicates
    pub fn generate_collection_key(&self, collection: &KillCollection) -> String {
        format!(
            "{}|{}|{}|{}|{}",
            collection.killer_steamid,
            collection.demo_name,
            collection.round,
            collection.start_kill_tick,
            collection.end_kill_tick
        )
    }

    /// Sanitize a collection by fixing common issues
    pub fn sanitize_collection(
        &self,
        collection: &mut KillCollection,
    ) -> Result<(), ValidationError> {
        // Trim whitespace from string fields
        collection.collection_type = collection.collection_type.trim().to_string();
        collection.killer_name = collection.killer_name.trim().to_string();
        collection.killer_steamid = collection.killer_steamid.trim().to_string();
        collection.demo_name = collection.demo_name.trim().to_string();
        collection.map_name = collection.map_name.trim().to_string();
        collection.killer_team = collection.killer_team.trim().to_string();
        collection.victim_team = collection.victim_team.trim().to_string();
        collection.folder = collection.folder.trim().to_string();

        // Sanitize weapons array
        collection.weapons = collection
            .weapons
            .iter()
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect();

        // Sanitize weapons_id array
        collection.weapons_id = collection
            .weapons_id
            .iter()
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
            .collect();

        // Remove .dem extension from demo name if present
        collection.demo_name =
            interface::utils::parser_utils::canonical_demo_name(&collection.demo_name);

        // Normalize team values
        collection.killer_team = self.normalize_team_value(&collection.killer_team);
        collection.victim_team = self.normalize_team_value(&collection.victim_team);

        // Sort kill ticks and corresponding arrays
        self.sort_kill_data(collection)?;

        Ok(())
    }

    /// Normalize team values to consistent format
    fn normalize_team_value(&self, team: &str) -> String {
        match team {
            "2" => "T".to_string(),
            "3" => "CT".to_string(),
            "T" | "CT" => team.to_string(),
            _ => team.to_string(),
        }
    }

    /// Sort kill data by tick to ensure consistency
    fn sort_kill_data(&self, collection: &mut KillCollection) -> Result<(), ValidationError> {
        if collection.kill_ticks.is_empty() {
            return Ok(());
        }

        // Create indices for sorting
        let mut indices: Vec<usize> = (0..collection.kill_ticks.len()).collect();
        indices.sort_by_key(|&i| collection.kill_ticks[i]);

        // Sort kill_ticks
        let sorted_ticks: Vec<i32> = indices.iter().map(|&i| collection.kill_ticks[i]).collect();
        collection.kill_ticks = sorted_ticks;

        // Sort corresponding arrays if they exist and have the same length
        if collection.weapons.len() == collection.kill_ticks.len() {
            let sorted_weapons: Vec<String> = indices
                .iter()
                .map(|&i| collection.weapons[i].clone())
                .collect();
            collection.weapons = sorted_weapons;
        }

        if collection.weapons_id.len() == collection.kill_ticks.len() {
            let sorted_weapons_id: Vec<String> = indices
                .iter()
                .map(|&i| collection.weapons_id[i].clone())
                .collect();
            collection.weapons_id = sorted_weapons_id;
        }

        if collection.victim_indices.len() == collection.kill_ticks.len() {
            let sorted_victim_indices: Vec<i32> = indices
                .iter()
                .map(|&i| collection.victim_indices[i])
                .collect();
            collection.victim_indices = sorted_victim_indices;
        }

        Ok(())
    }

    /// Check if a collection is potentially corrupted
    pub fn is_collection_corrupted(&self, collection: &KillCollection) -> bool {
        // Check for obvious corruption patterns
        if collection.killer_name.contains("[") || collection.killer_name.contains("]") {
            return true;
        }

        if collection.map_name.contains("[") || collection.map_name.contains("]") {
            return true;
        }

        if collection.demo_name.contains("[") || collection.demo_name.contains("]") {
            return true;
        }

        // Check for weapons with corruption patterns
        for weapon in &collection.weapons {
            if weapon.contains("[") || weapon.contains("]") || weapon.contains(",") {
                return true;
            }
        }

        // Check for array length mismatches
        if !collection.weapons_id.is_empty()
            && collection.weapons_id.len() != collection.weapons.len()
        {
            return true;
        }

        if collection.kill_ticks.len() != collection.weapons.len() {
            return true;
        }

        if collection.victim_indices.len() != collection.weapons.len() {
            return true;
        }

        // Check for invalid tick values
        if collection.start_kill_tick > collection.end_kill_tick {
            return true;
        }

        if collection.round_start_tick > collection.round_end_tick {
            return true;
        }

        // Enhanced corruption detection for Line 23-type issues
        // Check for empty killer name
        if collection.killer_name.is_empty() {
            return true;
        }

        // Check for truncated SteamID
        if collection.killer_steamid.len() < 17 {
            return true;
        }

        // Check for invalid map names (corruption patterns)
        if collection.map_name.starts_with("dde_") || collection.map_name.contains("dde_") {
            return true;
        }

        // Check for invalid tick combinations (both start and end being 0)
        if collection.start_kill_tick == 0 && collection.end_kill_tick == 0 && collection.round > 0
        {
            return true;
        }

        // Check for corrupted map names that don't start with known prefixes
        if !collection.map_name.starts_with("de_")
            && !collection.map_name.starts_with("cs_")
            && !collection.map_name.starts_with("ar_")
            && !collection.map_name.is_empty()
        {
            return true;
        }

        false
    }

    /// Filter out corrupted collections from a list
    pub fn filter_corrupted_collections(
        &self,
        collections: &[KillCollection],
    ) -> Vec<KillCollection> {
        collections
            .iter()
            .filter(|collection| !self.is_collection_corrupted(collection))
            .cloned()
            .collect()
    }

    /// Validate and sanitize a collection
    pub fn validate_and_sanitize_collection(
        &self,
        collection: &mut KillCollection,
    ) -> Result<(), ValidationError> {
        // First sanitize
        self.sanitize_collection(collection)?;

        // Then validate
        self.validate_collection(collection)?;

        Ok(())
    }

    /// Validate a batch of collections
    pub fn validate_collections(
        &self,
        collections: &[KillCollection],
    ) -> Result<Vec<ValidationError>, ValidationError> {
        let mut errors = Vec::new();

        for (index, collection) in collections.iter().enumerate() {
            if let Err(error) = self.validate_collection(collection) {
                errors.push(ValidationError::CorruptedData(format!(
                    "Collection {}: {}",
                    index, error
                )));
            }
        }

        // Check for duplicates
        if let Err(error) = self.validate_collections_for_duplicates(collections) {
            errors.push(error);
        }

        Ok(errors)
    }
}

impl Default for CollectionValidator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_collection_success() {
        let validator = CollectionValidator::new();

        let mut collection = KillCollection {
            collection_type: "ACE".to_string(),
            collection_num: 1,
            col_total: 1,
            tick_duration: 1000,
            map_name: "de_dust2".to_string(),
            killer_index: 5,
            killer_team: "T".to_string(),
            start_kill_tick: 1000,
            end_kill_tick: 2000,
            killer_name: "TestPlayer".to_string(),
            killer_steamid: "76561198000000000".to_string(),
            demo_name: "test_demo".to_string(),
            demo_path: "".to_string(),
            folder: "test".to_string(),
            killer_radius: 100.0,
            victims_radius: 200.0,
            killer_move_distance: 300.0,
            victim_team: "CT".to_string(),
            round_start_tick: 500,
            round_end_tick: 2500,
            round_freeze_end: 750,
            round: 5,
            weapons: vec!["ak47".to_string(), "ak47".to_string()],
            weapons_id: vec!["7".to_string(), "7".to_string()],
            kill_ticks: vec![1000, 1500],
            victim_indices: vec![10, 11],
            kills: Vec::new(),
            util_thrown: String::new(),
            parsed: 0,
            grenade_traj: 0,
            tag: String::new(),
            game_version: 0,
            hits: 0,
            misses: 0,
            hit_rate: 0.0,
            util_thrown_ticks: String::new(),
            util_land_ticks: String::new(),
            weapons_damaged: String::new(),
            weapons_damaged_num_hits: String::new(),
            weapons_damaged_hits_formatted: String::new(),
        };

        assert!(validator.validate_collection(&collection).is_ok());
    }

    #[test]
    fn test_validate_collection_missing_field() {
        let validator = CollectionValidator::new();

        let collection = KillCollection {
            collection_type: "".to_string(), // Missing required field
            collection_num: 1,
            col_total: 1,
            tick_duration: 1000,
            map_name: "de_dust2".to_string(),
            killer_index: 5,
            killer_team: "T".to_string(),
            start_kill_tick: 1000,
            end_kill_tick: 2000,
            killer_name: "TestPlayer".to_string(),
            killer_steamid: "76561198000000000".to_string(),
            demo_name: "test_demo".to_string(),
            demo_path: "".to_string(),
            folder: "test".to_string(),
            killer_radius: 100.0,
            victims_radius: 200.0,
            killer_move_distance: 300.0,
            victim_team: "CT".to_string(),
            round_start_tick: 500,
            round_end_tick: 2500,
            round_freeze_end: 750,
            round: 5,
            weapons: vec!["ak47".to_string()],
            weapons_id: vec!["7".to_string()],
            kill_ticks: vec![1000],
            victim_indices: vec![10],
            kills: Vec::new(),
            util_thrown: String::new(),
            parsed: 0,
            grenade_traj: 0,
            tag: String::new(),
            game_version: 0,
            hits: 0,
            misses: 0,
            hit_rate: 0.0,
            util_thrown_ticks: String::new(),
            util_land_ticks: String::new(),
            weapons_damaged: String::new(),
            weapons_damaged_num_hits: String::new(),
            weapons_damaged_hits_formatted: String::new(),
        };

        assert!(validator.validate_collection(&collection).is_err());
    }

    #[test]
    fn test_detect_corruption() {
        let validator = CollectionValidator::new();

        let mut collection = KillCollection {
            collection_type: "ACE".to_string(),
            collection_num: 1,
            col_total: 1,
            tick_duration: 1000,
            map_name: "de_dust2".to_string(),
            killer_index: 5,
            killer_team: "T".to_string(),
            start_kill_tick: 1000,
            end_kill_tick: 2000,
            killer_name: "TestPlayer[corrupted]".to_string(), // Corrupted name
            killer_steamid: "76561198000000000".to_string(),
            demo_name: "test_demo".to_string(),
            demo_path: "".to_string(),
            folder: "test".to_string(),
            killer_radius: 100.0,
            victims_radius: 200.0,
            killer_move_distance: 300.0,
            victim_team: "CT".to_string(),
            round_start_tick: 500,
            round_end_tick: 2500,
            round_freeze_end: 750,
            round: 5,
            weapons: vec!["ak47".to_string()],
            weapons_id: vec!["7".to_string()],
            kill_ticks: vec![1000],
            victim_indices: vec![10],
            kills: Vec::new(),
            util_thrown: String::new(),
            parsed: 0,
            grenade_traj: 0,
            tag: String::new(),
            game_version: 0,
            hits: 0,
            misses: 0,
            hit_rate: 0.0,
            util_thrown_ticks: String::new(),
            util_land_ticks: String::new(),
            weapons_damaged: String::new(),
            weapons_damaged_num_hits: String::new(),
            weapons_damaged_hits_formatted: String::new(),
        };

        assert!(validator.is_collection_corrupted(&collection));
    }
}
