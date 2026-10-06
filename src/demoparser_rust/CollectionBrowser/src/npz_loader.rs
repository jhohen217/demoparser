//! NPZ data loader for round replay visualization
//!
//! Loads the NumPy-based `.npz` format produced by the Python parser.
//! For the binary `.s2r` format, see `s2r_loader.rs`.

use anyhow::{anyhow, Result};
use ndarray::{Array1, Array3, Ix1, OwnedRepr};
use ndarray_npy::NpzReader;
use serde::Deserialize;
use std::path::{Path, PathBuf};

// ─── Shared data types ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct NpzData {
    pub frames: Vec<i32>,
    pub pos: Array3<f32>,    // [T, P, 3] - tick, player, xyz
    pub angles: Array3<f32>, // [T, P, 2] - tick, player, pitch/yaw
    pub player_meta: Vec<PlayerMeta>,
    pub kill_ticks: Vec<i32>,
    pub map_name: String,
    pub killer_index: usize,
    pub killer_steamid: u64,
    pub start_tick: i32,
    pub end_tick: i32,
    pub weapon_fire: Option<WeaponFireData>,
}

#[derive(Debug, Clone)]
pub struct WeaponFireData {
    pub tick: Vec<i32>,
    pub impact_tick: Vec<i32>,
    pub attacker: Vec<u8>,
    pub weapon_id: Vec<u16>,
    pub offsets: Vec<i32>,
    pub victim_idx: Vec<u8>,
    pub damage: Vec<u16>,
    pub kill: Vec<u8>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlayerMeta {
    pub steamid: String,
    pub name: String,
    pub index: usize,
    pub team: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CollectionInfo {
    pub collection_type: String,
    pub collection_num: u32,
    pub map_name: String,
    pub killer_index: u32,
    pub killer_steamid: u64,
    pub start_tick: u32,
    pub end_tick: u32,
    pub kill_ticks: String,
    pub killer_team: String,
    pub victim_team: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NpzMetadata {
    pub players: Vec<PlayerMeta>,
    pub kill_collection: CollectionInfo,
}

#[derive(Debug, Clone)]
pub struct PlayerBounds {
    pub min_x: f32,
    pub max_x: f32,
    pub min_y: f32,
    pub max_y: f32,
}

// ─── Helper functions ─────────────────────────────────────────────────────────

/// Normalise a raw team string to the canonical two-letter form expected by
/// `radar_view::get_team_color()`.  The parser may store numeric CS2 team IDs
/// (`"2"` for Terrorist, `"3"` for Counter-Terrorist) instead of `"T"`/`"CT"`.
pub fn normalize_team(raw: &str) -> &str {
    match raw {
        "2" | "Terrorist" => "T",
        "3" | "Counter-Terrorist" => "CT",
        other => other,
    }
}

impl PlayerBounds {
    /// Calculate the center point of the bounds
    pub fn center(&self) -> (f32, f32) {
        let center_x = (self.min_x + self.max_x) / 2.0;
        let center_y = (self.min_y + self.max_y) / 2.0;
        (center_x, center_y)
    }

    /// Calculate the width and height of the bounds
    pub fn dimensions(&self) -> (f32, f32) {
        let width = self.max_x - self.min_x;
        let height = self.max_y - self.min_y;
        (width, height)
    }
}

// ─── NPZ loading implementation ───────────────────────────────────────────────

impl NpzData {
    /// Load NPZ file from path
    pub fn load_from_file(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(anyhow!("NPZ file not found: {}", path.display()));
        }

        let file = std::fs::File::open(path)?;
        let mut npz = NpzReader::new(file)?;

        // Load dense arrays
        let frames: Array1<i32> = npz
            .by_name("frames")
            .map_err(|e| anyhow!("Failed to load frames: {}", e))?;

        let pos: Array3<f32> = npz
            .by_name("pos")
            .map_err(|e| anyhow!("Failed to load pos: {}", e))?;

        let angles: Array3<f32> = npz
            .by_name("angles")
            .map_err(|e| anyhow!("Failed to load angles: {}", e))?;

        // Load metadata
        let meta_bytes: Array1<u8> = npz
            .by_name("meta")
            .map_err(|e| anyhow!("Failed to load metadata: {}", e))?;

        let meta_json: Vec<u8> = meta_bytes.to_vec();
        let mut metadata: NpzMetadata = serde_json::from_slice(&meta_json)
            .map_err(|e| anyhow!("Failed to parse metadata: {}", e))?;

        // Normalise player team strings
        for p in &mut metadata.players {
            p.team = normalize_team(&p.team).to_string();
        }

        // Parse kill ticks from metadata
        let kill_ticks: Vec<i32> = metadata
            .kill_collection
            .kill_ticks
            .split(';')
            .filter_map(|s| s.parse::<i32>().ok())
            .collect();

        // Load weapon fire arrays if present
        let weapon_fire = if let Ok(wf_tick) = npz.by_name::<OwnedRepr<i32>, Ix1>("wf_tick") {
            Some(WeaponFireData {
                tick: wf_tick.to_vec(),
                impact_tick: npz
                    .by_name::<OwnedRepr<i32>, Ix1>("wf_impact_tick")
                    .map(|a| a.to_vec())
                    .unwrap_or_default(),
                attacker: npz
                    .by_name::<OwnedRepr<u8>, Ix1>("wf_attacker")
                    .map(|a| a.to_vec())
                    .unwrap_or_default(),
                weapon_id: npz
                    .by_name::<OwnedRepr<u16>, Ix1>("wf_weapon_id")
                    .map(|a| a.to_vec())
                    .unwrap_or_default(),
                offsets: npz
                    .by_name::<OwnedRepr<i32>, Ix1>("wf_offsets")
                    .map(|a| a.to_vec())
                    .unwrap_or_default(),
                victim_idx: npz
                    .by_name::<OwnedRepr<u8>, Ix1>("wf_victim_idx")
                    .map(|a| a.to_vec())
                    .unwrap_or_default(),
                damage: npz
                    .by_name::<OwnedRepr<u16>, Ix1>("wf_damage")
                    .map(|a| a.to_vec())
                    .unwrap_or_default(),
                kill: npz
                    .by_name::<OwnedRepr<u8>, Ix1>("wf_kill")
                    .map(|a| a.to_vec())
                    .unwrap_or_default(),
            })
        } else {
            None
        };

        Ok(NpzData {
            frames: frames.to_vec(),
            pos,
            angles,
            player_meta: metadata.players,
            kill_ticks,
            map_name: metadata.kill_collection.map_name,
            killer_index: metadata.kill_collection.killer_index as usize,
            killer_steamid: metadata.kill_collection.killer_steamid,
            start_tick: metadata.kill_collection.start_tick as i32,
            end_tick: metadata.kill_collection.end_tick as i32,
            weapon_fire,
        })
    }

    /// Get player position at a specific tick index
    pub fn get_player_position(
        &self,
        tick_idx: usize,
        player_idx: usize,
    ) -> Option<(f32, f32, f32)> {
        if tick_idx >= self.frames.len() || player_idx >= self.pos.shape()[1] {
            return None;
        }

        let x = self.pos[[tick_idx, player_idx, 0]];
        let y = self.pos[[tick_idx, player_idx, 1]];
        let z = self.pos[[tick_idx, player_idx, 2]];

        if x.is_nan() || y.is_nan() || z.is_nan() {
            return None;
        }

        Some((x, y, z))
    }

    /// Get player view angles at a specific tick index (pitch, yaw)
    pub fn get_player_angles(&self, tick_idx: usize, player_idx: usize) -> Option<(f32, f32)> {
        if tick_idx >= self.frames.len() || player_idx >= self.angles.shape()[1] {
            return None;
        }

        let pitch = self.angles[[tick_idx, player_idx, 0]];
        let yaw = self.angles[[tick_idx, player_idx, 1]];

        if pitch.is_nan() || yaw.is_nan() {
            return None;
        }

        Some((pitch, yaw))
    }

    /// Get tick index from absolute tick number
    pub fn get_tick_index(&self, tick: i32) -> Option<usize> {
        self.frames.iter().position(|&t| t == tick)
    }

    /// Get number of players
    pub fn num_players(&self) -> usize {
        self.pos.shape()[1]
    }

    /// Get number of ticks
    pub fn num_ticks(&self) -> usize {
        self.frames.len()
    }

    /// Get player metadata by index
    pub fn get_player_meta(&self, player_idx: usize) -> Option<&PlayerMeta> {
        self.player_meta.get(player_idx)
    }

    /// Check if a tick is a kill tick
    pub fn is_kill_tick(&self, tick: i32) -> bool {
        self.kill_ticks.contains(&tick)
    }

    /// Calculate the bounding box of all player positions across all ticks
    pub fn calculate_player_bounds(&self) -> Option<PlayerBounds> {
        let mut min_x = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        let mut found_valid = false;

        for tick_idx in 0..self.num_ticks() {
            for player_idx in 0..self.num_players() {
                if let Some((x, y, _z)) = self.get_player_position(tick_idx, player_idx) {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                    found_valid = true;
                }
            }
        }

        if found_valid {
            Some(PlayerBounds {
                min_x,
                max_x,
                min_y,
                max_y,
            })
        } else {
            None
        }
    }

    /// Calculate the median altitude (Z coordinate) across all players and ticks
    pub fn calculate_median_altitude(&self) -> Option<f32> {
        let mut altitudes = Vec::new();

        for tick_idx in 0..self.num_ticks() {
            for player_idx in 0..self.num_players() {
                if let Some((_x, _y, z)) = self.get_player_position(tick_idx, player_idx) {
                    altitudes.push(z);
                }
            }
        }

        if altitudes.is_empty() {
            return None;
        }

        altitudes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = altitudes.len() / 2;

        if altitudes.len() % 2 == 0 && mid > 0 {
            Some((altitudes[mid - 1] + altitudes[mid]) / 2.0)
        } else {
            Some(altitudes[mid])
        }
    }
}

// ─── Path builders ─────────────────────────────────────────────────────────────

/// Build NPZ file path from collection data
pub fn build_npz_path(
    parser_output: &Path,
    collection_type: &str,
    demo_name: &str,
    collection_num: u32,
    folder: &str,
) -> PathBuf {
    parser_output.join(folder).join(format!(
        "{}_{}_{}_{}.npz",
        collection_type,
        demo_name.replace(".dem", ""),
        collection_num,
        collection_type
    ))
}

/// Build NPZ file path with simpler naming
pub fn build_npz_path_simple(
    parser_output: &Path,
    collection_type: &str,
    demo_name: &str,
    collection_num: u32,
    folder: &str,
) -> PathBuf {
    let base_name = demo_name.replace(".dem", "");

    let simple_path = parser_output.join(folder).join(format!(
        "{}_{}_{}.npz",
        collection_type, base_name, collection_num
    ));

    if simple_path.exists() {
        return simple_path;
    }

    parser_output.join(folder).join(format!(
        "{}_{}_{}_{}.npz",
        collection_type, base_name, collection_num, collection_type
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_npz_path() {
        let parser_output = Path::new("C:/parsed");
        let path = build_npz_path(parser_output, "Ace", "match_001.dem", 1, "2024_12");

        assert!(path.to_string_lossy().contains("Ace_match_001_1_Ace.npz"));
    }
}
