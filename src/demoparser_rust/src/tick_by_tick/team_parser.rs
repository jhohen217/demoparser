//! Team parsing utilities for CS2 demo parsing
//!
//! This module provides functions to extract team information from CS2 demos,
//! handling the robust extraction of team data using multiple property fallbacks.

use ahash::AHashMap;
use log::debug;
use parser::second_pass::variants::{PropColumn, VarVec};
use std::collections::HashMap;

/// Extract a u64 value from VarVec at the specified index
fn get_u64_from_varvec(data: &Option<VarVec>, index: usize) -> Option<u64> {
    if let Some(VarVec::U64(vec)) = data {
        if let Some(Some(value)) = vec.get(index) {
            return Some(*value);
        }
    }
    None
}

/// Extract an i32 value from VarVec at the specified index
fn get_i32_from_varvec(data: &Option<VarVec>, index: usize) -> Option<i32> {
    if let Some(VarVec::I32(vec)) = data {
        if let Some(Some(value)) = vec.get(index) {
            return Some(*value);
        }
    }
    None
}

/// Extract a u32 value from VarVec at the specified index
fn get_u32_from_varvec(data: &Option<VarVec>, index: usize) -> Option<u32> {
    if let Some(VarVec::U32(vec)) = data {
        if let Some(Some(value)) = vec.get(index) {
            return Some(*value);
        }
    }
    None
}

/*
/// Extract a String value from VarVec at the specified index
fn get_string_from_varvec(data: &Option<VarVec>, index: usize) -> Option<String> {
    if let Some(VarVec::String(vec)) = data {
        if let Some(Some(value)) = vec.get(index) {
            return Some(value.clone());
        }
    }
    None
}
*/

/// Get data length from a VarVec
fn get_data_length(data: &Option<VarVec>) -> usize {
    match data {
        Some(VarVec::U64(vec)) => vec.len(),
        Some(VarVec::I32(vec)) => vec.len(),
        Some(VarVec::U32(vec)) => vec.len(),
        Some(VarVec::String(vec)) => vec.len(),
        Some(VarVec::F32(vec)) => vec.len(),
        Some(VarVec::Bool(vec)) => vec.len(),
        Some(VarVec::StringVec(vec)) => vec.len(),
        Some(VarVec::Binary(vec)) => vec.len(),
        Some(VarVec::U64Vec(vec)) => vec.len(),
        Some(VarVec::U32Vec(vec)) => vec.len(),
        Some(VarVec::XYVec(vec)) => vec.len(),
        Some(VarVec::XYZVec(vec)) => vec.len(),
        Some(VarVec::Stickers(vec)) => vec.len(),
        Some(VarVec::InputHistory(vec)) => vec.len(),
        Some(VarVec::UserCmdSubtickMoves(vec)) => vec.len(),
        None => 0,
    }
}

/// Convert team number to team name
pub fn team_number_to_name(team_num: i32) -> String {
    match team_num {
        2 => "T".to_string(),
        3 => "CT".to_string(),
        _ => format!("Team{}", team_num),
    }
}

/// Read the live side recorded for one dataframe row.
///
/// A match-level lookup is incorrect after halftime: the same SteamID is expected
/// to have two valid values during one demo. Callers should retain the last live
/// value for a player only when this particular row is sparse.
pub fn team_at_row(
    df: &AHashMap<u32, PropColumn>,
    name_to_id: &HashMap<String, u32>,
    index: usize,
) -> Option<String> {
    const TEAM_PROPERTIES: &[&str] = &[
        "CCSPlayerPawn.m_iTeamNum",
        "CCSPlayerController.m_iTeamNum",
        "team_num",
        "m_iTeamNum",
        "team_number",
        "team",
    ];

    TEAM_PROPERTIES.iter().find_map(|property| {
        let column = df.get(name_to_id.get(*property)?)?;
        let team_number = get_i32_from_varvec(&column.data, index)
            .or_else(|| get_u32_from_varvec(&column.data, index).map(|value| value as i32))?;
        matches!(team_number, 2 | 3).then(|| team_number_to_name(team_number))
    })
}

