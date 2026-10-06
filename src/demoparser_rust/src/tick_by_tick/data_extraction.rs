//! Data extraction utilities for CS2 demo parsing
//!
//! This module provides helper functions to extract various data types
//! from the CS2 demo parser's dataframe structures.

use super::weapon_mapper::get_weapon_id_with_fallback;
use ahash::AHashMap;
use log::debug;
use parser::first_pass::prop_controller::*;
use parser::second_pass::variants::{PropColumn, VarVec};
use std::collections::HashMap;

/// Extract an i32 value from the dataframe at the specified index
///
/// # Arguments
/// * `df` - The dataframe containing property columns
/// * `prop_id` - The property ID to extract
/// * `index` - The row index to extract from
///
/// # Returns
/// * `Some(i32)` if the value exists and can be extracted
/// * `None` if the property doesn't exist or the value is None
pub fn get_i32_value(df: &AHashMap<u32, PropColumn>, prop_id: &u32, index: usize) -> Option<i32> {
    if let Some(column) = df.get(prop_id) {
        if let Some(VarVec::I32(values)) = &column.data {
            if let Some(Some(value)) = values.get(index) {
                return Some(*value);
            }
        }
    }
    None
}

/// Extract a u32 value from the dataframe at the specified index
///
/// Supports automatic conversion from F32 to U32 for compatibility.
///
/// # Arguments
/// * `df` - The dataframe containing property columns
/// * `prop_id` - The property ID to extract
/// * `index` - The row index to extract from
///
/// # Returns
/// * `Some(u32)` if the value exists and can be extracted/converted
/// * `None` if the property doesn't exist or the value is None
pub fn get_u32_value(df: &AHashMap<u32, PropColumn>, prop_id: &u32, index: usize) -> Option<u32> {
    if let Some(column) = df.get(prop_id) {
        if let Some(data) = &column.data {
            match data {
                VarVec::U32(values) => {
                    if let Some(Some(value)) = values.get(index) {
                        return Some(*value);
                    }
                }
                VarVec::F32(values) => {
                    if let Some(Some(value)) = values.get(index) {
                        return Some(*value as u32);
                    }
                }
                VarVec::I32(values) => {
                    if let Some(Some(value)) = values.get(index) {
                        if *value >= 0 {
                            return Some(*value as u32);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Extract an f32 value from the dataframe at the specified index
///
/// Supports automatic conversion from I32 and U32 to F32 for compatibility.
///
/// # Arguments
/// * `df` - The dataframe containing property columns
/// * `prop_id` - The property ID to extract
/// * `index` - The row index to extract from
///
/// # Returns
/// * `Some(f32)` if the value exists and can be extracted/converted
/// * `None` if the property doesn't exist or the value is None
pub fn get_f32_value(df: &AHashMap<u32, PropColumn>, prop_id: &u32, index: usize) -> Option<f32> {
    if let Some(column) = df.get(prop_id) {
        if let Some(data) = &column.data {
            match data {
                VarVec::F32(values) => {
                    if let Some(Some(value)) = values.get(index) {
                        return Some(*value);
                    }
                }
                VarVec::I32(values) => {
                    if let Some(Some(value)) = values.get(index) {
                        return Some(*value as f32);
                    }
                }
                VarVec::U32(values) => {
                    if let Some(Some(value)) = values.get(index) {
                        return Some(*value as f32);
                    }
                }
                _ => {}
            }
        }
    }
    None
}

/// Extract a Source vector value without decomposing or re-axing it.
pub fn get_xyz_value(
    df: &AHashMap<u32, PropColumn>,
    prop_id: &u32,
    index: usize,
) -> Option<[f32; 3]> {
    if let Some(column) = df.get(prop_id) {
        if let Some(VarVec::XYZVec(values)) = &column.data {
            if let Some(Some(value)) = values.get(index) {
                return Some(*value);
            }
        }
    }
    None
}

/// Extract a String value from the dataframe at the specified index
///
/// # Arguments
/// * `df` - The dataframe containing property columns
/// * `prop_id` - The property ID to extract
/// * `index` - The row index to extract from
///
/// # Returns
/// * `Some(String)` if the value exists and can be extracted
/// * `None` if the property doesn't exist or the value is None
pub fn get_string_value(
    df: &AHashMap<u32, PropColumn>,
    prop_id: &u32,
    index: usize,
) -> Option<String> {
    if let Some(column) = df.get(prop_id) {
        if let Some(VarVec::String(values)) = &column.data {
            if let Some(Some(value)) = values.get(index) {
                return Some(value.clone());
            }
        }
    }
    None
}

/// Extract a boolean value as u32 (0 or 1) from the dataframe at the specified index
///
/// Supports automatic conversion from Bool, U32, and I32 types.
/// Returns 1 for true/non-zero values, 0 for false/zero values.
///
/// # Arguments
/// * `df` - The dataframe containing property columns
/// * `prop_id` - The property ID to extract
/// * `index` - The row index to extract from
///
/// # Returns
/// * `1` for true/non-zero values
/// * `0` for false/zero values or if the property doesn't exist
pub fn get_bool_as_u32(df: &AHashMap<u32, PropColumn>, prop_id: &u32, index: usize) -> u32 {
    if let Some(column) = df.get(prop_id) {
        if let Some(data) = &column.data {
            match data {
                VarVec::Bool(values) => {
                    if let Some(Some(value)) = values.get(index) {
                        return if *value { 1 } else { 0 };
                    }
                }
                VarVec::U32(values) => {
                    if let Some(Some(value)) = values.get(index) {
                        return if *value > 0 { 1 } else { 0 };
                    }
                }
                VarVec::I32(values) => {
                    if let Some(Some(value)) = values.get(index) {
                        return if *value > 0 { 1 } else { 0 };
                    }
                }
                _ => {}
            }
        }
    }
    0
}

/// Extract weapon ID with fallback mechanisms
///
/// Tries multiple property names to find the weapon ID, using proper weapon mapping
/// to handle CS2 weapon ID conversions and normalization.
///
/// # Arguments
/// * `df` - The dataframe containing property columns
/// * `name_to_id` - Mapping of property names to their IDs
/// * `index` - The row index to extract from
/// * `debug_first_tick` - Whether to print debug info for the first tick
///
/// # Returns
/// * The weapon ID as String, properly normalized using weapon mapper
pub fn extract_weapon_id(
    df: &AHashMap<u32, PropColumn>,
    name_to_id: &HashMap<String, u32>,
    index: usize,
    debug_first_tick: bool,
) -> String {
    let mut raw_weapon_id: Option<u32> = None;
    let weapon_name = get_string_value(df, &WEAPON_NAME_ID, index);

    // Try weapon ID properties in order of preference
    let weapon_id_properties = [
        "unItemDefIdx",
        "CCSPlayerController.CCSPlayerController_InventoryServices.ServerAuthoritativeWeaponSlot_t.unItemDefIdx",
        "m_iItemDefinitionIndex",
    ];

    for prop_name in &weapon_id_properties {
        if let Some(weapon_itemid_id) = name_to_id.get(*prop_name) {
            if let Some(id) = get_u32_value(df, weapon_itemid_id, index) {
                if debug_first_tick {
                    debug!("Found weapon ID {} from property {}", id, prop_name);
                }
                // The active-weapon aliases above identify a weapon entity.
                // Do not consult CCSPlayerPawn.m_iItemDefinitionIndex here:
                // that belongs to the player's wearable glove and can be a
                // valid non-zero value (for example 5030), not a weapon.
                if id > 0 {
                    raw_weapon_id = Some(id);
                    break;
                }
            }
        }
    }

    // Use weapon mapper to get proper ID with fallback logic
    let final_weapon_id = get_weapon_id_with_fallback(raw_weapon_id, weapon_name.as_deref());

    if debug_first_tick {
        debug!(
            "Final weapon ID: {} (raw: {:?}, name: {:?})",
            final_weapon_id, raw_weapon_id, weapon_name
        );
    }

    final_weapon_id.to_string()
}

/// Calculate mouse velocity from yaw changes between ticks
///
/// # Arguments
/// * `df` - The dataframe containing property columns
/// * `yaw_id` - The yaw property ID
/// * `current_index` - Current tick index
/// * `current_yaw` - Current yaw value
///
/// # Returns
/// * The calculated mouse velocity as f32
pub fn calculate_mouse_velocity(
    df: &AHashMap<u32, PropColumn>,
    yaw_id: &u32,
    current_index: usize,
    current_yaw: f32,
) -> f32 {
    if current_index > 0 {
        let prev_yaw = get_f32_value(df, yaw_id, current_index - 1).unwrap_or(0.0);
        let yaw_diff = current_yaw - prev_yaw;

        // Normalize to handle wraparound
        let normalized_diff = if yaw_diff > 180.0 {
            yaw_diff - 360.0
        } else if yaw_diff < -180.0 {
            yaw_diff + 360.0
        } else {
            yaw_diff
        };

        normalized_diff.abs()
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tick_by_tick::weapon_mapper::weapon_ids;

    fn column(data: VarVec) -> PropColumn {
        PropColumn {
            data: Some(data),
            num_nones: 0,
        }
    }

    #[test]
    fn weapon_id_never_treats_the_pawn_glove_definition_as_the_active_weapon() {
        let pawn_glove_item_id = 1;
        let active_weapon_item_id = 2;
        let mut name_to_id = HashMap::new();
        name_to_id.insert(
            "CCSPlayerPawn.m_iItemDefinitionIndex".to_string(),
            pawn_glove_item_id,
        );
        name_to_id.insert("m_iItemDefinitionIndex".to_string(), active_weapon_item_id);

        let mut df = AHashMap::new();
        df.insert(pawn_glove_item_id, column(VarVec::U32(vec![Some(5030)])));
        df.insert(
            active_weapon_item_id,
            column(VarVec::U32(vec![Some(weapon_ids::MP9)])),
        );
        df.insert(
            WEAPON_NAME_ID,
            column(VarVec::String(vec![Some("MP9".to_string())])),
        );

        assert_eq!(extract_weapon_id(&df, &name_to_id, 0, false), "34");
    }
}
