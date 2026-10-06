//! Module for converting interface KillCollections to tick-by-tick KillCollectionData
//!
//! This eliminates the need for intermediate CSV files by performing the conversion in-memory.

use anyhow::Result;
use std::collections::HashMap;

use crate::tick_by_tick::kill_collection_parser::{
    Collection as TbtCollection, CollectionDetail, DemoInfo as TbtDemoInfo, KillCollectionData,
    PlayerInfo as TbtPlayerInfo, RoundInfo as TbtRoundInfo,
};
use interface::core::demo_processor::DemoProcessor;
use interface::models::collection::KillCollection;

/// Convert interface KillCollections and DemoProcessor data to tick-by-tick format
pub fn convert_to_tick_by_tick_format(
    collections: &[KillCollection],
    processor: &DemoProcessor,
    original_file_path: Option<&std::path::PathBuf>,
) -> Result<KillCollectionData> {
    // 1. Convert DemoInfo
    let demo_info = processor.get_demo_info();

    let (final_demo_path, final_folder) = if let Some(path) = original_file_path {
        (
            path.to_string_lossy().to_string(),
            interface::utils::parser_utils::get_folder_from_demo_path(&path.to_string_lossy()),
        )
    } else {
        (
            demo_info.demo_path.clone(),
            interface::utils::parser_utils::get_folder_from_demo_path(&demo_info.demo_path),
        )
    };

    let tbt_demo_info = TbtDemoInfo {
        demo_path: final_demo_path,
        demo_name: demo_info.demo_name.clone(),
        map_name: demo_info.map_name.clone(),
        game_version: demo_info.game_version.clone(),
        folder: final_folder,
        total_ticks: demo_info.playback_ticks as u32,
        game_start_offset: processor.get_game_start_offset() as f64,
        // Calculate counts from collections
        ace_count: count_collections_by_type(collections, "ACE"),
        quad_count: count_collections_by_type(collections, "QUAD"),
        multi_count: count_collections_by_type(collections, "MULTI"),
        triple_count: count_collections_by_type(collections, "TRIPLE"),
        double_count: count_collections_by_type(collections, "DOUBLE"),
        single_count: count_collections_by_type(collections, "SINGLE"),
    };

    // 2. Convert RoundInfo
    let tbt_rounds: Vec<TbtRoundInfo> = processor
        .get_rounds()
        .iter()
        .map(|r| TbtRoundInfo {
            next_start_tick: r.next_start_tick.and_then(|tick| u32::try_from(tick).ok()),
            round: r.round as u32,
            start_tick: r.start_tick as u32,
            end_tick: r.end_tick as u32,
            round_freeze_end: r.freeze_end as u32,
        })
        .collect();

    // 3. Convert PlayerInfo
    // interface::PlayerInfo -> tick_by_tick::PlayerInfo
    // interface::PlayerInfo uses {name, steamid, index}
    // tick_by_tick::PlayerInfo uses {player_name, steam_id, killer_index, team}
    // Note: interface PlayerInfo doesn't supply team directly in the struct typically used here,
    // but we can try to infer it or leave it as "Unknown".
    // Looking at CollectionWriter::write_players, it writes just name, steamid, index.
    // The CSV parser for [PLAYERS] reads team if available, defaults to "Unknown".
    // Since we are bypassing CSV, we can stick with "Unknown" or meaningful default.
    // The tick-by-tick processor might update team later from parsed data.
    let tbt_players: Vec<TbtPlayerInfo> = processor
        .get_player_info()
        .iter()
        .map(|p| {
            TbtPlayerInfo {
                player_name: p.name.clone(),
                steam_id: p.steamid.parse().unwrap_or(0),
                killer_index: p.index as u32,
                team: "Unknown".to_string(), // Will be populated during detailed parsing if imperative
            }
        })
        .collect();

    // 4. Convert Collections
    let mut tbt_collections = Vec::new();
    let mut collection_details = HashMap::new();

    for col in collections {
        let tbt_col = TbtCollection {
            collection_type: col.collection_type.clone(),
            collection_num: col.collection_num as u32,
            tick_duration: col.tick_duration as u32,
            map_name: col.map_name.clone(),
            killer_index: col.killer_index as u32,
            killer_team: col.killer_team.clone(),
            start_kill_tick: col.start_kill_tick as u32,
            end_kill_tick: col.end_kill_tick as u32,
            killer_name: col.killer_name.clone(),
            steam_id: col.killer_steamid.parse().unwrap_or(0),
            demo_name: col.demo_name.clone(),
            folder: col.folder.clone(),
            killer_radius: col.killer_radius as f64,
            victims_radius: col.victims_radius as f64,
            killer_move_distance: col.killer_move_distance as f64,
            victim_team: col.victim_team.clone(),
            round_start_tick: col.round_start_tick as u32,
            round_end_tick: col.round_end_tick as u32,
            round_freeze_end: col.round_freeze_end as u32,
            round: col.round as u32,
            weapons: format!("[{}]", col.weapons.join(";")),
            weapons_id: format!("[{}]", col.weapons_id.join(";")),
            kill_ticks: format!(
                "[{}]",
                col.kill_ticks
                    .iter()
                    .map(|t| t.to_string())
                    .collect::<Vec<_>>()
                    .join(";")
            ),
            victims_index: format!(
                "[{}]",
                col.victim_indices
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(";")
            ),
            tick_parsed: 0, // Default
        };
        tbt_collections.push(tbt_col);

        // 5. Convert Collection Details (Kills)
        let mut details = Vec::new();

        // Sort kills by tick (consistency with CSV writer)
        let mut sorted_kills = col.kills.clone();
        sorted_kills.sort_by_key(|k| k.tick);

        let mut prev_kill: Option<interface::models::kill::Kill> = None;

        for kill in sorted_kills {
            // Calculate derived stats
            let (ticks_since, distance_moved) = if let Some(prev) = &prev_kill {
                let ticks_since = kill.tick - prev.tick;
                let distance_moved = interface::utils::calculations::calculate_distance(
                    &[prev.killer_pos_x, prev.killer_pos_y, prev.killer_pos_z],
                    &[kill.killer_pos_x, kill.killer_pos_y, kill.killer_pos_z],
                );
                (ticks_since as u32, distance_moved as f64)
            } else {
                (0, 0.0)
            };

            let detail = CollectionDetail {
                collection_type: col.collection_type.clone(),
                kill_tick: kill.tick as u32,
                killer_name: kill.killer_name.clone(),
                killer_steamid: kill.killer_steamid.parse().unwrap_or(0),
                player_team: kill.killer_team.clone(),
                player_weapon: kill.weapon.clone(),
                player_weapon_id: kill.weapon_id.parse().unwrap_or(0),
                player_pos_x: kill.killer_pos_x as f64,
                player_pos_y: kill.killer_pos_y as f64,
                player_pos_z: kill.killer_pos_z as f64,
                player_view_pitch: kill.killer_view_pitch as f64,
                player_view_yaw: kill.killer_view_yaw as f64,
                victim_name: kill.victim_name.clone(),
                victim_steamid: kill.victim_steamid.parse().unwrap_or(0),
                victim_team: kill.victim_team.clone(),
                victim_pos_x: kill.victim_pos_x as f64,
                victim_pos_y: kill.victim_pos_y as f64,
                victim_pos_z: kill.victim_pos_z as f64,
                distance_to_enemy: kill.distance_to_enemy as f64,
                ticks_since_last_kill: ticks_since,
                distance_moved_since_last_kill: distance_moved,
                killer_index: kill.killer_index as u32,
                victim_index: kill.victim_index as u32,
            };

            details.push(detail);
            prev_kill = Some(kill);
        }

        if !details.is_empty() {
            collection_details.insert(col.collection_num as u32, details);
        }
    }

    Ok(KillCollectionData {
        demo_info: tbt_demo_info,
        rounds: tbt_rounds,
        players: tbt_players,
        collections: tbt_collections,
        collection_details,
    })
}

fn count_collections_by_type(collections: &[KillCollection], type_name: &str) -> u32 {
    collections
        .iter()
        .filter(|c| c.collection_type == type_name)
        .count() as u32
}
