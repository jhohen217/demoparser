//! Kill model for the interface crate
//!
//! This module contains the Kill struct, which represents a kill event in a CS:GO demo.

use serde::{Deserialize, Serialize};

/// Kill struct representing a kill event in a CS:GO demo
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Kill {
    /// Tick when the kill occurred
    pub tick: i32,
    /// Killer player name
    pub killer_name: String,
    /// Killer player SteamID
    pub killer_steamid: String,
    /// Killer player team
    pub killer_team: String,
    /// Killer position X
    pub killer_pos_x: f32,
    /// Killer position Y
    pub killer_pos_y: f32,
    /// Killer position Z
    pub killer_pos_z: f32,
    /// Killer view pitch
    pub killer_view_pitch: f32,
    /// Killer view yaw
    pub killer_view_yaw: f32,
    /// Victim player name
    pub victim_name: String,
    /// Victim player SteamID
    pub victim_steamid: String,
    /// Victim player team
    pub victim_team: String,
    /// Victim position X
    pub victim_pos_x: f32,
    /// Victim position Y
    pub victim_pos_y: f32,
    /// Victim position Z
    pub victim_pos_z: f32,
    /// Weapon used for the kill
    pub weapon: String,
    /// Weapon ID used for the kill
    pub weapon_id: String,
    /// Distance between killer and victim
    pub distance_to_enemy: f32,
    /// Whether the kill was a headshot
    pub headshot: bool,
    /// Whether the kill penetrated a surface
    pub penetrated: bool,
    /// Whether the attacker was blind
    pub attacker_blind: bool,
    /// Whether the kill was through smoke
    pub thru_smoke: bool,
    /// Whether the kill was a no-scope
    pub no_scope: bool,
    /// Optional airborne state at the death event (not necessarily at shot launch).
    #[serde(default)]
    pub attacker_airborne: Option<bool>,
    #[serde(default)]
    pub victim_airborne: Option<bool>,
    /// Raw number of penetrated surfaces; absent on legacy boolean events.
    #[serde(default)]
    pub penetration_count: Option<u32>,
    /// Same bit positions as parser::second_pass::kill_modifiers; absent bits
    /// distinguish unavailable flags from the legacy boolean defaults above.
    #[serde(default)]
    pub modifier_known_flags: u8,
    /// Round number
    pub round: i32,
    /// Round start tick
    pub round_start_tick: i32,
    /// Round end tick
    pub round_end_tick: i32,
    /// Round freeze end tick
    pub round_freeze_end: i32,
    /// Ticks since last kill by this killer
    pub ticks_since_last_kill: i32,
    /// Distance moved since last kill by this killer
    pub distance_moved_since_last_kill: f32,
    /// Killer player index
    pub killer_index: i32,
    /// Victim player index
    pub victim_index: i32,
    /// Whether the killer is controlling a bot
    pub killer_is_controlling_bot: bool,
}

