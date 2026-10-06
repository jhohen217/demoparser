//! Game event parsing functionality
//!
//! This module handles parsing game events from demo output.

use super::death_parser::parse_death_events_from_output;
use super::types::{PlayerInfo, RoundInfo};
use crate::core::game_event::{GameEvent, TeamChange};
use parser::parse_demo::DemoOutput;
use parser::second_pass::variants::Variant;
use std::io;

/// Parse game events from the parser output
pub fn parse_game_events_from_output(
    output: &DemoOutput,
    rounds: &[RoundInfo],
    player_info: &[PlayerInfo],
) -> io::Result<Vec<GameEvent>> {
    let mut game_events: Vec<GameEvent> = Vec::new();

    // First, parse all death events
    let death_events = parse_death_events_from_output(output, rounds, player_info)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    for kill in death_events {
        game_events.push(GameEvent::Kill(kill));
    }

    // Next, parse all team change events
    for event in &output.game_events {
        if event.name == "player_team" {
            let mut tick = None;
            let mut user_steamid = None;
            let mut team_number = None;

            for field in &event.fields {
                match field.name.as_str() {
                    "tick" => {
                        if let Some(Variant::I32(t)) = &field.data {
                            tick = Some(*t);
                        }
                    }
                    "user_steamid" => {
                        if let Some(Variant::U64(id)) = &field.data {
                            user_steamid = Some(*id);
                        } else if let Some(Variant::String(s)) = &field.data {
                            user_steamid = s.parse::<u64>().ok();
                        }
                    }
                    "team" => {
                        if let Some(Variant::I32(t)) = &field.data {
                            team_number = Some(*t);
                        }
                    }
                    _ => {}
                }
            }

            if let (Some(tick), Some(user_steamid), Some(team_number)) =
                (tick, user_steamid, team_number)
            {
                game_events.push(GameEvent::TeamChange(TeamChange {
                    tick,
                    user_steamid,
                    team_number,
                }));
            }
        }
    }

    // Sort all game events by tick
    game_events.sort_by_key(|e| match e {
        GameEvent::Kill(k) => k.tick,
        GameEvent::TeamChange(tc) => tc.tick,
    });

    Ok(game_events)
}

/// Parse game start offset from parser output
pub fn parse_game_start_offset_from_output(output: &DemoOutput) -> Result<f32, String> {
    // Collect all round_start events with their game_time and tick
    let mut round_starts: Vec<(f32, i32)> = Vec::new();

    for event in &output.game_events {
        if event.name == "round_start" {
            let mut game_time = None;
            let mut tick = None;

            for field in &event.fields {
                match field.name.as_str() {
                    "game_time" => {
                        if let Some(Variant::F32(time)) = &field.data {
                            game_time = Some(*time);
                        }
                    }
                    "tick" => {
                        if let Some(Variant::I32(t)) = &field.data {
                            tick = Some(*t);
                        }
                    }
                    _ => {}
                }
            }

            // Only consider events with valid game_time (don't filter by tick)
            if let (Some(time_value), Some(tick_value)) = (game_time, tick) {
                round_starts.push((time_value, tick_value));
            }
        }
    }

    // Don't sort by tick, just return the first game_time value we find
    // This matches Python's behavior which takes the first round_start event's game_time
    if let Some((time_value, _)) = round_starts.first() {
        return Ok(*time_value);
    }

    // Return default game start offset if no proper round_start event is found
    Ok(0.0)
}
