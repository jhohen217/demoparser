//! Collection model for the interface crate
//!
//! This module contains the KillCollection struct, which represents a collection of kills in a CS:GO demo.

use crate::models::kill::Kill;
use serde::{Deserialize, Serialize};

/// KillCollection struct representing a collection of kills in a CS:GO demo
///
/// `Default` exists so tests and buffer code can build a collection from a handful of
/// relevant fields with `..Default::default()`; every field is a plain owned value, so the
/// derived zero/empty default is meaningful rather than a placeholder.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KillCollection {
    /// Collection type (e.g., "1K", "2K", "3K", "4K", "5K", "ACE")
    pub collection_type: String,
    /// Collection number
    pub collection_num: i32,
    /// Total collections in the demo
    pub col_total: i32,
    /// Tick duration of the collection
    pub tick_duration: i32,
    /// Map name
    pub map_name: String,
    /// Killer player index
    pub killer_index: i32,
    /// Killer player team
    pub killer_team: String,
    /// Start kill tick
    pub start_kill_tick: i32,
    /// End kill tick
    pub end_kill_tick: i32,
    /// Killer player name
    pub killer_name: String,
    /// Killer player SteamID
    pub killer_steamid: String,
    /// Demo name
    pub demo_name: String,
    /// Demo path
    pub demo_path: String,
    /// Folder (e.g., "test4")
    pub folder: String,
    /// Killer radius (distance covered by the killer)
    pub killer_radius: f32,
    /// Victims radius (distance between victims)
    pub victims_radius: f32,
    /// Killer move distance
    pub killer_move_distance: f32,
    /// Victim team
    pub victim_team: String,
    /// Round start tick
    pub round_start_tick: i32,
    /// Round end tick
    pub round_end_tick: i32,
    /// Round freeze end tick
    pub round_freeze_end: i32,
    /// Round number
    pub round: i32,
    /// Weapons used
    pub weapons: Vec<String>,
    /// Weapon IDs used
    pub weapons_id: Vec<String>,
    /// Kill ticks
    pub kill_ticks: Vec<i32>,
    /// Victim indices
    pub victim_indices: Vec<i32>,
    /// Individual kills that make up this collection
    pub kills: Vec<Kill>,
    /// Game version (from demo)
    pub game_version: u32,
    /// Tag field for external use (empty by default)
    pub tag: String,
    /// Utility thrown formatted as [weapon(count);weapon(count)]
    pub util_thrown: String,
    /// Parsed flag (0 = not parsed, 1 = tick-by-tick CSV exists)
    pub parsed: u32,
    /// Grenade trajectory mode this collection's tick data was produced with
    /// (0 = none, 1 = killer only, 2 = all players). Mirrors `grenade_trajectory_mode`.
    pub grenade_traj: u32,
    /// Total hits (bullets that connected)
    pub hits: u32,
    /// Total misses (bullets that didn't connect)
    pub misses: u32,
    /// Hit rate (hits / (hits + misses))
    pub hit_rate: f32,
    /// Utility thrown ticks formatted as [tick;tick;tick]
    pub util_thrown_ticks: String,
    /// Utility land ticks formatted as [tick;tick;tick]
    pub util_land_ticks: String,
    /// Weapons used to deal damage formatted as [weapon;weapon]
    pub weapons_damaged: String,
    /// Number of hits per weapon formatted as [count;count]
    pub weapons_damaged_num_hits: String,
    /// Weapons damaged hits formatted for display (e.g., "Weapons[ak47(8) - glock(2)]")
    pub weapons_damaged_hits_formatted: String,
}