impl Kill {
    /// Create a new kill
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tick: i32,
        killer_name: String,
        killer_steamid: String,
        killer_team: String,
        killer_pos_x: f32,
        killer_pos_y: f32,
        killer_pos_z: f32,
        killer_view_pitch: f32,
        killer_view_yaw: f32,
        victim_name: String,
        victim_steamid: String,
        victim_team: String,
        victim_pos_x: f32,
        victim_pos_y: f32,
        victim_pos_z: f32,
        weapon: String,
        weapon_id: String,
        distance_to_enemy: f32,
        headshot: bool,
        penetrated: bool,
        attacker_blind: bool,
        thru_smoke: bool,
        no_scope: bool,
        round: i32,
        round_start_tick: i32,
        round_end_tick: i32,
        round_freeze_end: i32,
        ticks_since_last_kill: i32,
        distance_moved_since_last_kill: f32,
        killer_index: i32,
        victim_index: i32,
        killer_is_controlling_bot: bool,
    ) -> Self {
        Kill {
            tick,
            killer_name,
            killer_steamid,
            killer_team,
            killer_pos_x,
            killer_pos_y,
            killer_pos_z,
            killer_view_pitch,
            killer_view_yaw,
            victim_name,
            victim_steamid,
            victim_team,
            victim_pos_x,
            victim_pos_y,
            victim_pos_z,
            weapon,
            weapon_id,
            distance_to_enemy,
            headshot,
            penetrated,
            attacker_blind,
            thru_smoke,
            no_scope,
            attacker_airborne: None,
            victim_airborne: None,
            penetration_count: None,
            modifier_known_flags: 0x1f,
            round,
            round_start_tick,
            round_end_tick,
            round_freeze_end,
            ticks_since_last_kill,
            distance_moved_since_last_kill,
            killer_index,
            victim_index,
            killer_is_controlling_bot,
        }
    }

    /// Get the tick when the kill occurred
    pub fn get_tick(&self) -> i32 {
        self.tick
    }

    /// Get the killer player name
    pub fn get_killer_name(&self) -> &str {
        &self.killer_name
    }

    /// Get the killer player SteamID
    pub fn get_killer_steamid(&self) -> &str {
        &self.killer_steamid
    }

    /// Get the killer player team
    pub fn get_killer_team(&self) -> &str {
        &self.killer_team
    }

    /// Get the killer position [x, y, z]
    pub fn get_killer_pos(&self) -> [f32; 3] {
        [self.killer_pos_x, self.killer_pos_y, self.killer_pos_z]
    }

    /// Get the killer view pitch
    pub fn get_killer_view_pitch(&self) -> f32 {
        self.killer_view_pitch
    }

    /// Get the killer view yaw
    pub fn get_killer_view_yaw(&self) -> f32 {
        self.killer_view_yaw
    }

    /// Get the victim player name
    pub fn get_victim_name(&self) -> &str {
        &self.victim_name
    }

    /// Get the victim player SteamID
    pub fn get_victim_steamid(&self) -> &str {
        &self.victim_steamid
    }

    /// Get the victim player team
    pub fn get_victim_team(&self) -> &str {
        &self.victim_team
    }

    /// Get the victim position [x, y, z]
    pub fn get_victim_pos(&self) -> [f32; 3] {
        [self.victim_pos_x, self.victim_pos_y, self.victim_pos_z]
    }

    /// Get the weapon used for the kill
    pub fn get_weapon(&self) -> &str {
        &self.weapon
    }

    /// Get the weapon ID used for the kill
    pub fn get_weapon_id(&self) -> &str {
        &self.weapon_id
    }

    /// Get the distance between killer and victim
    pub fn get_distance_to_enemy(&self) -> f32 {
        self.distance_to_enemy
    }

    /// Check if the kill was a headshot
    pub fn is_headshot(&self) -> bool {
        self.headshot
    }

    /// Check if the kill penetrated a surface
    pub fn is_penetrated(&self) -> bool {
        self.penetrated
    }

    /// Check if the attacker was blind
    pub fn is_attacker_blind(&self) -> bool {
        self.attacker_blind
    }

    /// Check if the kill was through smoke
    pub fn is_thru_smoke(&self) -> bool {
        self.thru_smoke
    }

    /// Check if the kill was a no-scope
    pub fn is_no_scope(&self) -> bool {
        self.no_scope
    }

    /// Get the round number
    pub fn get_round(&self) -> i32 {
        self.round
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

    /// Get the ticks since last kill by this killer
    pub fn get_ticks_since_last_kill(&self) -> i32 {
        self.ticks_since_last_kill
    }

    /// Get the distance moved since last kill by this killer
    pub fn get_distance_moved_since_last_kill(&self) -> f32 {
        self.distance_moved_since_last_kill
    }

    /// Get the killer player index
    pub fn get_killer_index(&self) -> i32 {
        self.killer_index
    }

    /// Get the victim player index
    pub fn get_victim_index(&self) -> i32 {
        self.victim_index
    }
}
