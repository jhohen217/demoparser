use super::cli::{CliArgs, CollectionTypeFilter};
use anyhow::{anyhow, Result};
use configparser::ini::Ini;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub parser_output: Option<PathBuf>,
    pub unzip_dir: Option<PathBuf>,
    pub ram_unzip: bool,
    pub pad_ticks: i32,
    pub skip_buy_time: bool,
    pub track_all_players: bool,
    pub collection_types: ConfigCollectionTypes,
    pub batch_threads: u32,
    /// Parsed and stored, but currently INERT: no code path uses it to gate concurrency.
    /// Actual parallelism comes from the Rayon thread pool (see `batch_threads`).
    /// Either wire this into a real memory/concurrency budget - a semaphore around demo
    /// loading is the natural place, and matters more once a demo is parsed once rather
    /// than once per collection - or remove it. See documentation/plans/03-memory-and-io.md.
    pub max_concurrent_files: u32,
    pub max_retries: u32,
    pub retry_delay: f64,
    pub show_progress: bool,
    // Overwrite settings - unified overwrite setting for entire pipeline
    pub overwrite: bool,
    // Grenade trajectory parsing mode
    pub grenade_trajectory_mode: u8,
}

#[derive(Debug, Clone)]
pub struct ConfigCollectionTypes {
    pub aces: bool,
    pub quads: bool,
    pub triples: bool,
    pub multi: bool,
    pub singles: bool,
    pub doubles: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            parser_output: None,
            unzip_dir: None,
            ram_unzip: false,
            pad_ticks: 0,
            skip_buy_time: true,
            track_all_players: true,
            collection_types: ConfigCollectionTypes {
                aces: true,
                quads: true,
                triples: true,
                multi: true,
                singles: true,
                doubles: true,
            },
            batch_threads: 0,
            max_concurrent_files: 16,
            max_retries: 3,
            retry_delay: 1.0,
            show_progress: true,
            // Unified overwrite setting defaults - controls entire pipeline
            overwrite: false,
            // Grenade trajectory parsing mode default
            grenade_trajectory_mode: 1,
        }
    }
}

pub fn load_config(config_path: Option<&Path>) -> Result<AppConfig> {
    let mut config = AppConfig::default();

    // Determine config file path
    let config_file = if let Some(path) = config_path {
        path.to_path_buf()
    } else {
        // Search for config.ini upwards from the executable's directory
        let exe_path =
            std::env::current_exe().map_err(|e| anyhow!("Failed to get executable path: {}", e))?;
        let mut current_dir = exe_path
            .parent()
            .ok_or_else(|| anyhow!("Failed to get executable directory"))?;

        loop {
            let potential_path = current_dir.join("config.ini");
            if potential_path.exists() {
                break potential_path;
            }
            if let Some(parent) = current_dir.parent() {
                current_dir = parent;
            } else {
                // Reached the root, config not found. Fallback to original logic for error message.
                let exe_dir = exe_path.parent().unwrap();
                break exe_dir.join("config.ini");
            }
        }
    };

    if !config_file.exists() {
        return Err(anyhow!(
            "Could not find config.ini at: {}",
            config_file.display()
        ));
    }

    let mut ini = Ini::new();
    ini.load(&config_file)
        .map_err(|e| anyhow!("Failed to load config file: {}", e))?;

    // Parse [Paths] section
    if let Some(parser_output) = ini.get("Paths", "ParserOutput") {
        config.parser_output = Some(PathBuf::from(parser_output));
    }

    if let Some(unzip_dir) = ini.get("Paths", "unzip_dir") {
        let path = PathBuf::from(unzip_dir.trim());
        if !path.as_os_str().is_empty() {
            config.unzip_dir = Some(path);
        }
    }

    if let Some(ram_unzip) = ini.get("Paths", "ram_unzip") {
        config.ram_unzip = ram_unzip.trim().eq_ignore_ascii_case("true");
    }

    // Parse [Parser] section
    if let Some(skip) = ini.get("Parser", "SkipBuyTime") {
        config.skip_buy_time = skip.trim().eq_ignore_ascii_case("true");
    }
    if let Some(pad_ticks) = ini.get("Parser", "PadTicks") {
        config.pad_ticks = pad_ticks.parse().unwrap_or(0);
    }

    if let Some(track_all) = ini.get("Parser", "TrackAllPlayers") {
        config.track_all_players = track_all == "1";
    }

    // Replay* is the explicit S2R filter. Fall back to the legacy short keys so
    // existing standalone parser configs retain their behavior.
    if let Some(aces) = ini
        .get("Parser", "ReplayAces")
        .or_else(|| ini.get("Parser", "Aces"))
    {
        config.collection_types.aces = aces.to_lowercase() == "true";
    }
    if let Some(quads) = ini
        .get("Parser", "ReplayQuads")
        .or_else(|| ini.get("Parser", "Quads"))
    {
        config.collection_types.quads = quads.to_lowercase() == "true";
    }
    if let Some(triples) = ini
        .get("Parser", "ReplayTriples")
        .or_else(|| ini.get("Parser", "Triples"))
    {
        config.collection_types.triples = triples.to_lowercase() == "true";
    }
    if let Some(multi) = ini
        .get("Parser", "ReplayMulti")
        .or_else(|| ini.get("Parser", "Multi"))
    {
        config.collection_types.multi = multi.to_lowercase() == "true";
    }
    if let Some(singles) = ini
        .get("Parser", "ReplaySingles")
        .or_else(|| ini.get("Parser", "Singles"))
    {
        config.collection_types.singles = singles.to_lowercase() == "true";
    }
    if let Some(doubles) = ini
        .get("Parser", "ReplayDoubles")
        .or_else(|| ini.get("Parser", "Doubles"))
    {
        config.collection_types.doubles = doubles.to_lowercase() == "true";
    }

    // Parse overwrite settings - unified overwrite for entire pipeline
    if let Some(overwrite) = ini.get("Parser", "overwrite") {
        config.overwrite = overwrite.to_lowercase() == "true";
    }

    // Parse grenade trajectory mode
    if let Some(mode) = ini.get("Parser", "grenade_trajectory_mode") {
        config.grenade_trajectory_mode = mode.parse().unwrap_or(1);
    }

    // Parse [Batch] section
    if let Some(threads) = ini.get("Batch", "threads") {
        config.batch_threads = threads.parse().unwrap_or(0);
    }
    if let Some(max_concurrent) = ini.get("Batch", "max_concurrent_files") {
        config.max_concurrent_files = max_concurrent.parse().unwrap_or(16);
    }
    if let Some(max_retries) = ini.get("Batch", "max_retries") {
        config.max_retries = max_retries.parse().unwrap_or(3);
    }
    if let Some(retry_delay) = ini.get("Batch", "retry_delay") {
        config.retry_delay = retry_delay.parse().unwrap_or(1.0);
    }
    if let Some(show_progress) = ini.get("Batch", "show_progress") {
        config.show_progress = show_progress.to_lowercase() == "true";
    }

    Ok(config)
}

