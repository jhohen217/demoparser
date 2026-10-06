//! Data types and structures for CS2 demo parsing
//!
//! This module contains the main data structures used to represent
//! tick-by-tick player data extracted from CS2 demo files.

use serde::Serialize;

/// A single sticker as observed on an econ item.  `None` means the demo did
/// not provide that part of the sticker state; it is deliberately distinct
/// from a zero value (which is meaningful for several sticker fields).
#[derive(Debug, Serialize, Clone, Default, PartialEq)]
pub struct StickerObservation {
    pub sticker_id: u32,
    pub wear: Option<f32>,
    pub slot: Option<u32>,
    pub scale: Option<f32>,
    pub rotation: Option<f32>,
    pub offset_x: Option<f32>,
    pub offset_y: Option<f32>,
    pub schema: Option<u32>,
}

/// The single keychain slot observed on a weapon econ item.
#[derive(Debug, Serialize, Clone, Default, PartialEq)]
pub struct KeychainObservation {
    pub keychain_id: u32,
    pub offset_x: Option<f32>,
    pub offset_y: Option<f32>,
    pub offset_z: Option<f32>,
    pub seed: Option<u32>,
    pub highlight: Option<u32>,
    pub sticker_id: Option<u32>,
    pub display_case_keychain_id: Option<u32>,
}

/// Cosmetic identity of the active weapon at one player tick.
///
/// The parser must only populate this when it has an explicit econ item
/// observation.  In particular, a missing paint kit is not represented as
/// paint kit zero: S2R v6 reserves frame cosmetic index zero for unknown.
#[derive(Debug, Serialize, Clone, Default, PartialEq)]
pub struct WeaponCosmeticObservation {
    pub item_definition_index: Option<u16>,
    pub item_id: Option<u64>,
    pub paint_kit_id: Option<u32>,
    pub paint_seed: Option<u32>,
    pub wear: Option<f32>,
    pub quality: Option<u16>,
    pub stattrak: Option<i32>,
    pub custom_name: Option<String>,
    pub stickers: Vec<StickerObservation>,
    pub keychain: Option<KeychainObservation>,
}

/// Cosmetic identity of the gloves worn by a player at one tick.
#[derive(Debug, Serialize, Clone, Default, PartialEq)]
pub struct GloveCosmeticObservation {
    pub item_definition_index: Option<u16>,
    pub item_id: Option<u64>,
    pub paint_kit_id: Option<u32>,
    pub paint_seed: Option<u32>,
    pub wear: Option<f32>,
    pub quality: Option<u16>,
}

/// Represents a weapon fire sequence (burst/spray)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct WeaponFireSequence {
    pub start_tick: i32,
    pub end_tick: i32,
    pub weapon: String,
    pub weapon_id: String,
    pub bullets_used: u32,
    pub kill: u32,         // 0 or 1
    pub victims: Vec<u32>, // victim indices
}

/// Represents a single tick of player data extracted from a CS2 demo
///
/// This structure contains all the relevant information about a player's state
/// at a specific tick, including position, weapon, movement, and input data.
#[derive(Debug, Serialize, Clone)]
#[allow(dead_code)]
pub struct TickRecord {
    /// Optional selected input state; Some(0) is observed, None is unknown.
    #[serde(skip)]
    pub button_observation: Option<super::button_press::ButtonObservation>,
    /// Optional observations used by the S2EX player-state lane. No guessed defaults.
    #[serde(skip)]
    pub state: PlayerStateObservation,
    #[serde(rename = "Tick")]
    pub tick: i32,

    /// Pawn entity index observed for this player tick. Auxiliary S2R authority
    /// sections use it to distinguish respawns; it is not part of legacy CSV.
    #[serde(skip)]
    pub pawn_entity_id: Option<u32>,

    /// Authoritative CCSPlayerController pawn-character definition index.
    #[serde(skip)]
    pub agent_definition_index: Option<u32>,

    #[serde(rename = "Weapon")]
    pub weapon: String,

    #[serde(rename = "WeaponID")]
    pub weapon_id: String,

    // Position data
    #[serde(rename = "PosX")]
    pub pos_x: f32,

    #[serde(rename = "PosY")]
    pub pos_y: f32,

    #[serde(rename = "PosZ")]
    pub pos_z: f32,

    // View angles
    #[serde(rename = "ViewPitch")]
    pub view_pitch: f32,

    #[serde(rename = "ViewYaw")]
    pub view_yaw: f32,

    // Player identification (not included in CSV serialization by default)
    #[serde(skip)]
    pub steamid: u64,

