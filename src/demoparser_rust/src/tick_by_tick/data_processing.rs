//! Data processing utilities for CS2 demo parsing
//!
//! This module handles the main processing logic for extracting
//! tick-by-tick data from parsed CS2 demos.

use ahash::AHashMap;
use parser::first_pass::prop_controller::*;
use parser::parse_demo::DemoOutput;
use parser::second_pass::variants::{PropColumn, VarVec};
use std::collections::HashSet;

use super::button_press::ButtonParser;
use super::cache::DataCache;
use super::data_extraction::*;
use super::data_types::{
    GloveCosmeticObservation, KeychainObservation, StickerObservation, TickRecord, PlayerStateObservation,
    WeaponCosmeticObservation,
};
use super::team_parser;
use super::velocity_processing::VelocityExtractor;

/// The parser emits its synthetic `entity_id` column as I32 on this source path,
/// while older paths may expose U32. Accept only nonnegative I32 indices here;
/// other cached properties retain their existing conversion rules.
fn get_entity_id_value(
    df: &AHashMap<u32, PropColumn>,
    prop_id: &u32,
    index: usize,
) -> Option<u32> {
    get_u32_value(df, prop_id, index).or_else(|| {
        get_i32_value(df, prop_id, index).and_then(|value| u32::try_from(value).ok())
    })
}

/// Data processing configuration
#[derive(Debug, Clone)]
pub struct ProcessingConfig {
    // pub target_steamid: u64,
    pub debug_output: bool,
    pub data_offset: usize,
}

impl ProcessingConfig {
    pub fn new(_target_steamid: u64) -> Self {
        Self {
            // target_steamid,
            debug_output: false,
            data_offset: 0,
        }
    }

    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug_output = debug;
        self
    }

    pub fn with_data_offset(mut self, offset: usize) -> Self {
        self.data_offset = offset;
        self
    }
}

/// Data processor for CS2 demo tick data
pub struct TickDataProcessor {
    config: ProcessingConfig,
    cache: DataCache,
    // The last direct team sample seen for each player.  This is intentionally
    // not a match-wide roster: a SteamID changes side at halftime.
    team_lookup: std::collections::HashMap<u64, String>,
}

fn keychain_observation(
    df: &AHashMap<u32, PropColumn>,
    index: usize,
) -> Option<KeychainObservation> {
    let packed = df
        .get(&WEAPON_KEYCHAIN_ID)
        .and_then(|column| column.data.as_ref())
        .and_then(|data| match data {
            VarVec::U32Vec(values) => values.get(index),
            _ => None,
        })?;
    if packed.len() != 9 || packed[0] == 0 {
        return None;
    }
    let mask = packed[1];
    let raw = |bit: u32, index: usize| ((mask & (1 << bit)) != 0).then_some(packed[index]);
    let float = |bit, index| {
        raw(bit, index)
            .map(f32::from_bits)
            .filter(|value| value.is_finite())
    };
    Some(KeychainObservation {
        keychain_id: packed[0],
        offset_x: float(0, 2),
        offset_y: float(1, 3),
        offset_z: float(2, 4),
        seed: raw(3, 5),
        highlight: raw(4, 6),
        sticker_id: raw(5, 7),
        display_case_keychain_id: raw(6, 8),
    })
}

impl TickDataProcessor {
    /// Create a new tick data processor
    pub fn new(config: ProcessingConfig) -> Self {
        Self {
            config,
            cache: DataCache::new(),
            // dead_players: HashSet::new(),
            team_lookup: std::collections::HashMap::new(),
        }
    }