impl KillCollection {
    /// Create a new kill collection
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        collection_type: String,
        collection_num: i32,
        col_total: i32,
        tick_duration: i32,
        map_name: String,
        killer_index: i32,
        killer_team: String,
        start_kill_tick: i32,
        end_kill_tick: i32,
        killer_name: String,
        killer_steamid: String,
        demo_name: String,
        demo_path: String,
        folder: String,
        killer_radius: f32,
        victims_radius: f32,
        killer_move_distance: f32,
        victim_team: String,
        round_start_tick: i32,
        round_end_tick: i32,
        round_freeze_end: i32,
        round: i32,
        weapons: Vec<String>,
        weapons_id: Vec<String>,
        kill_ticks: Vec<i32>,
        victim_indices: Vec<i32>,
        kills: Vec<Kill>,
    ) -> Self {
        KillCollection {
            collection_type,
            collection_num,
            col_total,
            tick_duration,
            map_name,
            killer_index,
            killer_team,
            start_kill_tick,
            end_kill_tick,
            killer_name,
            killer_steamid,
            demo_name,
            demo_path,
            folder,
            killer_radius,
            victims_radius,
            killer_move_distance,
            victim_team,
            round_start_tick,
            round_end_tick,
            round_freeze_end,
            round,
            weapons,
            weapons_id,
            kill_ticks,
            victim_indices,
            kills,
            game_version: 0,
            tag: String::new(), // Initialize as empty string
            util_thrown: String::new(),
            parsed: 0, // Default to not parsed
            grenade_traj: 0,
            hits: 0,
            misses: 0,
            hit_rate: 0.0,
            util_thrown_ticks: String::new(),
            util_land_ticks: String::new(),
            weapons_damaged: String::new(),
            weapons_damaged_num_hits: String::new(),
            weapons_damaged_hits_formatted: String::new(),
        }
    }

    /// Get the collection type
    pub fn get_collection_type(&self) -> &str {
        &self.collection_type
    }

    /// Get the collection number
    pub fn get_collection_num(&self) -> i32 {
        self.collection_num
    }

    /// Get the total collections in the demo
    pub fn get_col_total(&self) -> i32 {
        self.col_total
    }

    /// Get the tick duration of the collection
    pub fn get_tick_duration(&self) -> i32 {
        self.tick_duration
    }

    /// Get the map name
    pub fn get_map_name(&self) -> &str {
        &self.map_name
    }

    /// Get the killer player index
    pub fn get_killer_index(&self) -> i32 {
        self.killer_index
    }

    /// Get the killer player team
    pub fn get_killer_team(&self) -> &str {
        &self.killer_team
    }

    /// Get the start kill tick
    pub fn get_start_kill_tick(&self) -> i32 {
        self.start_kill_tick
    }

    /// Get the end kill tick
    pub fn get_end_kill_tick(&self) -> i32 {
        self.end_kill_tick
    }

    /// Get the killer player name
    pub fn get_killer_name(&self) -> &str {
        &self.killer_name
    }

    /// Get the killer player SteamID
    pub fn get_killer_steamid(&self) -> &str {
        &self.killer_steamid
    }

    /// Get the demo name
    pub fn get_demo_name(&self) -> &str {
        &self.demo_name
    }

    /// Get the demo path
    pub fn get_demo_path(&self) -> &str {
        &self.demo_path
    }

    /// Get the folder
    pub fn get_folder(&self) -> &str {
        &self.folder
    }

    /// Get the killer radius
    pub fn get_killer_radius(&self) -> f32 {
        self.killer_radius
    }

    /// Get the victims radius
    pub fn get_victims_radius(&self) -> f32 {
        self.victims_radius
    }

    /// Get the killer move distance
    pub fn get_killer_move_distance(&self) -> f32 {
        self.killer_move_distance
    }

    /// Get the victim team
    pub fn get_victim_team(&self) -> &str {
        &self.victim_team
    }

    /// Get the round start tick
    pub fn get_round_start_tick(&self) -> i32 {
        self.round_start_tick
    }

    /// Get the round end tick
    pub fn get_round_end_tick(&self) -> i32 {
        self.round_end_tick
    }

    /// Get the round freeze end tick
    pub fn get_round_freeze_end(&self) -> i32 {
        self.round_freeze_end
    }

    /// Get the round number
    pub fn get_round(&self) -> i32 {
        self.round
    }

    /// Get the weapons used
    pub fn get_weapons(&self) -> &[String] {
        &self.weapons
    }

    /// Get the weapon IDs used
    pub fn get_weapons_id(&self) -> &[String] {
        &self.weapons_id
    }

    /// Get the kill ticks
    pub fn get_kill_ticks(&self) -> &[i32] {
        &self.kill_ticks
    }

    /// Get the victim indices
    pub fn get_victim_indices(&self) -> &[i32] {
        &self.victim_indices
    }

    /// Get the game version
    pub fn get_game_version(&self) -> u32 {
        self.game_version
    }

    /// Get the tag field
    pub fn get_tag(&self) -> &str {
        &self.tag
    }

    /// Get the utility thrown string
    pub fn get_util_thrown(&self) -> &str {
        &self.util_thrown
    }

    /// Get the number of kills in the collection
    pub fn get_kill_count(&self) -> usize {
        self.kill_ticks.len()
    }

    /// Get the collection type based on the number of kills
    pub fn get_collection_type_from_kills(num_kills: usize) -> String {
        match num_kills {
            1 => "SINGLE".to_string(),
            2 => "DOUBLE".to_string(),
            3 => "TRIPLE".to_string(),
            4 => "QUAD".to_string(),
            5 => "ACE".to_string(),
            _ => "MULTI".to_string(),
        }
    }

    /// Check if this is a single kill
    pub fn is_single_kill(&self) -> bool {
        self.kill_ticks.len() == 1
    }

    /// Check if this is a multi-kill (3+ kills on same tick)
    pub fn is_multi_kill(&self) -> bool {
        if self.kill_ticks.len() < 3 {
            return false;
        }

        // Count occurrences of each tick
        let mut tick_counts = std::collections::HashMap::new();
        for &tick in &self.kill_ticks {
            *tick_counts.entry(tick).or_insert(0) += 1;
        }

        // Check if any tick has 3 or more kills
        tick_counts.values().any(|&count| count >= 3)
    }

    /// Check if this is a double-kill (exactly 2 kills on same tick)
    pub fn is_double_kill(&self) -> bool {
        if self.kill_ticks.len() != 2 {
            return false;
        }
        self.kill_ticks[0] == self.kill_ticks[1]
    }
}
