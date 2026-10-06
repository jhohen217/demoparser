//! Tick processor module for the interface crate
//!
//! This module contains functionality for processing ticks and creating collections.

use crate::core::demo_processor::{DemoInfo, PlayerInfo, RoundInfo};
use crate::core::game_event::GameEvent;
use crate::models::collection::KillCollection;
use crate::models::kill::Kill;
use crate::utils::calculations::{calculate_radius, calculate_total_distance};
use std::collections::HashMap;

/// Increment when discovery adds collections that older catalogs may omit.
pub const COLLECTION_DISCOVERY_VERSION: i64 = 2;

/// Tick processor for processing ticks and creating collections
pub struct TickProcessor {
    /// Demo info
    demo_info: DemoInfo,
    /// Player info
    player_info: Vec<PlayerInfo>,
    /// Round info
    rounds: Vec<RoundInfo>,
    /// Game events
    game_events: Vec<GameEvent>,
    /// Processed kills
    processed_kills: Vec<Kill>,
    /// Collections
    collections: Vec<KillCollection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo_info() -> DemoInfo {
        DemoInfo {
            demo_name: "halftime.dem".into(),
            demo_path: "C:/demos/halftime.dem".into(),
            map_name: "de_anubis".into(),
            game_version: "14082".into(),
            tick_rate: 64.0,
            playback_ticks: 300,
            playback_time: 5.0,
            demo_protocol: 1,
            network_protocol: 1,
            server_name: String::new(),
            client_name: String::new(),
            game_directory: "csgo".into(),
        }
    }

    fn kill() -> Kill {
        Kill {
            tick: 100,
            killer_name: "killer".into(),
            killer_steamid: "1".into(),
            killer_team: "T".into(),
            killer_pos_x: 0.0,
            killer_pos_y: 0.0,
            killer_pos_z: 0.0,
            killer_view_pitch: 0.0,
            killer_view_yaw: 0.0,
            victim_name: "victim".into(),
            victim_steamid: "2".into(),
            victim_team: "CT".into(),
            victim_pos_x: 1.0,
            victim_pos_y: 0.0,
            victim_pos_z: 0.0,
            weapon: "ak47".into(),
            weapon_id: "7".into(),
            distance_to_enemy: 1.0,
            headshot: false,
            penetrated: false,
            attacker_blind: false,
            thru_smoke: false,
            no_scope: false,
            attacker_airborne: None,
            victim_airborne: None,
            penetration_count: None,
            modifier_known_flags: 0,
            round: 1,
            round_start_tick: 0,
            round_end_tick: 200,
            round_freeze_end: 10,
            ticks_since_last_kill: 0,
            distance_moved_since_last_kill: 0.0,
            killer_index: 4,
            victim_index: 5,
            killer_is_controlling_bot: false,
        }
    }

    #[test]
    fn collection_keeps_the_kills_event_time_side_not_the_terminal_roster() {
        // This is the hand-off contract from death_parser: an early-half kill
        // says T/CT even though the final roster is CT/T after halftime.
        let kill = kill();
        let terminal_roster = vec![
            PlayerInfo {
                index: 4,
                name: "killer".into(),
                steamid: "1".into(),
                team: "CT".into(),
            },
            PlayerInfo {
                index: 5,
                name: "victim".into(),
                steamid: "2".into(),
                team: "T".into(),
            },
        ];
        let rounds = vec![RoundInfo {
            next_start_tick: None,
            round: 1,
            start_tick: 0,
            end_tick: 200,
            freeze_end: 10,
            winner: "T".into(),
            win_reason: String::new(),
        }];
        let mut second_kill = kill.clone();
        second_kill.tick = 100;
        second_kill.victim_name = "victim_two".into();
        second_kill.victim_steamid = "3".into();
        let mut processor = TickProcessor::new(
            demo_info(),
            terminal_roster,
            rounds,
            vec![GameEvent::Kill(kill), GameEvent::Kill(second_kill)],
        );

        processor.process_events();
        assert_eq!(processor.get_processed_kills().len(), 2);
        let kill_refs: Vec<&Kill> = processor.get_processed_kills().iter().collect();
        assert!(processor.is_double_kill_sequence(&kill_refs));
        processor.create_collections();

        let collection = &processor.get_collections()[0];
        assert_eq!(collection.killer_team, "T");
        assert_eq!(collection.victim_team, "CT");
    }