    /// Process demo output for multiple players and return grouped records
    ///
    /// This is the high-performance version that processes all players in a single pass
    /// and groups the results by SteamID, avoiding the need for multiple demo parses.
    ///
    /// # Arguments
    /// * `output` - The parsed demo output containing all player data
    /// * `tracked_steamids` - Vector of SteamIDs to extract data for
    ///
    /// # Returns
    /// * HashMap mapping SteamID to vector of TickRecords for that player
    pub fn process_demo_output_multi_player(
        &mut self,
        output: &DemoOutput,
        tracked_steamids: &[u64],
    ) -> Result<std::collections::HashMap<u64, Vec<TickRecord>>, Box<dyn std::error::Error>> {
        let data_len = self.get_data_length(&output.df)?;

        if self.config.debug_output {
            println!(
                "Processing {} data points for {} players...",
                data_len,
                tracked_steamids.len()
            );
        }

        // Initialize cache with demo data
        self.cache.initialize(output, self.config.debug_output);

        // Start each collection window with no historical team.  Values are
        // populated while rows are read in tick order, so halftime cannot leak
        // the terminal/first side into an earlier window.
        self.team_lookup.clear();

        // Create a set for fast SteamID lookup
        let tracked_set: HashSet<u64> = tracked_steamids.iter().copied().collect();

        // Track dead players per SteamID
        let mut dead_players_multi: std::collections::HashMap<u64, bool> =
            std::collections::HashMap::new();

        // Initialize result map
        let mut grouped_records: std::collections::HashMap<u64, Vec<TickRecord>> =
            std::collections::HashMap::new();
        for &steamid in tracked_steamids {
            grouped_records.insert(steamid, Vec::new());
        }

        // Calculate velocity offset using the first tracked SteamID as reference
        let reference_steamid = tracked_steamids.get(0).copied().unwrap_or(0);
        let velocity_offset = self.cache.get_velocity_offset(
            &output.df,
            data_len,
            reference_steamid,
            self.config.debug_output,
        );

        if self.config.debug_output {
            println!(
                "Using cached velocity offset: {} (reference SteamID: {})",
                velocity_offset, reference_steamid
            );
        }

        for i in 0..data_len {
            // Get current player's SteamID
            let current_steamid =
                if let Some(steamid_data) = output.df.get(&self.cache.common_prop_ids.steamid_id) {
                    if let Some(VarVec::U64(steamids)) = &steamid_data.data {
                        if let Some(Some(steamid)) = steamids.get(i) {
                            *steamid
                        } else {
                            continue; // Skip if no SteamID
                        }
                    } else {
                        continue; // Skip if no SteamID data
                    }
                } else {
                    continue; // Skip if no SteamID column
                };

            // Only process if this player is in our tracked list
            if !tracked_set.contains(&current_steamid) {
                continue;
            }

            let team = self.observe_team_at_row(&output.df, i, current_steamid);

            // Check if player is alive
            let alive = get_bool_as_u32(&output.df, &self.cache.common_prop_ids.is_alive_id, i);
            let was_dead = dead_players_multi
                .get(&current_steamid)
                .copied()
                .unwrap_or(false);

            if alive == 0 {
                // Emit one gap marker per death/lifecycle transition, but keep
                // observing later rows so a respawn starts a new pawn life.
                if !was_dead {
                    let dead_record = self.create_dead_record_for_steamid(
                        &output.df,
                        i,
                        current_steamid,
                        team.clone(),
                    )?;
                    if let Some(records) = grouped_records.get_mut(&current_steamid) {
                        records.push(dead_record);
                    }
                }
                dead_players_multi.insert(current_steamid, true);
                continue;
            }
            if was_dead {
                dead_players_multi.insert(current_steamid, false);
            }

            let record = self.extract_tick_record_for_steamid(
                &output.df,
                i,
                velocity_offset,
                current_steamid,
                team,
            )?;
            if let Some(records) = grouped_records.get_mut(&current_steamid) {
                records.push(record);
            }
        }

        if self.config.debug_output {
            for (&steamid, records) in &grouped_records {
                println!("SteamID {}: {} records", steamid, records.len());
            }
        }

        Ok(grouped_records)
    }

    /// Get the data length from the tick column
    fn get_data_length(
        &self,
        df: &AHashMap<u32, PropColumn>,
    ) -> Result<usize, Box<dyn std::error::Error>> {
        if let Some(tick_data) = df.get(&TICK_ID) {
            Ok(tick_data.len())
        } else {
            Err("No tick data found".into())
        }
    }