pub fn merge_cli_overrides(mut config: AppConfig, cli_args: &CliArgs) -> AppConfig {
    // Override track_all_players if provided
    if let Some(track_all) = cli_args.track_all_players {
        config.track_all_players = track_all;
    }

    // Override padding if provided
    if let Some(padding) = &cli_args.padding {
        if padding.to_lowercase() == "all" {
            config.pad_ticks = 0; // 0 means process full round
        } else if let Ok(ticks) = padding.parse::<i32>() {
            config.pad_ticks = ticks;
        }
    }

    // Override collection types if any CLI flags are set
    let cli_types = &cli_args.collection_types;
    if cli_types.all_types {
        // Enable all types
        config.collection_types.aces = true;
        config.collection_types.quads = true;
        config.collection_types.triples = true;
        config.collection_types.multi = true;
        config.collection_types.singles = true;
        config.collection_types.doubles = true;
    } else {
        // If any specific type flags are set, disable all others first, then enable only the specified ones
        if has_any_type_filter(cli_types) {
            config.collection_types.aces = false;
            config.collection_types.quads = false;
            config.collection_types.triples = false;
            config.collection_types.multi = false;
            config.collection_types.singles = false;
            config.collection_types.doubles = false;

            if cli_types.ace == Some(true) {
                config.collection_types.aces = true;
            }
            if cli_types.quad == Some(true) {
                config.collection_types.quads = true;
            }
            if cli_types.triple == Some(true) {
                config.collection_types.triples = true;
            }
            if cli_types.multi == Some(true) {
                config.collection_types.multi = true;
            }
            if cli_types.single == Some(true) {
                config.collection_types.singles = true;
            }
            if cli_types.double == Some(true) {
                config.collection_types.doubles = true;
            }
        }
    }

    config
}

fn has_any_type_filter(types: &CollectionTypeFilter) -> bool {
    types.ace.is_some()
        || types.quad.is_some()
        || types.triple.is_some()
        || types.multi.is_some()
        || types.single.is_some()
        || types.double.is_some()
}

impl AppConfig {
    pub fn should_process_collection_type(&self, collection_type: &str) -> bool {
        match collection_type.to_uppercase().as_str() {
            "ACE" => self.collection_types.aces,
            "QUAD" => self.collection_types.quads,
            "TRIPLE" => self.collection_types.triples,
            "MULTI" => self.collection_types.multi,
            "SINGLE" => self.collection_types.singles,
            "DOUBLE" => self.collection_types.doubles,
            _ => false,
        }
    }
}