    #[test]
    fn discovers_singles_and_separate_tick_doubles_without_renumbering_existing_collections() {
        let mut events = Vec::new();
        for (round, ticks) in [(1, vec![100]), (2, vec![110, 150]), (3, vec![120, 140, 160])] {
            for (index, tick) in ticks.into_iter().enumerate() {
                let mut row = kill();
                row.round = round;
                row.tick = tick;
                row.victim_name = format!("victim_{index}");
                row.victim_steamid = format!("{}", index + 2);
                events.push(GameEvent::Kill(row));
            }
        }
        let rounds = (1..=3).map(|round| RoundInfo {
            next_start_tick: None, round, start_tick: 0, end_tick: 200,
            freeze_end: 10, winner: "T".into(), win_reason: String::new(),
        }).collect();
        let mut processor = TickProcessor::new(demo_info(), vec![], rounds, events);
        processor.process_events();
        processor.create_collections();
        let rows = processor.get_collections();
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].collection_type.as_str(), rows[0].collection_num), ("TRIPLE", 1));
        assert_eq!((rows[1].collection_type.as_str(), rows[1].collection_num), ("SINGLE", 2));
        assert_eq!((rows[2].collection_type.as_str(), rows[2].collection_num), ("DOUBLE", 3));
        assert_eq!(rows[2].kill_ticks, vec![110, 150]);
    }
}

impl TickProcessor {
    /// Create a new tick processor
    pub fn new(
        demo_info: DemoInfo,
        player_info: Vec<PlayerInfo>,
        rounds: Vec<RoundInfo>,
        game_events: Vec<GameEvent>,
    ) -> Self {
        TickProcessor {
            demo_info,
            player_info,
            rounds,
            game_events,
            processed_kills: Vec::new(),
            collections: Vec::new(),
        }
    }

    /// Process game events
    pub fn process_events(&mut self) {
        let mut steamid_to_team: HashMap<u64, String> = HashMap::new();
        for player in &self.player_info {
            steamid_to_team.insert(player.steamid.parse().unwrap_or(0), player.team.clone());
        }

        for event in &self.game_events {
            match event {
                GameEvent::Kill(kill) => {
                    let mut kill = kill.clone();
                    // Per-event teams are authoritative and account for side switches.
                    // The final roster is only a fallback for older/incomplete events.
                    if kill.killer_team == "UNKNOWN" {
                        if let Some(team) =
                            steamid_to_team.get(&kill.killer_steamid.parse().unwrap_or(0))
                        {
                            kill.killer_team = team.clone();
                        }
                    }
                    if kill.victim_team == "UNKNOWN" {
                        if let Some(team) =
                            steamid_to_team.get(&kill.victim_steamid.parse().unwrap_or(0))
                        {
                            kill.victim_team = team.clone();
                        }
                    }
                    self.processed_kills.push(kill);
                }
                GameEvent::TeamChange(tc) => {
                    let team_name = if tc.team_number == 2 { "T" } else { "CT" }.to_string();
                    steamid_to_team.insert(tc.user_steamid, team_name);
                }
            }
        }
    }