    /// Extract cached property value as u32
    fn extract_cached_property(
        &self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        property_name: &str,
    ) -> Option<u32> {
        if let Some(prop_id) = self.cache.name_to_id.get(property_name) {
            if property_name == "entity_id" {
                get_entity_id_value(df, prop_id, index)
            } else {
                get_u32_value(df, prop_id, index)
            }
        } else {
            None
        }
    }

    /// Extract cached boolean property value as u32
    fn extract_cached_bool_property(
        &self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        property_name: &str,
    ) -> u32 {
        if let Some(prop_id) = self.cache.name_to_id.get(property_name) {
            get_bool_as_u32(df, prop_id, index)
        } else {
            0
        }
    }

    fn extract_cached_i32_property(
        &self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        property_name: &str,
    ) -> Option<i32> {
        self.cache
            .name_to_id
            .get(property_name)
            .and_then(|prop_id| get_i32_value(df, prop_id, index))
    }

    fn extract_cached_xyz_property(
        &self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        property_name: &str,
    ) -> Option<[f32; 3]> {
        self.cache
            .name_to_id
            .get(property_name)
            .and_then(|prop_id| get_xyz_value(df, prop_id, index))
    }

    /// Create a DEAD record for a specific SteamID (multi-player version)
    fn create_dead_record_for_steamid(
        &mut self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        steamid: u64,
        team: String,
    ) -> Result<TickRecord, Box<dyn std::error::Error>> {
        // Get tick and player information
        let tick = get_i32_value(df, &self.cache.common_prop_ids.tick_id, index).unwrap_or(0);

        Ok(TickRecord {
            button_observation: None,
            state: PlayerStateObservation::default(),
            tick,
            pawn_entity_id: self.extract_cached_property(df, index, "entity_id"),
            agent_definition_index: self.extract_cached_property(
                df,
                index,
                "CCSPlayerController.m_nPawnCharacterDefIndex",
            ),
            weapon: "DEAD".to_string(),
            weapon_id: "nan".to_string(),
            pos_x: f32::NAN, // Changed from 0.0 to NAN to prevent rendering at origin
            pos_y: f32::NAN, // Changed from 0.0 to NAN to prevent rendering at origin
            pos_z: f32::NAN, // Changed from 0.0 to NAN to prevent rendering at origin
            view_pitch: 0.0,
            view_yaw: 0.0,
            steamid,
            team,
            ammo: 0,
            in_reload: 0,
            scoped: 0,
            inspecting: 0,
            airborne: 0,
            velocity: 0.00,
            velocity_x: 0.00,
            velocity_y: 0.00,
            velocity_z: 0.00,
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
            ragdoll_damage_bone: self.extract_cached_i32_property(
                df,
                index,
                "CCSPlayerPawn.m_nRagdollDamageBone",
            ),
            ragdoll_damage_position: self.extract_cached_xyz_property(
                df,
                index,
                "CCSPlayerPawn.m_vRagdollDamagePosition",
            ),
            ragdoll_damage_force: self.extract_cached_xyz_property(
                df,
                index,
                "CCSPlayerPawn.m_vRagdollDamageForce",
            ),
            ragdoll_server_origin: self.extract_cached_xyz_property(
                df,
                index,
                "CCSPlayerPawn.m_vRagdollServerOrigin",
            ),
        })
    }

