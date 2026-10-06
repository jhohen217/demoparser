//! Tick data model for representing player state at specific ticks

use serde::{Deserialize, Serialize};

/// TickData struct representing player state at a specific tick
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TickData {
    /// Game tick number
    pub tick: u32,
    /// Player SteamID
    pub steamid: u64,
    /// Current weapon name
    pub weapon: String,
    /// Current weapon ID
    pub weapon_id: String,
    /// Player X position
    pub pos_x: f32,
    /// Player Y position
    pub pos_y: f32,
    /// Player Z position
    pub pos_z: f32,
    /// View pitch (vertical look angle)
    pub view_pitch: f32,
    /// View yaw (horizontal look angle)
    pub view_yaw: f32,
    /// Player index in the game
    pub player_index: i32,
    /// Current ammo count
    pub ammo: i32,
    /// Whether player is reloading
    pub in_reload: bool,
    /// Whether player is scoped
    pub scoped: bool,
    /// Whether player is inspecting weapon
    pub inspecting: bool,
    /// Whether player is airborne
    pub airborne: bool,
    /// Overall movement velocity
    pub velocity: f32,
    /// X-axis velocity
    pub velocity_x: f32,
    /// Y-axis velocity
    pub velocity_y: f32,
    /// Z-axis velocity
    pub velocity_z: f32,
    /// Mouse movement velocity
    pub mouse_velocity: f32,
    /// Whether player is strafing
    pub strafing: bool,
    /// Whether player is walking
    pub walking: bool,
    /// Whether player is defusing
    pub defusing: bool,
    /// Forward movement button pressed
    pub buttons_fw: bool,
    /// Left movement button pressed
    pub buttons_lf: bool,
    /// Right movement button pressed
    pub buttons_rt: bool,
    /// Back movement button pressed
    pub buttons_bk: bool,
    /// Fire button pressed
    pub buttons_fire: bool,
    /// Whether player is alive
    pub alive: bool,
    /// Player team (2 = T, 3 = CT)
    pub team: i32,
    /// Active weapon skin name
    pub weapon_skin: String,
    /// Weapon stickers data (list of sticker names)
    pub weapon_stickers: Vec<String>,
}

impl TickData {
    /// Create a new TickData instance
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tick: u32,
        steamid: u64,
        weapon: String,
        weapon_id: String,
        pos_x: f32,
        pos_y: f32,
        pos_z: f32,
        view_pitch: f32,
        view_yaw: f32,
        player_index: i32,
        ammo: i32,
        in_reload: bool,
        scoped: bool,
        inspecting: bool,
        airborne: bool,
        velocity: f32,
        velocity_x: f32,
        velocity_y: f32,
        velocity_z: f32,
        mouse_velocity: f32,
        strafing: bool,
        walking: bool,
        defusing: bool,
        buttons_fw: bool,
        buttons_lf: bool,
        buttons_rt: bool,
        buttons_bk: bool,
        buttons_fire: bool,
        alive: bool,
        team: i32,
        weapon_skin: String,
        weapon_stickers: Vec<String>,
    ) -> Self {
        TickData {
            tick,
            steamid,
            weapon,
            weapon_id,
            pos_x,
            pos_y,
            pos_z,
            view_pitch,
            view_yaw,
            player_index,
            ammo,
            in_reload,
            scoped,
            inspecting,
            airborne,
            velocity,
            velocity_x,
            velocity_y,
            velocity_z,
            mouse_velocity,
            strafing,
            walking,
            defusing,
            buttons_fw,
            buttons_lf,
            buttons_rt,
            buttons_bk,
            buttons_fire,
            alive,
            team,
            weapon_skin,
            weapon_stickers,
        }
    }

    /// Get player position as a tuple
    pub fn get_position(&self) -> (f32, f32, f32) {
        (self.pos_x, self.pos_y, self.pos_z)
    }

    /// Get player view angles as a tuple
    pub fn get_view_angles(&self) -> (f32, f32) {
        (self.view_pitch, self.view_yaw)
    }

    /// Get velocity as a tuple
    pub fn get_velocity(&self) -> (f32, f32, f32) {
        (self.velocity_x, self.velocity_y, self.velocity_z)
    }

    /// Check if any movement buttons are pressed
    pub fn is_moving(&self) -> bool {
        self.buttons_fw || self.buttons_lf || self.buttons_rt || self.buttons_bk
    }

    /// Get movement direction as a string
    pub fn get_movement_direction(&self) -> String {
        let mut directions = Vec::new();
        if self.buttons_fw {
            directions.push("FW");
        }
        if self.buttons_bk {
            directions.push("BK");
        }
        if self.buttons_lf {
            directions.push("LF");
        }
        if self.buttons_rt {
            directions.push("RT");
        }

        if directions.is_empty() {
            "NONE".to_string()
        } else {
            directions.join("+")
        }
    }

    /// Calculate 2D movement speed
    pub fn get_2d_speed(&self) -> f32 {
        (self.velocity_x * self.velocity_x + self.velocity_y * self.velocity_y).sqrt()
    }

    /// Check if player is performing an action
    pub fn is_performing_action(&self) -> bool {
        self.in_reload || self.inspecting || self.defusing || self.buttons_fire
    }
}

// Default implementation removed to prevent creation of TickData with dummy/placeholder values.
// TickData should only be created with real data from the demo parser.
