//! Data types for demo processing
//!
//! This module contains all the data structures used in demo processing.

use serde::{Deserialize, Serialize};

/// Demo info struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DemoInfo {
    /// Demo name
    pub demo_name: String,
    /// Demo path
    pub demo_path: String,
    /// Map name
    pub map_name: String,
    /// Game version extracted from game directory
    pub game_version: String,
    /// Tick rate
    pub tick_rate: f32,
    /// Playback ticks
    pub playback_ticks: i32,
    /// Playback time in seconds
    pub playback_time: f32,
    /// Demo protocol
    pub demo_protocol: i32,
    /// Network protocol
    pub network_protocol: i32,
    /// Server name
    pub server_name: String,
    /// Client name
    pub client_name: String,
    /// Game directory
    pub game_directory: String,
}

/// Player info struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerInfo {
    /// Player index
    pub index: i32,
    /// Player name
    pub name: String,
    /// Player SteamID
    pub steamid: String,
    /// Player team
    pub team: String,
}

/// Round info struct
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundInfo {
    pub next_start_tick: Option<i32>,
    /// Round number
    pub round: i32,
    /// Round start tick
    pub start_tick: i32,
    /// Round end tick
    pub end_tick: i32,
    /// Round freeze end tick
    pub freeze_end: i32,
    /// Round winner
    pub winner: String,
    /// Round win reason
    pub win_reason: String,
}
