//! Caching utilities for CS2 demo parsing
//!
//! This module provides efficient caching mechanisms to avoid repeated
//! property lookups and calculations during tick data processing.

use ahash::AHashMap;
use parser::first_pass::prop_controller::*;
use parser::parse_demo::DemoOutput;
use parser::second_pass::variants::{PropColumn, VarVec};
use std::collections::HashMap;

use super::data_extraction::get_f32_value;

/// Cached property IDs for frequently accessed properties
#[derive(Debug, Clone)]
pub struct CommonPropertyIds {
    pub velocity_id: u32,
    pub velocity_x_id: u32,
    pub velocity_y_id: u32,
    pub velocity_z_id: u32,
    pub player_x_id: u32,
    pub player_y_id: u32,
    pub player_z_id: u32,
    pub pitch_id: u32,
    pub yaw_id: u32,
    // pub entity_id_id: u32,
    pub steamid_id: u32,
    pub tick_id: u32,
    pub weapon_name_id: u32,
    pub is_airborne_id: u32,
    pub is_alive_id: u32,
}

impl CommonPropertyIds {
    pub fn new() -> Self {
        Self {
            velocity_id: VELOCITY_ID,
            velocity_x_id: VELOCITY_X_ID,
            velocity_y_id: VELOCITY_Y_ID,
            velocity_z_id: VELOCITY_Z_ID,
            player_x_id: PLAYER_X_ID,
            player_y_id: PLAYER_Y_ID,
            player_z_id: PLAYER_Z_ID,
            pitch_id: PITCH_ID,
            yaw_id: YAW_ID,
            // entity_id_id: ENTITY_ID_ID,
            steamid_id: STEAMID_ID,
            tick_id: TICK_ID,
            weapon_name_id: WEAPON_NAME_ID,
            is_airborne_id: IS_AIRBORNE_ID,
            is_alive_id: IS_ALIVE_ID,
        }
    }
}

/// Comprehensive data cache to avoid repeated lookups
#[derive(Debug)]
pub struct DataCache {
    /// Property name to ID mapping (cached once)
    pub name_to_id: HashMap<String, u32>,

    /// Common property IDs for fast access
    pub common_prop_ids: CommonPropertyIds,

    /// Detected velocity offset (cached once)
    velocity_offset: Option<usize>,

    /// Flag to track if cache has been initialized
    initialized: bool,
}

impl DataCache {
    pub fn new() -> Self {
        Self {
            name_to_id: HashMap::new(),
            common_prop_ids: CommonPropertyIds::new(),
            velocity_offset: None,
            initialized: false,
        }
    }

    /// Initialize the cache with data from demo output
    pub fn initialize(&mut self, output: &DemoOutput, debug: bool) {
        if self.initialized {
            return;
        }

        // Cache property name to ID mapping
        for (id, name) in &output.prop_controller.id_to_name {
            self.name_to_id.insert(name.clone(), *id);
        }

        // Custom user-command state is registered in prop_infos, not the
        // sendtable name map. Expose only the requested button fallback and
        // preserve any existing mapping; an absent registration stays absent.
        for prop in &output.prop_controller.prop_infos {
            if prop.is_player_prop && prop.prop_name == "usercmd_buttonstate_1" {
                self.name_to_id
                    .entry(prop.prop_name.clone())
                    .or_insert(prop.id);
            }
        }

        // `entity_id` is a synthetic player dataframe column, not a sendtable
        // property, so some parser paths emit it without an id-to-name entry.
        // Register only the known synthetic ID and only when that column exists.
        if output.df.contains_key(&ENTITY_ID_ID) {
            self.name_to_id
                .entry("entity_id".to_string())
                .or_insert(ENTITY_ID_ID);
        }

        if debug {
            println!("Cached {} property mappings", self.name_to_id.len());
        }

        self.initialized = true;
    }

    /// Get or calculate velocity offset
    pub fn get_velocity_offset(
        &mut self,
        df: &AHashMap<u32, PropColumn>,
        data_len: usize,
        target_steamid: u64,
        debug: bool,
    ) -> usize {
        if let Some(offset) = self.velocity_offset {
            return offset;
        }

        let offset = self.detect_velocity_offset(df, data_len, target_steamid, debug);
        self.velocity_offset = Some(offset);

        if debug {
            println!("Cached velocity offset: {}", offset);
        }

        offset
    }