    /// Create collections
    pub fn create_collections(&mut self) {
        // Group kills by killer and round
        let mut killer_round_kills: std::collections::HashMap<(String, i32), Vec<&Kill>> =
            std::collections::HashMap::new();
        let mut new_collections = Vec::new();
        let mut _team_kills_skipped = 0;

        // Set killer_index and victim_index based on player_info
        let mut processed_kills = self.processed_kills.clone();
        for kill in &mut processed_kills {
            // Set killer_index
            for player in &self.player_info {
                if player.steamid == kill.killer_steamid {
                    kill.killer_index = player.index;
                    break;
                }
            }

            // Set victim_index
            for player in &self.player_info {
                if player.steamid == kill.victim_steamid {
                    kill.victim_index = player.index;
                    break;
                }
            }

            // If killer_index is still 0, try to find a match by name
            if kill.killer_index == 0 {
                for player in &self.player_info {
                    if player.name == kill.killer_name {
                        kill.killer_index = player.index;
                        break;
                    }
                }
            }

            // If victim_index is still 0, try to find a match by name
            if kill.victim_index == 0 {
                for player in &self.player_info {
                    if player.name == kill.victim_name {
                        kill.victim_index = player.index;
                        break;
                    }
                }
            }

            // If killer_index is still 0, set it to a default value (4)
            if kill.killer_index == 0 {
                kill.killer_index = 4;
            }

            // If victim_index is still 0, set it to a default value (5)
            if kill.victim_index == 0 {
                kill.victim_index = 5;
            }
        }

        // Group kills by killer and round, using the original team assignments from the demo file
        for kill in &processed_kills {
            // TEMPORARILY DISABLED: Skip team kills - this is critical for matching Python implementation
            if kill.killer_team == kill.victim_team {
                _team_kills_skipped += 1;
                continue;
            }

            // Find the SteamID from player_info based on killer_index
            let killer_steamid = if let Some(player) = self
                .player_info
                .iter()
                .find(|p| p.index == kill.killer_index)
            {
                player.steamid.clone()
            } else if !kill.killer_steamid.is_empty() {
                kill.killer_steamid.clone()
            } else {
                // If we can't find a SteamID, try to find by name
                if let Some(player) = self.player_info.iter().find(|p| p.name == kill.killer_name) {
                    player.steamid.clone()
                } else {
                    // Last resort: use killer name
                    kill.killer_name.clone()
                }
            };

            let key = (killer_steamid.clone(), kill.round);
            killer_round_kills
                .entry(key.clone())
                .or_insert_with(Vec::new)
                .push(kill);
        }

        // Create collections for each killer and round
        let mut collection_num = 1;

        // Sort killer_round_kills by round first, then by killer_steamid within each round
        // This matches Python's pandas groupby(["correct_round", "attacker_steamid"]) behavior

        // Sort the keys by round first, then by killer_steamid if rounds are equal
        let mut sorted_keys: Vec<(String, i32)> = killer_round_kills.keys().cloned().collect();
        sorted_keys.sort_by(|a, b| {
            // First sort by round
            let round_cmp = a.1.cmp(&b.1);
            if round_cmp != std::cmp::Ordering::Equal {
                return round_cmp;
            }

            // Then sort by killer_steamid (or killer_name as fallback) if rounds are equal
            // This matches Python's pandas groupby behavior
            a.0.cmp(&b.0)
        });

        // Keep the IDs of previously discoverable collections stable when adding the
        // formerly omitted singles and separate-tick doubles to an existing catalog.
        sorted_keys.sort_by_key(|key| {
            let kills = &killer_round_kills[key];
            !(kills.len() >= 3 || (kills.len() == 2 && kills[0].tick == kills[1].tick))
        });

        // Process kills in sorted order
        for key in sorted_keys {
            let kills = killer_round_kills.get(&key).unwrap().clone();
            let (_killer_steamid, round) = key;
            // Skip if there are no kills
            if kills.is_empty() {
                continue;
            }

            // Get the round info - skip collection if round data is missing
            let round_info = match self.rounds.iter().find(|r| r.round == round).cloned() {
                Some(info) => info,
                None => {
                    // Skip this collection since we don't have valid round data
                    continue;
                }
            };

            // Sort kills by tick
            let mut sorted_kills = kills.clone();
            sorted_kills.sort_by_key(|kill| kill.tick);

            if let Some(collection) =
                self.create_collection(&sorted_kills, &round_info, collection_num)
            {
                new_collections.push(collection);
                collection_num += 1;
            }
        }

        // Add all new collections
        let _collections_created = new_collections.len();
        self.collections.extend(new_collections);
    }

