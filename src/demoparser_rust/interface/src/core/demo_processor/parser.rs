//! Main demo parser implementation
//!
//! This module contains the core DemoProcessor struct and its implementation.

use ahash::AHashMap;
use parser::first_pass::parser_settings::{create_mmap, rm_user_friendly_names, ParserInputs};
use parser::parse_demo::{Parser, ParsingMode};
use parser::second_pass::parser_settings::create_huffman_lookup_table;
use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::core::demo_processor::game_parser::{
    parse_game_events_from_output, parse_game_start_offset_from_output,
};
use crate::core::demo_processor::player_parser::parse_player_info_from_output;
use crate::core::demo_processor::round_parser::parse_round_info_from_output;
use crate::core::demo_processor::types::{DemoInfo, PlayerInfo, RoundInfo};
use crate::core::game_event::GameEvent;

/// Demo processor for processing demo files
pub struct DemoProcessor {
    /// Demo info
    demo_info: DemoInfo,
    /// Player info
    player_info: Vec<PlayerInfo>,
    /// Round info
    rounds: Vec<RoundInfo>,
    /// Game events
    game_events: Vec<GameEvent>,
    /// Game start offset
    game_start_offset: f32,
}

/// The friendly `team_num` mapping resolves to an unqualified legacy property
/// name on some CS2 builds.  Request the live pawn/controller paths explicitly
/// as well, otherwise the parser sees the sendtable definition but never emits
/// a dataframe column for the team timeline.
fn wanted_player_properties() -> Vec<String> {
    vec![
        "X".to_string(),
        "Y".to_string(),
        "Z".to_string(),
        "pitch".to_string(),
        "yaw".to_string(),
        "team_num".to_string(),
        "CCSPlayerPawn.m_iTeamNum".to_string(),
        "CCSPlayerController.m_iTeamNum".to_string(),
        // Required to populate the synthetic `STEAMID_ID` dataframe column
        // used to join a live pawn-side sample back to a player.
        "player_steamid".to_string(),
        "CCSPlayerController.m_steamID".to_string(),
        "active_weapon".to_string(),
        "item_def_idx".to_string(),
        "team_name".to_string(),
        "is_controlling_bot".to_string(),
        "is_airborne".to_string(),
    ]
}

impl DemoProcessor {
    /// Create a new demo processor
    pub fn new(demo_path: &str) -> io::Result<Self> {
        Self::new_with_round_policy(demo_path, false)
    }

    /// Read metadata for an explicitly scoped diagnostic interval, including a
    /// SourceTV control that ends before any round_end event. Production callers
    /// continue to require completed rounds through `new`.
    pub fn new_diagnostic_interval(demo_path: &str) -> io::Result<Self> {
        Self::new_with_round_policy(demo_path, true)
    }

