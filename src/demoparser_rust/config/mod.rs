//! Configuration management for the demoparser_rust project
//!
//! This module handles loading and parsing configuration from config.ini files
//! and CLI argument overrides.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub mod storage;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub paths: PathsConfig,
    pub parser: ParserConfig,
    pub downloader: DownloaderConfig,
    pub batch: BatchConfig,
}

#[derive(Debug, Clone)]
pub struct PathsConfig {
    pub parser_output: PathBuf,
    pub unzip_dir: Option<PathBuf>,
    pub ram_unzip: bool,
}

#[derive(Debug, Clone)]
pub struct ParserConfig {
    pub process_tick_data: bool,
    pub pad_ticks: i32,
    pub skip_buy_time: bool,
    pub track_all_players: bool,
    pub aces: bool,
    pub quads: bool,
    pub triples: bool,
    pub multi: bool,
    pub singles: bool,
    pub doubles: bool,
    pub catalog_aces: bool,
    pub catalog_quads: bool,
    pub catalog_triples: bool,
    pub catalog_multi: bool,
    pub catalog_singles: bool,
    pub catalog_doubles: bool,
    pub trim_aces: bool,
    pub trim_quads: bool,
    pub trim_triples: bool,
    pub trim_multi: bool,
    pub trim_singles: bool,
    pub trim_doubles: bool,
    pub overwrite: bool,
    /// Emit one verified playable DEM per trim-enabled round during import.
    pub trim_collection_rounds: bool,
    /// Remove a redundant raw .dem only after every requested clip and DuckDB asset row commits;
    /// a .dem.gz or .dem.zst archive sibling is mandatory and is never deleted.
    pub delete_source_after_trim: bool,
}

#[derive(Debug, Clone)]
pub struct DownloaderConfig {
    pub batch_size: usize,
}

#[derive(Debug, Clone)]
pub struct BatchConfig {
    pub threads: Option<usize>,
    pub prefetch_depth: usize,
    pub max_concurrent_files: usize,
    pub max_retries: usize,
    pub retry_delay: f64,
    pub show_progress: bool,
    pub autoclose: bool,
}

#[derive(Debug)]
pub enum ConfigError {
    IoError(std::io::Error),
    ParseError(String),
    NotFound(String),
}

impl From<std::io::Error> for ConfigError {
    fn from(error: std::io::Error) -> Self {
        ConfigError::IoError(error)
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::IoError(e) => write!(f, "IO error: {}", e),
            ConfigError::ParseError(msg) => write!(f, "Parse error: {}", msg),
            ConfigError::NotFound(msg) => write!(f, "Not found: {}", msg),
        }
    }
}

impl std::error::Error for ConfigError {}

impl AppConfig {
    /// Load configuration from config.ini file located next to the executable
    pub fn load() -> Result<Self, ConfigError> {
        let config_path = Self::find_config_file()?;
        Self::load_from_file(&config_path)
    }

    /// Load configuration from a specific file path
    pub fn load_from_file(config_path: &Path) -> Result<Self, ConfigError> {
        let content = fs::read_to_string(config_path)?;
        Self::parse_ini_content(&content)
    }

    /// Find the config.ini file next to the executable
    fn find_config_file() -> Result<PathBuf, ConfigError> {
        // Try to get the directory where the executable is located
        let exe_path = env::current_exe().map_err(|e| {
            ConfigError::ParseError(format!("Failed to get executable path: {}", e))
        })?;

        let exe_dir = exe_path.parent().ok_or_else(|| {
            ConfigError::ParseError("Failed to get executable directory".to_string())
        })?;

        let config_path = exe_dir.join("config.ini");

        if config_path.exists() {
            Ok(config_path)
        } else {
            // Fallback: try current working directory
            let cwd_config = PathBuf::from("config.ini");
            if cwd_config.exists() {
                Ok(cwd_config)
            } else {
                Err(ConfigError::NotFound(format!(
                    "config.ini not found in {} or current directory",
                    exe_dir.display()
                )))
            }
        }
    }

