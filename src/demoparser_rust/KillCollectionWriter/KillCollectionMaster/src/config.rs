//! Configuration management for the demoparser_rust project
//!
//! This module handles loading and parsing configuration from config.ini files
//! and CLI argument overrides.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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
}

#[derive(Debug, Clone)]
pub struct ParserConfig {
    pub process_tick_data: bool,
    pub pad_ticks: i32,
    pub track_all_players: bool,
    // master_file_format removed
    pub aces: bool,
    pub quads: bool,
    pub triples: bool,
    pub multi: bool,
    pub singles: bool,
    pub doubles: bool,
}

#[derive(Debug, Clone)]
pub struct DownloaderConfig {
    pub batch_size: usize,
}

#[derive(Debug, Clone)]
pub struct BatchConfig {
    pub threads: Option<usize>,
    pub max_concurrent_files: usize,
    pub max_retries: usize,
    pub retry_delay: f64,
    pub show_progress: bool,
}

// ... (Rest of file similar to config/mod.rs but without implementations that are unused here?
// Actually MasterWriter creates its own config using `load()`, so it needs implementations.
// But wait, inside Demoparser we construct it manually using `demo_processor.rs`.
// If `KillCollectionMaster` binary is deprecated/removed, we only use the struct definition.
// I'll include the struct definition mainly. )

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
    pub fn load() -> Result<Self, ConfigError> {
        // ... Implementation needed for standalone usage, but maybe not if only used as library.
        // Keeping standard impl for safety.
        let config_path = Self::find_config_file()?;
        Self::load_from_file(&config_path)
    }

    pub fn load_from_file(config_path: &Path) -> Result<Self, ConfigError> {
        let content = fs::read_to_string(config_path)?;
        Self::parse_ini_content(&content)
    }

    fn find_config_file() -> Result<PathBuf, ConfigError> {
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
            Err(ConfigError::NotFound(format!(
                "config.ini not found in {}",
                exe_dir.display()
            )))
        }
    }

    fn parse_ini_content(content: &str) -> Result<Self, ConfigError> {
        // Simplified parsing logic or full logic?
        // Full logic to be safe.
        let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut current_section: Option<String> = None;
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') && line.ends_with(']') {
                let section_name = line[1..line.len() - 1].to_string();
                current_section = Some(section_name.clone());
                sections.entry(section_name).or_insert_with(HashMap::new);
                continue;
            }
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

    fn build_config_from_sections(
        sections: HashMap<String, HashMap<String, String>>,
    ) -> Result<Self, ConfigError> {
        let paths_section = sections
            .get("Paths")
            .ok_or_else(|| ConfigError::ParseError("[Paths] section not found".to_string()))?;
        let parser_output = paths_section
            .get("ParserOutput")
            .ok_or_else(|| ConfigError::ParseError("ParserOutput not found".to_string()))?;

        let parser_section = sections
            .get("Parser")
            .ok_or_else(|| ConfigError::ParseError("[Parser] section not found".to_string()))?;

        let downloader_section = sections
            .get("Downloader")
            .ok_or_else(|| ConfigError::ParseError("[Downloader] section not found".to_string()))?;

        Ok(AppConfig {
            paths: PathsConfig {
                parser_output: PathBuf::from(parser_output),
            },
            parser: ParserConfig {
                process_tick_data: Self::parse_bool(parser_section, "process_tick_data", true)?,
                pad_ticks: Self::parse_i32(parser_section, "PadTicks", 0)?,
                track_all_players: Self::parse_bool(parser_section, "TrackAllPlayers", true)?,
                aces: Self::parse_bool(parser_section, "Aces", true)?,
                quads: Self::parse_bool(parser_section, "Quads", false)?,
                triples: Self::parse_bool(parser_section, "Triples", false)?,
                multi: Self::parse_bool(parser_section, "Multi", true)?,
                singles: Self::parse_bool(parser_section, "Singles", true)?,
                doubles: Self::parse_bool(parser_section, "Doubles", true)?,
            },
            downloader: DownloaderConfig {
                batch_size: Self::parse_usize(downloader_section, "batch_size", 32)?,
            },
            batch: BatchConfig {
                threads: None,
                max_concurrent_files: 16,
                max_retries: 3,
                retry_delay: 1.0,
                show_progress: true,
            },
        })
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
                _ => Ok(default),
            }
        } else {
            Ok(default)
        }
    }

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

    pub fn get_master_output_dir(&self) -> PathBuf {
        self.paths.parser_output.join("KillCollectionMaster")
    }
}