    /// Extract a complete tick record for a specific SteamID (multi-player version)
    fn extract_tick_record_for_steamid(
        &mut self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        velocity_offset: usize,
        steamid: u64,
        team: String,
    ) -> Result<TickRecord, Box<dyn std::error::Error>> {
        let debug_first_tick = self.config.debug_output && index == 0;

        // Basic tick data using cached property IDs
        let tick = get_i32_value(df, &self.cache.common_prop_ids.tick_id, index).unwrap_or(0);
        let weapon_name = get_string_value(df, &self.cache.common_prop_ids.weapon_name_id, index)
            .unwrap_or_default();

        // Extract weapon ID with fallback logic
        let weapon_id = extract_weapon_id(df, &self.cache.name_to_id, index, debug_first_tick);

        // Position data using cached property IDs
        let pos_x =
            get_f32_value(df, &self.cache.common_prop_ids.player_x_id, index).unwrap_or(0.0);
        let pos_y =
            get_f32_value(df, &self.cache.common_prop_ids.player_y_id, index).unwrap_or(0.0);
        let pos_z =
            get_f32_value(df, &self.cache.common_prop_ids.player_z_id, index).unwrap_or(0.0);
        let pitch = get_f32_value(df, &self.cache.common_prop_ids.pitch_id, index).unwrap_or(0.0);
        let yaw = get_f32_value(df, &self.cache.common_prop_ids.yaw_id, index).unwrap_or(0.0);

        // Button data using specialized parser
        let button_observation =
            ButtonParser::find_button_mask(df, &self.cache.name_to_id, index, debug_first_tick);
        let button_mask = button_observation.map_or(0, |observation| observation.mask);
        let button_states = ButtonParser::extract_button_states(button_mask);

        if debug_first_tick {
            println!(
                "DEBUG: Button mask found: 0x{:08X} ({})",
                button_mask, button_mask
            );
            ButtonParser::debug_print_buttons(button_mask, &button_states);
        }

        // Weapon state using cached lookups
        let ammo = self
            .extract_cached_property(df, index, "m_iClip1")
            .unwrap_or(0);
        let in_reload = self.extract_cached_bool_property(df, index, "m_bInReload");
        let scoped = self.extract_cached_bool_property(df, index, "CCSPlayerPawn.m_bIsScoped");
        let inspecting = button_states.inspect;

        // Extract velocity data using the dedicated velocity processor
        let (velocity, velocity_x, velocity_y, velocity_z) =
            VelocityExtractor::extract_velocity_data(
                df,
                &self.cache.common_prop_ids,
                index,
                velocity_offset,
                debug_first_tick,
            );

        let airborne = get_bool_as_u32(df, &self.cache.common_prop_ids.is_airborne_id, index);

        // Calculate mouse velocity and round to 2 decimal places
        let mouse_velocity = super::velocity_processing::round_to_2_decimals(
            calculate_mouse_velocity(df, &self.cache.common_prop_ids.yaw_id, index, yaw),
        );

        // Movement states using cached lookups
        let walking = self.extract_cached_bool_property(df, index, "CCSPlayerPawn.m_bIsWalking");
        let defusing = self.extract_cached_bool_property(df, index, "CCSPlayerPawn.m_bIsDefusing");

        // Player state using cached property ID
        let alive = get_bool_as_u32(df, &self.cache.common_prop_ids.is_alive_id, index);

        // Vitals (full property paths as registered in parser_config)
        let health = self
            .extract_cached_property(df, index, "CCSPlayerPawn.m_iHealth")
            .unwrap_or(100)
            .min(255) as u8;
        let armor = self
            .extract_cached_property(df, index, "CCSPlayerPawn.m_ArmorValue")
            .unwrap_or(0)
            .min(255) as u8;

        // Crouch state (full path: CCSPlayerPawn → CCSPlayer_MovementServices → m_bDucked)
        let crouching = self.extract_cached_bool_property(
            df,
            index,
            "CCSPlayerPawn.CCSPlayer_MovementServices.m_bDucked",
        );

        let weapon_cosmetic = self.weapon_cosmetic(df, index);
        let glove_cosmetic = self.glove_cosmetic(df, index);

        Ok(TickRecord {
            button_observation,
            state: self.player_state(df, index),
            tick,
            pawn_entity_id: self.extract_cached_property(df, index, "entity_id"),
            agent_definition_index: self.extract_cached_property(
                df,
                index,
                "CCSPlayerController.m_nPawnCharacterDefIndex",
            ),
            weapon: weapon_name,
            weapon_id,
            pos_x,
            pos_y,
            pos_z,
            view_pitch: pitch,
            view_yaw: yaw,
            steamid,
            team,
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
            walking,
            defusing,
            fw: button_states.forward,
            lf: button_states.left,
            rt: button_states.right,
            bk: button_states.back,
            fire: button_states.fire,
            right_click: button_states.right_click,
            alive,
            health,
            armor,
            crouching,
            weapon_cosmetic,
            glove_cosmetic,
            ragdoll_damage_bone: None,
            ragdoll_damage_position: None,
            ragdoll_damage_force: None,
            ragdoll_server_origin: None,
        })
    }