    /// Detect velocity offset (moved from TickDataProcessor)
    fn detect_velocity_offset(
        &self,
        df: &AHashMap<u32, PropColumn>,
        data_len: usize,
        target_steamid: u64,
        debug: bool,
    ) -> usize {
        // Find entries for target player
        let mut target_player_indices = Vec::new();

        for i in 0..std::cmp::min(data_len, 20) {
            if let Some(steamid_data) = df.get(&self.common_prop_ids.steamid_id) {
                if let Some(VarVec::U64(steamids)) = &steamid_data.data {
                    if let Some(Some(steamid)) = steamids.get(i) {
                        if *steamid == target_steamid {
                            target_player_indices.push(i);
                        }
                    }
                }
            }
        }

        if target_player_indices.is_empty() {
            if debug {
                println!("WARNING: No target player data found for velocity offset detection");
            }
            return 0;
        }

        // Check velocity data length
        let velocity_data_len = if let Some(vel_data) = df.get(&self.common_prop_ids.velocity_id) {
            vel_data.len()
        } else {
            if debug {
                println!("WARNING: No velocity data found in dataframe");
            }
            return 0;
        };

        if debug {
            println!("Target player indices: {:?}", target_player_indices);
            println!("Velocity data length: {}", velocity_data_len);
        }

        // Try different offsets
        for offset in 0..=5 {
            let mut found_valid_velocity = false;

            for &player_idx in &target_player_indices {
                let velocity_idx = player_idx + offset;

                if velocity_idx < velocity_data_len {
                    let velocity_row_steamid = df
                        .get(&self.common_prop_ids.steamid_id)
                        .and_then(|column| column.data.as_ref())
                        .and_then(|data| match data {
                            VarVec::U64(steamids) => steamids.get(velocity_idx).copied().flatten(),
                            _ => None,
                        });
                    if velocity_row_steamid != Some(target_steamid) {
                        continue;
                    }

                    if let Some(velocity) =
                        get_f32_value(df, &self.common_prop_ids.velocity_id, velocity_idx)
                    {
                        if velocity > 0.1 {
                            if debug {
                                println!("Found valid velocity {} at player_idx={}, velocity_idx={} (offset={})",
                                        velocity, player_idx, velocity_idx, offset);
                            }
                            found_valid_velocity = true;
                            break;
                        }
                    }
                }
            }

            if found_valid_velocity {
                if debug {
                    println!("Detected velocity offset: {}", offset);
                }
                return offset;
            }
        }

        if debug {
            println!("WARNING: Could not detect velocity offset, using 0");
        }

        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick_by_tick::button_press::{ButtonObservation, ButtonParser, ButtonSource};

    fn output_with_requested_properties(names: &[&str]) -> DemoOutput {
        let mut controller = PropController::new(
            names.iter().map(|name| name.to_string()).collect(),
            vec![],
            AHashMap::default(),
            AHashMap::from_iter([(
                "usercmd_buttonstate_1".to_string(),
                "friendly_buttons".to_string(),
            )]),
            false,
            &[],
            false,
        );
        controller.set_custom_propinfos();
        DemoOutput {
            ag2_recipes: Vec::new(),
            audio_events: vec![],
            smoke_voxels: vec![],
            infernos: vec![],
            utility: Default::default(),
            world_entity_audit: Default::default(),
            world_entities: Vec::new(),
            df: AHashMap::default(),
            game_events: vec![],
            skins: vec![],
            item_drops: vec![],
            weapon_entity_snapshots: vec![],
            chat_messages: vec![],
            convars: AHashMap::default(),
            header: None,
            player_md: vec![],
            roster: vec![],
            game_events_counter: Default::default(),
            uniq_prop_names: vec![],
            projectiles: vec![],
            voice_data: vec![],
            prop_controller: controller,
            df_per_player: AHashMap::default(),
        }
    }

    fn button_column(values: Vec<Option<u64>>) -> PropColumn {
        PropColumn {
            num_nones: values.iter().filter(|value| value.is_none()).count(),
            data: Some(VarVec::U64(values)),
        }
    }

    fn float_column(values: Vec<Option<f32>>) -> PropColumn {
        PropColumn {
            num_nones: values.iter().filter(|value| value.is_none()).count(),
            data: Some(VarVec::F32(values)),
        }
    }

    #[test]
    fn velocity_offset_ignores_nonzero_speed_from_another_player() {
        let prop_ids = CommonPropertyIds::new();
        let df = AHashMap::from([
            (
                prop_ids.steamid_id,
                button_column(vec![Some(10), Some(20), Some(10)]),
            ),
            (
                prop_ids.velocity_id,
                float_column(vec![Some(0.0), Some(100.0), Some(0.0)]),
            ),
        ]);

        let mut cache = DataCache::new();
        assert_eq!(cache.get_velocity_offset(&df, 3, 10, false), 0);
    }

    #[test]
    fn registered_usercmd_reaches_button_parser_with_presence_intact() {
        let mut output = output_with_requested_properties(&["usercmd_buttonstate_1"]);
        assert_eq!(USERCMD_BUTTONSTATE_1, 100000029);
        assert!(!output
            .prop_controller
            .id_to_name
            .contains_key(&USERCMD_BUTTONSTATE_1));
        let info = output
            .prop_controller
            .prop_infos
            .iter()
            .find(|prop| prop.prop_name == "usercmd_buttonstate_1")
            .unwrap();
        assert_eq!(info.id, USERCMD_BUTTONSTATE_1);
        assert_eq!(info.prop_friendly_name, "friendly_buttons");
        output.df.insert(
            USERCMD_BUTTONSTATE_1,
            button_column(vec![Some(0), Some(1), None]),
        );

        let mut cache = DataCache::new();
        cache.initialize(&output, false);
        for (index, mask) in [Some(0), Some(1), None].into_iter().enumerate() {
            assert_eq!(
                ButtonParser::find_button_mask(&output.df, &cache.name_to_id, index, false),
                mask.map(|mask| ButtonObservation {
                    mask,
                    source: ButtonSource::UserCommandState1
                })
            );
        }
        assert!(!cache.name_to_id.contains_key("friendly_buttons"));
    }

    #[test]
    fn movement_previous_zero_takes_precedence_over_registered_usercmd() {
        let mut output = output_with_requested_properties(&["usercmd_buttonstate_1"]);
        output.prop_controller.id_to_name.insert(
            42,
            "CCSPlayer_MovementServices.m_nButtonDownMaskPrev".to_string(),
        );
        output.df.insert(42, button_column(vec![Some(0), None]));
        output
            .df
            .insert(USERCMD_BUTTONSTATE_1, button_column(vec![Some(1), Some(1)]));
        let mut cache = DataCache::new();
        cache.initialize(&output, false);
        assert_eq!(
            ButtonParser::find_button_mask(&output.df, &cache.name_to_id, 0, false),
            Some(ButtonObservation {
                mask: 0,
                source: ButtonSource::MovementPrevious
            })
        );
        assert_eq!(
            ButtonParser::find_button_mask(&output.df, &cache.name_to_id, 1, false),
            Some(ButtonObservation {
                mask: 1,
                source: ButtonSource::UserCommandState1
            })
        );
    }

    #[test]
    fn existing_mapping_and_single_initialization_are_preserved() {
        let mut output = output_with_requested_properties(&["usercmd_buttonstate_1"]);
        output
            .prop_controller
            .id_to_name
            .insert(42, "usercmd_buttonstate_1".to_string());
        output
            .prop_controller
            .id_to_name
            .insert(43, "existing_sendtable_alias".to_string());
        output.df.insert(42, button_column(vec![Some(0)]));
        output
            .df
            .insert(USERCMD_BUTTONSTATE_1, button_column(vec![Some(1)]));
        let mut cache = DataCache::new();
        cache.initialize(&output, false);
        assert_eq!(cache.name_to_id.get("usercmd_buttonstate_1"), Some(&42));
        assert_eq!(cache.name_to_id.get("existing_sendtable_alias"), Some(&43));
        assert_eq!(
            ButtonParser::find_button_mask(&output.df, &cache.name_to_id, 0, false),
            Some(ButtonObservation {
                mask: 0,
                source: ButtonSource::UserCommandState1
            })
        );
        output
            .prop_controller
            .id_to_name
            .insert(44, "later_alias".to_string());
        cache.initialize(&output, false);
        assert!(!cache.name_to_id.contains_key("later_alias"));
    }

    #[test]
    fn absent_registration_is_not_inferred_from_numeric_column() {
        let mut output = output_with_requested_properties(&["usercmd_buttonstate_2", "X"]);
        output
            .df
            .insert(USERCMD_BUTTONSTATE_1, button_column(vec![Some(1)]));
        let mut cache = DataCache::new();
        cache.initialize(&output, false);
        assert!(!cache.name_to_id.contains_key("usercmd_buttonstate_1"));
        assert!(!cache.name_to_id.contains_key("usercmd_buttonstate_2"));
        assert!(!cache.name_to_id.contains_key("X"));
        assert_eq!(
            ButtonParser::find_button_mask(&output.df, &cache.name_to_id, 0, false),
            None
        );
    }

    #[test]
    fn synthetic_entity_id_is_registered_only_when_its_known_column_exists() {
        let mut output = output_with_requested_properties(&["X"]);
        assert!(!output
            .prop_controller
            .id_to_name
            .contains_key(&ENTITY_ID_ID));
        output.df.insert(ENTITY_ID_ID, button_column(vec![Some(206)]));

        let mut cache = DataCache::new();
        cache.initialize(&output, false);
        assert_eq!(cache.name_to_id.get("entity_id"), Some(&ENTITY_ID_ID));

        let output_without_column = output_with_requested_properties(&["X"]);
        let mut empty_cache = DataCache::new();
        empty_cache.initialize(&output_without_column, false);
        assert!(!empty_cache.name_to_id.contains_key("entity_id"));
    }
}
