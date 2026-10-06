//! Player info parsing functionality
//!
//! This module handles parsing player information from demo output.

use crate::core::demo_processor::types::PlayerInfo;
use parser::parse_demo::DemoOutput;

/// Parse player info from parser output
pub fn parse_player_info_from_output(output: &DemoOutput) -> Result<Vec<PlayerInfo>, String> {
    let mut players = Vec::new();

    // Community and casual demos often omit the end-of-match scoreboard message.
    // Upstream now exposes a controller-derived roster for exactly that case.
    //
    // Unlike the scoreboard, the roster is every CCSPlayerController the demo ever saw, so
    // spectators/unassigned (team 0/1) have to be dropped here - the scoreboard path never
    // contains them and downstream code assumes every PlayerInfo is on a playing side.
    let roster_fallback: Vec<_>;
    let player_metadata: &[_] = if output.player_md.is_empty() {
        roster_fallback = output
            .roster
            .iter()
            .filter(|p| matches!(p.team_number, Some(2) | Some(3)))
            .cloned()
            .collect();
        &roster_fallback
    } else {
        &output.player_md
    };

    for (i, player) in player_metadata.iter().enumerate() {
        let steamid = player
            .steamid
            .ok_or_else(|| format!("Player at index {} missing SteamID", i))?;

        let name = player
            .name
            .as_ref()
            .ok_or_else(|| format!("Player at index {} missing name", i))?;

        let team_num = player
            .team_number
            .ok_or_else(|| format!("Player {} missing team number", name))?;

        let team = match team_num {
            2 => "T".to_string(),
            3 => "CT".to_string(),
            _ => team_num.to_string(),
        };

        players.push(PlayerInfo {
            index: (i as i32) + 4, // Player indices start at 4
            name: name.clone(),
            steamid: steamid.to_string(),
            team,
        });
    }

    if players.is_empty() {
        return Err("No valid players found in demo".to_string());
    }

    Ok(players)
}