    // Team information
    #[serde(rename = "Team")]
    pub team: String,

    // Weapon state
    #[serde(rename = "Ammo")]
    pub ammo: u32,

    #[serde(rename = "InReload")]
    pub in_reload: u32,

    #[serde(rename = "Scoped")]
    pub scoped: u32,

    #[serde(rename = "Inspecting")]
    pub inspecting: u32,

    // Movement data
    #[serde(rename = "Airborne")]
    pub airborne: u32,

    #[serde(rename = "Velocity")]
    pub velocity: f32,

    #[serde(rename = "VelocityX")]
    pub velocity_x: f32,

    #[serde(rename = "VelocityY")]
    pub velocity_y: f32,

    #[serde(rename = "VelocityZ")]
    pub velocity_z: f32,

    #[serde(rename = "MouseVelocity")]
    pub mouse_velocity: f32,

    // Movement states
    #[serde(rename = "Walking")]
    pub walking: u32,

    #[serde(rename = "Defusing")]
    pub defusing: u32,

    // Button inputs
    #[serde(rename = "FW")]
    pub fw: u32,

    #[serde(rename = "LF")]
    pub lf: u32,

    #[serde(rename = "RT")]
    pub rt: u32,

    #[serde(rename = "BK")]
    pub bk: u32,

    #[serde(rename = "FIRE")]
    pub fire: u32,

    /// Secondary attack is retained for replay presentation but omitted from the legacy CSV.
    #[serde(skip_serializing)]
    pub right_click: u32,

    // Player state (kept internal for death detection, not serialized to CSV)
    #[serde(skip_serializing)]
    pub alive: u32,

    // Vitals
    #[serde(rename = "Health")]
    pub health: u8,

    #[serde(rename = "Armor")]
    pub armor: u8,

    // Crouch state
    #[serde(rename = "Crouching")]
    pub crouching: u32,

    /// Optional active-weapon cosmetic state.  Kept out of the legacy CSV
    /// representation; S2R v6 normalizes it into a compact signature table.
    #[serde(skip)]
    pub weapon_cosmetic: Option<WeaponCosmeticObservation>,

    /// Optional glove cosmetic state.  See `weapon_cosmetic` above.
    #[serde(skip)]
    pub glove_cosmetic: Option<GloveCosmeticObservation>,

    /// Fatal-hit state sampled on the first dead row for this pawn life.
    #[serde(skip)]
    pub ragdoll_damage_bone: Option<i32>,
    #[serde(skip)]
    pub ragdoll_damage_position: Option<[f32; 3]>,
    #[serde(skip)]
    pub ragdoll_damage_force: Option<[f32; 3]>,
    /// Server ragdoll origin sampled on the first dead row for this pawn life.
    /// Retained as nullable diagnostic provenance; it is not a replay physics input.
    #[serde(skip)]
    pub ragdoll_server_origin: Option<[f32; 3]>,
}

impl Default for TickRecord {
    fn default() -> Self {
        Self {
            button_observation: None,
            state: PlayerStateObservation::default(),
            tick: 0,
            pawn_entity_id: None,
            agent_definition_index: None,
            weapon: String::new(),
            weapon_id: "0".to_string(),
            pos_x: 0.0,
            pos_y: 0.0,
            pos_z: 0.0,
            view_pitch: 0.0,
            view_yaw: 0.0,
            steamid: 0,
            team: String::new(),
            ammo: 0,
            in_reload: 0,
            scoped: 0,
            inspecting: 0,
            airborne: 0,
            velocity: 0.0,
            velocity_x: 0.0,
            velocity_y: 0.0,
            velocity_z: 0.0,
            mouse_velocity: 0.0,
            walking: 0,
            defusing: 0,
            fw: 0,
            lf: 0,
            rt: 0,
            bk: 0,
            fire: 0,
            right_click: 0,
            alive: 0,
            health: 0,
            armor: 0,
            crouching: 0,
            weapon_cosmetic: None,
            glove_cosmetic: None,
            ragdoll_damage_bone: None,
            ragdoll_damage_position: None,
            ragdoll_damage_force: None,
            ragdoll_server_origin: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PlayerStateObservation {
    /// Network game time; separate from the demo/clip tick origin.
    pub game_time: Option<f32>,
    pub airborne: Option<bool>,
    pub scoped: Option<bool>,
    pub flash_duration: Option<f32>,
    pub flash_max_alpha: Option<f32>,
    pub duck_amount: Option<f32>,
    pub view_offset: Option<[f32; 3]>,
}