    fn player_state(&self, df: &AHashMap<u32, PropColumn>, index: usize) -> PlayerStateObservation {
        let boolean = |id: Option<u32>| {
            let column = df.get(&id?)?;
            match column.data.as_ref()? {
                VarVec::Bool(values) => values.get(index).copied().flatten(),
                _ => None,
            }
        };
        let float = |name: &str| self.cache.name_to_id.get(name)
            .and_then(|id| get_f32_value(df, id, index)).filter(|v| v.is_finite());
        let view_offset = (|| Some([
                float("CCSPlayerPawn.m_vecX")?,
                float("CCSPlayerPawn.m_vecY")?,
                float("CCSPlayerPawn.m_vecZ")?,
            ]))();
        PlayerStateObservation {
            game_time: float("game_time"),
            airborne: boolean(Some(IS_AIRBORNE_ID)),
            scoped: boolean(self.cache.name_to_id.get("CCSPlayerPawn.m_bIsScoped").copied()),
            flash_duration: float("CCSPlayerPawn.m_flFlashDuration"),
            flash_max_alpha: float("CCSPlayerPawn.m_flFlashMaxAlpha"),
            duck_amount: float("CCSPlayerPawn.CCSPlayer_MovementServices.m_flDuckAmount"),
            view_offset,
        }
    }

