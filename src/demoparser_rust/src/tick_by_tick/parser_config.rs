//! Parser configuration utilities for CS2 demo parsing
//!
//! This module handles the setup and configuration of the CS2 demo parser,
//! including property definitions and parser settings.

use super::button_press::ButtonParser;
use super::team_parser;
use ahash::AHashMap;
use parser::first_pass::parser_settings::{rm_user_friendly_names, ParserInputs};
use parser::second_pass::parser_settings::create_huffman_lookup_table;

/// Create the list of wanted properties for demo parsing
///
/// This function defines all the properties we want to extract from the demo,
/// including player position, weapon data, movement states, and button inputs.
///
/// # Returns
/// * A vector of property names to be extracted during parsing
pub fn create_wanted_properties() -> Vec<String> {
    let mut wanted_props = vec![
        // Basic tick and player info
        "tick".to_string(),
        "steamid".to_string(),
        "entity_id".to_string(),
        "CCSPlayerController.m_nPawnCharacterDefIndex".to_string(),
        // Opt in to the parser's authoritative all-weapon entity lane. This is
        // a capture marker rather than a dataframe column.
        "weapon_entity_snapshots".to_string(),
        "game_time".to_string(),
        // Weapon information
        "weapon_name".to_string(),
        "weapon_itemid".to_string(),          // WeaponID
        "item_def_idx".to_string(),           // Alternative WeaponID
        "m_iItemDefinitionIndex".to_string(), // Another WeaponID option
        // Explicit econ cosmetics; the parser pairs CEconItemAttribute slots
        // by definition index instead of assuming their order.
        "weapon_skin_id".to_string(),
        "weapon_paint_seed".to_string(),
        "weapon_float".to_string(),
        "weapon_stickers".to_string(),
        "weapon_keychain".to_string(),
        "item_id_high".to_string(),
        "item_id_low".to_string(),
        "weapon_quality".to_string(),
        "custom_name".to_string(),
        "glove_item_idx".to_string(),
        "glove_paint_id".to_string(),
        "glove_paint_seed".to_string(),
        "glove_paint_float".to_string(),
        // Position and orientation
        "X".to_string(),
        "Y".to_string(),
        "Z".to_string(),
        "pitch".to_string(),
        "yaw".to_string(),
        // Weapon state
        "m_iClip1".to_string(), // Ammo
        "m_bInReload".to_string(),
        "CCSPlayerPawn.m_bIsScoped".to_string(),
        "INSPECT".to_string(),
        // Movement and physics
        "is_airborne".to_string(),
        "velocity".to_string(),
        "velocity_X".to_string(),
        "velocity_Y".to_string(),
        "velocity_Z".to_string(),
        "mouse_dx".to_string(),
        // Player states
        "CCSPlayerPawn.m_flFlashDuration".to_string(),
        "CCSPlayerPawn.m_flFlashMaxAlpha".to_string(),
        "CCSPlayerPawn.CCSPlayer_MovementServices.m_flDuckAmount".to_string(),
        // CNetworkViewOffsetVector is flattened onto the pawn in the sendtable.
        "CCSPlayerPawn.m_vecX".to_string(),
        "CCSPlayerPawn.m_vecY".to_string(),
        "CCSPlayerPawn.m_vecZ".to_string(),
        "CCSPlayerPawn.m_bIsWalking".to_string(),
        "CCSPlayerPawn.m_bIsDefusing".to_string(),
        "is_alive".to_string(),
        // Vitals
        "CCSPlayerPawn.m_iHealth".to_string(),
        "CCSPlayerPawn.m_ArmorValue".to_string(),
        // Fatal bullet impact authority used to seed the replay ragdoll on its
        // authored PHYS body. These are Source world-space values plus a model
        // skeleton bone index, not inferred hitgroup markers.
        "CCSPlayerPawn.m_nRagdollDamageBone".to_string(),
        "CCSPlayerPawn.m_vRagdollDamagePosition".to_string(),
        "CCSPlayerPawn.m_vRagdollDamageForce".to_string(),
        // Diagnostic provenance only. This is retained separately from the kill-event tick and
        // does not feed replay physics.
        "CCSPlayerPawn.m_vRagdollServerOrigin".to_string(),
        // Crouch state (full path required)
        "CCSPlayerPawn.CCSPlayer_MovementServices.m_bDucked".to_string(),
    ];

    // Add button-related properties from our dedicated module
    wanted_props.extend(ButtonParser::get_button_property_names());

    // Add team-related properties from our team parser module
    wanted_props.extend(team_parser::get_team_properties());

    wanted_props
}