    /// Create a collection for a set of kills
    fn create_collection(
        &self,
        kills: &[&Kill],
        round_info: &RoundInfo,
        collection_num: i32,
    ) -> Option<KillCollection> {
        // Skip if there are no kills
        if kills.is_empty() {
            return None;
        }

        // Check if any kill in the sequence was made by the killer controlling a bot
        if kills.iter().any(|k| k.killer_is_controlling_bot) {
            return None;
        }

        // We've already filtered out team kills when grouping by killer and round
        // So we don't need to filter again here

        // Get the first and last kill
        let first_kill = kills[0];
        let last_kill = kills[kills.len() - 1];

        // Get the killer positions
        let killer_positions: Vec<[f32; 3]> = kills
            .iter()
            .map(|kill| [kill.killer_pos_x, kill.killer_pos_y, kill.killer_pos_z])
            .collect();

        // Get the victim positions
        let victim_positions: Vec<[f32; 3]> = kills
            .iter()
            .map(|kill| [kill.victim_pos_x, kill.victim_pos_y, kill.victim_pos_z])
            .collect();

        // Calculate the killer radius
        let killer_radius = calculate_radius(&killer_positions);

        // Calculate the victims radius
        let victims_radius = calculate_radius(&victim_positions);

        // Calculate the killer move distance
        let killer_move_distance = calculate_total_distance(&killer_positions);

        // Get the weapons used
        let weapons: Vec<String> = kills.iter().map(|kill| kill.weapon.clone()).collect();

        // Get the weapon IDs
        let weapons_id: Vec<String> = kills.iter().map(|kill| kill.weapon_id.clone()).collect();

        // Get the kill ticks
        let kill_ticks: Vec<i32> = kills.iter().map(|kill| kill.tick).collect();

        // Get the victim indices
        let victim_indices: Vec<i32> = kills.iter().map(|kill| kill.victim_index).collect();

        // Determine the collection type based on the number of kills
        // Priority matches Python: MULTI (for 3+ kills with 3+ on same tick) > ACE > QUAD > TRIPLE > DOUBLE > SINGLE
        let collection_type_str = if kills.len() >= 3 && self.is_multi_kill_sequence(kills) {
            "MULTI"
        } else if kills.len() == 5 && self.is_ace_kill_sequence(kills) {
            "ACE"
        } else if kills.len() == 4 && self.is_quad_kill_sequence(kills) {
            "QUAD"
        } else if kills.len() == 3 && self.is_triple_kill_sequence(kills) {
            "TRIPLE"
        } else if kills.len() == 2 && self.is_double_kill_sequence(kills) {
            "DOUBLE"
        } else if kills.len() == 1 && self.is_single_kill_sequence(kills) {
            "SINGLE"
        } else {
            "NORMAL"
        };

        // Find the player info for the killer to get the SteamID
        let killer_steamid = if let Some(player) = self
            .player_info
            .iter()
            .find(|p| p.index == first_kill.killer_index)
        {
            player.steamid.clone()
        } else {
            first_kill.killer_steamid.clone()
        };

        // Create the collection with the kills, ensuring SteamIDs are properly set
        let mut collection_kills: Vec<Kill> = kills.iter().map(|k| (*k).clone()).collect();

        // Make sure each kill has the correct SteamIDs from player_info
        for kill in &mut collection_kills {
            // Set killer SteamID from player_info
            if let Some(player) = self.player_info.iter().find(|p| p.name == kill.killer_name) {
                kill.killer_steamid = player.steamid.clone();
            }

            // Set victim SteamID from player_info
            if let Some(player) = self.player_info.iter().find(|p| p.name == kill.victim_name) {
                kill.victim_steamid = player.steamid.clone();
            }
        }

        let mut collection = KillCollection::new(
            collection_type_str.to_string(),
            collection_num,
            0, // col_total - will be set later when all collections are known
            last_kill.tick - first_kill.tick,
            self.demo_info.map_name.clone(),
            first_kill.killer_index,
            first_kill.killer_team.clone(),
            first_kill.tick,
            last_kill.tick,
            first_kill.killer_name.clone(),
            killer_steamid, // Use the SteamID from player_info
            self.demo_info.demo_name.clone(),
            self.demo_info.demo_path.clone(),
            crate::utils::parser_utils::get_folder_from_demo_path(&self.demo_info.demo_path),
            killer_radius,
            victims_radius,
            killer_move_distance,
            first_kill.victim_team.clone(),
            round_info.start_tick,
            round_info.end_tick,
            round_info.freeze_end,
            round_info.round,
            weapons,
            weapons_id,
            kill_ticks,
            victim_indices,
            collection_kills,
        );

        // Set game_version from demo_info
        collection.game_version = self.demo_info.game_version.parse().unwrap_or(0);

        // Only return collections with recognized types (skip NORMAL)
        // This matches the Python implementation which filters out NORMAL collections
        if collection_type_str != "NORMAL" {
            Some(collection)
        } else {
            // No verbose logging for failed collections
            None
        }
    }

    /// Get the processed kills
    pub fn get_processed_kills(&self) -> &[Kill] {
        &self.processed_kills
    }

    /// Get the collections
    pub fn get_collections(&self) -> &[KillCollection] {
        &self.collections
    }

