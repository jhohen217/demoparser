use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Represents a single kill collection entry from DuckDB
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionEntry {
    // Primary key fields
    pub steam_id: String,
    pub demo_name: String,
    pub round: i32,

    // Collection metadata
    pub collection_type: String,
    pub collection_num: i32,
    pub tick_duration: i32,

    // Map and player info
    pub map_name: String,
    pub killer_index: i32,
    pub killer_team: String,
    pub killer_name: String,

    // Timing
    pub start_kill_tick: i32,
    pub end_kill_tick: i32,
    pub round_start_tick: i32,
    pub round_end_tick: i32,
    pub round_freeze_end: i32,

    // Geometry
    pub killer_radius: f64,
    pub victims_radius: f64,
    pub killer_move_distance: f64,

    // Victim info
    pub victim_team: String,
    pub victims_index: String,
    pub victims_names: String,

    // Weapons
    pub weapons: String,
    pub weapons_id: String,
    pub kill_ticks: String,

    // Extended Physics
    pub grenade_traj: i32,
    pub movement_between_kills: String,

    // Optimization flag
    pub tick_data: i32,

    // On-demand loaded fields
    pub col_total: Option<i32>,
    pub killer_pos_x: Option<String>,
    pub killer_pos_y: Option<String>,
    pub killer_pos_z: Option<String>,
    pub killer_view_pitch: Option<String>,
    pub killer_view_yaw: Option<String>,
    pub ticks_between_kills: Option<String>,
    pub victim_distance: Option<String>,
    pub kill_weapon_ids: Option<String>,
    pub victim_pos_x: Option<String>,
    pub victim_pos_y: Option<String>,
    pub victim_pos_z: Option<String>,

    // Stats
    pub hits: i32,
    pub misses: i32,
    pub hit_rate: f64,
    pub weapons_formatted: String,

    // Metadata
    pub game_version: i32,
    pub tag: String,
    pub util_thrown: String,
    pub created_at: String,
    pub demo_relative_path: String,

    // UI state
    #[serde(skip)]
    pub selected: bool,
    #[serde(skip)]
    pub details_loaded: bool,

    // Source database
    #[serde(skip)]
    pub source_db: String,

    // Cached display strings for performance
    #[serde(skip)]
    pub duration_display: String,
    #[serde(skip)]
    pub killer_radius_display: String,
    #[serde(skip)]
    pub victims_radius_display: String,
    #[serde(skip)]
    pub move_distance_display: String,
    #[serde(skip)]
    pub hit_rate_display: String,
    #[serde(skip)]
    pub weapons_display: String,
}

impl CollectionEntry {
    /// Initialize cached display strings
    pub fn cache_display_strings(&mut self) {
        self.duration_display = format!("{:.1}s", self.tick_duration as f64 / 64.0);
        self.killer_radius_display = format!("{:.1}", self.killer_radius);
        self.victims_radius_display = format!("{:.1}", self.victims_radius);
        self.move_distance_display = format!("{:.1}", self.killer_move_distance);
        self.hit_rate_display = format!("{:.1}%", self.hit_rate * 100.0);

        // Cache formatted weapons display
        self.weapons_display = if self.tick_data == 1 && !self.weapons_formatted.is_empty() {
            self.weapons_formatted.clone()
        } else {
            // Format weapons field with " - " separator (replace semicolons)
            self.weapons.replace(';', " - ")
        };
    }

    /// Get the folder name from source_db
    pub fn folder(&self) -> String {
        std::path::Path::new(&self.source_db)
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| {
                // Parse {TYPE}_{folder}.duckdb
                s.split('_').skip(1).collect::<Vec<_>>().join("_")
            })
            .unwrap_or_default()
    }

    /// Get weapons display string - uses cached formatted version
    pub fn display_weapons(&self) -> &str {
        &self.weapons_display
    }
}

/// Collection type filter
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CollectionType {
    Ace,
    Quad,
    Triple,
    Double,
    Multi,
}

impl CollectionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ace => "ACE",
            Self::Quad => "QUAD",
            Self::Triple => "TRIPLE",
            Self::Double => "DOUBLE",
            Self::Multi => "MULTI",
        }
    }

    pub fn all() -> Vec<Self> {
        vec![
            Self::Ace,
            Self::Quad,
            Self::Multi,
            Self::Triple,
            Self::Double,
        ]
    }
}

/// Tick Filter state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TickFilterState {
    All,
    TickOnly,
    NoTick,
}

impl TickFilterState {
    pub fn next(&self) -> Self {
        match self {
            Self::All => Self::TickOnly,
            Self::TickOnly => Self::NoTick,
            Self::NoTick => Self::All,
        }
    }
}

/// Demo directory configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoDirectory {
    pub path: PathBuf,
    pub enabled: bool,
    pub recursive: bool,
    #[serde(skip)]
    pub valid_file_count: usize,
    #[serde(skip)]
    pub processed_demo_count: usize,
    #[serde(skip)]
    pub tick_data_enabled_count: Option<usize>,
    #[serde(skip)]
    pub total_collections_count: Option<usize>,
}

impl DemoDirectory {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            enabled: true,
            recursive: false,
            valid_file_count: 0,
            processed_demo_count: 0,
            tick_data_enabled_count: None,
            total_collections_count: None,
        }
    }

    /// Get the folder name from the path (last component)
    pub fn folder_name(&self) -> String {
        // Try to get normal file name first
        if let Some(name) = self.path.file_name().and_then(|s| s.to_str()) {
            return name.to_string();
        }

        // If that fails (e.g. for drive roots like "D:\"), try to inspect components
        // On Windows, "D:" prefix component is available
        if let Some(std::path::Component::Prefix(prefix_component)) = self.path.components().next()
        {
            return prefix_component
                .as_os_str()
                .to_string_lossy()
                .to_string()
                .replace(":", "");
        }

        // Fallback
        self.path.to_string_lossy().to_string()
    }

    /// Get the drive letter from the path (Windows-specific)
    pub fn drive_letter(&self) -> String {
        let path_str = self.path.to_string_lossy();
        if path_str.len() >= 2 && path_str.chars().nth(1) == Some(':') {
            path_str.chars().take(2).collect()
        } else {
            String::new()
        }
    }
}
