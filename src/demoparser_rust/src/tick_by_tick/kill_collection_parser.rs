use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct KillCollectionData {
    pub demo_info: DemoInfo,
    pub rounds: Vec<RoundInfo>,
    pub players: Vec<PlayerInfo>,
    pub collections: Vec<Collection>,
    pub collection_details: HashMap<u32, Vec<CollectionDetail>>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct DemoInfo {
    pub demo_path: String,
    pub demo_name: String,
    pub map_name: String,
    pub game_version: String,
    pub folder: String,
    pub total_ticks: u32,
    pub game_start_offset: f64,
    pub ace_count: u32,
    pub quad_count: u32,
    pub multi_count: u32,
    pub triple_count: u32,
    pub double_count: u32,
    pub single_count: u32,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RoundInfo {
    pub next_start_tick: Option<u32>,
    pub round: u32,
    pub start_tick: u32,
    pub end_tick: u32,
    pub round_freeze_end: u32,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PlayerInfo {
    pub player_name: String,
    pub steam_id: u64,
    pub killer_index: u32,
    pub team: String,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Collection {
    pub collection_type: String,
    pub collection_num: u32,
    pub tick_duration: u32,
    pub map_name: String,
    pub killer_index: u32,
    pub killer_team: String,
    pub start_kill_tick: u32,
    pub end_kill_tick: u32,
    pub killer_name: String,
    pub steam_id: u64,
    pub demo_name: String,
    pub folder: String, // Changed from month to folder
    pub killer_radius: f64,
    pub victims_radius: f64,
    pub killer_move_distance: f64,
    pub victim_team: String,
    pub round_start_tick: u32,
    pub round_end_tick: u32,
    pub round_freeze_end: u32,
    pub round: u32,
    pub weapons: String,
    pub weapons_id: String,
    pub kill_ticks: String,
    pub victims_index: String,
    pub tick_parsed: u32,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CollectionDetail {
    pub collection_type: String,
    pub kill_tick: u32,
    pub killer_name: String,
    pub killer_steamid: u64,
    pub player_team: String,
    pub player_weapon: String,
    pub player_weapon_id: u32,
    pub player_pos_x: f64,
    pub player_pos_y: f64,
    pub player_pos_z: f64,
    pub player_view_pitch: f64,
    pub player_view_yaw: f64,
    pub victim_name: String,
    pub victim_steamid: u64,
    pub victim_team: String,
    pub victim_pos_x: f64,
    pub victim_pos_y: f64,
    pub victim_pos_z: f64,
    pub distance_to_enemy: f64,
    pub ticks_since_last_kill: u32,
    pub distance_moved_since_last_kill: f64,
    pub killer_index: u32,
    pub victim_index: u32,
}

#[allow(dead_code)]
pub fn parse_kill_collection_csv(file_path: &Path) -> Result<KillCollectionData> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    let mut demo_info_values = HashMap::new();
    let mut rounds = Vec::new();
    let mut players = Vec::new();
    let mut collections = Vec::new();
    let mut collection_details: HashMap<u32, Vec<CollectionDetail>> = HashMap::new();

    let mut current_section = String::new();
    let mut collection_num = 0u32;

    while let Some(line) = lines.next() {
        let line = line?;
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = trimmed.to_string();
            if current_section.starts_with("[COLLECTION ") {
                if let Some(num_str) = current_section
                    .strip_prefix("[COLLECTION ")
                    .and_then(|s| s.strip_suffix(']'))
                {
                    collection_num = num_str.parse().unwrap_or(0);
                }
            }
        } else {
            match current_section.as_str() {
                "[DEMO_INFO]" => {
                    if let Some((key, value)) = trimmed.split_once(',') {
                        demo_info_values.insert(key.to_string(), value.to_string());
                    }
                }
                "[ROUNDS]" => {
                    if !trimmed.starts_with("Round,") {
                        if let Ok(round) = parse_round_line(trimmed) {
                            rounds.push(round);
                        }
                    }
                }
                "[PLAYERS]" => {
                    if !trimmed.starts_with("PlayerName,") {
                        if let Ok(player) = parse_player_line(trimmed) {
                            players.push(player);
                        }
                    }
                }
                "[KILL_COLLECTIONS]" => {
                    if !trimmed.starts_with("Type,") {
                        if let Ok(collection) = parse_collection_line(trimmed) {
                            collections.push(collection);
                        }
                    }
                }
                s if s.starts_with("[COLLECTION ") => {
                    if !trimmed.starts_with("Type,") {
                        if let Ok(detail) = parse_collection_detail_line(trimmed) {
                            collection_details
                                .entry(collection_num)
                                .or_default()
                                .push(detail);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    let demo_info = DemoInfo {
        demo_path: demo_info_values
            .get("DemoPath")
            .cloned()
            .unwrap_or_default(),
        demo_name: demo_info_values
            .get("DemoName")
            .cloned()
            .unwrap_or_default(),
        map_name: demo_info_values.get("MapName").cloned().unwrap_or_default(),
        game_version: demo_info_values
            .get("GameVersion")
            .cloned()
            .unwrap_or_else(|| "Unknown".to_string()),
        folder: demo_info_values
            .get("Folder")
            .or_else(|| demo_info_values.get("Month"))
            .cloned()
            .unwrap_or_default(),
        total_ticks: demo_info_values
            .get("TotalTicks")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        game_start_offset: demo_info_values
            .get("GameStartOffset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0),
        ace_count: demo_info_values
            .get("AceCount")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        quad_count: demo_info_values
            .get("QuadCount")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        multi_count: demo_info_values
            .get("MultiCount")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        triple_count: demo_info_values
            .get("TripleCount")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        double_count: demo_info_values
            .get("DoubleCount")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        single_count: demo_info_values
            .get("SingleCount")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    };

    Ok(KillCollectionData {
        demo_info,
        rounds,
        players,
        collections,
        collection_details,
    })
}

#[allow(dead_code)]
fn parse_round_line(line: &str) -> Result<RoundInfo> {
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() >= 4 {
        Ok(RoundInfo {
            next_start_tick: None,
            round: parts[0].parse()?,
            start_tick: parts[1].parse()?,
            end_tick: parts[2].parse()?,
            round_freeze_end: parts[3].parse()?,
        })
    } else {
        Err(anyhow!("Invalid round line format"))
    }
}

#[allow(dead_code)]
fn parse_player_line(line: &str) -> Result<PlayerInfo> {
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() >= 3 {
        let team = if parts.len() >= 4 && !parts[3].trim().is_empty() {
            parts[3].to_string()
        } else {
            "Unknown".to_string()
        };

        Ok(PlayerInfo {
            player_name: parts[0].to_string(),
            steam_id: parts[1].parse()?,
            killer_index: parts[2].parse()?,
            team,
        })
    } else {
        Err(anyhow!("Invalid player line format"))
    }
}

#[allow(dead_code)]
fn parse_collection_line(line: &str) -> Result<Collection> {
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() >= 24 {
        // Normalize team values from numeric to string representation
        let killer_team = normalize_team_value(parts[5]);
        let victim_team = normalize_team_value(parts[15]);

        Ok(Collection {
            collection_type: parts[0].to_string(),
            collection_num: parts[1].parse()?,
            tick_duration: parts[2].parse()?,
            map_name: parts[3].to_string(),
            killer_index: parts[4].parse()?,
            killer_team,
            start_kill_tick: parts[6].parse()?,
            end_kill_tick: parts[7].parse()?,
            killer_name: parts[8].to_string(),
            steam_id: parts[9].parse()?,
            demo_name: parts[10].to_string(),
            folder: parts[11].to_string(), // Changed from month to folder
            killer_radius: parts[12].parse()?,
            victims_radius: parts[13].parse()?,
            killer_move_distance: parts[14].parse()?,
            victim_team,
            round_start_tick: parts[16].parse()?,
            round_end_tick: parts[17].parse()?,
            round_freeze_end: parts[18].parse()?,
            round: parts[19].parse()?,
            weapons: parts[20].to_string(),
            weapons_id: parts[21].to_string(),
            kill_ticks: parts[22].to_string(),
            victims_index: parts[23].to_string(),
            tick_parsed: if parts.len() > 24 {
                parts[24].parse()?
            } else {
                0
            },
        })
    } else {
        Err(anyhow!("Invalid collection line format"))
    }
}

/// Normalize team values from numeric to string representation
#[allow(dead_code)]
fn normalize_team_value(team: &str) -> String {
    match team.trim() {
        "2" => "T".to_string(),
        "3" => "CT".to_string(),
        other => other.to_string(),
    }
}

#[allow(dead_code)]
fn parse_collection_detail_line(line: &str) -> Result<CollectionDetail> {
    let parts: Vec<&str> = line.split(',').collect();
    if parts.len() >= 23 {
        let kill_tick: u32 = parts[1].parse()?;
        Ok(CollectionDetail {
            collection_type: parts[0].to_string(),
            kill_tick,
            killer_name: parts[2].to_string(),
            killer_steamid: parts[3].parse()?,
            player_team: parts[4].to_string(),
            player_weapon: parts[5].to_string(),
            player_weapon_id: parts[6].parse()?,
            player_pos_x: parts[7].parse()?,
            player_pos_y: parts[8].parse()?,
            player_pos_z: parts[9].parse()?,
            player_view_pitch: parts[10].parse()?,
            player_view_yaw: parts[11].parse()?,
            victim_name: parts[12].to_string(),
            victim_steamid: parts[13].parse()?,
            victim_team: parts[14].to_string(),
            victim_pos_x: parts[15].parse()?,
            victim_pos_y: parts[16].parse()?,
            victim_pos_z: parts[17].parse()?,
            distance_to_enemy: parts[18].parse()?,
            ticks_since_last_kill: parts[19].parse()?,
            distance_moved_since_last_kill: parts[20].parse()?,
            killer_index: parts[21].parse()?,
            victim_index: parts[22].parse()?,
        })
    } else {
        Err(anyhow!("Invalid collection detail line format"))
    }
}