    /// Check if a sequence of kills is a multi-kill (3+ kills on the same tick)
    fn is_multi_kill_sequence(&self, kills: &[&Kill]) -> bool {
        // Need at least 3 kills
        if kills.len() < 3 {
            return false;
        }

        // Count occurrences of each tick
        let mut tick_counts: std::collections::HashMap<i32, i32> = std::collections::HashMap::new();
        for kill in kills {
            *tick_counts.entry(kill.tick).or_insert(0) += 1;
        }

        // MULTI should only apply when 3+ kills happen on the exact same tick
        // This matches the Python implementation
        tick_counts.values().any(|&count| count >= 3)
    }

    /// Check if a sequence of kills is a double-kill (exactly 2 kills in a round)
    fn is_double_kill_sequence(&self, kills: &[&Kill]) -> bool {
        // Need exactly 2 kills
        if kills.len() != 2 {
            return false;
        }

        // Check if both kills are by the same killer name
        if kills[0].killer_name != kills[1].killer_name {
            return false;
        }

        // Check if both victims are on the same team
        if kills[0].victim_team != kills[1].victim_team {
            return false;
        }

        // Check if all kills are in the same round
        let round = kills[0].round;
        if kills.iter().any(|k| k.round != round) {
            return false;
        }

        // Check if victims are different
        if kills[0].victim_name == kills[1].victim_name {
            return false;
        }

        true
    }

    /// Check if a sequence of kills is a single-kill
    fn is_single_kill_sequence(&self, kills: &[&Kill]) -> bool {
        kills.len() == 1
    }

    /// Check if a sequence of kills is a triple-kill (exactly 3 kills in a round)
    fn is_triple_kill_sequence(&self, kills: &[&Kill]) -> bool {
        // Need exactly 3 kills
        if kills.len() != 3 {
            return false;
        }

        // Check if all kills are by the same killer name (since SteamIDs might be empty)
        let killer_name = &kills[0].killer_name;
        if kills.iter().any(|k| k.killer_name != *killer_name) {
            return false;
        }

        // Check if all victims are on the same team
        let victim_team = &kills[0].victim_team;
        if kills.iter().any(|k| k.victim_team != *victim_team) {
            return false;
        }

        // Check if all kills are in the same round
        let round = kills[0].round;
        if kills.iter().any(|k| k.round != round) {
            return false;
        }

        // If we've passed all checks, this is a valid triple kill
        true
    }

    /// Check if a sequence of kills is a quad-kill (exactly 4 kills in a round)
    fn is_quad_kill_sequence(&self, kills: &[&Kill]) -> bool {
        // Need exactly 4 kills
        if kills.len() != 4 {
            return false;
        }

        // Check if all kills are by the same killer name (since SteamIDs might be empty)
        let killer_name = &kills[0].killer_name;
        if kills.iter().any(|k| k.killer_name != *killer_name) {
            return false;
        }

        // Check if all victims are on the same team
        let victim_team = &kills[0].victim_team;
        if kills.iter().any(|k| k.victim_team != *victim_team) {
            return false;
        }

        // Check if all kills are in the same round
        let round = kills[0].round;
        if kills.iter().any(|k| k.round != round) {
            return false;
        }

        // Check if all victims are different
        let mut victim_names = std::collections::HashSet::new();
        for kill in kills {
            if !victim_names.insert(&kill.victim_name) {
                return false;
            }
        }

        true
    }

    /// Check if a sequence of kills is an ace (5 kills in a round, all enemies)
    fn is_ace_kill_sequence(&self, kills: &[&Kill]) -> bool {
        // Check if there are exactly 5 kills
        if kills.len() != 5 {
            return false;
        }

        // Check if all kills are by the same killer name (since SteamIDs might be empty)
        let killer_name = &kills[0].killer_name;
        if kills.iter().any(|k| k.killer_name != *killer_name) {
            return false;
        }

        // Check if all victims are on the same team
        let victim_team = &kills[0].victim_team;
        if kills.iter().any(|k| k.victim_team != *victim_team) {
            return false;
        }

        // Check if all kills are in the same round
        let round = kills[0].round;
        if kills.iter().any(|k| k.round != round) {
            return false;
        }

        // Check if all victims are different
        let mut victim_names = std::collections::HashSet::new();
        for kill in kills {
            if !victim_names.insert(&kill.victim_name) {
                return false;
            }
        }

        true
    }
}