/// Extract team information for a specific steamid
///
/// This function looks through the dataframe to find team information for a given player.
/// It tries multiple property sources and handles various data types.
///
/// # Arguments
/// * `df` - The dataframe containing property columns
/// * `prop_controller` - Property controller for name mapping
/// * `target_steamid` - The steamid to find team info for
/// * `_index` - The data index to look at
///
/// # Returns
/// * `Some(String)` if team information is found
/// * `None` if no team information can be determined
/*
pub fn extract_team_for_steamid(
    df: &AHashMap<u32, PropColumn>,
    prop_controller: &parser::first_pass::prop_controller::PropController,
    target_steamid: u64,
    _index: usize,
) -> Option<String> {
    // First, find the steamid property to match against
    let mut steamid_prop_id = None;
    let mut team_prop_id = None;

    // Look for steamid and team properties
    for (prop_id, column) in df {
        if let Some(prop_name) = prop_controller.id_to_name.get(prop_id) {
            match prop_name.as_str() {
                "CCSPlayerController.m_steamID" => {
                    steamid_prop_id = Some(*prop_id);
                    debug!("Found mapped steamid property: {}", prop_name);
                },
                "steamid" => {
                    if steamid_prop_id.is_none() {
                        steamid_prop_id = Some(*prop_id);
                        debug!("Found explicit steamid property");
                    }
                },
                "CCSPlayerController.m_iTeamNum" | "CCSPlayerPawn.m_iTeamNum" => {
                    team_prop_id = Some(*prop_id);
                    debug!("Found tick-specific team property: {}", prop_name);
                },
                "m_iTeamNum" | "team_number" | "team" => {
                    if team_prop_id.is_none() {
                        team_prop_id = Some(*prop_id);
                    }
                },
                _ => {
                    // Check for unknown properties that might contain steamid data
                    if prop_name == "Unknown" && steamid_prop_id.is_none() {
                        if let Some(VarVec::U64(vec)) = &column.data {
                            if let Some(Some(first_val)) = vec.first() {
                                // SteamIDs are typically large numbers starting with 765611...
                                if *first_val > 76500000000000000 {
                                    steamid_prop_id = Some(*prop_id);
                                    debug!("Found steamid data in property ID {} (value: {})", prop_id, first_val);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // If we have both steamid and team properties, try to match and extract
    if let (Some(steamid_id), Some(team_id)) = (steamid_prop_id, team_prop_id) {
        let steamid_column = &df[&steamid_id];
        let team_column = &df[&team_id];

        let steamid_len = get_data_length(&steamid_column.data);
        let team_len = get_data_length(&team_column.data);
        let data_len = steamid_len.min(team_len);

        // Look for our target steamid
        for i in 0..data_len {
            if let Some(steamid) = get_u64_from_varvec(&steamid_column.data, i) {
                if steamid == target_steamid {
                    // Found our player, extract team
                    let team_name = if let Some(team_num) = get_i32_from_varvec(&team_column.data, i) {
                        team_number_to_name(team_num)
                    } else if let Some(team_num) = get_u32_from_varvec(&team_column.data, i) {
                        team_number_to_name(team_num as i32)
                    } else {
                        "Unknown".to_string()
                    };

                    debug!("Found team {} for steamid {}", team_name, target_steamid);
                    return Some(team_name);
                }
            }
        }
    }

    debug!("No team information found for steamid {}", target_steamid);
    None
}
*/

