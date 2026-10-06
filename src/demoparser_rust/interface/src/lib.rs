//! Interface crate for the demoparser_rust project
//!
//! This crate provides an interface for parsing CS:GO demo files and extracting kill collections.

pub mod core;
pub mod demo_cache;
pub mod models;
pub mod utils;

use std::io;
use std::path::Path;

use core::demo_processor::DemoProcessor;
use core::tick_processor::TickProcessor;
use models::collection::KillCollection;
use models::kill::Kill;

/// Process a demo file and extract kill collections
pub fn process_demo(demo_path: &str) -> io::Result<Vec<KillCollection>> {
    // Check if the demo file exists
    if !Path::new(demo_path).exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Demo file not found: {}", demo_path),
        ));
    }

    // Create a demo processor
    let demo_processor = DemoProcessor::new(demo_path)?;

    // Create a tick processor
    let mut tick_processor = TickProcessor::new(
        demo_processor.get_demo_info().clone(),
        demo_processor.get_player_info().to_vec(),
        demo_processor.get_rounds().to_vec(),
        demo_processor.get_game_events().to_vec(),
    );

    // Process game events
    tick_processor.process_events();

    // Create collections
    tick_processor.create_collections();

    // Return the collections
    Ok(tick_processor.get_collections().to_vec())
}

use crate::core::game_event::GameEvent;

/// Get kills from a demo file
pub fn get_kills(demo_path: &str) -> io::Result<Vec<Kill>> {
    // Check if the demo file exists
    if !Path::new(demo_path).exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Demo file not found: {}", demo_path),
        ));
    }

    // Create a demo processor
    let demo_processor = DemoProcessor::new(demo_path)?;

    // Return the death events
    let kills = demo_processor
        .get_game_events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::Kill(kill) => Some(kill.clone()),
            _ => None,
        })
        .collect();
    Ok(kills)
}

/// Filter collections by killer SteamID
pub fn filter_collections_by_killer_steamid(
    collections: &[KillCollection],
    killer_steamid: &str,
) -> Vec<KillCollection> {
    utils::single_filter::filter_collections_by_killer_steamid(collections, killer_steamid)
}

/// Filter collections by collection type
pub fn filter_collections_by_type(
    collections: &[KillCollection],
    collection_type: &str,
) -> Vec<KillCollection> {
    utils::single_filter::filter_collections_by_type(collections, collection_type)
}

/// Filter collections by map name
pub fn filter_collections_by_map(
    collections: &[KillCollection],
    map_name: &str,
) -> Vec<KillCollection> {
    utils::single_filter::filter_collections_by_map(collections, map_name)
}

/// Filter collections by weapon
pub fn filter_collections_by_weapon(
    collections: &[KillCollection],
    weapon: &str,
) -> Vec<KillCollection> {
    utils::single_filter::filter_collections_by_weapon(collections, weapon)
}

/// Filter collections by folder
pub fn filter_collections_by_folder(
    collections: &[KillCollection],
    folder: &str,
) -> Vec<KillCollection> {
    utils::single_filter::filter_collections_by_folder(collections, folder)
}

/// Filter kills by killer SteamID
pub fn filter_kills_by_killer_steamid(kills: &[Kill], killer_steamid: &str) -> Vec<Kill> {
    utils::single_filter::filter_kills_by_killer_steamid(kills, killer_steamid)
}

/// Filter kills by victim SteamID
pub fn filter_kills_by_victim_steamid(kills: &[Kill], victim_steamid: &str) -> Vec<Kill> {
    utils::single_filter::filter_kills_by_victim_steamid(kills, victim_steamid)
}

/// Filter kills by weapon
pub fn filter_kills_by_weapon(kills: &[Kill], weapon: &str) -> Vec<Kill> {
    utils::single_filter::filter_kills_by_weapon(kills, weapon)
}

/// Filter kills by round
pub fn filter_kills_by_round(kills: &[Kill], round: i32) -> Vec<Kill> {
    utils::single_filter::filter_kills_by_round(kills, round)
}

/// Filter kills by headshot
pub fn filter_kills_by_headshot(kills: &[Kill], headshot: bool) -> Vec<Kill> {
    utils::single_filter::filter_kills_by_headshot(kills, headshot)
}