    fn observe_team_at_row(
        &mut self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        steamid: u64,
    ) -> String {
        if let Some(team) = team_parser::team_at_row(df, &self.cache.name_to_id, index) {
            self.team_lookup.insert(steamid, team.clone());
            team
        } else {
            self.team_lookup
                .get(&steamid)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string())
        }
    }

    fn cosmetic_u32(
        &self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        name: &str,
    ) -> Option<u32> {
        let custom = match name {
            "weapon_skin_id" => Some(WEAPON_SKIN_ID),
            "weapon_paint_seed" => Some(WEAPON_PAINT_SEED),
            "weapon_float" => Some(WEAPON_FLOAT),
            "glove_paint_id" => Some(GLOVE_PAINT_ID),
            "glove_paint_seed" => Some(GLOVE_PAINT_SEED),
            "glove_paint_float" => Some(GLOVE_PAINT_FLOAT),
            _ => None,
        };
        custom
            .or_else(|| self.cache.name_to_id.get(name).copied())
            .and_then(|id| get_u32_value(df, &id, index))
    }
    fn cosmetic_f32(
        &self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        name: &str,
    ) -> Option<f32> {
        let custom = match name {
            "weapon_float" => Some(WEAPON_FLOAT),
            "glove_paint_float" => Some(GLOVE_PAINT_FLOAT),
            _ => None,
        };
        custom
            .or_else(|| self.cache.name_to_id.get(name).copied())
            .and_then(|id| get_f32_value(df, &id, index))
    }
    fn cosmetic_string(
        &self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
        name: &str,
    ) -> Option<String> {
        self.cache
            .name_to_id
            .get(name)
            .and_then(|id| get_string_value(df, id, index))
    }
    fn weapon_cosmetic(
        &self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
    ) -> Option<WeaponCosmeticObservation> {
        let paint_kit_id = self.cosmetic_u32(df, index, "weapon_skin_id")?;
        let high = self.cosmetic_u32(df, index, "item_id_high");
        let low = self.cosmetic_u32(df, index, "item_id_low");
        let stickers = df
            .get(&WEAPON_STICKERS_ID)
            .and_then(|column| column.data.as_ref())
            .and_then(|data| match data {
                VarVec::Stickers(values) => values.get(index),
                _ => None,
            })
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|sticker| StickerObservation {
                sticker_id: sticker.id,
                wear: sticker.wear,
                slot: Some(sticker.slot),
                scale: sticker.scale,
                rotation: sticker.rotation,
                offset_x: sticker.offset_x,
                offset_y: sticker.offset_y,
                schema: sticker.schema,
            })
            .collect();
        let keychain = keychain_observation(df, index);
        Some(WeaponCosmeticObservation {
            item_definition_index: self
                .cosmetic_u32(df, index, "m_iItemDefinitionIndex")
                .and_then(|v| u16::try_from(v).ok()),
            item_id: match (high, low) {
                (Some(high), Some(low)) => Some(((high as u64) << 32) | low as u64),
                _ => None,
            },
            paint_kit_id: Some(paint_kit_id),
            paint_seed: self.cosmetic_u32(df, index, "weapon_paint_seed"),
            wear: self.cosmetic_f32(df, index, "weapon_float"),
            quality: self
                .cosmetic_u32(df, index, "m_iEntityQuality")
                .and_then(|v| u16::try_from(v).ok()),
            stattrak: None,
            custom_name: self.cosmetic_string(df, index, "m_szCustomName"),
            stickers,
            keychain,
        })
    }
    fn glove_cosmetic(
        &self,
        df: &AHashMap<u32, PropColumn>,
        index: usize,
    ) -> Option<GloveCosmeticObservation> {
        let paint_kit_id = self.cosmetic_u32(df, index, "glove_paint_id")?;
        Some(GloveCosmeticObservation {
            item_definition_index: self
                .cosmetic_u32(df, index, "CCSPlayerPawn.m_iItemDefinitionIndex")
                .and_then(|v| u16::try_from(v).ok()),
            item_id: None,
            paint_kit_id: Some(paint_kit_id),
            paint_seed: self.cosmetic_u32(df, index, "glove_paint_seed"),
            wear: self.cosmetic_f32(df, index, "glove_paint_float"),
            quality: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_id_accepts_nonnegative_i32_and_legacy_u32_but_rejects_negative() {
        let prop_id = ENTITY_ID_ID;
        let i32_df = AHashMap::from([(
            prop_id,
            PropColumn {
                data: Some(VarVec::I32(vec![Some(206), Some(-1)])),
                num_nones: 0,
            },
        )]);
        assert_eq!(get_entity_id_value(&i32_df, &prop_id, 0), Some(206));
        assert_eq!(get_entity_id_value(&i32_df, &prop_id, 1), None);

        let u32_df = AHashMap::from([(
            prop_id,
            PropColumn {
                data: Some(VarVec::U32(vec![Some(441)])),
                num_nones: 0,
            },
        )]);
        assert_eq!(get_entity_id_value(&u32_df, &prop_id, 0), Some(441));
    }

    #[test]
    fn packed_keychain_column_decodes_optional_fields_without_numeric_float_conversion() {
        let mut df = AHashMap::new();
        df.insert(
            WEAPON_KEYCHAIN_ID,
            PropColumn {
                data: Some(VarVec::U32Vec(vec![vec![
                    8,
                    0b000_1111,
                    19.084707f32.to_bits(),
                    0.53239834f32.to_bits(),
                    3.5288072f32.to_bits(),
                    63_941,
                    0,
                    0,
                    0,
                ]])),
                num_nones: 0,
            },
        );

        let keychain = keychain_observation(&df, 0).expect("keychain");
        assert_eq!(keychain.keychain_id, 8);
        assert_eq!(keychain.offset_x, Some(19.084707));
        assert_eq!(keychain.offset_y, Some(0.53239834));
        assert_eq!(keychain.offset_z, Some(3.5288072));
        assert_eq!(keychain.seed, Some(63_941));
        assert_eq!(keychain.highlight, None);
    }

    // ProcessingConfig no longer carries a target_steamid: the processor handles every
    // tracked player in one pass (process_demo_output_multi_player), so a single target
    // is meaningless. The constructor still accepts and ignores the argument.
    #[test]
    fn test_processing_config_creation() {
        let config = ProcessingConfig::new(12345)
            .with_debug(true)
            .with_data_offset(2);

        assert_eq!(config.debug_output, true);
        assert_eq!(config.data_offset, 2);
    }

    #[test]
    fn test_processor_creation() {
        let config = ProcessingConfig::new(12345);
        let processor = TickDataProcessor::new(config);

        assert_eq!(processor.config.debug_output, false);
        assert_eq!(processor.config.data_offset, 0);
    }
}