    /// Parse INI file content
    fn parse_ini_content(content: &str) -> Result<Self, ConfigError> {
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut current_section: Option<String> = None;

        for line in content.lines() {
            let line = line.trim();

            // Skip empty lines and comments
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }

            // Check for section headers
            if line.starts_with('[') && line.ends_with(']') {
                let section_name = line[1..line.len() - 1].to_string();
                current_section = Some(section_name.clone());
                sections.entry(section_name).or_insert_with(HashMap::new);
                continue;
            }

            // Parse key-value pairs
            if let Some(ref section) = current_section {
                if let Some(eq_pos) = line.find('=') {
                    let key = line[..eq_pos].trim().to_string();
                    let value = line[eq_pos + 1..].trim().to_string();

                    sections.get_mut(section).unwrap().insert(key, value);
                }
            }
        }

        Self::build_config_from_sections(sections)
    }

    /// Build AppConfig from parsed sections
    fn build_config_from_sections(
        sections: HashMap<String, HashMap<String, String>>,
    ) -> Result<Self, ConfigError> {
        // Parse [Paths] section
        let paths_section = sections
            .get("Paths")
            .ok_or_else(|| ConfigError::ParseError("[Paths] section not found".to_string()))?;

        let parser_output = paths_section.get("ParserOutput").ok_or_else(|| {
            ConfigError::ParseError("ParserOutput not found in [Paths]".to_string())
        })?;

        let unzip_dir = paths_section
            .get("unzip_dir")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        let ram_unzip = Self::parse_bool(paths_section, "ram_unzip", false)?;

        let paths = PathsConfig {
            parser_output: PathBuf::from(parser_output),
            unzip_dir,
            ram_unzip,
        };

        // Parse [Parser] section
        let parser_section = sections
            .get("Parser")
            .ok_or_else(|| ConfigError::ParseError("[Parser] section not found".to_string()))?;

        // Replay filters control expensive S2R generation.  Legacy configs used the short
        // names (Aces, Quads, ...), so Replay* falls back to those keys.  Catalog filters are
        // deliberately independent and default on: the GUI can show every discovered
        // collection without paying to build a replay for every one.
        let legacy_aces = Self::parse_bool(parser_section, "Aces", true)?;
        let legacy_quads = Self::parse_bool(parser_section, "Quads", false)?;
        let legacy_triples = Self::parse_bool(parser_section, "Triples", false)?;
        let legacy_multi = Self::parse_bool(parser_section, "Multi", true)?;
        let legacy_singles = Self::parse_bool(parser_section, "Singles", true)?;
        let legacy_doubles = Self::parse_bool(parser_section, "Doubles", true)?;
        let parser = ParserConfig {
            skip_buy_time: Self::parse_bool(parser_section, "SkipBuyTime", true)?,
            process_tick_data: Self::parse_bool(parser_section, "process_tick_data", true)?,
            pad_ticks: Self::parse_i32(parser_section, "PadTicks", 0)?,
            track_all_players: Self::parse_bool(parser_section, "TrackAllPlayers", true)?,
            aces: Self::parse_bool(parser_section, "ReplayAces", legacy_aces)?,
            quads: Self::parse_bool(parser_section, "ReplayQuads", legacy_quads)?,
            triples: Self::parse_bool(parser_section, "ReplayTriples", legacy_triples)?,
            multi: Self::parse_bool(parser_section, "ReplayMulti", legacy_multi)?,
            singles: Self::parse_bool(parser_section, "ReplaySingles", legacy_singles)?,
            doubles: Self::parse_bool(parser_section, "ReplayDoubles", legacy_doubles)?,
            catalog_aces: Self::parse_bool(parser_section, "CatalogAces", true)?,
            catalog_quads: Self::parse_bool(parser_section, "CatalogQuads", true)?,
            catalog_triples: Self::parse_bool(parser_section, "CatalogTriples", true)?,
            catalog_multi: Self::parse_bool(parser_section, "CatalogMulti", true)?,
            catalog_singles: Self::parse_bool(parser_section, "CatalogSingles", true)?,
            catalog_doubles: Self::parse_bool(parser_section, "CatalogDoubles", true)?,
            // Trim filters are independent from both catalog and replay filters. Preserve the
            // integrated trimmer's historical all-types behavior unless a config opts out.
            trim_aces: Self::parse_bool(parser_section, "TrimAces", true)?,
            trim_quads: Self::parse_bool(parser_section, "TrimQuads", true)?,
            trim_triples: Self::parse_bool(parser_section, "TrimTriples", true)?,
            trim_multi: Self::parse_bool(parser_section, "TrimMulti", true)?,
            trim_singles: Self::parse_bool(parser_section, "TrimSingles", true)?,
            trim_doubles: Self::parse_bool(parser_section, "TrimDoubles", true)?,
            overwrite: Self::parse_bool(parser_section, "overwrite", false)?,
            trim_collection_rounds: Self::parse_bool(
                parser_section,
                "TrimCollectionRounds",
                false,
            )?,
            delete_source_after_trim: Self::parse_bool(
                parser_section,
                "DeleteSourceAfterTrim",
                false,
            )?,
        };

        // Parse [Downloader] section
        let downloader_section = sections
            .get("Downloader")
            .ok_or_else(|| ConfigError::ParseError("[Downloader] section not found".to_string()))?;

        let downloader = DownloaderConfig {
            batch_size: Self::parse_usize(downloader_section, "batch_size", 32)?,
        };

        // Parse [Batch] section (optional, use defaults if not present)
        let batch = if let Some(batch_section) = sections.get("Batch") {
            BatchConfig {
                threads: Self::parse_optional_usize(batch_section, "threads")?,
                prefetch_depth: Self::parse_usize(batch_section, "prefetch_depth", 1)?,
                // 0 = auto: build contiguous batches from individual estimates, bounded by
                // available memory and the parsing pool.
                max_concurrent_files: Self::parse_usize(batch_section, "max_concurrent_files", 0)?,
                max_retries: Self::parse_usize(batch_section, "max_retries", 3)?,
                retry_delay: Self::parse_f64(batch_section, "retry_delay", 1.0)?,
                show_progress: Self::parse_bool(batch_section, "show_progress", true)?,
                autoclose: Self::parse_bool(batch_section, "autoclose", true)?,
            }
        } else {
            // Default batch configuration
            BatchConfig {
                threads: None,           // Auto-detect
                prefetch_depth: 1,       // Default: decompress N+1 while processing N
                max_concurrent_files: 0, // Auto-size from individual memory estimates + pool
                max_retries: 3,
                retry_delay: 1.0,
                show_progress: true,
                autoclose: true,
            }
        };

        Ok(AppConfig {
            paths,
            parser,
            downloader,
            batch,
        })
    }

    // Helper parsing functions
    fn parse_i32(
        section: &HashMap<String, String>,
        key: &str,
        default: i32,
    ) -> Result<i32, ConfigError> {
        if let Some(value) = section.get(key) {
            value.parse().map_err(|e| {
                ConfigError::ParseError(format!("Failed to parse {} as i32: {}", key, e))
            })
        } else {
            Ok(default)
        }
    }

    fn parse_usize(
        section: &HashMap<String, String>,
        key: &str,
        default: usize,
    ) -> Result<usize, ConfigError> {
        if let Some(value) = section.get(key) {
            value.parse().map_err(|e| {
                ConfigError::ParseError(format!("Failed to parse {} as usize: {}", key, e))
            })
        } else {
            Ok(default)
        }
    }

    fn parse_optional_usize(
        section: &HashMap<String, String>,
        key: &str,
    ) -> Result<Option<usize>, ConfigError> {
        if let Some(value) = section.get(key) {
            if value == "0" {
                Ok(None) // 0 means auto-detect
            } else {
                Ok(Some(value.parse().map_err(|e| {
                    ConfigError::ParseError(format!("Failed to parse {} as usize: {}", key, e))
                })?))
            }
        } else {
            Ok(None)
        }
    }

    fn parse_f64(
        section: &HashMap<String, String>,
        key: &str,
        default: f64,
    ) -> Result<f64, ConfigError> {
        if let Some(value) = section.get(key) {
            value.parse().map_err(|e| {
                ConfigError::ParseError(format!("Failed to parse {} as f64: {}", key, e))
            })
        } else {
            Ok(default)
        }
    }

    fn parse_bool(
        section: &HashMap<String, String>,
        key: &str,
        default: bool,
    ) -> Result<bool, ConfigError> {
        if let Some(value) = section.get(key) {
            match value.to_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Ok(true),
                "false" | "0" | "no" | "off" => Ok(false),
                _ => Err(ConfigError::ParseError(format!(
                    "Failed to parse {} as bool: {}",
                    key, value
                ))),
            }
        } else {
            Ok(default)
        }
    }

    /// Get the optimal number of threads for parallel processing
    pub fn get_optimal_threads(&self) -> usize {
        self.batch.threads.unwrap_or_else(|| {
            // Auto-detect: use 100% of available cores, minimum 1
            let available_cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4); // Default to 4 if detection fails

            available_cores.max(1)
        })
    }

    /// Check if a collection type is enabled
    pub fn is_collection_type_enabled(&self, collection_type: &str) -> bool {
        match collection_type.to_uppercase().as_str() {
            "ACE" => self.parser.aces,
            "QUAD" => self.parser.quads,
            "TRIPLE" => self.parser.triples,
            "MULTI" => self.parser.multi,
            "SINGLE" => self.parser.singles,
            "DOUBLE" => self.parser.doubles,
            _ => false,
        }
    }

    /// Check whether a discovered collection belongs in the DuckDB catalog.
    pub fn is_catalog_collection_type_enabled(&self, collection_type: &str) -> bool {
        match collection_type.to_uppercase().as_str() {
            "ACE" => self.parser.catalog_aces,
            "QUAD" => self.parser.catalog_quads,
            "TRIPLE" => self.parser.catalog_triples,
            "MULTI" => self.parser.catalog_multi,
            "SINGLE" => self.parser.catalog_singles,
            "DOUBLE" => self.parser.catalog_doubles,
            _ => false,
        }
    }

    /// Check whether a catalogued round should produce a verified DEM clip. A round is selected
    /// when any collection in it has a trim-enabled type; every collection in a selected round
    /// still receives the shared DEM asset row.
    pub fn is_trim_collection_type_enabled(&self, collection_type: &str) -> bool {
        match collection_type.to_uppercase().as_str() {
            "ACE" => self.parser.trim_aces,
            "QUAD" => self.parser.trim_quads,
            "TRIPLE" => self.parser.trim_triples,
            "MULTI" => self.parser.trim_multi,
            "SINGLE" => self.parser.trim_singles,
            "DOUBLE" => self.parser.trim_doubles,
            _ => false,
        }
    }

    /// Get the master CSV output directory
    pub fn get_master_output_dir(&self) -> PathBuf {
        self.paths.parser_output.join("KillCollectionMaster")
    }

    /// Get the kill collections output directory
    pub fn get_kill_collections_output_dir(&self) -> PathBuf {
        self.paths.parser_output.join("KillCollections")
    }

    /// Get padding header value for CSV output
    pub fn get_padding_header_value(&self) -> i32 {
        self.parser.pad_ticks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ini_content() {
        let content = r#"
[Paths]
ParserOutput = C:\demofetch\DiscordBot\parsed

[Parser]
process_tick_data=true
PadTicks = 0
TrackAllPlayers = 1
Aces = true
Quads = false

[Downloader]
batch_size = 32
"#;

        let config = AppConfig::parse_ini_content(content).unwrap();
        assert_eq!(config.parser.process_tick_data, true);
        assert_eq!(config.parser.pad_ticks, 0);
        assert_eq!(config.parser.track_all_players, true);
        assert_eq!(config.parser.aces, true);
        assert_eq!(config.parser.quads, false);
        assert_eq!(config.downloader.batch_size, 32);
    }

    #[test]
    fn test_collection_type_enabled() {
        let content = r#"
[Paths]
ParserOutput = /tmp

[Parser]
Aces = true
Quads = false
Triples = true

[Downloader]
batch_size = 32
"#;

        let config = AppConfig::parse_ini_content(content).unwrap();
        assert!(config.is_collection_type_enabled("ACE"));
        assert!(!config.is_collection_type_enabled("QUAD"));
        assert!(config.is_collection_type_enabled("TRIPLE"));
    }

    #[test]
    fn catalog_and_replay_filters_are_independent_with_legacy_fallback() {
        let content = r#"
[Paths]
ParserOutput = /tmp

[Parser]
Aces = false
ReplayAces = true
ReplayTriples = false
CatalogAces = true
CatalogTriples = true
TrimAces = true
TrimTriples = false

[Downloader]
batch_size = 32
"#;

        let config = AppConfig::parse_ini_content(content).unwrap();
        assert!(config.is_collection_type_enabled("ACE"));
        assert!(!config.is_collection_type_enabled("TRIPLE"));
        assert!(config.is_catalog_collection_type_enabled("ACE"));
        assert!(config.is_catalog_collection_type_enabled("TRIPLE"));
        assert!(config.is_catalog_collection_type_enabled("DOUBLE"));
        assert!(config.is_trim_collection_type_enabled("ACE"));
        assert!(!config.is_trim_collection_type_enabled("TRIPLE"));
        assert!(config.is_trim_collection_type_enabled("DOUBLE"));
    }
}