/// Build a team lookup map from the dataframe
///
/// This function creates a HashMap that maps steamids to team names for efficient lookup.
/// It processes the entire dataframe once to build the lookup table.
///
/// # Arguments
/// * `df` - The dataframe containing property columns
/// * `prop_controller` - Property controller for name mapping
///
/// # Returns
/// * HashMap mapping steamid to team name
pub fn build_team_lookup_map(
    df: &AHashMap<u32, PropColumn>,
    prop_controller: &parser::first_pass::prop_controller::PropController,
) -> HashMap<u64, String> {
    let mut team_map = HashMap::new();

    // Find property IDs for steamid and team data
    let mut steamid_prop_id = None;
    let mut team_prop_id = None;

    for (prop_id, column) in df {
        if let Some(prop_name) = prop_controller.id_to_name.get(prop_id) {
            match prop_name.as_str() {
                "CCSPlayerController.m_steamID" => {
                    steamid_prop_id = Some(*prop_id);
                    debug!(
                        "Found mapped steamid property for team lookup: {}",
                        prop_name
                    );
                }
                "steamid" => {
                    if steamid_prop_id.is_none() {
                        steamid_prop_id = Some(*prop_id);
                        debug!("Found explicit steamid property for team lookup");
                    }
                }
                "CCSPlayerController.m_iTeamNum" | "CCSPlayerPawn.m_iTeamNum" => {
                    team_prop_id = Some(*prop_id);
                    debug!(
                        "Found tick-specific team property for lookup: {}",
                        prop_name
                    );
                }
                "m_iTeamNum" | "team_number" | "team" => {
                    if team_prop_id.is_none() {
                        team_prop_id = Some(*prop_id);
                    }
                }
                _ => {
                    // Check for unknown properties that might contain steamid data
                    if prop_name == "Unknown" && steamid_prop_id.is_none() {
                        if let Some(VarVec::U64(vec)) = &column.data {
                            if let Some(Some(first_val)) = vec.first() {
                                if *first_val > 76500000000000000 {
                                    steamid_prop_id = Some(*prop_id);
                                    debug!(
                                        "Found steamid data in unknown property for lookup: {}",
                                        prop_id
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Build the lookup map if we have the required properties
    if let (Some(steamid_id), Some(team_id)) = (steamid_prop_id, team_prop_id) {
        let steamid_column = &df[&steamid_id];
        let team_column = &df[&team_id];

        let steamid_len = get_data_length(&steamid_column.data);
        let team_len = get_data_length(&team_column.data);
        let data_len = steamid_len.min(team_len);

        debug!("Building team lookup map from {} data points", data_len);

        for i in 0..data_len {
            if let Some(steamid) = get_u64_from_varvec(&steamid_column.data, i) {
                // Skip if we already have this steamid (avoid duplicates)
                if team_map.contains_key(&steamid) {
                    continue;
                }

                let team_name = if let Some(team_num) = get_i32_from_varvec(&team_column.data, i) {
                    team_number_to_name(team_num)
                } else if let Some(team_num) = get_u32_from_varvec(&team_column.data, i) {
                    team_number_to_name(team_num as i32)
                } else {
                    "Unknown".to_string()
                };

                team_map.insert(steamid, team_name);
            }
        }

        debug!("Built team lookup map with {} entries", team_map.len());
    } else {
        debug!("Could not build team lookup map - missing required properties");
    }

    team_map
}

/// Get the list of team-related properties that should be requested during parsing
pub fn get_team_properties() -> Vec<String> {
    vec![
        // Team information properties from the working parser
        "CCSPlayerController.m_iTeamNum".to_string(),
        "CCSPlayerPawn.m_iTeamNum".to_string(),
        "m_iTeamNum".to_string(),
        "team_number".to_string(),
        "team".to_string(),
        // Player identification properties
        "CCSPlayerController.m_steamID".to_string(),
        "player_name".to_string(),
        "CCSPlayerController.m_iszPlayerName".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_side_from_the_current_dataframe_row() {
        let mut df = AHashMap::new();
        df.insert(
            42,
            PropColumn {
                data: Some(VarVec::I32(vec![Some(2), Some(3)])),
                num_nones: 0,
            },
        );
        let mut names = HashMap::new();
        names.insert("CCSPlayerPawn.m_iTeamNum".to_string(), 42);

        assert_eq!(team_at_row(&df, &names, 0).as_deref(), Some("T"));
        assert_eq!(team_at_row(&df, &names, 1).as_deref(), Some("CT"));
    }
}