/// Create multi-player parser settings for demo parsing
///
/// Sets up parser configuration to extract data for all players in the specified
/// tick range, allowing for efficient single-pass parsing followed by in-memory filtering.
///
/// # Arguments
/// * `tick_start` - Starting tick for parsing
/// * `tick_end` - Ending tick for parsing
/// * `huffman_table` - Reference to the huffman lookup table
/// * `parse_projectiles` - Whether to parse projectile/grenade entities
///
/// # Returns
/// * Configured `ParserInputs` struct ready for multi-player parsing
pub fn create_multi_player_parser_settings<'a>(
    tick_start: i32,
    tick_end: i32,
    huffman_table: &'a Vec<(u8, u8)>,
    parse_projectiles: bool,
) -> ParserInputs<'a> {
    // ParserInputs is a core API and expects raw sendtable property names.
    // Keep the output names raw here because the tick processor's cache already
    // looks up those canonical names.
    let mut wanted_props = rm_user_friendly_names(&create_wanted_properties())
        .expect("the parser's friendly-name mapping is infallible");
    if parse_projectiles {
        wanted_props.extend(
            [
                "m_VoxelFrameData",
                "m_nVoxelFrameDataSize",
                "m_nVoxelUpdate",
                "m_nSmokeEffectTickBegin",
                "m_vSmokeColor",
                "m_vSmokeDetonationPos",
            ]
            .into_iter()
            .map(str::to_owned),
        );
    }
    let tick_range: Vec<i32> = (tick_start..=tick_end).collect();

    ParserInputs {
        wanted_players: vec![], // Empty vector = get all players
        real_name_to_og_name: AHashMap::default(),
        wanted_player_props: wanted_props,
        wanted_events: if parse_projectiles {
            vec![
                "weapon_fire".to_string(),
                "player_hurt".to_string(),
                "player_death".to_string(),
                "player_blind".to_string(),
                "player_spawn".to_string(),
                "bullet_impact".to_string(),
                "fire_bullets".to_string(),
                "hegrenade_detonate".to_string(),
                "flashbang_detonate".to_string(),
                "smokegrenade_detonate".to_string(),
                "decoy_started".to_string(),
                "inferno_startburn".to_string(),
            ]
        } else {
            vec![] // Empty when not parsing grenades to avoid interfering with entity parsing
        },
        wanted_other_props: vec![],
        parse_ents: true,
        wanted_ticks: tick_range,
        parse_projectiles,
        parse_grenades: parse_projectiles, // Must match parse_projectiles for proper parsing
        only_header: false,
        list_props: false,
        only_convars: false,
        huffman_lookup_table: huffman_table,
        fallback_bytes: None,
        wanted_prop_states: AHashMap::default(),
        order_by_steamid: false,
    }
}

/// Event/projectile/weapon authority does not need the hot player-frame dataframe.  Keep the
/// velocity marker because it enables per-tick projectile collection, plus the raw smoke fields
/// and the weapon-entity capture marker. Entity decoding still retains the authoritative state
/// needed by those lanes, while avoiding dozens of unused output columns.
pub fn create_event_authority_parser_settings<'a>(
    tick_start: i32,
    tick_end: i32,
    huffman_table: &'a Vec<(u8, u8)>,
) -> ParserInputs<'a> {
    let event_props = vec![
        "is_airborne".to_string(),
        "velocity".to_string(),
        "weapon_entity_snapshots".to_string(),
        // Opt in to the door/breakable/mover lane. Also a capture marker, not a column.
        "world_entities".to_string(),
        "game_time".to_string(),
        "m_VoxelFrameData".to_string(),
        "m_nVoxelFrameDataSize".to_string(),
        "m_nVoxelUpdate".to_string(),
        "m_nSmokeEffectTickBegin".to_string(),
        "m_vSmokeColor".to_string(),
        "m_vSmokeDetonationPos".to_string(),
    ];
    let wanted_props = rm_user_friendly_names(&event_props)
        .expect("the parser's friendly-name mapping is infallible");

    ParserInputs {
        wanted_players: vec![],
        real_name_to_og_name: AHashMap::default(),
        wanted_player_props: wanted_props,
        wanted_events: vec![
            "weapon_fire".to_string(),
            "player_hurt".to_string(),
            "player_death".to_string(),
            "player_blind".to_string(),
            "player_spawn".to_string(),
            "bullet_impact".to_string(),
            "fire_bullets".to_string(),
            "hegrenade_detonate".to_string(),
            "flashbang_detonate".to_string(),
            "smokegrenade_detonate".to_string(),
            "decoy_started".to_string(),
            "inferno_startburn".to_string(),
        ],
        wanted_other_props: vec![],
        parse_ents: true,
        wanted_ticks: (tick_start..=tick_end).collect(),
        parse_projectiles: true,
        parse_grenades: true,
        only_header: false,
        list_props: false,
        only_convars: false,
        huffman_lookup_table: huffman_table,
        fallback_bytes: None,
        wanted_prop_states: AHashMap::default(),
        order_by_steamid: false,
    }
}

/// Initialize the huffman lookup table
///
/// Creates and returns the huffman lookup table required for demo parsing.
///
/// # Returns
/// * The huffman lookup table for efficient parsing
pub fn initialize_huffman_table() -> &'static Vec<(u8, u8)> {
    static TABLE: std::sync::OnceLock<Vec<(u8, u8)>> = std::sync::OnceLock::new();
    TABLE.get_or_init(create_huffman_lookup_table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_ragdoll_origin_is_requested_for_dead_row_diagnostics() {
        let wanted = create_wanted_properties();
        assert!(wanted.iter().any(|name| name == "CCSPlayerPawn.m_vRagdollServerOrigin"));

        let settings = create_multi_player_parser_settings(0, 1, initialize_huffman_table(), false);
        assert!(settings
            .wanted_player_props
            .iter()
            .any(|name| name == "CCSPlayerPawn.m_vRagdollServerOrigin"));
    }
}
