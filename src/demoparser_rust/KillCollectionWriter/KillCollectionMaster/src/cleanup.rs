//! Cleanup module for fixing corrupted kill collection master files
//!
//! This module provides utilities to detect, analyze, and repair corrupted
//! master CSV files that have been damaged due to concurrent access or
//! parsing errors.

use crate::master_writer::MasterWriter;
use crate::validation::CollectionValidator;
use interface::models::collection::KillCollection;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

/// Corruption patterns that indicate damaged data
const CORRUPTION_PATTERNS: &[&str] = &[
    "[[KILL_",
    "usp_sile[[",
    "TRIPLE,2,113,de_3,8556,de7166",
    "TTR[",
    "de_n7,1373,deCT",
    ",,,8",
    "TRIP,1715,1116,de_n,CT,30T",
];

/// Cleanup manager for repairing corrupted master files
pub struct CleanupManager {
    validator: CollectionValidator,
}

impl CleanupManager {
    /// Create a new cleanup manager
    pub fn new() -> Self {
        CleanupManager {
            validator: CollectionValidator::new(),
        }
    }

    /// Analyze a master file for corruption
    pub fn analyze_master_file(&self, file_path: &str) -> io::Result<CorruptionAnalysis> {
        let mut analysis = CorruptionAnalysis::new(file_path.to_string());

        if !Path::new(file_path).exists() {
            analysis.file_exists = false;
            return Ok(analysis);
        }

        let file = File::open(file_path)?;
        let reader = BufReader::new(file);
        let mut line_number = 0;
        let mut in_kill_collections = false;
        let mut header: Option<Vec<String>> = None;

        for line in reader.lines() {
            line_number += 1;
            let line = line?;

            // Check for corruption patterns
            for pattern in CORRUPTION_PATTERNS {
                if line.contains(pattern) {
                    analysis.corruption_issues.push(CorruptionIssue {
                        line_number,
                        issue_type: CorruptionType::KnownPattern,
                        description: format!("Found corruption pattern: {}", pattern),
                        line_content: line.clone(),
                    });
                }
            }

            // Check for malformed sections
            if line.starts_with('[') && !line.ends_with(']') && !line.contains("KILL_COLLECTIONS") {
                analysis.corruption_issues.push(CorruptionIssue {
                    line_number,
                    issue_type: CorruptionType::MalformedSection,
                    description: "Malformed section header".to_string(),
                    line_content: line.clone(),
                });
            }

            if line == "[KILL_COLLECTIONS]" {
                in_kill_collections = true;
                continue;
            }

            if in_kill_collections {
                if header.is_none() {
                    if line.starts_with("Type,") {
                        header = Some(line.split(',').map(|s| s.to_string()).collect());
                        continue;
                    }
                }

                if line.starts_with('[') {
                    in_kill_collections = false;
                    continue;
                }

                if let Some(ref h) = header {
                    let values: Vec<&str> = line.split(',').collect();

                    // Check for severely malformed CSV rows
                    if values.len() < h.len() / 2 {
                        analysis.corruption_issues.push(CorruptionIssue {
                            line_number,
                            issue_type: CorruptionType::MalformedCsvRow,
                            description: format!(
                                "CSV row has too few columns: {} vs expected {}",
                                values.len(),
                                h.len()
                            ),
                            line_content: line.clone(),
                        });
                        continue;
                    }

                    // Try to parse the collection
                    if values.len() >= h.len() {
                        match self.parse_collection_from_csv_row(h, &values) {
                            Ok(collection) => {
                                if self.validator.is_collection_corrupted(&collection) {
                                    analysis.corruption_issues.push(CorruptionIssue {
                                        line_number,
                                        issue_type: CorruptionType::InvalidData,
                                        description: "Collection data is corrupted".to_string(),
                                        line_content: line.clone(),
                                    });
                                } else {
                                    analysis.valid_collections += 1;
                                }
                            }
                            Err(_) => {
                                analysis.corruption_issues.push(CorruptionIssue {
                                    line_number,
                                    issue_type: CorruptionType::ParseError,
                                    description: "Failed to parse collection from CSV row"
                                        .to_string(),
                                    line_content: line.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        analysis.total_lines = line_number;
        Ok(analysis)
    }

    /// Parse a KillCollection from CSV row data (simplified version)
    fn parse_collection_from_csv_row(
        &self,
        header: &[String],
        values: &[&str],
    ) -> Result<KillCollection, String> {
        let mut collection = KillCollection {
            collection_type: String::new(),
            collection_num: 0,
            col_total: 0,
            tick_duration: 0,
            map_name: String::new(),
            killer_index: 0,
            killer_team: String::new(),
            start_kill_tick: 0,
            end_kill_tick: 0,
            killer_name: String::new(),
            killer_steamid: String::new(),
            demo_name: String::new(),
            demo_path: String::new(),
            folder: String::new(),
            killer_radius: 0.0,
            victims_radius: 0.0,
            killer_move_distance: 0.0,
            victim_team: String::new(),
            round_start_tick: 0,
            round_end_tick: 0,
            round_freeze_end: 0,
            round: 0,
            weapons: Vec::new(),
            weapons_id: Vec::new(),
            kill_ticks: Vec::new(),
            victim_indices: Vec::new(),
            kills: Vec::new(),
            game_version: 0,
            parsed: 0,
            grenade_traj: 0,
            tag: String::new(),
            util_thrown: String::new(),
            hits: 0,
            misses: 0,
            hit_rate: 0.0,
            util_thrown_ticks: String::new(),
            util_land_ticks: String::new(),
            weapons_damaged: String::new(),
            weapons_damaged_num_hits: String::new(),
            weapons_damaged_hits_formatted: String::new(),
        };

        for (i, field_name) in header.iter().enumerate() {
            if i >= values.len() {
                break;
            }

            let value = values[i];

            match field_name.as_str() {
                "Type" => collection.collection_type = value.to_string(),
                "CollectionNum" => {
                    collection.collection_num = value
                        .parse()
                        .map_err(|_| "Invalid CollectionNum".to_string())?
                }
                "TickDuration" => collection.tick_duration = value.parse().unwrap_or(0),
                "MapName" => collection.map_name = value.to_string(),
                "KillerIndex" => collection.killer_index = value.parse().unwrap_or(0),
                "KillerTeam" => collection.killer_team = value.to_string(),
                "StartKillTick" => collection.start_kill_tick = value.parse().unwrap_or(0),
                "EndKillTick" => collection.end_kill_tick = value.parse().unwrap_or(0),
                "KillerName" => collection.killer_name = value.to_string(),
                "SteamID" => collection.killer_steamid = value.to_string(),
                "DemoName" => collection.demo_name = value.to_string(),
                "Folder" => collection.folder = value.to_string(),
                "KillerRadius" => collection.killer_radius = value.parse().unwrap_or(0.0),
                "VictimsRadius" => collection.victims_radius = value.parse().unwrap_or(0.0),
                "KillerMoveDistance" => {
                    collection.killer_move_distance = value.parse().unwrap_or(0.0)
                }
                "VictimTeam" => collection.victim_team = value.to_string(),
                "RoundStartTick" => collection.round_start_tick = value.parse().unwrap_or(0),
                "RoundEndTick" => collection.round_end_tick = value.parse().unwrap_or(0),
                "RoundFreezeEnd" => collection.round_freeze_end = value.parse().unwrap_or(0),
                "Round" => collection.round = value.parse().unwrap_or(0),
                "Weapons" => {
                    let clean_value = value.trim_start_matches('[').trim_end_matches(']');
                    if !clean_value.is_empty() {
                        collection.weapons =
                            clean_value.split(';').map(|s| s.to_string()).collect();
                    }
                }
                "WeaponsID" => {
                    let clean_value = value.trim_start_matches('[').trim_end_matches(']');
                    if !clean_value.is_empty() {
                        collection.weapons_id =
                            clean_value.split(';').map(|s| s.to_string()).collect();
                    }
                }
                "KillTicks" => {
                    let clean_value = value.trim_start_matches('[').trim_end_matches(']');
                    if !clean_value.is_empty() {
                        collection.kill_ticks = clean_value
                            .split(';')
                            .filter_map(|s| s.parse().ok())
                            .collect();
                    }
                }
                "VictimsIndex" => {
                    let clean_value = value.trim_start_matches('[').trim_end_matches(']');
                    if !clean_value.is_empty() {
                        collection.victim_indices = clean_value
                            .split(';')
                            .filter_map(|s| s.parse().ok())
                            .collect();
                    }
                }
                "Tag" => collection.tag = value.to_string(),
                _ => {}
            }
        }

        Ok(collection)
    }

    /// Extract valid collections from a corrupted master file
    pub fn extract_valid_collections(
        &self,
        file_path: &str,
        collection_type: &str,
    ) -> io::Result<Vec<KillCollection>> {
        let mut valid_collections = Vec::new();

        if !Path::new(file_path).exists() {
            return Ok(valid_collections);
        }

        let file = File::open(file_path)?;
        let reader = BufReader::new(file);
        let mut in_kill_collections = false;
        let mut header: Option<Vec<String>> = None;

        for line in reader.lines() {
            let line = line?;

            if line.trim().is_empty() {
                continue;
            }

            if line == "[KILL_COLLECTIONS]" {
                in_kill_collections = true;
                continue;
            }

            if in_kill_collections {
                if header.is_none() {
                    if line.starts_with("Type,") {
                        header = Some(line.split(',').map(|s| s.to_string()).collect());
                        continue;
                    }
                }

                if line.starts_with('[') {
                    break; // Next section or corruption
                }

                if let Some(ref h) = header {
                    let values: Vec<&str> = line.split(',').collect();

                    // Skip obviously corrupted rows
                    if values.len() < h.len() / 2 {
                        continue;
                    }

                    // Skip rows with corruption patterns
                    let has_corruption = CORRUPTION_PATTERNS
                        .iter()
                        .any(|pattern| line.contains(pattern));
                    if has_corruption {
                        continue;
                    }

                    if values.len() >= h.len() {
                        if let Ok(collection) = self.parse_collection_from_csv_row(h, &values) {
                            // Only include collections that match the type and aren't corrupted
                            if collection.collection_type == collection_type
                                && !self.validator.is_collection_corrupted(&collection)
                            {
                                valid_collections.push(collection);
                            }
                        }
                    }
                }
            }
        }

        Ok(valid_collections)
    }

    /// Repair a corrupted master file
    pub fn repair_master_file(
        &self,
        file_path: &str,
        collection_type: &str,
        folder: &str,
    ) -> io::Result<RepairResult> {
        let mut result = RepairResult::new();

        // First, analyze the corruption
        let analysis = self.analyze_master_file(file_path)?;
        result.corruption_analysis = Some(analysis.clone());

        if !analysis.file_exists {
            result.success = false;
            result.error_message = Some("File does not exist".to_string());
            return Ok(result);
        }

        // Extract valid collections
        let valid_collections = self.extract_valid_collections(file_path, collection_type)?;
        result.collections_recovered = valid_collections.len();
        result.collections_lost = analysis.corruption_issues.len();

        if valid_collections.is_empty() {
            // If no valid collections found, create empty master file
            let master_writer = MasterWriter::new(file_path, collection_type, folder)?;
            master_writer.write_master_csv(&[], None)?;
            result.success = true;
            result.action_taken =
                "Created empty master file (no valid collections found)".to_string();
        } else {
            // Create backup of original file
            let backup_path = format!("{}.corrupt_backup", file_path);
            std::fs::copy(file_path, &backup_path)?;
            result.backup_created = Some(backup_path);

            // Write the repaired master file
            let master_writer = MasterWriter::new(file_path, collection_type, folder)?;
            master_writer.write_master_csv(&valid_collections, None)?;

            result.success = true;
            result.action_taken = format!(
                "Repaired master file: {} collections recovered",
                valid_collections.len()
            );
        }

        Ok(result)
    }

    /// Repair all corrupted master files in a directory
    pub fn repair_directory(&self, directory_path: &str) -> io::Result<Vec<RepairResult>> {
        let mut results = Vec::new();

        let dir = std::fs::read_dir(directory_path)?;

        for entry in dir {
            let entry = entry?;
            let path = entry.path();

            if path.is_file() {
                if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                    if filename.ends_with("_Master.csv") {
                        // Extract collection type and folder from filename
                        let parts: Vec<&str> = filename.split('_').collect();
                        if parts.len() >= 3 {
                            let collection_type = parts[0];
                            let folder = parts[1];

                            println!("Analyzing and repairing: {}", filename);
                            match self.repair_master_file(
                                path.to_str().unwrap(),
                                collection_type,
                                folder,
                            ) {
                                Ok(result) => {
                                    if !result.success {
                                        eprintln!(
                                            "Failed to repair {}: {:?}",
                                            filename, result.error_message
                                        );
                                    } else {
                                        println!(
                                            "Successfully repaired {}: {}",
                                            filename, result.action_taken
                                        );
                                    }
                                    results.push(result);
                                }
                                Err(e) => {
                                    eprintln!("Error repairing {}: {}", filename, e);
                                    let mut error_result = RepairResult::new();
                                    error_result.success = false;
                                    error_result.error_message = Some(e.to_string());
                                    results.push(error_result);
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(results)
    }
}

/// Analysis result for a master file
#[derive(Debug, Clone)]
pub struct CorruptionAnalysis {
    pub file_path: String,
    pub file_exists: bool,
    pub total_lines: usize,
    pub valid_collections: usize,
    pub corruption_issues: Vec<CorruptionIssue>,
}

impl CorruptionAnalysis {
    fn new(file_path: String) -> Self {
        CorruptionAnalysis {
            file_path,
            file_exists: true,
            total_lines: 0,
            valid_collections: 0,
            corruption_issues: Vec::new(),
        }
    }

    pub fn is_corrupted(&self) -> bool {
        !self.corruption_issues.is_empty()
    }

    pub fn severity(&self) -> CorruptionSeverity {
        if self.corruption_issues.is_empty() {
            CorruptionSeverity::None
        } else if self.corruption_issues.len() < 5 && self.valid_collections > 0 {
            CorruptionSeverity::Minor
        } else if self.valid_collections == 0 {
            CorruptionSeverity::Severe
        } else {
            CorruptionSeverity::Major
        }
    }
}

/// Individual corruption issue
#[derive(Debug, Clone)]
pub struct CorruptionIssue {
    pub line_number: usize,
    pub issue_type: CorruptionType,
    pub description: String,
    pub line_content: String,
}

/// Types of corruption that can be detected
#[derive(Debug, Clone)]
pub enum CorruptionType {
    KnownPattern,
    MalformedSection,
    MalformedCsvRow,
    InvalidData,
    ParseError,
}

/// Severity levels for corruption
#[derive(Debug, Clone)]
pub enum CorruptionSeverity {
    None,
    Minor,
    Major,
    Severe,
}

/// Result of a repair operation
#[derive(Debug)]
pub struct RepairResult {
    pub success: bool,
    pub action_taken: String,
    pub collections_recovered: usize,
    pub collections_lost: usize,
    pub backup_created: Option<String>,
    pub error_message: Option<String>,
    pub corruption_analysis: Option<CorruptionAnalysis>,
}

impl RepairResult {
    fn new() -> Self {
        RepairResult {
            success: false,
            action_taken: String::new(),
            collections_recovered: 0,
            collections_lost: 0,
            backup_created: None,
            error_message: None,
            corruption_analysis: None,
        }
    }
}

impl Default for CleanupManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_analyze_corrupted_file() {
        let cleanup_manager = CleanupManager::new();

        // Create a temporary corrupted file
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "[MANIFEST_INFO]").unwrap();
        writeln!(temp_file, "Type,TRIPLE").unwrap();
        writeln!(temp_file, "").unwrap();
        writeln!(temp_file, "[KILL_COLLECTIONS]").unwrap();
        writeln!(temp_file, "Type,CollectionNum,TickDuration,MapName").unwrap();
        writeln!(
            temp_file,
            "TRIPLE,2,113,de_3,8556,de7166,907,2,28881,29736,ketoky"
        )
        .unwrap(); // Corrupted line
        writeln!(
            temp_file,
            "TRIPLE,3,655,de_ancient,5,3,61926,62581,Zyxitan,76561198327058670"
        )
        .unwrap(); // Valid line

        let analysis = cleanup_manager
            .analyze_master_file(temp_file.path().to_str().unwrap())
            .unwrap();

        assert!(analysis.is_corrupted());
        assert!(analysis.corruption_issues.len() > 0);
    }

    #[test]
    fn test_extract_valid_collections() {
        let cleanup_manager = CleanupManager::new();

        // Create a temporary file with mixed valid and corrupt data
        let mut temp_file = NamedTempFile::new().unwrap();
        writeln!(temp_file, "[KILL_COLLECTIONS]").unwrap();
        writeln!(temp_file, "Type,CollectionNum,TickDuration,MapName,KillerIndex,KillerTeam,StartKillTick,EndKillTick,KillerName,SteamID,DemoName,KillerRadius,VictimsRadius,KillerMoveDistance,VictimTeam,RoundStartTick,RoundEndTick,RoundFreezeEnd,Round,Weapons,WeaponsID,KillTicks,VictimsIndex").unwrap();
        writeln!(temp_file, "TRIPLE,3,655,de_ancient,5,3,61926,62581,TestPlayer,76561198327058670,test_demo,108.48,294.05,321.36,2,58761,62926,60021,11,[ak47;ak47;ak47],[7;7;7],[61926;62315;62581],[11;10;6]").unwrap();
        writeln!(
            temp_file,
            "TRIPLE,2,113,de_3,8556,de7166,907,2,28881,29736,ketoky[[corrupted]]"
        )
        .unwrap(); // Corrupted

        let collections = cleanup_manager
            .extract_valid_collections(temp_file.path().to_str().unwrap(), "TRIPLE")
            .unwrap();

        assert_eq!(collections.len(), 1);
        assert_eq!(collections[0].killer_name, "TestPlayer");
    }
}