    fn new_with_round_policy(demo_path: &str, allow_no_round_end: bool) -> io::Result<Self> {
        // Check if the demo file exists
        if !Path::new(demo_path).exists() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Demo file not found: {}", demo_path),
            ));
        }

        // The demo's bytes only need to outlive the parse below, so both backings are bound
        // to locals here. This previously leaked the mmap to conjure a &'static, justified
        // by "we process one demo at a time" - which is not true of the batch pipeline,
        // where Rayon runs demos concurrently and every mapping stayed resident until exit.
        let cached_demo: Arc<Vec<u8>>;
        let mapped_demo: memmap2::Mmap;
        let demo_data: &[u8] = if Self::is_compressed(demo_path) {
            // Try to get from cache first
            if let Some(cached) = crate::demo_cache::get_cached_demo(demo_path) {
                cached_demo = cached;
                &cached_demo
            } else {
                let demo_bytes = Self::decompress_to_memory(demo_path)?;

                // Cache the decompressed data
                cached_demo = crate::demo_cache::cache_demo(demo_path.to_string(), demo_bytes);
                &cached_demo
            }
        } else {
            // Create a memory map of the demo file
            mapped_demo = create_mmap(demo_path.to_string()).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to create memory map: {}", e),
                )
            })?;

            &mapped_demo
        };

        // Create a huffman lookup table for the parser
        static HUFFMAN: std::sync::OnceLock<Vec<(u8, u8)>> = std::sync::OnceLock::new();
        let huffman_lookup_table = HUFFMAN.get_or_init(create_huffman_lookup_table);

        // The core parser expects raw property names. Keep the friendly names in the
        // output so the event parsers below can continue using their existing schema.
        let wanted_player_props = wanted_player_properties();
        let wanted_other_props = vec![
            "total_rounds_played".to_string(),
            "is_warmup_period".to_string(),
            "game_time".to_string(),
        ];
        let real_player_props = rm_user_friendly_names(&wanted_player_props).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to resolve player properties: {e}"),
            )
        })?;
        let real_other_props = rm_user_friendly_names(&wanted_other_props).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to resolve game properties: {e}"),
            )
        })?;
        let mut real_name_to_og_name = AHashMap::default();
        for (real_name, friendly_name) in real_player_props.iter().zip(&wanted_player_props) {
            real_name_to_og_name.insert(real_name.clone(), friendly_name.clone());
        }
        for (real_name, friendly_name) in real_other_props.iter().zip(&wanted_other_props) {
            real_name_to_og_name.insert(real_name.clone(), friendly_name.clone());
        }

        // Create parser settings for a single pass.
        let settings = ParserInputs {
            real_name_to_og_name,
            wanted_players: vec![],
            wanted_player_props: real_player_props,
            wanted_other_props: real_other_props,
            wanted_prop_states: AHashMap::default(),
            wanted_events: vec![
                "player_death".to_string(),
                "player_team".to_string(),
                "round_start".to_string(),
                "round_freeze_end".to_string(),
                "round_end".to_string(),
                "round_officially_ended".to_string(),
            ],
            parse_ents: true,
            wanted_ticks: vec![],
            parse_projectiles: false,
            parse_grenades: false,
            only_header: false,
            list_props: false,
            only_convars: false,
            huffman_lookup_table,
            order_by_steamid: false,
            fallback_bytes: None,
        };

        // Nested Rayon work shares the caller's bounded pool. Allow idle workers to help with
        // a large demo or the tail of a batch, instead of pinning each source to one core.
        // Normal still honors the core parser's property-specific threading restrictions.
        // Collection discovery retains round events, not animation recipes. The replay
        // authority pass captures the complete AG2 stream once from the original source.
        let mut parser = Parser::new(settings, ParsingMode::Normal).with_animation_recipes(false);

        // Parse the demo
        let output = parser.parse_demo(demo_data).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("Failed to parse demo: {}", e))
        })?;

        // Extract header information
        let header = output.header.as_ref().ok_or_else(|| {
            io::Error::new(io::ErrorKind::Other, "Failed to extract header information")
        })?;

        // Get the demo name
        let demo_name = crate::utils::parser_utils::get_demo_name(demo_path);

        // Get the map name
        let map_name = header
            .get("map_name")
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Demo header missing map_name"))?
            .clone();

        // Get the total ticks from the last round_end event
        let mut last_tick = None;
        for event in &output.game_events {
            if event.name == "round_end" {
                for field in &event.fields {
                    if field.name == "tick" {
                        if let Some(parser::second_pass::variants::Variant::I32(tick)) = &field.data
                        {
                            if last_tick.is_none() || *tick > last_tick.unwrap() {
                                last_tick = Some(*tick);
                            }
                        }
                    }
                }
            }
        }

        let no_round_end = last_tick.is_none();
        let last_tick = match last_tick {
            Some(tick) => tick,
            None if allow_no_round_end => 0, // Diagnostic exporter replaces this with the verified final packet tick.
            None => return Err(io::Error::new(io::ErrorKind::Other, "No round_end events found in demo")),
        };

        // New parser versions expose the CS2 patch as `patch_version`. Retain the
        // legacy fields in DemoInfo so existing serialized output remains compatible.
        let patch_version = header
            .get("patch_version")
            .or_else(|| header.get("network_protocol"))
            .and_then(|s| s.parse::<i32>().ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Other,
                    "Invalid or missing patch_version/network_protocol",
                )
            })?;

        let server_name = header
            .get("server_name")
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Demo header missing server_name"))?
            .clone();

        let client_name = header
            .get("client_name")
            .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "Demo header missing client_name"))?
            .clone();

        let game_directory = header
            .get("game_directory")
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::Other, "Demo header missing game_directory")
            })?
            .clone();

        // Extract tick rate from convars if available, otherwise use estimated default
        let tick_rate = if let Some(tick_rate_str) = output.convars.get("sv_tickrate") {
            tick_rate_str.parse::<f32>().unwrap_or(64.0)
        } else if let Some(tick_rate_str) = output.convars.get("tickrate") {
            tick_rate_str.parse::<f32>().unwrap_or(64.0)
        } else {
            // Default to 64 tick rate - this is just for time conversion, doesn't affect parsing
            64.0
        };

        // These names are retained for compatibility with existing consumers.
        let demo_protocol = patch_version;
        let network_protocol = patch_version;

        // Create the demo info - show full path but clean up Windows UNC prefix
        let clean_demo_path = if let Ok(canonical_path) = std::fs::canonicalize(demo_path) {
            let path_str = canonical_path.to_string_lossy().to_string();
            // Remove Windows UNC prefix \\?\ if present
            if path_str.starts_with("\\\\?\\") {
                path_str[4..].to_string()
            } else {
                path_str
            }
        } else {
            // Use original path if canonicalization fails
            demo_path.to_string()
        };

        let game_version = patch_version.to_string();

        let demo_info = DemoInfo {
            demo_name,
            demo_path: clean_demo_path,
            map_name,
            game_version,
            tick_rate,
            playback_ticks: last_tick,
            playback_time: last_tick as f32 / tick_rate,
            demo_protocol,
            network_protocol,
            server_name,
            client_name,
            game_directory,
        };

        // Parse player info
        let player_info = parse_player_info_from_output(&output).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to parse player info: {}", e),
            )
        })?;

        // Parse round info
        let rounds = match parse_round_info_from_output(&output) {
            Ok(rounds) => rounds,
            Err(_) if allow_no_round_end && no_round_end => Vec::new(),
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to parse round info: {}", e),
                ));
            }
        };

        // Parse game events
        let game_events = parse_game_events_from_output(&output, &rounds, &player_info).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("Failed to parse game events: {}", e),
                )
            })?;

        // Parse game start offset
        let game_start_offset = parse_game_start_offset_from_output(&output).map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to parse game start offset: {}", e),
            )
        })?;

        crate::demo_cache::cache_round_events(demo_path.to_string(), &output.game_events);

        Ok(DemoProcessor {
            demo_info,
            player_info,
            rounds,
            game_events,
            game_start_offset,
        })
    }

    /// Get the demo info
    pub fn get_demo_info(&self) -> &DemoInfo {
        &self.demo_info
    }

    /// Get the player info
    pub fn get_player_info(&self) -> &[PlayerInfo] {
        &self.player_info
    }

    /// Get the round info
    pub fn get_rounds(&self) -> &[RoundInfo] {
        &self.rounds
    }

    /// Get the game events
    pub fn get_game_events(&self) -> &[GameEvent] {
        &self.game_events
    }

    /// Get the game start offset
    pub fn get_game_start_offset(&self) -> f32 {
        self.game_start_offset
    }

    /// Check if a file is compressed based on its extension
    fn is_compressed(path: &str) -> bool {
        path.ends_with(".gz") || path.ends_with(".zst")
    }

    /// Decompress a file directly to memory
    fn decompress_to_memory(path: &str) -> io::Result<Vec<u8>> {
        use std::fs::File;
        use std::io::Read;

        if path.ends_with(".gz") {
            use flate2::read::GzDecoder;
            let file = File::open(path)?;
            let mut decoder = GzDecoder::new(file);
            let mut buffer = Vec::new();
            decoder.read_to_end(&mut buffer)?;
            Ok(buffer)
        } else if path.ends_with(".zst") {
            use zstd::stream::Decoder;
            let file = File::open(path)?;
            let mut decoder = Decoder::new(file)?;
            let mut buffer = Vec::new();
            decoder.read_to_end(&mut buffer)?;
            Ok(buffer)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Unsupported compressed format: {}", path),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::wanted_player_properties;
    use parser::first_pass::parser_settings::rm_user_friendly_names;

    #[test]
    fn requests_canonical_live_team_properties_for_event_time_attribution() {
        let requested = rm_user_friendly_names(&wanted_player_properties()).unwrap();
        assert!(requested.contains(&"CCSPlayerPawn.m_iTeamNum".to_string()));
        assert!(requested.contains(&"CCSPlayerController.m_iTeamNum".to_string()));
        assert!(requested.contains(&"CCSPlayerController.m_steamID".to_string()));
    }
}
